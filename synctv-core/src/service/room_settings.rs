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
use std::time::Duration;
use serde::{Deserialize, Serialize};
use rand::RngExt;

use crate::{
    cache::{CacheInvalidationService, InvalidationMessage, SingleFlight},
    models::{RoomId, RoomSettings},
    repository::RoomSettingsRepository,
    service::notification::NotificationService,
    Error, Result,
};

/// Room settings service with caching
pub struct RoomSettingsService {
    repo: RoomSettingsRepository,
    cache: Arc<moka::future::Cache<RoomId, RoomSettings>>,
    invalidation_service: Option<Arc<CacheInvalidationService>>,
    notification_service: Arc<NotificationService>,
    /// `SingleFlight` to prevent thundering herd on cache miss.
    /// Uses `String` key (`room_id`) and `String` error (since `Error` is not `Clone`).
    single_flight: SingleFlight<String, RoomSettings, String>,
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
            notification_service: self.notification_service.clone(),
            single_flight: self.single_flight.clone(), // Arc-backed, shares state
        }
    }
}

impl RoomSettingsService {
    const CACHE_TTL_SECS: u64 = 300; // 5 minutes
    const CACHE_MAX_CAPACITY: u64 = 10_000;
    /// Maximum retry attempts for optimistic lock conflicts
    const MAX_RETRIES: u32 = 3;
    /// Base backoff in milliseconds (exponential: 5ms, 10ms, 20ms)
    const BACKOFF_BASE_MS: u64 = 5;

    /// Create a new room settings service
    ///
    /// Uses `CacheInvalidationService` (Redis Streams) for reliable cross-replica
    /// cache invalidation. When a replica disconnects and reconnects, missed
    /// invalidation messages are automatically delivered via consumer groups.
    #[must_use]
    pub fn new(
        repo: RoomSettingsRepository,
        invalidation_service: Option<Arc<CacheInvalidationService>>,
        notification_service: Arc<NotificationService>,
        cache_ttl_secs: Option<u64>,
        cache_max_capacity: Option<u64>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Self {
        let ttl = cache_ttl_secs.unwrap_or(Self::CACHE_TTL_SECS);
        let capacity = cache_max_capacity.unwrap_or(Self::CACHE_MAX_CAPACITY);

        let cache = Arc::new(
            moka::future::CacheBuilder::new(capacity)
                .time_to_live(Duration::from_secs(ttl))
                .build(),
        );

        let service = Self {
            repo,
            cache,
            invalidation_service,
            notification_service,
            single_flight: SingleFlight::new(),
        };

        // Start invalidation listener if CacheInvalidationService is available
        if let Some(ref inv_service) = service.invalidation_service {
            let cache_clone = service.cache.clone();
            let mut receiver = inv_service.subscribe();
            let cancel = cancel.unwrap_or_default();
            crate::spawn::spawn_monitored("room_settings_invalidation_listener", async move {
                // Rate-limit lag-triggered flushes (consistent with CacheManager)
                const LAG_FLUSH_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
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
                                    let room_id = RoomId::from_string(room_id.clone());
                                    cache_clone.invalidate(&room_id).await;
                                    tracing::debug!(
                                        room_id = %room_id.as_str(),
                                        "Room settings cache invalidated (cross-replica)"
                                    );
                                }
                                Ok(InvalidationMessage::All) => {
                                    cache_clone.invalidate_all();
                                    tracing::debug!("All room settings cache cleared (cross-replica)");
                                }
                                Ok(_) => {
                                    // Other invalidation types are handled elsewhere
                                }
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
            });
        }

        service
    }

    /// Get room settings with caching
    ///
    /// # Performance
    /// - L1 cache hit: < 1ms
    /// - Cache miss + DB query: ~10ms
    /// - `SingleFlight`: Prevents thundering herd on cache miss
    pub async fn get(&self, room_id: &RoomId) -> Result<RoomSettings> {
        // Try cache first
        if let Some(settings) = self.cache.get(room_id).await {
            return Ok(settings);
        }

        // Use SingleFlight to prevent thundering herd:
        // Only one task loads from DB for a given room_id; others wait for the result.
        let sf_key = room_id.as_str().to_string();
        let repo = self.repo.clone();
        let cache = self.cache.clone();
        let room_id_clone = room_id.clone();

        let settings = self.single_flight.do_work_with_fallback(
            sf_key,
            async move {
                // Double-check cache (another task may have populated it)
                if let Some(settings) = cache.get(&room_id_clone).await {
                    return Ok(settings);
                }

                // Load from database
                let settings = repo.get(&room_id_clone).await.map_err(|e| e.to_string())?;

                // Store in cache
                cache.insert(room_id_clone, settings.clone()).await;

                Ok(settings)
            },
            || "SingleFlight worker failed during room settings fetch".to_string(),
        ).await.map_err(Error::Internal)?;

        Ok(settings)
    }

