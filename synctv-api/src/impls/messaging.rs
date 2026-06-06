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

use async_trait::async_trait;
use futures::StreamExt;
use prost::Message;
use rand::RngExt;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use synctv_common::ExecutionControl;
use synctv_core::spawn::spawn_monitored;
use synctv_core::{
    models::{
        ChatEventKind, ChatMessageEvent, ChatMessageStatus, ChatMessageType, NewChatImage, RoomId,
        RoomMember, RoomPermission, RoomPermissionSet, RoomPlaybackState, RoomSettings, RoomStatus,
        SendChatMessage, UserId, UserStatus,
    },
    service::{
        ChatService, ContentFilter, PlaybackStatePatch, PlaybackUpdateRequest, RateLimitConfig,
        RequestRateLimiterService, RoomService,
    },
    Error as CoreError,
};
use synctv_realtime::sync::{RealtimeEvent, WebRTCSignalKind};
use tokio::sync::Semaphore;

use crate::chat_event_dispatcher::ChatEventDispatcher;
use crate::impls::client::{GuestRoomAccess, RoomActor};

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

fn should_persist_playback_progress(
    last_write: Option<(f64, tokio::time::Instant)>,
    position: f64,
) -> bool {
    match last_write {
        Some((last_pos, last_time)) => {
            let pos_delta = (position - last_pos).abs();
            let elapsed = last_time.elapsed().as_secs_f64();
            pos_delta > PROGRESS_MIN_POSITION_DELTA || elapsed > PROGRESS_MIN_ELAPSED_SECS
        }
        None => true,
    }
}

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
use crate::playback_fanout::{
    default_playback_fanout_service, PlaybackFanoutActor, PlaybackFanoutService,
};
use crate::proto::client::{ClientMessage, ObserveResource, ServerMessage};
#[cfg(test)]
use crate::resource_change::ResourceInvalidation;
use crate::runtime::{
    RealtimeConnectionService, RealtimeDeliveryRequirement, RealtimeEventService,
};

mod resource_observer;
use resource_observer::{ResourceObserver, ResourceObserverParams};

const OBSERVED_PLAYBACK_LIFECYCLE_TICK_INTERVAL: Duration = Duration::from_secs(10);
const OBSERVED_PLAYBACK_LIFECYCLE_CONCURRENCY: usize = 16;
const GUEST_INTERNAL_USER_ID_BASE: i64 = 8_000_000_000_000_000_000;
const GUEST_INTERNAL_USER_ID_SPAN: u64 = 500_000_000_000_000_000;

#[derive(Debug, Clone)]
pub struct ObservedPlaybackLifecycleEvent {
    pub room_id: RoomId,
    pub state: RoomPlaybackState,
}

#[async_trait]
pub trait ObservedPlaybackLifecycleSubscriber: Send + Sync {
    async fn handle_observed_playback_lifecycle_event(
        &self,
        event: ObservedPlaybackLifecycleEvent,
    ) -> Result<(), String>;
}

pub struct ProviderPlaybackProgressSubscriber {
    playback_snapshot_service: Arc<dyn PlaybackSnapshotService>,
}

impl ProviderPlaybackProgressSubscriber {
    #[must_use]
    pub fn new(playback_snapshot_service: Arc<dyn PlaybackSnapshotService>) -> Self {
        Self {
            playback_snapshot_service,
        }
    }
}

#[async_trait]
impl ObservedPlaybackLifecycleSubscriber for ProviderPlaybackProgressSubscriber {
    async fn handle_observed_playback_lifecycle_event(
        &self,
        event: ObservedPlaybackLifecycleEvent,
    ) -> Result<(), String> {
        if !event.state.is_playing {
            return Ok(());
        }

        if !ResourceObserver::room_has_playback_snapshot_observers(event.room_id).await {
            return Ok(());
        }

        self.playback_snapshot_service
            .report_provider_playback_progress(
                &event.state,
                event.state.computed_position(),
                false,
                false,
            )
            .await;
        Ok(())
    }
}

pub fn spawn_observed_playback_lifecycle_event_source(
    playback_snapshot_service: Arc<dyn PlaybackSnapshotService>,
    subscribers: Vec<Arc<dyn ObservedPlaybackLifecycleSubscriber>>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    spawn_monitored("observed_playback_lifecycle_event_source", async move {
        let mut ticker = tokio::time::interval(OBSERVED_PLAYBACK_LIFECYCLE_TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    publish_observed_playback_lifecycle_events(
                        Arc::clone(&playback_snapshot_service),
                        subscribers.as_slice(),
                    )
                    .await;
                }
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
            }
        }
    })
}

async fn publish_observed_playback_lifecycle_events(
    playback_snapshot_service: Arc<dyn PlaybackSnapshotService>,
    subscribers: &[Arc<dyn ObservedPlaybackLifecycleSubscriber>],
) {
    if subscribers.is_empty() {
        return;
    }

    let active_rooms = ResourceObserver::active_playback_snapshot_rooms().await;
    if active_rooms.is_empty() {
        return;
    }

    tokio_stream::iter(active_rooms)
        .for_each_concurrent(OBSERVED_PLAYBACK_LIFECYCLE_CONCURRENCY, |room_id| {
            let playback_snapshot_service = Arc::clone(&playback_snapshot_service);
            let subscribers = subscribers.to_vec();
            async move {
                if let Err(error) = publish_observed_playback_lifecycle_event(
                    playback_snapshot_service,
                    subscribers,
                    room_id,
                )
                .await
                {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        "Failed to publish observed playback lifecycle event"
                    );
                }
            }
        })
        .await;
}

async fn publish_observed_playback_lifecycle_event(
    playback_snapshot_service: Arc<dyn PlaybackSnapshotService>,
    subscribers: Vec<Arc<dyn ObservedPlaybackLifecycleSubscriber>>,
    room_id: RoomId,
) -> Result<(), String> {
    if !ResourceObserver::room_has_playback_snapshot_observers(room_id).await {
        return Ok(());
    }

    let state = playback_snapshot_service
        .room_playback_state(&room_id)
        .await
        .map_err(|error| error.to_string())?;
    if !state.is_playing {
        return Ok(());
    }

    if !ResourceObserver::room_has_playback_snapshot_observers(room_id).await {
        return Ok(());
    }

    let event = ObservedPlaybackLifecycleEvent { room_id, state };
    tokio_stream::iter(subscribers)
        .for_each_concurrent(OBSERVED_PLAYBACK_LIFECYCLE_CONCURRENCY, |subscriber| {
            let event = event.clone();
            async move {
                if let Err(error) = subscriber
                    .handle_observed_playback_lifecycle_event(event)
                    .await
                {
                    tracing::warn!(
                        error = %error,
                        "Observed playback lifecycle subscriber failed"
                    );
                }
            }
        })
        .await;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct GuestRealtimeIdentity {
    pub guest_id: String,
    pub display_name: String,
    pub session_id: String,
    pub token_jti: String,
    pub room_guest_version: i64,
    pub permissions: RoomPermissionSet,
}

#[derive(Debug, Clone)]
pub enum RealtimePrincipal {
    User {
        user_id: UserId,
        username: String,
    },
    Guest {
        internal_user_id: UserId,
        identity: GuestRealtimeIdentity,
    },
}

impl RealtimePrincipal {
    #[must_use]
    pub fn user(user_id: UserId, username: String) -> Self {
        Self::User { user_id, username }
    }

    pub fn guest(
        room_id: RoomId,
        identity: GuestRealtimeIdentity,
    ) -> Result<Self, RealtimeJoinError> {
        Ok(Self::Guest {
            internal_user_id: internal_guest_user_id(room_id, &identity.session_id)?,
            identity,
        })
    }

    #[must_use]
    pub fn connection_user_id(&self) -> UserId {
        match self {
            Self::User { user_id, .. } => *user_id,
            Self::Guest {
                internal_user_id, ..
            } => *internal_user_id,
        }
    }

    #[must_use]
    pub fn username(&self) -> &str {
        match self {
            Self::User { username, .. } => username,
            Self::Guest { identity, .. } => &identity.display_name,
        }
    }

    fn public_actor_id(&self, public_id_codec: &crate::PublicIdCodec) -> Result<String, String> {
        match self {
            Self::User { user_id, .. } => public_id_codec
                .encode_user_id(*user_id)
                .map_err(|error| format!("Failed to encode user public id: {error}")),
            Self::Guest { identity, .. } => Ok(identity.guest_id.clone()),
        }
    }

    #[must_use]
    fn room_actor(&self, room_id: RoomId) -> RoomActor {
        match self {
            Self::User { user_id, .. } => RoomActor::User {
                room_id,
                user_id: *user_id,
            },
            Self::Guest { identity, .. } => RoomActor::Guest(GuestRoomAccess {
                room_id,
                guest_id: identity.guest_id.clone(),
                display_name: identity.display_name.clone(),
                session_id: identity.session_id.clone(),
                token_jti: identity.token_jti.clone(),
                permissions: identity.permissions,
                room_guest_version: identity.room_guest_version,
            }),
        }
    }

    #[must_use]
    fn is_guest(&self) -> bool {
        matches!(self, Self::Guest { .. })
    }

    #[must_use]
    fn guest_identity(&self) -> Option<&GuestRealtimeIdentity> {
        match self {
            Self::Guest { identity, .. } => Some(identity),
            Self::User { .. } => None,
        }
    }
}

fn internal_guest_user_id(room_id: RoomId, session_id: &str) -> Result<UserId, RealtimeJoinError> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    "synctv:guest:v1".hash(&mut hasher);
    room_id.hash(&mut hasher);
    session_id.hash(&mut hasher);
    let offset = hasher.finish() % GUEST_INTERNAL_USER_ID_SPAN;
    let offset = i64::try_from(offset).map_err(|_| {
        RealtimeJoinError::Internal("Guest internal user id span exceeds i64".to_string())
    })?;
    UserId::try_from(GUEST_INTERNAL_USER_ID_BASE + offset).map_err(|error| {
        RealtimeJoinError::Internal(format!("Guest internal user id is invalid: {error}"))
    })
}

#[must_use]
pub fn guest_public_id(session_id: &str) -> String {
    format!("gst_{session_id}")
}

#[must_use]
pub fn guest_display_name(session_id: &str) -> String {
    let short = session_id.chars().take(6).collect::<String>();
    format!("Guest {short}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeJoinError {
    InvalidInput(String),
    PermissionDenied(String),
    RateLimited(String),
    ServiceUnavailable(String),
    Internal(String),
}

impl RealtimeJoinError {
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidInput(message)
            | Self::PermissionDenied(message)
            | Self::RateLimited(message)
            | Self::ServiceUnavailable(message)
            | Self::Internal(message) => message,
        }
    }

    pub fn log_if_internal(&self, context: &'static str) {
        if let Self::Internal(message) = self {
            tracing::error!(context, error = %message, "Unexpected realtime join failure");
        }
    }
}

impl From<String> for RealtimeJoinError {
    fn from(message: String) -> Self {
        classify_realtime_join_error_message(message)
    }
}

impl From<crate::runtime::RealtimeAdmissionError> for RealtimeJoinError {
    fn from(error: crate::runtime::RealtimeAdmissionError) -> Self {
        match error {
            crate::runtime::RealtimeAdmissionError::Capacity(message) => Self::RateLimited(message),
            crate::runtime::RealtimeAdmissionError::ClusterUnavailable(message) => {
                Self::ServiceUnavailable(message)
            }
            crate::runtime::RealtimeAdmissionError::Internal(message) => Self::Internal(message),
        }
    }
}

impl From<crate::impls::ApiError> for RealtimeJoinError {
    fn from(error: crate::impls::ApiError) -> Self {
        let message = error.message().to_string();
        match error.classify() {
            crate::impls::ErrorKind::InvalidArgument => Self::InvalidInput(message),
            crate::impls::ErrorKind::RateLimited => Self::RateLimited(message),
            crate::impls::ErrorKind::ServiceUnavailable | crate::impls::ErrorKind::Timeout => {
                Self::ServiceUnavailable(message)
            }
            crate::impls::ErrorKind::PermissionDenied
            | crate::impls::ErrorKind::Unauthenticated => Self::PermissionDenied(message),
            _ => Self::Internal(message),
        }
    }
}

impl From<RealtimeJoinError> for crate::impls::ApiError {
    fn from(error: RealtimeJoinError) -> Self {
        match error {
            RealtimeJoinError::InvalidInput(message) => Self::InvalidInput(message),
            RealtimeJoinError::PermissionDenied(message) => Self::Authorization(message),
            RealtimeJoinError::RateLimited(message) => Self::RateLimited(message),
            RealtimeJoinError::ServiceUnavailable(message) => Self::ServiceUnavailable(message),
            RealtimeJoinError::Internal(message) => Self::Internal(message),
        }
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
        crate::impls::ErrorKind::InvalidArgument => RealtimeJoinError::InvalidInput(message),
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
    pub const fn fixed(membership_cache_ttl: Duration, base_interval: Duration) -> Self {
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

/// Configuration for message processing concurrency.
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
}

impl CachedMembership {
    /// Create a cached membership from a member lookup result.
    fn from_member(member: Option<&synctv_core::models::RoomMember>) -> Self {
        match member {
            Some(_) => Self { is_member: true },
            None => Self { is_member: false },
        }
    }
}

// Re-use the canonical role proto mapper from client::convert.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchResourceKind {
    PlaybackState,
    PlaybackSnapshot,
    RoomSettings,
    PlaylistItems,
    RoomMembers,
    ChatEvents,
}

impl WatchResourceKind {
    fn observe_id(self) -> &'static str {
        match self {
            Self::PlaybackState => "playback_state",
            Self::PlaybackSnapshot => "playback_snapshot",
            Self::RoomSettings => "room_settings",
            Self::PlaylistItems => "playlist_items",
            Self::RoomMembers => "room_members",
            Self::ChatEvents => "chat_events",
        }
    }
}

#[derive(Clone)]
pub struct ResourceWatchSessionConfig {
    pub room_id: RoomId,
    pub principal: RealtimePrincipal,
    pub room_service: Arc<RoomService>,
    pub chat_service: Option<Arc<ChatService>>,
    pub event_service: Arc<dyn RealtimeEventService>,
    pub connection_service: Arc<dyn RealtimeConnectionService>,
    pub public_id_codec: Arc<crate::PublicIdCodec>,
    pub sender: Arc<dyn MessageSender>,
    pub playback_snapshot_service: Option<Arc<dyn PlaybackSnapshotService>>,
    pub playlist_items_snapshot_service: Option<Arc<dyn PlaylistItemsSnapshotService>>,
    pub room_members_snapshot_service: Option<Arc<dyn RoomMembersSnapshotService>>,
    pub room_settings_snapshot_service: Option<Arc<dyn RoomSettingsSnapshotService>>,
}

pub struct ResourceWatchSession {
    room_id: RoomId,
    principal: RealtimePrincipal,
    user_id: UserId,
    connection_id: String,
    room_service: Arc<RoomService>,
    chat_service: Option<Arc<ChatService>>,
    event_service: Arc<dyn RealtimeEventService>,
    connection_service: Arc<dyn RealtimeConnectionService>,
    public_id_codec: Arc<crate::PublicIdCodec>,
    resource_observer: Arc<ResourceObserver>,
}

pub struct PreparedResourceWatchSession {
    session: ResourceWatchSession,
    event_rx: tokio::sync::mpsc::Receiver<RealtimeEvent>,
}

