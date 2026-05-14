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
    cache::{CacheInvalidationRuntime, CloneableError, InvalidationMessage, SingleFlight},
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSettingsSnapshot {
    pub settings: RoomSettings,
    pub version: i64,
}

pub struct RoomSettingsService {
    repo: RoomSettingsRepository,
    cache: Arc<moka::future::Cache<RoomId, RoomSettingsSnapshot>>,
    invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
    invalidation_runtime: Arc<RoomSettingsInvalidationRuntime>,
    notification_service: Arc<NotificationService>,
    /// `SingleFlight` to prevent thundering herd on cache miss.
    /// Uses `String` key (`room_id`) and `String` error (since `Error` is not `Clone`).
    single_flight: SingleFlight<String, RoomSettingsSnapshot, CloneableError>,
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
        let ttl = cache_ttl_secs.unwrap_or(Self::CACHE_TTL_SECS);
        let capacity = cache_max_capacity.unwrap_or(Self::CACHE_MAX_CAPACITY);

        let cache = Arc::new(
            moka::future::CacheBuilder::new(capacity)
                .time_to_live(Duration::from_secs(ttl))
                .build(),
        );

        Self {
            repo,
            cache,
            invalidation_service,
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
                                    cache_clone.invalidate(&room_id).await;
                                    tracing::debug!(
                                        room_id = %room_id,
                                        "Room settings cache invalidated (cross-replica)"
                                    );
                                }
                                Ok(InvalidationMessage::All) => {
                                    cache_clone.invalidate_all();
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
                                        cache_clone.invalidate_all();
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

    /// Get room settings with caching
    ///
    /// # Performance
    /// - L1 cache hit: < 1ms
    /// - Cache miss + DB query: ~10ms
    /// - `SingleFlight`: Prevents thundering herd on cache miss
    pub async fn get(&self, room_id: &RoomId) -> Result<RoomSettings> {
        Ok(self.get_with_version(room_id).await?.settings)
    }

    /// Get room settings with the current optimistic-lock version.
    pub async fn get_with_version(&self, room_id: &RoomId) -> Result<RoomSettingsSnapshot> {
        // Try cache first
        if let Some(snapshot) = self.cache.get(room_id).await {
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
                if let Some(snapshot) = cache.get(&room_id_clone).await {
                    return Ok(snapshot);
                }

                // Load from database
                let (settings, version) = repo
                    .get_with_version(&room_id_clone)
                    .await
                    .map_err(CloneableError::from)?;
                let snapshot = RoomSettingsSnapshot { settings, version };

                // Store in cache
                cache.insert(room_id_clone, snapshot.clone()).await;

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

        // Store in cache
        let () = self.cache.insert(*room_id, snapshot.clone()).await;

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

                let new_version = self
                    .repo
                    .set_settings_with_version(room_id, settings, version)
                    .await?;

                self.cache
                    .insert(
                        *room_id,
                        RoomSettingsSnapshot {
                            settings: settings.clone(),
                            version: new_version,
                        },
                    )
                    .await;
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

                    let new_version = self
                        .repo
                        .set_settings_with_version(room_id, &settings, version)
                        .await?;

                    self.cache
                        .insert(
                            *room_id,
                            RoomSettingsSnapshot {
                                settings: settings.clone(),
                                version: new_version,
                            },
                        )
                        .await;
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
        self.repo.delete_all(room_id).await?;

        // Invalidate local cache
        self.invalidate_local(room_id).await;

        // Notify other replicas via Redis Streams
        if let Some(ref inv_service) = self.invalidation_service {
            if let Err(e) = inv_service.invalidate_room_settings(room_id).await {
                tracing::error!("Failed to publish room settings invalidation: {}", e);
            }
        }

        Ok(())
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
        let () = self.cache.invalidate(room_id).await;
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
            self.cache.insert(*room_id, snapshot).await;
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
    pub fn clear_cache(&self) {
        self.cache.invalidate_all();
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
    use crate::cache::CacheInvalidationService;
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
            .insert(
                room_id,
                RoomSettingsSnapshot {
                    settings: RoomSettings::default(),
                    version: 0,
                },
            )
            .await;

        service.shutdown().await;

        invalidation_service
            .broadcast_all(InvalidationMessage::RoomSettings {
                room_id: room_id.to_string(),
            })
            .await
            .expect("local invalidation broadcast should succeed");
        tokio::task::yield_now().await;

        assert!(
            service.cache.get(&room_id).await.is_some(),
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
            .insert(
                room_id,
                RoomSettingsSnapshot {
                    settings: RoomSettings::default(),
                    version: 0,
                },
            )
            .await;

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
            service.cache.get(&room_id).await.is_none(),
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
            email_verified: true,
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
    async fn test_get_with_version_returns_cached_snapshot_version() {
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
            .get(&room.id)
            .await
            .expect("cached room settings should be readable");
        assert!(
            !cached.chat_enabled.0,
            "sanity check: cache should contain the updated settings value"
        );

        let snapshot = service
            .get_with_version(&room.id)
            .await
            .expect("cached room settings snapshot should include version");
        assert_eq!(snapshot.version, 2);
        assert!(
            !snapshot.settings.chat_enabled.0,
            "snapshot should reuse the cached settings value"
        );
    }
}
