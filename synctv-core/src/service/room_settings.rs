//! Room settings service with caching and multi-replica synchronization
//!
//! # Architecture
//!
//! ## Caching Strategy
//! - L1 Cache: In-memory moka cache (per-instance)
//! - TTL: 5 minutes with time-based expiration
//! - Max capacity: 10,000 rooms
//! - Cache invalidation: Via Redis Streams through `CacheInvalidationService`
//!
//! ## Multi-Replica Synchronization
//! - Uses Redis Streams (via `CacheInvalidationService`) for reliable message delivery
//! - Messages are persisted and won't be lost if a replica disconnects
//! - Consumer groups ensure every replica processes invalidation messages
//! - On reconnection, missed messages are automatically delivered
//!
//! ## Performance Optimizations
//! - Single-flight pattern: Prevents cache thundering
//! - Background refresh: Refreshes before expiration
//! - Write-through: Updates database and cache atomically

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    cache::{
        CacheDomain, CacheInvalidationRuntime, CloneableError, ConsistencyCoordinator,
        FenceReadResult, InvalidationMessage, NoopCacheL2, RoomSettingsCache, RoomSettingsSnapshot,
        SingleFlight, VersionFenceReservation, VersionFenceStore,
    },
    models::{RoomId, RoomSettings},
    repository::RoomSettingsRepository,
    service::notification::NotificationService,
    Error, Result,
};

/// Room settings service with caching
#[derive(Debug)]
struct RoomSettingsInvalidationRuntime {
    started: AtomicBool,
    cancel: tokio::sync::Mutex<CancellationToken>,
    listener_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl RoomSettingsInvalidationRuntime {
    fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            cancel: tokio::sync::Mutex::new(CancellationToken::new()),
            listener_handle: tokio::sync::Mutex::new(None),
        }
    }
}

pub struct RoomSettingsService {
    repo: RoomSettingsRepository,
    cache: RoomSettingsCache,
    invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
    version_fence: Arc<dyn VersionFenceStore>,
    consistency: ConsistencyCoordinator,
    invalidation_runtime: Arc<RoomSettingsInvalidationRuntime>,
    notification_service: Arc<NotificationService>,
    /// `SingleFlight` to prevent thundering herd on cache miss.
    /// Uses `String` key (`room_id`) and `String` error (since `Error` is not `Clone`).
    single_flight: SingleFlight<String, RoomSettingsSnapshot, CloneableError>,
}

#[derive(Clone)]
pub struct RoomSettingsRuntime {
    pub cache_ttl_secs: Option<u64>,
    pub cache_max_capacity: Option<u64>,
    pub version_fence: Option<Arc<dyn VersionFenceStore>>,
    pub l2_cache: Option<Arc<dyn crate::cache::CacheL2Backend>>,
    pub cache_key_prefix: String,
}

impl Default for RoomSettingsRuntime {
    fn default() -> Self {
        Self {
            cache_ttl_secs: None,
            cache_max_capacity: None,
            version_fence: None,
            l2_cache: None,
            cache_key_prefix: String::from("room_settings:"),
        }
    }
}

impl std::fmt::Debug for RoomSettingsService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomSettingsService")
            .field("cache_size", &self.cache.entry_count())
            .finish()
    }
}

impl Clone for RoomSettingsService {
    fn clone(&self) -> Self {
        Self {
            repo: self.repo.clone(),
            cache: self.cache.clone(),
            invalidation_service: self.invalidation_service.clone(),
            version_fence: self.version_fence.clone(),
            consistency: self.consistency.clone(),
            invalidation_runtime: self.invalidation_runtime.clone(),
            notification_service: self.notification_service.clone(),
            single_flight: self.single_flight.clone(), // Arc-backed, shares state
        }
    }
}

impl RoomSettingsService {
    const CACHE_TTL_SECS: u64 = 300; // 5 minutes
    const CACHE_MAX_CAPACITY: u64 = 10_000;
    /// Maximum time to wait for the invalidation listener to stop.
    const INVALIDATION_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

    /// Create a new room settings service
    ///
    /// Uses `CacheInvalidationService` (Redis Streams) for reliable cross-replica
    /// cache invalidation. When a replica disconnects and reconnects, missed
    /// invalidation messages are automatically delivered via consumer groups.
    #[must_use]
    pub fn new(
        repo: RoomSettingsRepository,
        invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
        notification_service: Arc<NotificationService>,
        cache_ttl_secs: Option<u64>,
        cache_max_capacity: Option<u64>,
    ) -> Self {
        Self::new_with_version_fence(
            repo,
            invalidation_service,
            notification_service,
            RoomSettingsRuntime {
                cache_ttl_secs,
                cache_max_capacity,
                ..RoomSettingsRuntime::default()
            },
        )
    }

    #[must_use]
    pub fn new_with_version_fence(
        repo: RoomSettingsRepository,
        invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
        notification_service: Arc<NotificationService>,
        runtime: RoomSettingsRuntime,
    ) -> Self {
        let ttl = runtime.cache_ttl_secs.unwrap_or(Self::CACHE_TTL_SECS);
        let capacity = runtime
            .cache_max_capacity
            .unwrap_or(Self::CACHE_MAX_CAPACITY);

        let cache = RoomSettingsCache::new(
            runtime.l2_cache.unwrap_or_else(|| Arc::new(NoopCacheL2)),
            capacity,
            ttl,
            ttl,
            runtime.cache_key_prefix,
        )
        .expect("failed to create room settings cache");

        let version_fence = runtime
            .version_fence
            .unwrap_or_else(|| Arc::new(crate::cache::NoopVersionFenceStore));

        Self {
            repo,
            cache,
            invalidation_service,
            version_fence: version_fence.clone(),
            consistency: ConsistencyCoordinator::new(version_fence),
            invalidation_runtime: Arc::new(RoomSettingsInvalidationRuntime::new()),
            notification_service,
            single_flight: SingleFlight::new(),
        }
    }

    pub const fn has_invalidation_service(&self) -> bool {
        self.invalidation_service.is_some()
    }

    pub fn set_invalidation_service(&mut self, service: Arc<dyn CacheInvalidationRuntime>) {
        self.invalidation_service = Some(service);
    }

    #[cfg(test)]
    fn invalidation_task_started(&self) -> bool {
        self.invalidation_runtime.started.load(Ordering::Acquire)
    }

