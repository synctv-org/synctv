//! WebSocket Ticket API
//!
//! Provides short-lived, one-time-use tickets for secure WebSocket authentication.
//!
//! # Security Benefits
//!
//! Instead of passing JWT tokens directly in WebSocket URLs (which appear in
//! browser history and server logs), clients can:
//! 1. Call POST /api/tickets to get a short-lived ticket
//! 2. Use the ticket in WebSocket URL: <ws://host/ws/room/{room_id}?ticket=xxx>
//!
//! The ticket is:
//! - Short-lived (default 30 seconds)
//! - Single-use (consumed on first use)
//! - Does not expose the actual JWT token

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use synctv_core::models::RoomId;

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
///   "usage": "Use in WebSocket URL: ws://host/ws/room/abc123?ticket=xxx"
/// }
/// ```
pub async fn create_ticket(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateTicketRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Validate room_id is non-empty
    if req.room_id.trim().is_empty() {
        return Err(AppError::bad_request("room_id is required"));
    }

    let room_id = RoomId::from_string(req.room_id.clone());

    // Check if ticket service is available
    let ws_ticket_service = state.ws_ticket_service.as_ref().ok_or_else(|| {
        AppError::internal_server_error(
            "WebSocket ticket service not configured (Redis required)",
        )
    })?;

    // Create a new room-bound ticket for this user (Issue #65)
    let ticket = ws_ticket_service
        .create_ticket(&auth.user_id, &room_id)
        .await
        .map_err(|e| {
            AppError::internal_server_error(format!("Failed to create WebSocket ticket: {e}"))
        })?;

    let response = TicketResponse {
        ticket,
        room_id: req.room_id.clone(),
        expires_in_secs: ws_ticket_service.ticket_ttl_secs(),
        usage: format!("Use in WebSocket URL: ws://host/ws/room/{}/ticket=xxx", req.room_id),
    };

    Ok((StatusCode::OK, Json(response)))
}
