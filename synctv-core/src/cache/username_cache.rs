//! Username cache service for fast username lookups
//!
//! Uses `TieredCache<UserId, CachedUsername>` to eliminate duplicate L1/L2/retry logic.
//! - L1: In-memory Moka LRU cache for frequently accessed usernames
//! - L2: Redis persistent cache for cross-node consistency

use std::collections::HashMap;
use std::sync::Arc;

use futures::{stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};

use crate::cache::l2_backend::CacheL2Backend;
use crate::cache::tiered::TieredCache;
use crate::models::UserId;
use crate::{cache::CacheInvalidationRuntime, Result};

/// L1 (in-memory) cache TTL in seconds.
/// Matches UserCache/RoomCache defaults so stale entries are bounded even
/// without cross-replica invalidation.
const L1_TTL_SECONDS: u64 = 5 * 60;
const USERNAME_PRELOAD_CONCURRENCY: usize = 16;

/// Wrapper around a username string for `TieredCache` serialization.
///
/// `TieredCache` stores values as JSON in Redis. A raw `String` would be
/// double-quoted (`"\"alice\""`), making cache values harder to inspect
/// and less explicit than a structured payload. This newtype serializes
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
    invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
}

impl UsernameCache {
    /// Create a new `UsernameCache`
    ///
    /// # Arguments
    /// * `l2` - L2 cache backend (e.g. `RedisCacheL2` or `NoopCacheL2`)
    /// * `key_prefix` - L2 key prefix (e.g., "synctv:username:")
    /// * `memory_cache_size` - Maximum number of entries in memory cache
    /// * `ttl_seconds` - Cache TTL in L2 (0 = no expiration)
    pub fn new(
        l2: Arc<dyn CacheL2Backend>,
        key_prefix: String,
        memory_cache_size: usize,
        ttl_seconds: u64,
    ) -> Self {
        Self::new_with_invalidation(l2, key_prefix, memory_cache_size, ttl_seconds, None)
    }

    pub fn new_with_invalidation(
        l2: Arc<dyn CacheL2Backend>,
        key_prefix: String,
        memory_cache_size: usize,
        ttl_seconds: u64,
        invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
    ) -> Self {
        let inner = TieredCache::new(
            l2,
            memory_cache_size as u64,
            L1_TTL_SECONDS,
            ttl_seconds,
            key_prefix,
            "username".to_string(),
        );

        Self {
            inner,
            invalidation_service,
        }
    }

