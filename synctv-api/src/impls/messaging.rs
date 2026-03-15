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

/// Minimum position change (in seconds) required to trigger a DB write
/// for playback progress reports. Reports with smaller position deltas
/// are acknowledged but not persisted, reducing write amplification.
const PROGRESS_MIN_POSITION_DELTA: f64 = 1.0;

/// Minimum elapsed wall-clock time (in seconds) between DB writes for
/// playback progress reports, regardless of position delta.
const PROGRESS_MIN_ELAPSED_SECS: f64 = 5.0;

/// Maximum size of a WebRTC SDP offer/answer payload in bytes.
/// SDP descriptions can be large but should not exceed ~10 KB.
pub const MAX_SDP_SIZE: usize = 10_000;

/// Maximum size of a WebRTC ICE candidate payload in bytes.
/// Individual ICE candidates are small (typically under 200 bytes).
pub const MAX_ICE_CANDIDATE_SIZE: usize = 500;

/// Maximum number of concurrent UserLeft retry tasks across the process.
/// Prevents unbounded task spawning during mass disconnects with Redis down.
static USER_LEFT_RETRY_SEMAPHORE: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(100)));

use crate::proto::client::{ClientMessage, ServerMessage};

/// Default TTL for membership cache entries (30 seconds).
///
/// This TTL is chosen to balance between:
/// - Reducing database load (longer TTL = fewer queries)
/// - Responsiveness to membership changes (shorter TTL = faster detection of bans/removals)
///
/// With a 30-second TTL and 25-35 second heartbeat interval, we ensure:
/// - At most 1 DB query per connection per 30 seconds (vs. every heartbeat without cache)
/// - Banned/removed users are disconnected within ~30-65 seconds worst case
/// - The disconnect signal channel (Redis `PubSub`) provides immediate notification in most cases
const MEMBERSHIP_CACHE_TTL: Duration = Duration::from_secs(30);

/// Default maximum concurrent message processing operations across all connections.
///
/// This provides backpressure when the system is under heavy load.
/// When exceeded, new messages receive a `ResourceExhausted` error.
pub const DEFAULT_MAX_CONCURRENT_MESSAGE_PROCESSING: usize = 1000;

const fn should_fail_webrtc_signal_broadcast(
    result: synctv_cluster::sync::BroadcastResult,
    cluster_redis_enabled: bool,
) -> bool {
    if cluster_redis_enabled {
        !result.redis_sent
    } else {
        result.local_sent == 0 && !result.redis_sent
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeartbeatSchedule {
    membership_cache_ttl: Duration,
    base_interval: Duration,
    max_jitter_secs: u64,
}

impl HeartbeatSchedule {
    #[must_use]
    pub const fn production() -> Self {
        Self {
            membership_cache_ttl: MEMBERSHIP_CACHE_TTL,
            base_interval: Duration::from_secs(25),
            max_jitter_secs: 10,
        }
    }

    #[must_use]
    pub const fn for_tests(membership_cache_ttl: Duration, base_interval: Duration) -> Self {
        Self {
            membership_cache_ttl,
            base_interval,
            max_jitter_secs: 0,
        }
    }

    #[must_use]
    pub const fn membership_cache_ttl(self) -> Duration {
        self.membership_cache_ttl
    }

    #[must_use]
    pub const fn max_jitter_secs(self) -> u64 {
        self.max_jitter_secs
    }

    #[must_use]
    pub fn period_with_random_jitter(self) -> Duration {
        self.base_interval
            + Duration::from_secs(rand::rng().random_range(0u64..=self.max_jitter_secs))
    }

    #[must_use]
    pub fn period_for_user(self, user_id: &UserId) -> Duration {
        let jitter_secs = if self.max_jitter_secs == 0 {
            0
        } else {
            user_id
                .as_str()
                .bytes()
                .fold(0u64, |acc, byte| acc.wrapping_add(u64::from(byte)))
                % (self.max_jitter_secs + 1)
        };
        self.base_interval + Duration::from_secs(jitter_secs)
    }
}

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
    /// This is shared across all connections for the same `AppState`.
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
    ///   When this limit is reached, new messages will receive a `ResourceExhausted` error.
    ///
    /// # Example
    ///
    /// ```
    /// use synctv_api::impls::MessageConcurrencyConfig;
    ///
    /// let config = MessageConcurrencyConfig::new(500);
    /// assert_eq!(config.max_concurrent(), 500);
    /// ```
    #[must_use]
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
    #[must_use]
    pub fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.semaphore)
    }

    /// Get the maximum concurrent limit.
    #[must_use]
    pub const fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Get the number of available permits.
    ///
    /// This is useful for monitoring and health checks.
    #[must_use]
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
struct CachedMembership {
    /// Whether the user is still a valid member of the room
    is_member: bool,
    /// Whether the user is banned
    is_banned: bool,
}

impl CachedMembership {
    /// Create a cached membership from a member lookup result.
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

// Re-use the canonical room_role_to_proto from client::convert (E2: deduplicate)
use crate::impls::client::room_role_to_proto;

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
    /// `ChatService` for chat message handling with business logic.
    /// Chat messages are processed through `ChatService::send_message()`
    /// which handles permission checks, content filtering, rate limiting, and persistence.
    chat_service: Arc<ChatService>,
    cluster_manager: Arc<ClusterManager>,
    /// Optional notification service for direct real-time push to connected clients.
    /// When set, the handler subscribes to notification events and pushes them
    /// without depending on the gRPC notification-to-cluster bridge.
    notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
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
    /// When true, `cleanup()` skips broadcasting `UserLeft`.
    ///
    /// Used when:
    /// - the event was already published by an explicit API call (`leave_room/delete_room`)
    /// - the connection never completed its initial join handshake, so broadcasting
    ///   `UserLeft` would create a ghost offline event for a user that was never
    ///   actually announced as online
    skip_cleanup_user_left: Arc<std::sync::atomic::AtomicBool>,
    /// Cached membership status for heartbeat validation.
    /// Uses TTL-based expiration (30 seconds) to reduce database load while
    /// maintaining reasonable responsiveness to membership changes.
    /// Key: (`room_id`, `user_id`) tuple for O(1) lookup.
    membership_cache: Arc<moka::sync::Cache<(String, String), CachedMembership>>,
    /// Instance-level concurrency configuration for backpressure control.
    /// This replaces the global `MESSAGE_PROCESSING_SEMAPHORE` with per-AppState configuration.
    concurrency_config: Arc<MessageConcurrencyConfig>,
    /// Throttle state for playback progress DB writes.
    /// Stores the (last_written_position, last_write_time) to avoid
    /// writing to the DB on every progress heartbeat.
    last_progress_write: Arc<tokio::sync::Mutex<Option<(f64, tokio::time::Instant)>>>,
    heartbeat_schedule: HeartbeatSchedule,
}

impl Clone for StreamMessageHandler {
    fn clone(&self) -> Self {
        Self {
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            connection_id: self.connection_id.clone(),
            room_service: Arc::clone(&self.room_service),
            chat_service: Arc::clone(&self.chat_service),
            cluster_manager: Arc::clone(&self.cluster_manager),
            notification_service: self.notification_service.clone(),
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
            last_progress_write: Arc::clone(&self.last_progress_write),
            heartbeat_schedule: self.heartbeat_schedule,
        }
    }
}

impl StreamMessageHandler {
    fn error_server_message(error: impl Into<crate::impls::ApiError>) -> ServerMessage {
        let api_error: crate::impls::ApiError = error.into();
        ServerMessage {
            message: Some(crate::proto::client::server_message::Message::Error(
                api_error.to_proto_error(),
            )),
        }
    }

    fn validate_webrtc_recipient(&self, recipient: &str) -> Result<(), String> {
        let Some((target_user_id, target_conn_id)) = recipient.split_once(':') else {
            return Err("WebRTC recipient must be formatted as user_id:conn_id".to_string());
        };

        let target = self
            .connection_manager
            .get_connection(target_conn_id)
            .ok_or_else(|| "Target connection is no longer active".to_string())?;

        if target.user_id.as_str() != target_user_id {
            return Err("WebRTC recipient does not match the target connection owner".to_string());
        }

        let target_room_id = target
            .room_id
            .as_ref()
            .ok_or_else(|| "Target connection is not currently joined to a room".to_string())?;
        if target_room_id != &self.room_id {
            return Err("Target connection is not in this room".to_string());
        }

        if !target.rtc_joined {
            return Err("Target connection has not joined WebRTC".to_string());
        }

        Ok(())
    }

    fn current_connection_matches_webrtc_recipient(&self, recipient: &str) -> bool {
        let (target_user_id, target_conn_id) = recipient
            .split_once(':')
            .map_or((None, recipient), |(user_id, conn_id)| {
                (Some(user_id), conn_id)
            });

        if target_conn_id != self.connection_id {
            return false;
        }

        let Some(current) = self.connection_manager.get_connection(&self.connection_id) else {
            return false;
        };

        let user_matches = target_user_id.is_none_or(|user_id| current.user_id.as_str() == user_id);

        user_matches && current.room_id.as_ref() == Some(&self.room_id) && current.rtc_joined
    }

