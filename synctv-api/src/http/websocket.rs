//! WebSocket handler with binary proto transmission
//!
//! This handler uses the unified `StreamMessage` trait from impls layer,
//! enabling full code reuse between gRPC and WebSocket.
//!
//! All business logic (rate limiting, content filtering, permissions, broadcasting)
//! is handled by `StreamMessageHandler.run()` with the `WebSocketStream` implementation.
//!
//! # Security Considerations
//!
//! Authentication can be provided via:
//! 1. Authorization header: `Authorization: Bearer <jwt>` (preferred, more secure)
//! 2. Query parameter: `?ticket=<ticket>` (recommended for browser clients, short-lived one-time-use)
//! 3. Query parameter: `?token=<jwt>` (legacy fallback, appears in logs/history)
//!
//! For browser clients, the ticket system is recommended:
//! - First call POST /api/tickets to get a short-lived ticket
//! - Then use `ws://host/ws/room/{room_id}?ticket=xxx`
//! - Tickets are single-use and expire quickly (30 seconds by default)

use axum::{
    extract::{Path, Query, State, WebSocketUpgrade},
    http::HeaderMap,
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::http::{AppError, AppState};
use crate::impls::messaging::{MessageSender, ProtoCodec, StreamMessage, StreamMessageHandler};
use crate::proto::client::{ClientMessage, ServerMessage};
use synctv_core::models::{RoomId, UserId};
use synctv_core::service::{ContentFilter, RateLimitConfig};

/// Query parameters for WebSocket connection
#[derive(Debug, Deserialize)]
pub struct WsQuery {
    /// JWT token for authentication (legacy method)
    /// NOTE: Token in URL may appear in server logs and browser history.
    /// Consider using ?ticket= instead for better security.
    pub token: Option<String>,

    /// WebSocket ticket for authentication (recommended)
    /// Short-lived, one-time-use ticket obtained via POST /api/tickets
    /// More secure than passing JWT in URL.
    pub ticket: Option<String>,
}

/// Authentication method used for WebSocket connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// Authorization header (most secure)
    Header,
    /// Ticket query parameter (recommended for browsers)
    Ticket,
    /// JWT token query parameter (legacy, less secure)
    TokenQuery,
}

/// Extract user ID from authentication credentials
///
/// Priority:
/// 1. Authorization header (most secure)
/// 2. Ticket query parameter (recommended for browsers)
/// 3. JWT token query parameter (legacy fallback)
async fn extract_user_id(
    state: &AppState,
    headers: &HeaderMap,
    query: &WsQuery,
) -> Result<(UserId, AuthMethod), AppError> {
    // Use the shared JwtValidator from AppState (created once at startup)
    let validator = &state.jwt_validator;

    // First, try Authorization header (most secure)
    if let Some(auth_header) = headers.get("Authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                let user_id = validator
                    .validate_and_extract_user_id(token)
                    .map_err(|e| AppError::unauthorized(format!("Invalid token: {e}")))?;

                // Check if token has been revoked (e.g. via logout)
                // Fail closed: deny if blacklist check errors (consistent with HTTP middleware)
                if state
                    .token_blacklist_service
                    .is_blacklisted(token)
                    .await
                    .unwrap_or(true)
                {
                    return Err(AppError::unauthorized("Token has been revoked"));
                }

                return Ok((user_id, AuthMethod::Header));
            }
        }
    }

    // Second, try ticket query parameter (recommended for browsers)
    if let Some(ref ticket) = query.ticket {
        if let Some(ref ws_ticket_service) = state.ws_ticket_service {
            let user_id = ws_ticket_service
                .validate_and_consume(ticket)
                .await
                .map_err(|e| AppError::unauthorized(format!("Invalid or expired ticket: {e}")))?;
            return Ok((user_id, AuthMethod::Ticket));
        }
        return Err(AppError::internal_server_error(
            "WebSocket ticket service not configured (Redis required)",
        ));
    }

    // Finally, try JWT token query parameter (legacy fallback)
    if let Some(ref token) = query.token {
        let user_id = validator
            .validate_and_extract_user_id(token)
            .map_err(|e| AppError::unauthorized(format!("Invalid token: {e}")))?;

        // Fail closed: deny if blacklist check errors (consistent with HTTP middleware)
        if state
            .token_blacklist_service
            .is_blacklisted(token)
            .await
            .unwrap_or(true)
        {
            return Err(AppError::unauthorized("Token has been revoked"));
        }

        return Ok((user_id, AuthMethod::TokenQuery));
    }

    Err(AppError::unauthorized(
        "Missing authentication: provide token via Authorization header, ?ticket=, or ?token=",
    ))
}

