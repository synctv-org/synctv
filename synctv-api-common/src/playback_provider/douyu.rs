use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::DouyuPlaybackProviderService;
use synctv_proto::playback_provider::douyu::{
    DouyuDanmakuEvent, DouyuResourceResponse, DouyuSegmentResponse, GetDouyuResourceRequest,
    GetDouyuSegmentRequest, WatchDouyuDanmakuRequest,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::DouyuProvider::NAME;

pub struct DouyuPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a DouyuPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type DouyuResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DouyuResourceResponse, ApiError>> + Send + 'static>,
>;
pub type DouyuSegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DouyuSegmentResponse, ApiError>> + Send + 'static>,
>;
pub type DouyuDanmakuEventStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DouyuDanmakuEvent, ApiError>> + Send + 'static>,
>;

pub async fn get_douyu_resource(
    deps: DouyuPlaybackProviderDeps<'_>,
    req: GetDouyuResourceRequest,
) -> Result<DouyuResourceResponseStream, ApiError> {
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
    let segment_base = playback_provider_route_base(&req.rid, PROVIDER, &req.version, "segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| DouyuResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_douyu_segment(
    deps: DouyuPlaybackProviderDeps<'_>,
    req: GetDouyuSegmentRequest,
) -> Result<DouyuSegmentResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (_, claims) = verify_playback_provider_access_with_deps(
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
        .segment_action(req.target_url, req.range.as_deref())
        .map_err(ApiError::from)?;
    let segment_base = playback_provider_route_base(&req.rid, PROVIDER, &req.version, "segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| DouyuSegmentResponse { chunk: Some(chunk) })
    })))
}

pub async fn watch_douyu_danmaku(
    deps: DouyuPlaybackProviderDeps<'_>,
    req: WatchDouyuDanmakuRequest,
) -> Result<DouyuDanmakuEventStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("danmakus/{}/{}", req.mode_name, req.media_index),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let stream = deps
        .playback_provider_service
        .watch_danmaku(
            &req.version,
            &req.mode_name,
            req.media_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    Ok(Box::pin(stream.map(|event| {
        event
            .map(|event| DouyuDanmakuEvent {
                id: event.id,
                user_id: event.user_id,
                user_name: event.user_name,
                text: event.text,
                color: event.color,
                level: event.level,
                badge_name: event.badge_name,
                badge_level: event.badge_level,
                sent_at_ms: event.sent_at_ms,
            })
            .map_err(ApiError::from)
    })))
}

crate::impl_has_playback_provider_access_fields!(DouyuPlaybackProviderDeps<'a>);

impl<'a> DouyuPlaybackProviderDeps<'a> {
    fn chunk_deps_with_hls(
        &self,
        segment_base: &'a str,
        claims: &'a crate::proxy_signature::ProxyUrlClaims,
    ) -> PlaybackTransportExecutorDeps<'a> {
        PlaybackTransportExecutorDeps {
            proxy_signing_key: self.runtime.proxy_signing_key,
            proxy_http_client: self.runtime.proxy_http_client,
            ssrf_guard: self.runtime.ssrf_guard,
            proxy_slice_cache: self.runtime.proxy_slice_cache,
            request_control: self.request_control,
            hls_rewrite: Some(HlsRewriteSigning {
                segment_base,
                claims,
                resource: "segments",
            }),
        }
    }
}
