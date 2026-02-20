//! Username cache service for fast username lookups
//!
//! Uses `TieredCache<UserId, CachedUsername>` to eliminate duplicate L1/L2/retry logic.
//! - L1: In-memory Moka LRU cache for frequently accessed usernames
//! - L2: Redis persistent cache for cross-node consistency

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::cache::l2_backend::CacheL2Backend;
use crate::cache::tiered::TieredCache;
use crate::models::UserId;
use crate::{cache::CacheInvalidationService, Result};

/// L1 (in-memory) cache TTL in minutes.
/// Matches UserCache/RoomCache defaults so stale entries are bounded even
/// without cross-replica invalidation.
const L1_TTL_MINUTES: u64 = 5;

/// Wrapper around a username string for `TieredCache` serialization.
///
/// `TieredCache` stores values as JSON in Redis. A raw `String` would be
/// double-quoted (`"\"alice\""`), breaking backward compatibility with
/// existing Redis data and making debugging harder. This newtype serializes
/// as `{"username":"alice"}` and is transparent to callers via `From`/`Into`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CachedUsername {
    username: String,
}

impl CachedUsername {
    const fn new(username: String) -> Self {
        Self { username }
    }

    fn into_inner(self) -> String {
        self.username
    }
}

/// Username cache service with L1 (Moka) + L2 (Redis) strategy
///
/// Delegates to `TieredCache<UserId, CachedUsername>` for all L1/L2 operations
/// including retry logic, metrics, and batch lookups.
///
/// ## Cache key design
///
/// The cache is keyed by `UserId`, NOT by the username string. This means
/// invalidation messages containing a `user_id` directly evict the correct
/// cache entry. When a username is changed:
///
/// 1. The old entry `user_id -> old_username` is invalidated via `invalidate(user_id)`
/// 2. The next lookup for `user_id` misses the cache and fetches the new username from DB
/// 3. The new entry `user_id -> new_username` is populated on that cache miss
///
/// A reverse mapping (`user_id -> old_username`) is NOT needed because the
/// cache key IS the `user_id`, so `InvalidationMessage::Username { user_id }`
/// maps directly to the cache key without needing to know the old username.
#[derive(Clone)]
pub struct UsernameCache {
    inner: TieredCache<UserId, CachedUsername>,
    /// Optional invalidation service for cross-replica cache sync
    invalidation_service: Option<Arc<CacheInvalidationService>>,
}

impl UsernameCache {
    /// Create a new `UsernameCache`
    ///
    /// # Arguments
    /// * `l2` - L2 cache backend (e.g. `RedisCacheL2` or `NoopCacheL2`)
    /// * `key_prefix` - L2 key prefix (e.g., "synctv:username:")
    /// * `memory_cache_size` - Maximum number of entries in memory cache
    /// * `ttl_seconds` - Cache TTL in L2 (0 = no expiration)
    #[must_use]
    pub fn new(
        l2: Arc<dyn CacheL2Backend>,
        key_prefix: String,
        memory_cache_size: usize,
        ttl_seconds: u64,
    ) -> Self {
        // TieredCache::new returns Result but only fails on construction errors
        // which won't happen with valid parameters. Unwrap is safe here.
        let inner = TieredCache::new(
            l2,
            memory_cache_size as u64,
            L1_TTL_MINUTES,
            ttl_seconds,
            key_prefix,
            "username".to_string(),
        )
        .expect("Failed to create TieredCache for UsernameCache");

        Self {
            inner,
            invalidation_service: None,
        }
    }

    /// Set the cache invalidation service for cross-replica sync
    #[must_use]
    pub fn with_invalidation_service(mut self, service: Arc<CacheInvalidationService>) -> Self {
        self.invalidation_service = Some(service);
        self
    }

    /// Get username for a user ID
    ///
    /// Checks memory cache first, then Redis cache.
    /// Returns None if not found in any cache.
    pub async fn get(&self, user_id: &UserId) -> Result<Option<String>> {
        let cached = self.inner.get(user_id).await?;
        Ok(cached.map(CachedUsername::into_inner))
    }

    /// Set username for a user ID
    ///
    /// Updates both memory cache and Redis cache.
    /// If a `CacheInvalidationService` is configured, broadcasts the invalidation
    /// to other replicas so they evict stale L1 entries.
    pub async fn set(&self, user_id: &UserId, username: &str) -> Result<()> {
        self.inner
            .set(user_id, CachedUsername::new(username.to_string()))
            .await?;

        // Broadcast invalidation to other replicas (best effort)
        if let Some(ref service) = self.invalidation_service {
            if let Err(e) = service.invalidate_username(user_id).await {
                tracing::warn!(
                    error = %e,
                    user_id = %user_id.as_str(),
                    "Failed to broadcast username cache invalidation to other replicas"
                );
            }
        }

        Ok(())
    }

