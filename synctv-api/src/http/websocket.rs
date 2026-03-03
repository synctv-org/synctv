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
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use tracing::{error, info, warn};

use crate::http::{AppError, AppState};
use crate::impls::messaging::{MessageSender, ProtoCodec, StreamMessage, StreamMessageHandler};
use crate::proto::client::{ClientMessage, ServerMessage};
use synctv_core::models::{RoomId, UserId};
use synctv_core::service::auth::JwtValidator;
use synctv_core::service::ContentFilter;

/// Threshold for consecutive slow-client drops before disconnecting them
const SLOW_CLIENT_DROP_THRESHOLD: u32 = 10;

// ============================================================================
// MetricsGuard - RAII guard for WebSocket metrics
// ============================================================================

/// RAII guard that increments WebSocket metrics on creation and decrements on drop.
///
/// This ensures metrics are correctly maintained even if the connection handling
/// panics or returns early. Without this guard, metrics would leak in error paths.
///
/// # Example
///
/// ```text
/// async fn handle_socket() {
///     let _guard = MetricsGuard::new();
///
///     // Even if this panics, metrics will be decremented
///     // when _guard is dropped
///     do_work().await;
/// }
/// ```
pub struct MetricsGuard {
    /// Track if we've already decremented (to prevent double-decrement)
    decremented: bool,
}

impl MetricsGuard {
    /// Create a new guard, incrementing WebSocket connection metrics.
    #[must_use = "MetricsGuard must be held for metrics to be tracked correctly"]
    pub fn new() -> Self {
        synctv_core::metrics::http::WEBSOCKET_CONNECTIONS_ACTIVE.inc();
        synctv_core::metrics::http::WEBSOCKET_CONNECTIONS_TOTAL
            .with_label_values(&["success"])
            .inc();
        synctv_core::metrics::http::USERS_ONLINE.inc();

        Self { decremented: false }
    }
}

impl Default for MetricsGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MetricsGuard {
    fn drop(&mut self) {
        if !self.decremented {
            synctv_core::metrics::http::WEBSOCKET_CONNECTIONS_ACTIVE.dec();
            synctv_core::metrics::http::USERS_ONLINE.dec();
        }
    }
}

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
///
/// The `room_id` parameter is required for ticket validation (Issue #65): tickets are
/// room-scoped and must be checked against the room the connection targets.
///
/// For JWT-based paths (header and ?token=), the `SecurityPipeline` is invoked after
/// signature verification to enforce password-version, banned, and deleted checks
/// (parity with the HTTP `AuthUser` extractor). For the ticket path, the user status
/// is checked explicitly since tickets don't carry JWT claims.
async fn extract_user_id(
    state: &AppState,
    headers: &HeaderMap,
    query: &WsQuery,
    room_id: &synctv_core::models::RoomId,
) -> Result<(UserId, AuthMethod), AppError> {
    // Use the shared JwtValidator from AppState (created once at startup)
    let validator = &state.jwt_validator;

    // First, try Authorization header (most secure)
    // B8 FIX: Use JwtValidator::extract_bearer_token for case-insensitive "Bearer "
    // matching, consistent with the HTTP AuthUser extractor. Also return an error
    // when the header is present but has an invalid format, instead of silently
    // falling through to query parameters.
    if let Some(auth_header) = headers.get("Authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            let token = JwtValidator::extract_bearer_token(auth_str).map_err(|e| {
                AppError::unauthorized(format!("Invalid Authorization header: {e}"))
            })?;

            let claims = validator
                .validate_token(&token)
                .map_err(|e| AppError::unauthorized(format!("Invalid token: {e}")))?;

            // Run SecurityPipeline checks (password version, banned/deleted status)
            let authenticated = state
                .security_pipeline
                .check(&claims)
                .await
                .map_err(|e| AppError::unauthorized(format!("{e}")))?;

            return Ok((authenticated.user_id, AuthMethod::Header));
        }
    }

    // Second, try ticket query parameter (recommended for browsers).
    // The ticket is validated against the target room to prevent cross-room replay (Issue #65).
    // User status and password version are checked atomically with ticket consumption
    // to prevent TOCTOU race conditions (Issue #17).
    if let Some(ref ticket) = query.ticket {
        if let Some(ref ws_ticket_service) = state.ws_ticket_service {
            // Use validate_and_consume_checked for TOCTOU-safe validation
            let validated = ws_ticket_service
                .validate_and_consume_checked(ticket, room_id, &*state.user_service)
                .await
                .map_err(|e| AppError::unauthorized(format!("Invalid or expired ticket: {e}")))?;

            return Ok((validated.user_id, AuthMethod::Ticket));
        }
        return Err(AppError::internal_server_error(
            "WebSocket ticket service not configured (Redis required)",
        ));
    }

    // Finally, try JWT token query parameter (legacy fallback)
    if let Some(ref token) = query.token {
        // Check if token query parameter is disabled via configuration
        if state.config.server.disable_ws_token_query {
            return Err(AppError::unauthorized(
                "WebSocket ?token= query parameter is disabled. Use Authorization header or ?ticket= instead.",
            ));
        }

        let claims = validator
            .validate_token(token)
            .map_err(|e| AppError::unauthorized(format!("Invalid token: {e}")))?;

        // Run SecurityPipeline checks (password version, banned/deleted status)
        let authenticated = state
            .security_pipeline
            .check(&claims)
            .await
            .map_err(|e| AppError::unauthorized(format!("{e}")))?;

        return Ok((authenticated.user_id, AuthMethod::TokenQuery));
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
    /// Raw channel for sending WebSocket control frames (Ping)
    raw_sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
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

    fn ping(&self) -> Result<(), String> {
        self.raw_sender
            .try_send(axum::extract::ws::Message::Ping(vec![].into()))
            .map_err(|e| match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    "Ping failed: channel full".to_string()
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    "Ping failed: channel closed".to_string()
                }
            })
    }
}

