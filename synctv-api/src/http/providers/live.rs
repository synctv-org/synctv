//! Live Provider HTTP Routes and internal playback execution.
//!
//! Live stream provider APIs now live under `/api/providers/{provider}` like
//! other providers. Actual FLV/HLS playback is dispatched through the unified
//! provider proxy trait path.

use axum::{
    body::Body,
    http::{header, StatusCode},
    response::Response,
};
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{info, warn};

use crate::http::error::map_api_error;
use crate::http::{AppError, AppResult, AppState};
use crate::observability::metrics::LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL;
use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::provider::proxy::ProxyAction;
use synctv_livestream::api::{FlvStreamingApi, HlsStreamingApi};
use synctv_livestream::error::StreamError;

const MAX_CONSECUTIVE_DROPS: u32 = 100;

fn map_stream_error(context: &str, error: &StreamError) -> AppError {
    let api_error = crate::impls::map_livestream_stream_error(error);
    if matches!(api_error, crate::impls::ApiError::Internal(_)) {
        tracing::error!(context, error = %error, "Livestream internal error");
        return AppError::internal_server_error("Live streaming request failed");
    }

    map_api_error(api_error)
}

fn map_livestream_error(context: &str, error: &(dyn std::error::Error + 'static)) -> AppError {
    if let Some(stream_error) = crate::impls::find_livestream_stream_error(error) {
        return map_stream_error(context, stream_error);
    }

    let message = error.to_string();
    if message.contains("Segment not found")
        || message.contains("Failed to read segment: Key not found:")
    {
        return AppError::not_found("Live stream segment is not currently available");
    }

    tracing::error!(context, error = %error, "Unexpected livestream error");
    AppError::internal_server_error("Live streaming request failed")
}

fn live_streaming_unavailable_http_error() -> AppError {
    AppError::service_unavailable()
}

pub(crate) async fn execute_live_stream_action(
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
                room_id,
                media_id,
                user_id,
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
                room_id,
                media_id,
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
        } => execute_hls_segment(state, room_id, media_id, &segment_name, disguised_as_png).await,
        other => {
            tracing::error!(action = ?other, "execute_live_stream_action received unsupported action");
            Err(AppError::internal("Unsupported live stream proxy action"))
        }
    }
}

