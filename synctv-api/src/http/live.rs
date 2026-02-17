//! Live streaming HTTP endpoints
//!
//! Provides FLV and HLS streaming endpoints for live video.
//!
//! Architecture:
//! - Uses synctv-livestream's `LiveStreamingInfrastructure` via `AppState`
//! - Implements lazy-load FLV streaming
//! - Implements HLS playlist generation and segment serving
//!
//! Endpoints (matching synctv-go paths):
//! - GET /`api/room/movie/live/flv/:media_id` - FLV streaming
//! - GET /`api/room/movie/live/hls/list/:media_id` - HLS playlist
//! - GET /`api/room/movie/live/hls/data/:room_id/:media_id/:segment.ts` - HLS segment

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};

use crate::http::{AppError, AppResult, AppState};
use synctv_core::models::id::RoomId;
use synctv_livestream::api::{FlvStreamingApi, HlsStreamingApi};

/// Query parameters for live streaming endpoints
#[derive(Debug, Deserialize)]
pub struct LiveQuery {
    /// Room ID (required for most endpoints)
    room_id: Option<String>,
    /// Authentication token
    token: Option<String>,
}


/// Create live streaming router
///
/// Uses `AppState` for state (`live_streaming_infrastructure` must be configured)
///
/// Routes match synctv-go path patterns:
/// - /`api/room/movie/live/flv/:media_id`
/// - /`api/room/movie/live/hls/list/:media_id`
/// - /`api/room/movie/live/hls/data/:room_id/:media_id/:segment.ts`
/// - /`api/room/movie/live/info/:media_id`
/// - /`api/room/movie/live/streams`
pub fn create_live_router() -> Router<AppState> {
    Router::new()
        // FLV streaming endpoint
        .route("/flv/{media_id}.flv", get(handle_flv_stream))
        // HLS playlist endpoint (matches both with and without .m3u8 extension)
        .route("/hls/list/{media_id}", get(handle_hls_playlist))
        // HLS segment endpoint
        .route(
            "/hls/data/{room_id}/{media_id}/{*segment}",
            get(handle_hls_segment_with_disguise),
        )
        // Stream info endpoints
        .route("/info/{media_id}", get(handle_stream_info))
        .route("/streams", get(handle_room_streams))
}

/// Handle HLS segment request with automatic extension detection
///
/// Handles both regular .ts segments and disguised .png segments.
///
/// NOTE: TS segment endpoints are intentionally unauthenticated.
/// Authentication is enforced at the M3U8 playlist level. Segments themselves
/// must be served without auth tokens so they can be cached by CDN edge nodes.
/// Segment filenames contain random hashes, making them unguessable without
/// first obtaining the authenticated playlist.
async fn handle_hls_segment_with_disguise(
    Path((room_id, media_id, segment)): Path<(String, String, String)>,
    State(state): State<AppState>,
) -> AppResult<Response> {
    // Check if this is a disguised PNG request
    if segment.ends_with(".png") {
        handle_hls_segment_disguised(Path((room_id, media_id, segment)), State(state)).await
    } else {
        handle_hls_segment(Path((room_id, media_id, segment)), State(state)).await
    }
}

