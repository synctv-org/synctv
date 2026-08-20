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

use std::sync::Arc;

use crate::{
    cache::{
        CacheDomain, CacheInvalidationRuntime, CloneableError, ConsistencyCoordinator,
        FenceReadResult, NoopCacheL2, RoomSettingsCache, RoomSettingsSnapshot, SingleFlight,
        VersionFenceReservation, VersionFenceStore,
    },
    models::{RoomId, RoomSettings},
    repository::RoomSettingsRepository,
    service::notification::NotificationService,
    Error, Result,
};

mod invalidation;

use invalidation::RoomSettingsInvalidationRuntime;

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
    pub version_fence: Arc<dyn VersionFenceStore>,
    pub l2_cache: Arc<dyn crate::cache::CacheL2Backend>,
    pub cache_key_prefix: String,
}

impl RoomSettingsRuntime {
    #[must_use]
    pub fn local_only() -> Self {
        Self {
            cache_ttl_secs: None,
            cache_max_capacity: None,
            version_fence: Arc::new(crate::cache::LocalVersionFenceStore::new()),
            l2_cache: Arc::new(NoopCacheL2),
            cache_key_prefix: String::from("room_settings:"),
        }
    }
}

fn normalize_cache_capacity(capacity: u64) -> u64 {
    capacity.max(1)
}

fn normalize_cache_ttl(ttl_seconds: u64) -> u64 {
    ttl_seconds.max(1)
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
                ..RoomSettingsRuntime::local_only()
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
        let ttl = normalize_cache_ttl(runtime.cache_ttl_secs.unwrap_or(Self::CACHE_TTL_SECS));
        let capacity = runtime
            .cache_max_capacity
            .unwrap_or(Self::CACHE_MAX_CAPACITY);
        let cache = RoomSettingsCache::new(
            runtime.l2_cache,
            normalize_cache_capacity(capacity),
            ttl,
            ttl,
            runtime.cache_key_prefix,
        );

        Self {
            repo,
            cache,
            invalidation_service,
            version_fence: runtime.version_fence.clone(),
            consistency: ConsistencyCoordinator::new(runtime.version_fence),
            invalidation_runtime: Arc::new(RoomSettingsInvalidationRuntime::new()),
            notification_service,
            single_flight: SingleFlight::new(),
        }
    }

    pub const fn has_invalidation_service(&self) -> bool {
        self.invalidation_service.is_some()
    }

    /// Get room settings with strong-read semantics.
    ///
    /// Settings affect room access, passwords, join policy, and role defaults,
    /// so default service reads must not trust an L1 entry that might be
    /// waiting for async invalidation. Read the database and refresh L1.
    pub async fn get(&self, room_id: &RoomId) -> Result<RoomSettings> {
        Ok(self.get_with_version(room_id).await?.settings)
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
        let subscriber_count = self.notification_service.notify_settings_updated(
            room_id,
            None,
            "",
            settings.clone(),
            version,
        );
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                version,
                "Room settings updated event had no local subscribers"
            );
        }
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

    /// Clear all cache
    pub async fn clear_cache(&self) {
        self.cache.clear().await;
    }
}

#[cfg(test)]
mod tests;