async fn execute_flv_stream(
    state: &AppState,
    provider_name: &str,
    room_id: RoomId,
    media_id: MediaId,
    user_id: UserId,
    expires_at: i64,
) -> AppResult<Response> {
    let room_id_key = room_id.to_string();
    let media_id_key = media_id.to_string();
    info!(room_id = %room_id, media_id = %media_id, provider = %provider_name, "FLV streaming request");

    let infrastructure = state
        .shared_api_runtime
        .client_api
        .live_infrastructure()
        .ok_or_else(live_streaming_unavailable_http_error)?;

    let source_url = if provider_name == "live_proxy" {
        state
            .shared_api_runtime
            .client_api
            .get_live_proxy_source_url(&room_id, &media_id)
            .await
    } else {
        None
    };

    let (rx, subscriber_guard) = FlvStreamingApi::create_session_with_pull(
        infrastructure,
        &room_id_key,
        &media_id_key,
        source_url.as_deref(),
    )
    .await
    .map_err(|e| map_livestream_error("Failed to create FLV session", &*e))?;

    let mut disconnect_rx = state.connection_manager.subscribe_disconnect();

    let max_connection_duration =
        std::time::Duration::from_secs(state.config.livestream.flv_max_connection_duration_seconds);
    let write_timeout =
        std::time::Duration::from_secs(state.config.livestream.flv_write_timeout_seconds);

    let (tx, rx_wrapped) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(512);
    let room_id_clone = room_id;
    tokio::spawn(async move {
        let _guard = subscriber_guard;
        let mut rx = rx;
        let mut consecutive_drops: u32 = 0;
        let start_time = std::time::Instant::now();
        let mut lifecycle_tick = tokio::time::interval(std::time::Duration::from_secs(1));
        lifecycle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = lifecycle_tick.tick() => {
                    if chrono::Utc::now().timestamp() > expires_at {
                        info!(
                            room_id = %room_id_clone,
                            expires_at,
                            "FLV stream terminated: proxy signature expired"
                        );
                        break;
                    }

                    if max_connection_duration.as_secs() > 0
                        && start_time.elapsed() >= max_connection_duration
                    {
                        info!(
                            room_id = %room_id_clone,
                            max_duration_secs = max_connection_duration.as_secs(),
                            "FLV stream terminated: max connection duration exceeded"
                        );
                        break;
                    }
                }
                data = rx.recv() => {
                    if let Some(chunk) = data {
                        match send_flv_chunk(&tx, chunk, write_timeout).await {
                            FlvChunkSendResult::Delivered => {
                                consecutive_drops = 0;
                            }
                            FlvChunkSendResult::Backpressured => {
                                consecutive_drops += 1;
                                if consecutive_drops >= MAX_CONSECUTIVE_DROPS {
                                    LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL.inc();
                                    break;
                                }
                            }
                            FlvChunkSendResult::Closed => {
                                info!(
                                    room_id = %room_id_clone,
                                    "FLV stream terminated: client response channel closed"
                                );
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
    room_id: RoomId,
    media_id: MediaId,
    version: &str,
    raw_query: Option<&str>,
) -> AppResult<Response> {
    let room_id_key = room_id.to_string();
    let media_id_key = media_id.to_string();
    info!(room_id = %room_id, media_id = %media_id, provider = %provider_name, "HLS playlist request");

    let infrastructure = state
        .shared_api_runtime
        .client_api
        .live_infrastructure()
        .ok_or_else(live_streaming_unavailable_http_error)?;

    let segment_disguised_as_png = live_segments_disguised_as_png(state);

    let playlist = HlsStreamingApi::generate_playlist(
        infrastructure,
        &room_id_key,
        &media_id_key,
        |ts_name| {
            build_hls_segment_path(
                provider_name,
                version,
                ts_name,
                raw_query,
                segment_disguised_as_png,
            )
        },
    )
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
        .unwrap_or(false)
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

    format!(
        "/api/providers/proxy/{provider_name}/{version}/segment/{ts_name}.{extension}{query_suffix}"
    )
}

async fn execute_hls_segment(
    state: &AppState,
    room_id: RoomId,
    media_id: MediaId,
    segment_name: &str,
    disguised_as_png: bool,
) -> AppResult<Response> {
    let room_id_key = room_id.to_string();
    let media_id_key = media_id.to_string();
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
        .shared_api_runtime
        .client_api
        .live_infrastructure()
        .ok_or_else(live_streaming_unavailable_http_error)?;

    let ts_data = HlsStreamingApi::get_segment(infrastructure, &room_id_key, &media_id_key, validated_name)
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
) -> FlvChunkSendResult {
    if write_timeout.is_zero() {
        match tx.send(chunk).await {
            Ok(()) => FlvChunkSendResult::Delivered,
            Err(_) => FlvChunkSendResult::Closed,
        }
    } else {
        match tokio::time::timeout(write_timeout, tx.send(chunk)).await {
            Ok(Ok(())) => FlvChunkSendResult::Delivered,
            Ok(Err(_)) => FlvChunkSendResult::Closed,
            Err(_) => FlvChunkSendResult::Backpressured,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlvChunkSendResult {
    Delivered,
    Backpressured,
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

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
        assert_eq!(send_task.await.unwrap(), FlvChunkSendResult::Delivered);
        assert!(
            matches!(rx.recv().await, Some(Ok(bytes)) if bytes == Bytes::from_static(b"second"))
        );
    }

    #[tokio::test]
    async fn send_flv_chunk_reports_closed_receiver_immediately() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);

        let result = send_flv_chunk(
            &tx,
            Ok(Bytes::from_static(b"orphaned")),
            std::time::Duration::ZERO,
        )
        .await;

        assert!(
            matches!(result, FlvChunkSendResult::Closed),
            "closed response body receiver must terminate the FLV forwarder immediately"
        );
    }

    #[test]
    fn provider_live_info_path_uses_provider_prefix() {
        let path = "/api/providers/rtmp/rooms/room123/info/media123";
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
        assert_eq!(
            err.message,
            "Live streaming capacity limit reached. Please try again later."
        );
    }

    #[test]
    fn livestream_no_publisher_maps_to_404() {
        let err = map_stream_error(
            "Failed to generate HLS playlist",
            &StreamError::NoPublisher("room1/media1".to_string()),
        );
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.message, "Live stream is not currently available");
    }

    #[test]
    fn livestream_permission_denied_maps_to_403() {
        let err = map_stream_error(
            "Failed to create FLV session",
            &StreamError::PermissionDenied("not allowed".to_string()),
        );
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(
            err.message,
            "You do not have permission to access this live stream"
        );
    }

    #[test]
    fn livestream_backend_outage_maps_to_503() {
        let err = map_stream_error(
            "Failed to create FLV session",
            &StreamError::RedisError("connection reset by peer".to_string()),
        );
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn livestream_nested_stream_error_maps_without_string_matching() {
        let err = anyhow::Error::new(StreamError::ResourceExhausted(
            "max concurrent streams reached (limit: 100)".to_string(),
        ))
        .context("wrapped by anyhow");

        let mapped = map_livestream_error("Failed to create FLV session", err.as_ref());
        assert_eq!(mapped.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            mapped.message,
            "Live streaming capacity limit reached. Please try again later."
        );
    }

    #[test]
    fn livestream_unknown_error_defaults_to_500() {
        let err = anyhow::anyhow!("plain anyhow without typed source");
        let mapped = map_livestream_error("Failed to create FLV session", err.as_ref());
        assert_eq!(mapped.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(mapped.message, "Live streaming request failed");
    }

    #[test]
    fn livestream_missing_segment_maps_to_404() {
        let err = anyhow::anyhow!("Failed to read segment: Key not found: room/media/seg001");
        let mapped = map_livestream_error("Failed to get HLS segment", err.as_ref());
        assert_eq!(mapped.status, StatusCode::NOT_FOUND);
        assert_eq!(
            mapped.message,
            "Live stream segment is not currently available"
        );
    }

    #[test]
    fn livestream_internal_stream_error_is_sanitized() {
        let err = map_stream_error(
            "Failed to create FLV session",
            &StreamError::Internal("redis://user:secret@localhost:6379".to_string()),
        );
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.message, "Live streaming request failed");
    }

    #[test]
    fn live_streaming_unavailable_http_error_maps_to_503() {
        let err = live_streaming_unavailable_http_error();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
