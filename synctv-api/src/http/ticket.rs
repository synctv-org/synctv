//! WebSocket Ticket API
//!
//! Provides short-lived, one-time-use tickets for secure WebSocket authentication.
//!
//! # Security Benefits
//!
//! Instead of passing JWT tokens directly in WebSocket URLs (which appear in
//! browser history and server logs), clients can:
//! 1. Call POST /api/tickets to get a short-lived ticket
//! 2. Use the ticket in WebSocket URL: <ws://host/ws/rooms/{room_id}?ticket=xxx>
//!
//! The ticket is:
//! - Short-lived (default 30 seconds)
//! - Single-use (consumed on first use)
//! - Does not expose the actual JWT token

use axum::{extract::State, Json};

use super::middleware::AuthUser;
use super::{AppError, AppResult, AppState};
use crate::impls::client::build_create_websocket_ticket_request;
pub use crate::proto::client::{CreateWebSocketTicketRequest, CreateWebSocketTicketResponse};

fn map_room_lookup_error(room_id: &str, err: synctv_core::Error) -> AppError {
    match err {
        synctv_core::Error::NotFound(_) => AppError::not_found(format!("Room {room_id} not found")),
        other => AppError::from(other),
    }
}

fn map_ticket_membership_probe_error(err: synctv_core::Error) -> AppError {
    AppError::from(err)
}

fn map_ticket_creation_error(err: synctv_core::Error) -> AppError {
    AppError::from(err)
}

/// Create a room-bound WebSocket ticket for secure authentication
///
/// This endpoint creates a short-lived, one-time-use ticket that can be used
/// to authenticate WebSocket connections without exposing the JWT token in the URL.
/// The ticket is bound to the specified room and cannot be used for other rooms.
///
/// # Example
///
/// ```http
/// POST /api/tickets
/// Authorization: Bearer <jwt>
/// Content-Type: application/json
///
/// { "room_id": "abc123" }
/// ```
///
/// Response:
/// ```json
/// {
///   "ticket": "abc123...",
///   "room_id": "abc123",
///   "expires_in_secs": 30,
///   "usage": "Use in WebSocket URL: ws://host/ws/rooms/abc123?ticket=xxx"
/// }
/// ```
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/tickets",
        tag = "WebSocket",
        request_body = CreateWebSocketTicketRequest,
        responses(
            (status = 200, description = "WebSocket ticket created", body = CreateWebSocketTicketResponse),
            (status = 400, description = "Invalid room_id", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Caller cannot create a ticket for this room", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Ticket backend unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn create_ticket(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateWebSocketTicketRequest>,
) -> AppResult<Json<CreateWebSocketTicketResponse>> {
    super::websocket::validate_websocket_runtime_dependencies(&state)?;
    let room_id =
        build_create_websocket_ticket_request(&req).map_err(super::error::map_api_error)?;

    // Verify room exists and is accessible
    let room = state
        .room_service
        .get_room(&room_id)
        .await
        .map_err(|err| map_room_lookup_error(&req.room_id, err))?;

    if room.is_banned {
        return Err(AppError::forbidden("Room is banned"));
    }

    // Check room membership: user must be a member of the room
    let is_member = state
        .room_service
        .member_service()
        .is_member(&room_id, &auth.user_id)
        .await
        .map_err(map_ticket_membership_probe_error)?;

    if !is_member {
        return Err(AppError::forbidden(
            "Not a member of this room. Join the room first.",
        ));
    }

    // Check if ticket service is available
    // Create a new room-bound ticket for this user (Issue #65)
    // Include password_version so tickets are invalidated on password change
    let ticket = state
        .ws_ticket_service
        .create_ticket(&auth.user_id, &room_id, auth.password_version)
        .await
        .map_err(map_ticket_creation_error)?;

    let response = CreateWebSocketTicketResponse {
        ticket,
        room_id: room_id.as_str().to_string(),
        expires_in_secs: state.ws_ticket_service.ticket_ttl_secs(),
        usage: format!(
            "Use in WebSocket URL: ws://host/ws/rooms/{}?ticket=xxx",
            room_id.as_str()
        ),
    };

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::{
        map_room_lookup_error, map_ticket_creation_error, map_ticket_membership_probe_error,
    };
    use axum::http::StatusCode;

    #[test]
    fn room_lookup_not_found_maps_to_404() {
        let err = map_room_lookup_error(
            "room_123",
            synctv_core::Error::NotFound("missing".to_string()),
        );
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert!(err.message.contains("room_123"));
    }

    #[test]
    fn room_lookup_database_error_does_not_map_to_404() {
        let err = map_room_lookup_error(
            "room_123",
            synctv_core::Error::Database(sqlx::Error::PoolTimedOut),
        );
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn membership_probe_backend_outage_maps_to_503() {
        let err = map_ticket_membership_probe_error(synctv_core::Error::ServiceUnavailable(
            "membership backend temporarily unavailable".to_string(),
        ));
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn ticket_creation_backend_outage_maps_to_503() {
        let err = map_ticket_creation_error(synctv_core::Error::ServiceUnavailable(
            "Failed to store ticket: connection reset by peer".to_string(),
        ));
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