/// WebSocket stream implementation of `StreamMessage` trait
///
/// This adapts WebSocket's `axum::extract::ws::WebSocket` to our unified `StreamMessage` interface.
struct WebSocketStream {
    receiver: futures::stream::SplitStream<axum::extract::ws::WebSocket>,
    sender: WebSocketMessageSender,
    _is_alive: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl StreamMessage for WebSocketStream {
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>> {
        loop {
            match self.receiver.next().await {
                Some(Ok(axum::extract::ws::Message::Binary(bytes))) => {
                    return Some(ProtoCodec::decode_client_message(&bytes));
                }
                Some(Ok(axum::extract::ws::Message::Close(_))) => {
                    return None; // Graceful close
                }
                Some(Err(e)) => return Some(Err(format!("WebSocket error: {e}"))),
                None => return None, // Stream ended
                Some(Ok(_)) => {
                    // Ignore non-binary messages (text, ping, pong) and continue loop
                }
            }
        }
    }

    fn send(&self, message: ServerMessage) -> Result<(), String> {
        MessageSender::send(&self.sender, message)
    }

    fn is_alive(&self) -> bool {
        self._is_alive.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// WebSocket message sender implementation
struct WebSocketMessageSender {
    sender: tokio::sync::mpsc::Sender<Vec<u8>>,
}

impl WebSocketMessageSender {
    const fn new(sender: tokio::sync::mpsc::Sender<Vec<u8>>) -> Self {
        Self { sender }
    }
}

impl crate::impls::messaging::MessageSender for WebSocketMessageSender {
    fn send(&self, message: ServerMessage) -> Result<(), String> {
        // Encode to binary proto
        let bytes = ProtoCodec::encode_server_message(&message)?;

        // Use try_send to provide backpressure for slow clients
        // If channel is full, drop the message (client is too slow)
        self.sender.try_send(bytes).map_err(|e| match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                "Channel full: WebSocket client too slow to consume messages".to_string()
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                "Channel closed: WebSocket client disconnected".to_string()
            }
        })
    }
}

/// WebSocket handler for room real-time updates
///
/// Clients can authenticate via:
/// 1. Authorization header: `Authorization: Bearer <token>` (most secure)
/// 2. Ticket query parameter: `?ticket=<ticket>` (recommended for browsers)
/// 3. Token query parameter: `?token=<jwt>` (legacy fallback)
///
/// Example:
/// - Native clients: `ws://host/ws/room/{room_id}` with `Authorization: Bearer <token>`
/// - Browser clients: `ws://host/ws/room/{room_id}?ticket=<ticket>` (obtained from POST /api/tickets)
/// - Legacy browser: `ws://host/ws/room/{room_id}?token=<jwt>` (appears in logs)
pub async fn websocket_handler(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<WsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, AppError> {
    // Extract user ID from authentication credentials
    let (user_id, auth_method) = extract_user_id(&state, &headers, &query).await?;

    // Log warning if using legacy token query parameter (less secure)
    if auth_method == AuthMethod::TokenQuery {
        warn!(
            room_id = %room_id,
            "WebSocket authentication via ?token= query parameter (consider using ?ticket= or Authorization header for better security)"
        );
    }

    // Check room membership before upgrading
    let rid = synctv_core::models::RoomId::from_string(room_id.clone());
    let is_member = state
        .room_service
        .member_service()
        .is_member(&rid, &user_id)
        .await
        .map_err(|e| {
            AppError::internal_server_error(format!("Failed to check membership: {e}"))
        })?;

    if !is_member {
        return Err(AppError::forbidden("Not a member of this room"));
    }

    // Authentication and membership verified, upgrade to WebSocket
    // Limit max message size to 64KB (default is 64MB which is excessive for signaling)
    Ok(ws.max_message_size(64 * 1024)
        .on_upgrade(move |socket| handle_socket(socket, state, room_id, user_id)))
}

async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    state: AppState,
    room_id: String,
    user_id: UserId,
) {
    // Track WebSocket metrics (aggregate; per-room stats belong in application dashboards)
    synctv_core::metrics::http::WEBSOCKET_CONNECTIONS_ACTIVE.inc();
    synctv_core::metrics::http::WEBSOCKET_CONNECTIONS_TOTAL
        .with_label_values(&["success"])
        .inc();
    synctv_core::metrics::http::USERS_ONLINE.inc();

    // Get username from user service
    let username = state
        .user_service
        .get_username(&user_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| user_id.as_str().to_string());

    info!(
        "WebSocket connection established: user={}, room={}",
        user_id.as_str(),
        room_id
    );

    // Check if cluster_manager is available
    let cluster_manager = if let Some(cm) = state.cluster_manager {
        cm
    } else {
        error!("ClusterManager not available, WebSocket connection not supported");
        return;
    };

    let rid = RoomId::from_string(room_id.clone());

    // Use the shared rate limiter from app state
    let rate_limiter = Arc::new(state.rate_limiter.clone());
    let rate_limit_config = Arc::new(RateLimitConfig::default());
    let content_filter = Arc::new(ContentFilter::new());

    // Create channel for sending messages to WebSocket with bounded capacity
    // Buffer size of 1000 messages provides backpressure for slow clients
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1000);
    let is_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

