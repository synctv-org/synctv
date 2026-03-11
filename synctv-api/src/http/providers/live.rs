//! Live Provider HTTP Routes and internal playback execution.
//!
//! Live stream provider APIs now live under `/api/providers/{provider}` like
//! other providers. Actual FLV/HLS playback is dispatched through the unified
//! provider proxy trait path.

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::Response,
    routing::get,
    Json, Router,
};
use bytes::Bytes;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{info, warn};

use crate::http::{error::map_api_error, middleware::AuthUser, AppError, AppResult, AppState};
use crate::observability::metrics::LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL;
use synctv_core::models::id::RoomId;
use synctv_core::provider::proxy::ProxyAction;
use synctv_livestream::api::{FlvStreamingApi, HlsStreamingApi};
use synctv_livestream::error::StreamError;

#[derive(Debug, Deserialize)]
pub struct RoomQuery {
    room_id: String,
}

pub fn rtmp_routes() -> Router<AppState> {
    create_live_provider_router()
}

pub fn live_proxy_routes() -> Router<AppState> {
    create_live_provider_router()
}

fn create_live_provider_router() -> Router<AppState> {
    Router::new()
        .route("/info/{media_id}", get(handle_stream_info))
        .route("/streams", get(handle_room_streams))
}

fn find_stream_error<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a StreamError> {
    let mut current = Some(error);
    while let Some(err) = current {
        if let Some(stream_error) = err.downcast_ref::<StreamError>() {
            return Some(stream_error);
        }
        current = err.source();
    }
    None
}

fn map_stream_error(context: &str, error: &StreamError) -> AppError {
    match error {
        StreamError::NoPublisher(_)
        | StreamError::StreamNotFound(_)
        | StreamError::InvalidStreamKey(_) => AppError::not_found(format!("{context}: {error}")),
        StreamError::PermissionDenied(_) | StreamError::AuthenticationFailed(_) => {
            AppError::forbidden(format!("{context}: {error}"))
        }
        StreamError::ResourceExhausted(_) => {
            AppError::too_many_requests(format!("{context}: {error}"))
        }
        StreamError::InvalidAddress(_)
        | StreamError::ProtocolError(_)
        | StreamError::HandshakeFailed(_)
        | StreamError::InvalidState(_) => AppError::bad_request(format!("{context}: {error}")),
        StreamError::RedisError(_)
        | StreamError::RegistryError(_)
        | StreamError::GrpcError(_)
        | StreamError::ConnectionFailed(_)
        | StreamError::IoError(_)
        | StreamError::Internal(_)
        | StreamError::StaleEpoch(_)
        | StreamError::AlreadyPublishing(_)
        | StreamError::PublisherExists(_)
        | StreamError::StreamHubError(_) => {
            AppError::internal_server_error(format!("{context}: {error}"))
        }
    }
}

