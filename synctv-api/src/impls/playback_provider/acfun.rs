use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::AcFunPlaybackProviderService;
use synctv_proto::playback_provider::acfun::{
    AcFunDanmakuEvent, AcFunDanmakuFileResponse, AcFunResourceResponse, AcFunSegmentResponse,
    GetAcFunDanmakuFileRequest, GetAcFunResourceRequest, GetAcFunSegmentRequest,
    WatchAcFunDanmakuRequest,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::AcFunProvider::NAME;

pub struct AcFunPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a AcFunPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type AcFunResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AcFunResourceResponse, ApiError>> + Send + 'static>,
>;
pub type AcFunSegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AcFunSegmentResponse, ApiError>> + Send + 'static>,
>;
pub type AcFunDanmakuFileResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AcFunDanmakuFileResponse, ApiError>> + Send + 'static>,
>;
pub type AcFunDanmakuEventStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AcFunDanmakuEvent, ApiError>> + Send + 'static>,
>;

pub async fn get_acfun_resource(
    deps: AcFunPlaybackProviderDeps<'_>,
    req: GetAcFunResourceRequest,
) -> Result<AcFunResourceResponseStream, ApiError> {
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
        chunk.map(|chunk| AcFunResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_acfun_segment(
    deps: AcFunPlaybackProviderDeps<'_>,
    req: GetAcFunSegmentRequest,
) -> Result<AcFunSegmentResponseStream, ApiError> {
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
        chunk.map(|chunk| AcFunSegmentResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_acfun_danmaku_file(
    deps: AcFunPlaybackProviderDeps<'_>,
    req: GetAcFunDanmakuFileRequest,
) -> Result<AcFunDanmakuFileResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("danmaku-files/{}/{}", req.mode_name, req.media_index),
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
        .danmaku_file_action(
            &req.version,
            &req.mode_name,
            req.media_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, req.head).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AcFunDanmakuFileResponse { chunk: Some(chunk) })
    })))
}

pub async fn watch_acfun_danmaku(
    deps: AcFunPlaybackProviderDeps<'_>,
    req: WatchAcFunDanmakuRequest,
) -> Result<AcFunDanmakuEventStream, ApiError> {
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
            .map(|event| AcFunDanmakuEvent {
                id: event.id,
                user_id: event.user_id,
                user_name: event.user_name,
                avatar_url: event.avatar_url,
                text: event.text,
                color: event.color,
                badge_name: event.badge_name,
                badge_level: event.badge_level,
                sent_at_ms: event.sent_at_ms,
            })
            .map_err(ApiError::from)
    })))
}

crate::impl_has_playback_provider_access_fields!(AcFunPlaybackProviderDeps<'a>);

impl<'a> AcFunPlaybackProviderDeps<'a> {
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