    /// Get multiple usernames at once
    ///
    /// More efficient than calling `get()` multiple times.
    /// Returns a map of `user_id` -> username.
    pub async fn get_batch(&self, user_ids: &[UserId]) -> Result<HashMap<UserId, String>> {
        let batch = self.inner.get_batch(user_ids).await?;
        Ok(batch
            .into_iter()
            .map(|(k, v)| (k, v.into_inner()))
            .collect())
    }

    /// Invalidate a cached username
    ///
    /// Removes the username from both memory (L1) and Redis (L2) cache.
    /// L1 is invalidated first so this replica immediately stops serving stale
    /// data, then L2 is cleared so other replicas don't re-populate from stale
    /// Redis data. This is consistent with `UserCache` and `RoomCache`.
    ///
    /// If a `CacheInvalidationService` is configured, broadcasts the invalidation
    /// to other replicas so they evict stale L1 entries.
    pub async fn invalidate(&self, user_id: &UserId) -> Result<()> {
        self.inner.invalidate(user_id).await?;

        // Broadcast invalidation to other replicas (best effort)
        if let Some(ref service) = self.invalidation_service {
            if let Err(e) = service.invalidate_username(user_id).await {
                tracing::warn!(
                    error = %e,
                    user_id = %user_id.as_str(),
                    "Failed to broadcast username cache invalidation to other replicas"
                );
            }
        }

        Ok(())
    }

    /// Invalidate a specific username cache entry by ID string (both L1 and L2)
    ///
    /// Used by the cross-replica invalidation listener to remove a single
    /// entry from the local in-memory cache and L2 Redis cache.
    pub async fn invalidate_by_id(&self, user_id: &str) {
        self.inner.invalidate_by_id(user_id).await;
    }

    /// Clear all cached usernames (memory only)
    ///
    /// This is useful for testing or manual cache clearing.
    /// Note: Redis cache is not cleared.
    pub async fn clear_memory(&self) {
        self.inner.clear_l1().await;
    }

    /// Preload usernames into cache
    ///
    /// Useful for warming up the cache with frequently accessed users.
    pub async fn preload(&self, entries: HashMap<UserId, String>) -> Result<()> {
        for (user_id, username) in entries {
            self.set(&user_id, &username).await?;
        }

        tracing::debug!("Username cache preloaded");
        Ok(())
    }
}

impl std::fmt::Debug for UsernameCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsernameCache")
            .field("inner", &self.inner)
            .field("invalidation_enabled", &self.invalidation_service.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_user_id(id: &str) -> UserId {
        UserId::from_string(id.to_string())
    }

    #[tokio::test]
    async fn test_memory_cache_only() {
        let cache = UsernameCache::new(Arc::new(crate::cache::NoopCacheL2), "test:".to_string(), 10, 0);

        let user_id = create_test_user_id("user1");

        // Cache miss
        assert!(cache.get(&user_id).await.unwrap().is_none());

        // Set and get
        cache.set(&user_id, "alice").await.unwrap();
        let retrieved = cache.get(&user_id).await.unwrap().unwrap();
        assert_eq!(retrieved, "alice");

        // Invalidate
        cache.invalidate(&user_id).await.unwrap();
        assert!(cache.get(&user_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_batch_lookup() {
        let cache = UsernameCache::new(Arc::new(crate::cache::NoopCacheL2), "test:".to_string(), 10, 0);

        let user1 = create_test_user_id("user1");
        let user2 = create_test_user_id("user2");
        let user3 = create_test_user_id("user3");

        // Set some entries
        cache.set(&user1, "alice").await.unwrap();
        cache.set(&user3, "charlie").await.unwrap();

        // Batch lookup
        let result = cache
            .get_batch(&[user1.clone(), user2.clone(), user3.clone()])
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result.get(&user1), Some(&"alice".to_string()));
        assert_eq!(result.get(&user2), None);
        assert_eq!(result.get(&user3), Some(&"charlie".to_string()));
    }

    #[tokio::test]
    async fn test_clear_memory() {
        let cache = UsernameCache::new(Arc::new(crate::cache::NoopCacheL2), "test:".to_string(), 10, 0);

        let user_id = create_test_user_id("user1");
        cache.set(&user_id, "alice").await.unwrap();
        assert!(cache.get(&user_id).await.unwrap().is_some());

        cache.clear_memory().await;
        assert!(cache.get(&user_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_invalidate_by_id() {
        let cache = UsernameCache::new(Arc::new(crate::cache::NoopCacheL2), "test:".to_string(), 10, 0);

        let user_id = create_test_user_id("user1");
        cache.set(&user_id, "alice").await.unwrap();
        assert!(cache.get(&user_id).await.unwrap().is_some());

        cache.invalidate_by_id("user1").await;
        assert!(cache.get(&user_id).await.unwrap().is_none());
    }
}
