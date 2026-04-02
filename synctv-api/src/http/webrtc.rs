//! WebRTC HTTP REST API endpoints
//!
//! Provides HTTP/JSON API for WebRTC configuration and control:
//! - `/api/rooms/{room_id}/webrtc/ice-servers` - Get ICE servers (built-in STUN + dynamic STUN/TURN)

use axum::{
    extract::{Path, State},
    response::Json,
};

use crate::http::middleware::AuthUser;
use crate::http::{error::map_api_error, AppResult, AppState};

// M-10: Use proto types directly instead of duplicating response structs.
// Proto types already derive serde::Serialize/Deserialize.
use crate::proto::client::GetIceServersResponse;

/// Get ICE servers configuration for WebRTC
///
/// Returns a list of STUN/TURN servers configured for this deployment.
/// For TURN servers, temporary credentials are generated for the authenticated user.
///
/// Path: `GET /api/rooms/{room_id}/webrtc/ice-servers`
/// Auth: Required (JWT)
/// Permissions: Room membership required
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/webrtc/ice-servers",
        tag = "WebRTC",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "ICE servers for the authenticated room member", body = GetIceServersResponse),
            (status = 400, description = "Invalid room ID", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Room membership required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_ice_servers(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<GetIceServersResponse>> {
    let user_id = auth.user_id;
    let room_id = crate::room_id_validation::parse_room_id(&room_id)
        .map_err(|e| crate::http::AppError::bad_request(format!("Invalid room_id: {e}")))?;

    // Membership check is performed inside client_api.get_ice_servers()
    // Errors are mapped via map_api_error for proper HTTP status codes:
    // - Authorization errors -> 403 Forbidden
    // - Other errors -> appropriate status codes based on error kind
    let response: GetIceServersResponse = state
        .client_api
        .get_ice_servers(&room_id, &user_id)
        .await
        .map_err(map_api_error)?;

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use crate::http::error::map_api_error;
    use crate::impls::ApiError;
    use crate::proto::client::IceServer;
    use axum::http::StatusCode;

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

    #[test]
    fn test_map_api_error_authorization_returns_forbidden() {
        // Verify that Authorization errors map to 403 Forbidden
        let api_err = ApiError::Authorization("User is not a member of this room".to_string());
        let app_err = map_api_error(api_err);
        assert_eq!(app_err.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_map_api_error_authentication_returns_unauthorized() {
        // Verify that Authentication errors map to 401 Unauthorized
        let api_err = ApiError::Authentication("Token expired".to_string());
        let app_err = map_api_error(api_err);
        assert_eq!(app_err.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_map_api_error_not_found_returns_404() {
        let api_err = ApiError::NotFound("Room not found".to_string());
        let app_err = map_api_error(api_err);
        assert_eq!(app_err.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_map_api_error_internal_returns_500() {
        let api_err = ApiError::Internal("Database connection failed".to_string());
        let app_err = map_api_error(api_err);
        assert_eq!(app_err.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_map_api_error_includes_error_code() {
        let api_err = ApiError::Authorization("Access denied".to_string());
        let app_err = map_api_error(api_err);
        // map_api_error should populate the error_code field
        assert!(app_err.error_code.is_some());
    }

    #[test]
    fn test_webrtc_room_id_uses_shared_validation_rules() {
        let err = crate::room_id_validation::parse_room_id("room@invalid").unwrap_err();
        assert!(matches!(
            err,
            crate::http::validation::ValidationError::InvalidFormat { .. }
        ));
    }
}
