//! Live provider HTTP transport adapter.
//!
//! Live playback execution is owned by `impls::playback_provider`. This module
//! adapts provider HTTP `PlaybackTransportAction` calls into semantic live
//! playback chunk streams and wraps those chunks as HTTP responses.

use axum::http::Method;
use synctv_core::models::{MediaId, RoomId};
use synctv_core::provider::PlaybackTransportAction;

use crate::http::providers::playback_provider::transport::stream_chunk_http_response;
use crate::http::{AppError, AppResult, AppState};
use crate::impls::playback_provider::common::{
    get_live_hls_playlist_chunks, get_live_hls_segment_chunks, stream_live_flv_chunks,
    LiveFlvChunksRequest, LiveHlsPlaylistChunksRequest, LiveHlsSegmentChunksRequest,
    LivePlaybackDeps,
};
use crate::proxy_signature::ProxySigningKeyQueryExt;

pub(crate) async fn execute_live_stream_action(
    state: &AppState,
    action: PlaybackTransportAction,
    raw_query: Option<&str>,
) -> AppResult<axum::response::Response> {
    let stream = match action {
        PlaybackTransportAction::LiveFlv {
            provider_name,
            room_id,
            media_id,
            user_id,
            expires_at,
        } => {
            let source_url =
                live_proxy_source_url(state, &provider_name, &room_id, &media_id).await;
            stream_live_flv_chunks(
                live_deps(state),
                LiveFlvChunksRequest {
                    provider_name,
                    room_id,
                    media_id,
                    user_id,
                    expires_at,
                    source_url,
                    head: false,
                },
            )
            .await
            .map_err(crate::http::error::map_api_error)?
        }
        PlaybackTransportAction::LiveHlsPlaylist {
            provider_name,
            room_id,
            media_id,
            version,
        } => {
            let source_url =
                live_proxy_source_url(state, &provider_name, &room_id, &media_id).await;
            let (signature_user_id, signature_room_id, signature_expires_at) = live_hls_signature(
                state,
                &provider_name,
                &version,
                raw_query.unwrap_or_default(),
            )?;
            let route_provider = match provider_name.as_str() {
                synctv_core::provider::LiveProxyProvider::NAME => "live-proxy".to_string(),
                other => other.to_string(),
            };
            get_live_hls_playlist_chunks(
                live_deps(state),
                LiveHlsPlaylistChunksRequest {
                    provider_name,
                    room_id,
                    media_id,
                    version,
                    signature_user_id,
                    signature_room_id,
                    signature_expires_at,
                    route_provider,
                    source_url,
                },
            )
            .await
            .map_err(crate::http::error::map_api_error)?
        }
        PlaybackTransportAction::LiveHlsSegment {
            provider_name,
            room_id,
            media_id,
            segment_name,
            disguised_as_png: _,
        } => {
            let source_url =
                live_proxy_source_url(state, &provider_name, &room_id, &media_id).await;
            get_live_hls_segment_chunks(
                live_deps(state),
                LiveHlsSegmentChunksRequest {
                    room_id,
                    media_id,
                    segment_name,
                    source_url,
                    head: false,
                },
            )
            .await
            .map_err(crate::http::error::map_api_error)?
        }
        other => {
            tracing::error!(action = ?other, "execute_live_stream_action received unsupported action");
            return Err(AppError::internal_server_error(
                "Unsupported live stream playback transport action",
            ));
        }
    };

    stream_chunk_http_response(stream, Method::GET).await
}

fn live_deps(state: &AppState) -> LivePlaybackDeps<'_> {
    LivePlaybackDeps {
        proxy_signing_key: &state.shared_api_runtime.proxy_signing_key,
        live_streaming_infrastructure: state.shared_api_runtime.client_api.live_infrastructure(),
        connection_runtime: state.connection_manager.as_ref(),
        livestream_config: &state.config.livestream,
        runtime_settings_store: state.runtime_settings_store.as_deref(),
    }
}

fn live_hls_signature(
    state: &AppState,
    provider_name: &str,
    version: &str,
    raw_query: &str,
) -> Result<(String, String, i64), AppError> {
    let (_sig, uid, rid, exp) =
        crate::http::providers::playback_provider::transport::signed_query_fields(raw_query)
            .map_err(crate::http::error::map_api_error)?;
    state
        .shared_api_runtime
        .proxy_signing_key
        .parse_and_verify_query(raw_query, provider_name, version, "hls-playlist")
        .map_err(|_| AppError::unauthorized("Invalid playback provider signature"))?;
    Ok((uid, rid, exp))
}

async fn live_proxy_source_url(
    state: &AppState,
    provider_name: &str,
    room_id: &RoomId,
    media_id: &MediaId,
) -> Option<String> {
    if provider_name != synctv_core::provider::LiveProxyProvider::NAME {
        return None;
    }

    state
        .shared_api_runtime
        .client_api
        .get_live_proxy_source_url(room_id, media_id)
        .await
}