    // Create WebSocket sender - wrapped in Arc for sharing with handler
    let ws_sender_for_handler = Arc::new(WebSocketMessageSender::new(tx.clone()));
    let ws_sender = WebSocketMessageSender::new(tx);

    // Create StreamMessageHandler with all configuration
    let stream_handler = StreamMessageHandler::new(
        rid.clone(),
        user_id.clone(),
        username.clone(),
        state.room_service.clone(),
        cluster_manager,
        (*state.connection_manager).clone(),
        rate_limiter,
        rate_limit_config,
        content_filter,
        ws_sender_for_handler,
    );

    // Split WebSocket into sender and receiver
    let (mut ws_sender_sink, ws_receiver) = socket.split();

    // Spawn task to handle server messages -> WebSocket
    let is_alive_clone = is_alive.clone();
    tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if let Err(e) = ws_sender_sink
                .send(axum::extract::ws::Message::Binary(bytes.into()))
                .await
            {
                error!("Failed to send WebSocket message: {}", e);
                is_alive_clone.store(false, std::sync::atomic::Ordering::Relaxed);
                break;
            }
        }
    });

    // Create WebSocketStream and run unified message loop
    let mut stream = WebSocketStream {
        receiver: ws_receiver,
        sender: ws_sender,
        _is_alive: is_alive,
    };

    // Run unified message loop - ALL logic is here!
    if let Err(e) = stream_handler.run(&mut stream).await {
        error!("Stream handler error: {}", e);
    }

    // Decrement WebSocket metrics on disconnect (always runs, even on error paths)
    synctv_core::metrics::http::WEBSOCKET_CONNECTIONS_ACTIVE.dec();
    synctv_core::metrics::http::USERS_ONLINE.dec();

    info!(
        "WebSocket connection closed: user={}, room={}",
        user_id.as_str(),
        room_id
    );
}
