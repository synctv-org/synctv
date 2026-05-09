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
use synctv_cluster::sync::{ClusterEvent, WebRTCSignalKind};
use synctv_common::ExecutionControl;
use synctv_core::spawn::spawn_monitored;
use synctv_core::{
    models::{PermissionBits, RoomId, RoomStatus, UserId, UserStatus},
    service::{
        ChatService, ContentFilter, RateLimitConfig, RequestRateLimiterService, RoomService,
    },
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

const USER_LEFT_RETRY_MAX_RETRIES: u32 = 5;
const USER_LEFT_RETRY_INITIAL_DELAY_MS: u64 = 100;
const USER_LEFT_RETRY_MAX_DELAY_MS: u64 = 5_000;
const PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS: f64 = 30.0;

/// Maximum number of concurrent UserLeft retry tasks across the process.
/// Prevents unbounded task spawning during mass disconnects with Redis down.
static USER_LEFT_RETRY_SEMAPHORE: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(100)));

fn is_private_ice_candidate_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
        }
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

use crate::impls::playback_snapshot::PlaybackSnapshotService;
use crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService;
use crate::impls::room_members_snapshot::RoomMembersSnapshotService;
use crate::impls::room_settings_snapshot::{
    default_room_settings_snapshot_service, RoomSettingsSnapshotService,
};
use crate::proto::client::{ClientMessage, ServerMessage};
#[cfg(test)]
use crate::resource_change::ResourceInvalidation;
use crate::runtime::{
    RealtimeConnectionService, RealtimeDeliveryRequirement, RealtimeEventService,
};

mod resource_observer;
use resource_observer::ResourceObserver;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeJoinError {
    PermissionDenied(String),
    RateLimited(String),
    ServiceUnavailable(String),
    Internal(String),
}

impl RealtimeJoinError {
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::PermissionDenied(message)
            | Self::RateLimited(message)
            | Self::ServiceUnavailable(message)
            | Self::Internal(message) => message,
        }
    }
}

impl From<String> for RealtimeJoinError {
    fn from(message: String) -> Self {
        classify_realtime_join_error_message(message)
    }
}

impl From<RealtimeJoinError> for String {
    fn from(error: RealtimeJoinError) -> Self {
        error.to_string()
    }
}

impl std::fmt::Display for RealtimeJoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for RealtimeJoinError {}

fn classify_realtime_join_error_message(message: String) -> RealtimeJoinError {
    match crate::impls::classify_error(&message) {
        crate::impls::ErrorKind::RateLimited => RealtimeJoinError::RateLimited(message),
        crate::impls::ErrorKind::ServiceUnavailable => {
            RealtimeJoinError::ServiceUnavailable(message)
        }
        crate::impls::ErrorKind::PermissionDenied => RealtimeJoinError::PermissionDenied(message),
        _ => RealtimeJoinError::Internal(message),
    }
}

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
            user_id.as_i64().unsigned_abs() % (self.max_jitter_secs + 1)
        };
        self.base_interval + Duration::from_secs(jitter_secs)
    }
}

// MessageConcurrencyConfig - Instance-level concurrency configuration

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
                is_banned: m.is_banned(),
            },
            None => Self {
                is_member: false,
                is_banned: false,
            },
        }
    }
}

// Re-use the canonical member/role proto mappers from client::convert.
use crate::impls::client::convert::member_status_to_proto;
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
    event_service: Arc<dyn RealtimeEventService>,
    /// Optional notification service for direct real-time push to connected clients.
    /// When set, the handler subscribes to notification events and pushes them
    /// without depending on the gRPC notification-to-cluster bridge.
    notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    connection_service: Arc<dyn RealtimeConnectionService>,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
    rate_limit_config: Arc<RateLimitConfig>,
    content_filter: Arc<ContentFilter>,
    public_id_codec: Arc<crate::PublicIdCodec>,
    sender: Arc<dyn MessageSender>,
    playback_snapshot_service: Option<Arc<dyn PlaybackSnapshotService>>,
    playlist_items_snapshot_service: Option<Arc<dyn PlaylistItemsSnapshotService>>,
    room_members_snapshot_service: Option<Arc<dyn RoomMembersSnapshotService>>,
    room_settings_snapshot_service: Arc<dyn RoomSettingsSnapshotService>,
    resource_observer: Arc<ResourceObserver>,
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
    membership_cache: Arc<moka::sync::Cache<(RoomId, UserId), CachedMembership>>,
    /// Room event receiver created during `pre_join()` so transports do not expose
    /// a window where the connection is joined in `ConnectionManager` but not yet
    /// subscribed in `RoomMessageHub`.
    pending_room_event_rx:
        Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<ClusterEvent>>>>,
    /// Instance-level concurrency configuration for backpressure control.
    /// This replaces the global `MESSAGE_PROCESSING_SEMAPHORE` with per-AppState configuration.
    concurrency_config: Arc<MessageConcurrencyConfig>,
    /// Throttle state for playback progress DB writes.
    /// Stores the (last_written_position, last_write_time) to avoid
    /// writing to the DB on every progress heartbeat.
    last_progress_write: Arc<tokio::sync::Mutex<Option<(f64, tokio::time::Instant)>>>,
    heartbeat_schedule: HeartbeatSchedule,
    filter_private_ice_candidates: bool,
}

impl Clone for StreamMessageHandler {
    fn clone(&self) -> Self {
        Self {
            room_id: self.room_id,
            user_id: self.user_id,
            username: self.username.clone(),
            connection_id: self.connection_id.clone(),
            room_service: Arc::clone(&self.room_service),
            chat_service: Arc::clone(&self.chat_service),
            event_service: Arc::clone(&self.event_service),
            notification_service: self.notification_service.clone(),
            connection_service: Arc::clone(&self.connection_service),
            rate_limiter: Arc::clone(&self.rate_limiter),
            rate_limit_config: Arc::clone(&self.rate_limit_config),
            content_filter: Arc::clone(&self.content_filter),
            public_id_codec: Arc::clone(&self.public_id_codec),
            sender: Arc::clone(&self.sender),
            playback_snapshot_service: self.playback_snapshot_service.clone(),
            playlist_items_snapshot_service: self.playlist_items_snapshot_service.clone(),
            room_members_snapshot_service: self.room_members_snapshot_service.clone(),
            room_settings_snapshot_service: Arc::clone(&self.room_settings_snapshot_service),
            resource_observer: Arc::clone(&self.resource_observer),
            ws_message_rate_limit: self.ws_message_rate_limit,
            has_webrtc_session: Arc::clone(&self.has_webrtc_session),
            skip_cleanup_user_left: Arc::clone(&self.skip_cleanup_user_left),
            membership_cache: Arc::clone(&self.membership_cache),
            pending_room_event_rx: Arc::clone(&self.pending_room_event_rx),
            concurrency_config: Arc::clone(&self.concurrency_config),
            last_progress_write: Arc::clone(&self.last_progress_write),
            heartbeat_schedule: self.heartbeat_schedule,
            filter_private_ice_candidates: self.filter_private_ice_candidates,
        }
    }
}

impl StreamMessageHandler {
    #[must_use]
    pub fn generate_connection_id() -> String {
        format!("conn_c{}", synctv_common::snanoid!(16))
    }

