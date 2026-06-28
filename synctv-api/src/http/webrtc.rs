//! WebRTC HTTP REST API endpoints
//!
//! Provides HTTP/JSON API for WebRTC configuration and control:
//! - `/api/rooms/{roomId}/webrtc/ice-servers` - Get ICE servers (built-in STUN + dynamic ICE)

use axum::{
    extract::{Path, State},
    response::Json,
};

use crate::http::middleware::RequestMetadata;
use crate::http::{error::map_api_error, AppResult, AppState};
use crate::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};

// Proto types already derive serde::Serialize/Deserialize.
use synctv_proto::client::GetIceServersResponse;

/// Get ICE servers configuration for WebRTC
///
/// Returns a list of ICE servers configured for this deployment.
///
/// Path: `GET /api/rooms/{roomId}/webrtc/ice-servers`
/// Auth: Required (JWT or room-bound guest token)
/// Permissions: `USE_WEBRTC`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/webrtc/ice-servers",
        tag = "WebRTC",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "ICE servers for the authenticated room actor", body = GetIceServersResponse),
            (status = 400, description = "Invalid room ID", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "WebRTC permission required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Room not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_ice_servers(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
) -> AppResult<Json<GetIceServersResponse>> {
    let public_room_id = path.room_id;
    let room_id = crate::impls::proto_validated_room_id(
        public_room_id.clone(),
        &state.shared_api_runtime.public_id_codec,
    )
    .map_err(map_api_error)?;

    let response = super::room::execute_room_actor_endpoint(
        &state,
        request_meta,
        public_room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::WebRtc,
        move |client_api, actor: crate::impls::client::RoomActor| async move {
            debug_assert_eq!(actor.room_id(), room_id);
            client_api.get_ice_servers_for_actor(&actor).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use crate::http::error::map_api_error;
    use crate::impls::ApiError;
    use axum::http::StatusCode;
    use synctv_proto::client::{GetIceServersResponse, IceServer};

    type TestResult<T = ()> = anyhow::Result<T>;

    #[test]
    fn test_stun_ice_server_serialization_never_exposes_auth_fields() -> TestResult {
        let server = IceServer {
            urls: vec![
                "stun:stun-auth.example.com:3478".to_string(),
                "stun:stun-auth-backup.example.com:3478".to_string(),
            ],
            username: None,
            credential: None,
        };

        let json = serde_json::to_value(&server)?;
        assert_eq!(
            json["urls"],
            serde_json::to_value([
                "stun:stun-auth.example.com:3478",
                "stun:stun-auth-backup.example.com:3478"
            ])
            .expect("urls should serialize")
        );
        assert_eq!(json["username"], serde_json::Value::Null);
        assert_eq!(json["credential"], serde_json::Value::Null);
        Ok(())
    }

    #[test]
    fn test_ice_servers_response_includes_webrtc_status() -> TestResult {
        let response = GetIceServersResponse {
            servers: Vec::new(),
            webrtc: Some(crate::webrtc_status::to_proto_status(
                &synctv_core::service::WebRtcRuntimeStatus::degraded(
                    synctv_core::service::BuiltinStunRuntimeReason::ExternalAddrInvalid,
                    "invalid external STUN address",
                    Some("127.0.0.1:3478".to_string()),
                ),
            )),
        };

        let json = serde_json::to_value(&response)?;
        assert_eq!(json["webrtc"]["builtinStunState"], "degraded");
        assert_eq!(json["webrtc"]["reason"], "external_addr_invalid");
        assert_eq!(json["webrtc"]["externalAddr"], "127.0.0.1:3478");
        Ok(())
    }

    #[test]
    fn test_map_api_error_authorization_returns_forbidden() {
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
}
