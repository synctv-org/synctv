use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::AlistPlaybackProviderService;
use synctv_proto::playback_provider::alist::{
    AlistFileStreamResponse, AlistSubtitleResponse, AlistThumbnailResponse,
    AlistTranscodedHlsManifestResponse, AlistTranscodedHlsSegmentResponse,
    GetAlistFileStreamRequest, GetAlistSubtitleRequest, GetAlistThumbnailRequest,
    GetAlistTranscodedHlsManifestRequest, GetAlistTranscodedHlsSegmentRequest,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::AlistProvider::NAME;

pub struct AlistPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a AlistPlaybackProviderService,
    pub proxy_signing_key: &'a synctv_core::proxy_signature::ProxySigningKey,
    pub public_id_codec: &'a synctv_core::PublicIdCodec,
    pub provider_stores: &'a dyn synctv_core::provider::store::ProviderStoreResolver,
    pub user_service: &'a synctv_core::service::UserService,
    pub playback_transport_services:
        &'a synctv_core::provider::playback_transport::PlaybackTransportServices,
    pub request_control: Option<&'a ExecutionControl>,
    pub proxy_http_client: &'a reqwest::Client,
    pub ssrf_guard: &'a synctv_common::ssrf::SsrfGuard,
    pub proxy_slice_cache: &'a synctv_proxy::slice_cache::SliceCache,
}

pub type AlistFileStreamResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AlistFileStreamResponse, ApiError>> + Send + 'static>,
>;
pub type AlistTranscodedHlsManifestResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<AlistTranscodedHlsManifestResponse, ApiError>>
            + Send
            + 'static,
    >,
>;
pub type AlistTranscodedHlsSegmentResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<AlistTranscodedHlsSegmentResponse, ApiError>>
            + Send
            + 'static,
    >,
>;
pub type AlistSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AlistSubtitleResponse, ApiError>> + Send + 'static>,
>;
pub type AlistThumbnailResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AlistThumbnailResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_alist_file_stream(
    deps: AlistPlaybackProviderDeps<'_>,
    req: GetAlistFileStreamRequest,
) -> Result<AlistFileStreamResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let (store, _) = verify_alist_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("files/{}/{}", req.mode_name, req.url_index),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let action = deps
        .playback_provider_service
        .file_stream_action(
            &req.version,
            &req.mode_name,
            req.url_index as usize,
            req.range.as_deref(),
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, head).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AlistFileStreamResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_alist_transcoded_hls_manifest(
    deps: AlistPlaybackProviderDeps<'_>,
    req: GetAlistTranscodedHlsManifestRequest,
) -> Result<AlistTranscodedHlsManifestResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_alist_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!(
                "transcoded-hls-manifests/{}/{}",
                req.mode_name, req.url_index
            ),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let action = deps
        .playback_provider_service
        .transcoded_hls_manifest_action(
            &req.version,
            &req.mode_name,
            req.url_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let segment_base =
        playback_provider_route_base("alist", &req.version, "transcoded-hls-segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims, "transcoded-hls-segments"),
        action,
        false,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AlistTranscodedHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_alist_transcoded_hls_segment(
    deps: AlistPlaybackProviderDeps<'_>,
    req: GetAlistTranscodedHlsSegmentRequest,
) -> Result<AlistTranscodedHlsSegmentResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let (store, claims) = verify_alist_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: "transcoded-hls-segments".to_string(),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: Some(&req.target_url),
        },
    )
    .await?;
    let action = deps
        .playback_provider_service
        .transcoded_hls_segment_action(
            &req.version,
            &req.target_url,
            req.range.as_deref(),
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let segment_base =
        playback_provider_route_base("alist", &req.version, "transcoded-hls-segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims, "transcoded-hls-segments"),
        action,
        head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AlistTranscodedHlsSegmentResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_alist_subtitle(
    deps: AlistPlaybackProviderDeps<'_>,
    req: GetAlistSubtitleRequest,
) -> Result<AlistSubtitleResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_alist_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("subtitles/{}/{}", req.mode_name, req.subtitle_index),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let action = deps
        .playback_provider_service
        .subtitle_action(
            &req.version,
            &req.mode_name,
            req.subtitle_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, false).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AlistSubtitleResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_alist_thumbnail(
    deps: AlistPlaybackProviderDeps<'_>,
    req: GetAlistThumbnailRequest,
) -> Result<AlistThumbnailResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_alist_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: "thumbnail".to_string(),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let action = deps
        .playback_provider_service
        .thumbnail_action(&req.version, store, deps.request_control)
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, false).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AlistThumbnailResponse { chunk: Some(chunk) })
    })))
}

async fn verify_alist_access(
    deps: &AlistPlaybackProviderDeps<'_>,
    request: PlaybackProviderAccessRequest<'_>,
) -> Result<
    (
        std::sync::Arc<dyn synctv_core::provider::store::ProviderStore>,
        synctv_core::proxy_signature::ProxyUrlClaims,
    ),
    ApiError,
> {
    verify_playback_provider_access_with_deps(&deps.access_deps(), PROVIDER, request).await
}

crate::impl_has_playback_provider_access_fields!(AlistPlaybackProviderDeps<'a>);

impl<'a> AlistPlaybackProviderDeps<'a> {
    fn chunk_deps(&self) -> PlaybackTransportExecutorDeps<'a> {
        PlaybackTransportExecutorDeps {
            proxy_signing_key: self.proxy_signing_key,
            proxy_http_client: self.proxy_http_client,
            ssrf_guard: self.ssrf_guard,
            proxy_slice_cache: self.proxy_slice_cache,
            request_control: self.request_control,
            hls_rewrite: None,
        }
    }

    fn chunk_deps_with_hls(
        &self,
        segment_base: &'a str,
        claims: &'a synctv_core::proxy_signature::ProxyUrlClaims,
        resource: &'static str,
    ) -> PlaybackTransportExecutorDeps<'a> {
        PlaybackTransportExecutorDeps {
            hls_rewrite: Some(HlsRewriteSigning {
                segment_base,
                claims,
                resource,
            }),
            ..self.chunk_deps()
        }
    }
}
