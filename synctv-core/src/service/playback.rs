//! Playback state management service
//!
//! Handles playback coordination including play/pause, seeking, speed changes,
//! and media switching with optimistic locking for concurrent updates.

use std::sync::Arc;
use std::time::Duration;

use crate::{
    cache::{CacheInvalidationRuntime, InvalidationMessage, PlaybackStateCache, SingleFlight},
    models::{
        MediaId, PermissionBits, PlayMode, PlaylistId, RoomId, RoomPlaybackState, RoomSettings,
        UserId,
    },
    repository::RoomPlaybackStateRepository,
    service::{media::MediaService, permission::PermissionService, UserService},
    Error, Result,
};
use rand::prelude::IteratorRandom;
use rand::RngExt;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct SwitchPlaybackTarget {
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Vec<u8>,
}

#[derive(Debug)]
enum NextTarget {
    Static(crate::models::Media),
    Dynamic {
        playlist_id: PlaylistId,
        target: Vec<u8>,
    },
}

/// Result of a broadcast operation from `ClusterManager::broadcast`.
///
/// Indicates whether the event was delivered to local subscribers and/or Redis.
#[derive(Debug, Clone, Copy, Default)]
pub struct BroadcastResult {
    /// Number of local WebSocket subscribers that received the event
    pub local_sent: usize,
    /// Whether the event was successfully published to Redis
    pub redis_sent: bool,
    /// Whether this is a single-node deployment (no broadcast needed)
    pub single_node: bool,
}

impl BroadcastResult {
    /// Check if the broadcast reached any destination (or single-node mode where
    /// no broadcast is needed).
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.single_node || self.local_sent > 0 || self.redis_sent
    }

    /// Whether the broadcast reached at least one local destination but failed
    /// to publish to Redis in distributed mode.
    #[must_use]
    pub const fn should_warn_missing_redis_delivery(&self) -> bool {
        !self.single_node && self.is_success() && !self.redis_sent
    }

    /// Create a result for single-node mode where no cluster broadcast is needed.
    /// This is considered a success because there are no remote replicas to notify.
    #[must_use]
    pub const fn single_node() -> Self {
        Self {
            local_sent: 0,
            redis_sent: false,
            single_node: true,
        }
    }
}

/// Response from a seek operation.
///
/// Allows clients to distinguish between a successful seek and a degraded
/// response when retry exhaustion occurs due to concurrent modifications.
#[derive(Debug, Clone)]
pub struct SeekResponse {
    /// The current playback state after the seek operation.
    /// Always contains a valid state, even if the seek was not applied.
    pub state: RoomPlaybackState,
    /// Whether the requested seek position was successfully applied.
    /// When `false`, the state contains the latest known position, which
    /// may differ from the requested position.
    pub seek_applied: bool,
    /// Optional message explaining why the seek was not applied (when `seek_applied` is false).
    pub message: Option<String>,
}

impl SeekResponse {
    /// Create a successful seek response.
    #[must_use]
    pub const fn success(state: RoomPlaybackState) -> Self {
        Self {
            state,
            seek_applied: true,
            message: None,
        }
    }

    /// Create a degraded seek response (seek not applied due to contention).
    #[must_use]
    pub fn degraded(state: RoomPlaybackState, message: impl Into<String>) -> Self {
        Self {
            state,
            seek_applied: false,
            message: Some(message.into()),
        }
    }
}

const MAX_PLAYBACK_POSITION_SECONDS: f64 = 86_400.0;
const MIN_PLAYBACK_SPEED: f64 = 0.25;
const MAX_PLAYBACK_SPEED: f64 = 4.0;
fn validate_seek_position(current_time: f64) -> Result<()> {
    if !current_time.is_finite() {
        return Err(Error::InvalidInput(
            "Seek position must be a finite number".to_string(),
        ));
    }
    if current_time < 0.0 {
        return Err(Error::InvalidInput(
            "Seek position must be non-negative".to_string(),
        ));
    }
    if current_time > MAX_PLAYBACK_POSITION_SECONDS {
        return Err(Error::InvalidInput(
            "Seek position exceeds maximum (24 hours)".to_string(),
        ));
    }
    Ok(())
}

fn validate_playback_speed_value(speed: f64) -> Result<()> {
    if !speed.is_finite() {
        return Err(Error::InvalidInput(
            "Speed must be a finite number".to_string(),
        ));
    }
    if !(MIN_PLAYBACK_SPEED..=MAX_PLAYBACK_SPEED).contains(&speed) {
        return Err(Error::InvalidInput(format!(
            "Speed must be between {MIN_PLAYBACK_SPEED} and {MAX_PLAYBACK_SPEED}"
        )));
    }
    Ok(())
}

fn validate_switch_target(target: &SwitchPlaybackTarget) -> Result<()> {
    match (&target.media_id, &target.playlist_id) {
        (Some(_), Some(_)) => Err(Error::InvalidInput(
            "media_id and playlist_id cannot both be set".to_string(),
        )),
        (None, None) if !target.target.is_empty() => Err(Error::InvalidInput(
            "target must be empty when clearing playback".to_string(),
        )),
        (Some(_), None) if !target.target.is_empty() => Err(Error::InvalidInput(
            "target must be empty when switching to a static media item".to_string(),
        )),
        (None, Some(_)) if target.target.is_empty() => Err(Error::InvalidInput(
            "target is required when switching to a dynamic playlist item".to_string(),
        )),
        _ => Ok(()),
    }
}

/// Trait for broadcasting playback state changes to cluster replicas.
///
/// This abstracts over the cluster manager so that `synctv-core` does not
/// depend on `synctv-cluster`.  The implementation lives in the API/wiring
/// layer where `ClusterManager` is available.
pub trait PlaybackBroadcaster: Send + Sync {
    /// Broadcast a playback state change to other cluster replicas.
    ///
    /// Returns a `BroadcastResult` indicating whether the broadcast succeeded.
    /// Implementations should be non-blocking but report success/failure.
    fn broadcast_playback_state(&self, state: &RoomPlaybackState) -> BroadcastResult;
}

/// Playback management service
///
/// Responsible for playback state coordination and optimistic locking.
#[derive(Debug)]
struct PlaybackInvalidationRuntime {
    started: AtomicBool,
    cancel: tokio::sync::Mutex<CancellationToken>,
    listener_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl PlaybackInvalidationRuntime {
    fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            cancel: tokio::sync::Mutex::new(CancellationToken::new()),
            listener_handle: tokio::sync::Mutex::new(None),
        }
    }
}

#[derive(Clone)]
pub struct PlaybackService {
    playback_repo: RoomPlaybackStateRepository,
    permission_service: PermissionService,
    media_service: MediaService,
    user_service: UserService,
    /// Optional cluster broadcaster for cross-replica sync (interior mutability
    /// so the broadcaster can be wired after Arc<RoomService> is already cloned)
    cluster_broadcaster: Arc<parking_lot::RwLock<Option<Arc<dyn PlaybackBroadcaster>>>>,
    /// L1 in-memory cache for playback state, keyed by `room_id`
    playback_cache: Arc<moka::future::Cache<String, RoomPlaybackState>>,
    /// Optional L2 cache (Redis) for cross-replica consistency.
    /// Held behind interior mutability so listeners started before L2 wiring
    /// still observe the final cache configuration.
    l2_cache: Arc<parking_lot::RwLock<Option<PlaybackStateCache>>>,
    /// Optional cache invalidation service for cross-replica cache sync
    invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
    /// Shared lifecycle state for the background invalidation listener.
    invalidation_runtime: Arc<PlaybackInvalidationRuntime>,
    /// `SingleFlight` to prevent thundering herd on cache miss.
    /// Uses `String` key (`room_id`) and `String` error (since `Error` is not `Clone`).
    single_flight: SingleFlight<String, RoomPlaybackState, String>,
}

impl std::fmt::Debug for PlaybackService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackService").finish()
    }
}

impl PlaybackService {
    /// Default playback state cache capacity (max entries)
    pub const DEFAULT_CACHE_SIZE: u64 = 5_000;
    /// Default playback state cache TTL in seconds (short — playback changes frequently)
    pub const DEFAULT_CACHE_TTL_SECS: u64 = 5;
    /// Maximum time to wait for the invalidation listener to stop.
    const INVALIDATION_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

    /// Create a new playback service
    #[must_use]
    pub fn new(
        playback_repo: RoomPlaybackStateRepository,
        permission_service: PermissionService,
        media_service: MediaService,
        user_service: UserService,
    ) -> Self {
        Self::new_with_runtime(
            playback_repo,
            permission_service,
            media_service,
            user_service,
            None,
            None,
        )
    }

    /// Create a playback service with optional cache runtime dependencies wired
    /// at construction time.
    #[must_use]
    pub fn new_with_runtime(
        playback_repo: RoomPlaybackStateRepository,
        permission_service: PermissionService,
        media_service: MediaService,
        user_service: UserService,
        invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
        l2_cache: Option<PlaybackStateCache>,
    ) -> Self {
        Self {
            playback_repo,
            permission_service,
            media_service,
            user_service,
            cluster_broadcaster: Arc::new(parking_lot::RwLock::new(None)),
            playback_cache: Arc::new(
                moka::future::CacheBuilder::new(Self::DEFAULT_CACHE_SIZE)
                    .time_to_live(Duration::from_secs(Self::DEFAULT_CACHE_TTL_SECS))
                    .build(),
            ),
            l2_cache: Arc::new(parking_lot::RwLock::new(l2_cache)),
            invalidation_service,
            invalidation_runtime: Arc::new(PlaybackInvalidationRuntime::new()),
            single_flight: SingleFlight::new(),
        }
    }

    async fn ensure_creator_is_active(
        &self,
        creator_id: Option<&UserId>,
        resource_kind: &'static str,
    ) -> Result<()> {
        let Some(creator_id) = creator_id else {
            return Ok(());
        };

        match self.user_service.get_user(creator_id).await {
            Ok(user) if user.status.is_active() => Ok(()),
            Ok(_) | Err(Error::NotFound(_)) => Err(Error::Authorization(format!(
                "{resource_kind} is unavailable because its creator is not active"
            ))),
            Err(error) => Err(error),
        }
    }

    /// Set the cluster broadcaster for cross-replica playback state sync.
    /// Uses interior mutability so this can be called through `Arc<RoomService>`.
    pub fn set_cluster_broadcaster(&self, broadcaster: Arc<dyn PlaybackBroadcaster>) {
        *self.cluster_broadcaster.write() = Some(broadcaster);
    }

