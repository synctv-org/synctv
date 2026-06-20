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

use super::middleware::RequestMetadata;
use super::{AppResult, AppState};
use crate::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{CreateWebSocketTicketRequest, CreateWebSocketTicketResponse};

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
/// "ticket": "abc123...",
/// "room_id": "abc123",
/// "expires_in_secs": 30,
/// "usage": "Use in WebSocket URL: ws://host/ws/rooms/abc123?ticket=xxx"
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
            (status = 400, description = "Invalid room_id", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Caller cannot create a ticket for this room", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Room not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Ticket backend unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn create_ticket(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<CreateWebSocketTicketRequest>,
) -> AppResult<Json<CreateWebSocketTicketResponse>> {
    super::websocket::validate_websocket_runtime_dependencies(&state)?;
    let request_meta = request_meta.0;
    let client_api = state.shared_api_runtime.client_api.clone();
    let public_room_id = req.room_id.clone();

    let ticket_response =
        crate::impls::ClientApiImpl::execute_scoped_room_actor_endpoint_with_control(
            client_api.clone(),
            &request_meta,
            public_room_id,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::Ticket,
            move |client_api, request_control, actor| async move {
                client_api
                    .create_websocket_ticket_for_actor_with_control(
                        actor,
                        req,
                        Some(&request_control),
                    )
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(ticket_response))
}
