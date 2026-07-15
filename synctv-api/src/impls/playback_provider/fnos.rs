use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::FnosPlaybackProviderService;
use synctv_proto::playback_provider::fnos::{
    FnosResourceResponse, FnosSegmentResponse, FnosSubtitleResponse, FnosThumbnailResponse,
    GetFnosResourceRequest, GetFnosSegmentRequest, GetFnosSubtitleRequest, GetFnosThumbnailRequest,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::FnosProvider::NAME;

pub struct FnosPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a FnosPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type FnosResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<FnosResourceResponse, ApiError>> + Send + 'static>,
>;
pub type FnosSegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<FnosSegmentResponse, ApiError>> + Send + 'static>,
>;
pub type FnosSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<FnosSubtitleResponse, ApiError>> + Send + 'static>,
>;
pub type FnosThumbnailResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<FnosThumbnailResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_fnos_resource(
    deps: FnosPlaybackProviderDeps<'_>,
    req: GetFnosResourceRequest,
) -> Result<FnosResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_playback_provider_access_with_deps(
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
    let segment_base = playback_provider_route_base(PROVIDER, &req.version, "segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| FnosResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_fnos_segment(
    deps: FnosPlaybackProviderDeps<'_>,
    req: GetFnosSegmentRequest,
) -> Result<FnosSegmentResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: "segments".to_string(),
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
        .segment_action(
            &req.version,
            req.target_url,
            req.range.as_deref(),
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let segment_base = playback_provider_route_base(PROVIDER, &req.version, "segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| FnosSegmentResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_fnos_subtitle(
    deps: FnosPlaybackProviderDeps<'_>,
    req: GetFnosSubtitleRequest,
) -> Result<FnosSubtitleResponseStream, ApiError> {
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
        chunk.map(|chunk| FnosSubtitleResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_fnos_thumbnail(
    deps: FnosPlaybackProviderDeps<'_>,
    req: GetFnosThumbnailRequest,
) -> Result<FnosThumbnailResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
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
        chunk.map(|chunk| FnosThumbnailResponse { chunk: Some(chunk) })
    })))
}

crate::impl_has_playback_provider_access_fields!(FnosPlaybackProviderDeps<'a>);

impl<'a> FnosPlaybackProviderDeps<'a> {
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
    ) -> PlaybackTransportExecutorDeps<'a> {
        PlaybackTransportExecutorDeps {
            hls_rewrite: Some(HlsRewriteSigning {
                segment_base,
                claims,
                resource: "segments",
            }),
            ..self.chunk_deps()
        }
    }
}
