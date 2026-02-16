//! WebRTC HTTP REST API endpoints
//!
//! Provides HTTP/JSON API for WebRTC configuration and control:
//! - `/api/rooms/{room_id}/webrtc/ice-servers` - Get ICE servers (built-in STUN + dynamic STUN/TURN)
//! - `/api/rooms/{room_id}/webrtc/network-quality` - Get network quality stats

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Json},
};

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