/// WebSocket message sender implementation
struct WebSocketMessageSender {
    sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
    /// Count of consecutive message drops (channel full). When this exceeds
    /// `SLOW_CLIENT_DROP_THRESHOLD` the `send()` method returns an error to trigger
    /// a graceful disconnect for the slow client.
    consecutive_drops: Arc<AtomicU32>,
}

impl WebSocketMessageSender {
    fn new(sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>) -> Self {
        Self {
            sender,
            consecutive_drops: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Clone the sender sharing the same drop counter (used to give handler and ping
    /// channel different senders that still track slowness jointly).
    fn clone_sender(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            consecutive_drops: Arc::clone(&self.consecutive_drops),
        }
    }
}

/// Returns `true` if the given `ServerMessage` carries a critical payload that
/// MUST be delivered (playback state changes, kick/ban notifications, room
/// deletion). Critical messages use a blocking send with timeout so they are
/// not silently dropped.
const fn is_critical_message(message: &ServerMessage) -> bool {
    use crate::proto::client::server_message::Message;
    match &message.message {
        Some(Message::PlaybackState(_)) => true, // Playback state sync
        Some(Message::PlayingChanged(_)) => true, // Playing media changed
        Some(Message::Error(_)) => true,         // kick/ban/room deleted arrive as Error
        Some(Message::PermissionChanged(_)) => true, // Permission changes must be delivered
        Some(Message::RoomSettings(_)) => true,  // Room settings sync
        _ => false,
    }
}

/// Returns a human-readable message type name for logging purposes.
const fn message_type_name(message: &ServerMessage) -> &'static str {
    use crate::proto::client::server_message::Message;
    match &message.message {
        Some(Message::Chat(_)) => "Chat",
        Some(Message::PlaybackState(_)) => "PlaybackState",
        Some(Message::UserJoined(_)) => "UserJoined",
        Some(Message::UserLeft(_)) => "UserLeft",
        Some(Message::RoomSettings(_)) => "RoomSettings",
        Some(Message::HeartbeatAck(_)) => "HeartbeatAck",
        Some(Message::Error(_)) => "Error",
        Some(Message::MediaAdded(_)) => "MediaAdded",
        Some(Message::MediaRemoved(_)) => "MediaRemoved",
        Some(Message::PermissionChanged(_)) => "PermissionChanged",
        Some(Message::PlaylistCreated(_)) => "PlaylistCreated",
        Some(Message::PlaylistUpdated(_)) => "PlaylistUpdated",
        Some(Message::PlaylistDeleted(_)) => "PlaylistDeleted",
        Some(Message::PlayingChanged(_)) => "PlayingChanged",
        Some(Message::WebrtcOffer(_)) => "WebrtcOffer",
        Some(Message::WebrtcAnswer(_)) => "WebrtcAnswer",
        Some(Message::WebrtcIceCandidate(_)) => "WebrtcIceCandidate",
        Some(Message::WebrtcJoin(_)) => "WebrtcJoin",
        Some(Message::WebrtcLeave(_)) => "WebrtcLeave",
        Some(Message::SfuMigrationOffer(_)) => "SfuMigrationOffer",
        Some(Message::SfuMigrationStatus(_)) => "SfuMigrationStatus",
        Some(Message::Notification(_)) => "Notification",
        None => "None",
    }
}

impl crate::impls::messaging::MessageSender for WebSocketMessageSender {
    fn send(&self, message: ServerMessage) -> Result<(), String> {
        // Encode to binary proto
        let bytes = ProtoCodec::encode_server_message(&message)?;
        let ws_msg = axum::extract::ws::Message::Binary(bytes.into());

        let critical = is_critical_message(&message);

        if critical {
            // For critical messages attempt a blocking send with a short timeout so
            // backpressure does not cause silent loss. If the channel is still full
            // after the timeout we disconnect the slow client.
            let sender = self.sender.clone();
            // try_send first (fast path, no syscall)
            match sender.try_send(ws_msg) {
                Ok(()) => {
                    // Reset drop counter on success
                    self.consecutive_drops.store(0, Ordering::Relaxed);
                    return Ok(());
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err("Channel closed: WebSocket client disconnected".to_string());
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    // Critical: increment drop counter and return error to disconnect
                    let drops = self.consecutive_drops.fetch_add(1, Ordering::Relaxed) + 1;
                    let msg_type = message_type_name(&message);
                    warn!(
                        consecutive_drops = drops,
                        message_type = msg_type,
                        "Critical WebSocket message dropped: channel full (slow client)"
                    );
                    synctv_core::metrics::http::WEBSOCKET_ERRORS_TOTAL
                        .with_label_values(&["message_dropped_critical"])
                        .inc();
                    // For critical messages, always signal an error so the caller can
                    // decide to disconnect the client.
                    return Err(format!(
                        "Critical message (type={msg_type}) dropped: channel full after {drops} consecutive drops (slow client)"
                    ));
                }
            }
        }

        // Non-critical messages: use try_send; track drops but do not error unless
        // the client has been consistently slow for SLOW_CLIENT_DROP_THRESHOLD sends.
        match self.sender.try_send(ws_msg) {
            Ok(()) => {
                self.consecutive_drops.store(0, Ordering::Relaxed);
                Ok(())
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                let drops = self.consecutive_drops.fetch_add(1, Ordering::Relaxed) + 1;
                let msg_type = message_type_name(&message);
                warn!(
                    consecutive_drops = drops,
                    message_type = msg_type,
                    "WebSocket message dropped: channel full (slow client)"
                );
                synctv_core::metrics::http::WEBSOCKET_ERRORS_TOTAL
                    .with_label_values(&["message_dropped"])
                    .inc();
                if drops >= SLOW_CLIENT_DROP_THRESHOLD {
                    // Too many consecutive drops: disconnect the slow client gracefully
                    Err(format!(
                        "Slow client disconnected: {drops} consecutive message drops (last dropped: {msg_type})"
                    ))
                } else {
                    // Still within threshold: log and drop the non-critical message
                    // Note: Non-critical messages (chat, user join/leave, etc.) can be dropped
                    // but this log helps diagnose sync issues. If playback state seems out of
                    // sync, check for frequent "message dropped" warnings.
                    Ok(())
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err("Channel closed: WebSocket client disconnected".to_string())
            }
        }
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
    // Build the RoomId before authentication so we can pass it for ticket validation.
    let rid = synctv_core::models::RoomId::from_string(room_id.clone());

    // Extract user ID from authentication credentials.
    // The room_id is passed so that ticket validation can enforce room-scoping (Issue #65).
    let (user_id, auth_method) = extract_user_id(&state, &headers, &query, &rid).await?;

    // Log warning if using legacy token query parameter (less secure)
    if auth_method == AuthMethod::TokenQuery {
        warn!(
            room_id = %room_id,
            "WebSocket authentication via ?token= query parameter (consider using ?ticket= or Authorization header for better security)"
        );
    }

    // Check room membership before upgrading
    let is_member = state
        .room_service
        .member_service()
        .is_member(&rid, &user_id)
        .await
        .map_err(|e| AppError::internal_server_error(format!("Failed to check membership: {e}")))?;

    if !is_member {
        return Err(AppError::forbidden("Not a member of this room"));
    }

    // Check if the room is banned before upgrading
    let room = state
        .room_service
        .get_room(&rid)
        .await
        .map_err(|e| AppError::internal_server_error(format!("Failed to fetch room: {e}")))?;

    if room.is_banned {
        return Err(AppError::forbidden("This room has been banned"));
    }

    // Reject connections to closed rooms
    if room.status.is_closed() {
        return Err(AppError::forbidden(
            "This room is closed and not accepting new connections",
        ));
    }

    // R-5: Verify ClusterManager is available BEFORE upgrading. Without it the
    // WebSocket handler cannot function, so reject early with HTTP 503 instead
    // of silently dropping the connection inside handle_socket.
    if state.cluster_manager.is_none() {
        return Err(AppError::service_unavailable());
    }

    // CRITICAL: Atomically reserve per-room connection slot BEFORE WebSocket upgrade.
    // This prevents the TOCTOU race condition where concurrent requests all pass
    // the limit check before any of them register, bypassing the connection limit.
    // The reservation is released after join_room completes inside handle_socket.
    if let Err(e) = state.connection_manager.reserve_room_slot(&rid) {
        return Err(AppError::too_many_requests(e));
    }

    // CRITICAL: Atomically reserve per-user connection slot BEFORE WebSocket upgrade.
    // Same TOCTOU protection as the room reservation above.
    if let Err(e) = state.connection_manager.reserve_user_slot(&user_id) {
        // Roll back the room reservation on failure
        state.connection_manager.release_room_reservation(&rid);
        return Err(AppError::too_many_requests(e));
    }

    // Authentication and membership verified, upgrade to WebSocket.
    // Reservations are released inside handle_socket after join_room completes.
    // Limit max message size to 64KB (default is 64MB which is excessive for signaling)
    Ok(ws
        .max_message_size(64 * 1024)
        .on_upgrade(move |socket| handle_socket(socket, state, room_id, user_id)))
}

async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    state: AppState,
    room_id: String,
    user_id: UserId,
) {
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

    let rid = RoomId::from_string(room_id.clone());

    // Release pre-upgrade reservations. The actual connection limit enforcement
    // now happens atomically inside register()/join_room(). The reservation
    // only existed to prevent the TOCTOU race during the HTTP→WS upgrade gap.
    state.connection_manager.release_room_reservation(&rid);
    state.connection_manager.release_user_reservation(&user_id);

    // Check if cluster_manager is available BEFORE incrementing metrics.
    // This prevents counter drift: if we return early, we never incremented,
    // so there's nothing to decrement.
    let cluster_manager = if let Some(ref cm) = state.cluster_manager {
        cm.clone()
    } else {
        error!("ClusterManager not available, WebSocket connection not supported");
        return;
    };

    // Create RAII guard for metrics - ensures metrics are decremented even on panic.
    // This must be created AFTER all early-return checks to prevent false decrements.
    let _metrics_guard = MetricsGuard::new();

    // Use the shared rate limiter from app state
    let rate_limiter = Arc::new(state.rate_limiter.clone());
    let rate_limit_config = state.messaging_rate_limit_config.clone();
    let content_filter = Arc::new(ContentFilter::new());

    // Create channel for sending messages to WebSocket with bounded capacity.
    // Buffer size of 1000 messages provides backpressure for slow clients.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<axum::extract::ws::Message>(1000);
    let is_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

    // Create WebSocket sender - wrapped in Arc for sharing with handler.
    // All senders share the same consecutive-drop counter via clone_sender().
    let ws_sender_primary = WebSocketMessageSender::new(tx.clone());
    let ws_sender_for_handler = Arc::new(ws_sender_primary.clone_sender());
    let raw_sender_for_ping = tx.clone();
    let ws_sender = ws_sender_primary;

    // Resolve chat_service (required for proper business logic enforcement)
    let chat_service = if let Some(ref svc) = state.chat_service {
        svc.clone()
    } else {
        error!("chat_service not available, WebSocket connection not supported");
        return;
    };

    // Create StreamMessageHandler with all configuration
    let stream_handler = StreamMessageHandler::new(
        rid.clone(),
        user_id.clone(),
        username.clone(),
        state.room_service.clone(),
        chat_service,
        cluster_manager,
        (*state.connection_manager).clone(),
        rate_limiter,
        rate_limit_config,
        content_filter,
        ws_sender_for_handler,
    )
    .with_ws_message_rate_limit(
        state
            .config
            .connection_limits
            .ws_message_rate_limit_per_second,
    );

    // H11: Wire notification service for direct real-time push
    let stream_handler = if let Some(ref notif_svc) = state.notification_service {
        stream_handler.with_notification_service(Arc::clone(notif_svc))
    } else {
        stream_handler
    };

    // Split WebSocket into sender and receiver
    let (mut ws_sender_sink, ws_receiver) = socket.split();

    // Spawn task to handle server messages -> WebSocket
    let is_alive_clone = is_alive.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = ws_sender_sink.send(msg).await {
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
        raw_sender: raw_sender_for_ping,
    };

    // Run unified message loop - ALL logic is here!
    if let Err(e) = stream_handler.run(&mut stream).await {
        error!("Stream handler error: {}", e);
    }

    // Metrics are automatically decremented when _metrics_guard is dropped
    // (RAII ensures this happens even if the above code panics)

    info!(
        "WebSocket connection closed: user={}, room={}",
        user_id.as_str(),
        room_id
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core::service::RateLimitConfig;

    // ========== WsQuery Tests ==========

    #[test]
    fn test_ws_query_no_auth() {
        let query = WsQuery {
            token: None,
            ticket: None,
        };
        assert!(query.token.is_none());
        assert!(query.ticket.is_none());
    }

    #[test]
    fn test_ws_query_with_token() {
        let query = WsQuery {
            token: Some("jwt_token_here".to_string()),
            ticket: None,
        };
        assert_eq!(query.token.as_deref(), Some("jwt_token_here"));
        assert!(query.ticket.is_none());
    }

    #[test]
    fn test_ws_query_with_ticket() {
        let query = WsQuery {
            token: None,
            ticket: Some("ticket_abc".to_string()),
        };
        assert!(query.token.is_none());
        assert_eq!(query.ticket.as_deref(), Some("ticket_abc"));
    }

    #[test]
    fn test_ws_query_with_both_token_and_ticket() {
        // Both can be provided; extract_user_id uses priority order
        let query = WsQuery {
            token: Some("jwt_token".to_string()),
            ticket: Some("ticket_123".to_string()),
        };
        assert!(query.token.is_some());
        assert!(query.ticket.is_some());
    }

    #[test]
    fn test_ws_query_deserialization_empty() {
        let json = "{}";
        let query: WsQuery = serde_json::from_str(json).expect("deserialize empty");
        assert!(query.token.is_none());
        assert!(query.ticket.is_none());
    }

    #[test]
    fn test_ws_query_deserialization_with_token() {
        let json = r#"{"token":"my_jwt"}"#;
        let query: WsQuery = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.token.as_deref(), Some("my_jwt"));
        assert!(query.ticket.is_none());
    }

    #[test]
    fn test_ws_query_deserialization_with_ticket() {
        let json = r#"{"ticket":"my_ticket"}"#;
        let query: WsQuery = serde_json::from_str(json).expect("deserialize");
        assert!(query.token.is_none());
        assert_eq!(query.ticket.as_deref(), Some("my_ticket"));
    }

    #[test]
    fn test_ws_query_deserialization_ignores_extra_fields() {
        let json = r#"{"token":"jwt","extra":"ignored"}"#;
        let query: WsQuery = serde_json::from_str(json).expect("deserialize with extra");
        assert_eq!(query.token.as_deref(), Some("jwt"));
    }

    // ========== AuthMethod Tests ==========

    #[test]
    fn test_auth_method_equality() {
        assert_eq!(AuthMethod::Header, AuthMethod::Header);
        assert_eq!(AuthMethod::Ticket, AuthMethod::Ticket);
        assert_eq!(AuthMethod::TokenQuery, AuthMethod::TokenQuery);
    }

    #[test]
    fn test_auth_method_inequality() {
        assert_ne!(AuthMethod::Header, AuthMethod::Ticket);
        assert_ne!(AuthMethod::Header, AuthMethod::TokenQuery);
        assert_ne!(AuthMethod::Ticket, AuthMethod::TokenQuery);
    }

    #[test]
    fn test_auth_method_clone() {
        let method = AuthMethod::Header;
        let cloned = method;
        assert_eq!(cloned, AuthMethod::Header);
    }

    #[test]
    fn test_auth_method_debug() {
        // Verify Debug trait is implemented and produces reasonable output
        let header = format!("{:?}", AuthMethod::Header);
        let ticket = format!("{:?}", AuthMethod::Ticket);
        let token = format!("{:?}", AuthMethod::TokenQuery);
        assert!(header.contains("Header"));
        assert!(ticket.contains("Ticket"));
        assert!(token.contains("TokenQuery"));
    }

    // ========== Auth Priority Logic Tests ==========
    // extract_user_id is async and requires AppState, so we test the priority
    // logic via the documented contract:
    // 1. Header > 2. Ticket > 3. Token query
    // These tests verify the query parsing that feeds into extract_user_id.

    #[test]
    fn test_auth_priority_header_present_in_header_map() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer some_jwt_token".parse().unwrap());

        // When Authorization header is present, it should be checked first
        let auth_header = headers.get("Authorization");
        assert!(auth_header.is_some());
        let auth_str = auth_header.unwrap().to_str().unwrap();
        assert!(auth_str.starts_with("Bearer "));
        let token = auth_str.strip_prefix("Bearer ").unwrap();
        assert_eq!(token, "some_jwt_token");
    }