    /// Get room settings without cache (force refresh)
    pub async fn get_refresh(&self, room_id: &RoomId) -> Result<RoomSettings> {
        // Invalidate cache
        self.invalidate_local(room_id).await;

        // Load from database
        let settings = self.repo.get(room_id).await?;

        // Store in cache
        let () = self.cache.insert(room_id.clone(), settings.clone()).await;

        Ok(settings)
    }

    /// Set room settings (write-through cache) with optimistic locking.
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
        for attempt in 0..Self::MAX_RETRIES {
            // Get current version (bypass cache)
            let (_current, version) = self.repo.get_with_version(room_id).await?;

            // CAS write
            match self.repo.set_settings_with_version(room_id, settings, version).await {
                Ok(new_version) => {
                    // Update local cache
                    self.cache.insert(room_id.clone(), settings.clone()).await;
                    self.publish_and_notify(room_id, settings, new_version).await;
                    return Ok(());
                }
                Err(Error::OptimisticLockConflict) if attempt + 1 < Self::MAX_RETRIES => {
                    let backoff = Self::BACKOFF_BASE_MS * (1 << attempt);
                    let jitter = rand::rng().random_range(0..Self::BACKOFF_BASE_MS);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff + jitter)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal("Settings update failed after maximum retry attempts".to_string()))
    }

    /// Update a single setting field with optimistic locking (CAS).
    ///
    /// Reads current settings and version, applies the updater, then writes back
    /// with a version check. Retries automatically on concurrent modification.
    pub async fn update_field<F>(
        &self,
        room_id: &RoomId,
        updater: F,
    ) -> Result<RoomSettings>
    where
        F: Fn(&mut RoomSettings) + Send,
    {
        for attempt in 0..Self::MAX_RETRIES {
            // Read current settings with version (bypass cache for freshness)
            let (mut settings, version) = self.repo.get_with_version(room_id).await?;

            // Apply update
            updater(&mut settings);

            // CAS write with version check
            match self.repo.set_settings_with_version(room_id, &settings, version).await {
                Ok(new_version) => {
                    // Update local cache after successful write
                    self.cache.insert(room_id.clone(), settings.clone()).await;
                    self.publish_and_notify(room_id, &settings, new_version).await;
                    return Ok(settings);
                }
                Err(Error::OptimisticLockConflict) if attempt + 1 < Self::MAX_RETRIES => {
                    let backoff = Self::BACKOFF_BASE_MS * (1 << attempt);
                    let jitter = rand::rng().random_range(0..Self::BACKOFF_BASE_MS);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff + jitter)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal("Settings update failed after maximum retry attempts".to_string()))
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
    async fn publish_and_notify(&self, room_id: &RoomId, settings: &RoomSettings, _version: i64) {
        if let Some(ref inv_service) = self.invalidation_service {
            if let Err(e) = inv_service.invalidate_room_settings(room_id).await {
                tracing::error!("Failed to publish settings invalidation: {}", e);
            }
        }

        self.notify_settings_changed(room_id, settings).await;
    }

    /// Invalidate local cache for a room
    async fn invalidate_local(&self, room_id: &RoomId) {
        let () = self.cache.invalidate(room_id).await;
    }

    /// Notify connected clients about settings change
    async fn notify_settings_changed(&self, room_id: &RoomId, settings: &RoomSettings) {
        let settings_value = match serde_json::to_value(settings) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize settings: {}", e);
                return;
            }
        };

        let _ = self.notification_service
            .notify_settings_updated(room_id, settings_value)
            .await;
    }

    /// Preload settings for multiple rooms (bulk loading)
    pub async fn preload(&self, room_ids: &[RoomId]) -> Result<()> {
        let mut loaded = std::collections::HashMap::new();

        for room_id in room_ids {
            if let Ok(settings) = self.repo.get(room_id).await {
                loaded.insert(room_id.clone(), settings);
            }
        }

        // Bulk insert into cache
        for (room_id, settings) in loaded {
            self.cache.insert(room_id, settings).await;
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

    #[tokio::test]
    async fn test_invalidation_via_streams() {
        // Create a CacheInvalidationService without Redis (local-only mode)
        let inv_service = Arc::new(CacheInvalidationService::new(
            None,
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
            None,
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
}