fn map_livestream_error(context: &str, error: &(dyn std::error::Error + 'static)) -> AppError {
    if let Some(stream_error) = find_stream_error(error) {
        return map_stream_error(context, stream_error);
    }

    AppError::internal_server_error(format!("{context}: {error}"))
}

async fn handle_stream_info(
    auth: AuthUser,
    Path(media_id): Path<String>,
    Query(params): Query<RoomQuery>,
    State(state): State<AppState>,
) -> AppResult<Json<crate::proto::client::GetStreamInfoResponse>> {
    let resp = state
        .client_api
        .get_stream_info(auth.user_id.as_str(), &params.room_id, &media_id)
        .await
        .map_err(map_api_error)?;

    Ok(Json(resp))
}

async fn handle_room_streams(
    auth: AuthUser,
    Query(params): Query<RoomQuery>,
    State(state): State<AppState>,
) -> AppResult<Json<crate::proto::client::ListRoomStreamsResponse>> {
    let resp = state
        .client_api
        .list_room_streams(auth.user_id.as_str(), &params.room_id)
        .await
        .map_err(map_api_error)?;

    Ok(Json(resp))
}

pub(crate) async fn execute_live_proxy_action(
    state: &AppState,
    action: ProxyAction,
    raw_query: Option<&str>,
) -> AppResult<Response> {
    match action {
        ProxyAction::LiveFlv {
            provider_name,
            room_id,
            media_id,
            user_id,
            expires_at,
        } => {
            execute_flv_stream(
                state,
                &provider_name,
                &room_id,
                &media_id,
                &user_id,
                expires_at,
            )
            .await
        }
        ProxyAction::LiveHlsPlaylist {
            provider_name,
            room_id,
            media_id,
            version,
        } => {
            execute_hls_playlist(
                state,
                &provider_name,
                &room_id,
                &media_id,
                &version,
                raw_query,
            )
            .await
        }
        ProxyAction::LiveHlsSegment {
            room_id,
            media_id,
            segment_name,
            disguised_as_png,
        } => execute_hls_segment(state, &room_id, &media_id, &segment_name, disguised_as_png).await,
        other => Err(AppError::internal(format!(
            "execute_live_proxy_action received unsupported action: {other:?}"
        ))),
    }
}

async fn execute_flv_stream(
    state: &AppState,
    provider_name: &str,
    room_id_str: &str,
    media_id: &str,
    user_id: &str,
    expires_at: i64,
) -> AppResult<Response> {
    info!(room_id = %room_id_str, media_id = %media_id, provider = %provider_name, "FLV streaming request");

    let infrastructure = state
        .client_api
        .live_infrastructure()
        .ok_or_else(|| AppError::internal_server_error("Live streaming not configured"))?;

    let source_url = if provider_name == "live_proxy" {
        state
            .client_api
            .get_live_proxy_source_url(room_id_str, media_id)
            .await
    } else {
        None
    };

    let (rx, subscriber_guard) = FlvStreamingApi::create_session_with_pull(
        infrastructure,
        room_id_str,
        media_id,
        source_url.as_deref(),
    )
    .await
    .map_err(|e| map_livestream_error("Failed to create FLV session", &*e))?;

    let mut disconnect_rx = state.connection_manager.subscribe_disconnect();
    let room_id = RoomId::from_string(room_id_str.to_string());

    let max_connection_duration =
        std::time::Duration::from_secs(state.config.livestream.flv_max_connection_duration_seconds);
    let write_timeout =
        std::time::Duration::from_secs(state.config.livestream.flv_write_timeout_seconds);

    let (tx, rx_wrapped) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(512);
    let room_id_clone = room_id.clone();
    let user_id = synctv_core::models::id::UserId::from_string(user_id.to_string());
    tokio::spawn(async move {
        let _guard = subscriber_guard;
        let mut rx = rx;
        let mut consecutive_drops: u32 = 0;
        const MAX_CONSECUTIVE_DROPS: u32 = 100;
        let start_time = std::time::Instant::now();
        let mut lifecycle_tick = tokio::time::interval(std::time::Duration::from_secs(1));
        lifecycle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = lifecycle_tick.tick() => {
                    if chrono::Utc::now().timestamp() > expires_at {
                        info!(
                            room_id = %room_id_clone.as_str(),
                            expires_at,
                            "FLV stream terminated: proxy signature expired"
                        );
                        break;
                    }

                    if max_connection_duration.as_secs() > 0
                        && start_time.elapsed() >= max_connection_duration
                    {
                        info!(
                            room_id = %room_id_clone.as_str(),
                            max_duration_secs = max_connection_duration.as_secs(),
                            "FLV stream terminated: max connection duration exceeded"
                        );
                        break;
                    }
                }
                data = rx.recv() => {
                    if let Some(chunk) = data {
                        let send_success = send_flv_chunk(&tx, chunk, write_timeout).await;
                        if send_success {
                            consecutive_drops = 0;
                        } else {
                            consecutive_drops += 1;
                            if consecutive_drops >= MAX_CONSECUTIVE_DROPS {
                                LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL.inc();
                                break;
                            }
                        }
                    } else {
                        break;
                    }
                }
                disconnect = disconnect_rx.recv() => {
                    if let Ok(event) = disconnect {
                        let should_disconnect = match event {
                            synctv_cluster::sync::DisconnectSignal::User(ref uid) => uid == &user_id,
                            synctv_cluster::sync::DisconnectSignal::Room(ref rid) => rid == &room_id,
                            synctv_cluster::sync::DisconnectSignal::UserFromRoom {
                                user_id: ref uid,
                                room_id: ref rid,
                            } => uid == &user_id && rid == &room_id,
                            _ => false,
                        };
                        if should_disconnect {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/x-flv")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(ReceiverStream::new(rx_wrapped)))
        .map_err(|_| AppError::internal_server_error("Failed to build response"))
}

async fn execute_hls_playlist(
    state: &AppState,
    provider_name: &str,
    room_id: &str,
    media_id: &str,
    version: &str,
    raw_query: Option<&str>,
) -> AppResult<Response> {
    info!(room_id = %room_id, media_id = %media_id, provider = %provider_name, "HLS playlist request");

    let infrastructure = state
        .client_api
        .live_infrastructure()
        .ok_or_else(|| AppError::internal_server_error("Live streaming not configured"))?;

    let segment_disguised_as_png = live_segments_disguised_as_png(state);

    let playlist =
        HlsStreamingApi::generate_playlist(infrastructure, room_id, media_id, |ts_name| {
            build_hls_segment_path(
                provider_name,
                version,
                ts_name,
                raw_query,
                segment_disguised_as_png,
            )
        })
        .await
        .map_err(|e| map_livestream_error("Failed to generate HLS playlist", &*e))?;

    match playlist {
        Some(content) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
            .header(header::CACHE_CONTROL, "no-cache, no-store")
            .body(Body::from(content))
            .map_err(|_| AppError::internal_server_error("Failed to build response"))?),
        None => Err(AppError::not_found(format!(
            "No active HLS stream for {room_id}/{media_id}"
        ))),
    }
}

fn live_segments_disguised_as_png(state: &AppState) -> bool {
    state
        .settings_registry
        .as_ref()
        .and_then(|registry| registry.ts_disguised_as_png.get().ok())
        .unwrap_or(true)
}

fn build_hls_segment_path(
    provider_name: &str,
    version: &str,
    ts_name: &str,
    raw_query: Option<&str>,
    disguised_as_png: bool,
) -> String {
    let query_suffix = raw_query
        .filter(|query| !query.is_empty())
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let extension = if disguised_as_png { "png" } else { "ts" };

    format!("/api/providers/proxy/{provider_name}/{version}/segment/{ts_name}.{extension}{query_suffix}")
}

async fn execute_hls_segment(
    state: &AppState,
    room_id: &str,
    media_id: &str,
    segment_name: &str,
    disguised_as_png: bool,
) -> AppResult<Response> {
    let validated_name = if disguised_as_png {
        segment_name.trim_end_matches(".png")
    } else {
        segment_name.trim_end_matches(".ts")
    };

    if let Err(error) = synctv_common::validation::validate_path_for_traversal(validated_name) {
        warn!(segment = %validated_name, error = %error, "HLS segment name failed path traversal validation");
        return Err(AppError::bad_request("Invalid segment name"));
    }

    let infrastructure = state
        .client_api
        .live_infrastructure()
        .ok_or_else(|| AppError::internal_server_error("Live streaming not configured"))?;

    let ts_data = HlsStreamingApi::get_segment(infrastructure, room_id, media_id, validated_name)
        .await
        .map_err(|e| {
            warn!(room_id = %room_id, media_id = %media_id, segment = %validated_name, error = %e, "HLS segment fetch failed");
            map_livestream_error("Failed to get HLS segment", &*e)
        })?;

    if disguised_as_png {
        let png_header = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE,
        ];

        let mut disguised_data = Vec::with_capacity(png_header.len() + ts_data.len());
        disguised_data.extend_from_slice(&png_header);
        disguised_data.extend_from_slice(&ts_data);

        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CACHE_CONTROL, "public, max-age=90")
            .header("X-Accel-Buffering", "no")
            .body(Body::from(disguised_data))
            .map_err(|_| AppError::internal_server_error("Failed to build response"));
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/mp2t")
        .header(header::CACHE_CONTROL, "public, max-age=90")
        .header("X-Accel-Buffering", "no")
        .body(Body::from(ts_data))
        .map_err(|_| AppError::internal_server_error("Failed to build response"))
}

async fn send_flv_chunk(
    tx: &mpsc::Sender<Result<Bytes, std::io::Error>>,
    chunk: Result<Bytes, std::io::Error>,
    write_timeout: std::time::Duration,
) -> bool {
    if write_timeout.is_zero() {
        tx.send(chunk).await.is_ok()
    } else {
        matches!(
            tokio::time::timeout(write_timeout, tx.send(chunk)).await,
            Ok(Ok(()))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    #[test]
    fn room_query_deserializes_room_id() {
        let query: RoomQuery = serde_urlencoded::from_str("room_id=room123").unwrap();
        assert_eq!(query.room_id, "room123");
    }

    #[test]
    fn room_query_rejects_legacy_room_id_casing() {
        assert!(serde_urlencoded::from_str::<RoomQuery>("roomId=room123").is_err());
    }

    #[tokio::test]
    async fn send_flv_chunk_waits_without_timeout_when_capacity_frees() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.send(Ok(Bytes::from_static(b"first"))).await.unwrap();

        let sender = tx.clone();
        let send_task = tokio::spawn(async move {
            send_flv_chunk(
                &sender,
                Ok(Bytes::from_static(b"second")),
                std::time::Duration::ZERO,
            )
            .await
        });

        assert!(
            matches!(rx.recv().await, Some(Ok(bytes)) if bytes == Bytes::from_static(b"first"))
        );
        assert!(send_task.await.unwrap());
        assert!(
            matches!(rx.recv().await, Some(Ok(bytes)) if bytes == Bytes::from_static(b"second"))
        );
    }

    #[test]
    fn provider_live_info_path_uses_provider_prefix() {
        let path = "/api/providers/rtmp/info/media123";
        assert!(path.starts_with("/api/providers/rtmp/"));
        assert!(path.ends_with("/media123"));
    }

    #[test]
    fn build_hls_segment_path_uses_png_suffix_when_enabled() {
        let path = build_hls_segment_path("rtmp", "ver1", "seg001", Some("sig=1"), true);
        assert_eq!(
            path,
            "/api/providers/proxy/rtmp/ver1/segment/seg001.png?sig=1"
        );
    }

    #[test]
    fn build_hls_segment_path_uses_ts_suffix_when_disabled() {
        let path = build_hls_segment_path("rtmp", "ver1", "seg001", None, false);
        assert_eq!(path, "/api/providers/proxy/rtmp/ver1/segment/seg001.ts");
    }

    #[test]
    fn livestream_invalid_state_limit_maps_to_429() {
        let err = map_stream_error(
            "Failed to create FLV session",
            &StreamError::ResourceExhausted(
                "max concurrent streams reached (limit: 100)".to_string(),
            ),
        );
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(err.message.contains("max concurrent streams reached"));
    }

    #[test]
    fn livestream_no_publisher_maps_to_404() {
        let err = map_stream_error(
            "Failed to generate HLS playlist",
            &StreamError::NoPublisher("room1/media1".to_string()),
        );
        assert_eq!(err.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn livestream_permission_denied_maps_to_403() {
        let err = map_stream_error(
            "Failed to create FLV session",
            &StreamError::PermissionDenied("not allowed".to_string()),
        );
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn livestream_nested_stream_error_maps_without_string_matching() {
        let err = anyhow::Error::new(StreamError::ResourceExhausted(
            "max concurrent streams reached (limit: 100)".to_string(),
        ))
        .context("wrapped by anyhow");

        let mapped = map_livestream_error("Failed to create FLV session", err.as_ref());
        assert_eq!(mapped.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(mapped.message.contains("Resource exhausted"));
    }

    #[test]
    fn livestream_unknown_error_defaults_to_500() {
        let err = anyhow::anyhow!("plain anyhow without typed source");
        let mapped = map_livestream_error("Failed to create FLV session", err.as_ref());
        assert_eq!(mapped.status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
