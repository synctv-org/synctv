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

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

use super::middleware::AuthUser;
use super::{AppError, AppState};

/// Request to create a WebSocket ticket (Issue #65: now room-scoped)
#[derive(Debug, Deserialize)]
pub struct CreateTicketRequest {
    /// The room ID the ticket is bound to.
    ///
    /// The returned ticket can only be used to authenticate a WebSocket
    /// connection to this exact room. Attempting to use it for a different
    /// room will result in an authentication error.
    pub room_id: String,
}

/// Response containing the WebSocket ticket
#[derive(Debug, Serialize)]
pub struct TicketResponse {
    /// The ticket string to use in WebSocket URL
    pub ticket: String,
    /// Room ID the ticket is bound to
    pub room_id: String,
    /// Ticket expiration time in seconds
    pub expires_in_secs: u64,
    /// Usage instructions
    pub usage: String,
}

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
pub async fn create_ticket(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateTicketRequest>,
) -> Result<impl IntoResponse, AppError> {
    super::websocket::validate_websocket_runtime_dependencies(&state)?;

    // Validate room_id is non-empty
    if req.room_id.trim().is_empty() {
        return Err(AppError::bad_request("room_id is required"));
    }

    let room_id = crate::room_id_validation::parse_room_id(&req.room_id)
        .map_err(|e| AppError::bad_request(format!("Invalid room_id: {e}")))?;

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

    let response = TicketResponse {
        ticket,
        room_id: room_id.as_str().to_string(),
        expires_in_secs: state.ws_ticket_service.ticket_ttl_secs(),
        usage: format!(
            "Use in WebSocket URL: ws://host/ws/rooms/{}?ticket=xxx",
            room_id.as_str()
        ),
    };

    Ok((StatusCode::OK, Json(response)))
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
