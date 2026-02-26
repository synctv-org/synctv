//! Unified Message Stream Implementation
//!
//! This module provides a unified implementation for handling real-time messaging
//! that can be used by both gRPC streaming and WebSocket connections.
//!
//! Architecture:
//! - Binary proto encoding/decoding
//! - Shared business logic in impls layer
//! - Transport-agnostic message handling via `MessageSender` and `StreamMessage` traits
//! - Cluster-aware broadcasting (local + Redis)
//! - All logic encapsulated in `StreamMessageHandler` (rate limiting, filtering, permissions)
//! - Complete IO abstraction via `StreamMessage` trait for both sending and receiving

use prost::Message;
use rand::RngExt;
use std::sync::Arc;
use std::time::Duration;
use synctv_cluster::sync::{ClusterEvent, ClusterManager, ConnectionManager};
use synctv_core::spawn::spawn_monitored;
use synctv_core::{
    models::{MemberStatus, PermissionBits, RoomId, UserId},
    service::{ChatService, ContentFilter, RateLimitConfig, RateLimiter, RoomService},
};
use tokio::sync::Semaphore;

use crate::proto::client::{ClientMessage, ServerMessage};

/// Default TTL for membership cache entries (60 seconds).
///
/// This TTL is chosen to balance between:
/// - Reducing database load (longer TTL = fewer queries)
/// - Responsiveness to membership changes (shorter TTL = faster detection of bans/removals)
///
/// With a 60-second TTL and 25-35 second heartbeat interval, we ensure:
/// - At most 1 DB query per connection per 60 seconds (vs. every heartbeat without cache)
/// - Banned/removed users are disconnected within ~60-95 seconds worst case
/// - The disconnect signal channel (Redis PubSub) provides immediate notification in most cases
const MEMBERSHIP_CACHE_TTL: Duration = Duration::from_secs(60);

/// Default maximum concurrent message processing operations across all connections.
/// This provides backpressure when the system is under heavy load.
/// When exceeded, new messages receive a ResourceExhausted error.
pub const DEFAULT_MAX_CONCURRENT_MESSAGE_PROCESSING: usize = 1000;

// ============================================================================
// MessageConcurrencyConfig - Instance-level concurrency configuration
// ============================================================================

/// Configuration for message processing concurrency.
///
/// This replaces the previous global `MESSAGE_PROCESSING_SEMAPHORE` with instance-level
/// configuration, enabling proper test isolation and per-AppState concurrency limits.
///
/// Each `AppState` instance can have its own `MessageConcurrencyConfig`, allowing:
/// - Different concurrency limits for different server instances
/// - Proper test isolation (tests don't share semaphores)
/// - Runtime configuration of concurrency limits
///
/// # Example
///
/// ```
/// use synctv_api::impls::MessageConcurrencyConfig;
/// use std::sync::Arc;
///
/// // Create with default limit (1000)
/// let default_config = MessageConcurrencyConfig::default();
///
/// // Create with custom limit
/// let custom_config = MessageConcurrencyConfig::new(500);
///
/// // Share across handlers via Arc
/// let shared = Arc::new(custom_config);
/// ```
#[derive(Clone, Debug)]
pub struct MessageConcurrencyConfig {
    /// Semaphore for limiting concurrent message processing.
    /// This is shared across all connections for the same AppState.
    semaphore: Arc<Semaphore>,
    /// The maximum number of concurrent message processing operations.
    max_concurrent: usize,
}

impl MessageConcurrencyConfig {
    /// Create a new concurrency config with the specified limit.
    ///
    /// # Arguments
    ///
    /// * `max_concurrent` - Maximum number of concurrent message processing operations.
    ///   When this limit is reached, new messages will receive a ResourceExhausted error.
    ///
    /// # Example
    ///
    /// ```
    /// use synctv_api::impls::MessageConcurrencyConfig;
    ///
    /// let config = MessageConcurrencyConfig::new(500);
    /// assert_eq!(config.max_concurrent(), 500);
    /// ```
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
        }
    }

    /// Get the semaphore for acquiring permits.
    ///
    /// Returns a cloned `Arc<Semaphore>` that can be used to acquire permits
    /// for message processing.
    pub fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.semaphore)
    }

    /// Get the maximum concurrent limit.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Get the number of available permits.
    ///
    /// This is useful for monitoring and health checks.
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

impl Default for MessageConcurrencyConfig {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CONCURRENT_MESSAGE_PROCESSING)
    }
}

/// Cached membership status for heartbeat validation.
///
/// This struct stores the result of a membership check to avoid
/// repeated database queries during heartbeat validation.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // Fields used for caching but not yet read
struct CachedMembership {
    /// Whether the user is still a valid member of the room
    is_member: bool,
    /// Whether the user is banned
    is_banned: bool,
}

impl CachedMembership {
    /// Create a cached membership from a member lookup result.
    #[allow(dead_code)] // Will be used when implementing cache lookup
    fn from_member(member: Option<&synctv_core::models::RoomMember>) -> Self {
        match member {
            Some(m) => Self {
                is_member: true,
                is_banned: m.status == MemberStatus::Banned,
            },
            None => Self {
                is_member: false,
                is_banned: false,
            },
        }
    }
}

/// Convert a `RoomRole` from the core models to the proto `RoomMemberRole` as i32.
const fn room_role_to_proto(role: synctv_core::models::RoomRole) -> i32 {
    match role {
        synctv_core::models::RoomRole::Creator => {
            synctv_proto::common::RoomMemberRole::Creator as i32
        }
        synctv_core::models::RoomRole::Admin => synctv_proto::common::RoomMemberRole::Admin as i32,
        synctv_core::models::RoomRole::Member => {
            synctv_proto::common::RoomMemberRole::Member as i32
        }
        synctv_core::models::RoomRole::Guest => synctv_proto::common::RoomMemberRole::Guest as i32,
    }
}

/// Trait for sending server messages to clients
///
/// Implemented by both gRPC streaming and WebSocket transports
pub trait MessageSender: Send + Sync {
    /// Send a server message to the client
    fn send(&self, message: ServerMessage) -> Result<(), String>;

    /// Check if connection is still alive.
    /// Default implementation returns true (connection assumed alive).
    fn is_alive(&self) -> bool {
        true
    }

