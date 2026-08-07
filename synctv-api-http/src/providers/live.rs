//! Live provider HTTP transport adapter.
//!
//! Live playback execution is owned by the common `playback_provider` domain. This module
//! adapts provider HTTP `PlaybackTransportAction` calls into semantic live
//! playback chunk streams and wraps those chunks as HTTP responses.

use axum::http::Method;
use synctv_core::models::{MediaId, RoomId};
use synctv_core::provider::PlaybackTransportAction;

use crate::http::{AppError, AppResult, AppState};
use crate::providers::playback_provider::transport::stream_chunk_http_response;
use synctv_api_common::playback_provider::common::{
    get_live_hls_master_chunks, get_live_hls_playlist_chunks, get_live_hls_segment_chunks,
    stream_live_flv_chunks, LiveFlvChunksRequest, LiveHlsMasterChunksRequest,
    LiveHlsPlaylistChunksRequest, LiveHlsSegmentChunksRequest, LivePlaybackDeps,
};
use synctv_api_common::proxy_signature::ProxySigningKeyQueryExt;

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
            let external_source =
                live_proxy_source(state, &provider_name, &room_id, &media_id).await;
            stream_live_flv_chunks(
                live_deps(state),
                LiveFlvChunksRequest {
                    provider_name,
                    room_id,
                    media_id,
                    user_id,
                    expires_at,
                    external_source,
                    head: false,
                },
            )
            .await
            .map_err(crate::http::error::map_api_error)?
        }
        PlaybackTransportAction::LiveHlsMaster {
            provider_name,
            room_id,
            media_id,
            version,
        } => {
            let external_source =
                live_proxy_source(state, &provider_name, &room_id, &media_id).await;
            let (signature_user_id, signature_room_id, signature_expires_at) = live_hls_signature(
                state,
                &provider_name,
                &version,
                "hls-master",
                raw_query.unwrap_or_default(),
            )?;
            let route_provider = match provider_name.as_str() {
                synctv_core::provider::LiveProxyProvider::NAME => "live-proxy".to_string(),
                other => other.to_string(),
            };
            get_live_hls_master_chunks(
                live_deps(state),
                LiveHlsMasterChunksRequest {
                    provider_name,
                    room_id,
                    media_id,
                    version,
                    signature_user_id,
                    signature_room_id,
                    signature_expires_at,
                    route_provider,
                    external_source,
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
            generation_id,
        } => {
            let resource = format!("hls/{generation_id}/index.m3u8");
            let (signature_user_id, signature_room_id, signature_expires_at) = live_hls_signature(
                state,
                &provider_name,
                &version,
                &resource,
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
                    generation_id,
                    signature_user_id,
                    signature_room_id,
                    signature_expires_at,
                    route_provider,
                },
            )
            .await
            .map_err(crate::http::error::map_api_error)?
        }
        PlaybackTransportAction::LiveHlsSegment {
            provider_name: _,
            room_id,
            media_id,
            generation_id,
            segment_name,
            disguised_as_png: _,
        } => get_live_hls_segment_chunks(
            live_deps(state),
            LiveHlsSegmentChunksRequest {
                room_id,
                media_id,
                generation_id,
                segment_name,
                head: false,
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?,
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
        livestream_config: &state.runtime_settings.livestream,
        runtime_settings_store: state.runtime_settings_store.as_deref(),
    }
}

fn live_hls_signature(
    state: &AppState,
    provider_name: &str,
    version: &str,
    resource: &str,
    raw_query: &str,
) -> Result<(String, String, i64), AppError> {
    let (_sig, uid, rid, exp) =
        crate::providers::playback_provider::transport::signed_query_fields(raw_query)
            .map_err(crate::http::error::map_api_error)?;
    state
        .shared_api_runtime
        .proxy_signing_key
        .parse_and_verify_query(raw_query, provider_name, version, resource)
        .map_err(|_| AppError::unauthorized("Invalid playback provider signature"))?;
    Ok((uid, rid, exp))
}

async fn live_proxy_source(
    state: &AppState,
    provider_name: &str,
    room_id: &RoomId,
    media_id: &MediaId,
) -> Option<synctv_core::models::LiveProxyMediaSourceConfig> {
    if provider_name != synctv_core::provider::LiveProxyProvider::NAME {
        return None;
    }

    state
        .shared_api_runtime
        .client_api
        .get_live_proxy_source_config(room_id, media_id)
        .await
}