    fn encode_user_id_for_webrtc(&self, user_id: UserId) -> String {
        self.public_id_codec
            .encode_user_id(user_id)
            .expect("positive WebRTC user ID must encode")
    }

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
            return Err("WebRTC recipient must be formatted as public_user_id:conn_id".to_string());
        };

        let target = self
            .connection_service
            .get_connection(target_conn_id)
            .ok_or_else(|| "Target connection is no longer active".to_string())?;

        if self.encode_user_id_for_webrtc(target.user_id) != target_user_id {
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
        let Some((target_user_id, target_conn_id)) = recipient.split_once(':') else {
            return false;
        };

        if target_conn_id != self.connection_id {
            return false;
        }

        let Some(current) = self.connection_service.get_connection(&self.connection_id) else {
            return false;
        };

        let user_matches = self.encode_user_id_for_webrtc(current.user_id) == target_user_id;

        user_matches && current.room_id.as_ref() == Some(&self.room_id) && current.rtc_joined
    }

    fn ice_candidate_contains_private_ip(candidate: &str) -> bool {
        candidate
            .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '[' | ']'))
            .filter_map(|part| part.parse::<std::net::IpAddr>().ok())
            .any(is_private_ice_candidate_ip)
    }

    /// Create a new stream message handler
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        room_id: RoomId,
        user_id: UserId,
        username: String,
        room_service: &Arc<RoomService>,
        chat_service: Arc<ChatService>,
        event_service: Arc<dyn RealtimeEventService>,
        connection_service: Arc<dyn RealtimeConnectionService>,
        rate_limiter: Arc<dyn RequestRateLimiterService>,
        rate_limit_config: Arc<RateLimitConfig>,
        content_filter: Arc<ContentFilter>,
        public_id_codec: Arc<crate::PublicIdCodec>,
        sender: Arc<dyn MessageSender>,
    ) -> Self {
        Self::with_concurrency_config(
            room_id,
            user_id,
            username,
            room_service,
            chat_service,
            event_service,
            connection_service,
            rate_limiter,
            rate_limit_config,
            content_filter,
            public_id_codec,
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
        room_service: &Arc<RoomService>,
        chat_service: Arc<ChatService>,
        event_service: Arc<dyn RealtimeEventService>,
        connection_service: Arc<dyn RealtimeConnectionService>,
        rate_limiter: Arc<dyn RequestRateLimiterService>,
        rate_limit_config: Arc<RateLimitConfig>,
        content_filter: Arc<ContentFilter>,
        public_id_codec: Arc<crate::PublicIdCodec>,
        sender: Arc<dyn MessageSender>,
        concurrency_config: Arc<MessageConcurrencyConfig>,
    ) -> Self {
        let connection_id = Self::generate_connection_id();
        Self::with_connection_id_and_concurrency_config(
            room_id,
            user_id,
            username,
            connection_id,
            room_service,
            chat_service,
            event_service,
            connection_service,
            rate_limiter,
            rate_limit_config,
            content_filter,
            public_id_codec,
            sender,
            concurrency_config,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_connection_id_and_concurrency_config(
        room_id: RoomId,
        user_id: UserId,
        username: String,
        connection_id: String,
        room_service: &Arc<RoomService>,
        chat_service: Arc<ChatService>,
        event_service: Arc<dyn RealtimeEventService>,
        connection_service: Arc<dyn RealtimeConnectionService>,
        rate_limiter: Arc<dyn RequestRateLimiterService>,
        rate_limit_config: Arc<RateLimitConfig>,
        content_filter: Arc<ContentFilter>,
        public_id_codec: Arc<crate::PublicIdCodec>,
        sender: Arc<dyn MessageSender>,
        concurrency_config: Arc<MessageConcurrencyConfig>,
    ) -> Self {
        // Create membership cache with TTL for heartbeat validation.
        // This reduces database queries from every heartbeat (25-35s) to at most once per TTL (30s).
        let membership_cache = Arc::new(
            moka::sync::Cache::builder()
                .time_to_live(HeartbeatSchedule::production().membership_cache_ttl())
                .build(),
        );
        let room_settings_snapshot_service =
            default_room_settings_snapshot_service(Arc::clone(room_service));
        let resource_observer = Arc::new(ResourceObserver::new(
            room_id,
            user_id,
            connection_id.clone(),
            Arc::clone(room_service),
            Arc::clone(&public_id_codec),
            Arc::clone(&sender),
            Arc::clone(&room_settings_snapshot_service),
        ));

        Self {
            room_id,
            user_id,
            username,
            connection_id,
            room_service: Arc::clone(room_service),
            chat_service,
            event_service,
            notification_service: None,
            connection_service,
            rate_limiter,
            rate_limit_config,
            content_filter,
            public_id_codec,
            sender,
            playback_snapshot_service: None,
            playlist_items_snapshot_service: None,
            room_members_snapshot_service: None,
            room_settings_snapshot_service,
            resource_observer,
            ws_message_rate_limit: 50, // default, overridden by with_ws_message_rate_limit()
            has_webrtc_session: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            skip_cleanup_user_left: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            membership_cache,
            pending_room_event_rx: Arc::new(tokio::sync::Mutex::new(None)),
            concurrency_config,
            last_progress_write: Arc::new(tokio::sync::Mutex::new(None)),
            heartbeat_schedule: HeartbeatSchedule::production(),
            filter_private_ice_candidates: true,
        }
    }

    /// Set the per-connection WebSocket message rate limit from config.
    #[must_use]
    pub const fn with_ws_message_rate_limit(mut self, limit: u32) -> Self {
        self.ws_message_rate_limit = limit;
        self
    }

    #[must_use]
    pub fn with_connection_id(mut self, connection_id: String) -> Self {
        if let Some(observer) = Arc::get_mut(&mut self.resource_observer) {
            observer.set_connection_id(connection_id.clone());
        }
        self.connection_id = connection_id;
        self
    }

    #[must_use]
    pub fn with_playback_snapshot_service(
        mut self,
        service: Arc<dyn PlaybackSnapshotService>,
    ) -> Self {
        self.resource_observer
            .set_playback_snapshot_service(Some(Arc::clone(&service)));
        self.playback_snapshot_service = Some(service);
        self
    }

    #[must_use]
    pub fn with_room_settings_snapshot_service(
        mut self,
        service: Arc<dyn RoomSettingsSnapshotService>,
    ) -> Self {
        self.resource_observer
            .set_room_settings_snapshot_service(Arc::clone(&service));
        self.room_settings_snapshot_service = service;
        self
    }

    #[must_use]
    pub fn with_playlist_items_snapshot_service(
        mut self,
        service: Arc<dyn PlaylistItemsSnapshotService>,
    ) -> Self {
        self.resource_observer
            .set_playlist_items_snapshot_service(Some(Arc::clone(&service)));
        self.playlist_items_snapshot_service = Some(service);
        self
    }

    #[must_use]
    pub fn with_room_members_snapshot_service(
        mut self,
        service: Arc<dyn RoomMembersSnapshotService>,
    ) -> Self {
        self.resource_observer
            .set_room_members_snapshot_service(Some(Arc::clone(&service)));
        self.room_members_snapshot_service = Some(service);
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
    pub const fn with_filter_private_ice_candidates(mut self, enabled: bool) -> Self {
        self.filter_private_ice_candidates = enabled;
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
        let cache_key = (*room_id, *user_id);
        self.membership_cache.invalidate(&cache_key);
    }

    fn public_room_id(&self) -> String {
        self.public_id_codec
            .encode_room_id(self.room_id)
            .expect("positive room ID must encode")
    }

    fn public_user_id(&self) -> String {
        self.public_id_codec
            .encode_user_id(self.user_id)
            .expect("positive user ID must encode")
    }

    async fn final_realtime_admission_denial_reason(
        &self,
    ) -> Result<Option<String>, RealtimeJoinError> {
        let user = self
            .room_service
            .user_service()
            .get_user(&self.user_id)
            .await
            .map_err(|error| {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to re-validate user access during pre_join; rejecting connection because final admission must fail closed"
                );
                RealtimeJoinError::ServiceUnavailable(
                    "User re-validation temporarily unavailable".to_string(),
                )
        })?;

        if user.status == UserStatus::Banned {
            return Ok(Some(
                "User is no longer allowed to use real-time messaging".to_string(),
            ));
        }
        if user.deleted_at.is_some() {
            return Ok(Some("User account is no longer available".to_string()));
        }

        let room = self.room_service.get_room(&self.room_id).await.map_err(|error| {
            tracing::warn!(
                error = %error,
                room_id = %self.room_id,
                user_id = %self.user_id,
                "Failed to re-validate room access during pre_join; rejecting connection because final admission must fail closed"
            );
            RealtimeJoinError::ServiceUnavailable(
                "Room re-validation temporarily unavailable".to_string(),
            )
        })?;

        if room.is_banned {
            return Ok(Some("This room has been banned".to_string()));
        }
        if room.status == RoomStatus::Closed {
            return Ok(Some(
                "This room is closed and not accepting new connections".to_string(),
            ));
        }

        let membership_lookup =
            probe_realtime_membership_access_with_room(&self.room_service, &room, &self.user_id)
                .await;
        if let Some(reason) = initial_realtime_join_denial_reason(&membership_lookup) {
            return Ok(Some(reason));
        }
        if let Err(error) = membership_lookup {
            tracing::warn!(
                error = %error,
                room_id = %self.room_id,
                user_id = %self.user_id,
                "Failed to re-validate membership during pre_join; rejecting connection because final admission must fail closed"
            );
            return Err(RealtimeJoinError::ServiceUnavailable(
                "Membership re-validation temporarily unavailable".to_string(),
            ));
        }

        Ok(None)
    }

    /// Register the connection and join the room, enforcing connection limits.
    ///
    /// Call this **before** returning the gRPC response stream so that limit
    /// violations surface as a proper gRPC error instead of silently failing
    /// inside a background task.  After a successful `pre_join`, call
    /// [`run_after_join`] to enter the message loop.
    pub async fn pre_join(&self) -> Result<(), RealtimeJoinError> {
        if let Err(e) = self
            .connection_service
            .register(self.connection_id.clone(), self.user_id)
            .await
        {
            tracing::warn!("Failed to register connection: {}", e);
            return Err(classify_realtime_join_error_message(e));
        }

        self.pre_join_after_registration().await
    }

    /// Continue admission after the connection was already registered.
    ///
    /// This is used by transports that need an early registration/backpressure
    /// step before they can finish reading the room-scoped handshake.
    pub async fn pre_join_after_registration(&self) -> Result<(), RealtimeJoinError> {
        if let Err(e) = self
            .connection_service
            .join_room(&self.connection_id, self.room_id)
            .await
        {
            self.connection_service
                .unregister(&self.connection_id)
                .await;
            return Err(classify_realtime_join_error_message(e));
        }

        let denial_reason = match self.final_realtime_admission_denial_reason().await {
            Ok(reason) => reason,
            Err(error) => {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.connection_service
                    .unregister(&self.connection_id)
                    .await;
                return Err(error);
            }
        };
        if let Some(reason) = denial_reason {
            self.skip_cleanup_user_left
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.connection_service
                .unregister(&self.connection_id)
                .await;
            return Err(RealtimeJoinError::PermissionDenied(reason.clone()));
        }

        if let Err(error) = self.cache_room_event_subscription().await {
            self.skip_cleanup_user_left
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.connection_service
                .unregister(&self.connection_id)
                .await;
            return Err(error);
        }

        Ok(())
    }

    async fn cache_room_event_subscription(&self) -> Result<(), RealtimeJoinError> {
        let mut pending_rx = self.pending_room_event_rx.lock().await;
        if pending_rx.is_some() {
            return Ok(());
        }

        let (event_rx, _connection_id) = self
            .event_service
            .subscribe_with_id(self.room_id, self.user_id, self.connection_id.clone())
            .await
            .map_err(|e| {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                RealtimeJoinError::Internal(format!(
                    "Failed to subscribe to cluster events during pre_join: {e}"
                ))
            })?;
        *pending_rx = Some(event_rx);

        Ok(())
    }

    async fn take_room_event_subscription(
        &self,
    ) -> Result<tokio::sync::mpsc::Receiver<ClusterEvent>, String> {
        if let Some(event_rx) = self.pending_room_event_rx.lock().await.take() {
            return Ok(event_rx);
        }

        self.event_service
            .subscribe_with_id(self.room_id, self.user_id, self.connection_id.clone())
            .await
            .map(|(event_rx, _connection_id)| event_rx)
            .map_err(|e| format!("Failed to subscribe to cluster events: {e}"))
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
        self.pre_join().await.map_err(String::from)?;
        self.run_after_join(stream).await
    }

    /// Continue the message loop after a successful [`pre_join`].
    ///
    /// This is identical to [`run`] but skips the register/join_room steps
    /// that were already performed by `pre_join`.
    pub async fn run_after_join<S: StreamMessage>(&self, stream: &mut S) -> Result<(), String> {
        let room_id_str = self.public_room_id();

        // Pre-join caches the room subscription so there is no gap between
        // admission success and the transport starting its receive loop.
        let mut event_rx = self.take_room_event_subscription().await?;

        // Subscribe to disconnect signals
        let mut disconnect_rx = self.connection_service.subscribe_disconnect();

        // Subscribe to admin events (KickUser, etc.) for cross-replica disconnect propagation.
        // KickUser events arrive via Redis PubSub on the admin channel and are not
        // delivered through the room-level event subscription, so each connection
        // must independently monitor admin events and disconnect when targeted.
        let mut admin_rx = self.event_service.subscribe_admin_events();

        // H11: Subscribe to notification events for direct real-time push.
        // This ensures notifications are delivered to WebSocket clients even when
        // the gRPC notification-to-cluster bridge is not running.
        let mut notification_rx = self
            .notification_service
            .as_ref()
            .map(|svc| svc.subscribe_events());

        // E6 fix: Fetch member data ONCE and pass to both methods
        let member_lookup =
            probe_realtime_membership_access(&self.room_service, &self.room_id, &self.user_id)
                .await;
        if let Some(reason) = initial_realtime_join_denial_reason(&member_lookup) {
            tracing::info!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                reason,
                "Aborting real-time join because membership was revoked before initialization completed"
            );
            self.skip_cleanup_user_left
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.cleanup(&room_id_str).await;
            return Ok(());
        }
        let member_data = match member_lookup {
            Ok(RealtimeMembershipAccess::Allowed(member)) => Some(member),
            Ok(RealtimeMembershipAccess::Denied(_)) => None,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to fetch membership during initial real-time join; using fallback member payload"
                );
                None
            }
        };

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
        let message_control = ExecutionControl::default();

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
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
                                    limit = global_msg_rate_limit,
                                    "Global WebSocket message rate limit exceeded, dropping message"
                                );
                                continue;
                            }

                            // Backpressure control: try to acquire a semaphore permit.
                            // If the system is overloaded, return ResourceExhausted error instead of processing.
                            let semaphore = self.concurrency_config.semaphore();
                            let Ok(permit) = semaphore.try_acquire_owned() else {
                                tracing::warn!(
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
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
                            if let Err(e) = self
                                .handle_client_message_with_control(&msg, Some(&message_control))
                                .await
                            {
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
                        for msg in cluster_event_to_server_messages(
                            &event,
                            &room_id_str,
                            &self.public_id_codec,
                        ) {
                            if let Err(e) = stream.send(msg) {
                                tracing::error!("Failed to send server message: {}", e);
                                send_failed = true;
                                break;
                            }
                        }
                        if send_failed {
                            break;
                        }

                        if let Err(error) = self
                            .resource_observer
                            .room_hub
                            .refresh_for_room_event(&event, Some(&self.connection_id))
                            .await
                        {
                            tracing::error!(
                                "Failed to refresh observed resources for room event: {}",
                                error
                            );
                            break;
                        }
                    } else {
                        tracing::error!("Cluster event channel closed");
                        break;
                    }
                }

                () = async {
                    match self.resource_observer.next_playback_snapshot_refresh_deadline().await {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    if let Err(error) = self
                        .resource_observer
                        .refresh_expired_playback_snapshot_observations()
                        .await
                    {
                        tracing::error!(
                            "Failed to refresh observed playback snapshot after expiration: {}",
                            error
                        );
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
                                    user_id = %self.user_id,
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
                                    room_id = %self.room_id,
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
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
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
                                user_id = %self.user_id,
                                room_id = %self.room_id,
                                "Disconnect signal channel lagged, re-subscribing and verifying membership"
                            );
                            disconnect_rx = self.connection_service.subscribe_disconnect();

                            // Fallback: check database to see if we were kicked/banned while lagged
                            match probe_realtime_membership_access(
                                &self.room_service,
                                &self.room_id,
                                &self.user_id,
                            )
                            .await
                            {
                                Ok(RealtimeMembershipAccess::Denied(reason)) => {
                                    tracing::info!(
                                        user_id = %self.user_id,
                                        room_id = %self.room_id,
                                        reason,
                                        "Real-time access is no longer valid (detected after disconnect signal lag), disconnecting"
                                    );
                                    self.skip_cleanup_user_left
                                        .store(true, std::sync::atomic::Ordering::Relaxed);
                                    break;
                                }
                                Ok(RealtimeMembershipAccess::Allowed(_)) => {}
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
                            let cache_key = (self.room_id, *user_id);
                            self.membership_cache.invalidate(&cache_key);

                            if *user_id == self.user_id {
                                tracing::info!(
                                    user_id = %self.user_id,
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
                            let cache_key = (*room_id, *user_id);
                            self.membership_cache.invalidate(&cache_key);

                            if *user_id == self.user_id && *room_id == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
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
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
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
                        Ok(ClusterEvent::ProviderCredentialChanged { ref event_id, ref user_id, ref provider, ref server_id, .. }) => {
                            self.resource_observer.handle_provider_credential_changed_admin_event(
                                event_id,
                                user_id,
                                provider,
                                server_id,
                            )
                            .await;
                        }
                        Ok(ClusterEvent::CacheInvalidate { ref event_id, ref targets, .. }) => {
                            self.resource_observer.handle_cache_invalidate_admin_event(event_id, targets).await;
                        }
                        Ok(_) => {
                            // Other admin events (KickPublisher, etc.) not relevant to this connection
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Channel lagged: we may have missed critical KickUser/KickUserFromRoom events.
                            // Re-subscribe to get a fresh receiver so future events are not lost.
                            tracing::warn!(
                                lagged = n,
                                user_id = %self.user_id,
                                "Admin event channel lagged, re-subscribing and verifying membership"
                            );
                            admin_rx = self.event_service.subscribe_admin_events();

                            // Fallback: query database to confirm member status since we may
                            // have missed a KickUser or KickUserFromRoom event during the lag.
                            match probe_realtime_membership_access(
                                &self.room_service,
                                &self.room_id,
                                &self.user_id,
                            )
                            .await
                            {
                                Ok(RealtimeMembershipAccess::Denied(reason)) => {
                                    tracing::info!(
                                        user_id = %self.user_id,
                                        room_id = %self.room_id,
                                        reason,
                                        "Real-time access is no longer valid (detected after admin event lag), disconnecting"
                                    );
                                    self.skip_cleanup_user_left
                                        .store(true, std::sync::atomic::Ordering::Relaxed);
                                    break;
                                }
                                Ok(RealtimeMembershipAccess::Allowed(_)) => {}
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
                                user_id = %self.user_id,
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
                    let cache_key = (self.room_id, self.user_id);
                    if let Some(cached) = self.membership_cache.get(&cache_key) {
                        if cached.is_banned {
                            tracing::info!(
                                user_id = %self.user_id,
                                room_id = %self.room_id,
                                "Periodic check (cached): user is banned, disconnecting"
                            );
                            self.skip_cleanup_user_left
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        if !cached.is_member {
                            tracing::info!(
                                user_id = %self.user_id,
                                room_id = %self.room_id,
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
                    match probe_realtime_membership_access(
                        &self.room_service,
                        &self.room_id,
                        &self.user_id,
                    )
                    .await
                    {
                        Ok(RealtimeMembershipAccess::Allowed(member)) => {
                            let cached = CachedMembership::from_member(Some(&member));
                            self.membership_cache.insert(cache_key, cached);
                        }
                        Ok(RealtimeMembershipAccess::Denied(reason)) => {
                            let cached = CachedMembership::from_member(None);
                            self.membership_cache.insert(cache_key, cached);
                            tracing::info!(
                                user_id = %self.user_id,
                                room_id = %self.room_id,
                                reason,
                                "Periodic check: real-time access is no longer valid, disconnecting"
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
                                user_id = %self.user_id,
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
        use crate::proto::client::server_message::Message;
        use crate::proto::client::UserJoinedRoom;
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
        let user_id = self.public_user_id();

        ServerMessage {
            message: Some(Message::UserJoined(UserJoinedRoom {
                room_id: room_id.to_string(),
                member: Some(ProtoRoomMember {
                    room_id: room_id.to_string(),
                    user_id,
                    username: self.username.clone(),
                    role: role_proto,
                    permissions,
                    status: member.map_or(
                        synctv_proto::common::MemberStatus::Active as i32,
                        |member| member_status_to_proto(member.status),
                    ),
                    added_permissions: added,
                    removed_permissions: removed,
                    admin_added_permissions: admin_added,
                    admin_removed_permissions: admin_removed,
                    joined_at: chrono::Utc::now().timestamp(),
                    is_online: true,
                    is_banned: false,
                    banned_at: 0,
                    banned_reason: String::new(),
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
            .connection_service
            .has_existing_presence_for_user_in_room_distributed(
                &self.user_id,
                &self.room_id,
                &self.connection_id,
            )
            .await
        {
            Ok(true) => {
                tracing::debug!(
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    connection_id = %self.connection_id,
                    "Skipping UserJoined broadcast because the user is already present in the room on another connection"
                );
                return;
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
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
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            user_id: self.user_id,
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
        let outcome = self.event_service.broadcast_outcome(event);
        if outcome.distributed_delivery_missed() {
            tracing::warn!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                "UserJoined broadcast missed the distributed fan-out path (non-critical: join is local-only)"
            );
        }
    }

    /// Cleanup on disconnect
    async fn cleanup(&self, room_id: &str) {
        self.resource_observer.clear_observations().await;

        // RT-2: If this connection had an active WebRTC session, decrement the
        // metric and broadcast WebRtcLeave so other peers can clean up.
        // Use Acquire ordering to synchronize with the Release store in handle_webrtc_join/leave.
        // IMPORTANT: We must check if the connection is STILL marked as RTC-joined
        // in the connection manager before decrementing the metric. This prevents
        // a race condition where:
        // 1. Cleanup task times out the WebRTC session (mark_rtc_joined(false))
        // 2. Connection ungracefully disconnects
        // 3. cleanup() sees has_webrtc_session=true and decrements the metric again
        // Result: Metric underflow (negative value)
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
                .connection_service
                .get_connection(&self.connection_id)
                .is_some_and(|conn| conn.rtc_joined);

            if is_still_rtc_joined {
                // Only decrement the metric if the connection was still RTC-joined
                synctv_core::metrics::http::WEBRTC_PEERS_ACTIVE.dec();

                // Mark the connection as no longer RTC-joined in the connection manager
                self.connection_service.mark_rtc_joined(
                    &self.room_id,
                    &self.user_id,
                    &self.connection_id,
                    false,
                );

                // Broadcast WebRtcLeave so other peers know this user dropped
                let leave_event = ClusterEvent::WebRTCLeave {
                    event_id: synctv_common::snanoid!(16),
                    room_id: self.room_id,
                    user_id: self.user_id,
                    conn_id: self.connection_id.clone(),
                    timestamp: chrono::Utc::now(),
                };
                self.event_service.broadcast(leave_event);

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
            self.connection_service
                .unregister(&self.connection_id)
                .await;
            self.event_service.unsubscribe(&self.connection_id);
            return;
        }

        let has_other_local_connection = self
            .connection_service
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
            .connection_service
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
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            user_id: self.user_id,
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
            UserLeftDeliveryPlan::LocalAndRedis => {
                Some(self.event_service.broadcast_outcome(event))
            }
        };

        if let Some(outcome) = result {
            if user_left_delivery_plan == UserLeftDeliveryPlan::LocalAndRedis
                && outcome.distributed_delivery_missed()
            {
                // Retry only when distributed delivery was expected but did not happen.
                // A room with zero remaining subscribers on a single node is not an error.
                // Spawn a background task to retry the broadcast with exponential backoff.
                // Use a global semaphore to limit concurrent retry tasks. During mass
                // disconnects with Redis down, thousands of connections may all try to
                // spawn retry tasks simultaneously. Without this bound, we'd exhaust
                // memory and CPU on unbounded task spawning.
                let event_service = self.event_service.clone();
                let room_id = self.room_id;
                let user_id = self.user_id;
                let username = self.username.clone();
                let connection_id = self.connection_id.clone();

                let semaphore = Arc::clone(&USER_LEFT_RETRY_SEMAPHORE);
                let permit = semaphore.try_acquire_owned();

                match permit {
                    Ok(permit) => {
                        tracing::warn!(
                            user = %username,
                            room = %room_id,
                            connection = %connection_id,
                            "UserLeft distributed publish missed; starting retry task"
                        );

                        spawn_monitored("userleft_retry", async move {
                            let _permit = permit; // Hold permit for duration of retry task

                            let mut delay_ms = USER_LEFT_RETRY_INITIAL_DELAY_MS;

                            for attempt in 1..=USER_LEFT_RETRY_MAX_RETRIES {
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms))
                                    .await;

                                let retry_event = ClusterEvent::UserLeft {
                                    event_id: synctv_common::snanoid!(16),
                                    room_id,
                                    user_id,
                                    username: username.clone(),
                                    timestamp: chrono::Utc::now(),
                                };

                                let retry_outcome =
                                    event_service.retry_broadcast_outcome(retry_event);

                                if retry_outcome.satisfies(
                                    RealtimeDeliveryRequirement::DistributedWhenAvailable,
                                ) {
                                    tracing::info!(
                                        user = %username,
                                        room = %room_id,
                                        connection = %connection_id,
                                        attempt = attempt,
                                        local_delivered = retry_outcome.local_delivered(),
                                        distributed_delivered = retry_outcome.distributed_delivered(),
                                        "UserLeft retry succeeded"
                                    );
                                    return;
                                }

                                tracing::warn!(
                                    user = %username,
                                    room = %room_id,
                                    connection = %connection_id,
                                    attempt = attempt,
                                    max_retries = USER_LEFT_RETRY_MAX_RETRIES,
                                    "UserLeft retry attempt failed"
                                );

                                // Exponential backoff with cap
                                delay_ms =
                                    std::cmp::min(delay_ms * 2, USER_LEFT_RETRY_MAX_DELAY_MS);
                            }

                            tracing::error!(
                                user = %username,
                                room = %room_id,
                                connection = %connection_id,
                                "UserLeft event permanently lost after {} retry attempts; other replicas may have stale user state",
                                USER_LEFT_RETRY_MAX_RETRIES
                            );
                        });
                    }
                    Err(_) => {
                        tracing::warn!(
                            user = %username,
                            room = %room_id,
                            connection = %connection_id,
                            "UserLeft retry task limit reached (max 100 concurrent); event may be lost"
                        );
                    }
                }
            }
        }

        // Now unregister from connection manager after broadcast has been sent
        self.connection_service
            .unregister(&self.connection_id)
            .await;
        self.event_service.unsubscribe(&self.connection_id);

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
        self.connection_service
            .register(self.connection_id.clone(), self.user_id)
            .await?;

        self.pre_join_after_registration()
            .await
            .map_err(String::from)?;

        let cancel_token = tokio_util::sync::CancellationToken::new();
        let room_id_str = self.public_room_id();

        // E6 fix: Fetch member data ONCE and pass to both methods
        let member_lookup =
            probe_realtime_membership_access(&self.room_service, &self.room_id, &self.user_id)
                .await;
        if let Some(reason) = initial_realtime_join_denial_reason(&member_lookup) {
            self.skip_cleanup_user_left
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.cleanup(&room_id_str).await;
            return Err(reason.clone());
        }
        let member_data = match member_lookup {
            Ok(RealtimeMembershipAccess::Allowed(member)) => Some(member),
            Ok(RealtimeMembershipAccess::Denied(_)) => None,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to fetch membership during start(); using fallback member payload"
                );
                None
            }
        };

        // Send initial UserJoined message to the client (mirrors run() behavior)
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

        let room_id = self.room_id;
        let room_id_str = self
            .public_id_codec
            .encode_room_id(room_id)
            .expect("positive room ID must encode");
        let event_connection_id = self.connection_id.clone();
        let event_user_id = self.user_id;
        let mut rx_events = match self.take_room_event_subscription().await {
            Ok(rx_events) => rx_events,
            Err(error) => {
                self.cleanup(&room_id_str).await;
                return Err(error);
            }
        };
        let sender = self.sender.clone();
        let event_handler = self.clone();
        let public_id_codec = self.public_id_codec.clone();

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
                                            public_id_codec
                                                .encode_user_id(event_user_id)
                                                .is_ok_and(|public_user_id| *to == public_user_id)
                                        };
                                    if !is_target {
                                        continue;
                                    }
                                }

                                let is_room_shutdown = matches!(
                                    event,
                                    ClusterEvent::RoomDeleted { .. }
                                        | ClusterEvent::RoomBanned { .. }
                                        | ClusterEvent::RoomOwnerInactive { .. }
                                );

                                for msg in cluster_event_to_server_messages(
                                    &event,
                                    &room_id_str,
                                    &public_id_codec,
                                ) {
                                    if let Err(e) = sender.send(msg) {
                                        tracing::error!("Failed to send message: {}", e);
                                        event_token.cancel();
                                        break;
                                    }
                                }

                                if let Err(error) =
                                    event_handler
                                        .resource_observer
                                        .room_hub
                                        .refresh_for_room_event(&event, Some(&event_connection_id))
                                        .await
                                {
                                    tracing::error!(
                                        "Failed to refresh observed resources in start(): {}",
                                        error
                                    );
                                    event_token.cancel();
                                    break;
                                }

                                // After delivering a terminal room-wide admin event, trigger cancellation so
                                // cleanup fires only after the event has been forwarded.
                                // This prevents the race where the cleanup task fires
                                // before the critical event reaches the client.
                                if is_room_shutdown {
                                    tracing::info!(
                                        "Terminal room event delivered in start(), triggering cleanup"
                                    );
                                    event_token.cancel();
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    () = async {
                        match event_handler
                            .resource_observer
                            .next_playback_snapshot_refresh_deadline()
                            .await {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        if let Err(error) = event_handler
                            .resource_observer
                            .refresh_expired_playback_snapshot_observations()
                            .await
                        {
                            tracing::error!(
                                "Failed to refresh observed playback snapshot in start(): {}",
                                error
                            );
                            event_token.cancel();
                            break;
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
            let message_control = ExecutionControl::from_parts(None, msg_token.clone());
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
                                let Ok(permit) = semaphore.try_acquire_owned() else {
                                    tracing::warn!(
                                        connection_id = %handler.connection_id,
                                        "System overloaded: message processing semaphore exhausted in start()"
                                    );
                                    continue;
                                };

                                // Process message with semaphore permit held
                                let _permit = permit;
                                if let Err(e) = handler
                                    .handle_client_message_with_control(&msg, Some(&message_control))
                                    .await
                                {
                                    tracing::error!("Failed to handle client message: {}", e);
                                    if let Err(send_err) = handler.sender.send(
                                        Self::error_server_message(e.clone()),
                                    ) {
                                        tracing::error!(
                                            "Failed to send message error to client in start(): {}",
                                            send_err
                                        );
                                        msg_token.cancel();
                                        break;
                                    }
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
            let mut disconnect_rx = self.connection_service.subscribe_disconnect();
            let mut admin_rx = self.event_service.subscribe_admin_events();
            let disconnect_token = cancel_token.clone();
            let connection_id = self.connection_id.clone();
            let user_id = self.user_id;
            let room_id = self.room_id;
            let room_service = Arc::clone(&self.room_service);
            let event_service = Arc::clone(&self.event_service);
            let connection_service = self.connection_service.clone();
            let admin_sender = self.sender.clone();
            let admin_handler = self.clone();
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
                                    user_id = %user_id,
                                    "Disconnect signal channel lagged in start(), re-subscribing and verifying"
                                );
                                disconnect_rx = connection_service.subscribe_disconnect();
                                // Verify membership after lag
                                let is_removed = match probe_realtime_membership_access(
                                    &room_service,
                                    &room_id,
                                    &user_id,
                                )
                                .await
                                {
                                    Ok(RealtimeMembershipAccess::Denied(_)) => true,
                                    Ok(RealtimeMembershipAccess::Allowed(_)) | Err(_) => false,
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
                            if let Ok(ClusterEvent::UserNotification { user_id: uid, title, content, notification_type, notification_id, timestamp, .. }) = &admin_event {
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
                            if let Ok(ClusterEvent::ProviderCredentialChanged { event_id, user_id: changed_user_id, provider, server_id, .. }) = &admin_event {
                                admin_handler
                                    .resource_observer
                                    .handle_provider_credential_changed_admin_event(
                                        event_id,
                                        changed_user_id,
                                        provider,
                                        server_id,
                                    )
                                    .await;
                                continue;
                            }
                            if let Ok(ClusterEvent::CacheInvalidate { event_id, targets, .. }) = &admin_event
                            {
                                admin_handler
                                    .resource_observer
                                    .handle_cache_invalidate_admin_event(event_id, targets)
                                    .await;
                                continue;
                            }
                            let should_disconnect = match &admin_event {
                                Ok(ClusterEvent::KickUser { user_id: uid, .. }) => {
                                    *uid == user_id
                                }
                                Ok(
                                    ClusterEvent::KickUserFromRoom { user_id: uid, room_id: rid, .. }
                                    | ClusterEvent::UserLeft { user_id: uid, room_id: rid, .. },
                                ) => {
                                    *uid == user_id && *rid == room_id
                                }
                                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => false,
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => true,
                            };
                            // Handle lag separately
                            if let Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) = admin_event {
                                tracing::warn!(
                                    lagged = n,
                                    user_id = %user_id,
                                    "Admin event channel lagged in start(), re-subscribing and verifying"
                                );
                                admin_rx = event_service.subscribe_admin_events();
                                // Verify membership after lag
                                let is_removed = match probe_realtime_membership_access(
                                    &room_service,
                                    &room_id,
                                    &user_id,
                                )
                                .await
                                {
                                    Ok(RealtimeMembershipAccess::Denied(_)) => true,
                                    Ok(RealtimeMembershipAccess::Allowed(_)) | Err(_) => false,
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
            let heartbeat_room_id = self.room_id;
            let heartbeat_user_id = self.user_id;
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

                            match probe_realtime_membership_access(
                                &heartbeat_room_service,
                                &heartbeat_room_id,
                                &heartbeat_user_id,
                            )
                            .await
                            {
                                Ok(RealtimeMembershipAccess::Denied(reason)) => {
                                    tracing::info!(
                                        user_id = %heartbeat_user_id,
                                        room_id = %heartbeat_room_id,
                                        reason,
                                        "start() periodic check: real-time access is no longer valid, disconnecting"
                                    );
                                    skip_cleanup_user_left
                                        .store(true, std::sync::atomic::Ordering::Relaxed);
                                    heartbeat_token.cancel();
                                    break;
                                }
                                Ok(RealtimeMembershipAccess::Allowed(_)) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        user_id = %heartbeat_user_id,
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
        let cleanup_room_id = self.public_room_id();
        let cleanup_token = cancel_token.clone();
        spawn_monitored("messaging_cleanup", async move {
            cleanup_token.cancelled().await;
            cleanup_handler.cleanup(&cleanup_room_id).await;
        });

        Ok((tx, cancel_token))
    }
}

impl StreamMessageHandler {
    /// Handle incoming client message with all validations
    pub async fn handle_client_message(&self, msg: &ClientMessage) -> Result<(), String> {
        self.handle_client_message_with_control(msg, None).await
    }

    pub async fn handle_client_message_with_control(
        &self,
        msg: &ClientMessage,
        control: Option<&ExecutionControl>,
    ) -> Result<(), String> {
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

                    let rate_limit_key =
                        format!("room:{}:user:{}:danmaku", self.room_id, self.user_id);
                    self.rate_limiter
                        .check_rate_limit_with_control(
                            &rate_limit_key,
                            self.rate_limit_config.danmaku_per_second,
                            self.rate_limit_config.window_seconds,
                            control,
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
                    );
                } else {
                    // Chat: delegate entirely to ChatService which handles permissions,
                    // room settings, rate limiting, content filtering, and persistence.
                    // This eliminates the dual-path fallback (H10).
                    self.handle_chat_message_with_control(&chat_msg.content, control)
                        .await?;
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
                self.handle_webrtc_leave(leave)?;
            }
            Some(Message::PlaybackProgress(report)) => {
                self.handle_playback_progress(report).await?;
            }
            Some(Message::PlaybackUpdate(update)) => {
                self.handle_playback_update(update).await?;
            }
            Some(Message::ObserveResource(observe)) => {
                self.resource_observer
                    .handle_observe_resource(observe)
                    .await?;
            }
            Some(Message::UnobserveResource(unobserve)) => {
                self.resource_observer
                    .handle_unobserve_resource(unobserve)
                    .await?;
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

    async fn handle_chat_message_with_control(
        &self,
        content: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<(), String> {
        // Delegate to ChatService which handles permission checks, content filtering,
        // rate limiting, and persistence (no fallback path).
        let saved_msg = self
            .chat_service
            .send_message_with_control(self.room_id, self.user_id, content.to_string(), control)
            .await
            .map_err(|e| e.to_string())?;

        // Touch room activity to prevent TTL expiry on active rooms
        self.room_service.touch_room_activity(self.room_id).await;

        // Track chat message metric
        synctv_core::metrics::http::CHAT_MESSAGES_TOTAL
            .with_label_values(&[] as &[&str])
            .inc();

        // Use the filtered content from ChatService (content filtering already applied)
        let event = ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            user_id: self.user_id,
            username: self.username.clone(),
            message: saved_msg.content,
            timestamp: chrono::Utc::now(),
            position: None,
            color: None,
        };

        // Broadcast to cluster (handles both local and Redis).
        // Chat is non-critical: log if Redis was not reached but do not fail the operation.
        let outcome = self.event_service.broadcast_outcome(event);
        if outcome.distributed_delivery_missed() {
            tracing::warn!(
                room_id = %self.room_id,
                "ChatMessage broadcast missed the distributed fan-out path (message may not be visible on other replicas)"
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
    fn handle_danmaku(&self, content: &str, position: f64, color: Option<String>) {
        let event = ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            user_id: self.user_id,
            username: self.username.clone(),
            message: content.to_string(),
            timestamp: chrono::Utc::now(),
            position: Some(position),
            color,
        };

        // Broadcast to cluster (handles both local and Redis).
        // Danmaku is ephemeral and non-critical.
        let outcome = self.event_service.broadcast_outcome(event);
        if outcome.distributed_delivery_missed() {
            tracing::debug!(
                room_id = %self.room_id,
                "Danmaku broadcast missed the distributed fan-out path (ephemeral, acceptable)"
            );
        }
    }

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

        if self.connection_service.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&offer.to)?;

        // P2P relay path: forward offer to target peer via cluster
        let event = ClusterEvent::WebRTCSignaling {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            message_type: WebRTCSignalKind::Offer,
            from: format!("{}|{}", self.public_user_id(), conn_id),
            to: offer.to.clone(),
            data: offer.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Cross-replica WebRTC signaling must reach Redis when cluster mode is enabled.
        let outcome = self.event_service.broadcast_outcome(event);
        if !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable) {
            tracing::warn!(
                room_id = %self.room_id,
                "WebRTC offer realtime delivery did not satisfy distributed delivery requirements"
            );
            synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                .with_label_values(&["webrtc_signal_no_redis"])
                .inc();
            return Err(
                "WebRTC offer delivery failed: distributed realtime fan-out unavailable"
                    .to_string(),
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

        if self.connection_service.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&answer.to)?;

        // Create event with server-set 'from' field
        let event = ClusterEvent::WebRTCSignaling {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            message_type: WebRTCSignalKind::Answer,
            from: format!("{}|{}", self.public_user_id(), conn_id),
            to: answer.to.clone(),
            data: answer.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Cross-replica WebRTC signaling must reach Redis when cluster mode is enabled.
        let outcome = self.event_service.broadcast_outcome(event);
        if !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable) {
            tracing::warn!(
                room_id = %self.room_id,
                "WebRTC answer realtime delivery did not satisfy distributed delivery requirements"
            );
            synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                .with_label_values(&["webrtc_signal_no_redis"])
                .inc();
            return Err(
                "WebRTC answer delivery failed: distributed realtime fan-out unavailable"
                    .to_string(),
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
        if self.filter_private_ice_candidates
            && Self::ice_candidate_contains_private_ip(&candidate.data)
        {
            return Err("WebRTC ICE candidate contains a private or local address".to_string());
        }

        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        let conn_id = self.connection_id.clone();

        if self.connection_service.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&candidate.to)?;

        // P2P relay path: forward ICE candidate to target peer via cluster
        let event = ClusterEvent::WebRTCSignaling {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            message_type: WebRTCSignalKind::IceCandidate,
            from: format!("{}|{}", self.public_user_id(), conn_id),
            to: candidate.to.clone(),
            data: candidate.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Cross-replica ICE signaling must reach Redis when cluster mode is enabled.
        let outcome = self.event_service.broadcast_outcome(event);
        if !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable) {
            tracing::warn!(
                room_id = %self.room_id,
                "ICE candidate realtime delivery did not satisfy distributed delivery requirements"
            );
            synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                .with_label_values(&["webrtc_signal_no_redis"])
                .inc();
            return Err(
                "WebRTC ICE candidate delivery failed: distributed realtime fan-out unavailable"
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
            self.connection_service
                .get_connection(&conn_id)
                .map(|conn| conn.rtc_joined),
            true,
        )
        .map_err(std::string::ToString::to_string)?;

        if !should_join {
            tracing::debug!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                connection_id = %conn_id,
                "Ignoring duplicate WebRTC join for already-joined connection"
            );
            return Ok(());
        }

        // Mark this connection as joined WebRTC session
        self.connection_service
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
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            user_id: self.user_id,
            conn_id,
            username: self.username.clone(),
            timestamp: chrono::Utc::now(),
        };

        // WebRTC join is semi-critical: log at warn if not propagated to Redis.
        let outcome = self.event_service.broadcast_outcome(event);
        if outcome.distributed_delivery_missed() {
            tracing::warn!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                "WebRTC join broadcast missed the distributed fan-out path (peer may not be visible cross-replica)"
            );
        }

        Ok(())
    }

    fn handle_webrtc_leave(
        &self,
        _leave: &crate::proto::client::WebRtcLeave,
    ) -> Result<(), String> {
        let conn_id = self.connection_id.clone();

        let should_leave = should_transition_webrtc_membership(
            self.connection_service
                .get_connection(&conn_id)
                .map(|conn| conn.rtc_joined),
            false,
        )
        .map_err(std::string::ToString::to_string)?;

        if !should_leave {
            tracing::debug!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                connection_id = %conn_id,
                "Ignoring duplicate WebRTC leave for already-left connection"
            );
            self.has_webrtc_session
                .store(false, std::sync::atomic::Ordering::Release);
            return Ok(());
        }

        // Mark this connection as left WebRTC session
        self.connection_service
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
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            user_id: self.user_id,
            conn_id,
            timestamp: chrono::Utc::now(),
        };

        // WebRTC leave is semi-critical: log at warn if distributed fan-out misses.
        let outcome = self.event_service.broadcast_outcome(event);
        if outcome.distributed_delivery_missed() {
            tracing::warn!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                "WebRTC leave broadcast missed the distributed fan-out path (peer may remain visible cross-replica)"
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
            .check_permission(&self.room_id, &self.user_id, PermissionBits::PLAY_CONTROL)
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
            let elapsed_ms = chrono::Utc::now()
                .signed_duration_since(state.updated_at)
                .num_milliseconds();
            let elapsed_secs = if elapsed_ms <= 0 {
                0.0
            } else {
                Duration::from_millis(u64::try_from(elapsed_ms).unwrap_or(u64::MAX)).as_secs_f64()
            };
            let expected_position = state.current_time + (elapsed_secs * state.speed);
            let drift = (report.current_time - expected_position).abs();

            if drift > PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS {
                tracing::warn!(
                    user_id = %self.user_id,
                    room_id = %self.room_id,
                    reported = report.current_time,
                    expected = expected_position,
                    drift = drift,
                    "Playback progress report rejected: drift exceeds {} seconds",
                    PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS
                );
                return Err(format!(
                    "Playback progress drift too large ({drift:.1}s > {PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS}s)"
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
                    .update_state(self.room_id, |s| {
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
                            event_id: synctv_common::snanoid!(16),
                            room_id: self.room_id,
                            user_id: self.user_id,
                            username: self.username.clone(),
                            state: updated_state.clone(),
                            timestamp: chrono::Utc::now(),
                        };
                        self.event_service.broadcast_local(&self.room_id, &event);
                        if let Some(service) = &self.playback_snapshot_service {
                            service
                                .report_provider_playback_progress(
                                    &updated_state,
                                    report.current_time,
                                    !report.is_playing,
                                    false,
                                )
                                .await;
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            error = %e,
                            room_id = %self.room_id,
                            "Failed to update playback state from progress report (non-critical)"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle playback state update command from WebSocket.
    async fn handle_playback_update(
        &self,
        update: &crate::proto::client::UpdatePlayback,
    ) -> Result<(), String> {
        let command = crate::impls::client::build_update_playback(*update)
            .map_err(|error| error.to_string())?;
        let previous_state = self
            .room_service
            .playback_service()
            .get_state(&self.room_id)
            .await
            .ok();

        let crate::impls::client::PlaybackUpdateCommand::Patch {
            playing,
            position,
            speed,
            version,
        } = command;
        let state = self
            .room_service
            .playback_service()
            .update_multiple_with_version(
                self.room_id,
                self.user_id,
                playing,
                position,
                speed,
                version,
            )
            .await
            .map_err(|e| e.to_string())?;
        if let Some(service) = &self.playback_snapshot_service {
            service
                .handle_provider_lifecycle_transition(previous_state.as_ref(), &state)
                .await;
        }

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
        self.user_id
    }
}

/// Convert a cluster event into one or more server messages.
fn cluster_event_to_server_messages(
    event: &synctv_cluster::sync::ClusterEvent,
    room_id: &str,
    public_id_codec: &crate::PublicIdCodec,
) -> Vec<ServerMessage> {
    use crate::proto::client::server_message::Message;
    use crate::proto::client::{
        ChatMessageReceive, ErrorMessage, MediaRemovedBatch, MediaUpdated, PlaybackState,
        PlaybackStateChanged, PlaylistCreated, PlaylistDeleted, PlaylistReordered, PlaylistUpdated,
        RoomSettingsChanged, ServerMessage, UserJoinedRoom, UserLeftRoom,
    };
    use synctv_cluster::sync::ClusterEvent;
    use synctv_proto::common::RoomMember;

    let encode_user = |id| {
        public_id_codec
            .encode_user_id(id)
            .expect("cluster event user id must be encodable")
    };
    let encode_room = |id| {
        public_id_codec
            .encode_room_id(id)
            .expect("cluster event room id must be encodable")
    };
    let encode_media = |id| {
        public_id_codec
            .encode_media_id(id)
            .expect("cluster event media id must be encodable")
    };
    let encode_playlist = |id| {
        public_id_codec
            .encode_playlist_id(id)
            .expect("cluster event playlist id must be encodable")
    };

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
                id: synctv_common::snanoid!(12),
                room_id: room_id.to_string(),
                user_id: encode_user(*user_id),
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
                    room_id: encode_room(state.room_id),
                    playing_media_id: state
                        .playing_media_id
                        .as_ref()
                        .map(|id| encode_media(*id))
                        .unwrap_or_default(),
                    current_time: state.current_time,
                    speed: state.speed,
                    is_playing: state.is_playing,
                    updated_at: state.updated_at.timestamp(),
                    version: state.version,
                    playing_playlist_id: state
                        .playing_playlist_id
                        .as_ref()
                        .map(|id| encode_playlist(*id))
                        .unwrap_or_default(),
                    target: state.target.clone(),
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
                    user_id: encode_user(*user_id),
                    username: username.clone(),
                    role: *role,
                    permissions: permissions.0,
                    status: synctv_proto::common::MemberStatus::Active as i32,
                    added_permissions: added_permissions.0,
                    removed_permissions: removed_permissions.0,
                    admin_added_permissions: admin_added_permissions.0,
                    admin_removed_permissions: admin_removed_permissions.0,
                    joined_at: joined_at.timestamp(),
                    is_online: true,
                    is_banned: false,
                    banned_at: 0,
                    banned_reason: String::new(),
                }),
            })),
        }],
        ClusterEvent::UserLeft { user_id, .. } => vec![ServerMessage {
            message: Some(Message::UserLeft(UserLeftRoom {
                room_id: room_id.to_string(),
                user_id: encode_user(*user_id),
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
                media_id: encode_media(*media_id),
                title: media_title.clone(),
                added_by: username.clone(),
                added_by_user_id: encode_user(*user_id),
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
                media_id: encode_media(*media_id),
                removed_by: username.clone(),
                removed_by_user_id: encode_user(*user_id),
            })),
        }],
        ClusterEvent::MediaRemovedBatch {
            media_ids,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaRemovedBatch(MediaRemovedBatch {
                room_id: room_id.to_string(),
                media_ids: media_ids.iter().map(|mid| encode_media(*mid)).collect(),
                removed_by: username.clone(),
                removed_by_user_id: encode_user(*user_id),
            })),
        }],
        ClusterEvent::MediaUpdated {
            media_id,
            media_title,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaUpdated(MediaUpdated {
                room_id: room_id.to_string(),
                media_id: encode_media(*media_id),
                title: media_title.clone(),
                updated_by: username.clone(),
                updated_by_user_id: encode_user(*user_id),
            })),
        }],
        ClusterEvent::PlaylistReordered {
            media_ids,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::PlaylistReordered(PlaylistReordered {
                room_id: room_id.to_string(),
                media_ids: media_ids.iter().map(|id| encode_media(*id)).collect(),
                reordered_by: username.clone(),
                reordered_by_user_id: encode_user(*user_id),
            })),
        }],
        ClusterEvent::PlaylistCreated { playlist, .. } => vec![ServerMessage {
            message: Some(Message::PlaylistCreated(PlaylistCreated {
                room_id: room_id.to_string(),
                playlist: Some(crate::impls::client::convert::playlist_to_proto(
                    playlist,
                    0,
                    public_id_codec,
                )),
            })),
        }],
        ClusterEvent::PlaylistUpdated { playlist, .. } => vec![ServerMessage {
            message: Some(Message::PlaylistUpdated(PlaylistUpdated {
                room_id: room_id.to_string(),
                playlist: Some(crate::impls::client::convert::playlist_to_proto(
                    playlist,
                    0,
                    public_id_codec,
                )),
            })),
        }],
        ClusterEvent::PlaylistDeleted { playlist_id, .. } => vec![ServerMessage {
            message: Some(Message::PlaylistDeleted(PlaylistDeleted {
                room_id: room_id.to_string(),
                playlist_id: encode_playlist(*playlist_id),
            })),
        }],
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
                    user_id: encode_user(*target_user_id),
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
        ClusterEvent::RoomSettingsChanged {
            settings_json,
            version,
            ..
        } => vec![ServerMessage {
            message: Some(Message::RoomSettings(RoomSettingsChanged {
                room_id: room_id.to_string(),
                settings: settings_json.clone(),
                version: *version,
            })),
        }],
        ClusterEvent::WebRTCSignaling {
            message_type,
            from,
            to,
            data,
            ..
        } => {
            let msg = match message_type {
                WebRTCSignalKind::Offer => ServerMessage {
                    message: Some(Message::WebrtcOffer(crate::proto::client::WebRtcOffer {
                        from: from.clone(),
                        to: to.clone(),
                        data: data.clone(),
                    })),
                },
                WebRTCSignalKind::Answer => ServerMessage {
                    message: Some(Message::WebrtcAnswer(crate::proto::client::WebRtcAnswer {
                        from: from.clone(),
                        to: to.clone(),
                        data: data.clone(),
                    })),
                },
                WebRTCSignalKind::IceCandidate => ServerMessage {
                    message: Some(Message::WebrtcIceCandidate(
                        crate::proto::client::WebRtcIceCandidate {
                            from: from.clone(),
                            to: to.clone(),
                            data: data.clone(),
                        },
                    )),
                },
            };
            vec![msg]
        }
        ClusterEvent::WebRTCJoin {
            user_id,
            conn_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::WebrtcJoin(crate::proto::client::WebRtcJoin {
                user_id: encode_user(*user_id),
                conn_id: conn_id.clone(),
                username: username.clone(),
            })),
        }],
        ClusterEvent::WebRTCLeave {
            user_id, conn_id, ..
        } => vec![ServerMessage {
            message: Some(Message::WebrtcLeave(crate::proto::client::WebRtcLeave {
                user_id: encode_user(*user_id),
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
        ClusterEvent::RoomBanned { .. } => {
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room has been banned".to_string(),
                    code: crate::impls::error_codes::FORBIDDEN,
                    detail: String::new(),
                })),
            }]
        }
        ClusterEvent::RoomOwnerInactive { .. } => {
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room is unavailable because its creator is not active".to_string(),
                    code: crate::impls::error_codes::FORBIDDEN,
                    detail: String::new(),
                })),
            }]
        }
        ClusterEvent::KickPublisher { .. }
        | ClusterEvent::KickUser { .. }
        | ClusterEvent::KickUserFromRoom { .. }
        | ClusterEvent::RoomCreated { .. }
        | ClusterEvent::CacheInvalidate { .. }
        | ClusterEvent::ProviderCredentialChanged { .. }
        | ClusterEvent::UserNotification { .. } => {
            // Admin/internal events are handled by other channels,
            // not forwarded to WebSocket clients via the room event path
            vec![]
        }
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
        Ok(false) => UserLeftDeliveryPlan::LocalAndRedis,
        Ok(true) | Err(()) => UserLeftDeliveryPlan::Skip,
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

#[derive(Debug)]
enum RealtimeMembershipAccess {
    Allowed(synctv_core::models::RoomMember),
    Denied(String),
}

async fn probe_realtime_membership_access_with_room(
    room_service: &RoomService,
    room: &synctv_core::models::Room,
    user_id: &UserId,
) -> synctv_core::Result<RealtimeMembershipAccess> {
    match room_service.check_membership_with_room(room, user_id).await {
        Ok(()) => match room_service
            .member_service()
            .get_member(&room.id, user_id)
            .await?
        {
            Some(member) => Ok(RealtimeMembershipAccess::Allowed(member)),
            None => Ok(RealtimeMembershipAccess::Denied(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            )),
        },
        Err(synctv_core::Error::Authorization(message))
            if message == synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM =>
        {
            match room_service
                .member_service()
                .get_member(&room.id, user_id)
                .await?
            {
                Some(member) if member.is_banned() => Ok(RealtimeMembershipAccess::Denied(
                    "User is banned from this room".to_string(),
                )),
                Some(_) | None => Ok(RealtimeMembershipAccess::Denied(message)),
            }
        }
        Err(synctv_core::Error::Authorization(message)) => {
            Ok(RealtimeMembershipAccess::Denied(message))
        }
        Err(error) => Err(error),
    }
}

async fn probe_realtime_membership_access(
    room_service: &RoomService,
    room_id: &RoomId,
    user_id: &UserId,
) -> synctv_core::Result<RealtimeMembershipAccess> {
    let room = room_service.get_room(room_id).await?;
    probe_realtime_membership_access_with_room(room_service, &room, user_id).await
}

#[inline]
fn initial_realtime_join_denial_reason(
    member_lookup: &std::result::Result<RealtimeMembershipAccess, synctv_core::Error>,
) -> Option<String> {
    match member_lookup {
        Ok(RealtimeMembershipAccess::Denied(reason)) => Some(reason.clone()),
        Ok(RealtimeMembershipAccess::Allowed(_)) | Err(_) => None,
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
        // A global user disconnect (ban/delete) must still let cleanup emit a
        // room-scoped UserLeft for the connection's current room.
        synctv_cluster::sync::DisconnectSignal::User(_uid) => false,
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
        // A global KickUser must still allow connection cleanup to publish a
        // room-scoped UserLeft on the affected room.
        ClusterEvent::KickUser { user_id: _uid, .. } => false,
        ClusterEvent::RoomBanned { room_id: rid, .. }
        | ClusterEvent::RoomOwnerInactive { room_id: rid, .. } => rid == room_id,
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
mod tests;