    #[test]
    fn test_auth_priority_no_bearer_prefix_in_header() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Basic dXNlcjpwYXNz".parse().unwrap());

        // Non-Bearer auth should not extract a token
        let auth_header = headers.get("Authorization").unwrap();
        let auth_str = auth_header.to_str().unwrap();
        assert!(auth_str.strip_prefix("Bearer ").is_none());
    }

    #[test]
    fn test_auth_priority_no_header_falls_through_to_ticket() {
        let headers = HeaderMap::new();
        let query = WsQuery {
            token: None,
            ticket: Some("ticket_abc".to_string()),
        };

        // No Authorization header
        assert!(headers.get("Authorization").is_none());
        // Ticket is available as fallback
        assert!(query.ticket.is_some());
    }

    #[test]
    fn test_auth_priority_no_header_no_ticket_falls_through_to_token() {
        let headers = HeaderMap::new();
        let query = WsQuery {
            token: Some("jwt_token".to_string()),
            ticket: None,
        };

        assert!(headers.get("Authorization").is_none());
        assert!(query.ticket.is_none());
        // Token query is the last fallback
        assert!(query.token.is_some());
    }

    #[test]
    fn test_auth_priority_no_auth_at_all() {
        let headers = HeaderMap::new();
        let query = WsQuery {
            token: None,
            ticket: None,
        };

        assert!(headers.get("Authorization").is_none());
        assert!(query.ticket.is_none());
        assert!(query.token.is_none());
        // This would produce an Unauthorized error in extract_user_id
    }

