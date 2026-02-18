//! Cache manager for coordinating multiple cache layers
//!
//! Provides a unified interface for managing all cache layers.
//! Supports cross-replica cache invalidation via `CacheInvalidationService`.

use super::{
    bloom_filter::ProtectedCache,
    username_cache::UsernameCache,
    user_cache::UserCache,
    room_cache::RoomCache,
    CacheInvalidationService,
    InvalidationMessage,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// Cache manager that coordinates all cache layers
#[derive(Clone)]
pub struct CacheManager {
    pub user_cache: Arc<UserCache>,
    pub room_cache: Arc<RoomCache>,
    pub username_cache: Option<Arc<UsernameCache>>,
    /// Optional protected cache (bloom filter) for cross-replica sync
    protected_cache: Option<Arc<ProtectedCache>>,
}

impl CacheManager {
    /// Create a new cache manager
    #[must_use]
    pub const fn new(user_cache: Arc<UserCache>, room_cache: Arc<RoomCache>) -> Self {
        Self {
            user_cache,
            room_cache,
            username_cache: None,
            protected_cache: None,
        }
    }

    /// Set the username cache for cross-replica invalidation
    #[must_use]
    pub fn with_username_cache(mut self, username_cache: Arc<UsernameCache>) -> Self {
        self.username_cache = Some(username_cache);
        self
    }

    /// Set the protected cache (bloom filter) for cross-replica sync
    #[must_use]
    pub fn with_protected_cache(mut self, protected_cache: Arc<ProtectedCache>) -> Self {
        self.protected_cache = Some(protected_cache);
        self
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
    pub fn start_invalidation_listener(&self, invalidation_service: &CacheInvalidationService) {
        let user_cache = self.user_cache.clone();
        let room_cache = self.room_cache.clone();
        let username_cache = self.username_cache.clone();
        let protected_cache = self.protected_cache.clone();
        let mut receiver = invalidation_service.subscribe();

        crate::spawn::spawn_monitored("cache_invalidation_listener", async move {
            loop {
                match receiver.recv().await {
                    Ok(msg) => {
                        match msg {
                            InvalidationMessage::User { ref user_id } => {
                                user_cache.invalidate_by_id(user_id).await;
                                debug!(
                                    user_id = %user_id,
                                    "User cache invalidated (cross-replica)"
                                );
                            }
                            InvalidationMessage::Username { ref user_id } => {
                                if let Some(ref uc) = username_cache {
                                    uc.invalidate_by_id(user_id).await;
                                    debug!(
                                        user_id = %user_id,
                                        "Username cache invalidated (cross-replica)"
                                    );
                                }
                            }
                            InvalidationMessage::Room { ref room_id } => {
                                room_cache.invalidate_by_id(room_id).await;
                                debug!(
                                    room_id = %room_id,
                                    "Room cache invalidated (cross-replica)"
                                );
                            }
                            InvalidationMessage::BloomFilterUpdate { ref keys } => {
                                if let Some(ref pc) = protected_cache {
                                    let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
                                    pc.mark_many_exists(&key_refs).await;
                                    debug!(
                                        count = keys.len(),
                                        "Bloom filter updated (cross-replica)"
                                    );
                                }
                            }
                            InvalidationMessage::UserTokenInvalidation { .. } => {
                                // Token blacklist removed - this message is now ignored
                            }
                            InvalidationMessage::All => {
                                user_cache.clear_l1().await;
                                room_cache.clear_l1().await;
                                if let Some(ref uc) = username_cache {
                                    uc.clear_memory().await;
                                }
                                debug!("All L1 caches cleared (cross-replica)");
                            }
                            // Permission messages are handled by PermissionService;
                            // PlaybackState/PlaybackStateUpdate messages are handled by PlaybackService;
                            // RoomSettings messages are handled by RoomSettingsService
                            InvalidationMessage::UserPermission { .. }
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
                        warn!(
                            lagged_messages = n,
                            "CacheManager invalidation listener lagged, flushing all L1 caches"
                        );
                        user_cache.clear_l1().await;
                        room_cache.clear_l1().await;
                        if let Some(ref uc) = username_cache {
                            uc.clear_memory().await;
                        }
                    }
                }
            }
        });
    }

    /// Clear all L1 caches (memory only)
    ///
    /// Useful for testing or manual cache clearing.
    /// Note: L2 (Redis) caches are not cleared.
    pub async fn clear_all_l1(&self) {
        self.user_cache.clear_l1().await;
        self.room_cache.clear_l1().await;
        if let Some(ref uc) = self.username_cache {
            uc.clear_memory().await;
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

    fn make_caches() -> (Arc<UserCache>, Arc<RoomCache>) {
        let user_cache = Arc::new(
            UserCache::new(None, 100, 5, 0, "test:user:".to_string()).unwrap(),
        );
        let room_cache = Arc::new(
            RoomCache::new(None, 100, 5, 0, "test:room:".to_string()).unwrap(),
        );
        (user_cache, room_cache)
    }

    #[tokio::test]
    async fn test_cache_manager_creation() {
        let (user_cache, room_cache) = make_caches();
        let _manager = CacheManager::new(user_cache, room_cache);
    }

    #[tokio::test]
    async fn test_clear_all_l1() {
        let (user_cache, room_cache) = make_caches();
        let manager = CacheManager::new(user_cache, room_cache);
        // This should not panic
        manager.clear_all_l1().await;
    }

    #[tokio::test]
    async fn test_invalidation_listener_user() {
        let (user_cache, room_cache) = make_caches();
        let manager = CacheManager::new(user_cache.clone(), room_cache.clone());

        let service = CacheInvalidationService::new(None, "test-node".to_string(), "synctv:cache:invalidate:stream".to_string());
        manager.start_invalidation_listener(&service);

        // Insert a user into L1 cache
        let user_id = crate::models::UserId::from_string("u1".to_string());
        let cached_user = crate::cache::user_cache::CachedUser::new(
            "u1".to_string(),
            "alice".to_string(),
            crate::models::UserRole::User,
            crate::models::UserStatus::Active,
            chrono::Utc::now(),
            0,
        );
        user_cache.set(&user_id, cached_user).await.unwrap();
        assert!(user_cache.get(&user_id).await.unwrap().is_some());

        // Broadcast invalidation (all nodes including local)
        service
            .broadcast_all(InvalidationMessage::User {
                user_id: "u1".to_string(),
            })
            .await
            .unwrap();

        // Give the spawned task time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // L1 entry should be gone
        assert!(user_cache.get(&user_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_invalidation_listener_room() {
        let (user_cache, room_cache) = make_caches();
        let manager = CacheManager::new(user_cache.clone(), room_cache.clone());

        let service = CacheInvalidationService::new(None, "test-node".to_string(), "synctv:cache:invalidate:stream".to_string());
        manager.start_invalidation_listener(&service);

        // Insert a room into L1 cache
        let room_id = crate::models::RoomId("r1".to_string());
        let cached_room = crate::cache::room_cache::CachedRoom::new(
            "r1".to_string(),
            "Test Room".to_string(),
            "u1".to_string(),
            true,
            chrono::Utc::now(),
        );
        room_cache.set(&room_id, cached_room).await.unwrap();
        assert!(room_cache.get(&room_id).await.unwrap().is_some());

        // Broadcast invalidation (all nodes including local)
        service
            .broadcast_all(InvalidationMessage::Room {
                room_id: "r1".to_string(),
            })
            .await
            .unwrap();

        // Give the spawned task time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // L1 entry should be gone
        assert!(room_cache.get(&room_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_invalidation_listener_all() {
        let (user_cache, room_cache) = make_caches();
        let manager = CacheManager::new(user_cache.clone(), room_cache.clone());

        let service = CacheInvalidationService::new(None, "test-node".to_string(), "synctv:cache:invalidate:stream".to_string());
        manager.start_invalidation_listener(&service);

        // Insert entries
        let user_id = crate::models::UserId::from_string("u1".to_string());
        let cached_user = crate::cache::user_cache::CachedUser::new(
            "u1".to_string(),
            "alice".to_string(),
            crate::models::UserRole::User,
            crate::models::UserStatus::Active,
            chrono::Utc::now(),
            0,
        );
        user_cache.set(&user_id, cached_user).await.unwrap();

        let room_id = crate::models::RoomId("r1".to_string());
        let cached_room = crate::cache::room_cache::CachedRoom::new(
            "r1".to_string(),
            "Test Room".to_string(),
            "u1".to_string(),
            true,
            chrono::Utc::now(),
        );
        room_cache.set(&room_id, cached_room).await.unwrap();

        // Broadcast All invalidation
        service
            .broadcast_all(InvalidationMessage::All)
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Both L1 entries should be gone
        assert!(user_cache.get(&user_id).await.unwrap().is_none());
        assert!(room_cache.get(&room_id).await.unwrap().is_none());
    }
}
