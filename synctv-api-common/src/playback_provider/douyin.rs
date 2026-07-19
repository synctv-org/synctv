use futures::StreamExt;
use synctv_core::provider::{DouyinDanmakuEvent, ExecutionControl};
use synctv_core::service::DouyinPlaybackProviderService;
use synctv_proto::playback_provider::douyin::{
    douyin_danmaku_event, ChatEvent, DouyinDanmakuEvent as ProtoDanmakuEvent,
    DouyinResourceResponse, DouyinSegmentResponse, GetDouyinResourceRequest,
    GetDouyinSegmentRequest, StreamClosedEvent, WatchDouyinDanmakuRequest,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::DouyinProvider::NAME;

pub struct DouyinPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a DouyinPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type DouyinResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DouyinResourceResponse, ApiError>> + Send + 'static>,
>;
pub type DouyinSegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DouyinSegmentResponse, ApiError>> + Send + 'static>,
>;
pub type DouyinDanmakuEventStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<ProtoDanmakuEvent, ApiError>> + Send + 'static>,
>;

pub async fn get_douyin_resource(
    deps: DouyinPlaybackProviderDeps<'_>,
    req: GetDouyinResourceRequest,
) -> Result<DouyinResourceResponseStream, ApiError> {
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
        chunk.map(|chunk| DouyinResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_douyin_segment(
    deps: DouyinPlaybackProviderDeps<'_>,
    req: GetDouyinSegmentRequest,
) -> Result<DouyinSegmentResponseStream, ApiError> {
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
    let segment_base = playback_provider_route_base(PROVIDER, &req.version, "segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| DouyinSegmentResponse { chunk: Some(chunk) })
    })))
}

pub async fn watch_douyin_danmaku(
    deps: DouyinPlaybackProviderDeps<'_>,
    req: WatchDouyinDanmakuRequest,
) -> Result<DouyinDanmakuEventStream, ApiError> {
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
            .map(|event| ProtoDanmakuEvent {
                event: Some(match event {
                    DouyinDanmakuEvent::Chat {
                        id,
                        user_id,
                        user_name,
                        text,
                        color,
                        sent_at_ms,
                    } => douyin_danmaku_event::Event::Chat(ChatEvent {
                        id,
                        user_id,
                        user_name,
                        text,
                        color,
                        sent_at_ms,
                    }),
                    DouyinDanmakuEvent::StreamClosed { action, message } => {
                        douyin_danmaku_event::Event::StreamClosed(StreamClosedEvent {
                            action,
                            message,
                        })
                    }
                }),
            })
            .map_err(ApiError::from)
    })))
}

crate::impl_has_playback_provider_access_fields!(DouyinPlaybackProviderDeps<'a>);

impl<'a> DouyinPlaybackProviderDeps<'a> {
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
