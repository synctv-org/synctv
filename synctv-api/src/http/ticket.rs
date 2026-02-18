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

use super::middleware::AuthUser;
use super::{AppError, AppState};

/// Request to create a WebSocket ticket
#[derive(Debug, Deserialize)]
pub struct CreateTicketRequest {
    /// Unused field, kept for API backward compatibility.
    #[serde(default, skip)]
    pub _room_id: Option<String>,
}

/// Response containing the WebSocket ticket
#[derive(Debug, Serialize)]
pub struct TicketResponse {
    /// The ticket string to use in WebSocket URL
    pub ticket: String,
    /// Ticket expiration time in seconds
    pub expires_in_secs: u64,
    /// Usage instructions
    pub usage: String,
}

/// Create a WebSocket ticket for secure authentication
///
/// This endpoint creates a short-lived, one-time-use ticket that can be used
/// to authenticate WebSocket connections without exposing the JWT token in the URL.
///
/// # Example
///
/// ```http
/// POST /api/tickets
/// Authorization: Bearer <jwt>
/// Content-Type: application/json
///
/// {}
/// ```
///
/// Response:
/// ```json
/// {
///   "ticket": "abc123...",
///   "expires_in_secs": 30,
///   "usage": "Use in WebSocket URL: ws://host/ws/room/{room_id}?ticket=xxx"
/// }
/// ```
pub async fn create_ticket(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(_req): Json<CreateTicketRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Check if ticket service is available
    let ws_ticket_service = state.ws_ticket_service.as_ref().ok_or_else(|| {
        AppError::internal_server_error(
            "WebSocket ticket service not configured (Redis required)",
        )
    })?;

    // Create a new ticket for this user
    let ticket = ws_ticket_service
        .create_ticket(&auth.user_id)
        .await
        .map_err(|e| {
            AppError::internal_server_error(format!("Failed to create WebSocket ticket: {e}"))
        })?;

    let response = TicketResponse {
        ticket,
        expires_in_secs: ws_ticket_service.ticket_ttl_secs(),
        usage: "Use in WebSocket URL: ws://host/ws/room/{room_id}?ticket=xxx".to_string(),
    };

    Ok((StatusCode::OK, Json(response)))
}
