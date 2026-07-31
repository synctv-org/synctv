use futures::StreamExt;
use synctv_core::provider::{ExecutionControl, PlaybackTransportAction};
use synctv_core::service::LiveProxyPlaybackProviderService;
use synctv_proto::playback_provider::live_proxy::{
    GetLiveProxyFlvStreamRequest, GetLiveProxyHlsPlaylistRequest, GetLiveProxyHlsSegmentRequest,
    LiveProxyFlvStreamResponse, LiveProxyHlsPlaylistResponse, LiveProxyHlsSegmentResponse,
};

use super::common::{
    get_live_hls_playlist_chunks, get_live_hls_segment_chunks, live_flv_access_from_claims,
    playback_transport_action_to_chunk_stream, stream_live_flv_chunks,
    verify_playback_provider_access_with_deps, HasLivePlaybackFields,
    HasPlaybackProviderAccessFields, LiveFlvChunksRequest, LiveHlsPlaylistChunksRequest,
    LiveHlsSegmentChunksRequest, LivePlaybackApiRuntime, PlaybackProviderAccessRequest,
    PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::LiveProxyProvider::NAME;

pub struct LiveProxyPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a LiveProxyPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub live_runtime: LivePlaybackApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type LiveProxyFlvStreamResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<LiveProxyFlvStreamResponse, ApiError>> + Send + 'static>,
>;
pub type LiveProxyHlsPlaylistResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<LiveProxyHlsPlaylistResponse, ApiError>> + Send + 'static,
    >,
>;
pub type LiveProxyHlsSegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<LiveProxyHlsSegmentResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_live_proxy_flv_stream(
    deps: LiveProxyPlaybackProviderDeps<'_>,
    req: GetLiveProxyFlvStreamRequest,
) -> Result<LiveProxyFlvStreamResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let action = resolve_live_proxy_flv_stream_action(&deps, req).await?;
    let stream = match action {
        PlaybackTransportAction::LiveFlv {
            provider_name,
            room_id,
            media_id,
            user_id,
            expires_at,
        } => {
            if provider_name != PROVIDER {
                return Err(ApiError::Internal(
                    "LiveProxy FLV action resolved with unexpected provider".to_string(),
                ));
            }
            let external_source = deps.live_proxy_source(&room_id, &media_id).await;
            stream_live_flv_chunks(
                deps.live_deps(),
                LiveFlvChunksRequest {
                    provider_name: PROVIDER.to_string(),
                    room_id,
                    media_id,
                    user_id,
                    expires_at,
                    external_source,
                    head,
                },
            )
            .await?
        }
        other => playback_transport_action_to_chunk_stream(deps.chunk_deps(), other, head).await?,
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| LiveProxyFlvStreamResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_live_proxy_hls_playlist(
    deps: LiveProxyPlaybackProviderDeps<'_>,
    req: GetLiveProxyHlsPlaylistRequest,
) -> Result<LiveProxyHlsPlaylistResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let signature_user_id = req.uid.clone();
    let signature_room_id = req.rid.clone();
    let signature_expires_at = req.exp;
    let action = resolve_live_proxy_hls_playlist_action(&deps, req).await?;
    let stream = match action {
        PlaybackTransportAction::LiveHlsPlaylist {
            provider_name,
            room_id,
            media_id,
            version,
        } => {
            if provider_name != PROVIDER {
                return Err(ApiError::Internal(
                    "LiveProxy HLS playlist action resolved with unexpected provider".to_string(),
                ));
            }
            let external_source = deps.live_proxy_source(&room_id, &media_id).await;
            get_live_hls_playlist_chunks(
                deps.live_deps(),
                LiveHlsPlaylistChunksRequest {
                    provider_name: PROVIDER.to_string(),
                    room_id,
                    media_id,
                    version,
                    signature_user_id,
                    signature_room_id,
                    signature_expires_at,
                    route_provider: "live-proxy".to_string(),
                    external_source,
                },
            )
            .await?
        }
        other => playback_transport_action_to_chunk_stream(deps.chunk_deps(), other, false).await?,
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| LiveProxyHlsPlaylistResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_live_proxy_hls_segment(
    deps: LiveProxyPlaybackProviderDeps<'_>,
    req: GetLiveProxyHlsSegmentRequest,
) -> Result<LiveProxyHlsSegmentResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let action = resolve_live_proxy_hls_segment_action(&deps, req).await?;
    let stream = match action {
        PlaybackTransportAction::LiveHlsSegment {
            provider_name,
            room_id,
            media_id,
            segment_name,
            disguised_as_png: _,
        } => {
            if provider_name != PROVIDER {
                return Err(ApiError::Internal(
                    "LiveProxy HLS segment action resolved with unexpected provider".to_string(),
                ));
            }
            let external_source = deps.live_proxy_source(&room_id, &media_id).await;
            get_live_hls_segment_chunks(
                deps.live_deps(),
                LiveHlsSegmentChunksRequest {
                    room_id,
                    media_id,
                    segment_name,
                    external_source,
                    head,
                },
            )
            .await?
        }
        other => playback_transport_action_to_chunk_stream(deps.chunk_deps(), other, head).await?,
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| LiveProxyHlsSegmentResponse { chunk: Some(chunk) })
    })))
}

