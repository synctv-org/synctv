//! Cache manager for coordinating multiple cache layers
//!
//! Provides a unified interface for managing all cache layers.
//! Supports cross-replica cache invalidation via `CacheInvalidationService`.

use super::{
    room_cache::RoomCache, user_cache::UserCache, username_cache::UsernameCache,
    CacheInvalidationRuntime, InvalidationMessage,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// Minimum interval between lag-triggered full L1 flushes.
///
/// When the broadcast channel lags, flushing ALL L1 caches is expensive and
/// may cascade into a DB stampede. This constant rate-limits the full flush
/// so it happens at most once every 5 seconds even under sustained lag.
const LAG_FLUSH_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Cache manager that coordinates all cache layers
#[derive(Clone)]
pub struct CacheManager {
    pub user_cache: Arc<UserCache>,
    pub room_cache: Arc<RoomCache>,
    pub username_cache: Option<Arc<UsernameCache>>,
}

impl CacheManager {
    /// Create a new cache manager
    #[must_use]
    pub const fn new(
        user_cache: Arc<UserCache>,
        room_cache: Arc<RoomCache>,
        username_cache: Option<Arc<UsernameCache>>,
    ) -> Self {
        Self {
            user_cache,
            room_cache,
            username_cache,
        }
    }

    /// Start listening for cross-replica cache invalidation messages
    ///
    /// Subscribes to `CacheInvalidationService` and dispatches invalidation
    /// messages to the appropriate cache:
    /// - `InvalidationMessage::User { user_id }` -> `user_cache.invalidate_by_id()`
    /// - `InvalidationMessage::Room { room_id }` -> `room_cache.invalidate_by_id()`
    /// - `InvalidationMessage::All` -> `clear_all_l1()`
    ///
    /// Permission-related messages are ignored here (handled by `PermissionService`).
    pub fn start_invalidation_listener(
        &self,
        invalidation_service: &Arc<dyn CacheInvalidationRuntime>,
    ) -> JoinHandle<()> {
        let user_cache = self.user_cache.clone();
        let room_cache = self.room_cache.clone();
        let username_cache = self.username_cache.clone();
        let mut receiver = invalidation_service.subscribe();

        crate::spawn::spawn_monitored("cache_invalidation_listener", async move {
            // Track the last time we performed a lag-triggered full
            // L1 flush so we can rate-limit it to at most once per 5 seconds.
            let mut last_lag_flush = std::time::Instant::now()
                .checked_sub(LAG_FLUSH_MIN_INTERVAL)
                .unwrap_or_else(std::time::Instant::now);

            loop {
                match receiver.recv().await {
                    Ok(msg) => {
                        match msg {
                            InvalidationMessage::User { ref user_id } => {
                                match user_cache.invalidate_by_id(user_id).await {
                                    Ok(()) => {
                                        debug!(
                                            user_id = %user_id,
                                            "User cache invalidated (cross-replica)"
                                        );
                                    }
                                    Err(error) => {
                                        warn!(
                                            user_id = %user_id,
                                            error = %error,
                                            "Ignoring malformed user cache invalidation"
                                        );
                                    }
                                }
                            }
                            InvalidationMessage::Username { ref user_id } => {
                                if let Some(ref uc) = username_cache {
                                    match uc.invalidate_by_id(user_id).await {
                                        Ok(()) => {
                                            debug!(
                                                user_id = %user_id,
                                                "Username cache invalidated (cross-replica)"
                                            );
                                        }
                                        Err(error) => {
                                            warn!(
                                                user_id = %user_id,
                                                error = %error,
                                                "Ignoring malformed username cache invalidation"
                                            );
                                        }
                                    }
                                }
                            }
                            InvalidationMessage::Room { ref room_id } => {
                                match room_cache.invalidate_by_id(room_id).await {
                                    Ok(()) => {
                                        debug!(
                                            room_id = %room_id,
                                            "Room cache invalidated (cross-replica)"
                                        );
                                    }
                                    Err(error) => {
                                        warn!(
                                            room_id = %room_id,
                                            error = %error,
                                            "Ignoring malformed room cache invalidation"
                                        );
                                    }
                                }
                            }
                            InvalidationMessage::All => {
                                user_cache.clear_l1();
                                room_cache.clear_l1();
                                if let Some(ref uc) = username_cache {
                                    uc.clear_memory();
                                }
                                debug!("All L1 caches cleared (cross-replica)");
                            }
                            // Permission messages are handled by PermissionService;
                            // PlaybackState/PlaybackStateUpdate messages are handled by PlaybackService;
                            // RoomSettings messages are handled by RoomSettingsService
                            InvalidationMessage::UserPermission { .. }
                            | InvalidationMessage::ProviderInstance { .. }
                            | InvalidationMessage::RoomPermission { .. }
                            | InvalidationMessage::PlaybackState { .. }
                            | InvalidationMessage::PlaybackStateUpdate { .. }
                            | InvalidationMessage::RoomSettings { .. } => {}
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!("Cache invalidation channel closed, stopping CacheManager listener");
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Rate-limit full L1 flushes to at most once per 5s.
                        // Without this, a sustained lag storm (e.g., Redis pubsub burst)
                        // would trigger a continuous cascade of DB re-fetches.
                        let now = std::time::Instant::now();
                        let elapsed = now.duration_since(last_lag_flush);
                        if elapsed >= LAG_FLUSH_MIN_INTERVAL {
                            warn!(
                                lagged_messages = n,
                                "CacheManager invalidation listener lagged, flushing all L1 and L2 caches (rate-limited to once per {}s)",
                                LAG_FLUSH_MIN_INTERVAL.as_secs()
                            );
                            // Flush both L1 and L2 so stale Redis entries cannot
                            // re-populate L1 on this or other replicas.
                            user_cache.clear().await;
                            room_cache.clear().await;
                            if let Some(ref uc) = username_cache {
                                uc.clear().await;
                            }
                            // Record metric so operators can observe flush frequency
                            crate::metrics::cache::CACHE_LAG_FLUSH_TOTAL
                                .with_label_values(&["cache_manager"])
                                .inc();
                            last_lag_flush = now;
                        } else {
                            let remaining = LAG_FLUSH_MIN_INTERVAL
                                .checked_sub(elapsed)
                                .unwrap_or_default();
                            warn!(
                                lagged_messages = n,
                                skip_flush_secs = remaining.as_secs(),
                                "CacheManager invalidation listener lagged, skipping flush (rate-limited)"
                            );
                        }
                    }
                }
            }
        })
    }

    /// Clear all L1 caches (memory only)
    ///
    /// Useful for testing or manual cache clearing.
    /// Note: L2 (Redis) caches are not cleared.
    pub fn clear_all_l1(&self) {
        self.user_cache.clear_l1();
        self.room_cache.clear_l1();
        if let Some(ref uc) = self.username_cache {
            uc.clear_memory();
        }
        tracing::debug!("All L1 caches cleared");
    }
}

impl std::fmt::Debug for CacheManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheManager").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheInvalidationRuntime, CacheInvalidationService};
    use crate::test_helpers::TestResultExt;

    fn make_caches() -> (Arc<UserCache>, Arc<RoomCache>) {
        let l2 = crate::cache::local_l2_cache_backend();
        let user_cache = Arc::new(UserCache::new(
            l2.clone(),
            100,
            5,
            0,
            "test:user:".to_string(),
        ));
        let room_cache = Arc::new(RoomCache::new(l2, 100, 5, 0, "test:room:".to_string()));
        (user_cache, room_cache)
    }

    #[tokio::test]
    async fn test_invalidation_listener_user() {
        let (user_cache, room_cache) = make_caches();
        let manager = CacheManager::new(user_cache.clone(), room_cache.clone(), None);

        let service: Arc<dyn CacheInvalidationRuntime> = Arc::new(CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        ));
        manager.start_invalidation_listener(&service);

        // Insert a user into L1 cache
        let user_id = crate::models::UserId::expect_positive(94_001);
        let cached_user = crate::cache::user_cache::CachedUser::new(
            user_id,
            "alice".to_string(),
            crate::models::UserRole::User,
            crate::models::UserStatus::Active,
            crate::SystemClock.now(),
        );
        user_cache
            .set(&user_id, cached_user)
            .await
            .checked("operation should succeed");
        assert!(user_cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .is_some());

        // Broadcast invalidation (all nodes including local)
        service
            .broadcast_all(InvalidationMessage::User {
                user_id: "94001".to_string(),
            })
            .await
            .checked("operation should succeed");

        // Give the spawned task time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // L1 entry should be gone
        assert!(user_cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .is_none());
    }

    #[tokio::test]
    async fn test_invalidation_listener_room() {
        let (user_cache, room_cache) = make_caches();
        let manager = CacheManager::new(user_cache.clone(), room_cache.clone(), None);

        let service: Arc<dyn CacheInvalidationRuntime> = Arc::new(CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        ));
        manager.start_invalidation_listener(&service);

        // Insert a room into L1 cache
        let room_id = crate::models::RoomId::expect_positive(1);
        let cached_room = crate::cache::room_cache::CachedRoom::new(
            "r1".to_string(),
            "Test Room".to_string(),
            "u1".to_string(),
            true,
            crate::SystemClock.now(),
        );
        room_cache
            .set(&room_id, cached_room)
            .await
            .checked("operation should succeed");
        assert!(room_cache
            .get(&room_id)
            .await
            .checked("operation should succeed")
            .is_some());

        // Broadcast invalidation (all nodes including local)
        service
            .broadcast_all(InvalidationMessage::Room {
                room_id: room_id.to_string(),
            })
            .await
            .checked("operation should succeed");

        // Give the spawned task time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // L1 entry should be gone
        assert!(room_cache
            .get(&room_id)
            .await
            .checked("operation should succeed")
            .is_none());
    }

    #[tokio::test]
    async fn test_invalidation_listener_all() {
        let (user_cache, room_cache) = make_caches();
        let manager = CacheManager::new(user_cache.clone(), room_cache.clone(), None);

        let service: Arc<dyn CacheInvalidationRuntime> = Arc::new(CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        ));
        manager.start_invalidation_listener(&service);

        // Insert entries
        let user_id = crate::models::UserId::expect_positive(1);
        let cached_user = crate::cache::user_cache::CachedUser::new(
            user_id,
            "alice".to_string(),
            crate::models::UserRole::User,
            crate::models::UserStatus::Active,
            crate::SystemClock.now(),
        );
        user_cache
            .set(&user_id, cached_user)
            .await
            .checked("operation should succeed");

        let room_id = crate::models::RoomId::expect_positive(1);
        let cached_room = crate::cache::room_cache::CachedRoom::new(
            "r1".to_string(),
            "Test Room".to_string(),
            "u1".to_string(),
            true,
            crate::SystemClock.now(),
        );
        room_cache
            .set(&room_id, cached_room)
            .await
            .checked("operation should succeed");

        // Broadcast All invalidation
        service
            .broadcast_all(InvalidationMessage::All)
            .await
            .checked("operation should succeed");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Both L1 entries should be gone
        assert!(user_cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .is_none());
        assert!(room_cache
            .get(&room_id)
            .await
            .checked("operation should succeed")
            .is_none());
    }
}