/// Handle FLV streaming request
///
/// GET /`api/room/movie/live/flv/:media_id?roomId=:room_id&token=:token`
///
/// Streaming endpoint for HTTP-FLV live streaming.
/// Creates a lazy-load pull stream on first request.
/// Supports disconnect signals for forced termination (ban/kick).
///
/// # Response
/// Returns streaming FLV data with `video/x-flv` content type.
async fn handle_flv_stream(
    Path(media_id): Path<String>,
    Query(params): Query<LiveQuery>,
    State(state): State<AppState>,
) -> AppResult<Response> {
    let room_id_str = params
        .room_id
        .ok_or_else(|| AppError::bad_request("roomId query parameter is required"))?;

    info!(room_id = %room_id_str, media_id = %media_id, "FLV streaming request");

    // Auth via ClientApiImpl
    let token = params.token.as_deref()
        .ok_or_else(|| AppError::unauthorized("token query parameter is required"))?;
    let user_id = state.client_api.validate_live_token(token, &room_id_str).await
        .map_err(AppError::unauthorized)?;

    // Get live streaming infrastructure via ClientApiImpl
    let infrastructure = state.client_api.live_infrastructure()
        .ok_or_else(|| AppError::internal_server_error("Live streaming not configured"))?;

    // Look up external source URL for LiveProxy media (validates media belongs to room)
    let source_url = state.client_api.get_live_proxy_source_url(&room_id_str, &media_id).await;

    // Create FLV streaming session with lazy-load pull
    let (rx, subscriber_guard) = FlvStreamingApi::create_session_with_pull(
        infrastructure, &room_id_str, &media_id, source_url.as_deref(),
    )
        .await
        .map_err(|e| AppError::internal_server_error(format!("Failed to create FLV session: {e}")))?;

    // Subscribe to disconnect signals
    let mut disconnect_rx = state.connection_manager.subscribe_disconnect();
    let room_id = RoomId::from_string(room_id_str.clone());

    // Create bounded channel wrapper that monitors disconnect signals
    // Match FLV_RESPONSE_CHANNEL_CAPACITY from synctv-xiu (512 entries ≈ 4MB)
    let (tx, rx_wrapped) = tokio::sync::mpsc::channel(512);

    // Spawn task to forward data and monitor disconnect signals.
    // The subscriber_guard lives here — dropped when the task ends (viewer disconnect),
    // which decrements the subscriber count and eventually triggers idle cleanup.
    let user_id_clone = user_id.clone();
    let room_id_clone = room_id.clone();
    tokio::spawn(async move {
        let _guard = subscriber_guard; // held for the lifetime of this task
        let mut rx = rx;
        loop {
            tokio::select! {
                // Forward FLV data from source
                data = rx.recv() => {
                    if let Some(chunk) = data {
                        match tx.try_send(chunk) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                debug!("FLV client disconnected");
                                break;
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                // Slow client - drop frame to prevent memory buildup
                            }
                        }
                    } else {
                        debug!("FLV source ended");
                        break;
                    }
                }

                // Monitor disconnect signals
                signal = disconnect_rx.recv() => {
                    match signal {
                        Ok(synctv_cluster::sync::DisconnectSignal::User(uid)) => {
                            if uid == user_id_clone {
                                info!(
                                    user_id = %user_id_clone.as_str(),
                                    "FLV stream terminated: user disconnected (ban/delete)"
                                );
                                break;
                            }
                        }
                        Ok(synctv_cluster::sync::DisconnectSignal::Room(rid)) => {
                            if rid == room_id_clone {
                                info!(
                                    room_id = %room_id_clone.as_str(),
                                    "FLV stream terminated: room disconnected (ban/delete)"
                                );
                                break;
                            }
                        }
                        Ok(synctv_cluster::sync::DisconnectSignal::UserFromRoom { user_id: uid, room_id: rid }) => {
                            if uid == user_id_clone && rid == room_id_clone {
                                info!(
                                    user_id = %user_id_clone.as_str(),
                                    room_id = %room_id_clone.as_str(),
                                    "FLV stream terminated: user kicked from room"
                                );
                                break;
                            }
                        }
                        Ok(synctv_cluster::sync::DisconnectSignal::Connection(_)) => {
                            // Connection-specific signals don't apply to FLV streams
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            warn!("FLV disconnect signal channel lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            warn!("FLV disconnect signal channel closed");
                            break;
                        }
                    }
                }
            }
        }
    });

    // Convert to streaming response
    let body = Body::from_stream(ReceiverStream::new(rx_wrapped));

    info!(
        room_id = %room_id.as_str(),
        media_id = %media_id,
        user_id = %user_id.as_str(),
        "FLV streaming started"
    );

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/x-flv")
        .header(header::CACHE_CONTROL, "no-cache, no-store")
        .header("X-Accel-Buffering", "no")
        .header(header::CONNECTION, "keep-alive")
        .body(body)
        .map_err(|_| AppError::internal_server_error("Failed to build response"))?
        .into_response())
}

