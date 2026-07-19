use futures::StreamExt;
use synctv_core::provider::{ExecutionControl, PlaybackTransportAction};
use synctv_core::service::RtmpPlaybackProviderService;
use synctv_proto::playback_provider::rtmp::{
    GetRtmpFlvStreamRequest, GetRtmpHlsPlaylistRequest, GetRtmpHlsSegmentRequest,
    RtmpFlvStreamResponse, RtmpHlsPlaylistResponse, RtmpHlsSegmentResponse,
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

const PROVIDER: &str = synctv_core::provider::RtmpProvider::NAME;

pub struct RtmpPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a RtmpPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub live_runtime: LivePlaybackApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type RtmpFlvStreamResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<RtmpFlvStreamResponse, ApiError>> + Send + 'static>,
>;
pub type RtmpHlsPlaylistResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<RtmpHlsPlaylistResponse, ApiError>> + Send + 'static>,
>;
pub type RtmpHlsSegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<RtmpHlsSegmentResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_rtmp_flv_stream(
    deps: RtmpPlaybackProviderDeps<'_>,
    req: GetRtmpFlvStreamRequest,
) -> Result<RtmpFlvStreamResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let action = resolve_rtmp_flv_stream_action(&deps, req).await?;
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
                    "RTMP FLV action resolved with unexpected provider".to_string(),
                ));
            }
            stream_live_flv_chunks(
                deps.live_deps(),
                LiveFlvChunksRequest {
                    provider_name: PROVIDER.to_string(),
                    room_id,
                    media_id,
                    user_id,
                    expires_at,
                    source_url: None,
                    head,
                },
            )
            .await?
        }
        other => playback_transport_action_to_chunk_stream(deps.chunk_deps(), other, head).await?,
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| RtmpFlvStreamResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_rtmp_hls_playlist(
    deps: RtmpPlaybackProviderDeps<'_>,
    req: GetRtmpHlsPlaylistRequest,
) -> Result<RtmpHlsPlaylistResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let signature_user_id = req.uid.clone();
    let signature_room_id = req.rid.clone();
    let signature_expires_at = req.exp;
    let action = resolve_rtmp_hls_playlist_action(&deps, req).await?;
    let stream = match action {
        PlaybackTransportAction::LiveHlsPlaylist {
            provider_name,
            room_id,
            media_id,
            version,
        } => {
            if provider_name != PROVIDER {
                return Err(ApiError::Internal(
                    "RTMP HLS playlist action resolved with unexpected provider".to_string(),
                ));
            }
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
                    route_provider: "rtmp".to_string(),
                    source_url: None,
                },
            )
            .await?
        }
        other => playback_transport_action_to_chunk_stream(deps.chunk_deps(), other, false).await?,
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| RtmpHlsPlaylistResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_rtmp_hls_segment(
    deps: RtmpPlaybackProviderDeps<'_>,
    req: GetRtmpHlsSegmentRequest,
) -> Result<RtmpHlsSegmentResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let action = resolve_rtmp_hls_segment_action(&deps, req).await?;
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
                    "RTMP HLS segment action resolved with unexpected provider".to_string(),
                ));
            }
            get_live_hls_segment_chunks(
                deps.live_deps(),
                LiveHlsSegmentChunksRequest {
                    room_id,
                    media_id,
                    segment_name,
                    source_url: None,
                    head,
                },
            )
            .await?
        }
        other => playback_transport_action_to_chunk_stream(deps.chunk_deps(), other, head).await?,
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| RtmpHlsSegmentResponse { chunk: Some(chunk) })
    })))
}

async fn resolve_rtmp_flv_stream_action(
    deps: &RtmpPlaybackProviderDeps<'_>,
    req: GetRtmpFlvStreamRequest,
) -> Result<PlaybackTransportAction, ApiError> {
    let (store, claims) = verify_rtmp_access(
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

async fn resolve_rtmp_hls_playlist_action(
    deps: &RtmpPlaybackProviderDeps<'_>,
    req: GetRtmpHlsPlaylistRequest,
) -> Result<PlaybackTransportAction, ApiError> {
    let (store, _) = verify_rtmp_access(
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

async fn resolve_rtmp_hls_segment_action(
    deps: &RtmpPlaybackProviderDeps<'_>,
    req: GetRtmpHlsSegmentRequest,
) -> Result<PlaybackTransportAction, ApiError> {
    let (store, _) = verify_rtmp_access(
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

async fn verify_rtmp_access(
    deps: &RtmpPlaybackProviderDeps<'_>,
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

crate::impl_has_playback_provider_access_fields!(RtmpPlaybackProviderDeps<'a>);

impl<'a> RtmpPlaybackProviderDeps<'a> {
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
}

crate::impl_has_live_playback_fields!(RtmpPlaybackProviderDeps<'a>);