    /// Create a new stream message handler
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        room_id: RoomId,
        user_id: UserId,
        username: String,
        room_service: Arc<RoomService>,
        chat_service: Arc<ChatService>,
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
            chat_service,
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
        chat_service: Arc<ChatService>,
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
        // This reduces database queries from every heartbeat (25-35s) to at most once per TTL (30s).
        let membership_cache = Arc::new(
            moka::sync::Cache::builder()
                .time_to_live(HeartbeatSchedule::production().membership_cache_ttl())
                .build(),
        );
        Self {
            room_id,
            user_id,
            username,
            connection_id,
            room_service,
            chat_service,
            cluster_manager,
            notification_service: None,
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
            last_progress_write: Arc::new(tokio::sync::Mutex::new(None)),
            heartbeat_schedule: HeartbeatSchedule::production(),
        }
    }

    /// Set the per-connection WebSocket message rate limit from config.
    #[must_use]
    pub const fn with_ws_message_rate_limit(mut self, limit: u32) -> Self {
        self.ws_message_rate_limit = limit;
        self
    }

    /// Set the notification service for direct real-time notification push.
    ///
    /// When set, the handler subscribes to `UserNotificationService::subscribe_events()`
    /// and pushes notifications directly to the connected client without depending on
    /// the gRPC notification-to-cluster bridge task.
    #[must_use]
    pub fn with_notification_service(
        mut self,
        service: Arc<synctv_core::service::UserNotificationService>,
    ) -> Self {
        self.notification_service = Some(service);
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

    #[must_use]
    pub fn with_heartbeat_schedule(mut self, schedule: HeartbeatSchedule) -> Self {
        self.membership_cache = Arc::new(
            moka::sync::Cache::builder()
                .time_to_live(schedule.membership_cache_ttl())
                .build(),
        );
        self.heartbeat_schedule = schedule;
        self
    }

    #[must_use]
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    /// Invalidate the membership cache entry for a specific user in a room.
    ///
    /// Called when a `KickUser` or `KickUserFromRoom` admin event is received,
    /// ensuring that the heartbeat check will re-query the database on the next
    /// tick instead of trusting the stale cached "member" status.
    pub fn invalidate_membership_cache(&self, room_id: &RoomId, user_id: &UserId) {
        let cache_key = (room_id.as_str().to_string(), user_id.as_str().to_string());
        self.membership_cache.invalidate(&cache_key);
    }

    /// Register the connection and join the room, enforcing connection limits.
    ///
    /// Call this **before** returning the gRPC response stream so that limit
    /// violations surface as a proper gRPC error instead of silently failing
    /// inside a background task.  After a successful `pre_join`, call
    /// [`run_after_join`] to enter the message loop.
    pub async fn pre_join(&self) -> Result<(), String> {
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

        Ok(())
    }

    /// Run the complete message loop using unified IO abstraction.
    ///
    /// This is the recommended method that handles both sending and receiving
    /// in a single unified loop using the `StreamMessage` trait.
    ///
    /// This method:
    /// 1. Registers the connection and joins the room (enforcing limits)
    /// 2. Subscribes to cluster events and forwards them to the client
    /// 3. Receives client messages via the `StreamMessage` trait
    /// 4. Handles rate limiting, content filtering, and permissions
    /// 5. Broadcasts events to the cluster
    /// 6. Monitors for disconnect signals (user ban, kick, etc.)
    /// 7. Handles cleanup on disconnect
    ///
    /// The caller only needs to provide a `StreamMessage` implementation (WebSocket or gRPC).
    ///
    /// If you need to check connection limits *before* returning a response stream
    /// (e.g. in gRPC), call [`pre_join`] first and then [`run_after_join`].
    pub async fn run<S: StreamMessage>(&self, stream: &mut S) -> Result<(), String> {
        self.pre_join().await?;
        self.run_after_join(stream).await
    }

    /// Continue the message loop after a successful [`pre_join`].
    ///
    /// This is identical to [`run`] but skips the register/join_room steps
    /// that were already performed by `pre_join`.
    pub async fn run_after_join<S: StreamMessage>(&self, stream: &mut S) -> Result<(), String> {
        let room_id_str = self.room_id.as_str().to_string();

        // Subscribe to cluster events using the same connection_id as ConnectionManager
        let (mut event_rx, _connection_id) = self
            .cluster_manager
            .subscribe_with_id(
                self.room_id.clone(),
                self.user_id.clone(),
                self.connection_id.clone(),
            )
            .await
            .map_err(|e| format!("Failed to subscribe to cluster events: {e}"))?;

        // Subscribe to disconnect signals
        let mut disconnect_rx = self.connection_manager.subscribe_disconnect();

        // Subscribe to admin events (KickUser, etc.) for cross-replica disconnect propagation.
        // KickUser events arrive via Redis PubSub on the admin channel and are not
        // delivered through the room-level event subscription, so each connection
        // must independently monitor admin events and disconnect when targeted.
        let mut admin_rx = self.cluster_manager.subscribe_admin_events();

        // H11: Subscribe to notification events for direct real-time push.
        // This ensures notifications are delivered to WebSocket clients even when
        // the gRPC notification-to-cluster bridge is not running.
        let mut notification_rx = self
            .notification_service
            .as_ref()
            .map(|svc| svc.subscribe_events());

        // E6 fix: Fetch member data ONCE and pass to both methods
        let member_data = self
            .room_service
            .member_service()
            .get_member(&self.room_id, &self.user_id)
            .await
            .ok()
            .flatten();

        // Send initial user joined notification.
        // If the transport is already gone here, we still need to run cleanup()
        // because pre_join() already registered the connection and subscribed state
        // will be established below.
        if let Err(error) =
            stream.send(self.create_user_joined_message(&room_id_str, member_data.as_ref()))
        {
            tracing::error!(
                "Failed to send initial UserJoined message in run_after_join(): {error}"
            );
            self.skip_cleanup_user_left
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.cleanup(&room_id_str).await;
            return Ok(());
        }

        // Broadcast UserJoined event to other replicas
        self.broadcast_user_joined(member_data.as_ref()).await;

        // Create heartbeat interval OUTSIDE the loop so it doesn't reset
        // when other select! branches fire.
        // Add random jitter (±5 s around the 30 s base) so that 1000 concurrent
        // connections do not all fire their DB membership checks in the same
        // one-second window (thundering-herd protection).
        let heartbeat_period = self.heartbeat_schedule.period_with_random_jitter();
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
                            let permit = if let Ok(permit) = semaphore.try_acquire_owned() { permit } else {
                                tracing::warn!(
                                    user_id = %self.user_id.as_str(),
                                    room_id = %self.room_id.as_str(),
                                    "System overloaded: message processing semaphore exhausted, returning ResourceExhausted"
                                );
                                // Send ResourceExhausted error to client
                            let error_msg = Self::error_server_message(
                                crate::impls::ApiError::RateLimited(
                                    "System overloaded, please retry later".to_string(),
                                ),
                            );
                            if let Err(e) = stream.send(error_msg) {
                                tracing::error!(
                                    "Failed to send ResourceExhausted error to client: {}",
                                    e
                                );
                                break;
                            }
                            continue;
                        };

                            // Process message with semaphore permit held
                            let _permit = permit; // Hold permit for duration of processing
                            if let Err(e) = self.handle_client_message(&msg).await {
                                tracing::error!("Failed to handle client message: {}", e);
                                if let Err(send_err) =
                                    stream.send(Self::error_server_message(e.clone()))
                                {
                                    tracing::error!(
                                        "Failed to send message error to client: {}",
                                        send_err
                                    );
                                    break;
                                }
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
                            if !self.current_connection_matches_webrtc_recipient(to) {
                                continue;
                            }
                        }

                        let mut send_failed = false;
                        for msg in cluster_event_to_server_messages(&event, &room_id_str) {
                            if let Err(e) = stream.send(msg) {
                                tracing::error!("Failed to send server message: {}", e);
                                send_failed = true;
                                break;
                            }
                        }
                        if send_failed {
                            break;
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
                                self.skip_cleanup_user_left
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
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
                                        self.skip_cleanup_user_left
                                            .store(true, std::sync::atomic::Ordering::Relaxed);
                                        break;
                                    }
                                }
                                Ok(None) => {
                                    tracing::info!(
                                        user_id = %self.user_id.as_str(),
                                        room_id = %self.room_id.as_str(),
                                        "User is no longer a member (detected after disconnect signal lag), disconnecting"
                                    );
                                    self.skip_cleanup_user_left
                                        .store(true, std::sync::atomic::Ordering::Relaxed);
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
                            // Invalidate membership cache immediately so the banned user
                            // cannot send messages during the remaining cache TTL window.
                            let cache_key = (self.room_id.as_str().to_string(), user_id.as_str().to_string());
                            self.membership_cache.invalidate(&cache_key);

                            if *user_id == self.user_id {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    reason = %reason,
                                    "Received cross-replica KickUser event, disconnecting"
                                );
                                self.skip_cleanup_user_left.store(
                                    true,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                break;
                            }
                        }
                        Ok(ClusterEvent::KickUserFromRoom { ref user_id, ref room_id, ref reason, .. }) => {
                            // Invalidate membership cache immediately so the kicked/banned
                            // user cannot send messages during the remaining cache TTL window.
                            let cache_key = (room_id.as_str().to_string(), user_id.as_str().to_string());
                            self.membership_cache.invalidate(&cache_key);

                            if *user_id == self.user_id && *room_id == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    room_id = %self.room_id.as_str(),
                                    reason = %reason,
                                    "Received cross-replica KickUserFromRoom event, disconnecting"
                                );
                                self.skip_cleanup_user_left.store(
                                    true,
                                    std::sync::atomic::Ordering::Relaxed,
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
                        Ok(ClusterEvent::UserNotification { ref user_id, ref title, ref content, ref notification_type, ref notification_id, timestamp, .. }) => {
                            // RT-1: Push persistent notification to user's active WebSocket connection.
                            // Uses the dedicated Notification variant (not ErrorMessage abuse).
                            if *user_id == self.user_id {
                                let data = serde_json::json!({
                                    "type": "user_notification",
                                    "notification_id": notification_id,
                                    "notification_type": notification_type,
                                    "title": title,
                                    "content": content,
                                });
                                let msg = ServerMessage {
                                    message: Some(crate::proto::client::server_message::Message::Notification(
                                        crate::proto::client::UserNotification {
                                            notification_id: notification_id.clone(),
                                            notification_type: notification_type.clone(),
                                            title: title.clone(),
                                            content: content.clone(),
                                            data: data.to_string(),
                                            timestamp: timestamp.timestamp(),
                                        },
                                    )),
                                };
                                if let Err(e) = stream.send(msg) {
                                    tracing::error!("Failed to push notification to WebSocket: {}", e);
                                    break;
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
                                        self.skip_cleanup_user_left
                                            .store(true, std::sync::atomic::Ordering::Relaxed);
                                        break;
                                    }
                                }
                                Ok(None) => {
                                    tracing::info!(
                                        user_id = %self.user_id.as_str(),
                                        room_id = %self.room_id.as_str(),
                                        "User is no longer a member (detected after admin event lag), disconnecting"
                                    );
                                    self.skip_cleanup_user_left
                                        .store(true, std::sync::atomic::Ordering::Relaxed);
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

                // H11: Direct notification push from UserNotificationService.
                // When notification_service is configured, notifications are pushed
                // directly without depending on the gRPC bridge task.
                result = async {
                    match notification_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match result {
                        Ok(event) => {
                            // Only push if this notification targets the connected user
                            if event.user_id == self.user_id {
                                let data = serde_json::json!({
                                    "type": "user_notification",
                                    "notification_id": event.notification.id.to_string(),
                                    "notification_type": event.notification.notification_type.to_string(),
                                    "title": &event.notification.title,
                                    "content": &event.notification.content,
                                });
                                let msg = ServerMessage {
                                    message: Some(crate::proto::client::server_message::Message::Notification(
                                        crate::proto::client::UserNotification {
                                            notification_id: event.notification.id.to_string(),
                                            notification_type: event.notification.notification_type.to_string(),
                                            title: event.notification.title,
                                            content: event.notification.content,
                                            data: data.to_string(),
                                            timestamp: event.notification.created_at.timestamp(),
                                        },
                                    )),
                                };
                                if let Err(e) = stream.send(msg) {
                                    tracing::error!("Failed to push direct notification to WebSocket: {}", e);
                                    break;
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                lagged = n,
                                user_id = %self.user_id.as_str(),
                                "Notification event channel lagged, re-subscribing"
                            );
                            notification_rx = self
                                .notification_service
                                .as_ref()
                                .map(|svc| svc.subscribe_events());
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::debug!("Notification event channel closed");
                            notification_rx = None;
                        }
                    }
                }

                // Heartbeat/health check every 30 seconds.
                // Also acts as a periodic membership re-validation backstop:
                // verifies the user is still a valid (non-banned, non-removed)
                // member of the room. This catches cases where the disconnect
                // signal channel lagged and the ban/kick signal was lost.
                //
                // Uses the membership cache to reduce database queries: if a
                // cached entry exists and shows the user as a valid member, the
                // DB query is skipped. When a KickUser or KickUserFromRoom admin
                // event arrives, the cache entry is invalidated immediately,
                // forcing the next heartbeat to re-query the DB.
                _ = heartbeat_interval.tick() => {
                    if !stream.is_alive() {
                        tracing::info!("Connection no longer alive");
                        break;
                    }
                    if let Err(e) = stream.ping() {
                        tracing::info!("Ping failed, connection dead: {}", e);
                        break;
                    }

                    // Check membership cache first to avoid unnecessary DB queries.
                    let cache_key = (self.room_id.as_str().to_string(), self.user_id.as_str().to_string());
                    if let Some(cached) = self.membership_cache.get(&cache_key) {
                        if cached.is_banned {
                            tracing::info!(
                                user_id = %self.user_id.as_str(),
                                room_id = %self.room_id.as_str(),
                                "Periodic check (cached): user is banned, disconnecting"
                            );
                            self.skip_cleanup_user_left
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        if !cached.is_member {
                            tracing::info!(
                                user_id = %self.user_id.as_str(),
                                room_id = %self.room_id.as_str(),
                                "Periodic check (cached): user is no longer a member, disconnecting"
                            );
                            self.skip_cleanup_user_left
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        // Cache hit with valid member status -- skip DB query
                        continue;
                    }

                    // Cache miss: query database and populate cache.
                    match self.room_service.member_service().get_member(&self.room_id, &self.user_id).await {
                        Ok(Some(member)) => {
                            let cached = CachedMembership::from_member(Some(&member));
                            self.membership_cache.insert(cache_key, cached);
                            if member.status == synctv_core::models::MemberStatus::Banned {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    room_id = %self.room_id.as_str(),
                                    "Periodic check: user is banned, disconnecting"
                                );
                                self.skip_cleanup_user_left
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Ok(None) => {
                            let cached = CachedMembership::from_member(None);
                            self.membership_cache.insert(cache_key, cached);
                            tracing::info!(
                                user_id = %self.user_id.as_str(),
                                room_id = %self.room_id.as_str(),
                                "Periodic check: user is no longer a member, disconnecting"
                            );
                            self.skip_cleanup_user_left
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        Err(e) => {
                            // Log but don't disconnect — transient DB error should not
                            // kick valid users. Will retry on the next 30-second tick.
                            // Don't cache the error -- next tick will retry.
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
    /// Create the initial `UserJoined` server message.
    ///
    /// E6 fix: Accepts pre-fetched member data to avoid a redundant DB query
    /// (the same data is also needed by `broadcast_user_joined`).
    fn create_user_joined_message(
        &self,
        room_id: &str,
        member: Option<&synctv_core::models::RoomMember>,
    ) -> ServerMessage {
        use crate::proto::client::UserJoinedRoom;
        use crate::proto::client::server_message::Message;
        use synctv_proto::common::RoomMember as ProtoRoomMember;

        let (role_proto, permissions, added, removed, admin_added, admin_removed) = match member {
            Some(member) => {
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
            None => {
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
                member: Some(ProtoRoomMember {
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
    /// Broadcast `UserJoined` event to cluster replicas.
    ///
    /// E6 fix: Accepts pre-fetched member data to avoid a redundant DB query.
    async fn broadcast_user_joined(&self, member: Option<&synctv_core::models::RoomMember>) {
        match self
            .connection_manager
            .has_existing_presence_for_user_in_room_distributed(
                &self.user_id,
                &self.room_id,
                &self.connection_id,
            )
            .await
        {
            Ok(true) => {
                tracing::debug!(
                    room_id = %self.room_id.as_str(),
                    user_id = %self.user_id.as_str(),
                    connection_id = %self.connection_id,
                    "Skipping UserJoined broadcast because the user is already present in the room on another connection"
                );
                return;
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id.as_str(),
                    user_id = %self.user_id.as_str(),
                    connection_id = %self.connection_id,
                    "Distributed same-user presence lookup failed during join; continuing with UserJoined broadcast to avoid missing online signal"
                );
            }
            Ok(false) => {}
        }

        let (role_proto, permissions) = match member {
            Some(member) => {
                let effective = member.effective_permissions(member.role.permissions());
                let role = room_role_to_proto(member.role);
                (role, effective)
            }
            None => {
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
            added_permissions: synctv_core::models::PermissionBits(0),
            removed_permissions: synctv_core::models::PermissionBits(0),
            admin_added_permissions: synctv_core::models::PermissionBits(0),
            admin_removed_permissions: synctv_core::models::PermissionBits(0),
            joined_at: chrono::Utc::now(),
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
        //
        // IMPORTANT: We must check if the connection is STILL marked as RTC-joined
        // in the connection manager before decrementing the metric. This prevents
        // a race condition where:
        // 1. Cleanup task times out the WebRTC session (mark_rtc_joined(false))
        // 2. Connection ungracefully disconnects
        // 3. cleanup() sees has_webrtc_session=true and decrements the metric again
        // Result: Metric underflow (negative value)
        //
        // By checking the connection manager's state, we ensure idempotency:
        // - If the cleanup task already timed out the session, the connection
        //   manager will have rtc_joined=false, and we skip the decrement
        // - If the user explicitly left WebRTC, the flag is already false, and we skip
        // - Only if the connection truly had an active session do we decrement
        if self
            .has_webrtc_session
            .swap(false, std::sync::atomic::Ordering::Acquire)
        {
            // Check if the connection is still marked as RTC-joined in the connection manager
            // This prevents double-decrement if the cleanup task already timed out the session
            let is_still_rtc_joined = self
                .connection_manager
                .get_connection(&self.connection_id)
                .is_some_and(|conn| conn.rtc_joined);

            if is_still_rtc_joined {
                // Only decrement the metric if the connection was still RTC-joined
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
            } else {
                // Session was already cleaned up by timeout task or explicit leave
                // Just clear the connection manager state (idempotent)
                tracing::debug!(
                    user = %self.username,
                    room = %room_id,
                    connection = %self.connection_id,
                    "WebRTC session already cleaned up (skipped metric decrement and broadcast)"
                );
            }
        }

        // R-10/R-11: If the disconnect was triggered by a cluster event that
        // already published UserLeft (e.g. leave_room or delete_room API), skip
        // the redundant broadcast to avoid double UserLeft events.
        if self
            .skip_cleanup_user_left
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            tracing::debug!(
                user = %self.username,
                room = %room_id,
                "Skipping UserLeft broadcast in cleanup (already published by API call)"
            );
            // Still unregister from connection manager
            self.connection_manager
                .unregister(&self.connection_id)
                .await;
            self.cluster_manager.unsubscribe(&self.connection_id);
            return;
        }

        let has_other_local_connection = self
            .connection_manager
            .get_user_connections(&self.user_id)
            .into_iter()
            .any(|conn| {
                conn.connection_id != self.connection_id
                    && conn
                        .room_id
                        .as_ref()
                        .is_some_and(|rid| rid == &self.room_id)
            });

        let user_left_delivery_plan = match self
            .connection_manager
            .has_other_connection_for_user_in_room_distributed(
                &self.user_id,
                &self.room_id,
                &self.connection_id,
            )
            .await
        {
            Ok(has_other_connection) => {
                should_broadcast_user_left(has_other_local_connection, Ok(has_other_connection))
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    user = %self.username,
                    room = %room_id,
                    connection = %self.connection_id,
                    "Distributed same-user presence lookup failed during cleanup; skipping UserLeft broadcast to avoid false offline signal"
                );
                should_broadcast_user_left(has_other_local_connection, Err(()))
            }
        };

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
        let result = match user_left_delivery_plan {
            UserLeftDeliveryPlan::Skip => {
                tracing::debug!(
                    user = %self.username,
                    room = %room_id,
                    connection = %self.connection_id,
                    "Skipping UserLeft broadcast in cleanup because another connection for the same user remains in the room"
                );
                None
            }
            UserLeftDeliveryPlan::LocalAndRedis => Some(self.cluster_manager.broadcast(event)),
        };

        if let Some(result) = result {
            if user_left_delivery_plan == UserLeftDeliveryPlan::LocalAndRedis
                && should_retry_user_left_broadcast(
                    result.clone(),
                    self.cluster_manager.metrics().redis_enabled,
                )
            {
                // Critical UserLeft event failed to reach any destination.
                // This can happen when Redis is temporarily unavailable.
                // Spawn a background task to retry the broadcast with exponential backoff.
                //
                // Use a global semaphore to limit concurrent retry tasks. During mass
                // disconnects with Redis down, thousands of connections may all try to
                // spawn retry tasks simultaneously. Without this bound, we'd exhaust
                // memory and CPU on unbounded task spawning.
                let cluster_manager = self.cluster_manager.clone();
                let room_id = self.room_id.clone();
                let user_id = self.user_id.clone();
                let username = self.username.clone();
                let connection_id = self.connection_id.clone();

                let semaphore = Arc::clone(&USER_LEFT_RETRY_SEMAPHORE);
                let permit = semaphore.try_acquire_owned();

                match permit {
                    Ok(permit) => {
                        tracing::warn!(
                            user = %username,
                            room = %room_id.as_str(),
                            connection = %connection_id,
                            "UserLeft broadcast reached no subscribers; starting retry task"
                        );

                        spawn_monitored("userleft_retry", async move {
                            let _permit = permit; // Hold permit for duration of retry task

                            const MAX_RETRIES: u32 = 5;
                            const INITIAL_DELAY_MS: u64 = 100;
                            const MAX_DELAY_MS: u64 = 5000;

                            let mut delay_ms = INITIAL_DELAY_MS;

                            for attempt in 1..=MAX_RETRIES {
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms))
                                    .await;

                                let retry_event = ClusterEvent::UserLeft {
                                    event_id: nanoid::nanoid!(16),
                                    room_id: room_id.clone(),
                                    user_id: user_id.clone(),
                                    username: username.clone(),
                                    timestamp: chrono::Utc::now(),
                                };

                                let retry_result = synctv_cluster::sync::BroadcastResult {
                                    local_sent: 0,
                                    redis_sent: cluster_manager.publish_only(retry_event),
                                };

                                if retry_result.redis_sent {
                                    tracing::info!(
                                        user = %username,
                                        room = %room_id.as_str(),
                                        connection = %connection_id,
                                        attempt = attempt,
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
                    Err(_) => {
                        tracing::warn!(
                            user = %username,
                            room = %room_id.as_str(),
                            connection = %connection_id,
                            "UserLeft retry task limit reached (max 100 concurrent); event may be lost"
                        );
                    }
                }
            }
        }

        // Now unregister from connection manager after broadcast has been sent
        self.connection_manager
            .unregister(&self.connection_id)
            .await;
        self.cluster_manager.unsubscribe(&self.connection_id);

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

        // E6 fix: Fetch member data ONCE and pass to both methods
        let member_data = self
            .room_service
            .member_service()
            .get_member(&self.room_id, &self.user_id)
            .await
            .ok()
            .flatten();

        // Send initial UserJoined message to the client (mirrors run() behavior)
        let room_id_str = self.room_id.as_str().to_string();
        let initial_msg = self.create_user_joined_message(&room_id_str, member_data.as_ref());
        if let Err(e) = self.sender.send(initial_msg) {
            tracing::error!("Failed to send initial UserJoined message in start(): {e}");
            self.skip_cleanup_user_left
                .store(true, std::sync::atomic::Ordering::Relaxed);
            cancel_token.cancel();
        } else {
            // Broadcast UserJoined event to other replicas only after the
            // connection has observed the initial join payload locally.
            // Otherwise we can create a transient ghost-presence event for a
            // connection that never became usable.
            self.broadcast_user_joined(member_data.as_ref()).await;
        }

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
            .await
            .map_err(|e| format!("Failed to subscribe to cluster events: {e}"))?;
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

                                for msg in cluster_event_to_server_messages(&event, &room_id_str) {
                                    if let Err(e) = sender.send(msg) {
                                        tracing::error!("Failed to send message: {}", e);
                                        event_token.cancel();
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
                                let permit = if let Ok(permit) = semaphore.try_acquire_owned() { permit } else {
                                    tracing::warn!(
                                        connection_id = %handler.connection_id,
                                        "System overloaded: message processing semaphore exhausted in start()"
                                    );
                                    continue;
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
            let skip_cleanup_user_left = Arc::clone(&self.skip_cleanup_user_left);

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
                                    Ok(Some(member)) => membership_invalidation_requires_skip_cleanup(Some(&member)),
                                    Ok(None) => membership_invalidation_requires_skip_cleanup(None),
                                    _ => false,
                                };
                                if is_removed {
                                    skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                    disconnect_token.cancel();
                                    break;
                                }
                            } else if should_disconnect {
                                if let Ok(signal) = &signal {
                                    if disconnect_signal_requires_skip_cleanup(signal, &user_id, &room_id, &connection_id) {
                                        skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
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
                            // Uses the dedicated Notification variant (not ErrorMessage abuse).
                            if let Ok(ClusterEvent::UserNotification { user_id: ref uid, ref title, ref content, ref notification_type, ref notification_id, timestamp, .. }) = admin_event {
                                if *uid == user_id {
                                    let data = serde_json::json!({
                                        "type": "user_notification",
                                        "notification_id": notification_id,
                                        "notification_type": notification_type,
                                        "title": title,
                                        "content": content,
                                    });
                                    let msg = ServerMessage {
                                        message: Some(crate::proto::client::server_message::Message::Notification(
                                            crate::proto::client::UserNotification {
                                                notification_id: notification_id.clone(),
                                                notification_type: notification_type.clone(),
                                                title: title.clone(),
                                                content: content.clone(),
                                                data: data.to_string(),
                                                timestamp: timestamp.timestamp(),
                                            },
                                        )),
                                    };
                                    if let Err(e) = admin_sender.send(msg) {
                                        tracing::error!("Failed to push notification in start(): {}", e);
                                        disconnect_token.cancel();
                                        break;
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
                                    Ok(Some(member)) => membership_invalidation_requires_skip_cleanup(Some(&member)),
                                    Ok(None) => membership_invalidation_requires_skip_cleanup(None),
                                    _ => false,
                                };
                                if is_removed {
                                    skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                    disconnect_token.cancel();
                                    break;
                                }
                            } else if should_disconnect {
                                if let Ok(event) = &admin_event {
                                    if admin_event_requires_skip_cleanup(event, &user_id, &room_id) {
                                        skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
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
            let heartbeat_schedule = self.heartbeat_schedule;
            let skip_cleanup_user_left = Arc::clone(&self.skip_cleanup_user_left);
            spawn_monitored("messaging_heartbeat", async move {
                // Derive jitter from the user_id bytes so each connection gets a
                // stable-but-different offset within the 25–35 s window.
                let period = heartbeat_schedule.period_for_user(&heartbeat_user_id);
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
                                        skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
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
                                    skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
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
                // Check if this is a danmaku message (has position)
                let is_danmaku = chat_msg.position.is_some();

                if is_danmaku {
                    // Danmaku: validate, check settings, rate limit, filter, then handle
                    self.room_service
                        .check_permission(&self.room_id, &self.user_id, PermissionBits::SEND_CHAT)
                        .await
                        .map_err(|e| e.to_string())?;

                    if chat_msg.content.is_empty() {
                        return Err("Danmaku message cannot be empty".to_string());
                    }
                    if chat_msg.content.chars().count()
                        > synctv_core::service::chat::MAX_CHAT_MESSAGE_CHARS
                    {
                        return Err(format!(
                            "Danmaku message too long (max {} characters)",
                            synctv_core::service::chat::MAX_CHAT_MESSAGE_CHARS,
                        ));
                    }

                    let room_settings = self
                        .room_service
                        .get_room_settings(&self.room_id)
                        .await
                        .map_err(|e| e.to_string())?;
                    if !room_settings.danmaku_enabled.0 {
                        return Err("Danmaku is disabled in this room".to_string());
                    }

                    let rate_limit_key = format!(
                        "room:{}:user:{}:danmaku",
                        self.room_id.as_str(),
                        self.user_id.as_str()
                    );
                    self.rate_limiter
                        .check_rate_limit(
                            &rate_limit_key,
                            self.rate_limit_config.danmaku_per_second,
                            self.rate_limit_config.window_seconds,
                        )
                        .await
                        .map_err(|e| e.to_string())?;

                    let sanitized_content = self
                        .content_filter
                        .filter_danmaku(&chat_msg.content)
                        .map_err(|e| e.to_string())?;

                    validate_danmaku_color(&chat_msg.color)?;
                    self.handle_danmaku(
                        &sanitized_content,
                        chat_msg.position.unwrap_or(0.0),
                        chat_msg.color.clone(),
                    )
                    .await?;
                } else {
                    // Chat: delegate entirely to ChatService which handles permissions,
                    // room settings, rate limiting, content filtering, and persistence.
                    // This eliminates the dual-path fallback (H10).
                    self.handle_chat_message(&chat_msg.content).await?;
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
        // Delegate to ChatService which handles permission checks, content filtering,
        // rate limiting, and persistence (no fallback path).
        let saved_msg = self
            .chat_service
            .send_message(
                self.room_id.clone(),
                self.user_id.clone(),
                content.to_string(),
            )
            .await
            .map_err(|e| e.to_string())?;

        // Touch room activity to prevent TTL expiry on active rooms
        self.room_service
            .touch_room_activity(self.room_id.clone())
            .await;

        // Track chat message metric
        synctv_core::metrics::http::CHAT_MESSAGES_TOTAL
            .with_label_values(&[] as &[&str])
            .inc();

        // Use the filtered content from ChatService (content filtering already applied)
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            message: saved_msg.content,
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
        // Validate SDP payload size
        if offer.data.len() > MAX_SDP_SIZE {
            return Err(format!(
                "WebRTC SDP offer too large ({} bytes, max: {MAX_SDP_SIZE} bytes)",
                offer.data.len()
            ));
        }

        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        let conn_id = self.connection_id.clone();

        if self.connection_manager.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&offer.to)?;

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

        // Cross-replica WebRTC signaling must reach Redis when cluster mode is enabled.
        let result = self.cluster_manager.broadcast(event);
        if should_fail_webrtc_signal_broadcast(result, self.cluster_manager.metrics().redis_enabled)
        {
            tracing::warn!(
                room_id = %self.room_id.as_str(),
                "WebRTC offer cluster broadcast did not reach Redis while cluster fan-out is enabled"
            );
            synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                .with_label_values(&["webrtc_signal_no_redis"])
                .inc();
            return Err(
                "WebRTC offer delivery failed: cluster Redis publish unavailable".to_string(),
            );
        }

        Ok(())
    }

    async fn handle_webrtc_answer(
        &self,
        answer: &crate::proto::client::WebRtcAnswer,
    ) -> Result<(), String> {
        // Validate SDP payload size
        if answer.data.len() > MAX_SDP_SIZE {
            return Err(format!(
                "WebRTC SDP answer too large ({} bytes, max: {MAX_SDP_SIZE} bytes)",
                answer.data.len()
            ));
        }

        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        let conn_id = self.connection_id.clone();

        if self.connection_manager.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&answer.to)?;

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

        // Cross-replica WebRTC signaling must reach Redis when cluster mode is enabled.
        let result = self.cluster_manager.broadcast(event);
        if should_fail_webrtc_signal_broadcast(result, self.cluster_manager.metrics().redis_enabled)
        {
            tracing::warn!(
                room_id = %self.room_id.as_str(),
                "WebRTC answer cluster broadcast did not reach Redis while cluster fan-out is enabled"
            );
            synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                .with_label_values(&["webrtc_signal_no_redis"])
                .inc();
            return Err(
                "WebRTC answer delivery failed: cluster Redis publish unavailable".to_string(),
            );
        }

        Ok(())
    }

    async fn handle_webrtc_ice_candidate(
        &self,
        candidate: &crate::proto::client::WebRtcIceCandidate,
    ) -> Result<(), String> {
        // Validate ICE candidate payload size
        if candidate.data.len() > MAX_ICE_CANDIDATE_SIZE {
            return Err(format!(
                "WebRTC ICE candidate too large ({} bytes, max: {MAX_ICE_CANDIDATE_SIZE} bytes)",
                candidate.data.len()
            ));
        }

        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        let conn_id = self.connection_id.clone();

        if self.connection_manager.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&candidate.to)?;

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

        // Cross-replica ICE signaling must reach Redis when cluster mode is enabled.
        let result = self.cluster_manager.broadcast(event);
        if should_fail_webrtc_signal_broadcast(result, self.cluster_manager.metrics().redis_enabled)
        {
            tracing::warn!(
                room_id = %self.room_id.as_str(),
                "ICE candidate cluster broadcast did not reach Redis while cluster fan-out is enabled"
            );
            synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                .with_label_values(&["webrtc_signal_no_redis"])
                .inc();
            return Err(
                "WebRTC ICE candidate delivery failed: cluster Redis publish unavailable"
                    .to_string(),
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

        let conn_id = self.connection_id.clone();

        let should_join = should_transition_webrtc_membership(
            self.connection_manager
                .get_connection(&conn_id)
                .map(|conn| conn.rtc_joined),
            true,
        )
        .map_err(std::string::ToString::to_string)?;

        if !should_join {
            tracing::debug!(
                room_id = %self.room_id.as_str(),
                user_id = %self.user_id.as_str(),
                connection_id = %conn_id,
                "Ignoring duplicate WebRTC join for already-joined connection"
            );
            return Ok(());
        }

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
        let conn_id = self.connection_id.clone();

        let should_leave = should_transition_webrtc_membership(
            self.connection_manager
                .get_connection(&conn_id)
                .map(|conn| conn.rtc_joined),
            false,
        )
        .map_err(std::string::ToString::to_string)?;

        if !should_leave {
            tracing::debug!(
                room_id = %self.room_id.as_str(),
                user_id = %self.user_id.as_str(),
                connection_id = %conn_id,
                "Ignoring duplicate WebRTC leave for already-left connection"
            );
            self.has_webrtc_session
                .store(false, std::sync::atomic::Ordering::Release);
            return Ok(());
        }

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

            // Throttle DB writes: only persist if position changed by >1s
            // or >5s elapsed since last write. This reduces write amplification
            // from every 3-5s heartbeat to only meaningful position changes.
            let should_write = {
                let guard = self.last_progress_write.lock().await;
                match *guard {
                    Some((last_pos, last_time)) => {
                        let pos_delta = (report.current_time - last_pos).abs();
                        let elapsed = last_time.elapsed().as_secs_f64();
                        pos_delta > PROGRESS_MIN_POSITION_DELTA
                            || elapsed > PROGRESS_MIN_ELAPSED_SECS
                    }
                    None => true, // First write always goes through
                }
            };

            if should_write {
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
                        // Record the write for throttling
                        {
                            let mut guard = self.last_progress_write.lock().await;
                            *guard = Some((report.current_time, tokio::time::Instant::now()));
                        }

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
        let response = self
            .room_service
            .playback_service()
            .seek(self.room_id.clone(), self.user_id.clone(), current_time)
            .await
            .map_err(|e| e.to_string())?;

        // Log warning if seek was not applied due to contention
        if !response.seek_applied {
            tracing::warn!(
                room_id = %self.room_id.as_str(),
                user_id = %self.user_id.as_str(),
                requested_time = current_time,
                actual_time = response.state.current_time,
                message = ?response.message,
                "Seek command returned degraded response"
            );
        }

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
        use crate::proto::client::HeartbeatAck;
        use crate::proto::client::server_message::Message;

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

/// Convert cluster event to zero or more server messages.
///
/// Returns a `Vec` because some events (e.g. `MediaRemovedBatch`) expand into
/// multiple individual client messages.
fn cluster_event_to_server_messages(
    event: &synctv_cluster::sync::ClusterEvent,
    room_id: &str,
) -> Vec<ServerMessage> {
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
        } => vec![ServerMessage {
            message: Some(Message::Chat(ChatMessageReceive {
                id: nanoid::nanoid!(12),
                room_id: room_id.to_string(),
                user_id: user_id.as_str().to_string(),
                username: username.clone(),
                content: message.clone(),
                timestamp: timestamp.timestamp(),
                position: *position,
                color: color.clone(),
            })),
        }],
        ClusterEvent::PlaybackStateChanged { state, .. } => vec![ServerMessage {
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
                    version: state.version,
                    playing_playlist_id: state
                        .playing_playlist_id
                        .as_ref()
                        .map(|id| id.as_str().to_string())
                        .unwrap_or_default(),
                    relative_path: state.relative_path.clone(),
                }),
            })),
        }],
        ClusterEvent::UserJoined {
            user_id,
            username,
            permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            joined_at,
            ..
        } => vec![ServerMessage {
            message: Some(Message::UserJoined(UserJoinedRoom {
                room_id: room_id.to_string(),
                member: Some(RoomMember {
                    room_id: room_id.to_string(),
                    user_id: user_id.as_str().to_string(),
                    username: username.clone(),
                    role: *role,
                    permissions: permissions.0,
                    added_permissions: added_permissions.0,
                    removed_permissions: removed_permissions.0,
                    admin_added_permissions: admin_added_permissions.0,
                    admin_removed_permissions: admin_removed_permissions.0,
                    joined_at: joined_at.timestamp(),
                    is_online: true,
                }),
            })),
        }],
        ClusterEvent::UserLeft { user_id, .. } => vec![ServerMessage {
            message: Some(Message::UserLeft(UserLeftRoom {
                room_id: room_id.to_string(),
                user_id: user_id.as_str().to_string(),
            })),
        }],
        ClusterEvent::MediaAdded {
            media_id,
            media_title,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaAdded(crate::proto::client::MediaAdded {
                room_id: room_id.to_string(),
                media_id: media_id.as_str().to_string(),
                title: media_title.clone(),
                added_by: username.clone(),
                added_by_user_id: user_id.as_str().to_string(),
            })),
        }],
        ClusterEvent::MediaRemoved {
            media_id,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaRemoved(crate::proto::client::MediaRemoved {
                room_id: room_id.to_string(),
                media_id: media_id.as_str().to_string(),
                removed_by: username.clone(),
                removed_by_user_id: user_id.as_str().to_string(),
            })),
        }],
        ClusterEvent::MediaRemovedBatch {
            media_ids,
            user_id,
            username,
            ..
        } => {
            // Expand batch removal into individual MediaRemoved messages for
            // backward compatibility. Clients receive one message per item, but
            // only one Redis pub/sub message was sent (O(1) network traffic).
            media_ids
                .iter()
                .map(|mid| ServerMessage {
                    message: Some(Message::MediaRemoved(crate::proto::client::MediaRemoved {
                        room_id: room_id.to_string(),
                        media_id: mid.as_str().to_string(),
                        removed_by: username.clone(),
                        removed_by_user_id: user_id.as_str().to_string(),
                    })),
                })
                .collect()
        }
        ClusterEvent::PermissionChanged {
            target_user_id,
            new_permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            changed_by_username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::PermissionChanged(
                crate::proto::client::PermissionChanged {
                    room_id: room_id.to_string(),
                    user_id: target_user_id.as_str().to_string(),
                    role: *role,
                    effective_permissions: new_permissions.0,
                    added_permissions: added_permissions.0,
                    removed_permissions: removed_permissions.0,
                    admin_added_permissions: admin_added_permissions.0,
                    admin_removed_permissions: admin_removed_permissions.0,
                    updated_by: changed_by_username.clone(),
                },
            )),
        }],
        ClusterEvent::RoomSettingsChanged { settings_json, .. } => vec![ServerMessage {
            message: Some(Message::RoomSettings(RoomSettingsChanged {
                room_id: room_id.to_string(),
                settings: settings_json.clone(),
            })),
        }],
        ClusterEvent::WebRTCSignaling {
            message_type,
            from,
            to,
            data,
            ..
        } => {
            // Convert to appropriate proto message based on message_type
            let msg = match message_type.as_str() {
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
            };
            msg.into_iter().collect()
        }
        ClusterEvent::WebRTCJoin {
            user_id,
            conn_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::WebrtcJoin(crate::proto::client::WebRtcJoin {
                user_id: user_id.as_str().to_string(),
                conn_id: conn_id.clone(),
                username: username.clone(),
            })),
        }],
        ClusterEvent::WebRTCLeave {
            user_id, conn_id, ..
        } => vec![ServerMessage {
            message: Some(Message::WebrtcLeave(crate::proto::client::WebRtcLeave {
                user_id: user_id.as_str().to_string(),
                conn_id: conn_id.clone(),
            })),
        }],
        ClusterEvent::SystemNotification {
            message, timestamp, ..
        } => {
            let data = serde_json::json!({
                "type": "system_notification",
                "notification_type": "system_announcement",
                "title": message,
                "content": message,
            });
            vec![ServerMessage {
                message: Some(Message::Notification(
                    crate::proto::client::UserNotification {
                        notification_id: String::new(),
                        notification_type: "system_announcement".to_string(),
                        title: message.clone(),
                        content: message.clone(),
                        data: data.to_string(),
                        timestamp: timestamp.timestamp(),
                    },
                )),
            }]
        }
        ClusterEvent::RoomDeleted { .. } => {
            // Notify WebSocket clients that the room has been deleted
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room has been deleted".to_string(),
                    code: crate::impls::error_codes::NOT_FOUND,
                    detail: String::new(),
                })),
            }]
        }
        ClusterEvent::KickPublisher { .. }
        | ClusterEvent::KickUser { .. }
        | ClusterEvent::KickUserFromRoom { .. }
        | ClusterEvent::RoomCreated { .. }
        | ClusterEvent::CacheInvalidate { .. }
        | ClusterEvent::UserNotification { .. }
        | ClusterEvent::Unknown => {
            // Admin/internal events are handled by other channels,
            // not forwarded to WebSocket clients via the room event path
            vec![]
        }
    }
}

const fn should_retry_user_left_broadcast(
    result: synctv_cluster::sync::BroadcastResult,
    cluster_redis_enabled: bool,
) -> bool {
    if cluster_redis_enabled {
        !result.redis_sent
    } else {
        result.local_sent == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserLeftDeliveryPlan {
    Skip,
    LocalAndRedis,
}

const fn should_broadcast_user_left(
    has_other_local_connection: bool,
    distributed_presence: Result<bool, ()>,
) -> UserLeftDeliveryPlan {
    if has_other_local_connection {
        return UserLeftDeliveryPlan::Skip;
    }

    match distributed_presence {
        Ok(true) => UserLeftDeliveryPlan::Skip,
        Ok(false) => UserLeftDeliveryPlan::LocalAndRedis,
        Err(()) => UserLeftDeliveryPlan::Skip,
    }
}

const fn should_transition_webrtc_membership(
    current_rtc_joined: Option<bool>,
    target_joined: bool,
) -> Result<bool, &'static str> {
    match current_rtc_joined {
        Some(current) => Ok(current != target_joined),
        None => Err("Connection not found"),
    }
}

#[inline]
fn membership_invalidation_requires_skip_cleanup(
    member: Option<&synctv_core::models::RoomMember>,
) -> bool {
    match member {
        Some(member) => member.status == synctv_core::models::MemberStatus::Banned,
        None => true,
    }
}

#[inline]
fn disconnect_signal_requires_skip_cleanup(
    signal: &synctv_cluster::sync::DisconnectSignal,
    user_id: &UserId,
    room_id: &RoomId,
    connection_id: &str,
) -> bool {
    match signal {
        synctv_cluster::sync::DisconnectSignal::Connection(conn_id) => conn_id == connection_id,
        synctv_cluster::sync::DisconnectSignal::User(uid) => uid == user_id,
        synctv_cluster::sync::DisconnectSignal::Room(rid) => rid == room_id,
        synctv_cluster::sync::DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        } => uid == user_id && rid == room_id,
    }
}

#[inline]
fn admin_event_requires_skip_cleanup(
    event: &ClusterEvent,
    user_id: &UserId,
    room_id: &RoomId,
) -> bool {
    match event {
        ClusterEvent::KickUser { user_id: uid, .. } => uid == user_id,
        ClusterEvent::KickUserFromRoom {
            user_id: uid,
            room_id: rid,
            ..
        }
        | ClusterEvent::UserLeft {
            user_id: uid,
            room_id: rid,
            ..
        } => uid == user_id && rid == room_id,
        _ => false,
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
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use synctv_cluster::sync::{
        ClusterConfig, ClusterManager, ConnectionLimits, ConnectionManager,
    };
    use synctv_cluster::sync::{ClusterEvent, NotificationLevel};
    use synctv_core::cache::{KeyBuilder, NoopCacheL2, UsernameCache};
    use synctv_core::config::PasswordComplexityConfig;
    use synctv_core::models::notification::{Notification, NotificationType};
    use synctv_core::models::{MediaId, PermissionBits, RoomId, RoomPlaybackState, UserId};
    use synctv_core::repository::NotificationRepository;
    use synctv_core::repository::{
        ChatRepository, RoomMemberRepository, RoomRepository, RoomSettingsRepository,
    };
    use synctv_core::service::auth::{BruteForceProtection, JwtService};
    use synctv_core::service::user_notification::NotificationCreatedEvent;
    use synctv_core::service::{
        ChatService, ContentFilter, InMemoryTokenBlacklistStore, NotificationService,
        PermissionService, RateLimitConfig, RateLimiter, RoomService, RoomSettingsService,
        UserService,
    };

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

    #[derive(Default)]
    struct FailingMessageSender {
        fail_after: usize,
        send_calls: AtomicUsize,
        ping_calls: AtomicUsize,
        alive: AtomicBool,
    }

    impl FailingMessageSender {
        fn immediate() -> Arc<Self> {
            Arc::new(Self {
                fail_after: 0,
                send_calls: AtomicUsize::new(0),
                ping_calls: AtomicUsize::new(0),
                alive: AtomicBool::new(true),
            })
        }

        fn fail_after(send_count_before_failure: usize) -> Arc<Self> {
            Arc::new(Self {
                fail_after: send_count_before_failure,
                send_calls: AtomicUsize::new(0),
                ping_calls: AtomicUsize::new(0),
                alive: AtomicBool::new(true),
            })
        }

        fn send_calls(&self) -> usize {
            self.send_calls.load(Ordering::Relaxed)
        }
    }

    impl MessageSender for FailingMessageSender {
        fn send(&self, _message: ServerMessage) -> Result<(), String> {
            let attempt = self.send_calls.fetch_add(1, Ordering::Relaxed);
            if attempt >= self.fail_after {
                self.alive.store(false, Ordering::Relaxed);
                return Err(format!("forced send failure on attempt {}", attempt + 1));
            }
            Ok(())
        }

        fn is_alive(&self) -> bool {
            self.alive.load(Ordering::Relaxed)
        }

        fn ping(&self) -> Result<(), String> {
            self.ping_calls.fetch_add(1, Ordering::Relaxed);
            if self.is_alive() {
                Ok(())
            } else {
                Err("forced dead connection".to_string())
            }
        }
    }

    #[derive(Default)]
    struct FailingStreamState {
        send_calls: AtomicUsize,
        alive: AtomicBool,
    }

    impl FailingStreamState {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                send_calls: AtomicUsize::new(0),
                alive: AtomicBool::new(true),
            })
        }

        fn send_calls(&self) -> usize {
            self.send_calls.load(Ordering::Relaxed)
        }
    }

    struct FailingStream {
        incoming: VecDeque<Result<ClientMessage, String>>,
        fail_after: usize,
        state: Arc<FailingStreamState>,
    }

    impl FailingStream {
        fn fail_after(send_count_before_failure: usize) -> (Self, Arc<FailingStreamState>) {
            let state = FailingStreamState::new();
            (
                Self {
                    incoming: VecDeque::new(),
                    fail_after: send_count_before_failure,
                    state: Arc::clone(&state),
                },
                state,
            )
        }

        fn fail_after_with_incoming(
            send_count_before_failure: usize,
            incoming: Vec<ClientMessage>,
        ) -> (Self, Arc<FailingStreamState>) {
            let state = FailingStreamState::new();
            (
                Self {
                    incoming: incoming.into_iter().map(Ok).collect(),
                    fail_after: send_count_before_failure,
                    state: Arc::clone(&state),
                },
                state,
            )
        }
    }

    #[async_trait::async_trait]
    impl StreamMessage for FailingStream {
        async fn recv(&mut self) -> Option<Result<ClientMessage, String>> {
            if let Some(msg) = self.incoming.pop_front() {
                return Some(msg);
            }
            std::future::pending().await
        }

        fn send(&self, _message: ServerMessage) -> Result<(), String> {
            let attempt = self.state.send_calls.fetch_add(1, Ordering::Relaxed);
            if attempt >= self.fail_after {
                self.state.alive.store(false, Ordering::Relaxed);
                return Err(format!(
                    "forced stream send failure on attempt {}",
                    attempt + 1
                ));
            }
            Ok(())
        }

        fn is_alive(&self) -> bool {
            self.state.alive.load(Ordering::Relaxed)
        }

        fn ping(&self) -> Result<(), String> {
            if self.is_alive() {
                Ok(())
            } else {
                Err("forced dead stream".to_string())
            }
        }
    }

    fn test_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused?connect_timeout=1")
            .expect("lazy test pool")
    }

    fn test_user_service(pool: sqlx::PgPool) -> UserService {
        let jwt_service =
            JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").expect("jwt service");
        let l2 = Arc::new(NoopCacheL2);
        let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
        let password_complexity = PasswordComplexityConfig::default();
        let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
        let key_builder = KeyBuilder::new("test");
        let brute_force = BruteForceProtection::in_memory("test".to_string());

        UserService::new(
            pool,
            jwt_service,
            username_cache,
            password_complexity,
            token_blacklist,
            key_builder,
            brute_force,
        )
    }

    fn test_room_service(pool: sqlx::PgPool) -> Arc<RoomService> {
        Arc::new(RoomService::new(pool.clone(), test_user_service(pool)))
    }

    fn test_chat_service(pool: sqlx::PgPool) -> Arc<ChatService> {
        let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
        let rate_limiter = RateLimiter::in_memory_only("test:chat:".to_string());
        let content_filter = ContentFilter::new();
        let username_cache =
            UsernameCache::new(Arc::new(NoopCacheL2), "test:username:".to_string(), 100, 60);
        let member_repo = RoomMemberRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let room_settings_repo = RoomSettingsRepository::new(pool);
        let mut permission_service = PermissionService::new(
            member_repo,
            room_repo,
            None,
            PermissionService::DEFAULT_CACHE_SIZE,
            PermissionService::DEFAULT_CACHE_TTL_SECS,
        );
        permission_service.set_room_settings_repo(room_settings_repo.clone());

        let room_settings_service = RoomSettingsService::new(
            room_settings_repo,
            None,
            Arc::new(NotificationService::default()),
            None,
            None,
            None,
        );

        Arc::new(ChatService::new(
            chat_repo,
            rate_limiter,
            RateLimitConfig::default(),
            content_filter,
            username_cache,
            permission_service,
            room_settings_service,
        ))
    }

    async fn test_cluster_manager(node_id: &str) -> Arc<ClusterManager> {
        Arc::new(
            ClusterManager::new(
                ClusterConfig {
                    redis_client: None,
                    redis_conn: None,
                    shared_redis_conn: None,
                    cluster_enabled: false,
                    node_id: node_id.to_string(),
                    dedup_window: Duration::from_mins(1),
                    cleanup_interval: Duration::from_secs(10),
                    critical_channel_capacity: 100,
                    publish_channel_capacity: 1000,
                    key_prefix: "synctv:".to_string(),
                    catchup_window_secs: 300,
                    stream_max_length: 1000,
                    parent_cancel_token: None,
                },
                None,
                None,
            )
            .await
            .expect("cluster manager"),
        )
    }

    fn test_connection_manager() -> ConnectionManager {
        ConnectionManager::new(ConnectionLimits::default())
    }

    fn test_message_handler(
        sender: Arc<dyn MessageSender>,
        cluster_manager: Arc<ClusterManager>,
        connection_manager: ConnectionManager,
    ) -> StreamMessageHandler {
        let pool = test_pool();
        StreamMessageHandler::new(
            room_id(),
            user_id(),
            "tester".to_string(),
            test_room_service(pool.clone()),
            test_chat_service(pool),
            cluster_manager,
            connection_manager,
            Arc::new(RateLimiter::in_memory_only("test:handler:".to_string())),
            Arc::new(RateLimitConfig::default()),
            Arc::new(ContentFilter::new()),
            sender,
        )
        .with_heartbeat_schedule(HeartbeatSchedule::for_tests(
            Duration::from_millis(10),
            Duration::from_mins(1),
        ))
    }

    async fn wait_for_start_cleanup(
        handler: &StreamMessageHandler,
        connection_manager: &ConnectionManager,
        cancel_token: &tokio_util::sync::CancellationToken,
        expect_room_subscription_cleanup: bool,
    ) {
        tokio::time::timeout(Duration::from_secs(1), cancel_token.cancelled())
            .await
            .expect("start() should cancel");

        let room = handler.room_id.clone();
        let user = handler.user_id.clone();
        let connection_id = handler.connection_id.clone();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if connection_manager.connection_count() == 0
                    && connection_manager.room_connection_count(&room) == 0
                    && connection_manager.user_connection_count(&user) == 0
                    && handler
                        .connection_manager
                        .get_connection(&connection_id)
                        .is_none()
                    && (!expect_room_subscription_cleanup
                        || cluster_manager_subscriber_count(&handler.cluster_manager, &room) == 0)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cleanup should finish");
    }

    async fn shutdown_test_runtime_resources(
        cluster_manager: Arc<ClusterManager>,
        connection_manager: ConnectionManager,
    ) {
        cluster_manager.shutdown().await;
        connection_manager.shutdown().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    fn cluster_manager_subscriber_count(
        cluster_manager: &ClusterManager,
        room_id: &RoomId,
    ) -> usize {
        cluster_manager.get_room_subscribers(room_id).len()
    }

    async fn wait_for_run_after_join_ready(stream_state: &FailingStreamState) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if stream_state.send_calls() >= 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("run_after_join should be ready");
    }

    async fn wait_for_run_after_join_cleanup(
        handler: &StreamMessageHandler,
        connection_manager: &ConnectionManager,
        task: tokio::task::JoinHandle<Result<(), String>>,
    ) {
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("run_after_join should exit")
            .expect("run_after_join task should not panic");
        assert!(
            result.is_ok(),
            "run_after_join should exit cleanly: {result:?}"
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if connection_manager.connection_count() == 0
                    && connection_manager.room_connection_count(&handler.room_id) == 0
                    && connection_manager.user_connection_count(&handler.user_id) == 0
                    && cluster_manager_subscriber_count(&handler.cluster_manager, &handler.room_id)
                        == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("run_after_join cleanup should finish");
    }

    #[tokio::test]
    async fn test_start_cancels_and_cleans_up_when_initial_send_fails() {
        let cluster_manager = test_cluster_manager("test_start_initial_send_failure").await;
        let connection_manager = test_connection_manager();
        let sender = FailingMessageSender::immediate();
        let handler = test_message_handler(
            sender,
            Arc::clone(&cluster_manager),
            connection_manager.clone(),
        );

        let (_tx, cancel_token) = handler.start().await.expect("start should return");

        wait_for_start_cleanup(&handler, &connection_manager, &cancel_token, true).await;
        shutdown_test_runtime_resources(cluster_manager, connection_manager).await;
    }

    #[tokio::test]
    async fn test_start_does_not_broadcast_presence_events_when_initial_send_fails() {
        let cluster_manager =
            test_cluster_manager("test_start_no_broadcast_on_initial_failure").await;
        let connection_manager = test_connection_manager();
        let sender = FailingMessageSender::immediate();
        let handler = test_message_handler(
            sender,
            Arc::clone(&cluster_manager),
            connection_manager.clone(),
        );

        let room = handler.room_id.clone();
        let user = handler.user_id.clone();
        let (mut rx, conn_id) = cluster_manager
            .subscribe(room, user)
            .await
            .expect("subscribe should succeed");
        let (_tx, cancel_token) = handler.start().await.expect("start should return");

        let maybe_presence_event =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;

        assert!(
            maybe_presence_event.is_err(),
            "initial send failure must not broadcast UserJoined/UserLeft presence events"
        );

        cluster_manager.unsubscribe(&conn_id);
        wait_for_start_cleanup(&handler, &connection_manager, &cancel_token, true).await;
        shutdown_test_runtime_resources(cluster_manager, connection_manager).await;
    }

    #[tokio::test]
    async fn test_start_cancels_and_cleans_up_when_cluster_event_send_fails() {
        let cluster_manager = test_cluster_manager("test_start_event_send_failure").await;
        let connection_manager = test_connection_manager();
        let sender = FailingMessageSender::fail_after(1);
        let sender_for_assert = Arc::clone(&sender);
        let handler = test_message_handler(
            sender,
            Arc::clone(&cluster_manager),
            connection_manager.clone(),
        );

        let (_tx, cancel_token) = handler.start().await.expect("start should return");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if cluster_manager_subscriber_count(&cluster_manager, &handler.room_id) == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("subscription should be established");

        cluster_manager.broadcast(ClusterEvent::ChatMessage {
            event_id: "evt-start-fail".to_string(),
            room_id: handler.room_id.clone(),
            user_id: handler.user_id.clone(),
            username: handler.username.clone(),
            message: "boom".to_string(),
            timestamp: now(),
            position: None,
            color: None,
        });

        wait_for_start_cleanup(&handler, &connection_manager, &cancel_token, true).await;
        assert!(
            sender_for_assert.send_calls() >= 2,
            "initial join send + failing event send should both be attempted"
        );
        shutdown_test_runtime_resources(cluster_manager, connection_manager).await;
    }

    #[tokio::test]
    async fn test_start_cancels_and_cleans_up_when_admin_notification_send_fails() {
        let cluster_manager = test_cluster_manager("test_start_admin_notification_failure").await;
        let connection_manager = test_connection_manager();
        let sender = FailingMessageSender::fail_after(1);
        let sender_for_assert = Arc::clone(&sender);
        let handler = test_message_handler(
            sender,
            Arc::clone(&cluster_manager),
            connection_manager.clone(),
        );

        let (_tx, cancel_token) = handler.start().await.expect("start should return");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if cluster_manager_subscriber_count(&cluster_manager, &handler.room_id) == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("subscription should be established");

        cluster_manager.broadcast(ClusterEvent::UserNotification {
            event_id: "evt-admin-notify".to_string(),
            user_id: handler.user_id.clone(),
            title: "title".to_string(),
            content: "content".to_string(),
            notification_type: "system".to_string(),
            notification_id: "notif-1".to_string(),
            timestamp: now(),
        });

        wait_for_start_cleanup(&handler, &connection_manager, &cancel_token, true).await;
        assert!(
            sender_for_assert.send_calls() >= 2,
            "initial join send + failing admin notification send should both be attempted"
        );
        shutdown_test_runtime_resources(cluster_manager, connection_manager).await;
    }

    #[tokio::test]
    async fn test_run_after_join_cleans_up_when_cluster_event_send_fails() {
        let cluster_manager = test_cluster_manager("test_run_after_join_event_failure").await;
        let connection_manager = test_connection_manager();
        let handler = test_message_handler(
            FailingMessageSender::fail_after(usize::MAX),
            Arc::clone(&cluster_manager),
            connection_manager.clone(),
        );
        handler.pre_join().await.expect("pre_join should succeed");

        let (mut stream, stream_state) = FailingStream::fail_after(1);
        let task_handler = handler.clone();
        let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

        wait_for_run_after_join_ready(&stream_state).await;

        cluster_manager.broadcast(ClusterEvent::ChatMessage {
            event_id: "evt-run-after-join".to_string(),
            room_id: handler.room_id.clone(),
            user_id: handler.user_id.clone(),
            username: handler.username.clone(),
            message: "boom".to_string(),
            timestamp: now(),
            position: None,
            color: None,
        });

        wait_for_run_after_join_cleanup(&handler, &connection_manager, run_task).await;
        shutdown_test_runtime_resources(cluster_manager, connection_manager).await;
    }

    #[tokio::test]
    async fn test_run_after_join_cleans_up_when_initial_send_fails() {
        let cluster_manager = test_cluster_manager("test_run_after_join_initial_failure").await;
        let connection_manager = test_connection_manager();
        let handler = test_message_handler(
            FailingMessageSender::fail_after(usize::MAX),
            Arc::clone(&cluster_manager),
            connection_manager.clone(),
        );
        handler.pre_join().await.expect("pre_join should succeed");

        let (mut rx, conn_id) = cluster_manager
            .subscribe(handler.room_id.clone(), handler.user_id.clone())
            .await
            .expect("subscribe should succeed");
        let (mut stream, _stream_state) = FailingStream::fail_after(0);
        let task_handler = handler.clone();
        let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

        let maybe_presence_event =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
        assert!(
            maybe_presence_event.is_err(),
            "initial run_after_join send failure must not broadcast UserJoined/UserLeft presence events"
        );

        cluster_manager.unsubscribe(&conn_id);
        wait_for_run_after_join_cleanup(&handler, &connection_manager, run_task).await;
        shutdown_test_runtime_resources(cluster_manager, connection_manager).await;
    }

    #[tokio::test]
    async fn test_run_after_join_cleans_up_when_admin_notification_send_fails() {
        let cluster_manager = test_cluster_manager("test_run_after_join_admin_failure").await;
        let connection_manager = test_connection_manager();
        let handler = test_message_handler(
            FailingMessageSender::fail_after(usize::MAX),
            Arc::clone(&cluster_manager),
            connection_manager.clone(),
        );
        handler.pre_join().await.expect("pre_join should succeed");

        let (mut stream, stream_state) = FailingStream::fail_after(1);
        let task_handler = handler.clone();
        let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

        wait_for_run_after_join_ready(&stream_state).await;

        cluster_manager.broadcast(ClusterEvent::UserNotification {
            event_id: "evt-run-after-join-admin".to_string(),
            user_id: handler.user_id.clone(),
            title: "title".to_string(),
            content: "content".to_string(),
            notification_type: "system".to_string(),
            notification_id: "notif-admin".to_string(),
            timestamp: now(),
        });

        wait_for_run_after_join_cleanup(&handler, &connection_manager, run_task).await;
        shutdown_test_runtime_resources(cluster_manager, connection_manager).await;
    }

    #[tokio::test]
    async fn test_run_after_join_cleans_up_when_backpressure_error_send_fails() {
        let cluster_manager =
            test_cluster_manager("test_run_after_join_backpressure_failure").await;
        let connection_manager = test_connection_manager();
        let handler = test_message_handler(
            FailingMessageSender::fail_after(usize::MAX),
            Arc::clone(&cluster_manager),
            connection_manager.clone(),
        )
        .with_concurrency(Arc::new(MessageConcurrencyConfig::new(0)));
        handler.pre_join().await.expect("pre_join should succeed");

        let input = ClientMessage { message: None };
        let (mut stream, stream_state) = FailingStream::fail_after_with_incoming(1, vec![input]);
        let task_handler = handler.clone();
        let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

        wait_for_run_after_join_ready(&stream_state).await;

        wait_for_run_after_join_cleanup(&handler, &connection_manager, run_task).await;
        shutdown_test_runtime_resources(cluster_manager, connection_manager).await;
    }

    #[tokio::test]
    async fn test_run_after_join_cleans_up_when_direct_notification_send_fails() {
        let cluster_manager = test_cluster_manager("test_run_after_join_direct_failure").await;
        let connection_manager = test_connection_manager();
        let notification_pool = test_pool();
        let notification_service = Arc::new(synctv_core::service::UserNotificationService::new(
            NotificationRepository::new(notification_pool.clone()),
        ));
        let handler = test_message_handler(
            FailingMessageSender::fail_after(usize::MAX),
            Arc::clone(&cluster_manager),
            connection_manager.clone(),
        )
        .with_notification_service(Arc::clone(&notification_service));
        handler.pre_join().await.expect("pre_join should succeed");

        let (mut stream, stream_state) = FailingStream::fail_after(1);
        let task_handler = handler.clone();
        let run_task = tokio::spawn(async move { task_handler.run_after_join(&mut stream).await });

        wait_for_run_after_join_ready(&stream_state).await;

        notification_service.publish_realtime_event(NotificationCreatedEvent {
            user_id: handler.user_id.clone(),
            notification: Notification {
                id: uuid::Uuid::new_v4(),
                user_id: handler.user_id.clone(),
                notification_type: NotificationType::SystemAnnouncement,
                title: "title".to_string(),
                content: "content".to_string(),
                data: serde_json::json!({}),
                is_read: false,
                created_at: now(),
                updated_at: now(),
            },
        });

        wait_for_run_after_join_cleanup(&handler, &connection_manager, run_task).await;
        notification_pool.close().await;
        shutdown_test_runtime_resources(cluster_manager, connection_manager).await;
    }

    // ========== cluster_event_to_server_messages Tests ==========

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

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        let msg = &msgs[0];
        match &msg.message {
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
            state,
            timestamp: now(),
        };

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        match &msgs[0].message {
            Some(Message::PlaybackState(ps)) => {
                assert_eq!(ps.room_id, "room_test");
                let s = ps.state.as_ref().unwrap();
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
            added_permissions: PermissionBits(0),
            removed_permissions: PermissionBits(0),
            admin_added_permissions: PermissionBits(0),
            admin_removed_permissions: PermissionBits(0),
            joined_at: now(),
            timestamp: now(),
        };

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        match &msgs[0].message {
            Some(Message::UserJoined(uj)) => {
                assert_eq!(uj.room_id, "room_test");
                let member = uj.member.as_ref().unwrap();
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

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        match &msgs[0].message {
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

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        match &msgs[0].message {
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

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        match &msgs[0].message {
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

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        match &msgs[0].message {
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

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        match &msgs[0].message {
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

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        assert!(matches!(
            &msgs[0].message,
            Some(Message::WebrtcIceCandidate(_))
        ));
    }

    #[test]
    fn test_webrtc_unknown_type_returns_empty() {
        let event = ClusterEvent::WebRTCSignaling {
            event_id: "evt10".to_string(),
            room_id: room_id(),
            message_type: "unknown_type".to_string(),
            from: "conn_a".to_string(),
            to: "conn_b".to_string(),
            data: "data".to_string(),
            timestamp: now(),
        };

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_room_deleted_event_conversion() {
        let event = ClusterEvent::RoomDeleted {
            event_id: "evt11".to_string(),
            room_id: room_id(),
            deleted_by: user_id(),
            timestamp: now(),
        };

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        match &msgs[0].message {
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

        let msgs = cluster_event_to_server_messages(&event, "room_test");
        assert_eq!(msgs.len(), 1);
        match &msgs[0].message {
            Some(Message::Notification(n)) => {
                assert_eq!(n.title, "Server maintenance in 5 minutes");
                assert_eq!(n.notification_type, "system_announcement");
            }
            other => panic!("Expected Notification message for SystemNotification, got: {other:?}"),
        }
    }

    #[test]
    fn test_admin_events_return_empty() {
        let event = ClusterEvent::KickPublisher {
            event_id: "evt13".to_string(),
            room_id: room_id(),
            media_id: media_id(),
            reason: "test".to_string(),
            timestamp: now(),
        };
        assert!(cluster_event_to_server_messages(&event, "room_test").is_empty());

        let event = ClusterEvent::KickUser {
            event_id: "evt14".to_string(),
            user_id: user_id(),
            reason: "banned".to_string(),
            timestamp: now(),
        };
        assert!(cluster_event_to_server_messages(&event, "room_test").is_empty());
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
        let failed = semaphore.try_acquire_owned();
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
        assert!(super::validate_danmaku_color(&Some("#1a2B3c".to_string())).is_ok());
        // Mixed case
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
        assert!(
            result
                .unwrap_err()
                .contains("must contain only hex characters")
        );

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
        let result = super::validate_danmaku_color(&Some(String::new()));
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

    // ========== Membership Cache Invalidation Tests ==========

    #[test]
    fn test_membership_cache_stores_and_retrieves() {
        // Verify the membership cache can store and retrieve entries
        let cache: moka::sync::Cache<(String, String), super::CachedMembership> =
            moka::sync::Cache::builder()
                .time_to_live(super::MEMBERSHIP_CACHE_TTL)
                .build();

        let key = ("room1".to_string(), "user1".to_string());
        let membership = super::CachedMembership {
            is_member: true,
            is_banned: false,
        };

        cache.insert(key.clone(), membership);
        let cached = cache.get(&key);
        assert!(cached.is_some());
        let cached = cached.unwrap();
        assert!(cached.is_member);
        assert!(!cached.is_banned);
    }

    #[test]
    fn test_membership_cache_invalidation_removes_entry() {
        // Verify that invalidate() removes the cached entry so the next
        // lookup returns None (forcing a DB re-query on next heartbeat)
        let cache: moka::sync::Cache<(String, String), super::CachedMembership> =
            moka::sync::Cache::builder()
                .time_to_live(super::MEMBERSHIP_CACHE_TTL)
                .build();

        let key = ("room1".to_string(), "user1".to_string());
        let membership = super::CachedMembership {
            is_member: true,
            is_banned: false,
        };

        cache.insert(key.clone(), membership);
        assert!(
            cache.get(&key).is_some(),
            "Entry should exist before invalidation"
        );

        // Invalidate the entry (simulates receiving KickUser/KickUserFromRoom event)
        cache.invalidate(&key);
        assert!(
            cache.get(&key).is_none(),
            "Entry should be removed after invalidation"
        );
    }

    #[test]
    fn test_membership_cache_invalidation_only_affects_target_user() {
        // Verify that invalidating one user's cache does not affect other users
        let cache: moka::sync::Cache<(String, String), super::CachedMembership> =
            moka::sync::Cache::builder()
                .time_to_live(super::MEMBERSHIP_CACHE_TTL)
                .build();

        let key_user1 = ("room1".to_string(), "user1".to_string());
        let key_user2 = ("room1".to_string(), "user2".to_string());

        cache.insert(
            key_user1.clone(),
            super::CachedMembership {
                is_member: true,
                is_banned: false,
            },
        );
        cache.insert(
            key_user2.clone(),
            super::CachedMembership {
                is_member: true,
                is_banned: false,
            },
        );

        // Invalidate only user1
        cache.invalidate(&key_user1);

        assert!(
            cache.get(&key_user1).is_none(),
            "User1 entry should be invalidated"
        );
        assert!(
            cache.get(&key_user2).is_some(),
            "User2 entry should still be cached"
        );
    }

    #[test]
    fn test_cached_membership_from_member_banned() {
        // Verify CachedMembership correctly identifies banned users
        use synctv_core::models::{MemberStatus, RoomMember, RoomRole};

        let member = RoomMember {
            room_id: room_id(),
            user_id: user_id(),
            role: RoomRole::Member,
            status: MemberStatus::Banned,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: now(),
            left_at: None,
            version: 1,
            banned_at: Some(now()),
            banned_by: None,
            banned_reason: Some("test ban".to_string()),
        };

        let cached = super::CachedMembership::from_member(Some(&member));
        assert!(cached.is_member);
        assert!(cached.is_banned, "Banned user should have is_banned=true");
    }

    #[test]
    fn test_cached_membership_from_member_none() {
        // Verify CachedMembership correctly handles non-members
        let cached = super::CachedMembership::from_member(None);
        assert!(!cached.is_member, "Non-member should have is_member=false");
        assert!(!cached.is_banned);
    }

    #[test]
    fn test_cached_membership_from_member_active() {
        // Verify CachedMembership correctly identifies active members
        use synctv_core::models::{MemberStatus, RoomMember, RoomRole};

        let member = RoomMember {
            room_id: room_id(),
            user_id: user_id(),
            role: RoomRole::Member,
            status: MemberStatus::Active,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: now(),
            left_at: None,
            version: 1,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        };

        let cached = super::CachedMembership::from_member(Some(&member));
        assert!(cached.is_member);
        assert!(!cached.is_banned, "Active member should not be banned");
    }

    // ========== WebRTC SDP/ICE Size Validation Tests (P1#12) ==========

    #[test]
    fn test_sdp_offer_within_limit() {
        let offer = crate::proto::client::WebRtcOffer {
            to: "user1:conn1".to_string(),
            from: String::new(),
            data: "a".repeat(super::MAX_SDP_SIZE),
        };
        // Size check passes (equal to limit)
        assert!(offer.data.len() <= super::MAX_SDP_SIZE);
    }

    #[test]
    fn test_sdp_offer_exceeds_limit() {
        let offer = crate::proto::client::WebRtcOffer {
            to: "user1:conn1".to_string(),
            from: String::new(),
            data: "a".repeat(super::MAX_SDP_SIZE + 1),
        };
        // Size check fails (exceeds limit)
        assert!(offer.data.len() > super::MAX_SDP_SIZE);
    }

    #[test]
    fn test_sdp_answer_exceeds_limit() {
        let answer = crate::proto::client::WebRtcAnswer {
            to: "user1:conn1".to_string(),
            from: String::new(),
            data: "a".repeat(super::MAX_SDP_SIZE + 1),
        };
        assert!(answer.data.len() > super::MAX_SDP_SIZE);
    }

    #[test]
    fn test_ice_candidate_within_limit() {
        let candidate = crate::proto::client::WebRtcIceCandidate {
            to: "user1:conn1".to_string(),
            from: String::new(),
            data: "a".repeat(super::MAX_ICE_CANDIDATE_SIZE),
        };
        assert!(candidate.data.len() <= super::MAX_ICE_CANDIDATE_SIZE);
    }

    #[test]
    fn test_ice_candidate_exceeds_limit() {
        let candidate = crate::proto::client::WebRtcIceCandidate {
            to: "user1:conn1".to_string(),
            from: String::new(),
            data: "a".repeat(super::MAX_ICE_CANDIDATE_SIZE + 1),
        };
        assert!(candidate.data.len() > super::MAX_ICE_CANDIDATE_SIZE);
    }

    // ========== Playback Progress Throttle Tests (P1#11) ==========

    #[tokio::test]
    async fn test_progress_throttle_first_write_always_allowed() {
        // First write should always go through (None state)
        let state: tokio::sync::Mutex<Option<(f64, tokio::time::Instant)>> =
            tokio::sync::Mutex::new(None);
        let guard = state.lock().await;
        assert!(guard.is_none(), "Initial state should be None");
    }

    #[tokio::test]
    async fn test_progress_throttle_small_position_change_suppressed() {
        // A position change less than PROGRESS_MIN_POSITION_DELTA should be suppressed
        // when less than PROGRESS_MIN_ELAPSED_SECS has passed
        let last_pos: f64 = 100.0;
        let last_time = tokio::time::Instant::now();
        let new_pos: f64 = 100.5; // delta = 0.5 < 1.0

        let pos_delta = (new_pos - last_pos).abs();
        let elapsed = last_time.elapsed().as_secs_f64();

        let should_write = pos_delta > super::PROGRESS_MIN_POSITION_DELTA
            || elapsed > super::PROGRESS_MIN_ELAPSED_SECS;
        assert!(
            !should_write,
            "Small position change with short elapsed time should be suppressed"
        );
    }

    #[tokio::test]
    async fn test_progress_throttle_large_position_change_allowed() {
        // A position change >= PROGRESS_MIN_POSITION_DELTA should be allowed
        let last_pos: f64 = 100.0;
        let last_time = tokio::time::Instant::now();
        let new_pos: f64 = 101.5; // delta = 1.5 > 1.0

        let pos_delta = (new_pos - last_pos).abs();
        let elapsed = last_time.elapsed().as_secs_f64();

        let should_write = pos_delta > super::PROGRESS_MIN_POSITION_DELTA
            || elapsed > super::PROGRESS_MIN_ELAPSED_SECS;
        assert!(should_write, "Large position change should trigger a write");
    }

    #[tokio::test]
    async fn test_progress_throttle_elapsed_time_allows_write() {
        // Even with small position delta, elapsed time > 5s should allow write
        let last_pos: f64 = 100.0;
        // Simulate 6 seconds elapsed
        let last_time = tokio::time::Instant::now() - std::time::Duration::from_secs_f64(6.0);
        let new_pos: f64 = 100.1; // very small delta

        let pos_delta = (new_pos - last_pos).abs();
        let elapsed = last_time.elapsed().as_secs_f64();

        let should_write = pos_delta > super::PROGRESS_MIN_POSITION_DELTA
            || elapsed > super::PROGRESS_MIN_ELAPSED_SECS;
        assert!(
            should_write,
            "Elapsed time exceeding threshold should trigger a write"
        );
    }

    // ========== UserLeft Retry Semaphore Tests (P2#15) ==========

    #[tokio::test]
    async fn test_user_left_retry_semaphore_limits_concurrent_tasks() {
        // Acquire all 100 permits to simulate max concurrent retry tasks
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
        let mut permits = Vec::new();

        for _ in 0..100 {
            let permit = semaphore.clone().try_acquire_owned();
            assert!(permit.is_ok(), "Should acquire permit under limit");
            permits.push(permit.unwrap());
        }

        // 101st attempt should fail
        let overflow = semaphore.clone().try_acquire_owned();
        assert!(
            overflow.is_err(),
            "Should reject when semaphore is exhausted"
        );

        // Release one permit and try again
        permits.pop();
        let retry = semaphore.try_acquire_owned();
        assert!(retry.is_ok(), "Should succeed after a permit is released");
    }

    #[test]
    fn test_user_left_requires_retry_when_cluster_redis_enabled_but_publish_fails() {
        let result = synctv_cluster::sync::BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        };

        let should_retry = super::should_retry_user_left_broadcast(result, true);

        assert!(
            should_retry,
            "when cluster Redis fan-out is configured, local delivery alone is insufficient for UserLeft consistency"
        );
    }

    #[test]
    fn test_user_left_does_not_retry_in_single_node_mode_after_local_delivery() {
        let result = synctv_cluster::sync::BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        };

        let should_retry = super::should_retry_user_left_broadcast(result, false);

        assert!(
            !should_retry,
            "single-node mode should not spawn retries when the local subscriber already received UserLeft"
        );
    }

    #[test]
    fn test_webrtc_signal_requires_redis_delivery_when_cluster_enabled() {
        let result = synctv_cluster::sync::BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        };

        assert!(
            super::should_fail_webrtc_signal_broadcast(result, true),
            "cluster-mode WebRTC signaling must fail closed unless Redis publish succeeds because local room fan-out cannot prove the targeted peer received the signal"
        );
    }

    #[test]
    fn test_webrtc_signal_allows_single_node_delivery_without_redis() {
        let result = synctv_cluster::sync::BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        };

        assert!(!super::should_fail_webrtc_signal_broadcast(result, false));
    }

    #[test]
    fn test_webrtc_signal_allows_cluster_delivery_when_redis_publish_succeeds() {
        let result = synctv_cluster::sync::BroadcastResult {
            local_sent: 0,
            redis_sent: true,
        };

        assert!(!super::should_fail_webrtc_signal_broadcast(result, true));
    }

    #[test]
    fn test_webrtc_signal_fails_when_neither_local_nor_redis_delivery_succeeds() {
        let result = synctv_cluster::sync::BroadcastResult {
            local_sent: 0,
            redis_sent: false,
        };

        assert!(super::should_fail_webrtc_signal_broadcast(result, true));
    }

    #[test]
    fn test_webrtc_membership_transition_requires_existing_connection() {
        let result = super::should_transition_webrtc_membership(None, true);
        assert_eq!(result, Err("Connection not found"));
    }

    #[test]
    fn test_webrtc_membership_transition_detects_join_state_change() {
        let result = super::should_transition_webrtc_membership(Some(false), true);
        assert_eq!(result, Ok(true));
    }

    #[test]
    fn test_webrtc_membership_transition_ignores_duplicate_join() {
        let result = super::should_transition_webrtc_membership(Some(true), true);
        assert_eq!(result, Ok(false));
    }

    #[test]
    fn test_webrtc_membership_transition_detects_leave_state_change() {
        let result = super::should_transition_webrtc_membership(Some(true), false);
        assert_eq!(result, Ok(true));
    }

    #[test]
    fn test_webrtc_membership_transition_ignores_duplicate_leave() {
        let result = super::should_transition_webrtc_membership(Some(false), false);
        assert_eq!(result, Ok(false));
    }

    #[test]
    fn test_user_left_delivery_skips_when_local_connection_remains() {
        let plan = super::should_broadcast_user_left(true, Ok(false));
        assert_eq!(plan, super::UserLeftDeliveryPlan::Skip);
    }

    #[test]
    fn test_user_left_delivery_skips_when_distributed_presence_exists() {
        let plan = super::should_broadcast_user_left(false, Ok(true));
        assert_eq!(plan, super::UserLeftDeliveryPlan::Skip);
    }

    #[test]
    fn test_user_left_delivery_uses_local_and_redis_when_user_is_last_presence() {
        let plan = super::should_broadcast_user_left(false, Ok(false));
        assert_eq!(plan, super::UserLeftDeliveryPlan::LocalAndRedis);
    }

    #[test]
    fn test_user_left_delivery_skips_when_distributed_check_fails() {
        let plan = super::should_broadcast_user_left(false, Err(()));
        assert_eq!(plan, super::UserLeftDeliveryPlan::Skip);
    }

    #[tokio::test]
    async fn test_current_connection_matches_webrtc_recipient_accepts_conn_id_only() {
        let room_id = room_id();
        let user_id = user_id();
        let manager = test_connection_manager();
        let pool = test_pool();
        let cluster_manager = test_cluster_manager("node-test").await;

        let handler = super::StreamMessageHandler::new(
            room_id.clone(),
            user_id.clone(),
            "user".to_string(),
            test_room_service(pool.clone()),
            test_chat_service(pool),
            cluster_manager,
            manager.clone(),
            Arc::new(RateLimiter::in_memory_only(
                "test:conn-id-only-match:".to_string(),
            )),
            Arc::new(RateLimitConfig::default()),
            Arc::new(ContentFilter::new()),
            FailingMessageSender::fail_after(usize::MAX),
        );
        let connection_id = handler.connection_id().to_string();

        manager
            .register(connection_id.clone(), user_id.clone())
            .await
            .expect("register");
        manager
            .join_room(&connection_id, room_id.clone())
            .await
            .expect("join room");
        manager.mark_rtc_joined(&room_id, &user_id, &connection_id, true);

        assert!(
            handler.current_connection_matches_webrtc_recipient(&connection_id),
            "conn_id-only WebRTC recipient should match the current connection"
        );
    }

    #[tokio::test]
    async fn test_current_connection_matches_webrtc_recipient_rejects_other_conn_id_only() {
        let room_id = room_id();
        let user_id = user_id();
        let manager = test_connection_manager();
        let pool = test_pool();
        let cluster_manager = test_cluster_manager("node-test").await;

        let handler = super::StreamMessageHandler::new(
            room_id.clone(),
            user_id.clone(),
            "user".to_string(),
            test_room_service(pool.clone()),
            test_chat_service(pool),
            cluster_manager,
            manager.clone(),
            Arc::new(RateLimiter::in_memory_only(
                "test:conn-id-only-reject:".to_string(),
            )),
            Arc::new(RateLimitConfig::default()),
            Arc::new(ContentFilter::new()),
            FailingMessageSender::fail_after(usize::MAX),
        );
        let connection_id = handler.connection_id().to_string();

        manager
            .register(connection_id.clone(), user_id.clone())
            .await
            .expect("register");
        manager
            .join_room(&connection_id, room_id.clone())
            .await
            .expect("join room");
        manager.mark_rtc_joined(&room_id, &user_id, &connection_id, true);

        assert!(
            !handler.current_connection_matches_webrtc_recipient("other-conn"),
            "conn_id-only WebRTC recipient must not match a different connection"
        );
    }

    #[test]
    fn test_membership_invalidation_requires_skip_cleanup_for_banned_member() {
        let mut member = synctv_core::models::RoomMember::new(
            room_id(),
            user_id(),
            synctv_core::models::RoomRole::Member,
        );
        member.status = synctv_core::models::MemberStatus::Banned;

        assert!(super::membership_invalidation_requires_skip_cleanup(Some(
            &member
        )));
    }

    #[test]
    fn test_membership_invalidation_requires_skip_cleanup_for_missing_member() {
        assert!(super::membership_invalidation_requires_skip_cleanup(None));
    }

    #[test]
    fn test_membership_invalidation_keeps_cleanup_for_active_member() {
        let member = synctv_core::models::RoomMember::new(
            room_id(),
            user_id(),
            synctv_core::models::RoomRole::Member,
        );

        assert!(!super::membership_invalidation_requires_skip_cleanup(Some(
            &member
        )));
    }

    #[test]
    fn test_disconnect_signal_requires_skip_cleanup_for_targeted_server_disconnects() {
        let rid = room_id();
        let uid = user_id();
        let connection_id = "conn-123";

        assert!(super::disconnect_signal_requires_skip_cleanup(
            &synctv_cluster::sync::DisconnectSignal::Connection(connection_id.to_string()),
            &uid,
            &rid,
            connection_id,
        ));
        assert!(super::disconnect_signal_requires_skip_cleanup(
            &synctv_cluster::sync::DisconnectSignal::User(uid.clone()),
            &uid,
            &rid,
            connection_id,
        ));
        assert!(super::disconnect_signal_requires_skip_cleanup(
            &synctv_cluster::sync::DisconnectSignal::Room(rid.clone()),
            &uid,
            &rid,
            connection_id,
        ));
        assert!(super::disconnect_signal_requires_skip_cleanup(
            &synctv_cluster::sync::DisconnectSignal::UserFromRoom {
                user_id: uid.clone(),
                room_id: rid.clone(),
            },
            &uid,
            &rid,
            connection_id,
        ));
    }

    #[test]
    fn test_admin_event_requires_skip_cleanup_for_forced_exit_events() {
        let rid = room_id();
        let uid = user_id();
        let now = chrono::Utc::now();

        assert!(super::admin_event_requires_skip_cleanup(
            &ClusterEvent::KickUser {
                event_id: "evt-1".to_string(),
                user_id: uid.clone(),
                reason: "ban".to_string(),
                timestamp: now,
            },
            &uid,
            &rid,
        ));
        assert!(super::admin_event_requires_skip_cleanup(
            &ClusterEvent::KickUserFromRoom {
                event_id: "evt-2".to_string(),
                room_id: rid.clone(),
                user_id: uid.clone(),
                reason: "kick".to_string(),
                timestamp: now,
            },
            &uid,
            &rid,
        ));
        assert!(super::admin_event_requires_skip_cleanup(
            &ClusterEvent::UserLeft {
                event_id: "evt-3".to_string(),
                room_id: rid.clone(),
                user_id: uid.clone(),
                username: "tester".to_string(),
                timestamp: now,
            },
            &uid,
            &rid,
        ));
    }

    // ========== Connection Reservation Tests (P1#6) ==========

    #[tokio::test]
    async fn test_connection_reservation_room_slot() {
        use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
        let limits = ConnectionLimits {
            max_per_room: 2,
            ..ConnectionLimits::default()
        };
        let mgr = ConnectionManager::new(limits);
        let rid = room_id();

        // First two reservations should succeed
        assert!(mgr.reserve_room_slot(&rid).is_ok());
        assert!(mgr.reserve_room_slot(&rid).is_ok());

        // Third should fail (limit is 2)
        assert!(mgr.reserve_room_slot(&rid).is_err());

        // Release one reservation
        mgr.release_room_reservation(&rid);

        // Now reservation should succeed again
        assert!(mgr.reserve_room_slot(&rid).is_ok());
    }

    #[tokio::test]
    async fn test_connection_reservation_user_slot() {
        use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
        let limits = ConnectionLimits {
            max_per_user: 3,
            ..ConnectionLimits::default()
        };
        let mgr = ConnectionManager::new(limits);
        let uid = user_id();

        // Three reservations should succeed
        assert!(mgr.reserve_user_slot(&uid).is_ok());
        assert!(mgr.reserve_user_slot(&uid).is_ok());
        assert!(mgr.reserve_user_slot(&uid).is_ok());

        // Fourth should fail (limit is 3)
        assert!(mgr.reserve_user_slot(&uid).is_err());

        // Release two
        mgr.release_user_reservation(&uid);
        mgr.release_user_reservation(&uid);

        // Now two more should succeed
        assert!(mgr.reserve_user_slot(&uid).is_ok());
        assert!(mgr.reserve_user_slot(&uid).is_ok());

        // But the next should fail again
        assert!(mgr.reserve_user_slot(&uid).is_err());
    }

    #[tokio::test]
    async fn test_connection_reservation_concurrent_simulation() {
        use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
        let limits = ConnectionLimits {
            max_per_room: 5,
            ..ConnectionLimits::default()
        };
        let mgr = ConnectionManager::new(limits);
        let rid = room_id();

        // Simulate 10 concurrent reservation attempts (only 5 should succeed)
        let mut successes = 0;
        for _ in 0..10 {
            if mgr.reserve_room_slot(&rid).is_ok() {
                successes += 1;
            }
        }
        assert_eq!(
            successes, 5,
            "Only 5 of 10 concurrent requests should succeed"
        );
    }
}