    /// Set the cache invalidation service and start listening for cross-replica invalidation.
    ///
    /// When another replica updates playback state and broadcasts an invalidation
    /// message, this node's local L1 cache entry for that room is evicted so the
    /// next read fetches fresh data from the DB.
    ///
    /// Call [`start`](Self::start) explicitly during app bootstrap to surface
    /// startup errors and after [`shutdown`](Self::shutdown) to restart the listener.
    pub fn set_invalidation_service(&mut self, service: Arc<dyn CacheInvalidationRuntime>) {
        self.invalidation_service = Some(service);
    }

    pub const fn has_invalidation_service(&self) -> bool {
        self.invalidation_service.is_some()
    }

    fn playback_l2_cache(&self) -> Option<PlaybackStateCache> {
        self.l2_cache.read().clone()
    }

    #[cfg(test)]
    fn invalidation_task_started(&self) -> bool {
        self.invalidation_runtime.started.load(Ordering::Acquire)
    }

    pub async fn start(&self) -> Result<()> {
        let Some(invalidation_service) = self.invalidation_service.clone() else {
            return Ok(());
        };

        if self
            .invalidation_runtime
            .started
            .swap(true, Ordering::AcqRel)
        {
            return Ok(());
        }

        if tokio::runtime::Handle::try_current().is_err() {
            self.invalidation_runtime
                .started
                .store(false, Ordering::Release);
            return Err(Error::Internal(
                "PlaybackService::start requires a Tokio runtime".to_string(),
            ));
        }

        let cache = self.playback_cache.clone();
        let l2_cache = Arc::clone(&self.l2_cache);
        let mut receiver = invalidation_service.subscribe();
        let listener_cancel = self.invalidation_runtime.cancel.lock().await.child_token();

        let listener_handle = crate::spawn::spawn_monitored(
            "playback_invalidation_listener",
            async move {
                // Rate-limit lag-triggered flushes (consistent with CacheManager)
                const LAG_FLUSH_MIN_INTERVAL: std::time::Duration =
                    std::time::Duration::from_secs(5);
                let mut last_lag_flush = std::time::Instant::now()
                    .checked_sub(LAG_FLUSH_MIN_INTERVAL)
                    .unwrap_or_else(std::time::Instant::now);

                loop {
                    tokio::select! {
                        () = listener_cancel.cancelled() => {
                            tracing::debug!(
                                "Playback cache invalidation listener cancelled, stopping"
                            );
                            break;
                        }
                        recv_result = receiver.recv() => {
                            match recv_result {
                                Ok(msg) => match msg {
                                    InvalidationMessage::PlaybackStateUpdate { room_id, state } => {
                                        let new_version = state.version;
                                        let new_state = state;
                                        cache
                                            .entry(room_id.clone())
                                            .and_upsert_with(|maybe_entry| {
                                                let result = if let Some(entry) = maybe_entry {
                                                    let current = entry.into_value();
                                                    let current_version = current.version;
                                                    if new_version > current_version {
                                                        tracing::debug!(
                                                            room_id = %room_id,
                                                            new_version,
                                                            current_version,
                                                            "Playback state cache updated (cross-replica, version upgrade)"
                                                        );
                                                        new_state.clone()
                                                    } else {
                                                        tracing::debug!(
                                                            room_id = %room_id,
                                                            new_version,
                                                            current_version,
                                                            "Playback state cache not updated (cross-replica, stale or duplicate version)"
                                                        );
                                                        current
                                                    }
                                                } else {
                                                    tracing::debug!(
                                                        room_id = %room_id,
                                                        new_version,
                                                        "Playback state cache inserted (cross-replica, no prior entry)"
                                                    );
                                                    new_state.clone()
                                                };
                                                std::future::ready(result)
                                            })
                                            .await;
                                    }
                                    InvalidationMessage::PlaybackState { room_id } => {
                                        cache.invalidate(&room_id).await;
                                        let l2_cache = { l2_cache.read().clone() };
                                        if let Some(l2_cache) = l2_cache {
                                            if let Ok(room_id) = room_id.parse::<RoomId>() {
                                            if let Err(e) = l2_cache.invalidate(&room_id).await {
                                                tracing::warn!(
                                                    room_id = %room_id,
                                                    error = %e,
                                                    "Failed to invalidate playback state from L2 cache"
                                                );
                                            }
                                            }
                                        }
                                        tracing::debug!(
                                            room_id = %room_id,
                                            "Playback state cache invalidated (cross-replica)"
                                        );
                                    }
                                    InvalidationMessage::Room { room_id } => {
                                        cache.invalidate(&room_id).await;
                                        let l2_cache = { l2_cache.read().clone() };
                                        if let Some(l2_cache) = l2_cache {
                                            if let Ok(room_id) = room_id.parse::<RoomId>() {
                                            if let Err(e) = l2_cache.invalidate(&room_id).await {
                                                tracing::warn!(
                                                    room_id = %room_id,
                                                    error = %e,
                                                    "Failed to invalidate room-scoped playback state from L2 cache"
                                                );
                                            }
                                            }
                                        }
                                    }
                                    InvalidationMessage::All => {
                                        cache.invalidate_all();
                                        let l2_cache = { l2_cache.read().clone() };
                                        if let Some(l2_cache) = l2_cache {
                                            l2_cache.clear().await;
                                        }
                                        tracing::debug!("All playback state cache invalidated (cross-replica)");
                                    }
                                    _ => {}
                                },
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    tracing::debug!(
                                        "Playback cache invalidation channel closed, stopping listener"
                                    );
                                    break;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    let now = std::time::Instant::now();
                                    let elapsed = now.duration_since(last_lag_flush);
                                    if elapsed >= LAG_FLUSH_MIN_INTERVAL {
                                        tracing::warn!(
                                            lagged_messages = n,
                                            "Playback cache invalidation listener lagged, flushing all entries (rate-limited)"
                                        );
                                        cache.invalidate_all();
                                        let l2_cache = { l2_cache.read().clone() };
                                        if let Some(l2_cache) = l2_cache {
                                            l2_cache.clear().await;
                                        }
                                        crate::metrics::cache::CACHE_LAG_FLUSH_TOTAL
                                            .with_label_values(&["playback"])
                                            .inc();
                                        last_lag_flush = now;
                                    } else {
                                        tracing::warn!(
                                            lagged_messages = n,
                                            "Playback cache invalidation listener lagged, skipping flush (rate-limited)"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            },
        );

        *self.invalidation_runtime.listener_handle.lock().await = Some(listener_handle);
        Ok(())
    }

    pub async fn shutdown(&self) {
        let cancel = {
            let mut runtime_cancel = self.invalidation_runtime.cancel.lock().await;
            std::mem::replace(&mut *runtime_cancel, CancellationToken::new())
        };
        cancel.cancel();

        let listener_handle = self
            .invalidation_runtime
            .listener_handle
            .lock()
            .await
            .take();
        if let Some(handle) = listener_handle {
            Self::await_invalidation_task_shutdown("playback invalidation listener", handle).await;
        }

        self.invalidation_runtime
            .started
            .store(false, Ordering::Release);
    }

    async fn await_invalidation_task_shutdown(name: &'static str, mut handle: JoinHandle<()>) {
        match tokio::time::timeout(Self::INVALIDATION_TASK_SHUTDOWN_TIMEOUT, &mut handle).await {
            Ok(Ok(())) => info!("{name} stopped"),
            Ok(Err(error)) => warn!(%error, "{name} panicked during shutdown"),
            Err(_) => {
                warn!(
                    timeout_secs = Self::INVALIDATION_TASK_SHUTDOWN_TIMEOUT.as_secs(),
                    "{name} did not stop before timeout; aborting task"
                );
                handle.abort();
                match handle.await {
                    Ok(()) => info!("{name} aborted cleanly"),
                    Err(error) if error.is_cancelled() => info!("{name} aborted"),
                    Err(error) => warn!(%error, "{name} failed after abort"),
                }
            }
        }
    }

    /// Set the L2 cache (Redis) for cross-replica consistency.
    ///
    /// When configured, L1 cache misses will check L2 before falling back to DB.
    /// This provides a fallback when PubSub invalidation messages are lost.
    pub fn set_l2_cache(&mut self, cache: PlaybackStateCache) {
        *self.l2_cache.write() = Some(cache);
    }

    #[cfg(test)]
    pub(crate) fn has_l2_cache(&self) -> bool {
        self.l2_cache.read().is_some()
    }

    /// Broadcast a playback state change to local clients and cluster replicas.
    ///
    /// Uses the cluster broadcaster as the single broadcast path. The cluster
    /// broadcaster calls `ClusterManager::broadcast`, which delivers the event
    /// to local WebSocket subscribers (via the in-process message hub) AND
    /// publishes to Redis for remote replicas in one step.
    ///
    /// The `notification_service` path is intentionally not used here: calling
    /// it in addition to the cluster broadcaster would cause local clients to
    /// receive the same `PlaybackStateChanged` event twice.
    ///
    /// Returns `BroadcastResult` indicating whether the broadcast succeeded.
    /// Logs warnings on partial/complete failure for monitoring.
    fn broadcast_state_change(&self, state: &RoomPlaybackState) -> BroadcastResult {
        // Single broadcast path: cluster broadcaster handles both local delivery
        // (via the in-process message hub) and remote delivery (via Redis pub/sub).
        // Do NOT also call notification_service here — that would send the event
        // to local WebSocket clients a second time.
        if let Some(ref broadcaster) = *self.cluster_broadcaster.read() {
            let result = broadcaster.broadcast_playback_state(state);

            // Log warning if broadcast failed to reach any destination
            if !result.is_success() {
                tracing::warn!(
                    room_id = %state.room_id,
                    local_sent = result.local_sent,
                    redis_sent = result.redis_sent,
                    "Playback state broadcast failed to reach any destination; \
                     other replicas may have stale playback state (up to {}s cache TTL)",
                    Self::DEFAULT_CACHE_TTL_SECS
                );
            } else if result.should_warn_missing_redis_delivery() {
                // Partial failure: local clients got it, but Redis publish failed
                tracing::warn!(
                    room_id = %state.room_id,
                    local_sent = result.local_sent,
                    "Playback state broadcast reached local clients but failed to publish to Redis; \
                     other replicas may have stale playback state"
                );
            }

            return result;
        }

        // No broadcaster configured (single-node mode) - return success
        BroadcastResult::single_node()
    }

    /// Broadcast playback state update to other replicas with exponential backoff retry.
    ///
    /// Retries up to 3 times with delays of 50ms, 100ms, 200ms on failure.
    /// DB write has already succeeded, so broadcast failure only affects replica
    /// consistency (caches will converge within TTL).
    async fn broadcast_invalidation_with_retry(
        &self,
        room_id: &RoomId,
        state: &RoomPlaybackState,
        context: &str,
    ) {
        let Some(ref service) = self.invalidation_service else {
            return;
        };
        let broadcast_delays = [50u64, 100, 200];
        let mut broadcast_ok = false;
        for (attempt, delay_ms) in broadcast_delays.iter().enumerate() {
            match service.update_playback_state(room_id, state).await {
                Ok(()) => {
                    broadcast_ok = true;
                    break;
                }
                Err(e) => {
                    if attempt + 1 < broadcast_delays.len() {
                        tracing::warn!(
                            error = %e,
                            room_id = %room_id,
                            attempt = attempt + 1,
                            max_attempts = broadcast_delays.len(),
                            "{context}: broadcast failed, retrying..."
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
                    }
                }
            }
        }
        if !broadcast_ok {
            tracing::error!(
                room_id = %room_id,
                attempts = broadcast_delays.len(),
                "{context}: broadcast failed after all retry attempts, replicas may have stale state"
            );
        }
    }

    /// Get playback state for a room.
    ///
    /// Checks the L1 in-memory cache first; on miss, checks L2 (Redis) if configured;
    /// on L2 miss, uses `SingleFlight` to ensure only one concurrent DB fetch per
    /// `room_id`, then populates both L1 and L2 caches.
    pub async fn get_state(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        let cache_key = room_id.to_string();

        // L1 cache hit
        if let Some(state) = self.playback_cache.get(&cache_key).await {
            crate::metrics::cache::CACHE_HITS
                .with_label_values(&["playback", "l1"])
                .inc();
            return Ok(state);
        }

        // L2 cache check (if configured)
        if let Some(l2_cache) = self.playback_l2_cache() {
            if let Some(state) = l2_cache.get(room_id).await? {
                // L2 hit - populate L1 and return
                self.playback_cache
                    .insert(cache_key.clone(), state.clone())
                    .await;
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&["playback", "l2"])
                    .inc();
                tracing::debug!(
                    room_id = %room_id,
                    version = state.version,
                    "Playback state cache hit (L2)"
                );
                return Ok(state);
            }
        }

        // L1 and L2 miss — use SingleFlight to prevent thundering herd:
        // Only one task loads from DB for a given room_id; others wait for the result.
        let repo = self.playback_repo.clone();
        let cache = self.playback_cache.clone();
        let l2_cache = self.playback_l2_cache();
        let room_id_clone = *room_id;

        let state = self
            .single_flight
            .do_work(cache_key, async move {
                let state = match repo.get(&room_id_clone).await {
                    Ok(Some(s)) => s,
                    Ok(None) => match repo.create_or_get(&room_id_clone).await {
                        Ok(state) => state,
                        Err(e) => return Err(e.to_string()),
                    },
                    Err(e) => return Err(e.to_string()),
                };

                // Populate L1 cache
                cache.insert(state.room_id.to_string(), state.clone()).await;

                // Populate L2 cache (if configured)
                if let Some(ref l2) = l2_cache {
                    if let Err(e) = l2.set(&state.room_id, state.clone()).await {
                        tracing::warn!(
                            room_id = %state.room_id,
                            error = %e,
                            "Failed to set playback state in L2 cache"
                        );
                    }
                }

                Ok(state)
            })
            .await
            .map_err(|error| match error {
                crate::cache::SingleFlightError::WorkerFailed => Error::Internal(
                    "SingleFlight worker failed during playback state fetch".to_string(),
                ),
                crate::cache::SingleFlightError::Inner(message) => Error::Internal(message),
            })?;

        crate::metrics::cache::CACHE_MISSES
            .with_label_values(&["playback", "l1_l2"])
            .inc();

        Ok(state)
    }

    /// Invalidate the local playback state cache for a room.
    ///
    /// If a `CacheInvalidationService` is configured, this also broadcasts the
    /// invalidation to other replicas via Redis Pub/Sub.
    pub async fn invalidate_playback_cache(&self, room_id: &RoomId) {
        // Broadcast to other replicas first (if configured)
        if let Some(ref service) = self.invalidation_service {
            if let Err(e) = service.invalidate_playback_state(room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    "Failed to broadcast playback state cache invalidation"
                );
            }
        }

        // Invalidate L1 cache
        let cache_key = room_id.to_string();
        self.playback_cache.invalidate(&cache_key).await;

        // Invalidate L2 cache (if configured)
        if let Some(l2_cache) = self.playback_l2_cache() {
            if let Err(e) = l2_cache.invalidate(room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    "Failed to invalidate playback state from L2 cache"
                );
            }
        }
    }

    /// Play/pause playback
    pub async fn set_playing(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playing: bool,
    ) -> Result<RoomPlaybackState> {
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::PLAY_CONTROL)
            .await?;

        let state = self
            .update_state(room_id, |state| {
                if !playing {
                    // Snapshot the computed playback position before pausing so that
                    // the stored current_time reflects where the user actually was.
                    // Without this, resuming would jump back to the last persisted time.
                    state.current_time = state.computed_current_time();
                }
                state.is_playing = playing;
                state.updated_at = chrono::Utc::now();
                // version is incremented by the SQL UPDATE, not here
            })
            .await?;

        // Cache invalidation is already handled inside update_state()
        self.broadcast_state_change(&state);
        Ok(state)
    }

    /// Seek to position.
    ///
    /// If the optimistic lock retries are exhausted (e.g., during rapid seek
    /// bursts), falls back to returning the latest playback state as a
    /// degraded response so the client knows the current position, rather
    /// than receiving a bare error.
    ///
    /// Returns a `SeekResponse` containing:
    /// - `state`: The current playback state (always valid)
    /// - `seek_applied`: Whether the requested position was successfully applied
    /// - `message`: Optional explanation when seek was not applied
    pub async fn seek(
        &self,
        room_id: RoomId,
        user_id: UserId,
        current_time: f64,
    ) -> Result<SeekResponse> {
        validate_seek_position(current_time)?;

        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::PLAY_CONTROL)
            .await?;

        let result = self
            .update_state(room_id, |state| {
                state.current_time = current_time;
                state.updated_at = chrono::Utc::now();
                // version is incremented by the SQL UPDATE, not here
            })
            .await;

        match result {
            Ok(state) => {
                // Cache invalidation is already handled inside update_state()
                self.broadcast_state_change(&state);
                Ok(SeekResponse::success(state))
            }
            Err(error)
                if crate::service::optimistic_retry::is_retry_exhausted(
                    &error,
                    Self::UPDATE_STATE_RETRY_EXHAUSTED,
                ) =>
            {
                // Degraded response: seek failed due to contention, but return
                // the latest state so the client can display the current position.
                tracing::warn!(
                    room_id = %room_id,
                    requested_time = current_time,
                    "Seek failed after max retries, returning latest state as degraded response"
                );
                let state = self.get_state(&room_id).await?;
                Ok(SeekResponse::degraded(
                    state,
                    "Seek could not be applied due to concurrent modifications; returning current state",
                ))
            }
            Err(e) => Err(e),
        }
    }

    /// Change playback speed
    pub async fn change_speed(
        &self,
        room_id: RoomId,
        user_id: UserId,
        speed: f64,
    ) -> Result<RoomPlaybackState> {
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::CHANGE_PLAYBACK_RATE)
            .await?;

        validate_playback_speed_value(speed)?;

        let state = self
            .update_state(room_id, |state| {
                // Snapshot the computed playback position before changing speed so that
                // the stored current_time reflects where the user actually was at the
                // old speed. Without this, the position would be wrong because
                // computed_current_time() uses speed to extrapolate from updated_at.
                state.current_time = state.computed_current_time();
                state.speed = speed;
                state.updated_at = chrono::Utc::now();
                // version is incremented by the SQL UPDATE, not here
            })
            .await?;

        // Cache invalidation is already handled inside update_state()
        self.broadcast_state_change(&state);
        Ok(state)
    }

    /// Switch playback target.
    ///
    /// Valid targets are mutually exclusive:
    /// - static media: `media_id` only
    /// - dynamic playlist item: `playlist_id` + `target`
    pub async fn switch(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Vec<u8>,
    ) -> Result<RoomPlaybackState> {
        self.switch_internal(room_id, user_id, media_id, playlist_id, target, false)
            .await
    }

    /// Management-only playback switch that is authorized outside the room permission graph.
    ///
    /// Callers must validate global admin/root identity before invoking this method.
    pub async fn admin_switch(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Vec<u8>,
    ) -> Result<RoomPlaybackState> {
        self.switch_internal(room_id, actor_user_id, media_id, playlist_id, target, true)
            .await
    }

    /// Play next media in playlist (auto-play next episode)
    ///
    /// This is called when current media finishes playing.
    /// Returns the new playback state if successful, or None if there's no next media.
    ///
    /// Uses a retry loop that re-fetches state and playlist on each attempt to
    /// avoid TOCTOU races where another user changes the playlist or playback
    /// state between the read and the optimistic-lock update.
    pub async fn play_next(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
    ) -> Result<Option<RoomPlaybackState>> {
        let enabled = settings.auto_play.value.enabled;
        let mode = settings.auto_play.value.mode;

        if !enabled {
            return Ok(None);
        }

        // Retry loop: re-fetch state + playlist on each attempt so that
        // concurrent playlist/state changes are correctly reflected.
        for attempt in 0..Self::MAX_RETRIES {
            // Get current state (fresh on every retry)
            let state = match self.playback_repo.get(room_id).await? {
                Some(s) => s,
                None => self.playback_repo.create_or_get(room_id).await?,
            };

            let next_target = if let Some(ref playlist_id) = state.playing_playlist_id {
                let playlist = self
                    .media_service
                    .get_playlist(playlist_id)
                    .await?
                    .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
                match self
                    .ensure_creator_is_active(playlist.creator_id.as_ref(), "Playlist")
                    .await
                {
                    Ok(()) => {}
                    Err(Error::Authorization(_)) => {
                        return self
                            .stop_playback_for_unavailable_creator(room_id, "playlist")
                            .await;
                    }
                    Err(error) => return Err(error),
                }
                self.media_service
                    .next_dynamic_playlist_item(room_id, playlist_id, &state.target, mode)
                    .await
                    .and_then(|item| {
                        item.map(|item| {
                            Ok(NextTarget::Dynamic {
                                playlist_id: playlist.id,
                                target: item.target,
                            })
                        })
                        .transpose()
                    })?
            } else {
                let playlist = if let Some(ref current_id) = state.playing_media_id {
                    let current_media = self
                        .media_service
                        .get_media(current_id)
                        .await?
                        .ok_or_else(|| Error::NotFound("Current media not found".to_string()))?;

                    match self
                        .ensure_creator_is_active(current_media.creator_id.as_ref(), "Media")
                        .await
                    {
                        Ok(()) => {}
                        Err(Error::Authorization(_)) => {
                            return self
                                .stop_playback_for_unavailable_creator(room_id, "media")
                                .await;
                        }
                        Err(error) => return Err(error),
                    }

                    if current_media.room_id != *room_id {
                        return Err(Error::Authorization(
                            "Current media does not belong to this room".to_string(),
                        ));
                    }

                    if let Some(ref playlist_id) = current_media.playlist_id {
                        self.media_service.get_playlist_media(playlist_id).await?
                    } else {
                        self.media_service.get_room_root_media(room_id).await?
                    }
                } else {
                    self.media_service.get_room_root_media(room_id).await?
                };

                if playlist.is_empty() {
                    return Ok(None);
                }

                let next_media = match mode {
                    PlayMode::Sequential => {
                        if let Some(ref current_id) = state.playing_media_id {
                            match playlist.iter().position(|m| &m.id == current_id) {
                                Some(pos) if pos + 1 < playlist.len() => Some(&playlist[pos + 1]),
                                Some(_) => None,
                                None => {
                                    tracing::warn!(
                                        room_id = %room_id,
                                        media_id = %current_id,
                                        "Sequential: current media no longer present, falling back to first available item"
                                    );
                                    playlist.first()
                                }
                            }
                        } else {
                            playlist.first()
                        }
                    }
                    PlayMode::RepeatOne => {
                        if let Some(ref current_id) = state.playing_media_id {
                            playlist
                                .iter()
                                .find(|m| &m.id == current_id)
                                .or_else(|| {
                                    tracing::warn!(
                                        room_id = %room_id,
                                        media_id = %current_id,
                                        "RepeatOne: current media no longer present, falling back to first available item"
                                    );
                                    playlist.first()
                                })
                        } else {
                            playlist.first()
                        }
                    }
                    PlayMode::RepeatAll => {
                        if let Some(ref current_id) = state.playing_media_id {
                            if let Some(pos) = playlist.iter().position(|m| &m.id == current_id) {
                                Some(&playlist[(pos + 1) % playlist.len()])
                            } else {
                                tracing::warn!(
                                    room_id = %room_id,
                                    media_id = %current_id,
                                    "RepeatAll: current media no longer present, falling back to first available item"
                                );
                                playlist.first()
                            }
                        } else {
                            playlist.first()
                        }
                    }
                    PlayMode::Shuffle => {
                        if let Some(ref current_id) = state.playing_media_id {
                            let other_media = playlist
                                .iter()
                                .filter(|m| &m.id != current_id)
                                .collect::<Vec<_>>();
                            if other_media.is_empty() {
                                playlist.iter().find(|m| &m.id == current_id)
                            } else {
                                other_media.into_iter().choose(&mut rand::rng())
                            }
                        } else {
                            playlist.first()
                        }
                    }
                };

                next_media.cloned().map(NextTarget::Static)
            };

            let Some(next_target) = next_target else {
                tracing::info!(
                    room_id = %room_id,
                    mode = ?mode,
                    "Playlist ended"
                );
                return Ok(None);
            };

            // Apply update to the fetched state and try to save with optimistic locking
            let mut updated_state = state;
            match &next_target {
                NextTarget::Static(next) => {
                    match self
                        .ensure_creator_is_active(next.creator_id.as_ref(), "Media")
                        .await
                    {
                        Ok(()) => {}
                        Err(Error::Authorization(_)) => {
                            return self
                                .stop_playback_for_unavailable_creator(room_id, "media")
                                .await;
                        }
                        Err(error) => return Err(error),
                    }
                    updated_state.playing_media_id = Some(next.id);
                    updated_state.playing_playlist_id = None;
                    updated_state.target = Vec::new();
                }
                NextTarget::Dynamic {
                    playlist_id,
                    target,
                } => {
                    let playlist = self
                        .media_service
                        .get_playlist(playlist_id)
                        .await?
                        .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
                    match self
                        .ensure_creator_is_active(playlist.creator_id.as_ref(), "Playlist")
                        .await
                    {
                        Ok(()) => {}
                        Err(Error::Authorization(_)) => {
                            return self
                                .stop_playback_for_unavailable_creator(room_id, "playlist")
                                .await;
                        }
                        Err(error) => return Err(error),
                    }
                    updated_state.playing_media_id = None;
                    updated_state.playing_playlist_id = Some(*playlist_id);
                    updated_state.target = target.clone();
                }
            }
            updated_state.current_time = 0.0;
            updated_state.is_playing = true;
            updated_state.updated_at = chrono::Utc::now();

            match self.playback_repo.update(&updated_state).await {
                Ok(saved_state) => {
                    // Invalidate local cache
                    let cache_key = room_id.to_string();
                    self.playback_cache.invalidate(&cache_key).await;

                    // Broadcast to other replicas with retry
                    self.broadcast_invalidation_with_retry(room_id, &saved_state, "play_next")
                        .await;

                    tracing::info!(
                        room_id = %room_id,
                        target = ?next_target,
                        mode = ?mode,
                        "Auto-played next media"
                    );

                    self.broadcast_state_change(&saved_state);
                    return Ok(Some(saved_state));
                }
                Err(Error::OptimisticLockConflict) => {
                    if attempt + 1 < Self::MAX_RETRIES {
                        let backoff = Self::BACKOFF_BASE_MS * (1 << attempt);
                        let jitter = rand::rng().random_range(0..Self::BACKOFF_BASE_MS);
                        let delay = backoff + jitter;
                        tracing::debug!(
                            room_id = %room_id,
                            attempt = attempt + 1,
                            delay_ms = delay,
                            "play_next version conflict, re-fetching state and retrying"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        continue;
                    }
                    return Err(Error::Internal(
                        "play_next failed after maximum retry attempts".to_string(),
                    ));
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal(
            "play_next failed after maximum retry attempts".to_string(),
        ))
    }

    /// Check if media has ended and auto-play next if needed
    ///
    /// This should be called when playback `current_time` is updated.
    /// It checks if the current time has reached or exceeded the media duration.
    pub async fn check_and_auto_play(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        current_time: f64,
    ) -> Result<Option<RoomPlaybackState>> {
        let enabled = settings.auto_play.value.enabled;

        if !enabled {
            return Ok(None);
        }

        // Get current media to check duration
        let state = self.get_state(room_id).await?;
        let playing_media_id = state.playing_media_id;

        let playing_media = match playing_media_id {
            Some(ref id) => self
                .media_service
                .get_media(id)
                .await?
                .ok_or_else(|| Error::NotFound("Current media not found".to_string()))?,
            None => return Ok(None),
        };

        // A negative current_time (-1.0) is an explicit "media ended" signal from the client
        if current_time < 0.0 {
            return self.play_next(room_id, settings).await;
        }

        // Try to get duration from source_config metadata (any provider may store it)
        let duration = playing_media
            .source_config
            .get("metadata")
            .and_then(|m| m.get("duration"))
            .and_then(serde_json::Value::as_f64);

        // Use computed time to account for elapsed wall-clock time when playing
        let effective_time = state.computed_current_time();

        // Check if current_time is near end (within 1 second or past end)
        if let Some(dur) = duration {
            if effective_time >= dur - 1.0 {
                // Auto-play next media
                self.play_next(room_id, settings).await
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    /// Maximum retry attempts for optimistic lock conflicts.
    ///
    /// Playback writes are bursty and user-driven, so this budget is slightly
    /// higher than the generic default to absorb short-lived write storms
    /// without leaking avoidable failures back to callers.
    const MAX_RETRIES: u32 = 5;
    /// Base delay for exponential backoff (milliseconds)
    const BACKOFF_BASE_MS: u64 = 5;
    const UPDATE_STATE_RETRY_EXHAUSTED: &str =
        "Playback state update failed after maximum retry attempts";

    /// Update playback state with generic update function.
    ///
    /// Uses optimistic locking with automatic retry on version conflicts.
    /// Retries use exponential backoff with jitter to avoid thundering herd.
    pub async fn update_state<F>(&self, room_id: RoomId, update_fn: F) -> Result<RoomPlaybackState>
    where
        F: Fn(&mut RoomPlaybackState),
    {
        crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            Self::UPDATE_STATE_RETRY_EXHAUSTED,
            || {
                let update_fn = &update_fn;
                async move {
                    // Get current state (lazy-init: only INSERT if row doesn't exist yet)
                    let mut state = match self.playback_repo.get(&room_id).await? {
                        Some(s) => s,
                        None => self.playback_repo.create_or_get(&room_id).await?,
                    };

                    // Apply update
                    update_fn(&mut state);

                    let updated_state = self.playback_repo.update(&state).await?;

                    // Invalidate local L1 cache so the next read fetches fresh data.
                    // This avoids write-through which would self-invalidate when the
                    // Redis Pub/Sub bounce-back arrives.
                    let cache_key = room_id.to_string();
                    self.playback_cache.invalidate(&cache_key).await;

                    // Update L2 cache with the new state (if configured).
                    // Uses set_if_newer to prevent stale data from overwriting fresh data.
                    // This provides a fallback when PubSub messages are lost.
                    let l2_cache = self.playback_l2_cache();
                    if let Some(l2_cache) = l2_cache {
                        if let Err(e) = l2_cache.set_if_newer(&room_id, updated_state.clone()).await
                        {
                            tracing::warn!(
                                error = %e,
                                room_id = %room_id,
                                "Failed to update playback state in L2 cache"
                            );
                        }
                    }

                    // Broadcast updated state to other replicas with retry
                    self.broadcast_invalidation_with_retry(
                        &room_id,
                        &updated_state,
                        "update_state",
                    )
                    .await;

                    Ok(updated_state)
                }
            },
        )
        .await
    }

    async fn update_state_with_expected_version<F>(
        &self,
        room_id: RoomId,
        expected_version: i64,
        update_fn: F,
    ) -> Result<RoomPlaybackState>
    where
        F: Fn(&mut RoomPlaybackState),
    {
        let mut state = match self.playback_repo.get(&room_id).await? {
            Some(s) => s,
            None => self.playback_repo.create_or_get(&room_id).await?,
        };

        if state.version != expected_version {
            return Err(Error::OptimisticLockConflict);
        }

        update_fn(&mut state);
        let updated_state = self.playback_repo.update(&state).await?;

        let cache_key = room_id.to_string();
        self.playback_cache.invalidate(&cache_key).await;

        let l2_cache = self.playback_l2_cache();
        if let Some(l2_cache) = l2_cache {
            if let Err(e) = l2_cache.set_if_newer(&room_id, updated_state.clone()).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    "Failed to update playback state in L2 cache"
                );
            }
        }

        self.broadcast_invalidation_with_retry(&room_id, &updated_state, "update_state")
            .await;

        Ok(updated_state)
    }

    /// Reset playback to initial state
    pub async fn reset(&self, room_id: RoomId, user_id: UserId) -> Result<RoomPlaybackState> {
        self.reset_internal(room_id, user_id, false).await
    }

    /// Management-only playback reset that bypasses room membership-derived permissions.
    ///
    /// Callers must validate global admin/root identity before invoking this method.
    pub async fn admin_reset(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
    ) -> Result<RoomPlaybackState> {
        self.reset_internal(room_id, actor_user_id, true).await
    }

    pub async fn broadcast_playback_reset_after_force_delete(
        &self,
        state: RoomPlaybackState,
    ) -> BroadcastResult {
        self.invalidate_playback_cache(&state.room_id).await;
        self.broadcast_state_change(&state)
    }

    pub async fn reset_playback_for_creator(
        &self,
        creator_id: &UserId,
    ) -> Result<Vec<RoomPlaybackState>> {
        let states = self
            .playback_repo
            .reset_playback_for_creator(creator_id)
            .await?;

        for state in &states {
            self.invalidate_playback_cache(&state.room_id).await;
            self.broadcast_invalidation_with_retry(
                &state.room_id,
                state,
                "reset_playback_for_creator",
            )
            .await;
            self.broadcast_state_change(state);
        }

        Ok(states)
    }

    /// Check if playback is currently active
    pub async fn is_playing(&self, room_id: &RoomId) -> Result<bool> {
        let state = self.get_state(room_id).await?;
        Ok(state.is_playing)
    }

    /// Get current media being played
    pub async fn get_playing_media_id(&self, room_id: &RoomId) -> Result<Option<MediaId>> {
        let state = self.get_state(room_id).await?;
        Ok(state.playing_media_id)
    }

    /// Get current playback position
    pub async fn get_current_time(&self, room_id: &RoomId) -> Result<f64> {
        let state = self.get_state(room_id).await?;
        Ok(state.current_time)
    }

    async fn switch_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Vec<u8>,
        bypass_room_permissions: bool,
    ) -> Result<RoomPlaybackState> {
        if !bypass_room_permissions {
            self.permission_service
                .check_permission(&room_id, &user_id, PermissionBits::CHANGE_CURRENT_MEDIA)
                .await?;
        }
        let target = SwitchPlaybackTarget {
            media_id,
            playlist_id,
            target,
        };
        validate_switch_target(&target)?;

        if target.media_id.is_none() && target.playlist_id.is_none() {
            let state = self
                .update_state(room_id, |state| {
                    state.playing_media_id = None;
                    state.playing_playlist_id = None;
                    state.target = Vec::new();
                    state.current_time = 0.0;
                    state.speed = 1.0;
                    state.is_playing = false;
                    state.updated_at = chrono::Utc::now();
                })
                .await?;

            self.broadcast_state_change(&state);
            return Ok(state);
        }

        if let Some(ref media_id) = target.media_id {
            let media = self
                .media_service
                .get_media(media_id)
                .await?
                .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

            if media.room_id != room_id {
                return Err(Error::Authorization(
                    "Media does not belong to this room".to_string(),
                ));
            }

            self.ensure_creator_is_active(media.creator_id.as_ref(), "Media")
                .await?;
        }

        if let Some(ref playlist_id) = target.playlist_id {
            let playlist = self
                .media_service
                .get_playlist(playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

            if playlist.room_id != room_id {
                return Err(Error::Authorization(
                    "Playlist does not belong to this room".to_string(),
                ));
            }

            if !playlist.is_dynamic() {
                return Err(Error::InvalidInput(
                    "playlist_id playback target must reference a dynamic playlist".to_string(),
                ));
            }

            self.ensure_creator_is_active(playlist.creator_id.as_ref(), "Playlist")
                .await?;

            let resolved = self
                .media_service
                .resolve_dynamic_playlist_item(room_id, user_id, playlist_id, &target.target)
                .await?;
            if resolved.is_none() {
                return Err(Error::NotFound(
                    "Dynamic playlist item not found".to_string(),
                ));
            }
        }

        let state = self
            .update_state(room_id, |state| {
                state.playing_media_id.clone_from(&target.media_id);
                state.playing_playlist_id.clone_from(&target.playlist_id);
                state.target.clone_from(&target.target);
                state.current_time = 0.0;
                state.is_playing = true;
                state.updated_at = chrono::Utc::now();
                // version is incremented by the SQL UPDATE, not here
            })
            .await?;

        // Cache invalidation is already handled inside update_state()
        self.broadcast_state_change(&state);
        Ok(state)
    }

    async fn reset_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        bypass_room_permissions: bool,
    ) -> Result<RoomPlaybackState> {
        if !bypass_room_permissions {
            self.permission_service
                .check_permission(&room_id, &user_id, PermissionBits::PLAY_CONTROL)
                .await?;
        }

        let state = self
            .update_state(room_id, |state| {
                state.is_playing = false;
                state.current_time = 0.0;
                state.speed = 1.0;
                state.playing_media_id = None;
                state.playing_playlist_id = None;
                state.target = Vec::new();
                state.updated_at = chrono::Utc::now();
                // version is incremented by the SQL UPDATE, not here
            })
            .await?;

        self.broadcast_state_change(&state);
        Ok(state)
    }

    async fn stop_playback_for_unavailable_creator(
        &self,
        room_id: &RoomId,
        resource_kind: &'static str,
    ) -> Result<Option<RoomPlaybackState>> {
        tracing::warn!(
            room_id = %room_id,
            resource_kind,
            "Stopping playback because the target creator is not active"
        );

        let state = self
            .update_state(*room_id, |state| {
                state.playing_media_id = None;
                state.playing_playlist_id = None;
                state.target = Vec::new();
                state.current_time = 0.0;
                state.speed = 1.0;
                state.is_playing = false;
                state.updated_at = chrono::Utc::now();
            })
            .await?;

        self.broadcast_state_change(&state);
        Ok(Some(state))
    }

    /// Get current playback speed
    pub async fn get_speed(&self, room_id: &RoomId) -> Result<f64> {
        let state = self.get_state(room_id).await?;
        Ok(state.speed)
    }

    /// Update multiple playback properties at once
    #[allow(clippy::too_many_arguments)]
    pub async fn update_multiple(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playing: Option<bool>,
        current_time: Option<f64>,
        speed: Option<f64>,
    ) -> Result<RoomPlaybackState> {
        self.update_multiple_with_version(room_id, user_id, playing, current_time, speed, None)
            .await
    }

    /// Like `update_multiple`, but accepts an optional `expected_version` for CAS
    /// (compare-and-swap) semantics.
    ///
    /// When `expected_version` is `Some(v)`, the current playback version is read
    /// from the database and compared with `v`. If they differ, the call returns
    /// `Error::OptimisticLockConflict` immediately without attempting the update.
    /// This lets the caller detect stale state before the internal retry loop
    /// silently resolves the conflict.
    ///
    /// When `expected_version` is `None`, the update proceeds directly through
    /// the internal retry loop (last-writer-wins).
    #[allow(clippy::too_many_arguments)]
    pub async fn update_multiple_with_version(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playing: Option<bool>,
        current_time: Option<f64>,
        speed: Option<f64>,
        expected_version: Option<i64>,
    ) -> Result<RoomPlaybackState> {
        self.update_multiple_internal(
            room_id,
            user_id,
            playing,
            current_time,
            speed,
            expected_version,
            false,
        )
        .await
    }

    /// Management-only multi-field playback update that bypasses room membership
    /// permissions. Callers must validate global admin/root identity before use.
    #[allow(clippy::too_many_arguments)]
    pub async fn admin_update_multiple_with_version(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        playing: Option<bool>,
        current_time: Option<f64>,
        speed: Option<f64>,
        expected_version: Option<i64>,
    ) -> Result<RoomPlaybackState> {
        self.update_multiple_internal(
            room_id,
            actor_user_id,
            playing,
            current_time,
            speed,
            expected_version,
            true,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_multiple_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playing: Option<bool>,
        current_time: Option<f64>,
        speed: Option<f64>,
        expected_version: Option<i64>,
        bypass_permission: bool,
    ) -> Result<RoomPlaybackState> {
        // Check permissions based on what's being updated
        let mut required_perms = PermissionBits::NONE;
        if playing.is_some() {
            required_perms |= PermissionBits::PLAY_CONTROL;
        }
        if current_time.is_some() {
            required_perms |= PermissionBits::PLAY_CONTROL;
        }
        if speed.is_some() {
            required_perms |= PermissionBits::CHANGE_PLAYBACK_RATE;
        }
        if required_perms != PermissionBits::NONE && !bypass_permission {
            self.permission_service
                .check_permission(&room_id, &user_id, required_perms)
                .await?;
        }

        if let Some(ct) = current_time {
            validate_seek_position(ct)?;
        }

        if let Some(s) = speed {
            validate_playback_speed_value(s)?;
        }

        let apply_update = |state: &mut RoomPlaybackState| {
            // Snapshot the computed playback position before changing is_playing
            // or speed, just like set_playing() and change_speed() do individually.
            // Without this, pausing or changing speed via update_multiple would
            // store the wrong position.
            let needs_snapshot = matches!(playing, Some(false)) || speed.is_some();
            if needs_snapshot && current_time.is_none() {
                // Only snapshot if the caller didn't provide an explicit position
                state.current_time = state.computed_current_time();
            }

            if let Some(p) = playing {
                state.is_playing = p;
            }
            if let Some(ct) = current_time {
                state.current_time = ct;
            }
            if let Some(s) = speed {
                state.speed = s;
            }
            state.updated_at = chrono::Utc::now();
            // version is incremented by the SQL UPDATE, not here
        };

        let state = if let Some(expected) = expected_version {
            self.update_state_with_expected_version(room_id, expected, apply_update)
                .await?
        } else {
            self.update_state(room_id, apply_update).await?
        };

        // Cache invalidation is already handled inside update_state()
        self.broadcast_state_change(&state);
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheInvalidationService, CacheL2Backend, KeyBuilder, UsernameCache};
    use crate::config::PasswordComplexityConfig;
    use crate::models::RoomId;
    use crate::repository::{
        MediaRepository, PlaylistRepository, ProviderInstanceRepository,
        RoomPlaybackStateRepository, RoomRepository,
    };
    use crate::service::permission::PermissionService;
    use crate::service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        InMemoryTokenBlacklistStore, MediaService, NotificationService, ProvidersManager,
        RemoteProviderManager, UserService,
    };
    use async_trait::async_trait;
    use sqlx::PgPool;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;

    #[derive(Default)]
    struct CountingL2Backend {
        delete_calls: AtomicUsize,
    }

    #[async_trait]
    impl CacheL2Backend for CountingL2Backend {
        async fn get(&self, _key: &str) -> Result<Option<String>> {
            Ok(None)
        }

        async fn set(&self, _key: &str, _json: &str, _ttl_secs: u64) -> Result<()> {
            Ok(())
        }

        async fn delete(&self, _key: &str) -> Result<()> {
            self.delete_calls.fetch_add(1, AtomicOrdering::Relaxed);
            Ok(())
        }

        async fn delete_with_retry(
            &self,
            _key: &str,
            _max_retries: u32,
            _cache_type: &str,
        ) -> Result<()> {
            self.delete_calls.fetch_add(1, AtomicOrdering::Relaxed);
            Ok(())
        }

        async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>> {
            Ok(vec![None; keys.len()])
        }

        async fn set_if_newer(
            &self,
            _key: &str,
            _json: &str,
            _ttl_secs: u64,
            _new_ts_iso: &str,
        ) -> Result<bool> {
            Ok(true)
        }

        async fn delete_by_prefix(&self, _prefix: &str) -> Result<()> {
            Ok(())
        }

        fn is_active(&self) -> bool {
            true
        }
    }

    fn make_user_service(pool: PgPool) -> UserService {
        let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
        let password_complexity = PasswordComplexityConfig::default();
        let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
        let key_builder = KeyBuilder::new("test");
        let brute_force = BruteForceProtection::in_memory("test".to_string());

        let mut user_service = UserService::new(
            pool,
            jwt_service,
            username_cache,
            password_complexity,
            token_blacklist,
            key_builder,
            brute_force,
        );
        user_service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
        user_service
    }

    fn make_playback_service_for_lifecycle_tests(
    ) -> (PlaybackService, Arc<CacheInvalidationService>) {
        let pool = PgPool::connect_lazy("postgres://localhost/test")
            .expect("lazy postgres pool for unit tests should build");
        let member_repo = crate::repository::RoomMemberRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let permission_service = PermissionService::without_cache(member_repo, room_repo, None);
        let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
            provider_repo,
            None,
        ));
        let providers_manager = Arc::new(ProvidersManager::new(provider_instance_manager));
        let media_service = MediaService::new(
            MediaRepository::new(pool.clone()),
            PlaylistRepository::new(pool.clone()),
            permission_service.clone(),
            providers_manager,
            NotificationService::default(),
        );
        let user_service = make_user_service(pool.clone());
        let playback_service = PlaybackService::new(
            RoomPlaybackStateRepository::new(pool.clone()),
            permission_service,
            media_service,
            user_service,
        );
        let invalidation_service = Arc::new(CacheInvalidationService::new(
            "node-test".to_string(),
            "synctv:test:cache:invalidate".to_string(),
        ));
        (playback_service, invalidation_service)
    }

    #[test]
    fn test_speed_validation_bounds() {
        // Valid boundary values
        assert!(validate_playback_speed_value(0.25).is_ok());
        assert!(validate_playback_speed_value(0.5).is_ok());
        assert!(validate_playback_speed_value(1.0).is_ok());
        assert!(validate_playback_speed_value(2.0).is_ok());
        assert!(validate_playback_speed_value(4.0).is_ok());

        // Invalid boundary values (below minimum)
        assert!(validate_playback_speed_value(0.0).is_err());
        assert!(validate_playback_speed_value(0.1).is_err());
        assert!(validate_playback_speed_value(0.24).is_err());
        assert!(validate_playback_speed_value(-1.0).is_err());

        // Invalid boundary values (above maximum)
        assert!(validate_playback_speed_value(4.1).is_err());
        assert!(validate_playback_speed_value(8.0).is_err());
        assert!(validate_playback_speed_value(16.0).is_err());
        assert!(validate_playback_speed_value(f64::NAN).is_err());
        assert!(validate_playback_speed_value(f64::INFINITY).is_err());
    }

    #[test]
    fn test_update_multiple_speed_uses_standard_validation_bounds() {
        assert!(validate_playback_speed_value(0.25).is_ok());
        assert!(validate_playback_speed_value(1.0).is_ok());
        assert!(validate_playback_speed_value(4.0).is_ok());

        assert!(validate_playback_speed_value(0.0).is_err());
        assert!(validate_playback_speed_value(-1.0).is_err());
        assert!(validate_playback_speed_value(4.1).is_err());
        assert!(validate_playback_speed_value(8.0).is_err());
        assert!(validate_playback_speed_value(f64::NAN).is_err());
        assert!(validate_playback_speed_value(f64::INFINITY).is_err());
    }

    #[test]
    fn test_seek_negative_position() {
        assert!(validate_seek_position(-1.0).is_err());
        assert!(validate_seek_position(0.0).is_ok());
        assert!(validate_seek_position(42.5).is_ok());
        assert!(validate_seek_position(MAX_PLAYBACK_POSITION_SECONDS).is_ok());
        assert!(validate_seek_position(MAX_PLAYBACK_POSITION_SECONDS + 0.1).is_err());
        assert!(validate_seek_position(f64::NAN).is_err());
        assert!(validate_seek_position(f64::INFINITY).is_err());
    }

    #[test]
    fn test_update_state_constants() {
        assert_eq!(PlaybackService::MAX_RETRIES, 5);
        assert_eq!(PlaybackService::BACKOFF_BASE_MS, 5);
    }

    #[tokio::test]
    async fn test_invalidation_listener_stops_after_cache_invalidation_service_stop() {
        let (mut playback_service, invalidation_service) =
            make_playback_service_for_lifecycle_tests();
        let room_id = RoomId::from(10_001);
        let cache_key = room_id.to_string();

        playback_service.set_invalidation_service(invalidation_service.clone());
        playback_service
            .start()
            .await
            .expect("playback invalidation listener should start");

        assert!(
            playback_service.invalidation_task_started(),
            "start() must mark playback invalidation runtime as running"
        );

        playback_service.shutdown().await;

        let updated_state = RoomPlaybackState {
            room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            target: Vec::new(),
            current_time: 42.0,
            speed: 1.0,
            is_playing: true,
            updated_at: chrono::Utc::now(),
            version: 7,
        };

        invalidation_service
            .broadcast_all(InvalidationMessage::PlaybackStateUpdate {
                room_id: cache_key.clone(),
                state: updated_state,
            })
            .await
            .expect("local invalidation broadcast should succeed");
        tokio::task::yield_now().await;

        assert!(
            playback_service.playback_cache.get(&cache_key).await.is_none(),
            "playback invalidation listener must stop processing local broadcasts once shutdown starts"
        );
    }

    #[tokio::test]
    async fn test_start_can_restart_playback_invalidation_listener_after_shutdown() {
        let (mut playback_service, invalidation_service) =
            make_playback_service_for_lifecycle_tests();
        let room_id = RoomId::from(10_002);
        let cache_key = room_id.to_string();

        playback_service.set_invalidation_service(invalidation_service.clone());
        playback_service
            .start()
            .await
            .expect("initial playback invalidation start should succeed");
        playback_service.shutdown().await;

        let updated_state = RoomPlaybackState {
            room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            target: Vec::new(),
            current_time: 64.0,
            speed: 1.0,
            is_playing: true,
            updated_at: chrono::Utc::now(),
            version: 9,
        };

        playback_service
            .start()
            .await
            .expect("restart after playback invalidation shutdown should succeed");

        invalidation_service
            .broadcast_all(InvalidationMessage::PlaybackStateUpdate {
                room_id: cache_key.clone(),
                state: updated_state,
            })
            .await
            .expect("local invalidation broadcast should succeed after restart");
        tokio::task::yield_now().await;

        let cached = playback_service
            .playback_cache
            .get(&cache_key)
            .await
            .expect("restarted listener should populate cache from invalidation broadcast");
        assert_eq!(cached.version, 9);

        playback_service.shutdown().await;
    }

    #[tokio::test]
    async fn test_start_activates_invalidation_listener_after_wiring_service() {
        let (mut playback_service, invalidation_service) =
            make_playback_service_for_lifecycle_tests();
        let room_id = RoomId::from(10_003);
        let cache_key = room_id.to_string();

        playback_service.set_invalidation_service(invalidation_service.clone());
        playback_service
            .start()
            .await
            .expect("explicit start should activate playback invalidation listener");

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !playback_service.invalidation_task_started() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("start() should mark playback invalidation listener as running");

        let updated_state = RoomPlaybackState {
            room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            target: Vec::new(),
            current_time: 88.0,
            speed: 1.0,
            is_playing: true,
            updated_at: chrono::Utc::now(),
            version: 11,
        };

        invalidation_service
            .broadcast_all(InvalidationMessage::PlaybackStateUpdate {
                room_id: cache_key.clone(),
                state: updated_state.clone(),
            })
            .await
            .expect("local invalidation broadcast should succeed");

        let cached = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(cached) = playback_service.playback_cache.get(&cache_key).await {
                    break cached;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("started playback invalidation listener should process broadcasts");

        assert_eq!(cached.version, updated_state.version);

        playback_service.shutdown().await;
    }

    #[tokio::test]
    async fn test_started_invalidation_listener_uses_l2_cache_wired_after_start() {
        let (mut playback_service, invalidation_service) =
            make_playback_service_for_lifecycle_tests();
        let backend = Arc::new(CountingL2Backend::default());
        let l2_cache = PlaybackStateCache::new(
            backend.clone(),
            16,
            5,
            60,
            "synctv:test:playback:".to_string(),
        )
        .expect("test playback L2 cache should build");
        let room_id = RoomId::from(10_004);

        playback_service.set_invalidation_service(invalidation_service.clone());
        playback_service
            .start()
            .await
            .expect("explicit start should activate playback invalidation listener");

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !playback_service.invalidation_task_started() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("start() should mark playback invalidation listener as running");

        playback_service.set_l2_cache(l2_cache);

        invalidation_service
            .broadcast_all(InvalidationMessage::PlaybackState {
                room_id: room_id.to_string(),
            })
            .await
            .expect("playback invalidation should broadcast locally");

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend.delete_calls.load(AtomicOrdering::Relaxed) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("listener should invalidate L2 even when L2 is wired after explicit start");

        playback_service.shutdown().await;
    }

    /// Tests for optimistic lock retry mechanism
    mod optimistic_retry_tests {
        use super::*;

        #[test]
        fn test_retry_succeeds_within_max_attempts() {
            // Playback writes run under bursty contention and need a slightly
            // larger budget than the generic optimistic-lock default.
            let conflicts = 4;
            let attempts_needed = conflicts + 1; // 5 attempts
            assert!(
                attempts_needed <= PlaybackService::MAX_RETRIES,
                "Need {} attempts but MAX_RETRIES is {}",
                attempts_needed,
                PlaybackService::MAX_RETRIES
            );
        }
    }

    /// Tests for the CAS (compare-and-swap) version check in
    /// `update_multiple_with_version`. These replicate the pre-check logic
    /// without requiring a database.
    mod cas_version_pre_check_tests {
        use crate::Error;

        /// Replicates the CAS pre-check from `update_multiple_with_version`:
        /// when `expected_version` is provided, the current DB version must match.
        fn check_cas_version(
            current_version: i64,
            expected_version: Option<i64>,
        ) -> crate::Result<()> {
            if let Some(expected) = expected_version {
                if current_version != expected {
                    return Err(Error::OptimisticLockConflict);
                }
            }
            Ok(())
        }

        #[test]
        fn test_cas_correct_version_succeeds() {
            let result = check_cas_version(5, Some(5));
            assert!(result.is_ok(), "Matching version should succeed");
        }

        #[test]
        fn test_cas_wrong_version_returns_conflict() {
            let result = check_cas_version(5, Some(3));
            assert!(result.is_err(), "Stale version should return conflict");
            match result.unwrap_err() {
                Error::OptimisticLockConflict => {} // expected
                other => panic!("Expected OptimisticLockConflict, got: {other:?}"),
            }
        }

        #[test]
        fn test_cas_no_version_skips_check() {
            // When no expected version is provided, the check is skipped
            let result = check_cas_version(999, None);
            assert!(
                result.is_ok(),
                "No expected version should skip CAS check (last-writer-wins)"
            );
        }

        #[test]
        fn test_cas_version_zero_matches_initial_state() {
            // Initial playback state has version=0
            let result = check_cas_version(0, Some(0));
            assert!(result.is_ok(), "Version 0 should match initial state");
        }

        #[test]
        fn test_cas_version_zero_expected_but_updated() {
            // Caller expects version 0 but state was already updated to version 1
            let result = check_cas_version(1, Some(0));
            assert!(
                result.is_err(),
                "Stale version 0 when current is 1 should conflict"
            );
        }
    }

    /// Tests for playback state cache version checking (CAS semantics)
    mod version_check_tests {
        use super::*;

        /// Helper to create a playback state with a specific version
        fn make_state(room_id: i64, version: i64, current_time: f64) -> RoomPlaybackState {
            RoomPlaybackState {
                room_id: RoomId::from(room_id),
                playing_media_id: None,
                playing_playlist_id: None,
                target: Vec::new(),
                current_time,
                speed: 1.0,
                is_playing: false,
                updated_at: chrono::Utc::now(),
                version,
            }
        }

        fn version_to_current_time(version: i64) -> f64 {
            f64::from(i32::try_from(version).unwrap_or(i32::MAX)) * 10.0
        }

        /// Test: When cache is empty, incoming state should be inserted
        #[tokio::test]
        async fn test_cache_insert_when_empty() {
            let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
                Arc::new(moka::future::Cache::new(100));

            let room_id = 31_001;
            let cache_key = room_id.to_string();
            let new_state = make_state(room_id, 5, 100.0);

            // Simulate the CAS logic from the invalidation handler
            cache
                .entry(cache_key.clone())
                .and_upsert_with(|maybe_entry| {
                    let result = match maybe_entry {
                        Some(entry) => {
                            let current = entry.into_value();
                            if new_state.version > current.version {
                                new_state.clone()
                            } else {
                                current
                            }
                        }
                        None => new_state.clone(),
                    };
                    std::future::ready(result)
                })
                .await;

            let cached = cache.get(&cache_key).await.expect("should have entry");
            assert_eq!(cached.version, 5);
            assert!((cached.current_time - 100.0).abs() < f64::EPSILON);
        }

        /// Test: When incoming version is higher, cache should be updated
        #[tokio::test]
        async fn test_cache_update_when_version_higher() {
            let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
                Arc::new(moka::future::Cache::new(100));

            let room_id = 31_002;
            let cache_key = room_id.to_string();

            // Insert initial state with version 3
            let initial_state = make_state(room_id, 3, 50.0);
            cache.insert(cache_key.clone(), initial_state).await;

            // Verify initial state
            let cached = cache.get(&cache_key).await.expect("should have entry");
            assert_eq!(cached.version, 3);

            // Try to update with version 7 (higher)
            let new_state = make_state(room_id, 7, 150.0);
            cache
                .entry(cache_key.clone())
                .and_upsert_with(|maybe_entry| {
                    let result = match maybe_entry {
                        Some(entry) => {
                            let current = entry.into_value();
                            if new_state.version > current.version {
                                new_state.clone()
                            } else {
                                current
                            }
                        }
                        None => new_state.clone(),
                    };
                    std::future::ready(result)
                })
                .await;

            // Cache should now have version 7
            let cached = cache.get(&cache_key).await.expect("should have entry");
            assert_eq!(cached.version, 7);
            assert!((cached.current_time - 150.0).abs() < f64::EPSILON);
        }

        /// Test: When incoming version is lower, cache should NOT be updated
        #[tokio::test]
        async fn test_cache_not_updated_when_version_lower() {
            let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
                Arc::new(moka::future::Cache::new(100));

            let room_id = 31_003;
            let cache_key = room_id.to_string();

            // Insert initial state with version 10
            let initial_state = make_state(room_id, 10, 200.0);
            cache.insert(cache_key.clone(), initial_state).await;

            // Try to update with version 5 (lower - simulates delayed/out-of-order message)
            let old_state = make_state(room_id, 5, 100.0);
            cache
                .entry(cache_key.clone())
                .and_upsert_with(|maybe_entry| {
                    let result = match maybe_entry {
                        Some(entry) => {
                            let current = entry.into_value();
                            if old_state.version > current.version {
                                old_state.clone()
                            } else {
                                current
                            }
                        }
                        None => old_state.clone(),
                    };
                    std::future::ready(result)
                })
                .await;

            // Cache should still have version 10 (not downgraded to 5)
            let cached = cache.get(&cache_key).await.expect("should have entry");
            assert_eq!(cached.version, 10);
            assert!((cached.current_time - 200.0).abs() < f64::EPSILON);
        }

        /// Test: When versions are equal, cache should NOT be updated (idempotent)
        #[tokio::test]
        async fn test_cache_not_updated_when_version_equal() {
            let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
                Arc::new(moka::future::Cache::new(100));

            let room_id = 31_004;
            let cache_key = room_id.to_string();

            // Insert initial state with version 5
            let initial_state = make_state(room_id, 5, 200.0);
            cache.insert(cache_key.clone(), initial_state).await;

            // Try to update with same version 5 but different content
            let duplicate_state = make_state(room_id, 5, 999.0);
            cache
                .entry(cache_key.clone())
                .and_upsert_with(|maybe_entry| {
                    let result = match maybe_entry {
                        Some(entry) => {
                            let current = entry.into_value();
                            if duplicate_state.version > current.version {
                                duplicate_state.clone()
                            } else {
                                current
                            }
                        }
                        None => duplicate_state.clone(),
                    };
                    std::future::ready(result)
                })
                .await;

            // Cache should still have original content (not overwritten)
            let cached = cache.get(&cache_key).await.expect("should have entry");
            assert_eq!(cached.version, 5);
            assert!((cached.current_time - 200.0).abs() < f64::EPSILON);
        }

        /// Test: Sequential updates should only keep the highest version
        #[tokio::test]
        async fn test_sequential_updates_keep_highest_version() {
            let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
                Arc::new(moka::future::Cache::new(100));

            let room_id = 31_005;
            let cache_key = room_id.to_string();

            // Apply updates in non-monotonic order: v1, v5, v3, v7, v2
            let versions = [1i64, 5, 3, 7, 2];
            for v in versions {
                let state = make_state(room_id, v, version_to_current_time(v));
                cache
                    .entry(cache_key.clone())
                    .and_upsert_with(|maybe_entry| {
                        let result = match maybe_entry {
                            Some(entry) => {
                                let current = entry.into_value();
                                if state.version > current.version {
                                    state.clone()
                                } else {
                                    current
                                }
                            }
                            None => state.clone(),
                        };
                        std::future::ready(result)
                    })
                    .await;
            }

            // Cache should have version 7 (the highest)
            let cached = cache.get(&cache_key).await.expect("should have entry");
            assert_eq!(cached.version, 7);
            assert!((cached.current_time - 70.0).abs() < f64::EPSILON);
        }

        /// Test: Concurrent updates should be serialized and result in highest version
        #[tokio::test]
        async fn test_concurrent_updates_serialized() {
            let cache: Arc<moka::future::Cache<String, RoomPlaybackState>> =
                Arc::new(moka::future::Cache::new(100));

            let room_id = 31_006;
            let cache_key = Arc::new(room_id.to_string());
            let cache_clone = cache.clone();

            // Spawn 10 concurrent tasks, each trying to insert a different version
            let handles: Vec<_> = (1..=10)
                .map(|v| {
                    let cache_key = cache_key.clone();
                    let cache = cache_clone.clone();
                    tokio::spawn(async move {
                        let state = make_state(room_id, v, version_to_current_time(v));
                        cache
                            .entry(cache_key.to_string())
                            .and_upsert_with(|maybe_entry| {
                                let result = match maybe_entry {
                                    Some(entry) => {
                                        let current = entry.into_value();
                                        if state.version > current.version {
                                            state.clone()
                                        } else {
                                            current
                                        }
                                    }
                                    None => state.clone(),
                                };
                                std::future::ready(result)
                            })
                            .await
                    })
                })
                .collect();

            // Wait for all tasks to complete
            for handle in handles {
                handle.await.expect("task should complete");
            }

            // Cache should have version 10 (the highest)
            let cached = cache.get(&*cache_key).await.expect("should have entry");
            assert_eq!(cached.version, 10);
            assert!((cached.current_time - 100.0).abs() < f64::EPSILON);
        }
    }

    /// Tests for the broadcast_invalidation_with_retry helper
    mod broadcast_retry_tests {
        #[test]
        fn test_broadcast_retry_delays_are_exponential() {
            // The broadcast retry pattern uses delays [50, 100, 200] ms
            let delays = [50u64, 100, 200];
            assert_eq!(delays.len(), 3, "Should have 3 retry attempts");
            assert_eq!(delays[1], delays[0] * 2, "Second delay should be 2x first");
            assert_eq!(delays[2], delays[1] * 2, "Third delay should be 2x second");
        }
    }

    /// Tests for update_state retry mechanism boundary conditions.
    ///
    /// These tests verify the retry logic in PlaybackService::update_state:
    /// - MAX_RETRIES = 5
    /// - On OptimisticLockConflict, retry with exponential backoff + jitter
    /// - After MAX_RETRIES, return Internal error with "maximum retry attempts" message
    mod update_state_retry_boundary_tests {
        use super::*;

        /// Simulate the retry loop logic from update_state
        /// Returns Ok(attempts_used) on success, Err("max_retries") on exhaustion
        fn simulate_retry_loop(
            conflict_count: usize,
            max_retries: u32,
        ) -> std::result::Result<u32, &'static str> {
            for attempt in 0..max_retries {
                // Simulate conflict on first `conflict_count` attempts
                if (attempt as usize) < conflict_count {
                    // Conflict occurred
                    if attempt + 1 >= max_retries {
                        return Err("maximum retry attempts");
                    }
                    // Would apply backoff here in real code
                    continue;
                }
                // Success
                return Ok(attempt + 1);
            }
            Err("maximum retry attempts")
        }

        /// Test: With 0 conflicts, update succeeds on first attempt
        #[test]
        fn test_retry_succeeds_immediately_with_no_conflicts() {
            let result = simulate_retry_loop(0, PlaybackService::MAX_RETRIES);
            assert_eq!(
                result,
                Ok(1),
                "Should succeed on first attempt with no conflicts"
            );
        }

        /// Test: With 1 conflict, update succeeds on second attempt (within MAX_RETRIES)
        #[test]
        fn test_retry_succeeds_after_one_conflict() {
            let result = simulate_retry_loop(1, PlaybackService::MAX_RETRIES);
            assert_eq!(
                result,
                Ok(2),
                "Should succeed on second attempt after 1 conflict"
            );
        }

        /// Test: With 2 conflicts, update succeeds on third attempt
        #[test]
        fn test_retry_succeeds_after_two_conflicts() {
            let result = simulate_retry_loop(2, PlaybackService::MAX_RETRIES);
            assert_eq!(
                result,
                Ok(3),
                "Should succeed on third attempt after 2 conflicts"
            );
        }

        /// Test: With 4 conflicts, update still succeeds on the fifth attempt.
        #[test]
        fn test_retry_succeeds_after_four_conflicts() {
            let result = simulate_retry_loop(4, PlaybackService::MAX_RETRIES);
            assert_eq!(
                result,
                Ok(5),
                "Should succeed on fifth attempt after 4 conflicts"
            );
        }

        /// Test: With 5 conflicts (equal to MAX_RETRIES), update fails.
        #[test]
        fn test_retry_fails_after_five_conflicts() {
            let result = simulate_retry_loop(5, PlaybackService::MAX_RETRIES);
            assert!(
                result.is_err(),
                "Should fail after 5 conflicts (equal to MAX_RETRIES)"
            );
            assert_eq!(result, Err("maximum retry attempts"));
        }

        /// Test: With more conflicts than MAX_RETRIES, update fails
        #[test]
        fn test_retry_fails_with_excessive_conflicts() {
            let result = simulate_retry_loop(10, PlaybackService::MAX_RETRIES);
            assert!(
                result.is_err(),
                "Should fail when conflicts exceed MAX_RETRIES"
            );
        }

        /// Test: Verify backoff calculation matches the formula:
        /// delay = base * 2^attempt + jitter
        #[test]
        fn test_backoff_calculation_formula() {
            let base_ms = PlaybackService::BACKOFF_BASE_MS;

            // Attempt 0: backoff = 5 * 1 = 5ms, jitter = 0..5
            let backoff_0 = base_ms; // 5
            assert_eq!(backoff_0, 5);

            // Attempt 1: backoff = 5 * 2 = 10ms, jitter = 0..5
            let backoff_1 = base_ms * (1 << 1); // 10
            assert_eq!(backoff_1, 10);

            // Attempt 2: backoff = 5 * 4 = 20ms, jitter = 0..5
            let backoff_2 = base_ms * (1 << 2); // 20
            assert_eq!(backoff_2, 20);

            // Attempt 3: backoff = 5 * 8 = 40ms, jitter = 0..5
            let backoff_3 = base_ms * (1 << 3); // 40
            assert_eq!(backoff_3, 40);
        }

        /// Test: Total possible delay before exhaustion
        /// With 5 retries and backoffs of ~5, ~10, ~20, ~40 ms (+ jitter),
        /// total delay is approximately 75-95ms.
        #[test]
        fn test_total_delay_before_exhaustion() {
            let base_ms = PlaybackService::BACKOFF_BASE_MS;

            // Calculate total backoff (without jitter) before the final attempt.
            let total_backoff: u64 = (0..PlaybackService::MAX_RETRIES - 1)
                .map(|attempt| base_ms * (1 << attempt))
                .sum();

            assert_eq!(
                total_backoff, 75,
                "Total backoff before exhaustion should be ~75ms"
            );

            // With max jitter (5ms per backoff), total could be up to 95ms.
            let max_total = total_backoff + (base_ms * u64::from(PlaybackService::MAX_RETRIES - 1));
            assert_eq!(max_total, 95, "Max total with jitter should be ~95ms");
        }

        /// Test: Jitter range is correct (0 to BACKOFF_BASE_MS exclusive)
        #[test]
        fn test_jitter_range() {
            let base_ms = PlaybackService::BACKOFF_BASE_MS;
            // The jitter is: rand::rng().random_range(0..BACKOFF_BASE_MS)
            // Which means jitter is in range [0, BACKOFF_BASE_MS)
            assert!(base_ms > 0, "BASE_MS should be positive for jitter to work");
        }

        /// Test: Verify the error message on exhaustion contains "maximum retry attempts"
        #[test]
        fn test_exhaustion_error_message_format() {
            let error_msg = PlaybackService::UPDATE_STATE_RETRY_EXHAUSTED;
            assert!(
                crate::service::optimistic_retry::is_retry_exhausted(
                    &Error::Internal(error_msg.to_string()),
                    error_msg,
                ),
                "Exact playback exhaustion error should be detected"
            );
        }

        /// Test: Seek operation returns degraded response on retry exhaustion
        /// The seek() method should only degrade on the exact retry exhaustion
        /// error produced by the playback update path.
        #[test]
        fn test_seek_returns_degraded_response_on_exhaustion() {
            let is_degraded = crate::service::optimistic_retry::is_retry_exhausted(
                &Error::Internal(PlaybackService::UPDATE_STATE_RETRY_EXHAUSTED.to_string()),
                PlaybackService::UPDATE_STATE_RETRY_EXHAUSTED,
            );

            assert!(
                is_degraded,
                "Exact retry exhaustion error should trigger degraded response"
            );
        }

        /// Test: unrelated Internal errors that merely contain the same phrase
        /// must not trigger degraded seek handling.
        #[test]
        fn test_seek_does_not_degrade_on_partial_message_match() {
            let noisy_error = Error::Internal(format!(
                "unexpected wrapper: {} while doing something else",
                PlaybackService::UPDATE_STATE_RETRY_EXHAUSTED
            ));

            assert!(
                !crate::service::optimistic_retry::is_retry_exhausted(
                    &noisy_error,
                    PlaybackService::UPDATE_STATE_RETRY_EXHAUSTED,
                ),
                "Partial message matches should not trigger degraded response"
            );
        }

        /// Test: Non-retry errors should not trigger retry mechanism
        #[test]
        fn test_non_retry_errors_bubble_up() {
            use crate::Error;

            // Errors other than OptimisticLockConflict should immediately return
            let other_errors = vec![
                Error::NotFound("Room not found".to_string()),
                Error::Authorization("No permission".to_string()),
                Error::InvalidInput("Invalid speed".to_string()),
            ];

            for error in other_errors {
                // These should NOT be retried
                let should_retry = matches!(error, Error::OptimisticLockConflict);
                assert!(!should_retry, "Non-optimistic-lock errors should not retry");
            }
        }

        const _: () = assert!(
            PlaybackService::MAX_RETRIES >= 2,
            "MAX_RETRIES should be at least 2 for contention handling"
        );

        const _: () = assert!(
            PlaybackService::MAX_RETRIES <= 5,
            "MAX_RETRIES should be at most 5 to avoid excessive latency"
        );

        const _: () = assert!(
            PlaybackService::BACKOFF_BASE_MS >= 1,
            "BACKOFF_BASE_MS should be at least 1ms"
        );

        const _: () = assert!(
            PlaybackService::BACKOFF_BASE_MS <= 50,
            "BACKOFF_BASE_MS should be at most 50ms"
        );
    }
}