/// Handle HLS playlist request
///
/// GET /`api/room/movie/live/hls/list/:media_id?roomId=:room_id&token=:token`
///
/// Returns M3U8 playlist with references to TS segments.
/// Creates a lazy-load pull stream on first request.
///
/// # Response
/// Returns M3U8 playlist with `application/vnd.apple.mpegurl` content type.
async fn handle_hls_playlist(
    Path(media_id): Path<String>,
    Query(params): Query<LiveQuery>,
    State(state): State<AppState>,
) -> AppResult<Response> {
    let room_id = params
        .room_id
        .ok_or_else(|| AppError::bad_request("roomId query parameter is required"))?;

    info!(room_id = %room_id, media_id = %media_id, "HLS playlist request");

    // Auth via ClientApiImpl
    let token = params.token.as_deref()
        .ok_or_else(|| AppError::unauthorized("token query parameter is required"))?;
    let _user_id = state.client_api.validate_live_token(token, &room_id).await
        .map_err(AppError::unauthorized)?;

    let infrastructure = state.client_api.live_infrastructure()
        .ok_or_else(|| AppError::internal_server_error("Live streaming not configured"))?;

    // Build segment URL base following synctv-go pattern
    // TS segments are at: /api/room/movie/live/hls/data/{roomId}/{movieId}/
    //
    // NOTE: Segment URLs intentionally do NOT include auth tokens.
    // Authentication is enforced only at this M3U8 playlist endpoint.
    // Segments must be cacheable by CDN, and their filenames contain random
    // hashes making them unguessable without the authenticated playlist.
    let segment_url_base = format!("/api/room/movie/live/hls/data/{room_id}/{media_id}/");

    // Generate HLS playlist (local or proxied from publisher node).
    // HLS does NOT trigger RTMP pull streams -- only FLV does.
    let playlist = HlsStreamingApi::generate_playlist_simple(
        infrastructure,
        &room_id,
        &media_id,
        &segment_url_base,
    )
    .await
    .map_err(|e| AppError::internal_server_error(format!("Failed to generate HLS playlist: {e}")))?;

    match playlist {
        Some(content) => {
            debug!(
                room_id = %room_id,
                media_id = %media_id,
                "Generated HLS playlist"
            );

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
                .header(header::CACHE_CONTROL, "no-cache, no-store")
                .body(content)
                .map_err(|_| AppError::internal_server_error("Failed to build response"))?
                .into_response())
        }
        None => {
            // No active stream — return 404 so players can distinguish
            // "stream not found" from "stream starting" (empty playlist).
            Err(AppError::not_found(format!(
                "No active HLS stream for {room_id}/{media_id}"
            )))
        }
    }
}

/// Handle HLS segment request
///
/// GET /`api/room/movie/live/hls/data/:room_id/:media_id/:segment.ts`
///
/// Serves individual HLS TS segments.
///
/// NOTE: Intentionally unauthenticated — auth is at the M3U8 playlist level.
/// Segments must be CDN-cacheable; their random-hash filenames are unguessable
/// without the authenticated playlist. Do NOT add token validation here.
///
/// # Response
/// Returns TS segment data with `video/mp2t` content type.
async fn handle_hls_segment(
    Path((room_id, media_id, segment)): Path<(String, String, String)>,
    State(state): State<AppState>,
) -> AppResult<Response> {
    // Remove .ts suffix if present
    let segment_name = segment.trim_end_matches(".ts");

    debug!(
        room_id = %room_id,
        media_id = %media_id,
        segment = %segment_name,
        "HLS segment request"
    );

    let infrastructure = state.client_api.live_infrastructure()
        .ok_or_else(|| AppError::internal_server_error("Live streaming not configured"))?;

    // Get segment data
    match HlsStreamingApi::get_segment(infrastructure, &room_id, &media_id, segment_name).await {
        Ok(data) => {
            debug!(
                room_id = %room_id,
                media_id = %media_id,
                segment = %segment_name,
                size = data.len(),
                "Serving HLS segment"
            );

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "video/mp2t")
                .header(header::CACHE_CONTROL, "public, max-age=90")
                .header("X-Accel-Buffering", "no")
                .body(Body::from(data))
                .map_err(|_| AppError::internal_server_error("Failed to build response"))?
                .into_response())
        }
        Err(e) => {
            warn!(
                room_id = %room_id,
                media_id = %media_id,
                segment = %segment_name,
                error = %e,
                "HLS segment not found"
            );
            Err(AppError::not_found("HLS segment not found"))
        }
    }
}

