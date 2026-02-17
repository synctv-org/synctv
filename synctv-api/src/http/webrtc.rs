//! WebRTC HTTP REST API endpoints
//!
//! Provides HTTP/JSON API for WebRTC configuration and control:
//! - `/api/rooms/{room_id}/webrtc/ice-servers` - Get ICE servers (built-in STUN + dynamic STUN/TURN)
//! - `/api/rooms/{room_id}/webrtc/network-quality` - Get network quality stats
//! - `/api/webrtc/session/{conn_id}/affinity` - Session affinity lookup (multi-replica routing)

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;

use crate::http::{AppError, AppResult, AppState};
use crate::http::middleware::AuthUser;
use synctv_core::models::RoomId;

// M-10: Use proto types directly instead of duplicating response structs.
// Proto types already derive serde::Serialize/Deserialize.
use crate::proto::client::{GetIceServersResponse, GetNetworkQualityResponse};

/// Get ICE servers configuration for WebRTC
///
/// Returns a list of STUN/TURN servers configured for this deployment.
/// For TURN servers, temporary credentials are generated for the authenticated user.
///
/// Path: `GET /api/rooms/{room_id}/webrtc/ice-servers`
/// Auth: Required (JWT)
/// Permissions: Room membership required
pub async fn get_ice_servers(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let user_id = auth.user_id;
    let room_id = RoomId::from_string(room_id);

    // Membership check is performed inside client_api.get_ice_servers()
    let response: GetIceServersResponse = state
        .client_api
        .get_ice_servers(&room_id, &user_id)
        .await
        .map_err(|e| AppError::internal_server_error(e.to_string()))?;

    Ok(Json(response))
}

/// Get network quality stats for WebRTC peers in a room
///
/// Path: `GET /api/rooms/{room_id}/webrtc/network-quality`
/// Auth: Required (JWT)
/// Permissions: Room membership required
pub async fn get_network_quality(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let user_id = auth.user_id;
    let room_id = RoomId::from_string(room_id);

    // Membership check is performed inside client_api.get_network_quality()
    let response: GetNetworkQualityResponse = state
        .client_api
        .get_network_quality(&room_id, &user_id)
        .await
        .map_err(|e| AppError::internal_server_error(e.to_string()))?;

    Ok(Json(response))
}

/// Query parameters for session affinity endpoint
#[derive(Debug, Deserialize)]
pub struct AffinityQuery {
    /// Optional base URL for the redirect target. When not provided, the
    /// response returns the replica ID in a JSON body instead of redirecting.
    /// Example: `?redirect_base=https://sfu-{replica}.example.com`
    pub redirect_base: Option<String>,
}

/// Look up which SFU replica owns a WebRTC session and optionally redirect.
///
/// In multi-replica deployments, WebRTC PeerConnections are local to the
/// replica that created them. This endpoint allows load balancers and API
/// gateways to query which replica owns a given session and route
/// subsequent signaling requests accordingly.
///
/// Path: `GET /api/webrtc/session/{conn_id}/affinity`
/// Auth: Required (JWT) -- prevents unauthenticated probing of session IDs.
///
/// ## Response behavior
///
/// - **Without `redirect_base`**: Returns `200 OK` with JSON body
///   `{"replica_id": "sfu-abc", "is_local": true}`.
/// - **With `redirect_base`**: If the session is on a DIFFERENT replica,
///   returns `307 Temporary Redirect` to `{redirect_base}/api/webrtc/...`.
///   If local, returns `200 OK` with JSON body.
/// - **Session not found**: Returns `404 Not Found`.
/// - **SFU not configured**: Returns `501 Not Implemented`.
pub async fn session_affinity_lookup(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(conn_id): Path<String>,
    Query(params): Query<AffinityQuery>,
) -> AppResult<Response> {
    let Some(ref sfu_mgr) = state.sfu_session_manager else {
        return Ok((
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
                "error": "SFU session manager is not configured"
            })),
        ).into_response());
    };

    let replica = sfu_mgr
        .lookup_session_replica(&conn_id)
        .await
        .map_err(|e| AppError::internal_server_error(e.to_string()))?;

    let Some(replica_id) = replica else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Session not found",
                "conn_id": conn_id
            })),
        ).into_response());
    };

    let is_local = replica_id == sfu_mgr.replica_id();

    // If the caller provided a redirect_base and the session is remote,
    // return a 307 redirect so the load balancer follows it automatically.
    if let Some(ref base) = params.redirect_base {
        if !is_local {
            let redirect_url = format!(
                "{}/api/webrtc/session/{}/affinity",
                base.trim_end_matches('/'),
                conn_id
            );
            return Ok((
                StatusCode::TEMPORARY_REDIRECT,
                [(header::LOCATION, redirect_url)],
                Json(serde_json::json!({
                    "replica_id": replica_id,
                    "is_local": false
                })),
            ).into_response());
        }
    }

    Ok(Json(serde_json::json!({
        "replica_id": replica_id,
        "is_local": is_local
    })).into_response())
}

#[cfg(test)]
mod tests {
    use crate::proto::client::IceServer;

    #[test]
    fn test_ice_server_serialization() {
        let server = IceServer {
            urls: vec!["stun:stun.example.com:3478".to_string()],
            username: None,
            credential: None,
            expiry_time: 0,
        };

        let json = serde_json::to_string(&server).expect("IceServer should serialize");
        assert!(json.contains("stun:stun.example.com:3478"));
    }

    #[test]
    fn test_turn_server_serialization() {
        let server = IceServer {
            urls: vec!["turn:turn.example.com:3478".to_string()],
            username: Some("1234567890:user123".to_string()),
            credential: Some("secret123".to_string()),
            expiry_time: 0,
        };

        let json = serde_json::to_string(&server).expect("IceServer should serialize");
        assert!(json.contains("turn:turn.example.com:3478"));
        assert!(json.contains("username"));
        assert!(json.contains("credential"));
    }
}