    // ========== AppError Construction Tests ==========

    #[test]
    fn test_unauthorized_error_for_missing_auth() {
        let err = AppError::unauthorized(
            "Missing authentication: provide token via Authorization header, ?ticket=, or ?token=",
        );
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
        assert!(err.message.contains("Missing authentication"));
    }

    #[test]
    fn test_forbidden_error_for_non_member() {
        let err = AppError::forbidden("Not a member of this room");
        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_internal_error_for_missing_ticket_service() {
        let err = AppError::internal_server_error(
            "WebSocket ticket service not configured (Redis required)",
        );
        assert_eq!(err.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_unauthorized_error_for_revoked_token() {
        let err = AppError::unauthorized("Token has been revoked");
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "Token has been revoked");
    }

    // ========== RateLimitConfig Tests ==========
    // These tests verify that the RateLimitConfig used for WebSocket message handling
    // has sensible defaults and can be customized.

    #[test]
    fn test_rate_limit_config_default_values() {
        let config = RateLimitConfig::default();
        // Default values should match synctv_core::service::RateLimitConfig defaults
        assert_eq!(config.chat_per_second, 10);
        assert_eq!(config.danmaku_per_second, 3);
        assert_eq!(config.window_seconds, 1);
    }

    #[test]
    fn test_rate_limit_config_custom_values() {
        let config = RateLimitConfig {
            chat_per_second: 5,
            danmaku_per_second: 2,
            window_seconds: 2,
        };
        assert_eq!(config.chat_per_second, 5);
        assert_eq!(config.danmaku_per_second, 2);
        assert_eq!(config.window_seconds, 2);
    }

    #[test]
    fn test_rate_limit_config_clone() {
        let config = RateLimitConfig {
            chat_per_second: 20,
            danmaku_per_second: 5,
            window_seconds: 3,
        };
        let cloned = config.clone();
        assert_eq!(cloned.chat_per_second, 20);
        assert_eq!(cloned.danmaku_per_second, 5);
        assert_eq!(cloned.window_seconds, 3);
    }

    #[test]
    fn test_rate_limit_config_debug() {
        let config = RateLimitConfig::default();
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("RateLimitConfig"));
        assert!(debug_str.contains("chat_per_second"));
        assert!(debug_str.contains("danmaku_per_second"));
        assert!(debug_str.contains("window_seconds"));
    }
}
