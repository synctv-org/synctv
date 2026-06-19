use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::DirectUrlPlaybackProviderService;
use synctv_proto::playback_provider::direct_url::{
    DirectUrlHlsManifestResponse, DirectUrlHlsSegmentResponse, DirectUrlStreamResponse,
    DirectUrlSubtitleResponse, GetDirectUrlHlsManifestRequest, GetDirectUrlHlsSegmentRequest,
    GetDirectUrlStreamRequest, GetDirectUrlSubtitleRequest,
};

use super::common::{
    playback_transport_action_to_chunk_stream, verify_playback_provider_http_access,
    HlsRewriteSigning, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::DirectUrlProvider::NAME;

pub struct DirectUrlPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a DirectUrlPlaybackProviderService,
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

pub type DirectUrlStreamResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DirectUrlStreamResponse, ApiError>> + Send + 'static>,
>;
pub type DirectUrlHlsManifestResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<DirectUrlHlsManifestResponse, ApiError>> + Send + 'static,
    >,
>;
pub type DirectUrlHlsSegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DirectUrlHlsSegmentResponse, ApiError>> + Send + 'static>,
>;
pub type DirectUrlSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DirectUrlSubtitleResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_direct_url_stream(
    deps: DirectUrlPlaybackProviderDeps<'_>,
    req: GetDirectUrlStreamRequest,
) -> Result<DirectUrlStreamResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let (store, _) = verify_direct_url_access(
        &deps,
        &req.version,
        format!("streams/{}/{}", req.mode_name, req.url_index),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        None,
    )
    .await?;
    let action = deps
        .playback_provider_service
        .stream_action(
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
        chunk.map(|chunk| DirectUrlStreamResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_direct_url_hls_manifest(
    deps: DirectUrlPlaybackProviderDeps<'_>,
    req: GetDirectUrlHlsManifestRequest,
) -> Result<DirectUrlHlsManifestResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_direct_url_access(
        &deps,
        &req.version,
        format!("hls-manifests/{}/{}", req.mode_name, req.url_index),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        None,
    )
    .await?;
    let action = deps
        .playback_provider_service
        .hls_manifest_action(
            &req.version,
            &req.mode_name,
            req.url_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let segment_base = playback_provider_route_base("direct-url", &req.version, "hls-segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims, "hls-segments"),
        action,
        false,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| DirectUrlHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_direct_url_hls_segment(
    deps: DirectUrlPlaybackProviderDeps<'_>,
    req: GetDirectUrlHlsSegmentRequest,
) -> Result<DirectUrlHlsSegmentResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let (store, claims) = verify_direct_url_access(
        &deps,
        &req.version,
        "hls-segments".to_string(),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        Some(&req.target_url),
    )
    .await?;
    let action = deps
        .playback_provider_service
        .hls_segment_action(
            &req.version,
            &req.target_url,
            req.range.as_deref(),
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let segment_base = playback_provider_route_base("direct-url", &req.version, "hls-segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims, "hls-segments"),
        action,
        head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| DirectUrlHlsSegmentResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_direct_url_subtitle(
    deps: DirectUrlPlaybackProviderDeps<'_>,
    req: GetDirectUrlSubtitleRequest,
) -> Result<DirectUrlSubtitleResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_direct_url_access(
        &deps,
        &req.version,
        format!("subtitles/{}/{}", req.mode_name, req.subtitle_index),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        None,
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
        chunk.map(|chunk| DirectUrlSubtitleResponse { chunk: Some(chunk) })
    })))
}

#[allow(clippy::too_many_arguments)]
async fn verify_direct_url_access(
    deps: &DirectUrlPlaybackProviderDeps<'_>,
    version: &str,
    resource: String,
    signature: &str,
    user_id: &str,
    room_id: &str,
    expires_at: i64,
    target_url: Option<&str>,
) -> Result<
    (
        std::sync::Arc<dyn synctv_core::provider::store::ProviderStore>,
        synctv_core::proxy_signature::ProxyUrlClaims,
    ),
    ApiError,
> {
    verify_playback_provider_http_access(
        deps.proxy_signing_key,
        deps.public_id_codec,
        deps.provider_stores,
        deps.user_service,
        deps.playback_transport_services,
        PROVIDER,
        version,
        resource,
        signature,
        user_id,
        room_id,
        expires_at,
        target_url,
    )
    .await
}

fn playback_provider_route_base(route_provider: &str, version: &str, resource: &str) -> String {
    let encoded_version: String =
        url::form_urlencoded::byte_serialize(version.as_bytes()).collect();
    format!("/api/playback-providers/{route_provider}/{encoded_version}/{resource}")
}

impl<'a> DirectUrlPlaybackProviderDeps<'a> {
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
