use futures::StreamExt;
use synctv_core::provider::{ExecutionControl, TrueNasHlsResourceRequest};
use synctv_core::service::TrueNasPlaybackProviderService;
use synctv_proto::playback_provider::truenas::{
    GetTrueNasHlsManifestRequest, GetTrueNasHlsResourceRequest, GetTrueNasResourceRequest,
    GetTrueNasSubtitleRequest, TrueNasHlsManifestResponse, TrueNasHlsResourceKind,
    TrueNasHlsResourceResponse, TrueNasResourceResponse, TrueNasSubtitleResponse,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::TrueNasProvider::NAME;

pub struct TrueNasPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a TrueNasPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type TrueNasResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TrueNasResourceResponse, ApiError>> + Send + 'static>,
>;
pub type TrueNasHlsManifestResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TrueNasHlsManifestResponse, ApiError>> + Send + 'static>,
>;
pub type TrueNasHlsResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TrueNasHlsResourceResponse, ApiError>> + Send + 'static>,
>;

pub type TrueNasSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TrueNasSubtitleResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_truenas_resource(
    deps: TrueNasPlaybackProviderDeps<'_>,
    req: GetTrueNasResourceRequest,
) -> Result<TrueNasResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("resources/{}/{}", req.mode_name, req.media_index),
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
        .resource_action(
            &req.version,
            &req.mode_name,
            req.media_index as usize,
            req.range.as_deref(),
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, req.head).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| TrueNasResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_truenas_hls_manifest(
    deps: TrueNasPlaybackProviderDeps<'_>,
    req: GetTrueNasHlsManifestRequest,
) -> Result<TrueNasHlsManifestResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("hls-manifests/{}/{}", req.mode_name, req.media_index),
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
        .hls_manifest_action(
            &req.version,
            &req.mode_name,
            req.media_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let resource_base = format!(
        "{}/{}/{}",
        playback_provider_route_base(PROVIDER, &req.version, "hls-resources"),
        urlencoding::encode(&req.mode_name),
        req.media_index
    );
    let resource_prefix = format!("hls-resources/{}/{}/*", req.mode_name, req.media_index);
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&resource_base, &claims, &resource_prefix),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| TrueNasHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_truenas_hls_resource(
    deps: TrueNasPlaybackProviderDeps<'_>,
    req: GetTrueNasHlsResourceRequest,
) -> Result<TrueNasHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = truenas_hls_resource_kind(req.resource_kind)?;
    let kind_name = truenas_hls_resource_kind_name(kind);
    let (store, claims) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!(
                "hls-resources/{}/{}/{kind_name}",
                req.mode_name, req.media_index
            ),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: Some(&req.target_url),
        },
    )
    .await?;
    let is_manifest = kind == TrueNasHlsResourceKind::Manifest;
    let action = deps
        .playback_provider_service
        .hls_resource_action(
            TrueNasHlsResourceRequest {
                version: &req.version,
                mode_name: &req.mode_name,
                media_index: req.media_index as usize,
                target_url: &req.target_url,
                is_manifest,
                range_header: req.range.as_deref(),
            },
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if is_manifest {
        let resource_base = format!(
            "{}/{}/{}",
            playback_provider_route_base(PROVIDER, &req.version, "hls-resources"),
            urlencoding::encode(&req.mode_name),
            req.media_index
        );
        let resource_prefix = format!("hls-resources/{}/{}/*", req.mode_name, req.media_index);
        playback_transport_action_to_chunk_stream(
            deps.chunk_deps_with_hls(&resource_base, &claims, &resource_prefix),
            action,
            req.head,
        )
        .await?
    } else {
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, req.head).await?
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| TrueNasHlsResourceResponse { chunk: Some(chunk) })
    })))
}

fn truenas_hls_resource_kind(value: i32) -> Result<TrueNasHlsResourceKind, ApiError> {
    let kind = TrueNasHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid TrueNAS HLS resource kind".to_string()))?;
    match kind {
        TrueNasHlsResourceKind::Media | TrueNasHlsResourceKind::Manifest => Ok(kind),
        TrueNasHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "TrueNAS HLS resource kind is required".to_string(),
        )),
    }
}

const fn truenas_hls_resource_kind_name(kind: TrueNasHlsResourceKind) -> &'static str {
    match kind {
        TrueNasHlsResourceKind::Media => "media",
        TrueNasHlsResourceKind::Manifest => "manifest",
        TrueNasHlsResourceKind::Unspecified => "unspecified",
    }
}

pub async fn get_truenas_subtitle(
    deps: TrueNasPlaybackProviderDeps<'_>,
    req: GetTrueNasSubtitleRequest,
) -> Result<TrueNasSubtitleResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
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
        chunk.map(|chunk| TrueNasSubtitleResponse { chunk: Some(chunk) })
    })))
}

crate::impl_has_playback_provider_access_fields!(TrueNasPlaybackProviderDeps<'a>);

impl<'a> TrueNasPlaybackProviderDeps<'a> {
    fn chunk_deps(&self) -> PlaybackTransportExecutorDeps<'a> {
        PlaybackTransportExecutorDeps {
            proxy_signing_key: self.runtime.proxy_signing_key,
            proxy_http_client: self.runtime.proxy_http_client,
            ssrf_guard: self.runtime.ssrf_guard,
            proxy_slice_cache: self.runtime.proxy_slice_cache,
            request_control: self.request_control,
            hls_rewrite: None,
        }
    }

    fn chunk_deps_with_hls(
        &self,
        segment_base: &'a str,
        claims: &'a crate::proxy_signature::ProxyUrlClaims,
        resource: &'a str,
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