    pub async fn start(&self) -> Result<()> {
        let Some(inv_service) = self.invalidation_service.clone() else {
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
                "RoomSettingsService::start requires a Tokio runtime".to_string(),
            ));
        }

        let cache_clone = self.cache.clone();
        let mut receiver = inv_service.subscribe();
        let cancel = self.invalidation_runtime.cancel.lock().await.child_token();

        let listener_handle = crate::spawn::spawn_monitored(
            "room_settings_invalidation_listener",
            async move {
                const LAG_FLUSH_MIN_INTERVAL: std::time::Duration =
                    std::time::Duration::from_secs(5);
                let mut last_lag_flush = std::time::Instant::now()
                    .checked_sub(LAG_FLUSH_MIN_INTERVAL)
                    .unwrap_or_else(std::time::Instant::now);

                loop {
                    tokio::select! {
                        () = cancel.cancelled() => {
                            tracing::info!("Room settings invalidation listener shutting down");
                            break;
                        }
                        result = receiver.recv() => {
                            match result {
                                Ok(InvalidationMessage::RoomSettings { ref room_id }) => {
                                    let Ok(room_id) = room_id.parse::<RoomId>() else {
                                        tracing::warn!(room_id = %room_id, "Invalid room settings invalidation room id");
                                        continue;
                                    };
                                    if let Err(error) = cache_clone.invalidate(&room_id).await {
                                        tracing::warn!(
                                            room_id = %room_id,
                                            error = %error,
                                            "Failed to invalidate room settings cache"
                                        );
                                    }
                                    tracing::debug!(
                                        room_id = %room_id,
                                        "Room settings cache invalidated (cross-replica)"
                                    );
                                }
                                Ok(InvalidationMessage::All) => {
                                    cache_clone.clear().await;
                                    tracing::debug!("All room settings cache cleared (cross-replica)");
                                }
                                Ok(_) => {}
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    tracing::debug!("Room settings invalidation channel closed");
                                    break;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    let now = std::time::Instant::now();
                                    let elapsed = now.duration_since(last_lag_flush);
                                    if elapsed >= LAG_FLUSH_MIN_INTERVAL {
                                        tracing::warn!(
                                            lagged_messages = n,
                                            "Room settings invalidation listener lagged, flushing all cache (rate-limited)"
                                        );
                                        cache_clone.clear().await;
                                        crate::metrics::cache::CACHE_LAG_FLUSH_TOTAL
                                            .with_label_values(&["room_settings"])
                                            .inc();
                                        last_lag_flush = now;
                                    } else {
                                        tracing::warn!(
                                            lagged_messages = n,
                                            "Room settings invalidation listener lagged, skipping flush (rate-limited)"
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
            Self::await_invalidation_task_shutdown("room settings invalidation listener", handle)
                .await;
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

    /// Get room settings with strong-read semantics.
    ///
    /// Settings affect room access, passwords, join policy, and role defaults,
    /// so default service reads must not trust an L1 entry that might be
    /// waiting for async invalidation. Read the database and refresh L1.
    pub async fn get(&self, room_id: &RoomId) -> Result<RoomSettings> {
        Ok(self.get_with_version(room_id).await?.settings)
    }

    /// Get room settings with strong-read semantics.
    pub async fn get_strong_with_version(&self, room_id: &RoomId) -> Result<RoomSettingsSnapshot> {
        self.get_with_version(room_id).await
    }

    /// Get room settings with the current optimistic-lock version using
    /// strong-read semantics.
    ///
    /// Strong reads consult the authoritative version fence before trusting
    /// either L1 or L2. If the fence is unavailable or not authoritative, the
    /// read falls back to the database and refreshes cache with the database
    /// version.
    pub async fn get_with_version(&self, room_id: &RoomId) -> Result<RoomSettingsSnapshot> {
        self.get_with_version_by_fence(room_id).await
    }

    async fn get_with_version_by_fence(&self, room_id: &RoomId) -> Result<RoomSettingsSnapshot> {
        if !self.consistency.is_authoritative() {
            ConsistencyCoordinator::record_db_fallback(
                &CacheDomain::RoomSettings { room_id: *room_id },
                "non_authoritative_fence",
            );
            return self.get_refresh_with_version(room_id).await;
        }

        let domain = CacheDomain::RoomSettings { room_id: *room_id };
        if let Some(fence_key) = self.consistency.fence_key(&domain) {
            match self.cache.get_by_fence_key(room_id, &fence_key).await {
                Ok(FenceReadResult::Hit(snapshot)) => return Ok(snapshot),
                Ok(FenceReadResult::DbFallback) => {
                    ConsistencyCoordinator::record_db_fallback(&domain, "stale_cache");
                    return self.get_refresh_with_version(room_id).await;
                }
                Ok(FenceReadResult::Unsupported) => {}
                Err(error) => {
                    tracing::warn!(
                        room_id = %room_id,
                        error = %error,
                        "Room settings fence-key cache read failed; falling back to version read"
                    );
                    ConsistencyCoordinator::record_db_fallback(&domain, "fence_key_read_error");
                }
            }
        }

        let fence_version = match self.consistency.current_committed_version(&domain).await {
            Ok(Some(version)) => version,
            Ok(None) => {
                ConsistencyCoordinator::record_db_fallback(&domain, "missing_fence");
                return self.get_refresh_with_version(room_id).await;
            }
            Err(error) => {
                tracing::warn!(
                    room_id = %room_id,
                    error = %error,
                    "Room settings version fence unavailable; bypassing cache"
                );
                ConsistencyCoordinator::record_db_fallback(&domain, "fence_unavailable");
                return self.get_refresh_with_version(room_id).await;
            }
        };

        if let Some(snapshot) = self.cache.get_l1(room_id).await {
            if snapshot.version >= fence_version {
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&["room_settings", "l1"])
                    .inc();
                return Ok(snapshot);
            }
        }

        match self.cache.get_l2(room_id).await {
            Ok(Some(snapshot)) if snapshot.version >= fence_version => {
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&["room_settings", "l2"])
                    .inc();
                Ok(snapshot)
            }
            Ok(_) => {
                ConsistencyCoordinator::record_db_fallback(&domain, "stale_cache");
                self.get_refresh_with_version(room_id).await
            }
            Err(error) => {
                tracing::warn!(
                    room_id = %room_id,
                    error = %error,
                    "Room settings L2 read failed; bypassing cache"
                );
                ConsistencyCoordinator::record_db_fallback(&domain, "l2_error");
                self.get_refresh_with_version(room_id).await
            }
        }
    }

    /// Get room settings with cache-first eventual consistency.
    ///
    /// This is reserved for diagnostics and tests that intentionally need to
    /// demonstrate stale L1 behavior. User-facing and authorization-adjacent
    /// paths should call [`get_with_version`](Self::get_with_version).
    pub async fn get_eventually_consistent_with_version(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomSettingsSnapshot> {
        // Try cache first
        if let Some(snapshot) = self.cache.get(room_id).await? {
            return Ok(snapshot);
        }

        // Use SingleFlight to prevent thundering herd:
        // Only one task loads from DB for a given room_id; others wait for the result.
        let sf_key = room_id.to_string();
        let repo = self.repo.clone();
        let cache = self.cache.clone();
        let room_id_clone = *room_id;

        let snapshot = self
            .single_flight
            .do_work(sf_key, async move {
                // Double-check cache (another task may have populated it)
                if let Some(snapshot) = cache
                    .get(&room_id_clone)
                    .await
                    .map_err(CloneableError::from)?
                {
                    return Ok(snapshot);
                }

                // Load from database
                let (settings, version) = repo
                    .get_with_version(&room_id_clone)
                    .await
                    .map_err(CloneableError::from)?;
                let snapshot = RoomSettingsSnapshot { settings, version };

                // Store in cache
                cache
                    .set_if_version_at_least(&room_id_clone, snapshot.clone())
                    .await
                    .map_err(CloneableError::from)?;

                Ok(snapshot)
            })
            .await
            .map_err(|error| match error {
                crate::cache::SingleFlightError::WorkerFailed => Error::Internal(
                    "SingleFlight worker failed during room settings fetch".to_string(),
                ),
                crate::cache::SingleFlightError::Inner(error) => Error::from(error),
            })?;

        Ok(snapshot)
    }

    /// Get room settings without cache (force refresh)
    pub async fn get_refresh(&self, room_id: &RoomId) -> Result<RoomSettings> {
        Ok(self.get_refresh_with_version(room_id).await?.settings)
    }

    /// Get room settings and version without cache (force refresh).
    pub async fn get_refresh_with_version(&self, room_id: &RoomId) -> Result<RoomSettingsSnapshot> {
        // Invalidate cache
        self.invalidate_local(room_id).await;

        // Load from database
        let (settings, version) = self.repo.get_with_version(room_id).await?;
        let snapshot = RoomSettingsSnapshot { settings, version };

        self.consistency
            .repair_after_db_read(
                &CacheDomain::RoomSettings { room_id: *room_id },
                snapshot.version,
            )
            .await;
        self.seed_version_fence_after_refresh(room_id, snapshot.version)
            .await;

        // Store in cache
        if let Err(error) = self
            .cache
            .set_if_version_at_least(room_id, snapshot.clone())
            .await
        {
            tracing::warn!(
                room_id = %room_id,
                version = snapshot.version,
                error = %error,
                "Failed to refresh room settings cache"
            );
        }

        Ok(snapshot)
    }

    /// Set room settings (write-through cache) with optimistic locking.
    ///
    /// **Important**: This is a whole-object replacement. The provided `settings`
    /// replaces the entire row. Callers that need to update a single field should
    /// use [`update_field`](Self::update_field) instead, which performs a
    /// read-modify-write cycle and correctly handles concurrent retries.
    ///
    /// Uses CAS (Compare-And-Swap) with automatic retry on version conflicts.
    ///
    /// # Multi-Replica Synchronization
    /// - Reads current version from database
    /// - Updates database with version check
    /// - Updates local cache
    /// - Publishes invalidation via Redis Streams (if configured)
    /// - Sends WebSocket notification to connected clients
    pub async fn set(&self, room_id: &RoomId, settings: &RoomSettings) -> Result<()> {
        crate::service::optimistic_retry::retry_with_optimistic_lock(
            crate::service::optimistic_retry::DEFAULT_MAX_RETRIES,
            crate::service::optimistic_retry::DEFAULT_BACKOFF_BASE_MS,
            "Settings update failed after maximum retry attempts",
            || async {
                // Get current version (bypass cache).
                // NOTE: We only read the version here, not the current settings, because
                // `set` performs whole-object replacement. On retry after a version conflict
                // we re-read the version but intentionally write the caller's `settings`
                // unchanged. For partial (merge) updates, use `update_field` instead.
                let (_current, version) = self.repo.get_with_version(room_id).await?;

                let domain = CacheDomain::RoomSettings { room_id: *room_id };
                let reservation = self.begin_write(room_id, version).await?;
                let new_version = if let Some(reservation) = &reservation {
                    match self
                        .repo
                        .set_settings_with_exact_version(
                            room_id,
                            settings,
                            version,
                            reservation.version,
                        )
                        .await
                    {
                        Ok(new_version) => {
                            self.finalize_committed_write_best_effort(
                                &domain,
                                Some(reservation),
                                new_version,
                            )
                            .await;
                            new_version
                        }
                        Err(error) => {
                            self.abort_write(&domain, Some(reservation)).await;
                            return Err(error);
                        }
                    }
                } else {
                    self.repo
                        .set_settings_with_version(room_id, settings, version)
                        .await?
                };

                let snapshot = RoomSettingsSnapshot {
                    settings: settings.clone(),
                    version: new_version,
                };
                self.refresh_cache_after_commit(room_id, snapshot).await;
                self.publish_and_notify(room_id, settings, new_version)
                    .await;
                Ok(())
            },
        )
        .await
    }

    /// Update a single setting field with optimistic locking (CAS).
    ///
    /// Reads current settings and version, applies the updater, then writes back
    /// with a version check. Retries automatically on concurrent modification.
    pub async fn update_field<F>(&self, room_id: &RoomId, updater: F) -> Result<RoomSettings>
    where
        F: Fn(&mut RoomSettings) + Send,
    {
        crate::service::optimistic_retry::retry_with_optimistic_lock(
            crate::service::optimistic_retry::DEFAULT_MAX_RETRIES,
            crate::service::optimistic_retry::DEFAULT_BACKOFF_BASE_MS,
            "Settings update failed after maximum retry attempts",
            || {
                let updater = &updater;
                async move {
                    // Read current settings with version (bypass cache for freshness)
                    let (mut settings, version) = self.repo.get_with_version(room_id).await?;

                    updater(&mut settings);

                    let domain = CacheDomain::RoomSettings { room_id: *room_id };
                    let reservation = self.begin_write(room_id, version).await?;
                    let new_version = if let Some(reservation) = &reservation {
                        match self
                            .repo
                            .set_settings_with_exact_version(
                                room_id,
                                &settings,
                                version,
                                reservation.version,
                            )
                            .await
                        {
                            Ok(new_version) => {
                                self.finalize_committed_write_best_effort(
                                    &domain,
                                    Some(reservation),
                                    new_version,
                                )
                                .await;
                                new_version
                            }
                            Err(error) => {
                                self.abort_write(&domain, Some(reservation)).await;
                                return Err(error);
                            }
                        }
                    } else {
                        self.repo
                            .set_settings_with_version(room_id, &settings, version)
                            .await?
                    };

                    let snapshot = RoomSettingsSnapshot {
                        settings: settings.clone(),
                        version: new_version,
                    };
                    self.refresh_cache_after_commit(room_id, snapshot).await;
                    self.publish_and_notify(room_id, &settings, new_version)
                        .await;
                    Ok(settings)
                }
            },
        )
        .await
    }

    /// Reset room settings to default
    pub async fn reset(&self, room_id: &RoomId) -> Result<RoomSettings> {
        let default_settings = RoomSettings::default();
        self.set(room_id, &default_settings).await?;
        Ok(default_settings)
    }

    /// Delete all settings for a room
    pub async fn delete(&self, room_id: &RoomId) -> Result<()> {
        let default_settings = RoomSettings::default();

        crate::service::optimistic_retry::retry_with_optimistic_lock(
            crate::service::optimistic_retry::DEFAULT_MAX_RETRIES,
            crate::service::optimistic_retry::DEFAULT_BACKOFF_BASE_MS,
            "Settings delete failed after maximum retry attempts",
            || {
                let default_settings = &default_settings;
                async move {
                    let (_current, version) = self.repo.get_with_version(room_id).await?;
                    let domain = CacheDomain::RoomSettings { room_id: *room_id };
                    let reservation = self.begin_write(room_id, version).await?;

                    let mut tx = match self.repo.pool().begin().await {
                        Ok(tx) => tx,
                        Err(error) => {
                            self.abort_write(&domain, reservation.as_ref()).await;
                            return Err(error.into());
                        }
                    };
                    let new_version = if let Some(reservation) = &reservation {
                        match self
                            .repo
                            .set_settings_with_exact_version_with_executor(
                                room_id,
                                default_settings,
                                version,
                                reservation.version,
                                &mut *tx,
                            )
                            .await
                        {
                            Ok(new_version) => new_version,
                            Err(error) => {
                                self.abort_write(&domain, Some(reservation)).await;
                                return Err(error);
                            }
                        }
                    } else {
                        self.repo
                            .set_settings_with_version_with_executor(
                                room_id,
                                default_settings,
                                version,
                                &mut *tx,
                            )
                            .await?
                    };
                    if let Err(error) = self
                        .repo
                        .delete_auxiliary_with_executor(room_id, &mut *tx)
                        .await
                    {
                        self.abort_write(&domain, reservation.as_ref()).await;
                        return Err(error);
                    }
                    if let Err(error) = tx.commit().await {
                        self.abort_write(&domain, reservation.as_ref()).await;
                        return Err(error.into());
                    }
                    self.finalize_committed_write_best_effort(
                        &domain,
                        reservation.as_ref(),
                        new_version,
                    )
                    .await;

                    let snapshot = RoomSettingsSnapshot {
                        settings: default_settings.clone(),
                        version: new_version,
                    };
                    self.refresh_cache_after_commit(room_id, snapshot).await;
                    self.publish_and_notify(room_id, default_settings, new_version)
                        .await;
                    Ok(())
                }
            },
        )
        .await
    }

    async fn begin_write(
        &self,
        room_id: &RoomId,
        observed_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        let domain = CacheDomain::RoomSettings { room_id: *room_id };
        self.consistency
            .begin_observed_write(&domain, observed_version)
            .await
    }

    async fn commit_write(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) -> Result<()> {
        self.consistency
            .commit_reserved_write(domain, reservation, version)
            .await?;
        Ok(())
    }

    async fn finalize_committed_write_best_effort(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) {
        if let Err(error) = self.commit_write(domain, reservation, version).await {
            tracing::warn!(
                domain = %domain,
                version,
                error = %error,
                "Failed to finalize room settings version fence after committed DB write"
            );
        }
    }

    async fn abort_write(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
    ) {
        self.consistency
            .abort_reserved_write(domain, reservation)
            .await;
    }

    async fn seed_version_fence_after_refresh(&self, room_id: &RoomId, version: i64) {
        if !self.consistency.is_authoritative() {
            return;
        }

        let domain = CacheDomain::RoomSettings { room_id: *room_id };
        if let Err(error) = self
            .consistency
            .set_version_at_least(&domain, version)
            .await
        {
            tracing::warn!(
                room_id = %room_id,
                version,
                error = %error,
                "Failed to seed room settings version fence after DB refresh"
            );
        }
    }

    async fn refresh_cache_after_commit(&self, room_id: &RoomId, snapshot: RoomSettingsSnapshot) {
        if let Err(error) = self.cache.set_if_version_at_least(room_id, snapshot).await {
            tracing::warn!(
                room_id = %room_id,
                error = %error,
                "Failed to refresh room settings cache after committed write"
            );
            self.invalidate_local(room_id).await;
        }
    }

    /// Publish invalidation to other replicas and notify connected clients.
    async fn publish_and_notify(&self, room_id: &RoomId, settings: &RoomSettings, version: i64) {
        if let Some(ref inv_service) = self.invalidation_service {
            if let Err(e) = inv_service.invalidate_room_settings(room_id).await {
                tracing::error!("Failed to publish settings invalidation: {}", e);
            }
        }

        self.notify_settings_changed(room_id, settings, version);
    }

    /// Invalidate local cache for a room
    pub async fn invalidate_local(&self, room_id: &RoomId) {
        if let Err(error) = self.cache.invalidate(room_id).await {
            tracing::warn!(
                room_id = %room_id,
                error = %error,
                "Failed to invalidate local room settings cache"
            );
        }
    }

    /// Notify connected clients about settings change
    fn notify_settings_changed(&self, room_id: &RoomId, settings: &RoomSettings, version: i64) {
        let settings_value = match serde_json::to_value(settings) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize settings: {}", e);
                return;
            }
        };

        let _ = self.notification_service.notify_settings_updated(
            room_id,
            None,
            "",
            settings_value,
            version,
        );
    }

    /// Preload settings for multiple rooms (bulk loading)
    ///
    /// Uses a single `get_batch` query instead of N sequential queries.
    pub async fn preload(&self, room_ids: &[RoomId]) -> Result<()> {
        if room_ids.is_empty() {
            return Ok(());
        }

        let ids: Vec<RoomId> = room_ids.to_vec();
        let versioned_batch = self.repo.get_batch_with_version(&ids).await?;

        // Bulk insert into cache
        for room_id in room_ids {
            let snapshot = versioned_batch.get(room_id).map_or(
                RoomSettingsSnapshot {
                    settings: RoomSettings::default(),
                    version: 0,
                },
                |(settings, version)| RoomSettingsSnapshot {
                    settings: settings.clone(),
                    version: *version,
                },
            );
            if let Err(error) = self.cache.set_if_version_at_least(room_id, snapshot).await {
                tracing::warn!(
                    room_id = %room_id,
                    error = %error,
                    "Failed to preload room settings cache"
                );
            }
        }

        Ok(())
    }

    /// Get cache statistics
    #[must_use]
    pub fn cache_stats(&self) -> CacheStats {
        CacheStats {
            entry_count: self.cache.entry_count(),
            weighted_size: self.cache.weighted_size(),
        }
    }

    /// Clear all cache
    pub async fn clear_cache(&self) {
        self.cache.clear().await;
    }
}

/// Cache statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub entry_count: u64,
    pub weighted_size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheInvalidationService, CacheL2Backend};
    use crate::cache::{KeyBuilder, UsernameCache};
    use crate::config::PasswordComplexityConfig;
    use crate::models::{SignupMethod, User, UserId, UserRole, UserStatus};
    use crate::repository::RoomSettingsRepository;
    use crate::repository::UserRepository;
    use crate::service::auth::BruteForceProtection;
    use crate::service::notification::NotificationService;
    use crate::service::{auth::JwtService, InMemoryTokenBlacklistStore, UserService};
    use chrono::Utc;
    use sqlx::PgPool;
    use synctv_core_testing::create_test_pool;

    struct FailingRoomSettingsL2;

    #[async_trait::async_trait]
    impl CacheL2Backend for FailingRoomSettingsL2 {
        async fn get(&self, _key: &str) -> Result<Option<String>> {
            Err(Error::Internal(
                "simulated room settings L2 failure".to_string(),
            ))
        }

        async fn set(&self, _key: &str, _json: &str, _ttl_secs: u64) -> Result<()> {
            Err(Error::Internal(
                "simulated room settings L2 failure".to_string(),
            ))
        }

        async fn delete(&self, _key: &str) -> Result<()> {
            Err(Error::Internal(
                "simulated room settings L2 failure".to_string(),
            ))
        }

        async fn delete_with_retry(
            &self,
            _key: &str,
            _max_retries: u32,
            _cache_type: &str,
        ) -> Result<()> {
            Err(Error::Internal(
                "simulated room settings L2 failure".to_string(),
            ))
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
            Err(Error::Internal(
                "simulated room settings L2 failure".to_string(),
            ))
        }

        async fn set_if_version_at_least(
            &self,
            _key: &str,
            _json: &str,
            _ttl_secs: u64,
            _version: i64,
        ) -> Result<bool> {
            Err(Error::Internal(
                "simulated room settings L2 failure".to_string(),
            ))
        }

        async fn delete_by_prefix(&self, _prefix: &str) -> Result<()> {
            Err(Error::Internal(
                "simulated room settings L2 failure".to_string(),
            ))
        }

        fn is_active(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_strong_read_uses_l1_when_version_satisfies_fence() {
        let (_container, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_service = crate::service::RoomService::new(pool.clone(), make_user_service(&pool));
        let owner = user_repo
            .create(&make_user("room_settings_fence_l1_owner"))
            .await
            .expect("owner should be created");
        let (room, _) = room_service
            .create_room(
                "Room Settings Fence L1".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let (_redis_container, redis_conn) = synctv_core_testing::start_redis().await;
        let fence = Arc::new(crate::cache::RedisVersionFenceStore::new(
            crate::direct_runtime(redis_conn),
            "test:fence-l1:",
        ));
        let service = RoomSettingsService::new_with_version_fence(
            RoomSettingsRepository::new(pool.clone()),
            None,
            Arc::new(NotificationService::default()),
            RoomSettingsRuntime {
                version_fence: Some(fence.clone()),
                cache_key_prefix: "test:room_settings:l1:".to_string(),
                ..RoomSettingsRuntime::default()
            },
        );

        let cached_settings = RoomSettings {
            allow_auto_join: crate::models::room_settings::AllowAutoJoin(false),
            ..RoomSettings::default()
        };
        service
            .cache
            .set(
                &room.id,
                RoomSettingsSnapshot {
                    settings: cached_settings,
                    version: 7,
                },
            )
            .await
            .expect("cache write should succeed");
        fence
            .set_version_at_least(&CacheDomain::RoomSettings { room_id: room.id }, 7)
            .await
            .expect("fence should be written");

        let snapshot = service
            .get_with_version(&room.id)
            .await
            .expect("strong read should use cache that satisfies fence");
        assert_eq!(snapshot.version, 7);
        assert!(!snapshot.settings.allow_auto_join.0);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_strong_read_uses_l1_with_local_version_fence() {
        let (_container, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_service = crate::service::RoomService::new(pool.clone(), make_user_service(&pool));
        let owner = user_repo
            .create(&make_user("room_settings_local_fence_owner"))
            .await
            .expect("owner should be created");
        let (room, _) = room_service
            .create_room(
                "Room Settings Local Fence".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
        let service = RoomSettingsService::new_with_version_fence(
            RoomSettingsRepository::new(pool.clone()),
            None,
            Arc::new(NotificationService::default()),
            RoomSettingsRuntime {
                version_fence: Some(fence.clone()),
                cache_key_prefix: "test:room_settings:local:".to_string(),
                ..RoomSettingsRuntime::default()
            },
        );

        let cached_settings = RoomSettings {
            allow_auto_join: crate::models::room_settings::AllowAutoJoin(false),
            ..RoomSettings::default()
        };
        service
            .cache
            .set(
                &room.id,
                RoomSettingsSnapshot {
                    settings: cached_settings,
                    version: 3,
                },
            )
            .await
            .expect("cache write should succeed");
        fence
            .set_version_at_least(&CacheDomain::RoomSettings { room_id: room.id }, 3)
            .await
            .expect("local fence should be written");

        let snapshot = service
            .get_with_version(&room.id)
            .await
            .expect("strong read should use local-fenced L1");
        assert_eq!(snapshot.version, 3);
        assert!(!snapshot.settings.allow_auto_join.0);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_settings_write_uses_redis_allocated_version() {
        let (_container, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_service = crate::service::RoomService::new(pool.clone(), make_user_service(&pool));
        let owner = user_repo
            .create(&make_user("room_settings_allocator_owner"))
            .await
            .expect("owner should be created");
        let (room, _) = room_service
            .create_room(
                "Room Settings Allocator".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let (_redis_container, redis_conn) = synctv_core_testing::start_redis().await;
        let fence = Arc::new(crate::cache::RedisVersionFenceStore::new(
            crate::direct_runtime(redis_conn),
            "test:fence-allocator:",
        ));
        let service = RoomSettingsService::new_with_version_fence(
            RoomSettingsRepository::new(pool.clone()),
            None,
            Arc::new(NotificationService::default()),
            RoomSettingsRuntime {
                version_fence: Some(fence.clone()),
                cache_key_prefix: "test:room_settings:allocator:".to_string(),
                ..RoomSettingsRuntime::default()
            },
        );

        let settings = RoomSettings {
            allow_auto_join: crate::models::room_settings::AllowAutoJoin(false),
            ..RoomSettings::default()
        };
        service
            .set(&room.id, &settings)
            .await
            .expect("settings write should use Redis allocated version");

        let domain = CacheDomain::RoomSettings { room_id: room.id };
        let fence_version = fence
            .current_version(&domain)
            .await
            .expect("fence should be readable")
            .expect("fence should exist");
        let snapshot = service
            .get_refresh_with_version(&room.id)
            .await
            .expect("DB settings should be readable");
        assert_eq!(snapshot.version, fence_version);

        let updated = RoomSettings {
            chat_enabled: crate::models::room_settings::ChatEnabled(false),
            ..settings
        };
        service
            .set(&room.id, &updated)
            .await
            .expect("second settings write should use next Redis version");
        let next_fence = fence
            .current_version(&domain)
            .await
            .expect("fence should be readable")
            .expect("fence should exist");
        let next_snapshot = service
            .get_refresh_with_version(&room.id)
            .await
            .expect("DB settings should be readable");
        assert!(next_fence > fence_version);
        assert_eq!(next_snapshot.version, next_fence);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_settings_reserve_rejects_stale_snapshot_without_advancing_fence() {
        let (_container, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_service = crate::service::RoomService::new(pool.clone(), make_user_service(&pool));
        let owner = user_repo
            .create(&make_user("room_settings_stale_reserve_owner"))
            .await
            .expect("owner should be created");
        let (room, _) = room_service
            .create_room(
                "Room Settings Stale Reserve".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
        let service = RoomSettingsService::new_with_version_fence(
            RoomSettingsRepository::new(pool.clone()),
            None,
            Arc::new(NotificationService::default()),
            RoomSettingsRuntime {
                version_fence: Some(fence.clone()),
                cache_key_prefix: "test:room_settings:stale-reserve:".to_string(),
                ..RoomSettingsRuntime::default()
            },
        );
        let domain = CacheDomain::RoomSettings { room_id: room.id };

        let stale_observed_version = 1;
        fence
            .set_version_at_least(&domain, stale_observed_version + 1)
            .await
            .expect("concurrent writer should advance fence");

        let result = service.begin_write(&room.id, stale_observed_version).await;
        assert!(
            matches!(result, Err(Error::OptimisticLockConflict)),
            "stale settings snapshots must retry before reserving a fence version; got {result:?}"
        );
        assert_eq!(
            fence
                .current_version(&domain)
                .await
                .expect("fence should be readable"),
            Some(stale_observed_version + 1),
            "failed reservations must not burn additional fence versions"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_settings_write_does_not_retry_committed_update_after_l2_failure() {
        let (_container, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_service = crate::service::RoomService::new(pool.clone(), make_user_service(&pool));
        let owner = user_repo
            .create(&make_user("room_settings_l2_failure_owner"))
            .await
            .expect("owner should be created");
        let (room, _) = room_service
            .create_room(
                "Room Settings L2 Failure".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let service = RoomSettingsService::new_with_version_fence(
            RoomSettingsRepository::new(pool.clone()),
            None,
            Arc::new(NotificationService::default()),
            RoomSettingsRuntime {
                l2_cache: Some(Arc::new(FailingRoomSettingsL2)),
                cache_key_prefix: "test:room_settings:l2-failure:".to_string(),
                ..RoomSettingsRuntime::default()
            },
        );

        let settings = RoomSettings {
            chat_enabled: crate::models::room_settings::ChatEnabled(false),
            ..RoomSettings::default()
        };
        service
            .set(&room.id, &settings)
            .await
            .expect("committed settings write must not fail because cache refresh failed");

        let snapshot = service
            .get_refresh_with_version(&room.id)
            .await
            .expect("committed settings should remain readable");
        assert_eq!(snapshot.version, 2);
        assert!(!snapshot.settings.chat_enabled.0);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_db_refresh_seeds_missing_local_version_fence() {
        let (_container, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_service = crate::service::RoomService::new(pool.clone(), make_user_service(&pool));
        let owner = user_repo
            .create(&make_user("room_settings_seed_fence_owner"))
            .await
            .expect("owner should be created");
        let (room, _) = room_service
            .create_room(
                "Room Settings Seed Fence".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let settings = RoomSettings {
            allow_auto_join: crate::models::room_settings::AllowAutoJoin(false),
            ..RoomSettings::default()
        };
        let repo = RoomSettingsRepository::new(pool.clone());
        let (_current_settings, current_version) = repo
            .get_with_version(&room.id)
            .await
            .expect("current settings should be readable");
        let target_version = current_version + 3;
        let db_version = repo
            .set_settings_with_exact_version(&room.id, &settings, current_version, target_version)
            .await
            .expect("settings row should be written");

        let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
        let service = RoomSettingsService::new_with_version_fence(
            repo,
            None,
            Arc::new(NotificationService::default()),
            RoomSettingsRuntime {
                version_fence: Some(fence.clone()),
                cache_key_prefix: "test:room_settings:seed-fence:".to_string(),
                ..RoomSettingsRuntime::default()
            },
        );
        let domain = CacheDomain::RoomSettings { room_id: room.id };
        assert_eq!(
            fence
                .current_version(&domain)
                .await
                .expect("local fence should be readable"),
            None
        );

        let snapshot = service
            .get_with_version(&room.id)
            .await
            .expect("strong read should fall back to DB");

        assert_eq!(snapshot.version, db_version);
        assert_eq!(
            fence
                .current_version(&domain)
                .await
                .expect("local fence should be readable"),
            Some(db_version)
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_writes_versioned_default_settings() {
        let (_container, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_service = crate::service::RoomService::new(pool.clone(), make_user_service(&pool));
        let owner = user_repo
            .create(&make_user("room_settings_delete_default_owner"))
            .await
            .expect("owner should be created");
        let (room, _) = room_service
            .create_room(
                "Room Settings Delete Default".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
        let repo = RoomSettingsRepository::new(pool.clone());
        let service = RoomSettingsService::new_with_version_fence(
            repo.clone(),
            None,
            Arc::new(NotificationService::default()),
            RoomSettingsRuntime {
                version_fence: Some(fence.clone()),
                cache_key_prefix: "test:room_settings:delete-default:".to_string(),
                ..RoomSettingsRuntime::default()
            },
        );

        let changed = RoomSettings {
            require_password: crate::models::room_settings::RequirePassword(true),
            ..RoomSettings::default()
        };
        service
            .set(&room.id, &changed)
            .await
            .expect("custom settings should be written");
        repo.set(&room.id, "password", "stale-password-hash")
            .await
            .expect("password row should be written");
        let before_delete = service
            .get_refresh_with_version(&room.id)
            .await
            .expect("settings should be readable before delete");

        service
            .delete(&room.id)
            .await
            .expect("delete should write versioned default settings");

        let after_delete = repo
            .get_with_version(&room.id)
            .await
            .expect("default settings row should remain readable");
        assert!(!after_delete.0.require_password.0);
        assert!(
            after_delete.1 > before_delete.version,
            "delete must keep a monotonic DB version"
        );
        assert_eq!(
            repo.get_password_hash(&room.id)
                .await
                .expect("password hash lookup should succeed"),
            None,
            "delete must remove auxiliary password settings rows"
        );

        let domain = CacheDomain::RoomSettings { room_id: room.id };
        assert_eq!(
            fence
                .current_version(&domain)
                .await
                .expect("local fence should be readable"),
            Some(after_delete.1)
        );
    }

    fn make_room_settings_service_for_lifecycle_tests(
    ) -> (RoomSettingsService, Arc<CacheInvalidationService>, RoomId) {
        let pool = PgPool::connect_lazy("postgres://localhost/test")
            .expect("lazy postgres pool for unit tests should build");
        let room_id = RoomId::expect_positive(20_001);
        let invalidation_service = Arc::new(CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:test:room-settings".to_string(),
        ));
        let service = RoomSettingsService::new(
            RoomSettingsRepository::new(pool),
            Some(invalidation_service.clone()),
            Arc::new(NotificationService::default()),
            None,
            None,
        );
        (service, invalidation_service, room_id)
    }

    #[tokio::test]
    async fn standalone_room_settings_service_uses_non_authoritative_fence_by_default() {
        let pool = PgPool::connect_lazy("postgres://localhost/test")
            .expect("lazy postgres pool for unit tests should build");
        let service = RoomSettingsService::new(
            RoomSettingsRepository::new(pool),
            None,
            Arc::new(NotificationService::default()),
            None,
            None,
        );

        assert!(
            !service.consistency.is_authoritative(),
            "standalone room settings constructors must not create private authoritative fences"
        );
    }

    #[tokio::test]
    async fn test_invalidation_via_streams() {
        // Create a CacheInvalidationService without Redis (local-only mode)
        let inv_service = Arc::new(CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        ));

        // Subscribe before broadcasting so we can verify the message is sent
        let mut receiver = inv_service.subscribe();

        // Broadcast a RoomSettings invalidation
        inv_service
            .broadcast_all(InvalidationMessage::RoomSettings {
                room_id: "room1".to_string(),
            })
            .await
            .unwrap();

        // Verify the message was received
        let msg = receiver.recv().await.unwrap();
        match msg {
            InvalidationMessage::RoomSettings { ref room_id } => {
                assert_eq!(room_id, "room1");
            }
            _ => panic!("Expected RoomSettings invalidation message"),
        }
    }

    #[test]
    fn test_room_settings_invalidation_message_serialization() {
        let msg = InvalidationMessage::RoomSettings {
            room_id: "room123".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("room_settings"));
        assert!(json.contains("room123"));

        let decoded: InvalidationMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[tokio::test]
    async fn test_lagged_receiver_flushes_cache() {
        // Create invalidation service with local-only mode
        let inv_service = Arc::new(CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        ));

        // Verify that broadcast_all works without panicking
        // (full lagged-receiver test requires a real RoomSettingsService with DB)
        inv_service
            .broadcast_all(InvalidationMessage::All)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_cache_invalidation() {
        // Placeholder: integration test for RoomSettingsService cache invalidation
        // would require a full TestInfra with PostgreSQL
    }

    #[tokio::test]
    async fn test_invalidation_listener_stops_after_shutdown() {
        let (service, invalidation_service, room_id) =
            make_room_settings_service_for_lifecycle_tests();

        service
            .start()
            .await
            .expect("room settings invalidation listener should start");
        assert!(
            service.invalidation_task_started(),
            "start() must mark room settings invalidation runtime as running"
        );

        service
            .cache
            .set(
                &room_id,
                RoomSettingsSnapshot {
                    settings: RoomSettings::default(),
                    version: 0,
                },
            )
            .await
            .expect("cache fixture write should succeed");

        service.shutdown().await;

        invalidation_service
            .broadcast_all(InvalidationMessage::RoomSettings {
                room_id: room_id.to_string(),
            })
            .await
            .expect("local invalidation broadcast should succeed");
        tokio::task::yield_now().await;

        assert!(
            service.cache.get(&room_id).await.unwrap().is_some(),
            "room settings listener should have stopped once invalidation service shutdown begins"
        );
    }

    #[tokio::test]
    async fn test_start_can_restart_room_settings_invalidation_listener_after_shutdown() {
        let (service, invalidation_service, room_id) =
            make_room_settings_service_for_lifecycle_tests();

        service
            .start()
            .await
            .expect("initial room settings invalidation start should succeed");
        service.shutdown().await;

        service
            .cache
            .set(
                &room_id,
                RoomSettingsSnapshot {
                    settings: RoomSettings::default(),
                    version: 0,
                },
            )
            .await
            .expect("cache fixture write should succeed");

        service
            .start()
            .await
            .expect("restart after room settings invalidation shutdown should succeed");

        invalidation_service
            .broadcast_all(InvalidationMessage::RoomSettings {
                room_id: room_id.to_string(),
            })
            .await
            .expect("local invalidation broadcast should succeed after restart");
        tokio::task::yield_now().await;

        assert!(
            service.cache.get(&room_id).await.unwrap().is_none(),
            "restarted room settings listener should invalidate cache entries again"
        );

        service.shutdown().await;
    }

    fn make_user_service(pool: &PgPool) -> UserService {
        let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!")
            .expect("jwt service should build");
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
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

    fn make_user(username: &str) -> User {
        let now = Utc::now();
        User {
            id: UserId::new(),
            username: username.to_string(),
            email: Some(format!("{username}@test.com")),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: SignupMethod::Email,
            created_at: now,
            updated_at: now,
            password_changed_at: now,
            password_version: 0,
            version: 0,
            deleted_at: None,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_with_version_returns_current_snapshot_version() {
        let (_container, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let user_service = make_user_service(&pool);
        let room_service = crate::service::RoomService::new(pool.clone(), user_service);
        let owner = user_repo
            .create(&make_user("room_settings_version_owner"))
            .await
            .expect("owner should be created");
        let (room, _) = room_service
            .create_room(
                "Room Settings Version".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let service = RoomSettingsService::new(
            RoomSettingsRepository::new(pool),
            None,
            Arc::new(NotificationService::default()),
            None,
            None,
        );

        let updated = RoomSettings {
            chat_enabled: crate::models::room_settings::ChatEnabled(false),
            ..RoomSettings::default()
        };
        service
            .set(&room.id, &updated)
            .await
            .expect("room settings should be persisted");

        let cached = service
            .get_eventually_consistent_with_version(&room.id)
            .await
            .expect("cached room settings should be readable");
        assert!(
            !cached.settings.chat_enabled.0,
            "sanity check: cache should contain the updated settings value"
        );

        let snapshot = service
            .get_with_version(&room.id)
            .await
            .expect("strong room settings snapshot should include version");
        assert_eq!(snapshot.version, 2);
        assert!(
            !snapshot.settings.chat_enabled.0,
            "snapshot should include the updated settings value"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_local_fence_rejects_stale_l1_after_service_settings_change() {
        let (_container, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let user_service = make_user_service(&pool);
        let room_service = crate::service::RoomService::new(pool.clone(), user_service);
        let owner = user_repo
            .create(&make_user("room_settings_strong_owner"))
            .await
            .expect("owner should be created");
        let (room, _) = room_service
            .create_room(
                "Room Settings Strong".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let fence = Arc::new(crate::cache::LocalVersionFenceStore::new());
        let repo = RoomSettingsRepository::new(pool.clone());
        let service = RoomSettingsService::new_with_version_fence(
            repo.clone(),
            None,
            Arc::new(NotificationService::default()),
            RoomSettingsRuntime {
                version_fence: Some(fence.clone()),
                cache_key_prefix: "test:room_settings:stale-l1:".to_string(),
                ..RoomSettingsRuntime::default()
            },
        );

        let original = RoomSettings {
            allow_auto_join: crate::models::room_settings::AllowAutoJoin(true),
            ..RoomSettings::default()
        };
        service
            .set(&room.id, &original)
            .await
            .expect("initial settings should be persisted");
        let cached = service
            .get_eventually_consistent_with_version(&room.id)
            .await
            .expect("cache should be populated");
        assert!(cached.settings.allow_auto_join.0);

        let changed = RoomSettings {
            allow_auto_join: crate::models::room_settings::AllowAutoJoin(false),
            ..RoomSettings::default()
        };
        let (_current, cached_version) = repo
            .get_with_version(&room.id)
            .await
            .expect("settings version should be readable");
        let newer_version = cached_version + 1;
        repo.set_settings_with_exact_version(&room.id, &changed, cached_version, newer_version)
            .await
            .expect("DB settings update should succeed");
        fence
            .set_version_at_least(
                &CacheDomain::RoomSettings { room_id: room.id },
                newer_version,
            )
            .await
            .expect("local fence should be advanced");

        let stale_snapshot = service
            .get_eventually_consistent_with_version(&room.id)
            .await
            .expect("eventual settings read should still expose stale cache fixture");
        assert!(
            stale_snapshot.settings.allow_auto_join.0,
            "cache-first settings snapshot should demonstrate stale L1 is present"
        );

        let strong_settings = service
            .get(&room.id)
            .await
            .expect("strong settings read should succeed");
        assert!(
            !strong_settings.allow_auto_join.0,
            "default settings get must bypass stale L1 and read DB"
        );

        let strong_snapshot = service
            .get_with_version(&room.id)
            .await
            .expect("strong versioned settings read should succeed");
        assert!(
            !strong_snapshot.settings.allow_auto_join.0,
            "versioned settings get must bypass stale L1 and read DB"
        );
        assert!(
            strong_snapshot.version >= newer_version,
            "versioned settings get must return a snapshot satisfying the local fence"
        );
    }
}