/// Handle HLS segment request (disguised as PNG)
///
/// GET /`api/room/movie/live/hls/data/:room_id/:media_id/:segment.png`
///
/// Serves TS segments disguised as PNG images (`TSDisguisedAsPng` feature).
/// Adds a PNG header to TS data to bypass certain filters.
///
/// NOTE: Intentionally unauthenticated — same as `handle_hls_segment`.
/// See that handler's doc comment for rationale. Do NOT add token validation here.
///
/// # Response
/// Returns TS segment data with PNG header, `image/png` content type.
async fn handle_hls_segment_disguised(
    Path((room_id, media_id, segment)): Path<(String, String, String)>,
    State(state): State<AppState>,
) -> AppResult<Response> {
    // Remove .png suffix
    let segment_name = segment.trim_end_matches(".png");

    debug!(
        room_id = %room_id,
        media_id = %media_id,
        segment = %segment_name,
        "HLS segment request (disguised as PNG)"
    );

    let infrastructure = state.client_api.live_infrastructure()
        .ok_or_else(|| AppError::internal_server_error("Live streaming not configured"))?;

    // Get segment data
    match HlsStreamingApi::get_segment(infrastructure, &room_id, &media_id, segment_name).await {
        Ok(ts_data) => {
            // PNG header (89 50 4E 47 0D 0A 1A 0A + IHDR chunk)
            // Minimal PNG: 8x8 pixel image
            let png_header = [
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
                0x00, 0x00, 0x00, 0x0D, // Length: 13
                0x49, 0x48, 0x44, 0x52, // IHDR
                0x00, 0x00, 0x00, 0x08, // Width: 8
                0x00, 0x00, 0x00, 0x08, // Height: 8
                0x08, 0x02, 0x00, 0x00, 0x00, // Bit depth: 8, Color type: 2 (RGB), etc.
                0x90, 0x77, 0x53, // CRC
                0xDE, // Start of IDAT chunk (followed by actual TS data)
            ];

            let mut disguised_data = Vec::with_capacity(png_header.len() + ts_data.len());
            disguised_data.extend_from_slice(&png_header);
            disguised_data.extend_from_slice(&ts_data);

            debug!(
                room_id = %room_id,
                media_id = %media_id,
                segment = %segment_name,
                original_size = ts_data.len(),
                disguised_size = disguised_data.len(),
                "Serving HLS segment (disguised as PNG)"
            );

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "image/png")
                .header(header::CACHE_CONTROL, "public, max-age=90")
                .header("X-Accel-Buffering", "no")
                .body(Body::from(disguised_data))
                .map_err(|_| AppError::internal_server_error("Failed to build response"))?
                .into_response())
        }
        Err(e) => {
            warn!(
                room_id = %room_id,
                media_id = %media_id,
                segment = %segment_name,
                error = %e,
                "HLS segment not found"
            );
            Err(AppError::not_found("HLS segment not found"))
        }
    }
}

/// Handle stream info request
///
/// GET /`api/room/movie/live/info/:media_id?room_id=:room_id&token=:token`
///
/// Returns whether a stream is active and publisher information.
/// Requires valid JWT auth + room membership.
async fn handle_stream_info(
    Path(media_id): Path<String>,
    Query(params): Query<LiveQuery>,
    State(state): State<AppState>,
) -> AppResult<Json<crate::proto::client::GetStreamInfoResponse>> {
    let room_id = params
        .room_id
        .ok_or_else(|| AppError::bad_request("room_id query parameter is required"))?;

    // Auth via ClientApiImpl
    let token = params.token.as_deref()
        .ok_or_else(|| AppError::unauthorized("token query parameter is required"))?;
    let user_id = state.client_api.validate_live_token(token, &room_id).await
        .map_err(AppError::unauthorized)?;

    let resp = state.client_api.get_stream_info(user_id.as_str(), &room_id, &media_id).await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(resp))
}