async fn resolve_live_proxy_flv_stream_action(
    deps: &LiveProxyPlaybackProviderDeps<'_>,
    req: GetLiveProxyFlvStreamRequest,
) -> Result<PlaybackTransportAction, ApiError> {
    let (store, claims) = verify_live_proxy_access(
        deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: "flv-stream".to_string(),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let access = live_flv_access_from_claims(deps.runtime.public_id_codec, &claims)?;
    deps.playback_provider_service
        .flv_stream_action(&req.version, store, access, deps.request_control)
        .await
        .map_err(ApiError::from)
}

async fn resolve_live_proxy_hls_playlist_action(
    deps: &LiveProxyPlaybackProviderDeps<'_>,
    req: GetLiveProxyHlsPlaylistRequest,
) -> Result<PlaybackTransportAction, ApiError> {
    let (store, _) = verify_live_proxy_access(
        deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: "hls-playlist".to_string(),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    deps.playback_provider_service
        .hls_playlist_action(&req.version, store, deps.request_control)
        .await
        .map_err(ApiError::from)
}

async fn resolve_live_proxy_hls_segment_action(
    deps: &LiveProxyPlaybackProviderDeps<'_>,
    req: GetLiveProxyHlsSegmentRequest,
) -> Result<PlaybackTransportAction, ApiError> {
    let (store, _) = verify_live_proxy_access(
        deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("hls-segments/{}", req.segment_name),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    deps.playback_provider_service
        .hls_segment_action(&req.version, &req.segment_name, store, deps.request_control)
        .await
        .map_err(ApiError::from)
}

async fn verify_live_proxy_access(
    deps: &LiveProxyPlaybackProviderDeps<'_>,
    request: PlaybackProviderAccessRequest<'_>,
) -> Result<
    (
        std::sync::Arc<dyn synctv_core::provider::ProviderStore>,
        crate::proxy_signature::ProxyUrlClaims,
    ),
    ApiError,
> {
    verify_playback_provider_access_with_deps(&deps.access_deps(), PROVIDER, request).await
}

crate::impl_has_playback_provider_access_fields!(LiveProxyPlaybackProviderDeps<'a>);

impl<'a> LiveProxyPlaybackProviderDeps<'a> {
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

    async fn live_proxy_source(
        &self,
        room_id: &synctv_core::models::RoomId,
        media_id: &synctv_core::models::MediaId,
    ) -> Option<synctv_core::models::LiveProxyMediaSourceConfig> {
        self.playback_provider_service
            .source_config_for_media(room_id, media_id)
            .await
    }
}

crate::impl_has_live_playback_fields!(LiveProxyPlaybackProviderDeps<'a>);