impl ResourceWatchSession {
    pub fn new(config: ResourceWatchSessionConfig) -> Self {
        let ResourceWatchSessionConfig {
            room_id,
            principal,
            room_service,
            chat_service,
            event_service,
            connection_service,
            public_id_codec,
            sender,
            playback_snapshot_service,
            playlist_items_snapshot_service,
            room_members_snapshot_service,
            room_settings_snapshot_service,
        } = config;
        let user_id = principal.connection_user_id();
        let connection_id = StreamMessageHandler::generate_connection_id();
        let room_settings_snapshot_service = room_settings_snapshot_service
            .unwrap_or_else(|| default_room_settings_snapshot_service(Arc::clone(&room_service)));
        let observer = ResourceObserver::new(ResourceObserverParams {
            room_id,
            user_id,
            actor: principal.room_actor(room_id),
            connection_id: connection_id.clone(),
            room_service: Arc::clone(&room_service),
            public_id_codec: Arc::clone(&public_id_codec),
            sender,
            playback_snapshot_service,
            playlist_items_snapshot_service,
            room_members_snapshot_service,
            room_settings_snapshot_service,
        });
        let observer = Arc::new(observer);

        Self {
            room_id,
            principal,
            user_id,
            connection_id,
            room_service,
            chat_service,
            event_service,
            connection_service,
            public_id_codec,
            resource_observer: observer,
        }
    }

    pub async fn run(
        self,
        observe: ObserveResource,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        self.prepare(&observe).await?.run(cancel_token).await
    }

    pub async fn prepare(
        self,
        observe: &ObserveResource,
    ) -> Result<PreparedResourceWatchSession, RealtimeJoinError> {
        self.connection_service
            .register_actor(
                self.connection_id.clone(),
                self.user_id,
                self.public_actor_id().map_err(RealtimeJoinError::from)?,
            )
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "Failed to register resource watch connection");
                RealtimeJoinError::from(
                    crate::runtime::RealtimeAdmissionError::from_runtime_message(error),
                )
            })?;

        if let Err(error) = self
            .connection_service
            .join_room(&self.connection_id, self.room_id)
            .await
        {
            self.connection_service
                .unregister(&self.connection_id)
                .await;
            return Err(RealtimeJoinError::from(
                crate::runtime::RealtimeAdmissionError::from_runtime_message(error),
            ));
        }

        if let Err(error) = self.ensure_realtime_room_access().await {
            self.connection_service
                .unregister(&self.connection_id)
                .await;
            return Err(RealtimeJoinError::from(error));
        }

        if let Err(error) = self.ensure_observe_resource_allowed(observe).await {
            self.connection_service
                .unregister(&self.connection_id)
                .await;
            return Err(classify_realtime_join_error_message(error));
        }

        let event_rx = match self
            .event_service
            .subscribe_with_id(self.room_id, self.user_id, self.connection_id.clone())
            .await
            .map(|(event_rx, _connection_id)| event_rx)
        {
            Ok(event_rx) => event_rx,
            Err(error) => {
                self.connection_service
                    .unregister(&self.connection_id)
                    .await;
                return Err(RealtimeJoinError::Internal(format!(
                    "Failed to subscribe to realtime events: {error}"
                )));
            }
        };

        let chat_replay_service = if matches!(
            observe.resource.as_ref(),
            Some(crate::proto::client::observe_resource::Resource::ChatEvents(_))
        ) {
            if let Some(chat_service) = self.chat_service.as_ref() {
                Some(Arc::clone(chat_service))
            } else {
                self.event_service.unsubscribe(&self.connection_id);
                self.connection_service
                    .unregister(&self.connection_id)
                    .await;
                return Err(RealtimeJoinError::Internal(
                    "Chat service is not available".to_string(),
                ));
            }
        } else {
            None
        };

        if let Err(error) = self
            .resource_observer
            .handle_observe_resource(observe)
            .await
        {
            self.event_service.unsubscribe(&self.connection_id);
            self.connection_service
                .unregister(&self.connection_id)
                .await;
            return Err(RealtimeJoinError::from(error));
        }

        if let Some(chat_service) = chat_replay_service {
            if let Err(error) = self
                .resource_observer
                .replay_chat_events_after(&chat_service, observe)
                .await
            {
                self.event_service.unsubscribe(&self.connection_id);
                self.connection_service
                    .unregister(&self.connection_id)
                    .await;
                return Err(RealtimeJoinError::from(error));
            }
        }
        if let Err(error) = self
            .resource_observer
            .replay_room_resource_events_after(observe)
            .await
        {
            self.event_service.unsubscribe(&self.connection_id);
            self.connection_service
                .unregister(&self.connection_id)
                .await;
            return Err(RealtimeJoinError::from(error));
        }

        Ok(PreparedResourceWatchSession {
            session: self,
            event_rx,
        })
    }

    fn public_actor_id(&self) -> Result<String, String> {
        self.principal.public_actor_id(&self.public_id_codec)
    }
}

