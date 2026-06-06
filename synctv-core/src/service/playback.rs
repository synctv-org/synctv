//! Playback state management service
//!
//! Handles playback coordination including play/pause, seeking, speed changes,
//! and media switching with optimistic locking for concurrent updates.

use std::sync::Arc;
use std::time::Duration;

use crate::{
    cache::{
        CacheDomain, CacheInvalidationRuntime, CloneableError, ConsistencyCoordinator,
        FenceReadResult, InvalidationMessage, PlaybackStateCache, SingleFlight,
        VersionFenceReservation, VersionFenceStore,
    },
    models::{MediaId, PlayMode, PlaylistId, RoomId, RoomPlaybackState, RoomSettings, UserId},
    repository::{
        realtime_outbox::{NewRealtimeOutboxEvent, RealtimeOutboxRepository},
        RoomPlaybackStateRepository,
    },
    service::{media::MediaService, permission::PermissionService, UserService},
    Error, Result,
};
use rand::prelude::IteratorRandom;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub type RealtimeOutboxPlaybackStateEventFactory =
    Arc<dyn Fn(&RoomPlaybackState) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct SwitchPlaybackTarget {
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PlaybackSourceExpectation {
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target_hash: String,
}

impl PlaybackSourceExpectation {
    #[must_use]
    pub fn matches(&self, state: &RoomPlaybackState) -> bool {
        self.media_id == state.playing_media_id
            && self.playlist_id == state.playing_playlist_id
            && self.target_hash.eq_ignore_ascii_case(&state.target_hash())
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlaybackStatePatch {
    pub playing: Option<bool>,
    pub position: Option<f64>,
    pub speed: Option<f64>,
}

impl PlaybackStatePatch {
    #[must_use]
    pub const fn new(playing: Option<bool>, position: Option<f64>, speed: Option<f64>) -> Self {
        Self {
            playing,
            position,
            speed,
        }
    }
}

#[derive(Clone)]
pub struct PlaybackUpdateRequest {
    pub room_id: RoomId,
    pub actor_user_id: UserId,
    pub patch: PlaybackStatePatch,
    pub expected_version: Option<i64>,
    pub expected_source: Option<PlaybackSourceExpectation>,
    pub outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
}

impl PlaybackUpdateRequest {
    #[must_use]
    pub const fn new(room_id: RoomId, actor_user_id: UserId, patch: PlaybackStatePatch) -> Self {
        Self {
            room_id,
            actor_user_id,
            patch,
            expected_version: None,
            expected_source: None,
            outbox_event_factory: None,
        }
    }

    #[must_use]
    pub const fn with_expected_version(mut self, expected_version: Option<i64>) -> Self {
        self.expected_version = expected_version;
        self
    }

    #[must_use]
    pub fn with_expected_source(mut self, expected_source: PlaybackSourceExpectation) -> Self {
        self.expected_source = Some(expected_source);
        self
    }

    #[must_use]
    pub fn with_outbox(
        mut self,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Self {
        self.outbox_event_factory = outbox_event_factory;
        self
    }
}

#[derive(Debug)]
enum NextTarget {
    Static(crate::models::Media),
    Dynamic {
        playlist_id: PlaylistId,
        target: Vec<u8>,
    },
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
fn validate_seek_position(position: f64) -> Result<()> {
    if !position.is_finite() {
        return Err(Error::InvalidInput(
            "Seek position must be a finite number".to_string(),
        ));
    }
    if position < 0.0 {
        return Err(Error::InvalidInput(
            "Seek position must be non-negative".to_string(),
        ));
    }
    if position > MAX_PLAYBACK_POSITION_SECONDS {
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

fn validate_position_update_source(state: &RoomPlaybackState) -> Result<()> {
    if state.playing_media_id.is_none() && state.playing_playlist_id.is_none() {
        return Err(Error::InvalidInput(
            "playback position update requires a current playback source".to_string(),
        ));
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

fn playback_source_is_set(state: &RoomPlaybackState) -> bool {
    state.playing_media_id.is_some() || state.playing_playlist_id.is_some()
}

fn playback_source_changed(before: &RoomPlaybackState, after: &RoomPlaybackState) -> bool {
    before.playing_media_id != after.playing_media_id
        || before.playing_playlist_id != after.playing_playlist_id
        || before.target != after.target
}

fn previous_progress_position_for_source_transition(
    before: &RoomPlaybackState,
    after: &RoomPlaybackState,
) -> Option<f64> {
    if playback_source_is_set(before) && playback_source_changed(before, after) {
        Some(before.computed_position())
    } else {
        None
    }
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
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    permission_service: PermissionService,
    media_service: MediaService,
    user_service: UserService,
    /// L1 in-memory cache for playback state, keyed by `room_id`
    playback_cache: Arc<moka::future::Cache<String, RoomPlaybackState>>,
    /// Optional L2 cache (Redis) for cross-replica consistency.
    /// Held behind interior mutability so listeners started before L2 wiring
    /// still observe the final cache configuration.
    l2_cache: Arc<parking_lot::RwLock<Option<PlaybackStateCache>>>,
    /// Optional cache invalidation service for cross-replica cache sync
    invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
    consistency: ConsistencyCoordinator,
    /// Shared lifecycle state for the background invalidation listener.
    invalidation_runtime: Arc<PlaybackInvalidationRuntime>,
    /// `SingleFlight` to prevent thundering herd on cache miss.
    /// Uses `String` key (`room_id`) and `String` error (since `Error` is not `Clone`).
    single_flight: SingleFlight<String, RoomPlaybackState, CloneableError>,
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
            None,
            None,
        )
    }

    /// Create a playback service with optional cache runtime dependencies wired
    /// at construction time.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_runtime(
        playback_repo: RoomPlaybackStateRepository,
        permission_service: PermissionService,
        media_service: MediaService,
        user_service: UserService,
        invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
        l2_cache: Option<PlaybackStateCache>,
        version_fence: Option<Arc<dyn VersionFenceStore>>,
        realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    ) -> Self {
        let version_fence =
            version_fence.unwrap_or_else(|| Arc::new(crate::cache::NoopVersionFenceStore));

        Self {
            playback_repo,
            realtime_outbox,
            permission_service,
            media_service,
            user_service,
            playback_cache: Arc::new(
                moka::future::CacheBuilder::new(Self::DEFAULT_CACHE_SIZE)
                    .time_to_live(Duration::from_secs(Self::DEFAULT_CACHE_TTL_SECS))
                    .build(),
            ),
            l2_cache: Arc::new(parking_lot::RwLock::new(l2_cache)),
            invalidation_service,
            consistency: ConsistencyCoordinator::new(version_fence),
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

    pub const fn has_invalidation_service(&self) -> bool {
        self.invalidation_service.is_some()
    }

    fn playback_l2_cache(&self) -> Option<PlaybackStateCache> {
        self.l2_cache.read().clone()
    }

    fn playback_domain(room_id: &RoomId) -> CacheDomain {
        CacheDomain::Playback { room_id: *room_id }
    }

    async fn advance_playback_version_fence(&self, room_id: &RoomId, version: i64) -> Result<()> {
        if !self.consistency.is_authoritative() {
            return Ok(());
        }

        self.consistency
            .set_version_at_least(&Self::playback_domain(room_id), version)
            .await?;
        Ok(())
    }

    async fn seed_playback_version_fence_after_reload(&self, room_id: &RoomId, version: i64) {
        if let Err(error) = self.advance_playback_version_fence(room_id, version).await {
            tracing::warn!(
                room_id = %room_id,
                version,
                error = %error,
                "Failed to seed playback version fence after DB reload"
            );
        }
    }

    async fn begin_playback_write_from_db_version(
        &self,
        room_id: &RoomId,
        db_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        let domain = Self::playback_domain(room_id);
        self.consistency
            .begin_observed_write(&domain, db_version)
            .await
    }

    async fn commit_playback_write(
        &self,
        room_id: &RoomId,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) -> Result<()> {
        self.consistency
            .commit_reserved_write(&Self::playback_domain(room_id), reservation, version)
            .await?;
        Ok(())
    }

    async fn finalize_committed_playback_write_best_effort(
        &self,
        room_id: &RoomId,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
        operation: &'static str,
    ) {
        if let Err(error) = self
            .commit_playback_write(room_id, reservation, version)
            .await
        {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                version,
                operation,
                "Failed to finalize playback fence after committed DB write"
            );
        }
    }

    async fn abort_playback_write(
        &self,
        room_id: &RoomId,
        reservation: Option<&VersionFenceReservation>,
    ) {
        self.consistency
            .abort_reserved_write(&Self::playback_domain(room_id), reservation)
            .await;
    }

    async fn persist_playback_update_with_previous_progress(
        &self,
        state: &RoomPlaybackState,
        observed_version: i64,
        previous_progress_position: Option<f64>,
        outbox_event_factory: Option<&RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        let reservation = self
            .begin_playback_write_from_db_version(&state.room_id, observed_version)
            .await?;
        let new_version = reservation
            .as_ref()
            .map_or(state.version + 1, |reservation| reservation.version);
        let mut tx = self.playback_repo.pool().begin().await?;
        let result = async {
            let updated_state = self
                .playback_repo
                .update_with_exact_version_executor_and_previous_progress(
                    state,
                    new_version,
                    previous_progress_position,
                    &mut tx,
                )
                .await?;
            if let Some(outbox) = &self.realtime_outbox {
                if let Some(event) = outbox_event_factory
                    .map(|factory| factory(&updated_state))
                    .transpose()?
                {
                    outbox.insert_with_executor(&event, &mut *tx).await?;
                }
            }
            Ok(updated_state)
        }
        .await;

        let updated_state = match result {
            Ok(updated_state) => {
                if let Err(error) = tx.commit().await {
                    self.abort_playback_write(&state.room_id, reservation.as_ref())
                        .await;
                    return Err(error.into());
                }
                updated_state
            }
            Err(error) => {
                if let Err(rollback_error) = tx.rollback().await {
                    tracing::warn!(
                        room_id = %state.room_id,
                        error = %rollback_error,
                        "Failed to roll back playback update transaction"
                    );
                }
                self.abort_playback_write(&state.room_id, reservation.as_ref())
                    .await;
                return Err(error);
            }
        };

        if let Some(reservation) = &reservation {
            self.finalize_committed_playback_write_best_effort(
                &state.room_id,
                Some(reservation),
                updated_state.version,
                "persist_playback_update",
            )
            .await;
        } else {
            self.finalize_committed_playback_write_best_effort(
                &state.room_id,
                None,
                updated_state.version,
                "persist_playback_update",
            )
            .await;
        }
        Ok(updated_state)
    }

    async fn write_playback_cache(&self, state: &RoomPlaybackState) {
        let cache_key = state.room_id.to_string();
        let new_state = state.clone();
        self.playback_cache
            .entry(cache_key)
            .and_upsert_with(|maybe_entry| {
                let result = match maybe_entry {
                    Some(entry) => {
                        let current = entry.into_value();
                        if new_state.version >= current.version {
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

        let l2_cache = self.playback_l2_cache();
        if let Some(l2_cache) = l2_cache {
            if let Err(e) = l2_cache
                .set_if_version_at_least(&state.room_id, state.clone())
                .await
            {
                tracing::warn!(
                    error = %e,
                    room_id = %state.room_id,
                    "Failed to update playback state in L2 cache"
                );
            }
        }
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

    #[cfg(test)]
    pub(crate) fn has_l2_cache(&self) -> bool {
        self.l2_cache.read().is_some()
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

    /// Get playback state for a room with strong cache semantics.
    ///
    /// L1/L2 values are used only when their optimistic-lock version is at
    /// least the authoritative playback version fence. If the fence cannot be
    /// read, this falls back to the database.
    pub async fn get_state(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        self.get_state_by_fence(room_id).await
    }

    async fn get_state_by_fence(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        let domain = Self::playback_domain(room_id);
        if !self.consistency.is_authoritative() {
            ConsistencyCoordinator::record_db_fallback(&domain, "non_authoritative_fence");
            return self.reload_state_from_store(room_id).await;
        }

        if let Some(l2_cache) = self.playback_l2_cache() {
            if let Some(fence_key) = self.consistency.fence_key(&domain) {
                let cache_key = room_id.to_string();
                let l1_value = self.playback_cache.get(&cache_key).await;
                match l2_cache
                    .get_by_fence_key_with_l1_value(room_id, &fence_key, l1_value)
                    .await
                {
                    Ok(FenceReadResult::Hit(state)) => {
                        self.playback_cache.insert(cache_key, state.clone()).await;
                        return Ok(state);
                    }
                    Ok(FenceReadResult::DbFallback) => {
                        ConsistencyCoordinator::record_db_fallback(&domain, "stale_cache");
                        return self.reload_state_from_store(room_id).await;
                    }
                    Ok(FenceReadResult::Unsupported) => {}
                    Err(error) => {
                        tracing::warn!(
                            room_id = %room_id,
                            error = %error,
                            "Playback fence-key cache read failed; falling back to version read"
                        );
                        ConsistencyCoordinator::record_db_fallback(&domain, "fence_key_read_error");
                    }
                }
            }
        }

        let fence_version = match self.consistency.current_committed_version(&domain).await {
            Ok(Some(version)) => version,
            Ok(None) => {
                ConsistencyCoordinator::record_db_fallback(&domain, "missing_fence");
                return self.reload_state_from_store(room_id).await;
            }
            Err(error) => {
                tracing::warn!(
                    room_id = %room_id,
                    error = %error,
                    "Playback version fence unavailable; bypassing cache"
                );
                ConsistencyCoordinator::record_db_fallback(&domain, "fence_unavailable");
                return self.reload_state_from_store(room_id).await;
            }
        };

        let cache_key = room_id.to_string();
        if let Some(state) = self.playback_cache.get(&cache_key).await {
            if state.version >= fence_version {
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&["playback", "l1"])
                    .inc();
                return Ok(state);
            }
        }

        if let Some(l2_cache) = self.playback_l2_cache() {
            match l2_cache.get_l2(room_id).await {
                Ok(Some(state)) if state.version >= fence_version => {
                    self.playback_cache
                        .insert(cache_key.clone(), state.clone())
                        .await;
                    crate::metrics::cache::CACHE_HITS
                        .with_label_values(&["playback", "l2"])
                        .inc();
                    return Ok(state);
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        room_id = %room_id,
                        error = %error,
                        "Playback L2 read failed; bypassing cache"
                    );
                    ConsistencyCoordinator::record_db_fallback(&domain, "l2_error");
                }
            }
        }

        ConsistencyCoordinator::record_db_fallback(&domain, "stale_cache");
        self.reload_state_from_store(room_id).await
    }

    /// Get playback state from cache with eventual consistency.
    ///
    /// This is kept for non-authoritative preloading or diagnostics. User-facing
    /// and permission-adjacent paths should call [`get_state`](Self::get_state).
    pub async fn get_state_eventually_consistent(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomPlaybackState> {
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
                        Err(e) => return Err(CloneableError::from(e)),
                    },
                    Err(e) => return Err(CloneableError::from(e)),
                };

                // Populate L1 cache
                cache.insert(state.room_id.to_string(), state.clone()).await;

                // Populate L2 cache (if configured)
                if let Some(ref l2) = l2_cache {
                    if let Err(e) = l2
                        .set_if_version_at_least(&state.room_id, state.clone())
                        .await
                    {
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
                crate::cache::SingleFlightError::Inner(error) => Error::from(error),
            })?;

        crate::metrics::cache::CACHE_MISSES
            .with_label_values(&["playback", "l1_l2"])
            .inc();

        Ok(state)
    }

    /// Reload playback state from the database after discarding cached copies.
    ///
    /// Use this when a caller detects that a cached playback state references
    /// resources that no longer exist. This avoids returning a stale playing
    /// media/playlist during the short window before cross-replica invalidation
    /// reaches the current node.
    pub async fn reload_state_from_store(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        let cache_key = room_id.to_string();
        self.playback_cache.invalidate(&cache_key).await;
        if let Some(l2_cache) = self.playback_l2_cache() {
            if let Err(e) = l2_cache.invalidate(room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    "Failed to invalidate playback state from L2 cache before DB reload"
                );
            }
        }

        let state = match self.playback_repo.get(room_id).await? {
            Some(state) => state,
            None => self.playback_repo.create_or_get(room_id).await?,
        };

        self.consistency
            .repair_after_db_read(&Self::playback_domain(room_id), state.version)
            .await;
        self.seed_playback_version_fence_after_reload(room_id, state.version)
            .await;

        if let Some(l2_cache) = self.playback_l2_cache() {
            match l2_cache
                .set_if_version_at_least(room_id, state.clone())
                .await
            {
                Ok(true) => {
                    self.playback_cache.insert(cache_key, state.clone()).await;
                }
                Ok(false) => {
                    tracing::debug!(
                        room_id = %room_id,
                        version = state.version,
                        "Skipped playback cache update after DB reload because L2 has newer state"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        room_id = %room_id,
                        "Failed to update playback state in L2 cache after DB reload"
                    );
                }
            }
        } else {
            self.playback_cache.insert(cache_key, state.clone()).await;
        }

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
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::PLAY_CONTROL,
            )
            .await?;

        let state = self
            .update_state(room_id, |state| {
                if !playing {
                    // Snapshot the computed playback position before pausing so that
                    // the stored position reflects where the user actually was.
                    // Without this, resuming would jump back to the last persisted time.
                    state.position = state.computed_position();
                }
                state.is_playing = playing;
                state.updated_at = chrono::Utc::now();
                // version is incremented by the SQL UPDATE, not here
            })
            .await?;

        Ok(state)
    }

    /// Seek to position.
    ///
    /// If the optimistic lock retries are exhausted (e.g., during rapid seek
    /// bursts), falls back to returning the latest playback state as a
    /// degraded response so the client knows the playback position, rather
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
        position: f64,
    ) -> Result<SeekResponse> {
        let result = self
            .update_playback_state(PlaybackUpdateRequest::new(
                room_id,
                user_id,
                PlaybackStatePatch::new(None, Some(position), None),
            ))
            .await;

        match result {
            Ok(state) => Ok(SeekResponse::success(state)),
            Err(error)
                if crate::service::optimistic_retry::is_retry_exhausted(
                    &error,
                    Self::UPDATE_STATE_RETRY_EXHAUSTED,
                ) =>
            {
                // Degraded response: seek failed due to contention, but return
                // the latest state so the client can display the playback position.
                tracing::warn!(
                    room_id = %room_id,
                    requested_time = position,
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
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CHANGE_PLAYBACK_RATE,
            )
            .await?;

        validate_playback_speed_value(speed)?;

        let state = self
            .update_state(room_id, |state| {
                // Snapshot the computed playback position before changing speed so that
                // the stored position reflects where the user actually was at the
                // old speed. Without this, the position would be wrong because
                // computed_position() uses speed to extrapolate from updated_at.
                state.position = state.computed_position();
                state.speed = speed;
                state.updated_at = chrono::Utc::now();
                // version is incremented by the SQL UPDATE, not here
            })
            .await?;

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
        self.switch_with_outbox(room_id, user_id, media_id, playlist_id, target, None)
            .await
    }

    pub async fn switch_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Vec<u8>,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.switch_internal(
            room_id,
            user_id,
            media_id,
            playlist_id,
            target,
            false,
            outbox_event_factory,
        )
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
        self.admin_switch_with_outbox(room_id, actor_user_id, media_id, playlist_id, target, None)
            .await
    }

    pub async fn admin_switch_with_outbox(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Vec<u8>,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.switch_internal(
            room_id,
            actor_user_id,
            media_id,
            playlist_id,
            target,
            true,
            outbox_event_factory,
        )
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

        crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "play_next failed after maximum retry attempts",
            || async {
                // Get current state (fresh on every retry)
                let state = match self.playback_repo.get(room_id).await? {
                    Some(s) => s,
                    None => self.playback_repo.create_or_get(room_id).await?,
                };

                let next_target = if let Some(ref playlist_id) = state.playing_playlist_id {
                    let playlist = self
                        .media_service
                        .get_room_playlist(room_id, playlist_id)
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
                            .get_room_media(room_id, current_id)
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

                        if let Some(ref playlist_id) = current_media.playlist_id {
                            self.media_service
                                .get_room_playlist_media(room_id, playlist_id)
                                .await?
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
                                    Some(pos) if pos + 1 < playlist.len() => {
                                        Some(&playlist[pos + 1])
                                    }
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
                                playlist.iter().find(|m| &m.id == current_id).or_else(|| {
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
                                if let Some(pos) = playlist.iter().position(|m| &m.id == current_id)
                                {
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
                let previous_state = updated_state.clone();
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
                            .get_room_playlist(room_id, playlist_id)
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
                let observed_version = updated_state.version;
                updated_state.position = 0.0;
                updated_state.is_playing = true;
                updated_state.updated_at = chrono::Utc::now();
                let previous_progress_position =
                    previous_progress_position_for_source_transition(&previous_state, &updated_state);

                let saved_state = self
                    .persist_playback_update_with_previous_progress(
                        &updated_state,
                        observed_version,
                        previous_progress_position,
                        None,
                    )
                    .await?;
                self.write_playback_cache(&saved_state).await;

                // Broadcast to other replicas with retry
                self.broadcast_invalidation_with_retry(room_id, &saved_state, "play_next")
                    .await;

                tracing::info!(
                    room_id = %room_id,
                    target = ?next_target,
                    mode = ?mode,
                    "Auto-played next media"
                );

                Ok(Some(saved_state))
            },
        )
        .await
    }

    /// Check if media has ended and auto-play next if needed
    ///
    /// This should be called when playback `position` is updated.
    /// It checks if the current time has reached or exceeded the media duration.
    pub async fn check_and_auto_play(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        position: f64,
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
                .get_room_media(room_id, id)
                .await?
                .ok_or_else(|| Error::NotFound("Current media not found".to_string()))?,
            None => return Ok(None),
        };

        // A negative position (-1.0) is an explicit "media ended" signal from the client
        if position < 0.0 {
            return self.play_next(room_id, settings).await;
        }

        // Try to get duration from source_config metadata (any provider may store it)
        let duration = playing_media
            .source_config
            .get("metadata")
            .and_then(|m| m.get("duration"))
            .and_then(serde_json::Value::as_f64);

        // Use computed time to account for elapsed wall-clock time when playing
        let effective_time = state.computed_position();

        // Check if position is near end (within 1 second or past end)
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
        self.update_state_with_outbox(room_id, update_fn, None)
            .await
    }

    pub async fn update_state_with_outbox<F>(
        &self,
        room_id: RoomId,
        update_fn: F,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState>
    where
        F: Fn(&mut RoomPlaybackState),
    {
        self.update_state_checked_with_outbox(
            room_id,
            |state| {
                update_fn(state);
                Ok(())
            },
            outbox_event_factory,
        )
        .await
    }

    async fn update_state_checked_with_outbox<F>(
        &self,
        room_id: RoomId,
        update_fn: F,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState>
    where
        F: Fn(&mut RoomPlaybackState) -> Result<()>,
    {
        crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            Self::UPDATE_STATE_RETRY_EXHAUSTED,
            || {
                let update_fn = &update_fn;
                let outbox_event_factory = outbox_event_factory.clone();
                async move {
                    let mut state = match self.playback_repo.get(&room_id).await? {
                        Some(s) => s,
                        None => self.playback_repo.create_or_get(&room_id).await?,
                    };

                    let observed_version = state.version;
                    let previous_state = state.clone();
                    update_fn(&mut state)?;
                    let previous_progress_position =
                        previous_progress_position_for_source_transition(&previous_state, &state);

                    let updated_state = self
                        .persist_playback_update_with_previous_progress(
                            &state,
                            observed_version,
                            previous_progress_position,
                            outbox_event_factory.as_ref(),
                        )
                        .await?;
                    self.write_playback_cache(&updated_state).await;

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

    /// Reset playback to initial state
    pub async fn reset(&self, room_id: RoomId, user_id: UserId) -> Result<RoomPlaybackState> {
        self.reset_with_outbox(room_id, user_id, None).await
    }

    pub async fn reset_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.reset_internal(room_id, user_id, false, outbox_event_factory)
            .await
    }

    /// Management-only playback reset that bypasses room membership-derived permissions.
    ///
    /// Callers must validate global admin/root identity before invoking this method.
    pub async fn admin_reset(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
    ) -> Result<RoomPlaybackState> {
        self.admin_reset_with_outbox(room_id, actor_user_id, None)
            .await
    }

    pub async fn admin_reset_with_outbox(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.reset_internal(room_id, actor_user_id, true, outbox_event_factory)
            .await
    }

    pub async fn broadcast_playback_reset_after_force_delete(
        &self,
        state: RoomPlaybackState,
    ) -> RoomPlaybackState {
        self.invalidate_playback_cache(&state.room_id).await;
        self.broadcast_invalidation_with_retry(
            &state.room_id,
            &state,
            "broadcast_playback_reset_after_force_delete",
        )
        .await;
        state
    }

    pub async fn reset_playback_for_creator(
        &self,
        creator_id: &UserId,
    ) -> Result<Vec<RoomPlaybackState>> {
        self.reset_playback_for_creator_with_outbox(creator_id, None)
            .await
    }

    pub async fn reset_playback_for_creator_with_outbox(
        &self,
        creator_id: &UserId,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<Vec<RoomPlaybackState>> {
        let states = {
            let mut tx = self.playback_repo.pool().begin().await?;
            let impacted_states = self
                .playback_repo
                .find_playback_for_creator_with_executor(creator_id, &mut *tx)
                .await?;
            let mut reset_states = Vec::with_capacity(impacted_states.len());
            let mut reservations = Vec::with_capacity(impacted_states.len());

            let reset_result: Result<()> = async {
                for mut state in impacted_states {
                    let reservation = self
                        .begin_playback_write_from_db_version(&state.room_id, state.version)
                        .await?;
                    let reserved_version = reservation
                        .as_ref()
                        .map_or(state.version + 1, |reservation| reservation.version);
                    let room_id = state.room_id;
                    reservations.push((room_id, reservation));

                    let previous_state = state.clone();
                    state.playing_media_id = None;
                    state.playing_playlist_id = None;
                    state.target.clear();
                    state.position = 0.0;
                    state.speed = 1.0;
                    state.is_playing = false;
                    state.updated_at = chrono::Utc::now();
                    let previous_progress_position =
                        previous_progress_position_for_source_transition(&previous_state, &state);

                    let updated = self
                        .playback_repo
                        .update_with_exact_version_executor_and_previous_progress(
                            &state,
                            reserved_version,
                            previous_progress_position,
                            &mut tx,
                        )
                        .await?;
                    if let Some(outbox) = &self.realtime_outbox {
                        if let Some(event) = outbox_event_factory
                            .as_ref()
                            .map(|factory| factory(&updated))
                            .transpose()?
                        {
                            outbox.insert_with_executor(&event, &mut *tx).await?;
                        }
                    }
                    reset_states.push(updated);
                }
                Ok(())
            }
            .await;

            if let Err(error) = reset_result {
                for (room_id, reservation) in &reservations {
                    self.abort_playback_write(room_id, reservation.as_ref())
                        .await;
                }
                return Err(error);
            }

            if let Err(error) = tx.commit().await {
                for (room_id, reservation) in &reservations {
                    self.abort_playback_write(room_id, reservation.as_ref())
                        .await;
                }
                return Err(error.into());
            }
            for (state, (_, reservation)) in reset_states.iter().zip(reservations.iter()) {
                self.finalize_committed_playback_write_best_effort(
                    &state.room_id,
                    reservation.as_ref(),
                    state.version,
                    "reset_playback_for_creator",
                )
                .await;
            }
            reset_states
        };

        for state in &states {
            self.write_playback_cache(state).await;
            self.broadcast_invalidation_with_retry(
                &state.room_id,
                state,
                "reset_playback_for_creator",
            )
            .await;
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
    pub async fn get_position(&self, room_id: &RoomId) -> Result<f64> {
        let state = self.get_state(room_id).await?;
        Ok(state.computed_position())
    }

    #[allow(clippy::too_many_arguments)]
    async fn switch_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Vec<u8>,
        bypass_room_permissions: bool,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        if !bypass_room_permissions {
            self.permission_service
                .check_permission(
                    &room_id,
                    &user_id,
                    crate::models::RoomPermission::CHANGE_CURRENT_MEDIA,
                )
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
                .update_state_with_outbox(
                    room_id,
                    |state| {
                        state.playing_media_id = None;
                        state.playing_playlist_id = None;
                        state.target = Vec::new();
                        state.position = 0.0;
                        state.speed = 1.0;
                        state.is_playing = false;
                        state.updated_at = chrono::Utc::now();
                    },
                    outbox_event_factory,
                )
                .await?;

            return Ok(state);
        }

        if let Some(ref media_id) = target.media_id {
            let media = self
                .media_service
                .get_room_media(&room_id, media_id)
                .await?
                .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

            self.ensure_creator_is_active(media.creator_id.as_ref(), "Media")
                .await?;
        }

        if let Some(ref playlist_id) = target.playlist_id {
            let playlist = self
                .media_service
                .get_room_playlist(&room_id, playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

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
            .update_state_with_outbox(
                room_id,
                |state| {
                    state.playing_media_id.clone_from(&target.media_id);
                    state.playing_playlist_id.clone_from(&target.playlist_id);
                    state.target.clone_from(&target.target);
                    state.position = 0.0;
                    state.is_playing = true;
                    state.updated_at = chrono::Utc::now();
                },
                outbox_event_factory,
            )
            .await?;

        Ok(state)
    }

    async fn reset_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        bypass_room_permissions: bool,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        if !bypass_room_permissions {
            self.permission_service
                .check_permission(
                    &room_id,
                    &user_id,
                    crate::models::RoomPermission::PLAY_CONTROL,
                )
                .await?;
        }

        let state = self
            .update_state_with_outbox(
                room_id,
                |state| {
                    state.is_playing = false;
                    state.position = 0.0;
                    state.speed = 1.0;
                    state.playing_media_id = None;
                    state.playing_playlist_id = None;
                    state.target = Vec::new();
                    state.updated_at = chrono::Utc::now();
                },
                outbox_event_factory,
            )
            .await?;

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
                state.position = 0.0;
                state.speed = 1.0;
                state.is_playing = false;
                state.updated_at = chrono::Utc::now();
            })
            .await?;

        Ok(Some(state))
    }

    /// Get current playback speed
    pub async fn get_speed(&self, room_id: &RoomId) -> Result<f64> {
        let state = self.get_state(room_id).await?;
        Ok(state.speed)
    }

    pub async fn update_playback_state(
        &self,
        request: PlaybackUpdateRequest,
    ) -> Result<RoomPlaybackState> {
        self.update_playback_state_internal(request, false).await
    }

    /// Management-only multi-field playback update. Callers must validate
    /// global admin/root identity before this bypasses room membership checks.
    pub async fn admin_update_playback_state(
        &self,
        request: PlaybackUpdateRequest,
    ) -> Result<RoomPlaybackState> {
        self.update_playback_state_internal(request, true).await
    }

    async fn update_playback_state_internal(
        &self,
        request: PlaybackUpdateRequest,
        bypass_permission: bool,
    ) -> Result<RoomPlaybackState> {
        let PlaybackUpdateRequest {
            room_id,
            actor_user_id,
            patch,
            expected_version,
            expected_source,
            outbox_event_factory,
        } = request;
        let PlaybackStatePatch {
            playing,
            position,
            speed,
        } = patch;

        // Check permissions based on what's being updated
        let mut required_permissions = Vec::new();
        if playing.is_some() {
            required_permissions.push(crate::models::RoomPermission::PLAY_CONTROL);
        }
        if position.is_some() {
            required_permissions.push(crate::models::RoomPermission::PLAY_CONTROL);
        }
        if speed.is_some() {
            required_permissions.push(crate::models::RoomPermission::CHANGE_PLAYBACK_RATE);
        }
        if !required_permissions.is_empty() && !bypass_permission {
            self.permission_service
                .check_permissions(&room_id, &actor_user_id, &required_permissions)
                .await?;
        }

        if let Some(ct) = position {
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
            if needs_snapshot && position.is_none() {
                // Only snapshot if the caller didn't provide an explicit position
                state.position = state.computed_position();
            }

            if let Some(p) = playing {
                state.is_playing = p;
            }
            if let Some(ct) = position {
                state.position = ct;
            }
            if let Some(s) = speed {
                state.speed = s;
            }
            state.updated_at = chrono::Utc::now();
            // version is incremented by the SQL UPDATE, not here
        };

        let state = if expected_version.is_some() || expected_source.is_some() {
            let mut state = match self.playback_repo.get(&room_id).await? {
                Some(state) => state,
                None => self.playback_repo.create_or_get(&room_id).await?,
            };

            if position.is_some() {
                validate_position_update_source(&state)?;
            }
            if expected_version.is_some_and(|expected| state.version != expected) {
                return Err(Error::OptimisticLockConflict);
            }
            if expected_source
                .as_ref()
                .is_some_and(|expected| !expected.matches(&state))
            {
                return Err(Error::OptimisticLockConflict);
            }

            let observed_version = state.version;
            apply_update(&mut state);
            let updated_state = self
                .persist_playback_update_with_previous_progress(
                    &state,
                    observed_version,
                    None,
                    outbox_event_factory.as_ref(),
                )
                .await?;
            self.write_playback_cache(&updated_state).await;

            self.broadcast_invalidation_with_retry(&room_id, &updated_state, "update_state")
                .await;

            updated_state
        } else if position.is_some() {
            self.update_state_checked_with_outbox(
                room_id,
                |state| {
                    validate_position_update_source(state)?;
                    apply_update(state);
                    Ok(())
                },
                outbox_event_factory,
            )
            .await?
        } else {
            self.update_state_with_outbox(room_id, apply_update, outbox_event_factory)
                .await?
        };

        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheInvalidationService, CacheL2Backend, KeyBuilder, UsernameCache};
    use crate::models::{RoomId, SignupMethod, User, UserRole, UserStatus};
    use crate::repository::{
        MediaRepository, PlaylistRepository, ProviderInstanceRepository,
        RoomPlaybackStateRepository, RoomRepository,
    };
    use crate::service::permission::PermissionService;
    use crate::service::{
        auth::{BruteForceProtection, JwtService},
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
            _new_ts_millis: i64,
        ) -> Result<bool> {
            Ok(true)
        }

        async fn set_if_version_at_least(
            &self,
            _key: &str,
            _json: &str,
            _ttl_secs: u64,
            _version: i64,
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

    fn make_user_service(pool: &PgPool) -> UserService {
        let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
        let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
        let key_builder = KeyBuilder::new("test");
        let brute_force = BruteForceProtection::in_memory("test".to_string());

        UserService::new_for_tests(
            pool,
            jwt_service,
            username_cache,
            token_blacklist,
            key_builder,
            brute_force,
        )
    }

    fn make_user(username: &str) -> User {
        let now = chrono::Utc::now();
        User {
            id: UserId::new(),
            username: username.to_string(),
            role: UserRole::User,
            avatar_file_reference_id: None,
            status: UserStatus::Active,
            signup_method: SignupMethod::Email,
            created_at: now,
            updated_at: now,
            version: 0,
            deleted_at: None,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        }
    }

    fn make_playback_service_for_lifecycle_tests(
    ) -> (PlaybackService, Arc<CacheInvalidationService>) {
        make_playback_service_for_lifecycle_tests_with_l2(None)
    }

    fn make_playback_service_for_lifecycle_tests_with_l2(
        l2_cache: Option<PlaybackStateCache>,
    ) -> (PlaybackService, Arc<CacheInvalidationService>) {
        let pool = PgPool::connect_lazy("postgres://localhost/test")
            .expect("lazy postgres pool for unit tests should build");
        let member_repo = crate::repository::RoomMemberRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let permission_service = PermissionService::without_cache(member_repo, room_repo, None)
            .expect("permission service should build");
        let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
            provider_repo,
            None,
        ));
        let providers_manager = Arc::new(
            ProvidersManager::new(provider_instance_manager)
                .expect("providers manager should build"),
        );
        let media_service = MediaService::new(
            MediaRepository::new(pool.clone()),
            PlaylistRepository::new(pool.clone()),
            permission_service.clone(),
            providers_manager,
            NotificationService::default(),
        );
        let user_service = make_user_service(&pool);
        let invalidation_service = Arc::new(CacheInvalidationService::new(
            "node-test".to_string(),
            "synctv:test:cache:invalidate".to_string(),
        ));
        let playback_service = PlaybackService::new_with_runtime(
            RoomPlaybackStateRepository::new(pool.clone()),
            permission_service,
            media_service,
            user_service,
            Some(invalidation_service.clone()),
            l2_cache,
            None,
            None,
        );
        (playback_service, invalidation_service)
    }

    #[tokio::test]
    async fn standalone_playback_service_uses_non_authoritative_fence_by_default() {
        let pool = PgPool::connect_lazy("postgres://localhost/test")
            .expect("lazy postgres pool for unit tests should build");
        let member_repo = crate::repository::RoomMemberRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let permission_service = PermissionService::without_cache(member_repo, room_repo, None)
            .expect("permission service should build");
        let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
            provider_repo,
            None,
        ));
        let providers_manager = Arc::new(
            ProvidersManager::new(provider_instance_manager)
                .expect("providers manager should build"),
        );
        let media_service = MediaService::new(
            MediaRepository::new(pool.clone()),
            PlaylistRepository::new(pool.clone()),
            permission_service.clone(),
            providers_manager,
            NotificationService::default(),
        );
        let service = PlaybackService::new(
            RoomPlaybackStateRepository::new(pool.clone()),
            permission_service,
            media_service,
            make_user_service(&pool),
        );

        assert!(
            !service.consistency.is_authoritative(),
            "standalone playback constructors must not create private authoritative fences"
        );
    }

    #[tokio::test]
    async fn write_playback_cache_refreshes_l1_when_l2_is_configured() {
        let pool = PgPool::connect_lazy("postgres://localhost/test")
            .expect("lazy postgres pool for unit tests should build");
        let member_repo = crate::repository::RoomMemberRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let permission_service = PermissionService::without_cache(member_repo, room_repo, None)
            .expect("permission service should build");
        let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
            provider_repo,
            None,
        ));
        let providers_manager = Arc::new(
            ProvidersManager::new(provider_instance_manager)
                .expect("providers manager should build"),
        );
        let media_service = MediaService::new(
            MediaRepository::new(pool.clone()),
            PlaylistRepository::new(pool.clone()),
            permission_service.clone(),
            providers_manager,
            NotificationService::default(),
        );
        let l2_backend = Arc::new(CountingL2Backend::default());
        let l2_cache = PlaybackStateCache::new(
            l2_backend,
            100,
            PlaybackService::DEFAULT_CACHE_TTL_SECS,
            60,
            "test:playback:l1-refresh:".to_string(),
        );
        let service = PlaybackService::new_with_runtime(
            RoomPlaybackStateRepository::new(pool.clone()),
            permission_service,
            media_service,
            make_user_service(&pool),
            None,
            Some(l2_cache),
            None,
            None,
        );

        let room_id = RoomId::expect_positive(10_004);
        let cache_key = room_id.to_string();
        let stale_state = RoomPlaybackState {
            room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            target: Vec::new(),
            current_progress_id: None,
            position: 10.0,
            speed: 1.0,
            is_playing: false,
            updated_at: chrono::Utc::now(),
            version: 3,
        };
        service
            .playback_cache
            .insert(cache_key.clone(), stale_state)
            .await;

        let fresh_state = RoomPlaybackState {
            room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            target: Vec::new(),
            current_progress_id: None,
            position: 42.0,
            speed: 1.0,
            is_playing: true,
            updated_at: chrono::Utc::now(),
            version: 4,
        };
        service.write_playback_cache(&fresh_state).await;

        let cached = service
            .playback_cache
            .get(&cache_key)
            .await
            .expect("local L1 cache should be refreshed by local write");
        assert_eq!(cached.version, fresh_state.version);
        assert!((cached.position - fresh_state.position).abs() < f64::EPSILON);
        assert!(cached.is_playing);
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
    fn test_position_update_requires_current_playback_source() {
        let mut state = RoomPlaybackState {
            room_id: RoomId::expect_positive(20_001),
            playing_media_id: None,
            playing_playlist_id: None,
            target: Vec::new(),
            current_progress_id: None,
            position: 0.0,
            speed: 1.0,
            is_playing: false,
            updated_at: chrono::Utc::now(),
            version: 1,
        };

        let err = validate_position_update_source(&state).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        state.playing_media_id = Some(MediaId::expect_positive(30_001));
        assert!(validate_position_update_source(&state).is_ok());

        state.playing_media_id = None;
        state.playing_playlist_id = Some(PlaylistId::expect_positive(40_001));
        state.target = b"dynamic-target".to_vec();
        assert!(validate_position_update_source(&state).is_ok());
    }

    #[test]
    fn test_switch_target_source_shape_matches_progress_schema() {
        assert!(validate_switch_target(&SwitchPlaybackTarget {
            media_id: Some(MediaId::expect_positive(30_010)),
            playlist_id: None,
            target: Vec::new(),
        })
        .is_ok());
        assert!(validate_switch_target(&SwitchPlaybackTarget {
            media_id: None,
            playlist_id: Some(PlaylistId::expect_positive(40_010)),
            target: b"dynamic-target".to_vec(),
        })
        .is_ok());
        assert!(validate_switch_target(&SwitchPlaybackTarget {
            media_id: Some(MediaId::expect_positive(30_011)),
            playlist_id: None,
            target: b"static-media-must-not-have-target".to_vec(),
        })
        .is_err());
        assert!(validate_switch_target(&SwitchPlaybackTarget {
            media_id: None,
            playlist_id: Some(PlaylistId::expect_positive(40_011)),
            target: Vec::new(),
        })
        .is_err());
    }

    #[tokio::test]
    async fn test_invalidation_listener_stops_after_cache_invalidation_service_stop() {
        let (playback_service, invalidation_service) = make_playback_service_for_lifecycle_tests();
        let room_id = RoomId::expect_positive(10_001);
        let cache_key = room_id.to_string();

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
            current_progress_id: None,
            position: 42.0,
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
        let (playback_service, invalidation_service) = make_playback_service_for_lifecycle_tests();
        let room_id = RoomId::expect_positive(10_002);
        let cache_key = room_id.to_string();

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
            current_progress_id: None,
            position: 64.0,
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
        let (playback_service, invalidation_service) = make_playback_service_for_lifecycle_tests();
        let room_id = RoomId::expect_positive(10_003);
        let cache_key = room_id.to_string();

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
            current_progress_id: None,
            position: 88.0,
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
    async fn test_started_invalidation_listener_uses_configured_l2_cache() {
        let backend = Arc::new(CountingL2Backend::default());
        let l2_cache = PlaybackStateCache::new(
            backend.clone(),
            16,
            5,
            60,
            "synctv:test:playback:".to_string(),
        );
        let (playback_service, invalidation_service) =
            make_playback_service_for_lifecycle_tests_with_l2(Some(l2_cache));
        let room_id = RoomId::expect_positive(10_004);

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
        .expect("listener should invalidate configured L2 cache");

        playback_service.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_db_reload_seeds_missing_local_playback_fence() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let user_repo = crate::repository::UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let owner = user_repo
            .create(&make_user("playback_seed_fence_owner"))
            .await
            .expect("owner should be created");
        let room = room_repo
            .create(&crate::models::Room::new(
                "Playback Seed Fence".to_string(),
                owner.id,
            ))
            .await
            .expect("room should be created");
        let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
        let mut state = playback_repo
            .create_or_get(&room.id)
            .await
            .expect("playback state should exist");
        state.position = 42.0;
        let state = playback_repo
            .update_with_exact_version(&state, 5)
            .await
            .expect("playback state should have a nonzero version");

        let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
        let member_repo = crate::repository::RoomMemberRepository::new(pool.clone());
        let permission_service = PermissionService::without_cache(member_repo, room_repo, None)
            .expect("permission service should build");
        let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
            provider_repo,
            None,
        ));
        let providers_manager = Arc::new(
            ProvidersManager::new(provider_instance_manager)
                .expect("providers manager should build"),
        );
        let media_service = MediaService::new(
            MediaRepository::new(pool.clone()),
            PlaylistRepository::new(pool.clone()),
            permission_service.clone(),
            providers_manager,
            NotificationService::default(),
        );
        let playback_service = PlaybackService::new_with_runtime(
            playback_repo,
            permission_service,
            media_service,
            make_user_service(&pool),
            None,
            None,
            Some(fence.clone()),
            None,
        );
        let domain = CacheDomain::Playback { room_id: room.id };
        assert_eq!(
            fence
                .current_version(&domain)
                .await
                .expect("local fence should be readable"),
            None
        );

        let loaded = playback_service
            .get_state(&room.id)
            .await
            .expect("strong playback read should fall back to DB");

        assert_eq!(loaded.version, state.version);
        assert_eq!(
            fence
                .current_version(&domain)
                .await
                .expect("local fence should be readable"),
            Some(state.version)
        );
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
        fn make_state(room_id: i64, version: i64, position: f64) -> RoomPlaybackState {
            RoomPlaybackState {
                room_id: RoomId::expect_positive(room_id),
                playing_media_id: None,
                playing_playlist_id: None,
                target: Vec::new(),
                current_progress_id: None,
                position,
                speed: 1.0,
                is_playing: false,
                updated_at: chrono::Utc::now(),
                version,
            }
        }

        #[allow(clippy::cast_precision_loss)]
        fn version_to_position(version: i64) -> f64 {
            version as f64 * 10.0
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
            assert!((cached.position - 100.0).abs() < f64::EPSILON);
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
            assert!((cached.position - 150.0).abs() < f64::EPSILON);
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
            assert!((cached.position - 200.0).abs() < f64::EPSILON);
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
            assert!((cached.position - 200.0).abs() < f64::EPSILON);
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
                let state = make_state(room_id, v, version_to_position(v));
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
            assert!((cached.position - 70.0).abs() < f64::EPSILON);
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
                        let state = make_state(room_id, v, version_to_position(v));
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
            assert!((cached.position - 100.0).abs() < f64::EPSILON);
        }
    }
}
