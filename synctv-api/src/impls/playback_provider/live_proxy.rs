use std::sync::Arc;

use futures::StreamExt;
use synctv_core::provider::{ExecutionControl, PlaybackTransportAction};
use synctv_core::service::LiveProxyPlaybackProviderService;
use synctv_proto::playback_provider::live_proxy::{
    GetLiveProxyFlvStreamRequest, GetLiveProxyHlsPlaylistRequest, GetLiveProxyHlsSegmentRequest,
    LiveProxyFlvStreamResponse, LiveProxyHlsPlaylistResponse, LiveProxyHlsSegmentResponse,
};

use super::common::{
    get_live_hls_playlist_chunks, get_live_hls_segment_chunks,
    playback_transport_action_to_chunk_stream, stream_live_flv_chunks,
    verify_playback_provider_access_with_deps, HasLivePlaybackFields,
    HasPlaybackProviderAccessFields, LiveFlvChunksRequest, LiveHlsPlaylistChunksRequest,
    LiveHlsSegmentChunksRequest, PlaybackProviderAccessRequest, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::LiveProxyProvider::NAME;

pub struct LiveProxyPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a LiveProxyPlaybackProviderService,
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
    pub live_streaming_infrastructure:
        Option<&'a Arc<synctv_livestream::LiveStreamingInfrastructure>>,
    pub connection_runtime: &'a dyn synctv_realtime::sync::ConnectionRuntime,
    pub livestream_config: &'a synctv_core::config::LivestreamConfig,
    pub settings_registry: Option<&'a synctv_core::service::SettingsRegistry>,
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
            let source_url = deps.live_proxy_source_url(&room_id, &media_id).await;
            stream_live_flv_chunks(
                deps.live_deps(),
                LiveFlvChunksRequest {
                    provider_name: PROVIDER.to_string(),
                    room_id,
                    media_id,
                    user_id,
                    expires_at,
                    source_url,
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
            let source_url = deps.live_proxy_source_url(&room_id, &media_id).await;
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
                    source_url,
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
            let source_url = deps.live_proxy_source_url(&room_id, &media_id).await;
            get_live_hls_segment_chunks(
                deps.live_deps(),
                LiveHlsSegmentChunksRequest {
                    room_id,
                    media_id,
                    segment_name,
                    source_url,
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
    deps.playback_provider_service
        .flv_stream_action(&req.version, store, &claims, deps.request_control)
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
        std::sync::Arc<dyn synctv_core::provider::store::ProviderStore>,
        synctv_core::proxy_signature::ProxyUrlClaims,
    ),
    ApiError,
> {
    verify_playback_provider_access_with_deps(&deps.access_deps(), PROVIDER, request).await
}

crate::impl_has_playback_provider_access_fields!(LiveProxyPlaybackProviderDeps<'a>);

impl<'a> LiveProxyPlaybackProviderDeps<'a> {
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

    async fn live_proxy_source_url(
        &self,
        room_id: &synctv_core::models::RoomId,
        media_id: &synctv_core::models::MediaId,
    ) -> Option<String> {
        self.playback_provider_service
            .source_url_for_media(room_id, media_id)
            .await
    }
}

crate::impl_has_live_playback_fields!(LiveProxyPlaybackProviderDeps<'a>);