impl PreparedResourceWatchSession {
    pub async fn run(
        self,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        let Self {
            session,
            mut event_rx,
        } = self;
        let mut disconnect_rx = session.connection_service.subscribe_disconnect();
        let mut admin_rx = session.event_service.subscribe_admin_events();

        let result = async {
            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => break Ok(()),
                    event = event_rx.recv() => {
                        let Some(event) = event else {
                            break Err("Realtime event channel closed".to_string());
                        };
                        if watch_admin_event_matches(&event, &session.user_id, &session.room_id) {
                            tracing::info!(
                                user_id = %session.user_id,
                                room_id = %session.room_id,
                                "Resource watch terminating after room access event"
                            );
                            break Ok(());
                        }
                        if let Err(error) = session
                            .resource_observer
                            .room_hub
                            .refresh_for_room_event(&event, Some(&session.connection_id))
                            .await
                        {
                            break Err(error);
                        }
                    }
                    signal = disconnect_rx.recv() => {
                        match signal {
                            Ok(signal) => {
                                if watch_disconnect_signal_matches(
                                    &signal,
                                    &session.user_id,
                                    &session.room_id,
                                    &session.connection_id,
                                ) {
                                    tracing::info!(
                                        user_id = %session.user_id,
                                        room_id = %session.room_id,
                                        connection_id = %session.connection_id,
                                        "Resource watch terminating after disconnect signal"
                                    );
                                    break Ok(());
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(
                                    lagged = n,
                                    user_id = %session.user_id,
                                    room_id = %session.room_id,
                                    "Resource watch disconnect signal channel lagged, re-subscribing and verifying access"
                                );
                                disconnect_rx = session.connection_service.subscribe_disconnect();
                                if let Err(reason) = session.ensure_realtime_room_access().await {
                                    tracing::info!(
                                        user_id = %session.user_id,
                                        room_id = %session.room_id,
                                        reason,
                                        "Resource watch access is no longer valid after disconnect signal lag"
                                    );
                                    break Ok(());
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                break Err("Disconnect signal channel closed".to_string());
                            }
                        }
                    }
                    admin_event = admin_rx.recv() => {
                        match admin_event {
                            Ok(RealtimeEvent::ProviderCredentialChanged { ref event_id, ref user_id, ref provider, ref server_id, .. }) => {
                                session.resource_observer
                                    .handle_provider_credential_changed_admin_event(
                                        event_id,
                                        user_id,
                                        provider,
                                        server_id,
                                )
                                    .await;
                            }
                            Ok(RealtimeEvent::CacheInvalidate { ref event_id, ref targets, .. }) => {
                                session.resource_observer
                                    .handle_cache_invalidate_admin_event(event_id, targets)
                                    .await;
                            }
                            Ok(event) => {
                                if watch_admin_event_matches(&event, &session.user_id, &session.room_id) {
                                    tracing::info!(
                                        user_id = %session.user_id,
                                        room_id = %session.room_id,
                                        "Resource watch terminating after admin event"
                                    );
                                    break Ok(());
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(
                                    lagged = n,
                                    user_id = %session.user_id,
                                    room_id = %session.room_id,
                                    "Resource watch admin event channel lagged, re-subscribing and verifying access"
                                );
                                admin_rx = session.event_service.subscribe_admin_events();
                                if let Err(reason) = session.ensure_realtime_room_access().await {
                                    tracing::info!(
                                        user_id = %session.user_id,
                                        room_id = %session.room_id,
                                        reason,
                                        "Resource watch access is no longer valid after admin event lag"
                                    );
                                    break Ok(());
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                break Err("Admin event channel closed".to_string());
                            }
                        }
                    }
                    () = async {
                        match session
                            .resource_observer
                            .next_playback_snapshot_refresh_deadline()
                            .await {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        if let Err(error) = session
                            .resource_observer
                            .refresh_expired_playback_snapshot_observations()
                            .await
                        {
                            break Err(error);
                        }
                    }
                }
            }
        }
        .await;

        session.resource_observer.clear_observations().await;
        session.event_service.unsubscribe(&session.connection_id);
        session
            .connection_service
            .unregister(&session.connection_id)
            .await;
        result
    }
}

impl ResourceWatchSession {
    async fn ensure_realtime_room_access(&self) -> Result<(), String> {
        let room = self
            .room_service
            .get_room(&self.room_id)
            .await
            .map_err(|error| error.to_string())?;
        if room.is_banned {
            return Err("This room has been banned".to_string());
        }
        if room.status.is_closed() {
            return Err("This room is closed and not accepting new connections".to_string());
        }
        if self.principal.is_guest() {
            return self.ensure_guest_admission_for_action().await;
        }
        match probe_realtime_membership_access_with_room(&self.room_service, &room, &self.user_id)
            .await
        {
            Ok(RealtimeMembershipAccess::Allowed(_)) => Ok(()),
            Ok(RealtimeMembershipAccess::Denied(reason)) => Err(reason),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn ensure_guest_admission_for_action(&self) -> Result<(), String> {
        match guest_admission_denial_reason(
            &self.room_service,
            &self.room_id,
            &self.user_id,
            &self.principal,
        )
        .await
        {
            Ok(Some(reason)) => Err(reason),
            Ok(None) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn check_realtime_permission(&self, permission: RoomPermission) -> Result<(), String> {
        if self.principal.is_guest() {
            let permissions = self
                .room_service
                .get_guest_permissions(&self.room_id)
                .await
                .map_err(|error| error.to_string())?;
            if permissions.has(permission) {
                Ok(())
            } else {
                Err("Guests do not have permission to perform this action".to_string())
            }
        } else {
            self.room_service
                .check_permission(&self.room_id, &self.user_id, permission)
                .await
                .map_err(|error| error.to_string())
        }
    }

    async fn ensure_observe_resource_allowed(
        &self,
        observe: &ObserveResource,
    ) -> Result<(), String> {
        let Some(resource) = observe.resource.as_ref() else {
            return Err("observe resource is required".to_string());
        };

        match resource {
            crate::proto::client::observe_resource::Resource::PlaybackState(_)
            | crate::proto::client::observe_resource::Resource::RoomSettings(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                Ok(())
            }
            crate::proto::client::observe_resource::Resource::PlaylistItems(_) => {
                if self.principal.is_guest() {
                    Err("Guests cannot observe playlist items".to_string())
                } else {
                    Ok(())
                }
            }
            crate::proto::client::observe_resource::Resource::RoomMembers(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                self.check_realtime_permission(RoomPermission::VIEW_MEMBER_LIST)
                    .await
            }
            crate::proto::client::observe_resource::Resource::ChatEvents(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                self.check_realtime_permission(RoomPermission::VIEW_CHAT_HISTORY)
                    .await
            }
            crate::proto::client::observe_resource::Resource::PlaybackSnapshot(_) => {
                if self.principal.is_guest() {
                    Err(
                        "Guests cannot observe playback snapshots because playback snapshots may depend on signed-in provider credentials"
                            .to_string(),
                    )
                } else {
                    Ok(())
                }
            }
        }
    }
}

pub fn watch_playback_state_observe(
    req: crate::proto::client::WatchPlaybackStateRequest,
) -> Result<ObserveResource, String> {
    let playback_state = req
        .playback_state
        .ok_or_else(|| "playback_state watch body is required".to_string())?;
    build_watch_observe(
        WatchResourceKind::PlaybackState,
        req.delivery_mode,
        crate::proto::client::observe_resource::Resource::PlaybackState(playback_state),
    )
}

pub fn watch_playback_snapshot_observe(
    req: crate::proto::client::WatchPlaybackSnapshotRequest,
) -> Result<ObserveResource, String> {
    let playback_snapshot = req
        .playback_snapshot
        .ok_or_else(|| "playback_snapshot watch body is required".to_string())?;
    build_watch_observe(
        WatchResourceKind::PlaybackSnapshot,
        req.delivery_mode,
        crate::proto::client::observe_resource::Resource::PlaybackSnapshot(playback_snapshot),
    )
}

pub fn watch_room_settings_observe(
    req: crate::proto::client::WatchRoomSettingsRequest,
) -> Result<ObserveResource, String> {
    let room_settings = req
        .room_settings
        .ok_or_else(|| "room_settings watch body is required".to_string())?;
    build_watch_observe(
        WatchResourceKind::RoomSettings,
        req.delivery_mode,
        crate::proto::client::observe_resource::Resource::RoomSettings(room_settings),
    )
}

pub fn watch_playlist_items_observe(
    req: crate::proto::client::WatchPlaylistItemsRequest,
) -> Result<ObserveResource, String> {
    let playlist_items = req
        .playlist_items
        .ok_or_else(|| "playlist_items watch body is required".to_string())?;
    build_watch_observe(
        WatchResourceKind::PlaylistItems,
        req.delivery_mode,
        crate::proto::client::observe_resource::Resource::PlaylistItems(playlist_items),
    )
}

pub fn watch_room_members_observe(
    req: crate::proto::client::WatchRoomMembersRequest,
) -> Result<ObserveResource, String> {
    let room_members = req
        .room_members
        .ok_or_else(|| "room_members watch body is required".to_string())?;
    build_watch_observe(
        WatchResourceKind::RoomMembers,
        req.delivery_mode,
        crate::proto::client::observe_resource::Resource::RoomMembers(room_members),
    )
}

pub fn watch_chat_events_observe(
    req: crate::proto::client::WatchChatEventsRequest,
) -> Result<ObserveResource, String> {
    let chat_events = req.chat_events.unwrap_or_default();
    build_watch_observe(
        WatchResourceKind::ChatEvents,
        req.delivery_mode,
        crate::proto::client::observe_resource::Resource::ChatEvents(chat_events),
    )
}

fn build_watch_observe(
    kind: WatchResourceKind,
    delivery_mode: i32,
    resource: crate::proto::client::observe_resource::Resource,
) -> Result<ObserveResource, String> {
    Ok(ObserveResource {
        observe_id: kind.observe_id().to_string(),
        delivery_mode,
        resource: Some(resource),
    })
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

fn user_notification_server_message(
    notification_id: impl Into<String>,
    notification_type: impl Into<String>,
    title: impl Into<String>,
    content: impl Into<String>,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> ServerMessage {
    let notification_id = notification_id.into();
    let notification_type = notification_type.into();
    let title = title.into();
    let content = content.into();
    let data = serde_json::json!({
        "type": "user_notification",
        "notification_id": &notification_id,
        "notification_type": &notification_type,
        "title": &title,
        "content": &content,
    });

    ServerMessage {
        message: Some(crate::proto::client::server_message::Message::Notification(
            crate::proto::client::UserNotification {
                notification_id,
                notification_type,
                title,
                content,
                data: data.to_string(),
                timestamp: timestamp.timestamp(),
            },
        )),
    }
}

fn system_notification_server_message(
    message: impl Into<String>,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> Result<ServerMessage, String> {
    let message = required_realtime_text(&message.into(), "system notification message", 1024)?;
    let data = serde_json::json!({
        "type": "system_notification",
        "notification_type": "system_announcement",
        "title": &message,
        "content": &message,
    });

    Ok(ServerMessage {
        message: Some(crate::proto::client::server_message::Message::Notification(
            crate::proto::client::UserNotification {
                notification_id: String::new(),
                notification_type: "system_announcement".to_string(),
                title: message.clone(),
                content: message,
                data: data.to_string(),
                timestamp: timestamp.timestamp(),
            },
        )),
    })
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
    principal: RealtimePrincipal,
    user_id: UserId,
    username: String,
    connection_id: String,
    room_service: Arc<RoomService>,
    /// `ChatService` for chat message handling with business logic.
    /// Chat messages are processed through `ChatService::send_message()`
    /// which handles permission checks, content filtering, rate limiting, and persistence.
    chat_service: Arc<ChatService>,
    event_service: Arc<dyn RealtimeEventService>,
    playback_fanout: Arc<dyn PlaybackFanoutService>,
    chat_event_dispatcher: Arc<dyn ChatEventDispatcher>,
    /// Optional notification service for direct real-time push to connected clients.
    /// When set, the handler subscribes to notification events and pushes them
    /// without depending on the gRPC notification-to-realtime bridge.
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
        Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<RealtimeEvent>>>>,
    /// Authenticated member/settings snapshot validated during `pre_join()`.
    pending_initial_join_state: Arc<tokio::sync::Mutex<Option<InitialRealtimeJoinState>>>,
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

pub struct StreamMessageHandlerConfig {
    pub room_id: RoomId,
    pub principal: RealtimePrincipal,
    pub connection_id: Option<String>,
    pub room_service: Arc<RoomService>,
    pub chat_service: Arc<ChatService>,
    pub event_service: Arc<dyn RealtimeEventService>,
    pub connection_service: Arc<dyn RealtimeConnectionService>,
    pub rate_limiter: Arc<dyn RequestRateLimiterService>,
    pub rate_limit_config: Arc<RateLimitConfig>,
    pub content_filter: Arc<ContentFilter>,
    pub public_id_codec: Arc<crate::PublicIdCodec>,
    pub sender: Arc<dyn MessageSender>,
    pub concurrency_config: Arc<MessageConcurrencyConfig>,
}

#[derive(Clone)]
pub struct StreamMessageHandlerRuntime {
    pub playback_snapshot_service: Option<Arc<dyn PlaybackSnapshotService>>,
    pub playlist_items_snapshot_service: Option<Arc<dyn PlaylistItemsSnapshotService>>,
    pub room_members_snapshot_service: Option<Arc<dyn RoomMembersSnapshotService>>,
    pub room_settings_snapshot_service: Option<Arc<dyn RoomSettingsSnapshotService>>,
    pub playback_fanout: Arc<dyn PlaybackFanoutService>,
    pub chat_event_dispatcher: Arc<dyn ChatEventDispatcher>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub ws_message_rate_limit: Option<u32>,
    pub heartbeat_schedule: Option<HeartbeatSchedule>,
    pub filter_private_ice_candidates: bool,
}

impl StreamMessageHandlerRuntime {
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn local(event_service: Arc<dyn RealtimeEventService>) -> Self {
        Self {
            playback_snapshot_service: None,
            playlist_items_snapshot_service: None,
            room_members_snapshot_service: None,
            room_settings_snapshot_service: None,
            playback_fanout: default_playback_fanout_service(
                crate::realtime_fanout::local_realtime_fanout_service(event_service.clone()),
            ),
            chat_event_dispatcher: crate::chat_event_dispatcher::default_chat_event_dispatcher(
                event_service.clone(),
            ),
            notification_service: None,
            ws_message_rate_limit: None,
            heartbeat_schedule: None,
            filter_private_ice_candidates: true,
        }
    }
}

impl Clone for StreamMessageHandler {
    fn clone(&self) -> Self {
        Self {
            room_id: self.room_id,
            principal: self.principal.clone(),
            user_id: self.user_id,
            username: self.username.clone(),
            connection_id: self.connection_id.clone(),
            room_service: Arc::clone(&self.room_service),
            chat_service: Arc::clone(&self.chat_service),
            event_service: Arc::clone(&self.event_service),
            playback_fanout: Arc::clone(&self.playback_fanout),
            chat_event_dispatcher: Arc::clone(&self.chat_event_dispatcher),
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
            pending_initial_join_state: Arc::clone(&self.pending_initial_join_state),
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

    fn error_server_message(error: impl Into<crate::impls::ApiError>) -> ServerMessage {
        let api_error: crate::impls::ApiError = error.into();
        ServerMessage {
            message: Some(crate::proto::client::server_message::Message::Error(
                api_error.to_proto_error(),
            )),
        }
    }

    fn validate_webrtc_recipient(&self, recipient: &str) -> Result<(), String> {
        let Some((target_actor_id, target_conn_id)) = recipient.rsplit_once(':') else {
            return Err(
                "WebRTC recipient must be formatted as public_actor_id:conn_id".to_string(),
            );
        };

        let target = self
            .connection_service
            .get_connection(target_conn_id)
            .ok_or_else(|| "Target connection is no longer active".to_string())?;

        if target.actor_id != target_actor_id {
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
        let Some((target_actor_id, target_conn_id)) = recipient.rsplit_once(':') else {
            return false;
        };

        if target_conn_id != self.connection_id {
            return false;
        }

        let Some(current) = self.connection_service.get_connection(&self.connection_id) else {
            return false;
        };

        current.actor_id == target_actor_id
            && current.room_id.as_ref() == Some(&self.room_id)
            && current.rtc_joined
    }

    fn ice_candidate_contains_private_ip(candidate: &str) -> bool {
        candidate
            .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '[' | ']'))
            .filter_map(|part| part.parse::<std::net::IpAddr>().ok())
            .any(is_private_ice_candidate_ip)
    }

    pub fn new(config: StreamMessageHandlerConfig) -> Self {
        let runtime = StreamMessageHandlerRuntime::local(config.event_service.clone());
        Self::new_with_runtime(config, runtime)
    }

    pub fn new_with_runtime(
        config: StreamMessageHandlerConfig,
        runtime: StreamMessageHandlerRuntime,
    ) -> Self {
        let StreamMessageHandlerConfig {
            room_id,
            principal,
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
        } = config;
        let connection_id = connection_id.unwrap_or_else(Self::generate_connection_id);
        let user_id = principal.connection_user_id();
        let username = principal.username().to_string();
        let heartbeat_schedule = runtime
            .heartbeat_schedule
            .unwrap_or_else(HeartbeatSchedule::production);
        let membership_cache = Arc::new(
            moka::sync::Cache::builder()
                .time_to_live(heartbeat_schedule.membership_cache_ttl())
                .build(),
        );
        let room_settings_snapshot_service = runtime
            .room_settings_snapshot_service
            .unwrap_or_else(|| default_room_settings_snapshot_service(Arc::clone(&room_service)));
        let room_actor = principal.room_actor(room_id);
        let chat_event_dispatcher = runtime.chat_event_dispatcher;
        let playback_fanout = runtime.playback_fanout;
        let resource_observer = Arc::new(ResourceObserver::new(ResourceObserverParams {
            room_id,
            user_id,
            actor: room_actor,
            connection_id: connection_id.clone(),
            room_service: Arc::clone(&room_service),
            public_id_codec: Arc::clone(&public_id_codec),
            sender: Arc::clone(&sender),
            playback_snapshot_service: runtime.playback_snapshot_service.clone(),
            playlist_items_snapshot_service: runtime.playlist_items_snapshot_service.clone(),
            room_members_snapshot_service: runtime.room_members_snapshot_service.clone(),
            room_settings_snapshot_service: Arc::clone(&room_settings_snapshot_service),
        }));

        Self {
            room_id,
            principal,
            user_id,
            username,
            connection_id,
            room_service,
            chat_service,
            event_service,
            playback_fanout,
            chat_event_dispatcher,
            notification_service: runtime.notification_service,
            connection_service,
            rate_limiter,
            rate_limit_config,
            content_filter,
            public_id_codec,
            sender,
            playback_snapshot_service: runtime.playback_snapshot_service,
            playlist_items_snapshot_service: runtime.playlist_items_snapshot_service,
            room_members_snapshot_service: runtime.room_members_snapshot_service,
            room_settings_snapshot_service,
            resource_observer,
            ws_message_rate_limit: runtime.ws_message_rate_limit.unwrap_or(50),
            has_webrtc_session: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            skip_cleanup_user_left: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            membership_cache,
            pending_room_event_rx: Arc::new(tokio::sync::Mutex::new(None)),
            pending_initial_join_state: Arc::new(tokio::sync::Mutex::new(None)),
            concurrency_config,
            last_progress_write: Arc::new(tokio::sync::Mutex::new(None)),
            heartbeat_schedule,
            filter_private_ice_candidates: runtime.filter_private_ice_candidates,
        }
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

    fn public_room_id(&self) -> Result<String, String> {
        self.public_id_codec
            .encode_room_id(self.room_id)
            .map_err(|error| format!("Failed to encode room public id: {error}"))
    }

    fn public_actor_id(&self) -> Result<String, String> {
        self.principal.public_actor_id(&self.public_id_codec)
    }

    fn is_own_join_event(&self, event: &RealtimeEvent, public_actor_id: &str) -> bool {
        match event {
            RealtimeEvent::UserJoined { user_id, .. } => {
                !self.principal.is_guest() && *user_id == self.user_id
            }
            RealtimeEvent::GuestJoined { guest_id, .. } => {
                self.principal.is_guest() && guest_id == public_actor_id
            }
            _ => false,
        }
    }

    async fn guest_permissions(&self) -> Result<RoomPermissionSet, synctv_core::Error> {
        self.room_service.get_guest_permissions(&self.room_id).await
    }

    async fn check_realtime_permission(
        &self,
        permission: RoomPermission,
    ) -> Result<(), synctv_core::Error> {
        if self.principal.is_guest() {
            let permissions = self.guest_permissions().await?;
            if permissions.has(permission) {
                Ok(())
            } else {
                Err(synctv_core::Error::Authorization(
                    "Guests do not have permission to perform this action".to_string(),
                ))
            }
        } else {
            self.room_service
                .check_permission(&self.room_id, &self.user_id, permission)
                .await
        }
    }

    async fn ensure_observe_resource_allowed(
        &self,
        observe: &crate::proto::client::ObserveResource,
    ) -> Result<(), String> {
        let Some(resource) = observe.resource.as_ref() else {
            return Err("observe resource is required".to_string());
        };

        match resource {
            crate::proto::client::observe_resource::Resource::PlaybackState(_)
            | crate::proto::client::observe_resource::Resource::RoomSettings(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                Ok(())
            }
            crate::proto::client::observe_resource::Resource::PlaylistItems(_) => {
                if self.principal.is_guest() {
                    Err("Guests cannot observe playlist items".to_string())
                } else {
                    Ok(())
                }
            }
            crate::proto::client::observe_resource::Resource::RoomMembers(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                self.check_realtime_permission(RoomPermission::VIEW_MEMBER_LIST)
                    .await
                    .map_err(|e| e.to_string())
            }
            crate::proto::client::observe_resource::Resource::ChatEvents(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                self.check_realtime_permission(RoomPermission::VIEW_CHAT_HISTORY)
                    .await
                    .map_err(|e| e.to_string())
            }
            crate::proto::client::observe_resource::Resource::PlaybackSnapshot(_) => {
                if self.principal.is_guest() {
                    Err(
                        "Guests cannot observe playback snapshots because playback snapshots may depend on signed-in provider credentials"
                            .to_string(),
                    )
                } else {
                    Ok(())
                }
            }
        }
    }

    async fn guest_admission_denial_reason(&self) -> Result<Option<String>, RealtimeJoinError> {
        guest_admission_denial_reason(
            &self.room_service,
            &self.room_id,
            &self.user_id,
            &self.principal,
        )
        .await
    }

    async fn prepare_initial_realtime_join_state(
        &self,
    ) -> Result<Result<InitialRealtimeJoinState, String>, RealtimeJoinError> {
        if self.principal.is_guest() {
            return Ok(match self.guest_admission_denial_reason().await? {
                Some(reason) => Err(reason),
                None => Ok(InitialRealtimeJoinState {
                    member: None,
                    room_settings: None,
                }),
            });
        }

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
            return Ok(Err(
                "User is no longer allowed to use real-time messaging".to_string()
            ));
        }
        if user.deleted_at.is_some() {
            return Ok(Err("User account is no longer available".to_string()));
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
            return Ok(Err("This room has been banned".to_string()));
        }
        if room.status == RoomStatus::Closed {
            return Ok(Err(
                "This room is closed and not accepting new connections".to_string()
            ));
        }

        let membership_lookup =
            probe_realtime_membership_access_with_room(&self.room_service, &room, &self.user_id)
                .await;
        let member = match membership_lookup {
            Ok(RealtimeMembershipAccess::Allowed(member)) => member,
            Ok(RealtimeMembershipAccess::Denied(reason)) => return Ok(Err(reason)),
            Err(error) => {
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
        };

        let room_settings = self.room_service.get_room_settings(&self.room_id).await.map_err(
            |error| {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to load room settings during pre_join; rejecting connection because permission snapshots must fail closed"
                );
                RealtimeJoinError::from(crate::impls::ApiError::from(error))
            },
        )?;

        Ok(Ok(InitialRealtimeJoinState {
            member: Some(member),
            room_settings: Some(room_settings),
        }))
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
            .register_actor(
                self.connection_id.clone(),
                self.user_id,
                self.public_actor_id()?,
            )
            .await
        {
            tracing::warn!("Failed to register connection: {}", e);
            return Err(RealtimeJoinError::from(
                crate::runtime::RealtimeAdmissionError::from_runtime_message(e),
            ));
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
            return Err(RealtimeJoinError::from(
                crate::runtime::RealtimeAdmissionError::from_runtime_message(e),
            ));
        }

        let initial_join_state = match self.prepare_initial_realtime_join_state().await {
            Ok(state) => state,
            Err(error) => {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.connection_service
                    .unregister(&self.connection_id)
                    .await;
                return Err(error);
            }
        };
        let initial_join_state = match initial_join_state {
            Ok(state) => state,
            Err(reason) => {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.connection_service
                    .unregister(&self.connection_id)
                    .await;
                return Err(RealtimeJoinError::PermissionDenied(reason));
            }
        };

        if let Err(error) = self
            .cache_initial_realtime_join_state(initial_join_state)
            .await
        {
            self.skip_cleanup_user_left
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.connection_service
                .unregister(&self.connection_id)
                .await;
            return Err(error);
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

    async fn cache_initial_realtime_join_state(
        &self,
        state: InitialRealtimeJoinState,
    ) -> Result<(), RealtimeJoinError> {
        let mut pending_state = self.pending_initial_join_state.lock().await;
        if pending_state.is_some() {
            return Err(RealtimeJoinError::Internal(
                "Initial realtime join state is already cached".to_string(),
            ));
        }
        *pending_state = Some(state);
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
                    "Failed to subscribe to realtime events during pre_join: {e}"
                ))
            })?;
        *pending_rx = Some(event_rx);

        Ok(())
    }

    async fn take_room_event_subscription(
        &self,
    ) -> Result<tokio::sync::mpsc::Receiver<RealtimeEvent>, String> {
        if let Some(event_rx) = self.pending_room_event_rx.lock().await.take() {
            return Ok(event_rx);
        }

        self.event_service
            .subscribe_with_id(self.room_id, self.user_id, self.connection_id.clone())
            .await
            .map(|(event_rx, _connection_id)| event_rx)
            .map_err(|e| format!("Failed to subscribe to realtime events: {e}"))
    }

    /// Run the complete message loop using unified IO abstraction.
    ///
    /// This is the recommended method that handles both sending and receiving
    /// in a single unified loop using the `StreamMessage` trait.
    ///
    /// This method:
    /// 1. Registers the connection and joins the room (enforcing limits)
    /// 2. Subscribes to realtime events and forwards them to the client
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
        let room_id_str = self.public_room_id()?;

        // Pre-join caches the room subscription so there is no gap between
        // admission success and the transport starting its receive loop.
        let mut event_rx = self.take_room_event_subscription().await?;
        let event_actor_id = self.public_actor_id()?;

        // Subscribe to disconnect signals
        let mut disconnect_rx = self.connection_service.subscribe_disconnect();

        // Subscribe to admin events (KickUser, etc.) for cross-replica disconnect propagation.
        // KickUser events arrive via Redis PubSub on the admin channel and are not
        // delivered through the room-level event subscription, so each connection
        // must independently monitor admin events and disconnect when targeted.
        let mut admin_rx = self.event_service.subscribe_admin_events();

        // Subscribe to notification events directly so WebSocket clients receive
        // notifications even when the gRPC notification bridge is not running.
        let mut notification_rx = self
            .notification_service
            .as_ref()
            .map(|svc| svc.subscribe_events());

        // Fetch member data and room settings once and reuse them for the join
        // payload and realtime event. Authenticated users must have both so
        // outbound permission snapshots cannot silently fall back to role-only
        // defaults when a read fails.
        let initial_join = self.take_initial_realtime_join_state(&room_id_str).await?;

        // Send initial user joined notification.
        // If the transport is already gone here, we still need to run cleanup()
        // because pre_join() already registered the connection and subscribed state
        // will be established below.
        if let Err(error) = stream.send(self.create_user_joined_message(
            &room_id_str,
            initial_join.member.as_ref(),
            initial_join.room_settings.as_ref(),
        )?) {
            tracing::error!(
                "Failed to send initial UserJoined message in run_after_join(): {error}"
            );
            self.skip_cleanup_user_left
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.cleanup(&room_id_str).await;
            return Ok(());
        }

        // Broadcast UserJoined event to other replicas
        self.broadcast_user_joined(
            initial_join.member.as_ref(),
            initial_join.room_settings.as_ref(),
        )
        .await;

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
                            self.connection_service.record_message(&self.connection_id);

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

                // Realtime event (broadcast to client)
                event = event_rx.recv() => {
                    if let Some(event) = event {
                        // Filter WebRTC signaling: only deliver to the intended recipient.
                        // SDP data contains IP addresses, so broadcasting to all room
                        // members is both a privacy leak and causes incorrect WebRTC behavior.
                                if let RealtimeEvent::WebRTCSignaling { ref to, .. } = event {
                                    if !self.current_connection_matches_webrtc_recipient(to) {
                                        continue;
                                    }
                                }
                                if self.is_own_join_event(&event, &event_actor_id) {
                                    continue;
                                }
                        if !matches!(event, RealtimeEvent::ChatMessageEvent { .. }) {
                            let mut send_failed = false;
                            let messages = match realtime_event_to_server_messages(
                                &event,
                                &room_id_str,
                                &self.public_id_codec,
                            ) {
                                Ok(messages) => messages,
                                Err(error) => {
                                    tracing::error!(
                                        room_id = %self.room_id,
                                        event_id = %event.event_id(),
                                        error = %error,
                                        "Failed to convert realtime event to server message"
                                    );
                                    break;
                                }
                            };
                            for msg in messages {
                                if let Err(e) = stream.send(msg) {
                                    tracing::error!("Failed to send server message: {}", e);
                                    send_failed = true;
                                    break;
                                }
                            }
                            if send_failed {
                                break;
                            }
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
                        tracing::error!("Realtime event channel closed");
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
                        Ok(synctv_realtime::sync::DisconnectSignal::Connection(conn_id)) => {
                            if conn_id == self.connection_id {
                                tracing::info!(
                                    connection_id = %self.connection_id,
                                    "Received disconnect signal for this connection"
                                );
                                break;
                            }
                        }
                        Ok(synctv_realtime::sync::DisconnectSignal::User(uid)) => {
                            if uid == self.user_id {
                                tracing::info!(
                                    user_id = %self.user_id,
                                    "Received disconnect signal for this user (room kick or platform ban)"
                                );
                                self.skip_cleanup_user_left
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Ok(synctv_realtime::sync::DisconnectSignal::Room(rid)) => {
                            if rid == self.room_id {
                                tracing::info!(
                                    room_id = %self.room_id,
                                    "Received disconnect signal for this room"
                                );
                                // Room deletion already published RoomDeleted;
                                // skip redundant UserLeft.
                                self.skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Ok(synctv_realtime::sync::DisconnectSignal::UserFromRoom { user_id: uid, room_id: rid }) => {
                            if uid == self.user_id && rid == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
                                    "Received disconnect signal: kicked from room"
                                );
                                // The leave_room API already published UserLeft;
                                // skip redundant broadcast in cleanup().
                                self.skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Channel lagged: we may have missed critical disconnect signals.
                            // Re-subscribe to get a fresh receiver so future signals are not lost,
                            // then verify membership to catch any missed room kick or platform ban.
                            tracing::warn!(
                                lagged = n,
                                user_id = %self.user_id,
                                room_id = %self.room_id,
                                "Disconnect signal channel lagged, re-subscribing and verifying membership"
                            );
                            disconnect_rx = self.connection_service.subscribe_disconnect();

                            match realtime_membership_denial_reason(
                                &self.room_service,
                                &self.room_id,
                                &self.user_id,
                            )
                            .await
                            {
                                Ok(Some(reason)) => {
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
                                Ok(None) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "Failed to verify membership after disconnect signal lag"
                                    );
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::error!("Disconnect signal channel closed");
                            break;
                        }
                    }
                }

                // Admin events from cluster (cross-replica room kick or platform ban propagation)
                admin_event = admin_rx.recv() => {
                    match admin_event {
                        Ok(RealtimeEvent::KickUser { ref user_id, ref reason, .. }) => {
                            // Invalidate membership cache immediately so the disconnected user
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
                        Ok(RealtimeEvent::KickUserFromRoom { ref user_id, ref room_id, ref reason, .. }) => {
                            // Invalidate membership cache immediately so the kicked or platform-banned
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
                        Ok(RealtimeEvent::UserLeft { ref user_id, ref room_id, .. }) => {
                            if *user_id == self.user_id && *room_id == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
                                    "Received cross-replica UserLeft event, disconnecting"
                                );
                                // UserLeft was already published by the leave_room
                                // or delete_room API call. Skip the redundant
                                // broadcast in cleanup().
                                self.skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Ok(RealtimeEvent::UserNotification { ref user_id, ref title, ref content, ref notification_type, ref notification_id, timestamp, .. }) => {
                            // Uses the dedicated Notification variant (not ErrorMessage abuse).
                            if *user_id == self.user_id {
                                let msg = user_notification_server_message(
                                    notification_id.clone(),
                                    notification_type.clone(),
                                    title.clone(),
                                    content.clone(),
                                    timestamp,
                                );
                                if let Err(e) = stream.send(msg) {
                                    tracing::error!("Failed to push notification to WebSocket: {}", e);
                                    break;
                                }
                            }
                        }
                        Ok(RealtimeEvent::SystemNotification { ref message, timestamp, .. }) => {
                            let msg = match system_notification_server_message(message.clone(), timestamp) {
                                Ok(msg) => msg,
                                Err(error) => {
                                    tracing::error!(error = %error, "Invalid system notification realtime event");
                                    break;
                                }
                            };
                            if let Err(e) = stream.send(msg) {
                                tracing::error!("Failed to push system notification to WebSocket: {}", e);
                                break;
                            }
                        }
                        Ok(RealtimeEvent::ProviderCredentialChanged { ref event_id, ref user_id, ref provider, ref server_id, .. }) => {
                            self.resource_observer.handle_provider_credential_changed_admin_event(
                                event_id,
                                user_id,
                                provider,
                                server_id,
                            )
                            .await;
                        }
                        Ok(RealtimeEvent::CacheInvalidate { ref event_id, ref targets, .. }) => {
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

                            match realtime_membership_denial_reason(
                                &self.room_service,
                                &self.room_id,
                                &self.user_id,
                            )
                            .await
                            {
                                Ok(Some(reason)) => {
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
                                Ok(None) => {}
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
                // verifies the user is still a valid (active member of the room. This catches cases where the disconnect
                // signal channel lagged and the room kick or platform ban signal was lost.
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

                    if self.principal.is_guest() {
                        match self.guest_admission_denial_reason().await {
                            Ok(Some(reason)) => {
                                tracing::info!(
                                    room_id = %self.room_id,
                                    user_id = %self.user_id,
                                    reason,
                                    "Periodic check: guest access is no longer valid, disconnecting"
                                );
                                break;
                            }
                            Ok(None) => continue,
                            Err(error) => {
                                tracing::warn!(
                                    error = %error,
                                    user_id = %self.user_id,
                                    "Periodic guest access check failed (will retry)"
                                );
                                continue;
                            }
                        }
                    }

                    // Check membership cache first to avoid unnecessary DB queries.
                    let cache_key = (self.room_id, self.user_id);
                    if let Some(cached) = self.membership_cache.get(&cache_key) {
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

    /// Create the initial `UserJoined` server message.
    fn create_user_joined_message(
        &self,
        room_id: &str,
        member: Option<&synctv_core::models::RoomMember>,
        room_settings: Option<&synctv_core::models::RoomSettings>,
    ) -> Result<ServerMessage, String> {
        use crate::proto::client::server_message::Message;
        use crate::proto::client::UserJoinedRoom;
        use synctv_proto::common::RoomMember as ProtoRoomMember;

        let (role_proto, permissions, added, removed, admin_added, admin_removed) =
            if self.principal.is_guest() {
                let permissions = self
                    .principal
                    .guest_identity()
                    .map_or(RoomPermissionSet::default_guest().0, |identity| {
                        identity.permissions.0
                    });
                (
                    synctv_proto::common::RoomMemberRole::Guest as i32,
                    permissions,
                    0,
                    0,
                    0,
                    0,
                )
            } else {
                match member {
                    Some(member) => {
                        let settings = room_settings.ok_or_else(|| {
                        "authenticated UserJoined payload requires room settings for permissions"
                            .to_string()
                    })?;
                        let effective = self
                            .room_service
                            .permission_service()
                            .effective_member_permissions(member, settings);
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
                            synctv_core::models::RoomPermissionSet::default_member().0,
                            0,
                            0,
                            0,
                            0,
                        )
                    }
                }
            };
        let user_id = self.public_actor_id()?;

        Ok(ServerMessage {
            message: Some(Message::UserJoined(UserJoinedRoom {
                room_id: room_id.to_string(),
                member: Some(ProtoRoomMember {
                    room_id: room_id.to_string(),
                    user_id,
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
        })
    }

    /// Broadcast `UserJoined` event to cluster replicas.
    async fn broadcast_user_joined(
        &self,
        member: Option<&synctv_core::models::RoomMember>,
        room_settings: Option<&synctv_core::models::RoomSettings>,
    ) {
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

        let (
            role_proto,
            permissions,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        ) = if let Some(identity) = self.principal.guest_identity() {
            (
                synctv_proto::common::RoomMemberRole::Guest as i32,
                identity.permissions,
                synctv_core::models::RoomPermissionSet(0),
                synctv_core::models::RoomPermissionSet(0),
                synctv_core::models::RoomPermissionSet(0),
                synctv_core::models::RoomPermissionSet(0),
            )
        } else {
            match member {
                Some(member) => {
                    let Some(settings) = room_settings else {
                        tracing::error!(
                            room_id = %self.room_id,
                            user_id = %self.user_id,
                            connection_id = %self.connection_id,
                            "Skipping UserJoined broadcast because room settings are missing"
                        );
                        return;
                    };
                    let effective = self
                        .room_service
                        .permission_service()
                        .effective_member_permissions(member, settings);
                    let role = room_role_to_proto(member.role);
                    (
                        role,
                        effective,
                        synctv_core::models::RoomPermissionSet(member.added_permissions),
                        synctv_core::models::RoomPermissionSet(member.removed_permissions),
                        synctv_core::models::RoomPermissionSet(member.admin_added_permissions),
                        synctv_core::models::RoomPermissionSet(member.admin_removed_permissions),
                    )
                }
                None => {
                    // Fallback: if we can't fetch membership, use Member defaults
                    (
                        synctv_proto::common::RoomMemberRole::Member as i32,
                        synctv_core::models::RoomPermissionSet::default_member(),
                        synctv_core::models::RoomPermissionSet(0),
                        synctv_core::models::RoomPermissionSet(0),
                        synctv_core::models::RoomPermissionSet(0),
                        synctv_core::models::RoomPermissionSet(0),
                    )
                }
            }
        };

        let event = if self.principal.is_guest() {
            let guest_id = match self.public_actor_id() {
                Ok(actor_id) => actor_id,
                Err(error) => {
                    tracing::error!(
                        room_id = %self.room_id,
                        user_id = %self.user_id,
                        error = %error,
                        "Skipping GuestJoined broadcast because actor public id encoding failed"
                    );
                    return;
                }
            };
            RealtimeEvent::GuestJoined {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                guest_id,
                username: self.username.clone(),
                permissions,
                role: role_proto,
                joined_at: chrono::Utc::now(),
                timestamp: chrono::Utc::now(),
            }
        } else {
            RealtimeEvent::UserJoined {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                user_id: self.user_id,
                username: self.username.clone(),
                permissions,
                role: role_proto,
                added_permissions,
                removed_permissions,
                admin_added_permissions,
                admin_removed_permissions,
                joined_at: chrono::Utc::now(),
                timestamp: chrono::Utc::now(),
            }
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

        // If this connection had an active WebRTC session, decrement the metric
        // and broadcast WebRtcLeave so other peers can clean up.
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
                match self.public_actor_id() {
                    Ok(actor_id) => {
                        let leave_event = RealtimeEvent::WebRTCLeave {
                            event_id: synctv_common::snanoid!(16),
                            room_id: self.room_id,
                            actor_id,
                            conn_id: self.connection_id.clone(),
                            timestamp: chrono::Utc::now(),
                        };
                        self.event_service.broadcast(leave_event);
                    }
                    Err(error) => {
                        tracing::error!(
                            room_id = %self.room_id,
                            user_id = %self.user_id,
                            connection_id = %self.connection_id,
                            error = %error,
                            "Skipping WebRTC leave broadcast because actor public id encoding failed"
                        );
                    }
                }

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

        // If the disconnect was triggered by a realtime event that already
        // published UserLeft, skip the redundant broadcast to avoid duplicate
        // UserLeft events.
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
                    "Distributed same-user presence lookup failed during cleanup; using local presence fallback for UserLeft broadcast"
                );
                should_broadcast_user_left(has_other_local_connection, Err(()))
            }
        };

        // Broadcast UserLeft BEFORE unregistering from the connection manager.
        // This order prevents state divergence: if the broadcast reaches subscribers
        // while this connection is still registered, they see a consistent view.
        // Previously, unregistering first could leave the hub with a stale subscriber
        // if the broadcast was delayed or had no receivers.
        let event = if self.principal.is_guest() {
            let guest_id = match self.public_actor_id() {
                Ok(actor_id) => actor_id,
                Err(error) => {
                    tracing::error!(
                        room_id = %self.room_id,
                        user_id = %self.user_id,
                        error = %error,
                        "Skipping GuestLeft broadcast because actor public id encoding failed"
                    );
                    self.connection_service
                        .unregister(&self.connection_id)
                        .await;
                    self.event_service.unsubscribe(&self.connection_id);
                    return;
                }
            };
            RealtimeEvent::GuestLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                guest_id,
                username: self.username.clone(),
                timestamp: chrono::Utc::now(),
            }
        } else {
            RealtimeEvent::UserLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                user_id: self.user_id,
                username: self.username.clone(),
                timestamp: chrono::Utc::now(),
            }
        };
        let retry_event_template = event.clone();
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
                let username = self.username.clone();
                let connection_id = self.connection_id.clone();
                let room_label = room_id.to_string();
                let retry_event_template = retry_event_template.clone();

                let semaphore = Arc::clone(&USER_LEFT_RETRY_SEMAPHORE);
                let permit = semaphore.try_acquire_owned();

                match permit {
                    Ok(permit) => {
                        tracing::warn!(
                            user = %username,
                            room = %room_label,
                            connection = %connection_id,
                            "UserLeft distributed publish missed; starting retry task"
                        );

                        spawn_monitored("userleft_retry", async move {
                            let _permit = permit; // Hold permit for duration of retry task

                            let mut delay_ms = USER_LEFT_RETRY_INITIAL_DELAY_MS;

                            for attempt in 1..=USER_LEFT_RETRY_MAX_RETRIES {
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms))
                                    .await;

                                let retry_event =
                                    rebuild_leave_event_for_retry(&retry_event_template);

                                let retry_outcome =
                                    event_service.retry_broadcast_outcome(retry_event);

                                if retry_outcome.satisfies(
                                    RealtimeDeliveryRequirement::DistributedWhenAvailable,
                                ) {
                                    tracing::info!(
                                        user = %username,
                                        room = %room_label,
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
                                    room = %room_label,
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
                                room = %room_label,
                                connection = %connection_id,
                                "UserLeft event permanently lost after {} retry attempts; other replicas may have stale user state",
                                USER_LEFT_RETRY_MAX_RETRIES
                            );
                        });
                    }
                    Err(_) => {
                        tracing::warn!(
                            user = %username,
                            room = %room_label,
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
    /// 2. Subscribes to realtime events and forwards them to the client
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
            .register_actor(
                self.connection_id.clone(),
                self.user_id,
                self.public_actor_id()?,
            )
            .await?;

        self.pre_join_after_registration()
            .await
            .map_err(String::from)?;

        let cancel_token = tokio_util::sync::CancellationToken::new();
        let room_id_str = self.public_room_id()?;

        // Fetch member data and room settings once and reuse them for the join
        // payload and realtime event. Authenticated users must have both so
        // outbound permission snapshots cannot silently fall back to role-only
        // defaults when a read fails.
        let initial_join = self.take_initial_realtime_join_state(&room_id_str).await?;

        // Send initial UserJoined message to the client (mirrors run() behavior)
        let initial_msg = self.create_user_joined_message(
            &room_id_str,
            initial_join.member.as_ref(),
            initial_join.room_settings.as_ref(),
        )?;
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
            self.broadcast_user_joined(
                initial_join.member.as_ref(),
                initial_join.room_settings.as_ref(),
            )
            .await;
        }

        // Use bounded channel to prevent memory exhaustion from fast clients
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ClientMessage>(256);

        let room_id_str = self.public_room_id()?;
        let event_connection_id = self.connection_id.clone();
        let event_actor_id = self.public_actor_id()?;
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
                                if let RealtimeEvent::WebRTCSignaling { ref to, .. } = event {
                                    let is_target = to.rsplit_once(':').is_some_and(
                                        |(actor_id, conn)| {
                                            actor_id == event_actor_id
                                                && conn == event_connection_id
                                        },
                                    );
                                    if !is_target {
                                        continue;
                                    }
                                }
                                if event_handler.is_own_join_event(&event, &event_actor_id) {
                                    continue;
                                }

                                let is_room_shutdown = matches!(
                                    event,
                                    RealtimeEvent::RoomDeleted { .. }
                                        | RealtimeEvent::RoomBanned { .. }
                                        | RealtimeEvent::RoomOwnerInactive { .. }
                                );

                                if !matches!(event, RealtimeEvent::ChatMessageEvent { .. }) {
                                    let messages = match realtime_event_to_server_messages(
                                        &event,
                                        &room_id_str,
                                        &public_id_codec,
                                    ) {
                                        Ok(messages) => messages,
                                        Err(error) => {
                                            tracing::error!(
                                                room_id = %event_handler.room_id,
                                                event_id = %event.event_id(),
                                                error = %error,
                                                "Failed to convert realtime event to server message"
                                            );
                                            event_token.cancel();
                                            break;
                                        }
                                    };
                                    for msg in messages {
                                        if let Err(e) = sender.send(msg) {
                                            tracing::error!("Failed to send message: {}", e);
                                            event_token.cancel();
                                            break;
                                        }
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
                                handler.connection_service.record_message(&handler.connection_id);

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
            let is_guest = self.principal.is_guest();

            spawn_monitored("messaging_disconnect_monitor", async move {
                loop {
                    tokio::select! {
                        () = disconnect_token.cancelled() => break,

                        signal = disconnect_rx.recv() => {
                            let should_disconnect = match &signal {
                                Ok(synctv_realtime::sync::DisconnectSignal::Connection(conn_id)) => {
                                    *conn_id == connection_id
                                }
                                Ok(synctv_realtime::sync::DisconnectSignal::User(uid)) => {
                                    *uid == user_id
                                }
                                Ok(synctv_realtime::sync::DisconnectSignal::Room(rid)) => {
                                    *rid == room_id
                                }
                                Ok(synctv_realtime::sync::DisconnectSignal::UserFromRoom { user_id: uid, room_id: rid }) => {
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
                                if !is_guest {
                                    match realtime_membership_denial_reason(
                                        &room_service,
                                        &room_id,
                                        &user_id,
                                    )
                                    .await
                                    {
                                        Ok(Some(reason)) => {
                                            tracing::info!(
                                                user_id = %user_id,
                                                room_id = %room_id,
                                                reason,
                                                "start() real-time access is no longer valid after disconnect signal lag"
                                            );
                                            skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                            disconnect_token.cancel();
                                            break;
                                        }
                                        Ok(None) => {}
                                        Err(error) => {
                                            tracing::warn!(
                                                error = %error,
                                                user_id = %user_id,
                                                room_id = %room_id,
                                                "start() failed to verify membership after disconnect signal lag"
                                            );
                                        }
                                    }
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
                            // Uses the dedicated Notification variant (not ErrorMessage abuse).
                            if let Ok(RealtimeEvent::UserNotification { user_id: uid, title, content, notification_type, notification_id, timestamp, .. }) = &admin_event {
                                if *uid == user_id {
                                    let msg = user_notification_server_message(
                                        notification_id.clone(),
                                        notification_type.clone(),
                                        title.clone(),
                                        content.clone(),
                                        *timestamp,
                                    );
                                    if let Err(e) = admin_sender.send(msg) {
                                        tracing::error!("Failed to push notification in start(): {}", e);
                                        disconnect_token.cancel();
                                        break;
                                    }
                                }
                                continue;
                            }
                            if let Ok(RealtimeEvent::SystemNotification { message, timestamp, .. }) = &admin_event {
                                let msg = match system_notification_server_message(message.clone(), *timestamp) {
                                    Ok(msg) => msg,
                                    Err(error) => {
                                        tracing::error!(error = %error, "Invalid system notification realtime event");
                                        disconnect_token.cancel();
                                        break;
                                    }
                                };
                                if let Err(e) = admin_sender.send(msg) {
                                    tracing::error!(
                                        "Failed to push system notification in start(): {}",
                                        e
                                    );
                                    disconnect_token.cancel();
                                    break;
                                }
                                continue;
                            }
                            if let Ok(RealtimeEvent::ProviderCredentialChanged { event_id, user_id: changed_user_id, provider, server_id, .. }) = &admin_event {
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
                            if let Ok(RealtimeEvent::CacheInvalidate { event_id, targets, .. }) = &admin_event
                            {
                                admin_handler
                                    .resource_observer
                                    .handle_cache_invalidate_admin_event(event_id, targets)
                                    .await;
                                continue;
                            }
                            let should_disconnect = match &admin_event {
                                Ok(RealtimeEvent::KickUser { user_id: uid, .. }) => {
                                    *uid == user_id
                                }
                                Ok(
                                    RealtimeEvent::KickUserFromRoom { user_id: uid, room_id: rid, .. }
                                    | RealtimeEvent::UserLeft { user_id: uid, room_id: rid, .. },
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
                                if !is_guest {
                                    match realtime_membership_denial_reason(
                                        &room_service,
                                        &room_id,
                                        &user_id,
                                    )
                                    .await
                                    {
                                        Ok(Some(reason)) => {
                                            tracing::info!(
                                                user_id = %user_id,
                                                room_id = %room_id,
                                                reason,
                                                "start() real-time access is no longer valid after admin event lag"
                                            );
                                            skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                            disconnect_token.cancel();
                                            break;
                                        }
                                        Ok(None) => {}
                                        Err(error) => {
                                            tracing::warn!(
                                                error = %error,
                                                user_id = %user_id,
                                                room_id = %room_id,
                                                "start() failed to verify membership after admin event lag"
                                            );
                                        }
                                    }
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
        // Verifies every 25-35 seconds that the user is still a valid, member.
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
            let heartbeat_handler = self.clone();
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

                            if heartbeat_handler.principal.is_guest() {
                                match heartbeat_handler.guest_admission_denial_reason().await {
                                    Ok(Some(reason)) => {
                                        tracing::info!(
                                            user_id = %heartbeat_user_id,
                                            room_id = %heartbeat_room_id,
                                            reason,
                                            "start() periodic check: guest access is no longer valid, disconnecting"
                                        );
                                        heartbeat_token.cancel();
                                        break;
                                    }
                                    Ok(None) => continue,
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            user_id = %heartbeat_user_id,
                                            "start() periodic guest access check failed (will retry)"
                                        );
                                        continue;
                                    }
                                }
                            }

                            match realtime_membership_denial_reason(
                                &heartbeat_room_service,
                                &heartbeat_room_id,
                                &heartbeat_user_id,
                            )
                            .await
                            {
                                Ok(Some(reason)) => {
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
                                Ok(None) => {}
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
        let cleanup_room_id = self.public_room_id()?;
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
                if self.principal.is_guest() {
                    return Err("Guests cannot send chat messages".to_string());
                }

                // ChatService handles permissions, room settings, rate limiting,
                // content filtering, persistence, and event dispatch.
                self.handle_chat_message_with_control(chat_msg, control)
                    .await?;
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
                self.ensure_observe_resource_allowed(observe).await?;
                self.resource_observer
                    .handle_observe_resource(observe)
                    .await?;
                self.resource_observer
                    .replay_chat_events_after(&self.chat_service, observe)
                    .await?;
                self.resource_observer
                    .replay_room_resource_events_after(observe)
                    .await?;
            }
            Some(Message::UnobserveResource(unobserve)) => {
                self.resource_observer
                    .handle_unobserve_resource(unobserve)
                    .await?;
            }
            None => {
                return Err("Empty message".to_string());
            }
        }

        Ok(())
    }

    async fn handle_chat_message_with_control(
        &self,
        chat_msg: &crate::proto::client::ChatMessageSend,
        control: Option<&ExecutionControl>,
    ) -> Result<(), String> {
        if self.principal.is_guest() {
            self.ensure_guest_admission_for_action().await?;
            return Err("Guests cannot send chat messages".to_string());
        }

        // Delegate to ChatService which handles permission checks, content filtering,
        // rate limiting, and persistence (no fallback path).
        let images = chat_msg
            .images
            .iter()
            .map(proto_chat_image_to_core)
            .collect::<Result<Vec<_>, _>>()?;
        let reply_to_message_id = parse_optional_chat_message_id(&chat_msg.reply_to_message_id)?;
        let playback_state = self
            .room_service
            .playback_service()
            .get_state(&self.room_id)
            .await
            .map_err(|error| format!("Failed to load playback state for chat metadata: {error}"))?;
        let metadata = chat_metadata_for_send(
            serde_json::Value::Object(Default::default()),
            &chat_msg.display_position,
            &chat_msg.display_color,
            Some(&playback_state),
        )?;
        let outcome = self
            .chat_service
            .send_message_event_with_control_outcome(
                SendChatMessage {
                    room_id: self.room_id,
                    user_id: self.user_id,
                    client_message_id: (!chat_msg.client_message_id.trim().is_empty())
                        .then(|| chat_msg.client_message_id.trim().to_string()),
                    content: chat_msg.content.clone(),
                    message_type: if images.is_empty() {
                        ChatMessageType::Text
                    } else {
                        ChatMessageType::Image
                    },
                    reply_to_message_id,
                    metadata,
                    images,
                },
                control,
            )
            .await
            .map_err(|e| e.to_string())?;
        // Touch room activity to prevent TTL expiry on active rooms
        self.room_service.touch_room_activity(self.room_id).await;

        // Track chat message metric
        synctv_core::metrics::http::CHAT_MESSAGES_TOTAL
            .with_label_values(&[] as &[&str])
            .inc();

        if outcome.inserted {
            self.chat_event_dispatcher.dispatch(&outcome.event);
        }

        Ok(())
    }

    async fn ensure_guest_admission_for_action(&self) -> Result<(), String> {
        match self.guest_admission_denial_reason().await {
            Ok(Some(reason)) => Err(reason),
            Ok(None) => Ok(()),
            Err(error) => Err(error.to_string()),
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

        if self.principal.is_guest() {
            self.ensure_guest_admission_for_action().await?;
        }

        // Check permission
        self.check_realtime_permission(RoomPermission::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        let conn_id = self.connection_id.clone();

        if self.connection_service.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&offer.to)?;

        // P2P relay path: forward offer to target peer via cluster
        let event = RealtimeEvent::WebRTCSignaling {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            message_type: WebRTCSignalKind::Offer,
            from: format!("{}|{}", self.public_actor_id()?, conn_id),
            to: offer.to.clone(),
            data: offer.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Cross-replica WebRTC signaling must reach Redis when distributed mode is enabled.
        let outcome = self.event_service.broadcast_outcome(event);
        if !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable) {
            tracing::warn!(
                room_id = %self.room_id,
                "WebRTC offer realtime delivery did not satisfy distributed delivery requirements"
            );
            synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
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

        if self.principal.is_guest() {
            self.ensure_guest_admission_for_action().await?;
        }

        // Check permission
        self.check_realtime_permission(RoomPermission::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        let conn_id = self.connection_id.clone();

        if self.connection_service.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&answer.to)?;

        // Create event with server-set 'from' field
        let event = RealtimeEvent::WebRTCSignaling {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            message_type: WebRTCSignalKind::Answer,
            from: format!("{}|{}", self.public_actor_id()?, conn_id),
            to: answer.to.clone(),
            data: answer.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Cross-replica WebRTC signaling must reach Redis when distributed mode is enabled.
        let outcome = self.event_service.broadcast_outcome(event);
        if !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable) {
            tracing::warn!(
                room_id = %self.room_id,
                "WebRTC answer realtime delivery did not satisfy distributed delivery requirements"
            );
            synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
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

        if self.principal.is_guest() {
            self.ensure_guest_admission_for_action().await?;
        }

        // Check permission
        self.check_realtime_permission(RoomPermission::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        let conn_id = self.connection_id.clone();

        if self.connection_service.get_connection(&conn_id).is_none() {
            return Err("Connection not found".to_string());
        }
        self.validate_webrtc_recipient(&candidate.to)?;

        // P2P relay path: forward ICE candidate to target peer via cluster
        let event = RealtimeEvent::WebRTCSignaling {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            message_type: WebRTCSignalKind::IceCandidate,
            from: format!("{}|{}", self.public_actor_id()?, conn_id),
            to: candidate.to.clone(),
            data: candidate.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Cross-replica ICE signaling must reach Redis when distributed mode is enabled.
        let outcome = self.event_service.broadcast_outcome(event);
        if !outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable) {
            tracing::warn!(
                room_id = %self.room_id,
                "ICE candidate realtime delivery did not satisfy distributed delivery requirements"
            );
            synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
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
        if self.principal.is_guest() {
            self.ensure_guest_admission_for_action().await?;
        }

        // Check permission
        self.check_realtime_permission(RoomPermission::USE_WEBRTC)
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
        let event = RealtimeEvent::WebRTCJoin {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            actor_id: self.public_actor_id()?,
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
        let event = RealtimeEvent::WebRTCLeave {
            event_id: synctv_common::snanoid!(16),
            room_id: self.room_id,
            actor_id: self.public_actor_id()?,
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
    /// Drift bounds: ignores reports where the reported position deviates
    /// more than 30 seconds from the expected server-side position (computed
    /// from last known time + wall-clock elapsed). This keeps progress
    /// heartbeats weak: play/pause/seek messages remain the explicit sync path,
    /// while stale heartbeats cannot spoof arbitrary playback positions.
    async fn handle_playback_progress(
        &self,
        report: &crate::proto::client::PlaybackProgressReport,
    ) -> Result<(), String> {
        if report.position < 0.0 {
            return Err("Playback position must be non-negative".to_string());
        }

        // Only members with SEEK permission may update the canonical playback
        // state via progress reports. Without this check any room member could
        // silently rewrite the server-side position by sending crafted progress
        // messages, effectively acting as an unauthorized seek.
        self.check_realtime_permission(RoomPermission::PLAY_CONTROL)
            .await
            .map_err(|e| e.to_string())?;
        if self.principal.is_guest() {
            return Err("Guests cannot update canonical playback progress".to_string());
        }

        let playback_service = self.room_service.playback_service();
        let state = playback_service
            .get_state(&self.room_id)
            .await
            .map_err(|e| e.to_string())?;
        let expected_source = crate::impls::client::build_playback_source_expectation(
            report.expected_media_id.clone(),
            report.expected_playlist_id.clone(),
            report.expected_target_hash.clone(),
            &self.public_id_codec,
        )
        .map_err(|error| error.to_string())?;
        if expected_source
            .as_ref()
            .is_some_and(|expected_source| !expected_source.matches(&state))
        {
            tracing::debug!(
                user_id = %self.user_id,
                room_id = %self.room_id,
                "Playback progress report ignored: playback source changed"
            );
            return Ok(());
        }

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
                let elapsed_ms = u64::try_from(elapsed_ms)
                    .map_err(|_| "playback progress elapsed time exceeds u64::MAX".to_string())?;
                Duration::from_millis(elapsed_ms).as_secs_f64()
            };
            let expected_position = state.position + (elapsed_secs * state.speed);
            let drift = (report.position - expected_position).abs();

            if drift > PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS {
                tracing::warn!(
                    user_id = %self.user_id,
                    room_id = %self.room_id,
                    reported = report.position,
                    expected = expected_position,
                    drift = drift,
                    "Playback progress report ignored: drift exceeds {} seconds",
                    PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS
                );
                return Ok(());
            }

            // Throttle DB writes: only persist if position changed by >1s
            // or >5s elapsed since last write. This reduces write amplification
            // from every 3-5s heartbeat to only meaningful position changes.
            let should_write = {
                let guard = self.last_progress_write.lock().await;
                should_persist_playback_progress(*guard, report.position)
            };

            if should_write {
                // Update the canonical position and broadcast to same-replica
                // clients so they can detect drift. The sender is excluded by
                // event_id filtering (each connection ignores events it originated).
                let prepared_fanout = self.playback_fanout.prepare_state_changed_outbox_fanout(
                    PlaybackFanoutActor::new(self.user_id, &self.username),
                );
                let mut request = PlaybackUpdateRequest::new(
                    self.room_id,
                    self.user_id,
                    PlaybackStatePatch::new(None, Some(report.position), None),
                )
                .with_expected_version(Some(state.version))
                .with_outbox(Some(prepared_fanout.outbox_factory()));
                if let Some(expected_source) = expected_source {
                    request = request.with_expected_source(expected_source);
                }
                let update_result = playback_service.update_playback_state(request).await;

                let updated_state = match update_result {
                    Ok(updated_state) => updated_state,
                    Err(CoreError::OptimisticLockConflict) => {
                        tracing::debug!(
                            room_id = %self.room_id,
                            "Playback progress report ignored: playback state changed concurrently"
                        );
                        return Ok(());
                    }
                    Err(e) => {
                        return Err(format!(
                            "Failed to update playback state from progress report: {e}"
                        ));
                    }
                };

                prepared_fanout.publish_after_outbox_commit();
                // Record the write for throttling
                {
                    let mut guard = self.last_progress_write.lock().await;
                    *guard = Some((report.position, tokio::time::Instant::now()));
                }

                if let Some(service) = &self.playback_snapshot_service {
                    service
                        .report_provider_playback_progress(
                            &updated_state,
                            report.position,
                            !report.is_playing,
                            false,
                        )
                        .await;
                }
            }
        }

        Ok(())
    }

    /// Handle playback state update command from WebSocket.
    async fn handle_playback_update(
        &self,
        update: &crate::proto::client::UpdatePlaybackRequest,
    ) -> Result<(), String> {
        self.check_realtime_permission(RoomPermission::PLAY_CONTROL)
            .await
            .map_err(|e| e.to_string())?;
        if self.principal.is_guest() {
            return Err("Guests cannot control playback".to_string());
        }
        let command =
            crate::impls::client::build_update_playback(update.clone(), &self.public_id_codec)
                .map_err(|error| error.to_string())?;
        let previous_state = self
            .room_service
            .playback_service()
            .get_state(&self.room_id)
            .await
            .map_err(|error| {
                format!("Failed to load previous playback state for provider lifecycle transition: {error}")
            })?;

        let crate::impls::client::PlaybackUpdateCommand::Patch {
            playing,
            position,
            speed,
            version,
            expected_source,
        } = command;
        let playback_service = self.room_service.playback_service();
        let prepared_fanout =
            self.playback_fanout
                .prepare_state_changed_outbox_fanout(PlaybackFanoutActor::new(
                    self.user_id,
                    &self.username,
                ));
        let mut request = PlaybackUpdateRequest::new(
            self.room_id,
            self.user_id,
            PlaybackStatePatch::new(playing, position, speed),
        )
        .with_expected_version(version)
        .with_outbox(Some(prepared_fanout.outbox_factory()));
        if let Some(expected_source) = expected_source {
            request = request.with_expected_source(expected_source);
        }
        let state = playback_service
            .update_playback_state(request)
            .await
            .map_err(|e| e.to_string())?;
        prepared_fanout.publish_after_outbox_commit();
        if let Some(service) = &self.playback_snapshot_service {
            service
                .handle_provider_lifecycle_transition(Some(&previous_state), &state)
                .await;
        }

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

/// Convert a realtime event into one or more server messages.
fn realtime_event_to_server_messages(
    event: &synctv_realtime::sync::RealtimeEvent,
    room_id: &str,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Vec<ServerMessage>, String> {
    use crate::proto::client::server_message::Message;
    use crate::proto::client::{
        ChatMessageReceive, ErrorMessage, MediaRemovedBatch, MediaUpdated, PlaybackStateChanged,
        PlaylistCreated, PlaylistDeleted, PlaylistReordered, PlaylistUpdated, RoomSettingsChanged,
        ServerMessage, UserJoinedRoom, UserLeftRoom,
    };
    use synctv_proto::common::RoomMember;
    use synctv_realtime::sync::RealtimeEvent;

    let encode_user = |id| {
        public_id_codec
            .encode_user_id(id)
            .map_err(|error| format!("Failed to encode realtime event user id: {error}"))
    };
    let encode_room = |id| {
        public_id_codec
            .encode_room_id(id)
            .map_err(|error| format!("Failed to encode realtime event room id: {error}"))
    };
    let encode_media = |id| {
        public_id_codec
            .encode_media_id(id)
            .map_err(|error| format!("Failed to encode realtime event media id: {error}"))
    };
    let encode_playlist = |id| {
        public_id_codec
            .encode_playlist_id(id)
            .map_err(|error| format!("Failed to encode realtime event playlist id: {error}"))
    };

    let messages = match event {
        RealtimeEvent::ChatMessage {
            user_id,
            username,
            message,
            timestamp,
            display_position,
            display_color,
            ..
        } => vec![ServerMessage {
            message: Some(Message::Chat(ChatMessageReceive {
                id: event.event_id().to_string(),
                room_id: room_id.to_string(),
                user_id: encode_user(*user_id)?,
                username: required_realtime_text(username, "user username", 50)?,
                content: message.clone(),
                timestamp: timestamp.timestamp(),
                display_position: optional_chat_metadata_text(
                    display_position.as_deref(),
                    "display position",
                    64,
                )?
                .unwrap_or_default(),
                display_color: optional_chat_metadata_text(
                    display_color.as_deref(),
                    "display color",
                    64,
                )?
                .unwrap_or_default(),
                client_message_id: String::new(),
                status: crate::proto::client::ChatMessageStatus::Active as i32,
                version: 1,
                edited_at: 0,
                deleted_at: 0,
                reply_to_message_id: String::new(),
                images: Vec::new(),
                deleted_by_user_id: String::new(),
                delete_reason: String::new(),
                playback_media_id: String::new(),
                playback_playlist_id: String::new(),
                playback_target: Vec::new(),
                playback_target_hash: String::new(),
                playback_position_seconds: None,
                reactions: Vec::new(),
                reaction_count: 0,
            })),
        }],
        RealtimeEvent::ChatMessageEvent { .. } => Vec::new(),
        RealtimeEvent::PlaybackStateChanged { state, .. } => vec![ServerMessage {
            message: Some(Message::PlaybackState(PlaybackStateChanged {
                room_id: room_id.to_string(),
                state: Some(playback_state_to_proto(
                    state,
                    &encode_room,
                    &encode_media,
                    &encode_playlist,
                )?),
            })),
        }],
        RealtimeEvent::UserJoined {
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
                    user_id: encode_user(*user_id)?,
                    username: required_realtime_text(username, "user username", 50)?,
                    role: validated_room_member_role(*role)?,
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
        RealtimeEvent::GuestJoined {
            guest_id,
            username,
            permissions,
            role,
            joined_at,
            ..
        } => vec![ServerMessage {
            message: Some(Message::UserJoined(UserJoinedRoom {
                room_id: room_id.to_string(),
                member: Some(RoomMember {
                    room_id: room_id.to_string(),
                    user_id: required_realtime_text(guest_id, "guest id", 128)?,
                    username: required_realtime_text(username, "guest username", 64)?,
                    role: validated_room_member_role(*role)?,
                    permissions: permissions.0,
                    added_permissions: 0,
                    removed_permissions: 0,
                    admin_added_permissions: 0,
                    admin_removed_permissions: 0,
                    joined_at: joined_at.timestamp(),
                    is_online: true,
                }),
            })),
        }],
        RealtimeEvent::UserLeft { user_id, .. } => vec![ServerMessage {
            message: Some(Message::UserLeft(UserLeftRoom {
                room_id: room_id.to_string(),
                user_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::GuestLeft { guest_id, .. } => vec![ServerMessage {
            message: Some(Message::UserLeft(UserLeftRoom {
                room_id: room_id.to_string(),
                user_id: required_realtime_text(guest_id, "guest id", 128)?,
            })),
        }],
        RealtimeEvent::MediaAdded {
            media_id,
            media_title,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaAdded(crate::proto::client::MediaAdded {
                room_id: room_id.to_string(),
                media_id: encode_media(*media_id)?,
                name: required_realtime_text(media_title, "media title", 512)?,
                creator_username: required_realtime_text(username, "creator username", 50)?,
                creator_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::MediaRemoved {
            media_id,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaRemoved(crate::proto::client::MediaRemoved {
                room_id: room_id.to_string(),
                media_id: encode_media(*media_id)?,
                removed_by: required_realtime_text(username, "removed-by username", 50)?,
                removed_by_user_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::MediaRemovedBatch {
            media_ids,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaRemovedBatch(MediaRemovedBatch {
                room_id: room_id.to_string(),
                media_ids: encode_non_empty_media_ids(
                    media_ids,
                    &encode_media,
                    "media removed batch",
                )?,
                removed_by: required_realtime_text(username, "removed-by username", 50)?,
                removed_by_user_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::MediaUpdated {
            media_id,
            media_title,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaUpdated(MediaUpdated {
                room_id: room_id.to_string(),
                media_id: encode_media(*media_id)?,
                name: required_realtime_text(media_title, "media title", 512)?,
                updated_by: required_realtime_text(username, "updated-by username", 50)?,
                updated_by_user_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::PlaylistReordered {
            media_ids,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::PlaylistReordered(PlaylistReordered {
                room_id: room_id.to_string(),
                media_ids: encode_non_empty_media_ids(
                    media_ids,
                    &encode_media,
                    "playlist reorder",
                )?,
                reordered_by: required_realtime_text(username, "reordered-by username", 50)?,
                reordered_by_user_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::PlaylistCreated { playlist, .. } => vec![ServerMessage {
            message: Some(Message::PlaylistCreated(PlaylistCreated {
                room_id: room_id.to_string(),
                playlist: Some(crate::impls::client::convert::try_playlist_to_proto(
                    playlist,
                    0,
                    public_id_codec,
                )?),
            })),
        }],
        RealtimeEvent::PlaylistUpdated { playlist, .. } => vec![ServerMessage {
            message: Some(Message::PlaylistUpdated(PlaylistUpdated {
                room_id: room_id.to_string(),
                playlist: Some(crate::impls::client::convert::try_playlist_to_proto(
                    playlist,
                    0,
                    public_id_codec,
                )?),
            })),
        }],
        RealtimeEvent::PlaylistDeleted { playlist_id, .. } => vec![ServerMessage {
            message: Some(Message::PlaylistDeleted(PlaylistDeleted {
                room_id: room_id.to_string(),
                playlist_id: encode_playlist(*playlist_id)?,
            })),
        }],
        RealtimeEvent::PermissionChanged {
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
                    user_id: encode_user(*target_user_id)?,
                    role: validated_room_member_role(*role)?,
                    effective_permissions: new_permissions.0,
                    added_permissions: added_permissions.0,
                    removed_permissions: removed_permissions.0,
                    admin_added_permissions: admin_added_permissions.0,
                    admin_removed_permissions: admin_removed_permissions.0,
                    updated_by: required_realtime_text(
                        changed_by_username,
                        "permission updated-by username",
                        50,
                    )?,
                },
            )),
        }],
        RealtimeEvent::RoomSettingsChanged {
            settings_json,
            version,
            ..
        } => vec![ServerMessage {
            message: Some(Message::RoomSettings(RoomSettingsChanged {
                room_id: room_id.to_string(),
                settings: validated_room_settings_json(settings_json)?,
                version: validated_non_negative_version(*version, "room settings")?,
            })),
        }],
        RealtimeEvent::WebRTCSignaling {
            message_type,
            from,
            to,
            data,
            ..
        } => {
            let msg = match message_type {
                WebRTCSignalKind::Offer => ServerMessage {
                    message: Some(Message::WebrtcOffer(crate::proto::client::WebRtcOffer {
                        from: required_realtime_text(from, "webrtc from", 256)?,
                        to: required_realtime_text(to, "webrtc to", 256)?,
                        data: data.clone(),
                    })),
                },
                WebRTCSignalKind::Answer => ServerMessage {
                    message: Some(Message::WebrtcAnswer(crate::proto::client::WebRtcAnswer {
                        from: required_realtime_text(from, "webrtc from", 256)?,
                        to: required_realtime_text(to, "webrtc to", 256)?,
                        data: data.clone(),
                    })),
                },
                WebRTCSignalKind::IceCandidate => ServerMessage {
                    message: Some(Message::WebrtcIceCandidate(
                        crate::proto::client::WebRtcIceCandidate {
                            from: required_realtime_text(from, "webrtc from", 256)?,
                            to: required_realtime_text(to, "webrtc to", 256)?,
                            data: data.clone(),
                        },
                    )),
                },
            };
            vec![msg]
        }
        RealtimeEvent::WebRTCJoin {
            actor_id,
            conn_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::WebrtcJoin(crate::proto::client::WebRtcJoin {
                user_id: required_realtime_text(actor_id, "webrtc actor id", 128)?,
                conn_id: required_realtime_text(conn_id, "webrtc connection id", 128)?,
                username: required_realtime_text(username, "webrtc username", 64)?,
            })),
        }],
        RealtimeEvent::WebRTCLeave {
            actor_id, conn_id, ..
        } => vec![ServerMessage {
            message: Some(Message::WebrtcLeave(crate::proto::client::WebRtcLeave {
                user_id: required_realtime_text(actor_id, "webrtc actor id", 128)?,
                conn_id: required_realtime_text(conn_id, "webrtc connection id", 128)?,
            })),
        }],
        RealtimeEvent::SystemNotification {
            message, timestamp, ..
        } => vec![system_notification_server_message(
            message.clone(),
            *timestamp,
        )?],
        RealtimeEvent::RoomDeleted { .. } => {
            // Notify WebSocket clients that the room has been deleted
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room has been deleted".to_string(),
                    code: crate::impls::error_codes::NOT_FOUND,
                    detail: String::new(),
                })),
            }]
        }
        RealtimeEvent::RoomBanned { .. } => {
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room has been banned".to_string(),
                    code: crate::impls::error_codes::FORBIDDEN,
                    detail: String::new(),
                })),
            }]
        }
        RealtimeEvent::RoomOwnerInactive { .. } => {
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room is unavailable because its creator is not active".to_string(),
                    code: crate::impls::error_codes::FORBIDDEN,
                    detail: String::new(),
                })),
            }]
        }
        RealtimeEvent::KickPublisher { .. }
        | RealtimeEvent::KickUser { .. }
        | RealtimeEvent::KickUserFromRoom { .. }
        | RealtimeEvent::RoomCreated { .. }
        | RealtimeEvent::CacheInvalidate { .. }
        | RealtimeEvent::ProviderCredentialChanged { .. }
        | RealtimeEvent::UserNotification { .. } => {
            // Admin/internal events are handled by other channels,
            // not forwarded to WebSocket clients via the room event path
            vec![]
        }
    };
    Ok(messages)
}

fn playback_state_to_proto(
    state: &RoomPlaybackState,
    encode_room: &impl Fn(RoomId) -> Result<String, String>,
    encode_media: &impl Fn(synctv_core::models::MediaId) -> Result<String, String>,
    encode_playlist: &impl Fn(synctv_core::models::PlaylistId) -> Result<String, String>,
) -> Result<crate::proto::client::PlaybackState, String> {
    let position = state.computed_position();
    if !position.is_finite() || position < 0.0 {
        return Err("Playback position must be a finite non-negative number".to_string());
    }
    if !state.speed.is_finite() || state.speed <= 0.0 {
        return Err("Playback speed must be a finite positive number".to_string());
    }
    if state.version < 0 {
        return Err("Playback version must be non-negative".to_string());
    }

    Ok(crate::proto::client::PlaybackState {
        room_id: encode_room(state.room_id)?,
        playing_media_id: state
            .playing_media_id
            .as_ref()
            .map(|id| encode_media(*id))
            .transpose()?
            .unwrap_or_default(),
        position,
        speed: state.speed,
        is_playing: state.is_playing,
        updated_at: state.updated_at.timestamp(),
        version: state.version,
        playing_playlist_id: state
            .playing_playlist_id
            .as_ref()
            .map(|id| encode_playlist(*id))
            .transpose()?
            .unwrap_or_default(),
        target: state.target.clone(),
        target_hash: state.target_hash(),
    })
}

fn validated_room_member_role(role: i32) -> Result<i32, String> {
    let role = synctv_proto::common::RoomMemberRole::try_from(role)
        .map_err(|_| "Room member role is not defined".to_string())?;
    if role == synctv_proto::common::RoomMemberRole::Unspecified {
        return Err("Room member role is unspecified".to_string());
    }
    Ok(role as i32)
}

fn required_realtime_text(value: &str, field_name: &str, max_len: usize) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > max_len || trimmed.chars().any(char::is_control) {
        return Err(format!(
            "Realtime {field_name} must be 1-{max_len} non-control characters"
        ));
    }
    Ok(trimmed.to_string())
}

fn encode_non_empty_media_ids(
    media_ids: &[synctv_core::models::MediaId],
    encode_media: &impl Fn(synctv_core::models::MediaId) -> Result<String, String>,
    field_name: &'static str,
) -> Result<Vec<String>, String> {
    if media_ids.is_empty() {
        return Err(format!("Realtime {field_name} media_ids must not be empty"));
    }
    media_ids
        .iter()
        .map(|id| encode_media(*id))
        .collect::<Result<Vec<_>, _>>()
}

fn validated_room_settings_json(settings_json: &[u8]) -> Result<Vec<u8>, String> {
    if settings_json.is_empty() {
        return Err("Room settings JSON must not be empty".to_string());
    }
    let value: serde_json::Value = serde_json::from_slice(settings_json)
        .map_err(|error| format!("Room settings JSON is invalid: {error}"))?;
    if !value.is_object() {
        return Err("Room settings JSON must be an object".to_string());
    }
    Ok(settings_json.to_vec())
}

fn validated_non_negative_version(version: i64, field_name: &'static str) -> Result<i64, String> {
    if version < 0 {
        return Err(format!("{field_name} version must be non-negative"));
    }
    Ok(version)
}

pub(crate) fn chat_event_kind_to_proto(
    kind: ChatEventKind,
) -> crate::proto::client::ChatMessageEventKind {
    match kind {
        ChatEventKind::Created => crate::proto::client::ChatMessageEventKind::Created,
        ChatEventKind::Edited => crate::proto::client::ChatMessageEventKind::Edited,
        ChatEventKind::Deleted => crate::proto::client::ChatMessageEventKind::Deleted,
        ChatEventKind::ReactionsChanged => {
            crate::proto::client::ChatMessageEventKind::ReactionsChanged
        }
    }
}

pub(crate) fn chat_status_to_proto(
    status: ChatMessageStatus,
) -> crate::proto::client::ChatMessageStatus {
    match status {
        ChatMessageStatus::Active => crate::proto::client::ChatMessageStatus::Active,
        ChatMessageStatus::Edited => crate::proto::client::ChatMessageStatus::Edited,
        ChatMessageStatus::Deleted => crate::proto::client::ChatMessageStatus::Deleted,
    }
}

pub(crate) fn chat_display_position_from_metadata(
    metadata: &serde_json::Value,
) -> Result<String, String> {
    chat_presentation_text_from_metadata(metadata, "display_position", "display position", 64)
}

pub(crate) fn chat_display_color_from_metadata(
    metadata: &serde_json::Value,
) -> Result<String, String> {
    chat_presentation_text_from_metadata(metadata, "display_color", "display color", 64)
}

fn chat_presentation_text_from_metadata(
    metadata: &serde_json::Value,
    key: &'static str,
    field_name: &'static str,
    max_len: usize,
) -> Result<String, String> {
    let Some(presentation) = optional_chat_metadata_object(metadata, "presentation")? else {
        return Ok(String::new());
    };
    let Some(value) = presentation.get(key) else {
        return Ok(String::new());
    };
    let raw = value
        .as_str()
        .ok_or_else(|| format!("Chat {field_name} must be a string"))?;
    Ok(validate_chat_metadata_text(raw, field_name, max_len)?.unwrap_or_default())
}

pub(crate) fn chat_playback_media_id_from_metadata(
    metadata: &serde_json::Value,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<String, String> {
    let Some(id) = chat_playback_positive_id_from_metadata(metadata, "media_id")? else {
        return Ok(String::new());
    };
    let id = synctv_core::models::MediaId::try_from(id)
        .map_err(|_| "Invalid chat playback media_id".to_string())?;
    public_id_codec
        .encode_media_id(id)
        .map_err(|error| format!("Failed to encode chat playback media id: {error}"))
}

pub(crate) fn chat_playback_playlist_id_from_metadata(
    metadata: &serde_json::Value,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<String, String> {
    let Some(id) = chat_playback_positive_id_from_metadata(metadata, "playlist_id")? else {
        return Ok(String::new());
    };
    let id = synctv_core::models::PlaylistId::try_from(id)
        .map_err(|_| "Invalid chat playback playlist_id".to_string())?;
    public_id_codec
        .encode_playlist_id(id)
        .map_err(|error| format!("Failed to encode chat playback playlist id: {error}"))
}

fn chat_playback_positive_id_from_metadata(
    metadata: &serde_json::Value,
    field: &str,
) -> Result<Option<i64>, String> {
    let Some(playback) = optional_chat_metadata_object(metadata, "playback")? else {
        return Ok(None);
    };
    let Some(value) = playback.get(field) else {
        return Ok(None);
    };
    let raw = value
        .as_str()
        .ok_or_else(|| format!("Chat playback {field} must be a string"))?
        .trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let id = raw
        .parse::<i64>()
        .map_err(|_| format!("Chat playback {field} must be a positive integer"))?;
    if id <= 0 {
        return Err(format!("Chat playback {field} must be positive"));
    }
    Ok(Some(id))
}

pub(crate) struct ChatPlaybackMetadata {
    pub media_id: String,
    pub playlist_id: String,
    pub target: Vec<u8>,
    pub target_hash: String,
    pub position_seconds: Option<f64>,
}

pub(crate) fn chat_playback_metadata_from_metadata(
    metadata: &serde_json::Value,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<ChatPlaybackMetadata, String> {
    let target = chat_playback_target_from_metadata(metadata)?;
    let target_hash = if target.is_empty() {
        String::new()
    } else {
        chat_playback_target_hash(&target)
    };

    Ok(ChatPlaybackMetadata {
        media_id: chat_playback_media_id_from_metadata(metadata, public_id_codec)?,
        playlist_id: chat_playback_playlist_id_from_metadata(metadata, public_id_codec)?,
        target,
        target_hash,
        position_seconds: chat_playback_position_seconds_from_metadata(metadata)?,
    })
}

pub(crate) fn chat_playback_target_from_metadata(
    metadata: &serde_json::Value,
) -> Result<Vec<u8>, String> {
    let Some(playback) = optional_chat_metadata_object(metadata, "playback")? else {
        return Ok(Vec::new());
    };
    let Some(value) = playback.get("target_hex") else {
        return Ok(Vec::new());
    };
    let raw_target = value
        .as_str()
        .ok_or_else(|| "Chat playback target_hex must be a string".to_string())?
        .trim();
    if raw_target.is_empty() {
        return Ok(Vec::new());
    }

    hex::decode(raw_target).map_err(|error| format!("Invalid chat playback target_hex: {error}"))
}

pub(crate) fn chat_playback_position_seconds_from_metadata(
    metadata: &serde_json::Value,
) -> Result<Option<f64>, String> {
    let Some(playback) = optional_chat_metadata_object(metadata, "playback")? else {
        return Ok(None);
    };
    let Some(value) = playback.get("position_seconds") else {
        return Ok(None);
    };
    let seconds = value
        .as_f64()
        .ok_or_else(|| "Chat playback position_seconds must be a number".to_string())?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(
            "Chat playback position_seconds must be a finite non-negative number".to_string(),
        );
    }
    Ok(Some(seconds))
}

fn optional_chat_metadata_object<'a>(
    metadata: &'a serde_json::Value,
    key: &'static str,
) -> Result<Option<&'a serde_json::Map<String, serde_json::Value>>, String> {
    metadata
        .get(key)
        .map(|value| chat_metadata_object(value, key))
        .transpose()
}

fn chat_metadata_object<'a>(
    value: &'a serde_json::Value,
    key: &'static str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("Chat metadata {key} must be an object"))
}

fn validate_chat_metadata_text(
    value: &str,
    field_name: &str,
    max_len: usize,
) -> Result<Option<String>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > max_len || trimmed.chars().any(char::is_control) {
        return Err(format!(
            "Chat {field_name} must be 1-{max_len} non-control characters"
        ));
    }
    Ok(Some(trimmed.to_string()))
}

fn optional_chat_metadata_text(
    value: Option<&str>,
    field_name: &str,
    max_len: usize,
) -> Result<Option<String>, String> {
    value
        .map(|value| validate_chat_metadata_text(value, field_name, max_len))
        .transpose()
        .map(Option::flatten)
}

pub(crate) fn chat_playback_target_hash(target: &[u8]) -> String {
    hex::encode(Sha256::digest(target))
}

pub(crate) fn chat_metadata_for_send(
    base: serde_json::Value,
    display_position: &str,
    display_color: &str,
    playback_state: Option<&RoomPlaybackState>,
) -> Result<serde_json::Value, String> {
    let serde_json::Value::Object(mut metadata) = base else {
        return Err("metadata must be a JSON object".to_string());
    };
    metadata.remove("position");
    metadata.remove("color");

    let display_position = validate_chat_metadata_text(display_position, "display position", 64)?;
    let display_color = validate_chat_metadata_text(display_color, "display color", 64)?;
    let mut presentation = serde_json::Map::new();
    if let Some(display_position) = display_position {
        presentation.insert(
            "display_position".to_string(),
            serde_json::Value::String(display_position),
        );
    }
    if let Some(display_color) = display_color {
        presentation.insert(
            "display_color".to_string(),
            serde_json::Value::String(display_color),
        );
    }
    if presentation.is_empty() {
        metadata.remove("presentation");
    } else {
        metadata.insert(
            "presentation".to_string(),
            serde_json::Value::Object(presentation),
        );
    }

    if let Some(state) = playback_state
        .filter(|state| state.playing_media_id.is_some() || state.playing_playlist_id.is_some())
    {
        let mut playback = serde_json::Map::new();
        if let Some(media_id) = state.playing_media_id {
            playback.insert(
                "media_id".to_string(),
                serde_json::Value::String(media_id.as_i64().to_string()),
            );
        }
        if let Some(playlist_id) = state.playing_playlist_id {
            playback.insert(
                "playlist_id".to_string(),
                serde_json::Value::String(playlist_id.as_i64().to_string()),
            );
        }
        if !state.target.is_empty() {
            playback.insert(
                "target_hex".to_string(),
                serde_json::Value::String(hex::encode(&state.target)),
            );
        }
        let position_seconds = state.computed_position().max(0.0);
        if position_seconds.is_finite() {
            playback.insert(
                "position_seconds".to_string(),
                serde_json::json!(position_seconds),
            );
        }
        metadata.insert("playback".to_string(), serde_json::Value::Object(playback));
    } else {
        metadata.remove("playback");
    }

    Ok(serde_json::Value::Object(metadata))
}

pub(crate) fn chat_message_event_to_proto(
    event: &ChatMessageEvent,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<crate::proto::client::ChatMessageEvent, String> {
    let message = &event.message.message;
    let room_id = public_id_codec
        .encode_room_id(message.room_id)
        .map_err(|error| format!("Failed to encode chat event room id: {error}"))?;
    let user_id = message
        .user_id
        .map(|id| {
            public_id_codec
                .encode_user_id(id)
                .map_err(|error| format!("Failed to encode chat event user id: {error}"))
        })
        .transpose()?
        .unwrap_or_default();
    let deleted_by_user_id = message
        .deleted_by
        .map(|id| {
            public_id_codec
                .encode_user_id(id)
                .map_err(|error| format!("Failed to encode chat event deleted_by user id: {error}"))
        })
        .transpose()?
        .unwrap_or_default();
    let reactions = event
        .message
        .reactions
        .iter()
        .map(crate::impls::client::chat_reaction_summary_to_proto)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let reaction_count =
        crate::impls::client::chat_reaction_count(&reactions).map_err(|e| e.to_string())?;
    let playback = chat_playback_metadata_from_metadata(&message.metadata, public_id_codec)?;
    Ok(crate::proto::client::ChatMessageEvent {
        event_id: event.event_id.clone(),
        room_id: room_id.clone(),
        kind: chat_event_kind_to_proto(event.kind) as i32,
        message: Some(crate::proto::client::ChatMessageReceive {
            id: message.id.to_string(),
            room_id,
            user_id,
            username: String::new(),
            content: message.content.clone(),
            timestamp: message.created_at.timestamp(),
            display_position: chat_display_position_from_metadata(&message.metadata)?,
            display_color: chat_display_color_from_metadata(&message.metadata)?,
            client_message_id: message.client_message_id.clone().unwrap_or_default(),
            status: chat_status_to_proto(message.status) as i32,
            version: message.version,
            edited_at: message.edited_at.map_or(0, |ts| ts.timestamp()),
            deleted_at: message.deleted_at.map_or(0, |ts| ts.timestamp()),
            reply_to_message_id: message
                .reply_to_message_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            images: event
                .message
                .images
                .iter()
                .map(core_chat_image_to_proto)
                .collect::<Result<Vec<_>, _>>()?,
            deleted_by_user_id,
            delete_reason: message.delete_reason.clone().unwrap_or_default(),
            playback_media_id: playback.media_id,
            playback_playlist_id: playback.playlist_id,
            playback_target: playback.target,
            playback_target_hash: playback.target_hash,
            playback_position_seconds: playback.position_seconds,
            reactions,
            reaction_count,
        }),
        occurred_at: event.occurred_at.timestamp(),
        sequence: event.sequence,
    })
}

pub(crate) fn core_chat_image_to_proto(
    image: &synctv_core::models::ChatImage,
) -> Result<crate::proto::client::ChatImage, String> {
    Ok(crate::proto::client::ChatImage {
        id: image.id.clone(),
        storage_backend: image.storage_backend.clone(),
        object_key: image.object_key.clone(),
        url: required_chat_image_url(image)?,
        mime_type: required_chat_image_mime_type(image)?,
        size_bytes: required_chat_image_size_bytes(image)?,
        width: required_chat_image_dimension(image, image.width, "width")?,
        height: required_chat_image_dimension(image, image.height, "height")?,
        metadata: crate::impls::client::convert::json_to_vec(
            &image.metadata,
            "chat image metadata",
        )
        .map_err(|error| error.to_string())?,
    })
}

fn required_chat_image_url(image: &synctv_core::models::ChatImage) -> Result<String, String> {
    let url = image
        .url
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| "chat image url is missing".to_string())?;
    if url.is_empty() {
        return Err("chat image url is empty".to_string());
    }
    Ok(url.to_string())
}

fn required_chat_image_mime_type(image: &synctv_core::models::ChatImage) -> Result<String, String> {
    let mime_type = image
        .mime_type
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| format!("chat image {} is missing mime_type", image.id))?;
    if mime_type.is_empty() {
        return Err(format!("chat image {} has empty mime_type", image.id));
    }
    Ok(mime_type.to_string())
}

fn required_chat_image_size_bytes(image: &synctv_core::models::ChatImage) -> Result<i64, String> {
    match image.size_bytes {
        Some(size_bytes) if size_bytes > 0 => Ok(size_bytes),
        _ => Err(format!(
            "chat image {} is missing valid size_bytes",
            image.id
        )),
    }
}

fn required_chat_image_dimension(
    image: &synctv_core::models::ChatImage,
    value: Option<i32>,
    field: &'static str,
) -> Result<i32, String> {
    match value {
        Some(value) if value > 0 => Ok(value),
        _ => Err(format!("chat image {} is missing valid {field}", image.id)),
    }
}

pub(crate) fn proto_chat_image_to_core(
    image: &crate::proto::client::ChatImage,
) -> Result<NewChatImage, String> {
    let metadata = if image.metadata.is_empty() {
        serde_json::Value::Object(Default::default())
    } else {
        serde_json::from_slice(&image.metadata).map_err(|error| error.to_string())?
    };
    Ok(NewChatImage {
        id: image.id.clone(),
        storage_backend: image.storage_backend.clone(),
        object_key: image.object_key.clone(),
        url: (!image.url.trim().is_empty()).then(|| image.url.clone()),
        mime_type: (!image.mime_type.trim().is_empty()).then(|| image.mime_type.clone()),
        size_bytes: (image.size_bytes > 0).then_some(image.size_bytes),
        width: (image.width > 0).then_some(image.width),
        height: (image.height > 0).then_some(image.height),
        metadata,
    })
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
        Ok(false) | Err(()) => UserLeftDeliveryPlan::LocalAndRedis,
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

fn guest_policy_error_to_denial_reason(
    error: synctv_core::Error,
) -> Result<Option<String>, synctv_core::Error> {
    match error {
        synctv_core::Error::Authorization(reason) => Ok(Some(reason)),
        error => Err(error),
    }
}

fn rebuild_leave_event_for_retry(event: &RealtimeEvent) -> RealtimeEvent {
    match event {
        RealtimeEvent::UserLeft {
            room_id,
            user_id,
            username,
            ..
        } => RealtimeEvent::UserLeft {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.clone(),
            timestamp: chrono::Utc::now(),
        },
        RealtimeEvent::GuestLeft {
            room_id,
            guest_id,
            username,
            ..
        } => RealtimeEvent::GuestLeft {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            guest_id: guest_id.clone(),
            username: username.clone(),
            timestamp: chrono::Utc::now(),
        },
        _ => event.clone(),
    }
}

#[derive(Debug)]
enum RealtimeMembershipAccess {
    Allowed(RoomMember),
    Denied(String),
}

struct InitialRealtimeJoinState {
    member: Option<RoomMember>,
    room_settings: Option<RoomSettings>,
}

impl StreamMessageHandler {
    async fn take_initial_realtime_join_state(
        &self,
        room_id_str: &str,
    ) -> Result<InitialRealtimeJoinState, String> {
        if let Some(state) = self.pending_initial_join_state.lock().await.take() {
            return Ok(state);
        }

        if self.principal.is_guest() {
            return Ok(InitialRealtimeJoinState {
                member: None,
                room_settings: None,
            });
        }

        let member_lookup =
            probe_realtime_membership_access(&self.room_service, &self.room_id, &self.user_id)
                .await;

        let member = match member_lookup {
            Ok(RealtimeMembershipAccess::Allowed(member)) => member,
            Ok(RealtimeMembershipAccess::Denied(reason)) => {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.cleanup(room_id_str).await;
                return Err(reason);
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to fetch membership during initial real-time join"
                );
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.cleanup(room_id_str).await;
                return Err(error.to_string());
            }
        };

        let room_settings = self
            .room_service
            .get_room_settings(&self.room_id)
            .await
            .map_err(|error| {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to fetch room settings during initial real-time join"
                );
                error.to_string()
            });
        let room_settings = match room_settings {
            Ok(room_settings) => room_settings,
            Err(error) => {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.cleanup(room_id_str).await;
                return Err(error);
            }
        };

        Ok(InitialRealtimeJoinState {
            member: Some(member),
            room_settings: Some(room_settings),
        })
    }
}

async fn guest_admission_denial_reason(
    room_service: &RoomService,
    room_id: &RoomId,
    user_id: &UserId,
    principal: &RealtimePrincipal,
) -> Result<Option<String>, RealtimeJoinError> {
    let room = room_service.get_room(room_id).await.map_err(|error| {
        tracing::warn!(
            error = %error,
            room_id = %room_id,
            user_id = %user_id,
            "Failed to re-validate guest room access; rejecting connection because final admission must fail closed"
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

    let policy_denial = room_service
        .check_guest_allowed(room_id, room_service.settings_registry().map(AsRef::as_ref))
        .await
        .map_or_else(
            |error| match guest_policy_error_to_denial_reason(error) {
                Ok(reason) => Ok(reason),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        user_id = %user_id,
                        "Failed to validate guest policy"
                    );
                    Err(RealtimeJoinError::ServiceUnavailable(
                        "Guest policy validation temporarily unavailable".to_string(),
                    ))
                }
            },
            |()| Ok(None),
        )?;
    if let Some(reason) = policy_denial {
        return Ok(Some(reason));
    }

    if let Some(identity) = principal.guest_identity() {
        match guest_token_blacklist_denial_reason(
            room_service,
            room_id,
            user_id,
            &identity.token_jti,
        )
        .await
        {
            Ok(Some(reason)) => return Ok(Some(reason)),
            Ok(None) => {}
            Err(error) => return Err(error),
        }

        let current_version =
            room_service
                .get_room_guest_version(room_id)
                .await
                .map_err(|error| {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        user_id = %user_id,
                        "Failed to validate guest token version"
                    );
                    RealtimeJoinError::ServiceUnavailable(
                        "Guest access validation temporarily unavailable".to_string(),
                    )
                })?;
        if identity.room_guest_version < current_version {
            return Ok(Some(
                "Guest access for this room has been revoked".to_string(),
            ));
        }
    }

    Ok(None)
}

async fn guest_token_blacklist_denial_reason(
    room_service: &RoomService,
    room_id: &RoomId,
    user_id: &UserId,
    token_jti: &str,
) -> Result<Option<String>, RealtimeJoinError> {
    let user_service = room_service.user_service();
    let key = user_service.key_builder().guest_token_blacklist(token_jti);
    match user_service
        .token_blacklist_store()
        .is_blacklisted_checked(&key)
        .await
    {
        Ok(true) => Ok(Some("Guest token has been revoked".to_string())),
        Ok(false) => Ok(None),
        Err(error) => {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                user_id = %user_id,
                "Failed to validate guest token blacklist during realtime admission check"
            );
            Err(RealtimeJoinError::ServiceUnavailable(
                "Guest access validation temporarily unavailable".to_string(),
            ))
        }
    }
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
            if room_service
                .member_service()
                .is_in_kick_cooldown(&room.id, user_id)
                .await?
            {
                Ok(RealtimeMembershipAccess::Denied(
                    synctv_core::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE.to_string(),
                ))
            } else {
                Ok(RealtimeMembershipAccess::Denied(message))
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

async fn realtime_membership_denial_reason(
    room_service: &RoomService,
    room_id: &RoomId,
    user_id: &UserId,
) -> synctv_core::Result<Option<String>> {
    match probe_realtime_membership_access(room_service, room_id, user_id).await? {
        RealtimeMembershipAccess::Allowed(_) => Ok(None),
        RealtimeMembershipAccess::Denied(reason) => Ok(Some(reason)),
    }
}

#[inline]
fn disconnect_signal_requires_skip_cleanup(
    signal: &synctv_realtime::sync::DisconnectSignal,
    user_id: &UserId,
    room_id: &RoomId,
    connection_id: &str,
) -> bool {
    match signal {
        synctv_realtime::sync::DisconnectSignal::Connection(conn_id) => conn_id == connection_id,
        // A global user disconnect (ban/delete) must still let cleanup emit a
        // room-scoped UserLeft for the connection's current room.
        synctv_realtime::sync::DisconnectSignal::User(_uid) => false,
        synctv_realtime::sync::DisconnectSignal::Room(rid) => rid == room_id,
        synctv_realtime::sync::DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        } => uid == user_id && rid == room_id,
    }
}

#[inline]
fn admin_event_requires_skip_cleanup(
    event: &RealtimeEvent,
    user_id: &UserId,
    room_id: &RoomId,
) -> bool {
    match event {
        // A global KickUser must still allow connection cleanup to publish a
        // room-scoped UserLeft on the affected room.
        RealtimeEvent::KickUser { user_id: _uid, .. } => false,
        RealtimeEvent::RoomBanned { room_id: rid, .. }
        | RealtimeEvent::RoomOwnerInactive { room_id: rid, .. } => rid == room_id,
        RealtimeEvent::KickUserFromRoom {
            user_id: uid,
            room_id: rid,
            ..
        }
        | RealtimeEvent::UserLeft {
            user_id: uid,
            room_id: rid,
            ..
        } => uid == user_id && rid == room_id,
        _ => false,
    }
}

#[inline]
fn watch_disconnect_signal_matches(
    signal: &synctv_realtime::sync::DisconnectSignal,
    user_id: &UserId,
    room_id: &RoomId,
    connection_id: &str,
) -> bool {
    match signal {
        synctv_realtime::sync::DisconnectSignal::Connection(conn_id) => conn_id == connection_id,
        synctv_realtime::sync::DisconnectSignal::User(uid) => uid == user_id,
        synctv_realtime::sync::DisconnectSignal::Room(rid) => rid == room_id,
        synctv_realtime::sync::DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        } => uid == user_id && rid == room_id,
    }
}

#[inline]
fn watch_admin_event_matches(event: &RealtimeEvent, user_id: &UserId, room_id: &RoomId) -> bool {
    match event {
        RealtimeEvent::KickUser { user_id: uid, .. } => uid == user_id,
        RealtimeEvent::KickUserFromRoom {
            user_id: uid,
            room_id: rid,
            ..
        }
        | RealtimeEvent::UserLeft {
            user_id: uid,
            room_id: rid,
            ..
        } => uid == user_id && rid == room_id,
        RealtimeEvent::RoomDeleted { room_id: rid, .. }
        | RealtimeEvent::RoomBanned { room_id: rid, .. }
        | RealtimeEvent::RoomOwnerInactive { room_id: rid, .. } => rid == room_id,
        _ => false,
    }
}

fn parse_optional_chat_message_id(raw: &str) -> Result<Option<i64>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<i64>()
        .map(Some)
        .map_err(|_| "Invalid chat message id".to_string())
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