    /// Create a username cache for local-only operation without shared L2 state.
    pub fn local_only(key_prefix: String, memory_cache_size: usize, ttl_seconds: u64) -> Self {
        Self::new(
            crate::cache::local_l2_cache_backend(),
            key_prefix,
            memory_cache_size,
            ttl_seconds,
        )
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
    /// Updates both memory cache (L1) and Redis cache (L2).
    ///
    /// This method does NOT broadcast invalidation to other replicas. The
    /// rationale: `set()` writes the correct value to L2 (Redis), which is
    /// shared across all nodes. Other nodes will pick up the updated value
    /// from L2 when their L1 TTL expires. If immediate cross-node eviction
    /// is needed (e.g., after a username change), call [`invalidate`] instead.
    ///
    /// Previously, `set()` called `invalidate_username()` which broadcast to
    /// all nodes, causing the just-written value to be deleted on the self
    /// node when the invalidation listener processed the message.
    pub async fn set(&self, user_id: &UserId, username: &str) -> Result<()> {
        self.inner
            .set(user_id, CachedUsername::new(username.to_string()))
            .await
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
                    user_id = %user_id,
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
    pub async fn invalidate_by_id(&self, user_id: &str) -> Result<()> {
        self.inner.invalidate_by_id(user_id).await
    }

    /// Clear all cached usernames (memory only)
    ///
    /// This is useful for testing or manual cache clearing.
    /// Note: Redis cache is not cleared.
    pub fn clear_memory(&self) {
        self.inner.clear_l1();
    }

    /// Clear both L1 (in-memory) and L2 (Redis) username caches.
    ///
    /// Used during lag-triggered full flushes to prevent stale L2 entries from
    /// re-populating L1 on this or other replicas.
    pub async fn clear(&self) {
        self.inner.clear().await;
    }

    /// Preload usernames into cache
    ///
    /// Useful for warming up the cache with frequently accessed users.
    /// Writes each entry to both L1 and L2 without broadcasting any
    /// invalidation messages (since `set()` does not broadcast).
    pub async fn preload(&self, entries: HashMap<UserId, String>) -> Result<()> {
        let count = entries.len();
        stream::iter(entries)
            .map(Ok::<_, crate::Error>)
            .try_for_each_concurrent(
                USERNAME_PRELOAD_CONCURRENCY,
                |(user_id, username)| async move { self.set(&user_id, &username).await },
            )
            .await?;

        tracing::debug!(count, "Username cache preloaded");
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn l1_ttl(&self) -> Option<std::time::Duration> {
        self.inner.l1_ttl()
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
    use crate::cache::CacheInvalidationService;
    use crate::test_helpers::{TestOptionExt, TestResultExt};

    fn create_test_user_id(id: i64) -> UserId {
        UserId::expect_positive(id)
    }

    fn test_username_cache(capacity: usize, ttl_seconds: u64) -> UsernameCache {
        UsernameCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            "test:".to_string(),
            capacity,
            ttl_seconds,
        )
    }

    fn test_username_cache_with_invalidation(
        capacity: usize,
        ttl_seconds: u64,
        invalidation_service: Arc<dyn CacheInvalidationRuntime>,
    ) -> UsernameCache {
        UsernameCache::new_with_invalidation(
            Arc::new(crate::cache::NoopCacheL2),
            "test:".to_string(),
            capacity,
            ttl_seconds,
            Some(invalidation_service),
        )
    }

    #[tokio::test]
    async fn test_memory_cache_only() {
        let cache = test_username_cache(10, 0);

        let user_id = create_test_user_id(97_001);

        // Cache miss
        assert!(cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .is_none());

        // Set and get
        cache
            .set(&user_id, "alice")
            .await
            .checked("operation should succeed");
        let retrieved = cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(retrieved, "alice");

        // Invalidate
        cache
            .invalidate(&user_id)
            .await
            .checked("operation should succeed");
        assert!(cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .is_none());
    }

    #[tokio::test]
    async fn test_batch_lookup() {
        let cache = test_username_cache(10, 0);

        let user1 = create_test_user_id(97_002);
        let user2 = create_test_user_id(97_003);
        let user3 = create_test_user_id(97_004);

        // Set some entries
        cache
            .set(&user1, "alice")
            .await
            .checked("operation should succeed");
        cache
            .set(&user3, "charlie")
            .await
            .checked("operation should succeed");

        // Batch lookup
        let result = cache
            .get_batch(&[user1, user2, user3])
            .await
            .checked("operation should succeed");

        assert_eq!(result.len(), 2);
        assert_eq!(result.get(&user1), Some(&"alice".to_string()));
        assert_eq!(result.get(&user2), None);
        assert_eq!(result.get(&user3), Some(&"charlie".to_string()));
    }

    #[tokio::test]
    async fn test_clear_memory() {
        let cache = test_username_cache(10, 0);

        let user_id = create_test_user_id(97_005);
        cache
            .set(&user_id, "alice")
            .await
            .checked("operation should succeed");
        assert!(cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .is_some());

        cache.clear_memory();
        assert!(cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .is_none());
    }

    #[tokio::test]
    async fn test_invalidate_by_id() {
        let cache = test_username_cache(10, 0);

        let user_id = create_test_user_id(97_006);
        cache
            .set(&user_id, "alice")
            .await
            .checked("operation should succeed");
        assert!(cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .is_some());

        cache
            .invalidate_by_id(&user_id.to_string())
            .await
            .checked("operation should succeed");
        assert!(cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .is_none());
    }

    /// set() with an invalidation service configured must not self-invalidate
    /// the value it just inserted.
    #[tokio::test]
    async fn test_set_with_invalidation_service_no_self_invalidation() {
        let invalidation_service = Arc::new(CacheInvalidationService::new(
            "test-node".to_string(),
            "test:cache:invalidate:stream".to_string(),
        ));

        let cache = test_username_cache_with_invalidation(100, 0, invalidation_service);

        let user_id = create_test_user_id(97_007);

        // set() should write the value and NOT self-invalidate
        cache
            .set(&user_id, "alice")
            .await
            .checked("operation should succeed");

        let retrieved = cache
            .get(&user_id)
            .await
            .checked("operation should succeed");
        assert_eq!(
            retrieved.as_deref(),
            Some("alice"),
            "set() must not self-invalidate: value should be retrievable immediately after set()"
        );
    }

    /// preload() calls set() for each entry, and all preloaded entries must be
    /// retrievable.
    #[tokio::test]
    async fn test_preload_all_entries_retrievable() {
        let invalidation_service = Arc::new(CacheInvalidationService::new(
            "test-node".to_string(),
            "test:cache:invalidate:stream".to_string(),
        ));

        let cache = test_username_cache_with_invalidation(100, 0, invalidation_service);

        let mut entries = HashMap::new();
        entries.insert(create_test_user_id(97_008), "alice".to_string());
        entries.insert(create_test_user_id(97_009), "bob".to_string());
        entries.insert(create_test_user_id(97_010), "charlie".to_string());
        entries.insert(create_test_user_id(97_011), "diana".to_string());
        entries.insert(create_test_user_id(97_012), "eve".to_string());

        cache
            .preload(entries.clone())
            .await
            .checked("operation should succeed");

        // All preloaded entries must be retrievable
        for (user_id, expected_name) in &entries {
            let retrieved = cache.get(user_id).await.checked("operation should succeed");
            assert_eq!(
                retrieved.as_deref(),
                Some(expected_name.as_str()),
                "preload entry for {user_id} should be retrievable"
            );
        }

        // Verify batch lookup also works
        let all_ids: Vec<UserId> = entries.keys().copied().collect();
        let batch = cache
            .get_batch(&all_ids)
            .await
            .checked("operation should succeed");
        assert_eq!(
            batch.len(),
            entries.len(),
            "batch lookup should return all preloaded entries"
        );
    }

    /// set() must NOT broadcast any invalidation message. Only invalidate()
    /// should broadcast to other replicas.
    #[tokio::test]
    async fn test_set_does_not_broadcast_invalidation() {
        let invalidation_service = Arc::new(CacheInvalidationService::new(
            "test-node".to_string(),
            "test:cache:invalidate:stream".to_string(),
        ));

        let mut receiver = invalidation_service.subscribe();

        let cache = test_username_cache_with_invalidation(100, 0, invalidation_service);

        let user_id = create_test_user_id(97_013);

        // set() should NOT produce any invalidation message
        cache
            .set(&user_id, "alice")
            .await
            .checked("operation should succeed");

        // Give a short window for any message to arrive
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), receiver.recv()).await;

        assert!(
            result.is_err(),
            "set() should not broadcast any invalidation message; \
             invalidation should only happen on explicit invalidate() calls"
        );
    }

    #[test]
    fn test_l1_ttl_matches_five_minutes() {
        let cache = test_username_cache(10, 0);

        assert_eq!(
            cache.l1_ttl(),
            Some(std::time::Duration::from_mins(5)),
            "Username cache L1 TTL must be five minutes, not five seconds"
        );
    }
}