    /// Send a ping to check connection liveness.
    /// Default implementation is a no-op (gRPC uses HTTP/2 PING automatically).
    fn ping(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Unified IO abstraction for bidirectional messaging
///
/// This trait encapsulates both sending and receiving operations for real-time communication.
/// Implemented by both WebSocket and gRPC streaming transports, allowing complete code reuse.
///
/// The key insight is that WebSocket and gRPC streaming are conceptually identical:
/// - Both are bidirectional byte streams
/// - Both use proto encoding
/// - Both need the same business logic (rate limiting, permissions, broadcasting)
///
/// By implementing this trait, we ensure that ALL connection handling logic lives in impls/,
/// with the transport layer (http/, grpc/) providing only the IO implementation.
#[async_trait::async_trait]
pub trait StreamMessage: Send + Sync {
    /// Receive a client message (blocking/async)
    ///
    /// Returns None when the connection is closed
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>>;

    /// Send a server message
    fn send(&self, message: ServerMessage) -> Result<(), String>;

    /// Check if connection is still alive
    fn is_alive(&self) -> bool;

    /// Send a ping to check connection liveness.
    /// Default implementation is a no-op (gRPC uses HTTP/2 PING automatically).
    fn ping(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Per-connection stream message handler with complete logic encapsulation
///
/// Each connection gets its own handler instance with:
/// - Connection state (`room_id`, `user_id`, username)
/// - Message I/O channels
/// - Rate limiting, content filtering, permission checking
/// - Cluster broadcasting
///
/// The handler runs its own message loop, external code only needs to:
/// 1. Create the handler with proper I/O channels
/// 2. Call `start()` to begin processing
pub struct StreamMessageHandler {
    room_id: RoomId,
    user_id: UserId,
    username: String,
    connection_id: String,
    room_service: Arc<RoomService>,
    /// Optional ChatService for proper chat message handling with business logic.
    /// When set, chat messages are processed through ChatService::send_message()
    /// which handles permission checks, content filtering, rate limiting, and persistence.
    /// When not set, falls back to direct persistence via room_service.save_chat_message().
    chat_service: Option<Arc<ChatService>>,
    cluster_manager: Arc<ClusterManager>,
    connection_manager: ConnectionManager,
    rate_limiter: Arc<RateLimiter>,
    rate_limit_config: Arc<RateLimitConfig>,
    content_filter: Arc<ContentFilter>,
    sender: Arc<dyn MessageSender>,
    /// Global per-connection WebSocket message rate limit (messages per second)
    ws_message_rate_limit: u32,
    /// Tracks whether this connection has an active WebRTC session.
    /// Used by `cleanup()` to decrement `WEBRTC_PEERS_ACTIVE` on ungraceful disconnect.
    has_webrtc_session: Arc<std::sync::atomic::AtomicBool>,
    /// R-10/R-11: When true, `cleanup()` skips broadcasting UserLeft because the
    /// event was already published by an explicit API call (leave_room/delete_room)
    /// and the WS handler is disconnecting in response to that cluster event.
    skip_cleanup_user_left: Arc<std::sync::atomic::AtomicBool>,
    /// Cached membership status for heartbeat validation.
    /// Uses TTL-based expiration (60 seconds) to reduce database load while
    /// maintaining reasonable responsiveness to membership changes.
    /// Key: (room_id, user_id) tuple for O(1) lookup.
    membership_cache: Arc<moka::sync::Cache<(String, String), CachedMembership>>,
    /// Instance-level concurrency configuration for backpressure control.
    /// This replaces the global MESSAGE_PROCESSING_SEMAPHORE with per-AppState configuration.
    concurrency_config: Arc<MessageConcurrencyConfig>,
}

impl Clone for StreamMessageHandler {
    fn clone(&self) -> Self {
        Self {
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            connection_id: self.connection_id.clone(),
            room_service: Arc::clone(&self.room_service),
            chat_service: self.chat_service.clone(),
            cluster_manager: Arc::clone(&self.cluster_manager),
            connection_manager: self.connection_manager.clone(),
            rate_limiter: Arc::clone(&self.rate_limiter),
            rate_limit_config: Arc::clone(&self.rate_limit_config),
            content_filter: Arc::clone(&self.content_filter),
            sender: Arc::clone(&self.sender),
            ws_message_rate_limit: self.ws_message_rate_limit,
            has_webrtc_session: Arc::clone(&self.has_webrtc_session),
            skip_cleanup_user_left: Arc::clone(&self.skip_cleanup_user_left),
            membership_cache: Arc::clone(&self.membership_cache),
            concurrency_config: Arc::clone(&self.concurrency_config),
        }
    }
}

impl StreamMessageHandler {
    /// Create a new stream message handler
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        room_id: RoomId,
        user_id: UserId,
        username: String,
        room_service: Arc<RoomService>,
        cluster_manager: Arc<ClusterManager>,
        connection_manager: ConnectionManager,
        rate_limiter: Arc<RateLimiter>,
        rate_limit_config: Arc<RateLimitConfig>,
        content_filter: Arc<ContentFilter>,
        sender: Arc<dyn MessageSender>,
    ) -> Self {
        Self::with_concurrency_config(
            room_id,
            user_id,
            username,
            room_service,
            cluster_manager,
            connection_manager,
            rate_limiter,
            rate_limit_config,
            content_filter,
            sender,
            Arc::new(MessageConcurrencyConfig::default()),
        )
    }

    /// Create a new stream message handler with a specific concurrency configuration.
    ///
    /// This is the preferred constructor when you need to control the concurrency limit
    /// for message processing (e.g., in tests or when configuring multiple server instances).
    #[allow(clippy::too_many_arguments)]
    pub fn with_concurrency_config(
        room_id: RoomId,
        user_id: UserId,
        username: String,
        room_service: Arc<RoomService>,
        cluster_manager: Arc<ClusterManager>,
        connection_manager: ConnectionManager,
        rate_limiter: Arc<RateLimiter>,
        rate_limit_config: Arc<RateLimitConfig>,
        content_filter: Arc<ContentFilter>,
        sender: Arc<dyn MessageSender>,
        concurrency_config: Arc<MessageConcurrencyConfig>,
    ) -> Self {
        let connection_id = format!("{}_{}", user_id.as_str(), nanoid::nanoid!(8));
        // Create membership cache with TTL for heartbeat validation.
        // This reduces database queries from every heartbeat (25-35s) to at most once per TTL (60s).
        let membership_cache = Arc::new(
            moka::sync::Cache::builder()
                .time_to_live(MEMBERSHIP_CACHE_TTL)
                .build(),
        );
        Self {
            room_id,
            user_id,
            username,
            connection_id,
            room_service,
            chat_service: None, // Can be set via with_chat_service()
            cluster_manager,
            connection_manager,
            rate_limiter,
            rate_limit_config,
            content_filter,
            sender,
            ws_message_rate_limit: 50, // default, overridden by with_ws_message_rate_limit()
            has_webrtc_session: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            skip_cleanup_user_left: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            membership_cache,
            concurrency_config,
        }
    }

    /// Set the per-connection WebSocket message rate limit from config.
    #[must_use]
    pub const fn with_ws_message_rate_limit(mut self, limit: u32) -> Self {
        self.ws_message_rate_limit = limit;
        self
    }

    /// Set the concurrency configuration for this handler.
    ///
    /// This allows configuring the message processing concurrency limit
    /// after creating the handler.
    #[must_use]
    pub fn with_concurrency(mut self, config: Arc<MessageConcurrencyConfig>) -> Self {
        self.concurrency_config = config;
        self
    }

    /// Run the complete message loop using unified IO abstraction
    ///
    /// This is the NEW recommended method that handles both sending and receiving
    /// in a single unified loop using the `StreamMessage` trait.
    ///
    /// This method:
    /// 1. Subscribes to cluster events and forwards them to the client
    /// 2. Receives client messages via the `StreamMessage` trait
    /// 3. Handles rate limiting, content filtering, and permissions
    /// 4. Broadcasts events to the cluster
    /// 5. Monitors for disconnect signals (user ban, kick, etc.)
    /// 6. Handles cleanup on disconnect
    ///
    /// The caller only needs to provide a `StreamMessage` implementation (WebSocket or gRPC).
    pub async fn run<S: StreamMessage>(&self, stream: &mut S) -> Result<(), String> {
        let room_id_str = self.room_id.as_str().to_string();

        // Register connection with connection manager
        if let Err(e) = self
            .connection_manager
            .register(self.connection_id.clone(), self.user_id.clone())
            .await
        {
            tracing::warn!("Failed to register connection: {}", e);
            return Err(e);
        }

        // Associate connection with the room (enforces per-room connection limit)
        if let Err(e) = self
            .connection_manager
            .join_room(&self.connection_id, self.room_id.clone())
            .await
        {
            // Roll back the registration since we can't join the room
            self.connection_manager
                .unregister(&self.connection_id)
                .await;
            return Err(e);
        }

        // Subscribe to cluster events using the same connection_id as ConnectionManager
        let (mut event_rx, _connection_id) = self
            .cluster_manager
            .subscribe_with_id(
                self.room_id.clone(),
                self.user_id.clone(),
                self.connection_id.clone(),
            )
            .await;

        // Subscribe to disconnect signals
        let mut disconnect_rx = self.connection_manager.subscribe_disconnect();

        // Subscribe to admin events (KickUser, etc.) for cross-replica disconnect propagation.
        // KickUser events arrive via Redis PubSub on the admin channel and are not
        // delivered through the room-level event subscription, so each connection
        // must independently monitor admin events and disconnect when targeted.
        let mut admin_rx = self.cluster_manager.subscribe_admin_events();

        // Send initial user joined notification
        stream.send(self.create_user_joined_message(&room_id_str).await)?;

        // Broadcast UserJoined event to other replicas
        self.broadcast_user_joined().await;

        // Create heartbeat interval OUTSIDE the loop so it doesn't reset
        // when other select! branches fire.
        // Add random jitter (±5 s around the 30 s base) so that 1000 concurrent
        // connections do not all fire their DB membership checks in the same
        // one-second window (thundering-herd protection).
        let heartbeat_jitter_secs = rand::rng().random_range(0u64..=10); // 0..=10
        let heartbeat_period = std::time::Duration::from_secs(25 + heartbeat_jitter_secs);
        let mut heartbeat_interval = tokio::time::interval(heartbeat_period);
        heartbeat_interval.tick().await; // Skip the immediate first tick

        // Global per-connection message rate limiter (token bucket).
        // Configured via connection_limits.ws_message_rate_limit_per_second.
        // This is local to each connection (no Redis needed).
        let global_msg_rate_limit = self.ws_message_rate_limit;
        let mut global_msg_count: u32 = 0;
        let mut global_msg_window_start = tokio::time::Instant::now();

        // Main message loop using tokio::select! for concurrent operations
        loop {
            tokio::select! {
                // Incoming client message
                client_msg_result = stream.recv() => {
                    match client_msg_result {
                        Some(Ok(msg)) => {
                            // Global per-connection rate limit check (before any processing)
                            let now = tokio::time::Instant::now();
                            if now.duration_since(global_msg_window_start) >= std::time::Duration::from_secs(1) {
                                // Reset window
                                global_msg_count = 0;
                                global_msg_window_start = now;
                            }
                            global_msg_count += 1;
                            if global_msg_count > global_msg_rate_limit {
                                tracing::warn!(
                                    user_id = %self.user_id.as_str(),
                                    room_id = %self.room_id.as_str(),
                                    limit = global_msg_rate_limit,
                                    "Global WebSocket message rate limit exceeded, dropping message"
                                );
                                continue;
                            }

                            // Backpressure control: try to acquire a semaphore permit.
                            // If the system is overloaded, return ResourceExhausted error instead of processing.
                            let semaphore = self.concurrency_config.semaphore();
                            let permit = match semaphore.try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    tracing::warn!(
                                        user_id = %self.user_id.as_str(),
                                        room_id = %self.room_id.as_str(),
                                        "System overloaded: message processing semaphore exhausted, returning ResourceExhausted"
                                    );
                                    // Send ResourceExhausted error to client
                                    let error_msg = ServerMessage {
                                        message: Some(crate::proto::client::server_message::Message::Error(
                                            crate::proto::client::ErrorMessage {
                                                message: "System overloaded, please retry later".to_string(),
                                                code: crate::impls::error_codes::RESOURCE_EXHAUSTED,
                                                detail: String::new(),
                                            },
                                        )),
                                    };
                                    let _ = stream.send(error_msg);
                                    continue;
                                }
                            };

                            // Process message with semaphore permit held
                            let _permit = permit; // Hold permit for duration of processing
                            if let Err(e) = self.handle_client_message(&msg).await {
                                tracing::error!("Failed to handle client message: {}", e);
                                // Don't break on individual message errors, continue processing
                            }
                        }
                        Some(Err(e)) => {
                            tracing::error!("Error receiving message: {}", e);
                            break;
                        }
                        None => {
                            tracing::info!("Client disconnected gracefully");
                            break;
                        }
                    }
                }

                // Cluster event (broadcast to client)
                event = event_rx.recv() => {
                    if let Some(event) = event {
                        // Filter WebRTC signaling: only deliver to the intended recipient.
                        // SDP data contains IP addresses, so broadcasting to all room
                        // members is both a privacy leak and causes incorrect WebRTC behavior.
                        if let ClusterEvent::WebRTCSignaling { ref to, .. } = event {
                            let is_target = if let Some((_user, conn)) = to.rsplit_once(':') {
                                conn == self.connection_id
                            } else {
                                // Fallback: `to` is just a user_id
                                *to == self.user_id.as_str()
                            };
                            if !is_target {
                                continue;
                            }
                        }

                        if let Some(msg) = cluster_event_to_server_message(&event, &room_id_str) {
                            if let Err(e) = stream.send(msg) {
                                tracing::error!("Failed to send server message: {}", e);
                                break;
                            }
                        }
                    } else {
                        tracing::error!("Cluster event channel closed");
                        break;
                    }
                }

                // Disconnect signal (forced disconnect by server)
                signal = disconnect_rx.recv() => {
                    match signal {
                        Ok(synctv_cluster::sync::DisconnectSignal::Connection(conn_id)) => {
                            if conn_id == self.connection_id {
                                tracing::info!(
                                    connection_id = %self.connection_id,
                                    "Received disconnect signal for this connection"
                                );
                                break;
                            }
                        }
                        Ok(synctv_cluster::sync::DisconnectSignal::User(uid)) => {
                            if uid == self.user_id {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    "Received disconnect signal for this user (ban/kick)"
                                );
                                break;
                            }
                        }
                        Ok(synctv_cluster::sync::DisconnectSignal::Room(rid)) => {
                            if rid == self.room_id {
                                tracing::info!(
                                    room_id = %self.room_id.as_str(),
                                    "Received disconnect signal for this room"
                                );
                                // R-10/R-11: Room deletion already published
                                // RoomDeleted event; skip redundant UserLeft.
                                self.skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Ok(synctv_cluster::sync::DisconnectSignal::UserFromRoom { user_id: uid, room_id: rid }) => {
                            if uid == self.user_id && rid == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    room_id = %self.room_id.as_str(),
                                    "Received disconnect signal: kicked from room"
                                );
                                // R-10/R-11: The leave_room API already published
                                // a UserLeft event; skip redundant broadcast in cleanup().
                                self.skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Channel lagged: we may have missed critical disconnect signals.
                            // Re-subscribe to get a fresh receiver so future signals are not lost,
                            // then verify membership to catch any missed kick/ban.
                            tracing::warn!(
                                lagged = n,
                                user_id = %self.user_id.as_str(),
                                room_id = %self.room_id.as_str(),
                                "Disconnect signal channel lagged, re-subscribing and verifying membership"
                            );
                            disconnect_rx = self.connection_manager.subscribe_disconnect();

                            // Fallback: check database to see if we were kicked/banned while lagged
                            match self.room_service.member_service().get_member(&self.room_id, &self.user_id).await {
                                Ok(Some(member)) => {
                                    if member.status == synctv_core::models::MemberStatus::Banned {
                                        tracing::info!(
                                            user_id = %self.user_id.as_str(),
                                            room_id = %self.room_id.as_str(),
                                            "User is banned (detected after disconnect signal lag), disconnecting"
                                        );
                                        break;
                                    }
                                }
                                Ok(None) => {
                                    tracing::info!(
                                        user_id = %self.user_id.as_str(),
                                        room_id = %self.room_id.as_str(),
                                        "User is no longer a member (detected after disconnect signal lag), disconnecting"
                                    );
                                    break;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "Failed to verify membership after disconnect signal lag"
                                    );
                                    // Continue - we'll catch it on the next event or heartbeat
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::error!("Disconnect signal channel closed");
                            break;
                        }
                    }
                }

                // Admin events from cluster (cross-replica kick/ban propagation)
                admin_event = admin_rx.recv() => {
                    match admin_event {
                        Ok(ClusterEvent::KickUser { ref user_id, ref reason, .. }) => {
                            if *user_id == self.user_id {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    reason = %reason,
                                    "Received cross-replica KickUser event, disconnecting"
                                );
                                break;
                            }
                        }
                        Ok(ClusterEvent::KickUserFromRoom { ref user_id, ref room_id, ref reason, .. }) => {
                            if *user_id == self.user_id && *room_id == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    room_id = %self.room_id.as_str(),
                                    reason = %reason,
                                    "Received cross-replica KickUserFromRoom event, disconnecting"
                                );
                                break;
                            }
                        }
                        Ok(ClusterEvent::UserLeft { ref user_id, ref room_id, .. }) => {
                            if *user_id == self.user_id && *room_id == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    room_id = %self.room_id.as_str(),
                                    "Received cross-replica UserLeft event, disconnecting"
                                );
                                // R-10/R-11: The UserLeft event was already published
                                // by the leave_room/delete_room API call. Skip the
                                // redundant broadcast in cleanup().
                                self.skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Ok(ClusterEvent::UserNotification { ref user_id, ref title, ref content, ref notification_type, ref notification_id, .. }) => {
                            // RT-1: Push persistent notification to user's active WebSocket connection.
                            // NOTE: Uses Error variant with code 5000 (NOTIFICATION_PUSH) as a
                            // transport for notifications. See error_codes::NOTIFICATION_PUSH docs.
                            if *user_id == self.user_id {
                                let json_data = serde_json::json!({
                                    "type": "user_notification",
                                    "notification_id": notification_id,
                                    "notification_type": notification_type,
                                    "title": title,
                                    "content": content,
                                });
                                let msg = ServerMessage {
                                    message: Some(crate::proto::client::server_message::Message::Error(
                                        crate::proto::client::ErrorMessage {
                                            message: json_data.to_string(),
                                            code: crate::impls::error_codes::NOTIFICATION_PUSH,
                                            detail: String::new(),
                                        },
                                    )),
                                };
                                if let Err(e) = stream.send(msg) {
                                    tracing::error!("Failed to push notification to WebSocket: {}", e);
                                }
                            }
                        }
                        Ok(_) => {
                            // Other admin events (KickPublisher, etc.) not relevant to this connection
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Channel lagged: we may have missed critical KickUser/KickUserFromRoom events.
                            // Re-subscribe to get a fresh receiver so future events are not lost.
                            tracing::warn!(
                                lagged = n,
                                user_id = %self.user_id.as_str(),
                                "Admin event channel lagged, re-subscribing and verifying membership"
                            );
                            admin_rx = self.cluster_manager.subscribe_admin_events();

                            // Fallback: query database to confirm member status since we may
                            // have missed a KickUser or KickUserFromRoom event during the lag.
                            match self.room_service.member_service().get_member(&self.room_id, &self.user_id).await {
                                Ok(Some(member)) => {
                                    if member.status == synctv_core::models::MemberStatus::Banned {
                                        tracing::info!(
                                            user_id = %self.user_id.as_str(),
                                            room_id = %self.room_id.as_str(),
                                            "User is banned (detected after admin event lag), disconnecting"
                                        );
                                        break;
                                    }
                                }
                                Ok(None) => {
                                    tracing::info!(
                                        user_id = %self.user_id.as_str(),
                                        room_id = %self.room_id.as_str(),
                                        "User is no longer a member (detected after admin event lag), disconnecting"
                                    );
                                    break;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "Failed to verify membership after admin event lag"
                                    );
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::error!("Admin event channel closed");
                            break;
                        }
                    }
                }

                // Heartbeat/health check every 30 seconds.
                // Also acts as a periodic membership re-validation backstop:
                // verifies the user is still a valid (non-banned, non-removed)
                // member of the room. This catches cases where the disconnect
                // signal channel lagged and the ban/kick signal was lost.
                _ = heartbeat_interval.tick() => {
                    if !stream.is_alive() {
                        tracing::info!("Connection no longer alive");
                        break;
                    }
                    if let Err(e) = stream.ping() {
                        tracing::info!("Ping failed, connection dead: {}", e);
                        break;
                    }

                    // Periodic membership re-validation (backstop for lost disconnect signals).
                    match self.room_service.member_service().get_member(&self.room_id, &self.user_id).await {
                        Ok(Some(member)) => {
                            if member.status == synctv_core::models::MemberStatus::Banned {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    room_id = %self.room_id.as_str(),
                                    "Periodic check: user is banned, disconnecting"
                                );
                                break;
                            }
                        }
                        Ok(None) => {
                            tracing::info!(
                                user_id = %self.user_id.as_str(),
                                room_id = %self.room_id.as_str(),
                                "Periodic check: user is no longer a member, disconnecting"
                            );
                            break;
                        }
                        Err(e) => {
                            // Log but don't disconnect — transient DB error should not
                            // kick valid users. Will retry on the next 30-second tick.
                            tracing::warn!(
                                error = %e,
                                user_id = %self.user_id.as_str(),
                                "Periodic membership check failed (will retry)"
                            );
                        }
                    }
                }
            }
        }

