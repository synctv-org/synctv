use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::TwitchPlaybackProviderService;
use synctv_proto::playback_provider::twitch::{
    GetTwitchResourceRequest, GetTwitchSegmentRequest, TwitchChatEvent, TwitchResourceResponse,
    TwitchSegmentResponse, WatchTwitchChatRequest,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::TwitchProvider::NAME;

pub struct TwitchPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a TwitchPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type TwitchResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TwitchResourceResponse, ApiError>> + Send + 'static>,
>;
pub type TwitchSegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TwitchSegmentResponse, ApiError>> + Send + 'static>,
>;
pub type TwitchChatEventStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TwitchChatEvent, ApiError>> + Send + 'static>,
>;

pub async fn get_twitch_resource(
    deps: TwitchPlaybackProviderDeps<'_>,
    req: GetTwitchResourceRequest,
) -> Result<TwitchResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
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
        .map_err(|error| {
            tracing::error!(
                error = %error,
                version = %req.version,
                mode_name = %req.mode_name,
                media_index = req.media_index,
                "Failed to resolve Twitch playback resource"
            );
            ApiError::from(error)
        })?;
    let segment_base = playback_provider_route_base(&req.rid, "twitch", &req.version, "segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims),
        action,
        head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| TwitchResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_twitch_segment(
    deps: TwitchPlaybackProviderDeps<'_>,
    req: GetTwitchSegmentRequest,
) -> Result<TwitchSegmentResponseStream, ApiError> {
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
    let segment_base = playback_provider_route_base(&req.rid, "twitch", &req.version, "segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| TwitchSegmentResponse { chunk: Some(chunk) })
    })))
}

pub async fn watch_twitch_chat(
    deps: TwitchPlaybackProviderDeps<'_>,
    req: WatchTwitchChatRequest,
) -> Result<TwitchChatEventStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("chats/{}/{}", req.mode_name, req.media_index),
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
        .watch_chat(
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
            .map(|event| TwitchChatEvent {
                id: event.id,
                user_name: event.user_name,
                text: event.text,
                color: event.color,
                badges: event.badges,
                sent_at_ms: event.sent_at_ms,
            })
            .map_err(ApiError::from)
    })))
}

crate::impl_has_playback_provider_access_fields!(TwitchPlaybackProviderDeps<'a>);

impl<'a> TwitchPlaybackProviderDeps<'a> {
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