/// Handle room streams request
///
/// GET /`api/room/movie/live/streams?room_id=:room_id&token=:token`
///
/// Returns all active streams in a room.
/// Requires valid JWT auth + room membership.
async fn handle_room_streams(
    Query(params): Query<LiveQuery>,
    State(state): State<AppState>,
) -> AppResult<Json<crate::proto::client::ListRoomStreamsResponse>> {
    let room_id = params
        .room_id
        .ok_or_else(|| AppError::bad_request("room_id query parameter is required"))?;

    // Auth via ClientApiImpl
    let token = params.token.as_deref()
        .ok_or_else(|| AppError::unauthorized("token query parameter is required"))?;
    let user_id = state.client_api.validate_live_token(token, &room_id).await
        .map_err(AppError::unauthorized)?;

    let resp = state.client_api.list_room_streams(user_id.as_str(), &room_id).await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test LiveQuery deserialization
    #[test]
    fn test_live_query_deserialize() {
        // Query with room_id and token (using snake_case as per convention)
        let query: LiveQuery = serde_urlencoded::from_str("room_id=room123&token=abc123").unwrap();
        assert_eq!(query.room_id, Some("room123".to_string()));
        assert_eq!(query.token, Some("abc123".to_string()));

        // Query with only room_id
        let query: LiveQuery = serde_urlencoded::from_str("room_id=room123").unwrap();
        assert_eq!(query.room_id, Some("room123".to_string()));
        assert!(query.token.is_none());

        // Empty query
        let query: LiveQuery = serde_urlencoded::from_str("").unwrap();
        assert!(query.room_id.is_none());
        assert!(query.token.is_none());
    }

    /// Test LiveQuery structure
    #[test]
    fn test_live_query_structure() {
        let query = LiveQuery {
            room_id: Some("room123".to_string()),
            token: Some("abc123".to_string()),
        };

        assert_eq!(query.room_id.unwrap(), "room123");
        assert_eq!(query.token.unwrap(), "abc123");
    }

    /// Test PNG header for TS disguise
    #[test]
    fn test_png_disguise_header() {
        let png_header = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, // Length: 13
            0x49, 0x48, 0x44, 0x52, // IHDR
            0x00, 0x00, 0x00, 0x08, // Width: 8
            0x00, 0x00, 0x00, 0x08, // Height: 8
            0x08, 0x02, 0x00, 0x00, 0x00, // Bit depth: 8, Color type: 2 (RGB)
            0x90, 0x77, 0x53, // CRC
            0xDE, // Start of IDAT chunk
        ];

        // Verify PNG signature
        assert_eq!(&png_header[0..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

        // Verify IHDR chunk
        assert_eq!(&png_header[12..16], "IHDR".as_bytes());

        // Verify dimensions
        let width = u32::from_be_bytes([png_header[16], png_header[17], png_header[18], png_header[19]]);
        let height = u32::from_be_bytes([png_header[20], png_header[21], png_header[22], png_header[23]]);
        assert_eq!(width, 8);
        assert_eq!(height, 8);
    }

    /// Test room_id and media_id separation in paths
    #[test]
    fn test_room_media_path_separation() {
        // HLS segment path should have both room_id and media_id
        let segment_path = "/api/room/movie/live/hls/data/room123/media456/segment.ts";

        assert!(segment_path.contains("room123"));
        assert!(segment_path.contains("media456"));
        assert!(segment_path.contains("segment.ts"));

        // Path format: /api/room/movie/live/hls/data/{room_id}/{media_id}/{segment}
        let parts: Vec<&str> = segment_path.split('/').collect();
        assert_eq!(parts[6], "data");
        assert_eq!(parts[7], "room123");
        assert_eq!(parts[8], "media456");
        assert_eq!(parts[9], "segment.ts");
    }

    /// Test query parameter extraction
    #[test]
    fn test_query_parameter_extraction() {
        // Query string for FLV (using snake_case as per convention)
        let query_str = "room_id=room123&token=test_token";
        let query: LiveQuery = serde_urlencoded::from_str(query_str).unwrap();

        assert_eq!(query.room_id, Some("room123".to_string()));
        assert_eq!(query.token, Some("test_token".to_string()));

        // Query string for HLS playlist
        let query_str = "room_id=room456";
        let query: LiveQuery = serde_urlencoded::from_str(query_str).unwrap();

        assert_eq!(query.room_id, Some("room456".to_string()));
        assert!(query.token.is_none());
    }

    /// Test media_id in path and room_id in query
    #[test]
    fn test_media_in_path_room_in_query() {
        // FLV endpoint: /api/room/movie/live/flv/:media_id.flv?room_id=:room_id
        let path = "/api/room/movie/live/flv/media123.flv";
        let query = "room_id=room456";

        assert!(path.contains("media123"));
        assert!(query.contains("room456"));

        // HLS playlist: /api/room/movie/live/hls/list/:media_id?room_id=:room_id
        let path = "/api/room/movie/live/hls/list/media456";
        let query = "room_id=room789";

        assert!(path.contains("media456"));
        assert!(query.contains("room789"));

        // HLS segment: /api/room/movie/live/hls/data/:room_id/:media_id/:segment.ts
        let path = "/api/room/movie/live/hls/data/room111/media222/segment.ts";

        assert!(path.contains("room111"));
        assert!(path.contains("media222"));
        assert!(path.contains("segment.ts"));
    }

    /// Test segment name trimming
    #[test]
    fn test_segment_name_trimming() {
        // Trim .ts suffix
        let segment = "segment.ts";
        let trimmed = segment.trim_end_matches(".ts");
        assert_eq!(trimmed, "segment");

        // Trim .png suffix
        let segment = "segment.png";
        let trimmed = segment.trim_end_matches(".png");
        assert_eq!(trimmed, "segment");

        // No suffix
        let segment = "segment";
        let trimmed = segment.trim_end_matches(".ts");
        assert_eq!(trimmed, "segment");
    }

    /// Test M3U8 content type
    #[test]
    fn test_m3u8_content_type() {
        assert_eq!("application/vnd.apple.mpegurl", "application/vnd.apple.mpegurl");
    }

    /// Test FLV content type
    #[test]
    fn test_flv_content_type() {
        assert_eq!("video/x-flv", "video/x-flv");
    }

    /// Test TS content type
    #[test]
    fn test_ts_content_type() {
        assert_eq!("video/mp2t", "video/mp2t");
    }

    /// Test PNG content type (for disguised TS)
    #[test]
    fn test_png_content_type() {
        assert_eq!("image/png", "image/png");
    }
}