        // Cleanup: notify cluster that user left
        self.cleanup(&room_id_str).await;

        Ok(())
    }

    /// Create initial user joined message with actual role and permissions
    /// fetched from the room membership data.
    async fn create_user_joined_message(&self, room_id: &str) -> ServerMessage {
        use crate::proto::client::server_message::Message;
        use crate::proto::client::UserJoinedRoom;
        use synctv_proto::common::RoomMember;

        // Fetch the actual role and permissions from the membership record
        let (role_proto, permissions, added, removed, admin_added, admin_removed) = match self
            .room_service
            .member_service()
            .get_member(&self.room_id, &self.user_id)
            .await
        {
            Ok(Some(member)) => {
                let effective = member.effective_permissions(member.role.permissions());
                let role = room_role_to_proto(member.role);
                (
                    role,
                    effective.0,
                    member.added_permissions,
                    member.removed_permissions,
                    member.admin_added_permissions,
                    member.admin_removed_permissions,
                )
            }
            _ => {
                // Fallback: if we can't fetch membership, use Member defaults
                (
                    synctv_proto::common::RoomMemberRole::Member as i32,
                    synctv_core::models::PermissionBits::DEFAULT_MEMBER,
                    0,
                    0,
                    0,
                    0,
                )
            }
        };

        ServerMessage {
            message: Some(Message::UserJoined(UserJoinedRoom {
                room_id: room_id.to_string(),
                member: Some(RoomMember {
                    room_id: room_id.to_string(),
                    user_id: self.user_id.as_str().to_string(),
                    username: self.username.clone(),
                    role: role_proto,
                    permissions,
                    added_permissions: added,
                    removed_permissions: removed,
                    admin_added_permissions: admin_added,
                    admin_removed_permissions: admin_removed,
                    joined_at: chrono::Utc::now().timestamp(),
                    is_online: true,
                }),
            })),
        }
    }

    /// Broadcast `UserJoined` event to cluster replicas
    async fn broadcast_user_joined(&self) {
        // Fetch the actual role and permissions from the membership record
        let (role_proto, permissions) = match self
            .room_service
            .member_service()
            .get_member(&self.room_id, &self.user_id)
            .await
        {
            Ok(Some(member)) => {
                let effective = member.effective_permissions(member.role.permissions());
                let role = room_role_to_proto(member.role);
                (role, effective)
            }
            _ => {
                // Fallback: if we can't fetch membership, use Member defaults
                (
                    synctv_proto::common::RoomMemberRole::Member as i32,
                    synctv_core::models::PermissionBits(
                        synctv_core::models::PermissionBits::DEFAULT_MEMBER,
                    ),
                )
            }
        };

        let event = ClusterEvent::UserJoined {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            permissions,
            role: role_proto,
            timestamp: chrono::Utc::now(),
        };
        let result = self.cluster_manager.broadcast(event);
        if !result.redis_sent {
            tracing::warn!(
                room_id = %self.room_id.as_str(),
                user_id = %self.user_id.as_str(),
                "UserJoined cluster broadcast did not reach Redis (non-critical: join is local-only)"
            );
        }
    }

    /// Cleanup on disconnect
    async fn cleanup(&self, room_id: &str) {
        // RT-2: If this connection had an active WebRTC session, decrement the
        // metric and broadcast WebRtcLeave so other peers can clean up.
        // Use Acquire ordering to synchronize with the Release store in handle_webrtc_join/leave.
        if self
            .has_webrtc_session
            .swap(false, std::sync::atomic::Ordering::Acquire)
        {
            synctv_core::metrics::http::WEBRTC_PEERS_ACTIVE.dec();

            // Mark the connection as no longer RTC-joined in the connection manager
            self.connection_manager.mark_rtc_joined(
                &self.room_id,
                &self.user_id,
                &self.connection_id,
                false,
            );

            // Broadcast WebRtcLeave so other peers know this user dropped
            let leave_event = ClusterEvent::WebRTCLeave {
                event_id: nanoid::nanoid!(16),
                room_id: self.room_id.clone(),
                user_id: self.user_id.clone(),
                conn_id: self.connection_id.clone(),
                timestamp: chrono::Utc::now(),
            };
            self.cluster_manager.broadcast(leave_event);

            tracing::info!(
                user = %self.username,
                room = %room_id,
                connection = %self.connection_id,
                "WebRTC session cleaned up on disconnect"
            );
        }

        // R-10/R-11: If the disconnect was triggered by a cluster event that
        // already published UserLeft (e.g. leave_room or delete_room API), skip
        // the redundant broadcast to avoid double UserLeft events.
        if self.skip_cleanup_user_left.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::debug!(
                user = %self.username,
                room = %room_id,
                "Skipping UserLeft broadcast in cleanup (already published by API call)"
            );
            // Still unregister from connection manager
            self.connection_manager
                .unregister(&self.connection_id)
                .await;
            return;
        }

        // Broadcast UserLeft BEFORE unregistering from the connection manager.
        // This order prevents state divergence: if the broadcast reaches subscribers
        // while this connection is still registered, they see a consistent view.
        // Previously, unregistering first could leave the hub with a stale subscriber
        // if the broadcast was delayed or had no receivers.
        let event = ClusterEvent::UserLeft {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            timestamp: chrono::Utc::now(),
        };
        let result = self.cluster_manager.broadcast(event);

        if result.local_sent == 0 && !result.redis_sent {
            // Critical UserLeft event failed to reach any destination.
            // This can happen when Redis is temporarily unavailable.
            // Spawn a background task to retry the broadcast with exponential backoff.
            let cluster_manager = self.cluster_manager.clone();
            let room_id = self.room_id.clone();
            let user_id = self.user_id.clone();
            let username = self.username.clone();
            let connection_id = self.connection_id.clone();

            tracing::warn!(
                user = %username,
                room = %room_id.as_str(),
                connection = %connection_id,
                "UserLeft broadcast reached no subscribers; starting retry task"
            );

            spawn_monitored("userleft_retry", async move {
                const MAX_RETRIES: u32 = 5;
                const INITIAL_DELAY_MS: u64 = 100;
                const MAX_DELAY_MS: u64 = 5000;

                let mut delay_ms = INITIAL_DELAY_MS;

                for attempt in 1..=MAX_RETRIES {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

                    let retry_event = ClusterEvent::UserLeft {
                        event_id: nanoid::nanoid!(16),
                        room_id: room_id.clone(),
                        user_id: user_id.clone(),
                        username: username.clone(),
                        timestamp: chrono::Utc::now(),
                    };

                    let retry_result = cluster_manager.broadcast(retry_event);

                    if retry_result.local_sent > 0 || retry_result.redis_sent {
                        tracing::info!(
                            user = %username,
                            room = %room_id.as_str(),
                            connection = %connection_id,
                            attempt = attempt,
                            local_sent = retry_result.local_sent,
                            redis_sent = retry_result.redis_sent,
                            "UserLeft retry succeeded"
                        );
                        return;
                    }

                    tracing::warn!(
                        user = %username,
                        room = %room_id.as_str(),
                        connection = %connection_id,
                        attempt = attempt,
                        max_retries = MAX_RETRIES,
                        "UserLeft retry attempt failed"
                    );

                    // Exponential backoff with cap
                    delay_ms = std::cmp::min(delay_ms * 2, MAX_DELAY_MS);
                }

                tracing::error!(
                    user = %username,
                    room = %room_id.as_str(),
                    connection = %connection_id,
                    "UserLeft event permanently lost after {} retry attempts; other replicas may have stale user state",
                    MAX_RETRIES
                );
            });
        }

        // Now unregister from connection manager after broadcast has been sent
        self.connection_manager
            .unregister(&self.connection_id)
            .await;

        tracing::info!(
            "Cleanup complete for user {} in room {} (connection: {})",
            self.username,
            room_id,
            self.connection_id
        );
    }

    /// Start the message handling loop
    ///
    /// This method:
    /// 1. Registers the connection and joins the room (enforcing connection limits)
    /// 2. Subscribes to cluster events and forwards them to the client
    /// 3. Spawns a task to handle incoming client messages
    /// 4. Returns a sender and a cancellation token for the caller to manage lifecycle
    ///
    /// Returns a tuple of (sender, `CancellationToken`), or an error if connection limits
    /// are exceeded. Drop the `CancellationToken` or call `cancel()` on it to stop the
    /// spawned tasks and trigger cleanup (unregister, unsubscribe).
    pub async fn start(
        &self,
    ) -> Result<
        (
            tokio::sync::mpsc::Sender<ClientMessage>,
            tokio_util::sync::CancellationToken,
        ),
        String,
    > {
        // Register connection with connection manager
        self.connection_manager
            .register(self.connection_id.clone(), self.user_id.clone())
            .await?;

        // Associate connection with the room (enforces per-room connection limit)
        if let Err(e) = self
            .connection_manager
            .join_room(&self.connection_id, self.room_id.clone())
            .await
        {
            self.connection_manager
                .unregister(&self.connection_id)
                .await;
            return Err(e);
        }

        let cancel_token = tokio_util::sync::CancellationToken::new();

        // Send initial UserJoined message to the client (mirrors run() behavior)
        let room_id_str = self.room_id.as_str().to_string();
        let initial_msg = self.create_user_joined_message(&room_id_str).await;
        if let Err(e) = self.sender.send(initial_msg) {
            tracing::error!("Failed to send initial UserJoined message in start(): {e}");
        }

        // Broadcast UserJoined event to other replicas (mirrors run() behavior)
        self.broadcast_user_joined().await;

        // Use bounded channel to prevent memory exhaustion from fast clients
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ClientMessage>(256);

        // Subscribe to cluster events using the same connection_id as ConnectionManager
        let room_id = self.room_id.clone();
        let user_id = self.user_id.clone();
        let room_id_str = room_id.as_str().to_string();
        let event_connection_id = self.connection_id.clone();
        let event_user_id = self.user_id.clone();
        let (mut rx_events, _connection_id) = self
            .cluster_manager
            .subscribe_with_id(room_id, user_id, self.connection_id.clone())
            .await;
        let sender = self.sender.clone();

        let event_token = cancel_token.clone();
        spawn_monitored("messaging_event_dispatch", async move {
            loop {
                tokio::select! {
                    () = event_token.cancelled() => break,
                    event = rx_events.recv() => {
                        match event {
                            Some(event) => {
                                // Filter WebRTC signaling: only deliver to the intended
                                // recipient (same logic as run()). SDP data contains IP
                                // addresses, so broadcasting to all room members is both
                                // a privacy leak and causes incorrect WebRTC behavior.
                                if let ClusterEvent::WebRTCSignaling { ref to, .. } = event {
                                    let is_target = if let Some((_user, conn)) = to.rsplit_once(':') {
                                        conn == event_connection_id
                                    } else {
                                        *to == event_user_id.as_str()
                                    };
                                    if !is_target {
                                        continue;
                                    }
                                }

                                let is_room_deleted = matches!(event, ClusterEvent::RoomDeleted { .. });

                                if let Some(msg) = cluster_event_to_server_message(&event, &room_id_str) {
                                    if let Err(e) = sender.send(msg) {
                                        tracing::error!("Failed to send message: {}", e);
                                        break;
                                    }
                                }

                                // After delivering RoomDeleted, trigger cancellation so
                                // cleanup fires only after the event has been forwarded.
                                // This prevents the race where the cleanup task fires
                                // before the critical event reaches the client.
                                if is_room_deleted {
                                    tracing::info!(
                                        "RoomDeleted event delivered in start(), triggering cleanup"
                                    );
                                    event_token.cancel();
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        // Spawn task to handle incoming messages (with rate limiting matching run())
        let handler = self.clone();
        let msg_token = cancel_token.clone();
        let global_msg_rate_limit = self.ws_message_rate_limit;
        spawn_monitored("messaging_client_handler", async move {
            let mut global_msg_count: u32 = 0;
            let mut global_msg_window_start = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    () = msg_token.cancelled() => break,
                    msg = rx.recv() => {
                        match msg {
                            Some(msg) => {
                                // Global per-connection rate limit check (matching run() logic)
                                let now = tokio::time::Instant::now();
                                if now.duration_since(global_msg_window_start) >= std::time::Duration::from_secs(1) {
                                    global_msg_count = 0;
                                    global_msg_window_start = now;
                                }
                                global_msg_count += 1;
                                if global_msg_count > global_msg_rate_limit {
                                    tracing::warn!(
                                        connection_id = %handler.connection_id,
                                        limit = global_msg_rate_limit,
                                        "gRPC start() message rate limit exceeded, dropping message"
                                    );
                                    continue;
                                }

                                // Backpressure control: try to acquire a semaphore permit.
                                // If the system is overloaded, skip this message.
                                let semaphore = handler.concurrency_config.semaphore();
                                let permit = match semaphore.try_acquire_owned() {
                                    Ok(permit) => permit,
                                    Err(_) => {
                                        tracing::warn!(
                                            connection_id = %handler.connection_id,
                                            "System overloaded: message processing semaphore exhausted in start()"
                                        );
                                        continue;
                                    }
                                };

                                // Process message with semaphore permit held
                                let _permit = permit;
                                if let Err(e) = handler.handle_client_message(&msg).await {
                                    tracing::error!("Failed to handle client message: {}", e);
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        // Spawn task to monitor disconnect signals and admin events.
        // When a relevant signal is received, cancel the token to stop all other tasks.
        {
            let mut disconnect_rx = self.connection_manager.subscribe_disconnect();
            let mut admin_rx = self.cluster_manager.subscribe_admin_events();
            let disconnect_token = cancel_token.clone();
            let connection_id = self.connection_id.clone();
            let user_id = self.user_id.clone();
            let room_id = self.room_id.clone();
            let room_service = Arc::clone(&self.room_service);
            let cluster_manager = Arc::clone(&self.cluster_manager);
            let connection_manager = self.connection_manager.clone();
            let admin_sender = self.sender.clone();

            spawn_monitored("messaging_disconnect_monitor", async move {
                loop {
                    tokio::select! {
                        () = disconnect_token.cancelled() => break,

                        signal = disconnect_rx.recv() => {
                            let should_disconnect = match &signal {
                                Ok(synctv_cluster::sync::DisconnectSignal::Connection(conn_id)) => {
                                    *conn_id == connection_id
                                }
                                Ok(synctv_cluster::sync::DisconnectSignal::User(uid)) => {
                                    *uid == user_id
                                }
                                Ok(synctv_cluster::sync::DisconnectSignal::Room(rid)) => {
                                    *rid == room_id
                                }
                                Ok(synctv_cluster::sync::DisconnectSignal::UserFromRoom { user_id: uid, room_id: rid }) => {
                                    *uid == user_id && *rid == room_id
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => false,
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => true,
                            };
                            // Handle lag separately (needs mutable borrow of disconnect_rx)
                            if let Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) = signal {
                                tracing::warn!(
                                    lagged = n,
                                    user_id = %user_id.as_str(),
                                    "Disconnect signal channel lagged in start(), re-subscribing and verifying"
                                );
                                disconnect_rx = connection_manager.subscribe_disconnect();
                                // Verify membership after lag
                                let is_removed = match room_service.member_service().get_member(&room_id, &user_id).await {
                                    Ok(Some(member)) => member.status == synctv_core::models::MemberStatus::Banned,
                                    Ok(None) => true,
                                    _ => false,
                                };
                                if is_removed {
                                    disconnect_token.cancel();
                                    break;
                                }
                            } else if should_disconnect {
                                tracing::info!(
                                    connection_id = %connection_id,
                                    "Disconnect signal received in start(), cancelling"
                                );
                                disconnect_token.cancel();
                                break;
                            }
                        }

                        admin_event = admin_rx.recv() => {
                            // RT-1: Push UserNotification to this user's WebSocket.
                            // NOTE: Uses Error variant with code 5000 (NOTIFICATION_PUSH) as a
                            // transport for notifications. See error_codes::NOTIFICATION_PUSH docs.
                            if let Ok(ClusterEvent::UserNotification { user_id: ref uid, ref title, ref content, ref notification_type, ref notification_id, .. }) = admin_event {
                                if *uid == user_id {
                                    let json_data = serde_json::json!({
                                        "type": "user_notification",
                                        "notification_id": notification_id,
                                        "notification_type": notification_type,
                                        "title": title,
                                        "content": content,
                                    });
                                    let msg = ServerMessage {
                                        message: Some(crate::proto::client::server_message::Message::Error(
                                            crate::proto::client::ErrorMessage {
                                                message: json_data.to_string(),
                                                code: crate::impls::error_codes::NOTIFICATION_PUSH,
                                                detail: String::new(),
                                            },
                                        )),
                                    };
                                    if let Err(e) = admin_sender.send(msg) {
                                        tracing::error!("Failed to push notification in start(): {}", e);
                                    }
                                }
                                continue;
                            }
                            let should_disconnect = match &admin_event {
                                Ok(ClusterEvent::KickUser { user_id: uid, .. }) => {
                                    *uid == user_id
                                }
                                Ok(ClusterEvent::KickUserFromRoom { user_id: uid, room_id: rid, .. }) => {
                                    *uid == user_id && *rid == room_id
                                }
                                Ok(ClusterEvent::UserLeft { user_id: uid, room_id: rid, .. }) => {
                                    *uid == user_id && *rid == room_id
                                }
                                Ok(_) => false,
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => false,
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => true,
                            };
                            // Handle lag separately
                            if let Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) = admin_event {
                                tracing::warn!(
                                    lagged = n,
                                    user_id = %user_id.as_str(),
                                    "Admin event channel lagged in start(), re-subscribing and verifying"
                                );
                                admin_rx = cluster_manager.subscribe_admin_events();
                                // Verify membership after lag
                                let is_removed = match room_service.member_service().get_member(&room_id, &user_id).await {
                                    Ok(Some(member)) => member.status == synctv_core::models::MemberStatus::Banned,
                                    Ok(None) => true,
                                    _ => false,
                                };
                                if is_removed {
                                    disconnect_token.cancel();
                                    break;
                                }
                            } else if should_disconnect {
                                tracing::info!(
                                    connection_id = %connection_id,
                                    "Admin event triggered disconnect in start(), cancelling"
                                );
                                disconnect_token.cancel();
                                break;
                            }
                        }
                    }
                }
            });
        }

        // Spawn periodic heartbeat task for membership re-validation (mirrors run() behavior).
        // Verifies every 25-35 seconds that the user is still a valid, non-banned member.
        // Jitter prevents the thundering-herd problem where all 1000+ concurrent connections
        // fire their DB membership checks simultaneously at the same 30-second boundary.
        // This catches cases where disconnect signals were lost (e.g., channel lag).
        {
            let heartbeat_token = cancel_token.clone();
            let heartbeat_room_id = self.room_id.clone();
            let heartbeat_user_id = self.user_id.clone();
            let heartbeat_room_service = Arc::clone(&self.room_service);
            let heartbeat_sender = Arc::clone(&self.sender);
            spawn_monitored("messaging_heartbeat", async move {
                // Derive jitter from the user_id bytes so each connection gets a
                // stable-but-different offset within the 25–35 s window.
                let jitter_secs = heartbeat_user_id
                    .as_str()
                    .bytes()
                    .fold(0u64, |a, b| a.wrapping_add(u64::from(b)))
                    % 11; // 0..=10
                let period = std::time::Duration::from_secs(25 + jitter_secs);
                let mut interval = tokio::time::interval(period);
                interval.tick().await; // Skip the immediate first tick
                loop {
                    tokio::select! {
                        () = heartbeat_token.cancelled() => break,
                        _ = interval.tick() => {
                            // Check connection liveness first (mirrors run() behavior)
                            if !heartbeat_sender.is_alive() {
                                tracing::info!("start() connection no longer alive");
                                heartbeat_token.cancel();
                                break;
                            }
                            if let Err(e) = heartbeat_sender.ping() {
                                tracing::info!("start() ping failed, connection dead: {}", e);
                                heartbeat_token.cancel();
                                break;
                            }

                            match heartbeat_room_service.member_service().get_member(&heartbeat_room_id, &heartbeat_user_id).await {
                                Ok(Some(member)) => {
                                    if member.status == synctv_core::models::MemberStatus::Banned {
                                        tracing::info!(
                                            user_id = %heartbeat_user_id.as_str(),
                                            room_id = %heartbeat_room_id.as_str(),
                                            "start() periodic check: user is banned, disconnecting"
                                        );
                                        heartbeat_token.cancel();
                                        break;
                                    }
                                }
                                Ok(None) => {
                                    tracing::info!(
                                        user_id = %heartbeat_user_id.as_str(),
                                        room_id = %heartbeat_room_id.as_str(),
                                        "start() periodic check: user is no longer a member, disconnecting"
                                    );
                                    heartbeat_token.cancel();
                                    break;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        user_id = %heartbeat_user_id.as_str(),
                                        "start() periodic membership check failed (will retry)"
                                    );
                                }
                            }
                        }
                    }
                }
            });
        }

        // Spawn cleanup task that waits for cancellation
        let cleanup_handler = self.clone();
        let cleanup_room_id = self.room_id.as_str().to_string();
        let cleanup_token = cancel_token.clone();
        spawn_monitored("messaging_cleanup", async move {
            cleanup_token.cancelled().await;
            cleanup_handler.cleanup(&cleanup_room_id).await;
        });

        Ok((tx, cancel_token))
    }

    /// Handle incoming client message with all validations
    pub async fn handle_client_message(&self, msg: &ClientMessage) -> Result<(), String> {
        use crate::proto::client::client_message::Message;

        match &msg.message {
            Some(Message::Chat(chat_msg)) => {
                // R-6: Check permission first, before spending resources on rate
                // limiting and content filtering for users who lack SEND_CHAT.
                self.room_service
                    .check_permission(&self.room_id, &self.user_id, PermissionBits::SEND_CHAT)
                    .await
                    .map_err(|e| e.to_string())?;

                // Validate message length
                if chat_msg.content.is_empty() {
                    return Err("Chat message cannot be empty".to_string());
                }
                if chat_msg.content.chars().count()
                    > synctv_core::service::chat::MAX_CHAT_MESSAGE_CHARS
                {
                    return Err(format!(
                        "Chat message too long (max {} characters)",
                        synctv_core::service::chat::MAX_CHAT_MESSAGE_CHARS,
                    ));
                }

                // Check if this is a danmaku message (has position)
                let is_danmaku = chat_msg.position.is_some();

                // Check if chat/danmaku is enabled in room settings
                let room_settings = self
                    .room_service
                    .get_room_settings(&self.room_id)
                    .await
                    .map_err(|e| e.to_string())?;
                if is_danmaku {
                    if !room_settings.danmaku_enabled.0 {
                        return Err("Danmaku is disabled in this room".to_string());
                    }
                } else if !room_settings.chat_enabled.0 {
                    return Err("Chat is disabled in this room".to_string());
                }

                // Check rate limit.
                // The key includes room_id so that rate limits are per-user
                // per-room: a user spamming in one room is not throttled in
                // other rooms they belong to.
                let rate_limit_key = if is_danmaku {
                    format!(
                        "room:{}:user:{}:danmaku",
                        self.room_id.as_str(),
                        self.user_id.as_str()
                    )
                } else {
                    format!(
                        "room:{}:user:{}:chat",
                        self.room_id.as_str(),
                        self.user_id.as_str()
                    )
                };

                let rate_limit = if is_danmaku {
                    self.rate_limit_config.danmaku_per_second
                } else {
                    self.rate_limit_config.chat_per_second
                };

                self.rate_limiter
                    .check_rate_limit(
                        &rate_limit_key,
                        rate_limit,
                        self.rate_limit_config.window_seconds,
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                // Filter and sanitize content
                let sanitized_content = if is_danmaku {
                    self.content_filter
                        .filter_danmaku(&chat_msg.content)
                        .map_err(|e| e.to_string())?
                } else {
                    self.content_filter
                        .filter_chat(&chat_msg.content)
                        .map_err(|e| e.to_string())?
                };

                // Handle message
                if is_danmaku {
                    // Validate danmaku color format to prevent XSS/injection attacks
                    validate_danmaku_color(&chat_msg.color)?;

                    self.handle_danmaku(
                        &sanitized_content,
                        chat_msg.position.unwrap_or(0.0),
                        chat_msg.color.clone(),
                    )
                    .await?;
                } else {
                    self.handle_chat_message(&sanitized_content).await?;
                }
            }
            Some(Message::Heartbeat(_)) => {
                // Respond with HeartbeatAck to let client know server is alive
                // This completes the heartbeat request-response cycle
                self.send_heartbeat_ack()?;
            }
            Some(Message::WebrtcOffer(offer)) => {
                self.handle_webrtc_offer(offer).await?;
            }
            Some(Message::WebrtcAnswer(answer)) => {
                self.handle_webrtc_answer(answer).await?;
            }
            Some(Message::WebrtcIceCandidate(candidate)) => {
                self.handle_webrtc_ice_candidate(candidate).await?;
            }
            Some(Message::WebrtcJoin(join)) => {
                self.handle_webrtc_join(join).await?;
            }
            Some(Message::WebrtcLeave(leave)) => {
                self.handle_webrtc_leave(leave).await?;
            }
            Some(Message::PlaybackProgress(report)) => {
                self.handle_playback_progress(report).await?;
            }
            Some(Message::PlayCommand(_)) => {
                self.handle_play_command().await?;
            }
            Some(Message::PauseCommand(_)) => {
                self.handle_pause_command().await?;
            }
            Some(Message::SeekCommand(seek)) => {
                self.handle_seek_command(seek.current_time).await?;
            }
            Some(Message::SetSpeedCommand(speed_cmd)) => {
                self.handle_set_speed_command(speed_cmd.speed).await?;
            }
            Some(Message::SfuMigrationAnswer(_)) => {
                return Err("SFU is no longer supported".to_string());
            }
            None => {
                return Err("Empty message".to_string());
            }
        }

        Ok(())
    }

    async fn handle_chat_message(&self, content: &str) -> Result<(), String> {
        // Save to database
        let _saved_msg = self
            .room_service
            .save_chat_message(
                self.room_id.clone(),
                self.user_id.clone(),
                content.to_string(),
            )
            .await
            .map_err(|e| e.to_string())?;

        // Track chat message metric
        synctv_core::metrics::http::CHAT_MESSAGES_TOTAL
            .with_label_values(&[] as &[&str])
            .inc();

        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            message: content.to_string(),
            timestamp: chrono::Utc::now(),
            position: None,
            color: None,
        };

        // Broadcast to cluster (handles both local and Redis).
        // Chat is non-critical: log if Redis was not reached but do not fail the operation.
        let result = self.cluster_manager.broadcast(event);
        if !result.redis_sent {
            tracing::warn!(
                room_id = %self.room_id.as_str(),
                "ChatMessage cluster broadcast did not reach Redis (message may not be visible on other replicas)"
            );
            synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                .with_label_values(&["chat_no_redis"])
                .inc();
        }

        Ok(())
    }

    /// Handle danmaku (bullet comment) messages.
    ///
    /// Danmaku are intentionally ephemeral and NOT persisted to the database.
    /// Unlike regular chat messages, danmaku are time-anchored video overlays
    /// that only make sense in the context of the current playback session.
    /// They are broadcast to all connected clients for real-time display but
    /// are not saved for later retrieval. This is consistent with how major
    /// danmaku platforms (Bilibili, Niconico) treat live/real-time danmaku.
    async fn handle_danmaku(
        &self,
        content: &str,
        position: f64,
        color: Option<String>,
    ) -> Result<(), String> {
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            message: content.to_string(),
            timestamp: chrono::Utc::now(),
            position: Some(position),
            color,
        };

        // Broadcast to cluster (handles both local and Redis).
        // Danmaku is ephemeral and non-critical.
        let result = self.cluster_manager.broadcast(event);
        if !result.redis_sent {
            tracing::debug!(
                room_id = %self.room_id.as_str(),
                "Danmaku cluster broadcast did not reach Redis (ephemeral, acceptable)"
            );
        }

        Ok(())
    }

    // ==================== WebRTC Message Handlers ====================

    async fn handle_webrtc_offer(
        &self,
        offer: &crate::proto::client::WebRtcOffer,
    ) -> Result<(), String> {
        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        // Get connection ID from ConnectionManager
        let conn_id = self
            .connection_manager
            .get_connection_id(&self.room_id, &self.user_id)
            .ok_or_else(|| "Connection not found".to_string())?;

        // Issue #64: validate the target conn_id is still active so stale offers
        // are rejected early rather than silently dropped.
        // The 'to' field is formatted as "user_id:conn_id"; we parse the conn_id part.
        if let Some((_target_user, target_conn)) = offer.to.rsplit_once(':') {
            if self
                .connection_manager
                .get_connection(target_conn)
                .is_none()
            {
                tracing::warn!(
                    room_id = %self.room_id.as_str(),
                    target_conn = %target_conn,
                    "WebRTC offer target conn_id is stale (peer reconnected?), dropping offer"
                );
                return Err(
                    "Target connection is no longer active (peer may have reconnected)".to_string(),
                );
            }
        } else {
            // 'to' contains only a user_id without a conn_id qualifier.
            // In multi-device scenarios this is unreliable: the signaling message
            // will be delivered to whichever connection happens to match first.
            tracing::warn!(
                room_id = %self.room_id.as_str(),
                to = %offer.to,
                "WebRTC offer 'to' field has no conn_id qualifier; routing may be imprecise in multi-device scenarios"
            );
        }

        // P2P relay path: forward offer to target peer via cluster
        let event = ClusterEvent::WebRTCSignaling {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            message_type: "offer".to_string(),
            from: format!("{}|{}", self.user_id.as_str(), conn_id),
            to: offer.to.clone(),
            data: offer.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Broadcast to cluster. WebRTC signaling is best-effort.
        let result = self.cluster_manager.broadcast(event);
        if !result.redis_sent {
            tracing::debug!(
                room_id = %self.room_id.as_str(),
                "WebRTC offer cluster broadcast did not reach Redis (signaling may fail cross-replica)"
            );
        }

        Ok(())
    }

    async fn handle_webrtc_answer(
        &self,
        answer: &crate::proto::client::WebRtcAnswer,
    ) -> Result<(), String> {
        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        // Get connection ID
        let conn_id = self
            .connection_manager
            .get_connection_id(&self.room_id, &self.user_id)
            .ok_or_else(|| "Connection not found".to_string())?;

        // Create event with server-set 'from' field
        let event = ClusterEvent::WebRTCSignaling {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            message_type: "answer".to_string(),
            from: format!("{}|{}", self.user_id.as_str(), conn_id),
            to: answer.to.clone(),
            data: answer.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Broadcast to cluster. WebRTC signaling is best-effort.
        let result = self.cluster_manager.broadcast(event);
        if !result.redis_sent {
            tracing::debug!(
                room_id = %self.room_id.as_str(),
                "WebRTC answer cluster broadcast did not reach Redis (signaling may fail cross-replica)"
            );
        }

        Ok(())
    }

    async fn handle_webrtc_ice_candidate(
        &self,
        candidate: &crate::proto::client::WebRtcIceCandidate,
    ) -> Result<(), String> {
        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        // Get connection ID
        let conn_id = self
            .connection_manager
            .get_connection_id(&self.room_id, &self.user_id)
            .ok_or_else(|| "Connection not found".to_string())?;

        // P2P relay path: forward ICE candidate to target peer via cluster
        let event = ClusterEvent::WebRTCSignaling {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            message_type: "ice_candidate".to_string(),
            from: format!("{}|{}", self.user_id.as_str(), conn_id),
            to: candidate.to.clone(),
            data: candidate.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Broadcast to cluster. ICE candidates are best-effort signaling.
        let result = self.cluster_manager.broadcast(event);
        if !result.redis_sent {
            tracing::debug!(
                room_id = %self.room_id.as_str(),
                "ICE candidate cluster broadcast did not reach Redis (signaling may fail cross-replica)"
            );
        }

        Ok(())
    }

    async fn handle_webrtc_join(
        &self,
        _join: &crate::proto::client::WebRtcJoin,
    ) -> Result<(), String> {
        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        // Get connection ID
        let conn_id = self
            .connection_manager
            .get_connection_id(&self.room_id, &self.user_id)
            .ok_or_else(|| "Connection not found".to_string())?;

        // Mark this connection as joined WebRTC session
        self.connection_manager
            .mark_rtc_joined(&self.room_id, &self.user_id, &conn_id, true);

        // Track WebRTC peer metrics and session state for cleanup()
        // Order matters: increment metric FIRST, then set the flag.
        // This prevents race condition where cleanup() sees the flag but metric
        // hasn't been incremented yet, which would cause undercount on dec().
        synctv_core::metrics::http::WEBRTC_PEERS_ACTIVE.inc();
        self.has_webrtc_session
            .store(true, std::sync::atomic::Ordering::Release);

        // Broadcast Join event to all RTC-joined users in the room
        let event = ClusterEvent::WebRTCJoin {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            conn_id,
            username: self.username.clone(),
            timestamp: chrono::Utc::now(),
        };

        // WebRTC join is semi-critical: log at warn if not propagated to Redis.
        let result = self.cluster_manager.broadcast(event);
        if !result.redis_sent {
            tracing::warn!(
                room_id = %self.room_id.as_str(),
                user_id = %self.user_id.as_str(),
                "WebRTC join cluster broadcast did not reach Redis (peer may not be visible cross-replica)"
            );
        }

        Ok(())
    }

    async fn handle_webrtc_leave(
        &self,
        _leave: &crate::proto::client::WebRtcLeave,
    ) -> Result<(), String> {
        // Get connection ID
        let conn_id = self
            .connection_manager
            .get_connection_id(&self.room_id, &self.user_id)
            .ok_or_else(|| "Connection not found".to_string())?;

        // Mark this connection as left WebRTC session
        self.connection_manager
            .mark_rtc_joined(&self.room_id, &self.user_id, &conn_id, false);

        // Track WebRTC peer metrics and session state for cleanup()
        // Order matters: clear the flag FIRST, then decrement metric.
        // This prevents race condition where cleanup() might also try to dec()
        // after we've already decremented, which would cause undercount.
        self.has_webrtc_session
            .store(false, std::sync::atomic::Ordering::Release);
        synctv_core::metrics::http::WEBRTC_PEERS_ACTIVE.dec();

        // Broadcast Leave event to all RTC-joined users in the room
        let event = ClusterEvent::WebRTCLeave {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            conn_id,
            timestamp: chrono::Utc::now(),
        };

        // WebRTC leave is semi-critical: log at warn if not propagated to Redis.
        let result = self.cluster_manager.broadcast(event);
        if !result.redis_sent {
            tracing::warn!(
                room_id = %self.room_id.as_str(),
                user_id = %self.user_id.as_str(),
                "WebRTC leave cluster broadcast did not reach Redis (peer may remain visible cross-replica)"
            );
        }

        Ok(())
    }

    /// Handle playback progress report from client.
    ///
    /// Clients send periodic progress heartbeats so the server knows each
    /// client's actual playback position. The server updates the canonical
    /// playback state, which:
    /// - Gives new joiners an accurate starting position (solves drift for late joiners)
    /// - Enables server-side drift detection across clients
    ///
    /// Rate limited by design: the heartbeat interval on the client side
    /// (typically 3-5 seconds) is the throttle. The server accepts the report
    /// and performs a lightweight state update.
    ///
    /// Drift bounds: rejects reports where the reported position deviates
    /// more than 30 seconds from the expected server-side position (computed
    /// from last known time + wall-clock elapsed). This prevents clients from
    /// spoofing arbitrary playback positions.
    async fn handle_playback_progress(
        &self,
        report: &crate::proto::client::PlaybackProgressReport,
    ) -> Result<(), String> {
        if report.current_time < 0.0 {
            return Err("Playback position must be non-negative".to_string());
        }

        // Only members with SEEK permission may update the canonical playback
        // state via progress reports. Without this check any room member could
        // silently rewrite the server-side position by sending crafted progress
        // messages, effectively acting as an unauthorized seek.
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::SEEK)
            .await
            .map_err(|e| e.to_string())?;

        let playback_service = self.room_service.playback_service();
        let state = playback_service
            .get_state(&self.room_id)
            .await
            .map_err(|e| e.to_string())?;

        // Only accept progress reports when playback is active and the
        // reported state matches the server's playing state
        if state.is_playing && report.is_playing {
            // Drift bounds check: compute expected position from last known
            // state + elapsed wall-clock time, reject if too far off.
            let elapsed_secs = chrono::Utc::now()
                .signed_duration_since(state.updated_at)
                .num_milliseconds() as f64
                / 1000.0;
            let expected_position = state.current_time + (elapsed_secs * state.speed);
            let drift = (report.current_time - expected_position).abs();

            const MAX_DRIFT_SECONDS: f64 = 30.0;
            if drift > MAX_DRIFT_SECONDS {
                tracing::warn!(
                    user_id = %self.user_id.as_str(),
                    room_id = %self.room_id.as_str(),
                    reported = report.current_time,
                    expected = expected_position,
                    drift = drift,
                    "Playback progress report rejected: drift exceeds {} seconds",
                    MAX_DRIFT_SECONDS
                );
                return Err(format!(
                    "Playback progress drift too large ({drift:.1}s > {MAX_DRIFT_SECONDS}s)"
                ));
            }

            // Update the canonical position and broadcast to same-replica
            // clients so they can detect drift. The sender is excluded by
            // event_id filtering (each connection ignores events it originated).
            match playback_service
                .update_state(self.room_id.clone(), |s| {
                    s.current_time = report.current_time;
                    s.updated_at = chrono::Utc::now();
                })
                .await
            {
                Ok(updated_state) => {
                    // Local-only broadcast (no Redis) -- progress reports are
                    // high-frequency and only relevant to same-replica clients.
                    let event = synctv_cluster::sync::ClusterEvent::PlaybackStateChanged {
                        event_id: nanoid::nanoid!(16),
                        room_id: self.room_id.clone(),
                        user_id: self.user_id.clone(),
                        username: self.username.clone(),
                        state: updated_state,
                        timestamp: chrono::Utc::now(),
                    };
                    self.cluster_manager
                        .message_hub()
                        .broadcast(&self.room_id, event);
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        room_id = %self.room_id.as_str(),
                        "Failed to update playback state from progress report (non-critical)"
                    );
                }
            }
        }

        Ok(())
    }

    /// Handle Play command from WebSocket
    async fn handle_play_command(&self) -> Result<(), String> {
        // Permission check (PLAY_PAUSE) is handled by PlaybackService::set_playing()
        self.room_service
            .playback_service()
            .set_playing(self.room_id.clone(), self.user_id.clone(), true)
            .await
            .map_err(|e| e.to_string())?;

        // PlaybackStateChanged broadcast is handled by room_service
        Ok(())
    }

    /// Handle Pause command from WebSocket
    async fn handle_pause_command(&self) -> Result<(), String> {
        // Permission check (PLAY_PAUSE) is handled by PlaybackService::set_playing()
        self.room_service
            .playback_service()
            .set_playing(self.room_id.clone(), self.user_id.clone(), false)
            .await
            .map_err(|e| e.to_string())?;

        // PlaybackStateChanged broadcast is handled by room_service
        Ok(())
    }

    /// Handle Seek command from WebSocket
    async fn handle_seek_command(&self, current_time: f64) -> Result<(), String> {
        if current_time < 0.0 {
            return Err("Seek position must be non-negative".to_string());
        }

        // Permission check (SEEK) is handled by PlaybackService::seek()
        self.room_service
            .playback_service()
            .seek(self.room_id.clone(), self.user_id.clone(), current_time)
            .await
            .map_err(|e| e.to_string())?;

        // PlaybackStateChanged broadcast is handled by room_service
        Ok(())
    }

    /// Handle `SetPlaybackSpeed` command from WebSocket
    async fn handle_set_speed_command(&self, speed: f64) -> Result<(), String> {
        // R-1: No WS-layer speed validation; PlaybackService::change_speed() is
        // the single authority for speed range enforcement.

        // Permission check (CHANGE_SPEED) is handled by PlaybackService::change_speed()
        self.room_service
            .playback_service()
            .change_speed(self.room_id.clone(), self.user_id.clone(), speed)
            .await
            .map_err(|e| e.to_string())?;

        // PlaybackStateChanged broadcast is handled by room_service
        Ok(())
    }

    /// Send heartbeat acknowledgment to client
    fn send_heartbeat_ack(&self) -> Result<(), String> {
        use crate::proto::client::server_message::Message;
        use crate::proto::client::HeartbeatAck;

        let msg = ServerMessage {
            message: Some(Message::HeartbeatAck(HeartbeatAck {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };

        self.sender.send(msg)
    }

    /// Get room ID
    #[must_use]
    pub const fn get_room_id(&self) -> &RoomId {
        &self.room_id
    }

    /// Get user ID
    #[must_use]
    pub fn get_user_id(&self) -> UserId {
        self.user_id.clone()
    }
}

/// Convert cluster event to server message
fn cluster_event_to_server_message(
    event: &synctv_cluster::sync::ClusterEvent,
    room_id: &str,
) -> Option<ServerMessage> {
    use crate::proto::client::server_message::Message;
    use crate::proto::client::{
        ChatMessageReceive, ErrorMessage, PlaybackState, PlaybackStateChanged, RoomSettingsChanged,
        ServerMessage, UserJoinedRoom, UserLeftRoom,
    };
    use synctv_cluster::sync::ClusterEvent;
    use synctv_proto::common::RoomMember;

    match event {
        ClusterEvent::ChatMessage {
            user_id,
            username,
            message,
            timestamp,
            position,
            color,
            ..
        } => Some(ServerMessage {
            message: Some(Message::Chat(ChatMessageReceive {
                id: nanoid::nanoid!(12),
                room_id: room_id.to_string(),
                user_id: user_id.as_str().to_string(),
                username: username.clone(),
                content: message.clone(),
                timestamp: timestamp.timestamp_micros(),
                position: *position,
                color: color.clone(),
            })),
        }),
        ClusterEvent::PlaybackStateChanged { state, .. } => Some(ServerMessage {
            message: Some(Message::PlaybackState(PlaybackStateChanged {
                room_id: room_id.to_string(),
                state: Some(PlaybackState {
                    room_id: state.room_id.as_str().to_string(),
                    playing_media_id: state
                        .playing_media_id
                        .as_ref()
                        .map(|id| id.as_str().to_string())
                        .unwrap_or_default(),
                    current_time: state.current_time,
                    speed: state.speed,
                    is_playing: state.is_playing,
                    updated_at: state.updated_at.timestamp(),
                    version: state.version as i32,
                    playing_playlist_id: state
                        .playing_playlist_id
                        .as_ref()
                        .map(|id| id.as_str().to_string())
                        .unwrap_or_default(),
                    relative_path: state.relative_path.clone(),
                }),
            })),
        }),
        ClusterEvent::UserJoined {
            user_id,
            username,
            permissions,
            role,
            ..
        } => Some(ServerMessage {
            message: Some(Message::UserJoined(UserJoinedRoom {
                room_id: room_id.to_string(),
                member: Some(RoomMember {
                    room_id: room_id.to_string(),
                    user_id: user_id.as_str().to_string(),
                    username: username.clone(),
                    role: *role,
                    permissions: permissions.0,
                    added_permissions: 0,
                    removed_permissions: 0,
                    admin_added_permissions: 0,
                    admin_removed_permissions: 0,
                    joined_at: chrono::Utc::now().timestamp(),
                    is_online: true,
                }),
            })),
        }),
        ClusterEvent::UserLeft { user_id, .. } => Some(ServerMessage {
            message: Some(Message::UserLeft(UserLeftRoom {
                room_id: room_id.to_string(),
                user_id: user_id.as_str().to_string(),
            })),
        }),
        ClusterEvent::MediaAdded {
            media_id,
            media_title,
            user_id,
            username,
            ..
        } => Some(ServerMessage {
            message: Some(Message::MediaAdded(crate::proto::client::MediaAdded {
                room_id: room_id.to_string(),
                media_id: media_id.as_str().to_string(),
                title: media_title.clone(),
                added_by: username.clone(),
                added_by_user_id: user_id.as_str().to_string(),
            })),
        }),
        ClusterEvent::MediaRemoved {
            media_id,
            user_id,
            username,
            ..
        } => Some(ServerMessage {
            message: Some(Message::MediaRemoved(crate::proto::client::MediaRemoved {
                room_id: room_id.to_string(),
                media_id: media_id.as_str().to_string(),
                removed_by: username.clone(),
                removed_by_user_id: user_id.as_str().to_string(),
            })),
        }),
        ClusterEvent::PermissionChanged {
            target_user_id,
            new_permissions,
            role,
            added_permissions,
            removed_permissions,
            changed_by_username,
            ..
        } => Some(ServerMessage {
            message: Some(Message::PermissionChanged(
                crate::proto::client::PermissionChanged {
                    room_id: room_id.to_string(),
                    user_id: target_user_id.as_str().to_string(),
                    role: *role,
                    effective_permissions: new_permissions.0,
                    added_permissions: added_permissions.0,
                    removed_permissions: removed_permissions.0,
                    admin_added_permissions: 0,
                    admin_removed_permissions: 0,
                    updated_by: changed_by_username.clone(),
                },
            )),
        }),
        ClusterEvent::RoomSettingsChanged { settings_json, .. } => Some(ServerMessage {
            message: Some(Message::RoomSettings(RoomSettingsChanged {
                room_id: room_id.to_string(),
                settings: settings_json.clone(),
            })),
        }),
        ClusterEvent::WebRTCSignaling {
            message_type,
            from,
            to,
            data,
            ..
        } => {
            // Convert to appropriate proto message based on message_type
            match message_type.as_str() {
                "offer" => Some(ServerMessage {
                    message: Some(Message::WebrtcOffer(crate::proto::client::WebRtcOffer {
                        from: from.clone(),
                        to: to.clone(),
                        data: data.clone(),
                    })),
                }),
                "answer" => Some(ServerMessage {
                    message: Some(Message::WebrtcAnswer(crate::proto::client::WebRtcAnswer {
                        from: from.clone(),
                        to: to.clone(),
                        data: data.clone(),
                    })),
                }),
                "ice_candidate" => Some(ServerMessage {
                    message: Some(Message::WebrtcIceCandidate(
                        crate::proto::client::WebRtcIceCandidate {
                            from: from.clone(),
                            to: to.clone(),
                            data: data.clone(),
                        },
                    )),
                }),
                "sfu_migration_offer" => {
                    // Parse the migration offer data (contains migration_id + sdp)
                    match serde_json::from_str::<serde_json::Value>(data) {
                        Ok(parsed) => {
                            let migration_id =
                                parsed["migration_id"].as_str().unwrap_or("").to_string();
                            let sdp = parsed["sdp"].as_str().unwrap_or("").to_string();
                            Some(ServerMessage {
                                message: Some(Message::SfuMigrationOffer(
                                    crate::proto::client::SfuMigrationOffer {
                                        migration_id,
                                        data: sdp,
                                    },
                                )),
                            })
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse SFU migration offer data: {}", e);
                            None
                        }
                    }
                }
                "sfu_migration_status" => {
                    // Parse the migration status data
                    match serde_json::from_str::<serde_json::Value>(data) {
                        Ok(parsed) => {
                            let migration_id =
                                parsed["migration_id"].as_str().unwrap_or("").to_string();
                            let state = parsed["state"].as_i64().unwrap_or(0) as i32;
                            let total_peers = parsed["total_peers"].as_i64().unwrap_or(0) as i32;
                            let completed_peers =
                                parsed["completed_peers"].as_i64().unwrap_or(0) as i32;
                            let failed_peers = parsed["failed_peers"].as_i64().unwrap_or(0) as i32;
                            Some(ServerMessage {
                                message: Some(Message::SfuMigrationStatus(
                                    crate::proto::client::SfuMigrationStatus {
                                        migration_id,
                                        state,
                                        total_peers,
                                        completed_peers,
                                        failed_peers,
                                    },
                                )),
                            })
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse SFU migration status data: {}", e);
                            None
                        }
                    }
                }
                _ => {
                    tracing::warn!("Unknown WebRTC message type: {}", message_type);
                    None
                }
            }
        }
        ClusterEvent::WebRTCJoin {
            user_id,
            conn_id,
            username,
            ..
        } => Some(ServerMessage {
            message: Some(Message::WebrtcJoin(crate::proto::client::WebRtcJoin {
                user_id: user_id.as_str().to_string(),
                conn_id: conn_id.clone(),
                username: username.clone(),
            })),
        }),
        ClusterEvent::WebRTCLeave {
            user_id, conn_id, ..
        } => Some(ServerMessage {
            message: Some(Message::WebrtcLeave(crate::proto::client::WebRtcLeave {
                user_id: user_id.as_str().to_string(),
                conn_id: conn_id.clone(),
            })),
        }),
        ClusterEvent::SystemNotification { message, .. } => {
            Some(ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: message.clone(),
                    code: 0, // System notifications use code 0 (not actual errors)
                    detail: String::new(),
                })),
            })
        }
        ClusterEvent::RoomDeleted { .. } => {
            // Notify WebSocket clients that the room has been deleted
            Some(ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room has been deleted".to_string(),
                    code: crate::impls::error_codes::NOT_FOUND,
                    detail: String::new(),
                })),
            })
        }
        ClusterEvent::KickPublisher { .. }
        | ClusterEvent::KickUser { .. }
        | ClusterEvent::KickUserFromRoom { .. }
        | ClusterEvent::RoomCreated { .. }
        | ClusterEvent::CacheInvalidate { .. }
        | ClusterEvent::UserNotification { .. } => {
            // Admin/internal events are handled by other channels,
            // not forwarded to WebSocket clients via the room event path
            None
        }
    }
}

/// Validate danmaku color format.
///
/// Only accepts hex color format: `#RRGGBB` (6 hex digits with # prefix).
/// Returns `Ok(())` if the color is valid or `None` (default color).
/// Returns `Err` with a descriptive message if the color format is invalid.
///
/// # Security
///
/// This validation prevents XSS attacks by rejecting any non-hex characters
/// and enforcing strict format requirements. The color value is typically
/// rendered in CSS/HTML contexts where injection attacks could be dangerous.
///
/// # Examples
///
/// ```
/// # use synctv_api::impls::messaging::validate_danmaku_color;
/// assert!(validate_danmaku_color(&Some("#FF0000".to_string())).is_ok()); // Red
/// assert!(validate_danmaku_color(&Some("#abcdef".to_string())).is_ok()); // Lowercase
/// assert!(validate_danmaku_color(&None).is_ok()); // No color = default
/// assert!(validate_danmaku_color(&Some("red".to_string())).is_err()); // Invalid format
/// assert!(validate_danmaku_color(&Some("javascript:alert(1)".to_string())).is_err()); // XSS
/// ```
pub fn validate_danmaku_color(color: &Option<String>) -> Result<(), String> {
    let Some(color_str) = color else {
        // None is valid - means default color
        return Ok(());
    };

    // Must start with #
    if !color_str.starts_with('#') {
        return Err(format!(
            "Invalid danmaku color: must start with '#', got: {color_str}"
        ));
    }

    // Must be exactly 7 characters (# + 6 hex digits)
    if color_str.len() != 7 {
        return Err(format!(
            "Invalid danmaku color: must be 7 characters (#RRGGBB), got {} characters: {color_str}",
            color_str.len()
        ));
    }

    // All characters after # must be valid hex digits
    let hex_part = &color_str[1..];
    if !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "Invalid danmaku color: must contain only hex characters (0-9, a-f, A-F), got: {color_str}"
        ));
    }

    Ok(())
}

/// Binary codec for proto messages
pub struct ProtoCodec;

impl ProtoCodec {
    /// Encode `ClientMessage` to binary
    pub fn encode_client_message(msg: &ClientMessage) -> Result<Vec<u8>, String> {
        Ok(msg.encode_to_vec())
    }

    /// Decode `ClientMessage` from binary
    pub fn decode_client_message(data: &[u8]) -> Result<ClientMessage, String> {
        ClientMessage::decode(data).map_err(|e| format!("Failed to decode message: {e}"))
    }

    /// Encode `ServerMessage` to binary
    pub fn encode_server_message(msg: &ServerMessage) -> Result<Vec<u8>, String> {
        Ok(msg.encode_to_vec())
    }

    /// Decode `ServerMessage` from binary
    pub fn decode_server_message(data: &[u8]) -> Result<ServerMessage, String> {
        ServerMessage::decode(data).map_err(|e| format!("Failed to decode message: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::client::server_message::Message;
    use synctv_cluster::sync::{ClusterEvent, NotificationLevel};
    use synctv_core::models::{MediaId, PermissionBits, RoomId, RoomPlaybackState, UserId};

    fn room_id() -> RoomId {
        RoomId("room_test".to_string())
    }
    fn user_id() -> UserId {
        UserId("user_test".to_string())
    }
    fn media_id() -> MediaId {
        MediaId::from_string("media_test".to_string())
    }
    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    // ========== cluster_event_to_server_message Tests ==========

    #[test]
    fn test_chat_message_event_conversion() {
        let event = ClusterEvent::ChatMessage {
            event_id: "evt1".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "alice".to_string(),
            message: "hello world".to_string(),
            timestamp: now(),
            position: Some(42.5),
            color: Some("#ff0000".to_string()),
        };

        let msg = cluster_event_to_server_message(&event, "room_test");
        assert!(msg.is_some());
        let msg = msg.unwrap();
        match msg.message {
            Some(Message::Chat(chat)) => {
                assert_eq!(chat.room_id, "room_test");
                assert_eq!(chat.user_id, "user_test");
                assert_eq!(chat.username, "alice");
                assert_eq!(chat.content, "hello world");
                assert_eq!(chat.position, Some(42.5));
                assert_eq!(chat.color, Some("#ff0000".to_string()));
            }
            other => panic!("Expected Chat message, got: {other:?}"),
        }
    }

    #[test]
    fn test_playback_state_changed_event_conversion() {
        let state = RoomPlaybackState {
            room_id: room_id(),
            playing_media_id: Some(media_id()),
            playing_playlist_id: None,
            relative_path: String::new(),
            current_time: 123.456,
            speed: 1.5,
            is_playing: true,
            updated_at: now(),
            version: 7,
        };
        let event = ClusterEvent::PlaybackStateChanged {
            event_id: "evt2".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "bob".to_string(),
            state: state.clone(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::PlaybackState(ps)) => {
                assert_eq!(ps.room_id, "room_test");
                let s = ps.state.unwrap();
                assert_eq!(s.current_time, 123.456);
                assert_eq!(s.speed, 1.5);
                assert!(s.is_playing);
                assert_eq!(s.playing_media_id, "media_test");
                assert_eq!(s.version, 7);
            }
            other => panic!("Expected PlaybackState, got: {other:?}"),
        }
    }

    #[test]
    fn test_user_joined_event_conversion() {
        let event = ClusterEvent::UserJoined {
            event_id: "evt3".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "carol".to_string(),
            permissions: PermissionBits(PermissionBits::DEFAULT_MEMBER),
            role: 3,
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::UserJoined(uj)) => {
                assert_eq!(uj.room_id, "room_test");
                let member = uj.member.unwrap();
                assert_eq!(member.user_id, "user_test");
                assert_eq!(member.username, "carol");
                assert_eq!(member.role, 3);
                assert!(member.is_online);
            }
            other => panic!("Expected UserJoined, got: {other:?}"),
        }
    }

    #[test]
    fn test_user_left_event_conversion() {
        let event = ClusterEvent::UserLeft {
            event_id: "evt4".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "dave".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::UserLeft(ul)) => {
                assert_eq!(ul.room_id, "room_test");
                assert_eq!(ul.user_id, "user_test");
            }
            other => panic!("Expected UserLeft, got: {other:?}"),
        }
    }

    #[test]
    fn test_media_added_event_conversion() {
        let event = ClusterEvent::MediaAdded {
            event_id: "evt5".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "eve".to_string(),
            media_id: media_id(),
            media_title: "Test Video".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::MediaAdded(ma)) => {
                assert_eq!(ma.room_id, "room_test");
                assert_eq!(ma.media_id, "media_test");
                assert_eq!(ma.title, "Test Video");
                assert_eq!(ma.added_by, "eve");
            }
            other => panic!("Expected MediaAdded, got: {other:?}"),
        }
    }

    #[test]
    fn test_media_removed_event_conversion() {
        let event = ClusterEvent::MediaRemoved {
            event_id: "evt6".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "frank".to_string(),
            media_id: media_id(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::MediaRemoved(mr)) => {
                assert_eq!(mr.room_id, "room_test");
                assert_eq!(mr.media_id, "media_test");
                assert_eq!(mr.removed_by, "frank");
            }
            other => panic!("Expected MediaRemoved, got: {other:?}"),
        }
    }

    #[test]
    fn test_webrtc_offer_event_conversion() {
        let event = ClusterEvent::WebRTCSignaling {
            event_id: "evt7".to_string(),
            room_id: room_id(),
            message_type: "offer".to_string(),
            from: "conn_a".to_string(),
            to: "conn_b".to_string(),
            data: "sdp_data".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::WebrtcOffer(o)) => {
                assert_eq!(o.from, "conn_a");
                assert_eq!(o.to, "conn_b");
                assert_eq!(o.data, "sdp_data");
            }
            other => panic!("Expected WebrtcOffer, got: {other:?}"),
        }
    }

    #[test]
    fn test_webrtc_answer_event_conversion() {
        let event = ClusterEvent::WebRTCSignaling {
            event_id: "evt8".to_string(),
            room_id: room_id(),
            message_type: "answer".to_string(),
            from: "conn_b".to_string(),
            to: "conn_a".to_string(),
            data: "answer_sdp".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::WebrtcAnswer(a)) => {
                assert_eq!(a.from, "conn_b");
                assert_eq!(a.to, "conn_a");
            }
            other => panic!("Expected WebrtcAnswer, got: {other:?}"),
        }
    }

    #[test]
    fn test_webrtc_ice_candidate_event_conversion() {
        let event = ClusterEvent::WebRTCSignaling {
            event_id: "evt9".to_string(),
            room_id: room_id(),
            message_type: "ice_candidate".to_string(),
            from: "conn_a".to_string(),
            to: "conn_b".to_string(),
            data: "candidate_data".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        assert!(matches!(msg.message, Some(Message::WebrtcIceCandidate(_))));
    }

    #[test]
    fn test_webrtc_unknown_type_returns_none() {
        let event = ClusterEvent::WebRTCSignaling {
            event_id: "evt10".to_string(),
            room_id: room_id(),
            message_type: "unknown_type".to_string(),
            from: "conn_a".to_string(),
            to: "conn_b".to_string(),
            data: "data".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test");
        assert!(msg.is_none());
    }

    #[test]
    fn test_room_deleted_event_conversion() {
        let event = ClusterEvent::RoomDeleted {
            event_id: "evt11".to_string(),
            room_id: room_id(),
            deleted_by: user_id(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::Error(e)) => {
                assert!(e.message.contains("deleted"));
                assert_eq!(e.code, crate::impls::error_codes::NOT_FOUND);
            }
            other => panic!("Expected Error message for RoomDeleted, got: {other:?}"),
        }
    }

    #[test]
    fn test_system_notification_event_conversion() {
        let event = ClusterEvent::SystemNotification {
            event_id: "evt12".to_string(),
            message: "Server maintenance in 5 minutes".to_string(),
            level: NotificationLevel::Warning,
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::Error(e)) => {
                assert_eq!(e.message, "Server maintenance in 5 minutes");
                assert_eq!(e.code, 0);
            }
            other => panic!("Expected Error message for SystemNotification, got: {other:?}"),
        }
    }

    #[test]
    fn test_admin_events_return_none() {
        let event = ClusterEvent::KickPublisher {
            event_id: "evt13".to_string(),
            room_id: room_id(),
            media_id: media_id(),
            reason: "test".to_string(),
            timestamp: now(),
        };
        assert!(cluster_event_to_server_message(&event, "room_test").is_none());

        let event = ClusterEvent::KickUser {
            event_id: "evt14".to_string(),
            user_id: user_id(),
            reason: "banned".to_string(),
            timestamp: now(),
        };
        assert!(cluster_event_to_server_message(&event, "room_test").is_none());
    }

    // ========== ProtoCodec Tests ==========

    #[test]
    fn test_server_message_encode_decode_roundtrip() {
        let msg = ServerMessage {
            message: Some(Message::UserLeft(crate::proto::client::UserLeftRoom {
                room_id: "room1".to_string(),
                user_id: "user1".to_string(),
            })),
        };

        let encoded = ProtoCodec::encode_server_message(&msg).unwrap();
        let decoded = ProtoCodec::decode_server_message(&encoded).unwrap();
        match decoded.message {
            Some(Message::UserLeft(ul)) => {
                assert_eq!(ul.room_id, "room1");
                assert_eq!(ul.user_id, "user1");
            }
            other => panic!("Expected UserLeft after roundtrip, got: {other:?}"),
        }
    }

    #[test]
    fn test_client_message_decode_invalid_data() {
        let result = ProtoCodec::decode_client_message(&[0xFF, 0xFF, 0xFF]);
        assert!(result.is_err());
    }

    #[test]
    fn test_server_message_decode_invalid_data() {
        let result = ProtoCodec::decode_server_message(&[0xFF, 0xFF, 0xFF]);
        assert!(result.is_err());
    }

    #[test]
    fn test_server_message_encode_empty() {
        let msg = ServerMessage { message: None };
        let encoded = ProtoCodec::encode_server_message(&msg).unwrap();
        let decoded = ProtoCodec::decode_server_message(&encoded).unwrap();
        assert!(decoded.message.is_none());
    }

    // ========== Backpressure Control Tests ==========

    #[test]
    fn test_message_concurrency_config_can_be_acquired() {
        // Test that the semaphore can be acquired under normal conditions
        let config = super::MessageConcurrencyConfig::new(100);
        let semaphore = config.semaphore();
        // Use try_acquire to check without blocking
        let permit = semaphore.try_acquire();
        assert!(
            permit.is_ok(),
            "Semaphore should be acquirable under normal load"
        );
        // Release the permit immediately
        drop(permit);
    }

    #[test]
    fn test_message_concurrency_config_enforces_limit() {
        // Test that semaphore enforces the concurrent processing limit.
        // Each test gets its own config instance, so no cross-test interference.
        let config = super::MessageConcurrencyConfig::new(10);
        let semaphore = config.semaphore();

        // Acquire all 10 permits
        let permits: Vec<_> = (0..10)
            .map(|_| semaphore.clone().try_acquire_owned())
            .collect::<Result<Vec<_>, _>>()
            .expect("Should acquire all 10 permits");

        assert_eq!(config.available_permits(), 0, "No permits should remain");

        // Next acquisition should fail
        let failed = semaphore.clone().try_acquire_owned();
        assert!(failed.is_err(), "Should fail when no permits available");

        // Drop all permits
        drop(permits);
        assert_eq!(config.available_permits(), 10, "All permits restored");
    }

    #[test]
    fn test_resource_exhausted_error_code() {
        // Test that RESOURCE_EXHAUSTED error code is properly defined
        assert_eq!(
            crate::impls::error_codes::RESOURCE_EXHAUSTED,
            2002,
            "RESOURCE_EXHAUSTED should be error code 2002"
        );
    }

    #[test]
    fn test_resource_exhausted_error_message_format() {
        // Test that ResourceExhausted error messages are properly formatted
        let error_msg = ServerMessage {
            message: Some(Message::Error(crate::proto::client::ErrorMessage {
                message: "System overloaded, please retry later".to_string(),
                code: crate::impls::error_codes::RESOURCE_EXHAUSTED,
                detail: String::new(),
            })),
        };

        match error_msg.message {
            Some(Message::Error(e)) => {
                assert_eq!(e.code, crate::impls::error_codes::RESOURCE_EXHAUSTED);
                assert!(!e.message.is_empty());
            }
            other => panic!("Expected Error message, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_concurrency_config_backpressure_with_async() {
        // Test that semaphore backpressure works correctly with async operations.
        // Each test gets its own config instance, so no cross-test interference.
        let config = std::sync::Arc::new(super::MessageConcurrencyConfig::new(50));
        let semaphore = config.semaphore();

        // Acquire a permit for message processing
        let permit = semaphore.try_acquire_owned();
        assert!(permit.is_ok(), "Should be able to acquire permit");
        let after_acquire = config.available_permits();

        // Drop the permit (simulating message processing completion)
        drop(permit);

        // Verify permits are restored
        let after_release = config.available_permits();
        assert!(
            after_release > after_acquire,
            "Available permits should increase after releasing: was {after_acquire}, now {after_release}"
        );
    }

    #[test]
    fn test_concurrency_config_default_matches_constant() {
        // Verify default config uses the correct constant
        let config = super::MessageConcurrencyConfig::default();
        assert_eq!(
            config.max_concurrent(),
            super::DEFAULT_MAX_CONCURRENT_MESSAGE_PROCESSING,
            "Default should match DEFAULT_MAX_CONCURRENT_MESSAGE_PROCESSING"
        );
    }

    // ========== Danmaku Color Validation Tests ==========

    #[test]
    fn test_validate_danmaku_color_valid_hex_colors() {
        // Valid hex color formats: #RRGGBB
        assert!(super::validate_danmaku_color(&Some("#FF0000".to_string())).is_ok()); // Red
        assert!(super::validate_danmaku_color(&Some("#00FF00".to_string())).is_ok()); // Green
        assert!(super::validate_danmaku_color(&Some("#0000FF".to_string())).is_ok()); // Blue
        assert!(super::validate_danmaku_color(&Some("#FFFFFF".to_string())).is_ok()); // White
        assert!(super::validate_danmaku_color(&Some("#000000".to_string())).is_ok()); // Black
        assert!(super::validate_danmaku_color(&Some("#abcdef".to_string())).is_ok()); // Lowercase
        assert!(super::validate_danmaku_color(&Some("#ABCDEF".to_string())).is_ok()); // Uppercase
        assert!(super::validate_danmaku_color(&Some("#123456".to_string())).is_ok()); // Mixed digits
        assert!(super::validate_danmaku_color(&Some("#1a2B3c".to_string())).is_ok()); // Mixed case
    }

    #[test]
    fn test_validate_danmaku_color_none_is_valid() {
        // None should be valid (no color specified = default color)
        assert!(super::validate_danmaku_color(&None).is_ok());
    }

    #[test]
    fn test_validate_danmaku_color_invalid_format_no_hash() {
        // Missing # prefix should be rejected
        let result = super::validate_danmaku_color(&Some("FF0000".to_string()));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must start with '#'"));
    }

    #[test]
    fn test_validate_danmaku_color_invalid_format_wrong_length() {
        // Wrong length should be rejected
        let result = super::validate_danmaku_color(&Some("#FFF".to_string())); // Too short
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be 7 characters"));

        let result = super::validate_danmaku_color(&Some("#FFFFFFFF".to_string())); // Too long (no alpha)
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be 7 characters"));
    }

    #[test]
    fn test_validate_danmaku_color_invalid_characters() {
        // Non-hex characters should be rejected
        let result = super::validate_danmaku_color(&Some("#GGGGGG".to_string()));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must contain only hex characters"));

        let result = super::validate_danmaku_color(&Some("#ZZZZZZ".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_danmaku_color_xss_injection() {
        // XSS injection attempts should be rejected
        let result = super::validate_danmaku_color(&Some("javascript:alert(1)".to_string()));
        assert!(result.is_err());

        let result = super::validate_danmaku_color(&Some("<script>".to_string()));
        assert!(result.is_err());

        let result = super::validate_danmaku_color(&Some("rgb(255,0,0)".to_string()));
        assert!(result.is_err());

        let result = super::validate_danmaku_color(&Some("red".to_string()));
        assert!(result.is_err());

        let result = super::validate_danmaku_color(&Some("#expression(alert(1))".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_danmaku_color_empty_string() {
        // Empty string should be rejected
        let result = super::validate_danmaku_color(&Some("".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_danmaku_color_special_characters() {
        // Special characters should be rejected
        let result = super::validate_danmaku_color(&Some("#FF 000".to_string())); // Space
        assert!(result.is_err());

        let result = super::validate_danmaku_color(&Some("#FF-000".to_string())); // Dash
        assert!(result.is_err());

        let result = super::validate_danmaku_color(&Some("#FF\n000".to_string())); // Newline
        assert!(result.is_err());

        let result = super::validate_danmaku_color(&Some("#\u{0000}F0000".to_string())); // Null byte
        assert!(result.is_err());
    }
}
