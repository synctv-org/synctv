//! User information cache (L1: Moka in-memory, L2: Redis)
//!
//! Provides fast access to user profile data with a two-tier caching strategy:
//! - L1: In-memory Moka cache (very fast, local to the node)
//! - L2: Redis cache (fast, shared across nodes)

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{models::UserId, Error, Result};

/// User cache with L1 (Moka) + L2 (Redis) strategy
#[derive(Clone)]
pub struct UserCache {
    redis_conn: Option<redis::aio::ConnectionManager>,
    l1_cache: Arc<moka::future::Cache<UserId, CachedUser>>,
    l2_ttl_seconds: u64,
    key_prefix: String,
}

/// Cached user data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedUser {
    id: String,
    username: String,
    role: String,  // UserRole as string (root, admin, user)
    status: String,  // UserStatus as string (active, pending, banned)
    created_at: chrono::DateTime<chrono::Utc>,
    /// Timestamp of last update - used to prevent stale data from overwriting fresh data
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl CachedUser {
    /// Create a new `CachedUser`
    #[must_use]
    pub fn new(
        id: String,
        username: String,
        role: String,
        status: String,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            id,
            username,
            role,
            status,
            created_at,
            updated_at: chrono::Utc::now(),
        }
    }

    /// Create a new `CachedUser` with explicit updated_at timestamp
    #[must_use]
    pub fn with_updated_at(
        id: String,
        username: String,
        role: String,
        status: String,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self { id, username, role, status, created_at, updated_at }
    }

    /// Get the updated_at timestamp
    #[must_use]
    pub const fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }
}

impl UserCache {
    /// Create a new `UserCache`
    ///
    /// # Arguments
    /// * `redis_conn` - Optional Redis `ConnectionManager`. If None, only L1 caching is used.
    /// * `l1_max_capacity` - Maximum number of entries in L1 cache
    /// * `l1_ttl_minutes` - TTL for L1 cache entries in minutes
    /// * `l2_ttl_seconds` - TTL for L2 (Redis) cache entries in seconds
    /// * `key_prefix` - Redis key prefix (e.g., "synctv:user:")
    pub fn new(
        redis_conn: Option<redis::aio::ConnectionManager>,
        l1_max_capacity: u64,
        l1_ttl_minutes: u64,
        l2_ttl_seconds: u64,
        key_prefix: String,
    ) -> Result<Self> {
        let l1_cache = moka::future::CacheBuilder::new(l1_max_capacity)
            .time_to_live(std::time::Duration::from_secs(l1_ttl_minutes * 60))
            .build();

        Ok(Self {
            redis_conn,
            l1_cache: Arc::new(l1_cache),
            l2_ttl_seconds,
            key_prefix,
        })
    }

    /// Get user data from cache
    ///
    /// Checks L1 first, then L2. Returns None if not found in either cache.
    pub async fn get(&self, user_id: &UserId) -> Result<Option<CachedUser>> {
        // Check L1 (in-memory) cache first
        if let Some(user) = self.l1_cache.get(user_id).await {
            crate::metrics::cache::CACHE_HITS
                .with_label_values(&["user", "l1"])
                .inc();
            tracing::debug!(
                user_id = %user_id.as_str(),
                "User cache hit (L1)"
            );
            return Ok(Some(user));
        }

        // Check L2 (Redis) cache
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let key = format!("{}{}", self.key_prefix, user_id.as_str());
            let user_json: Option<String> = conn
                .get(&key)
                .await
                .map_err(|e| Error::Internal(format!("Failed to get user from cache: {e}")))?;

            if let Some(json) = user_json {
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&["user", "l2"])
                    .inc();
                tracing::debug!(
                    user_id = %user_id.as_str(),
                    "User cache hit (L2)"
                );

                let user: CachedUser = serde_json::from_str(&json).map_err(|e| {
                    Error::Internal(format!("Failed to deserialize cached user: {e}"))
                })?;

                // Populate L1 cache
                self.l1_cache.insert(user_id.clone(), user.clone()).await;

                return Ok(Some(user));
            }
        }

        crate::metrics::cache::CACHE_MISSES
            .with_label_values(&["user", "l1"])
            .inc();
        tracing::debug!(user_id = %user_id.as_str(), "User cache miss");
        Ok(None)
    }

    /// Set user data in cache
    ///
    /// Updates both L1 and L2 caches.
    pub async fn set(&self, user_id: &UserId, user: CachedUser) -> Result<()> {
        // Update L1 cache
        self.l1_cache.insert(user_id.clone(), user.clone()).await;

        // Update L2 cache
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let key = format!("{}{}", self.key_prefix, user_id.as_str());
            let json = serde_json::to_string(&user).map_err(|e| {
                Error::Internal(format!("Failed to serialize user for caching: {e}"))
            })?;

            // Add TTL jitter to prevent cache avalanche (±10% random jitter)
            let ttl_with_jitter = if self.l2_ttl_seconds > 0 {
                self.add_ttl_jitter(self.l2_ttl_seconds)
            } else {
                0
            };

            if ttl_with_jitter > 0 {
                let _: () = conn
                    .set_ex(&key, json, ttl_with_jitter)
                    .await
                    .map_err(|e| Error::Internal(format!("Failed to set user in cache: {e}")))?;
            } else {
                let _: () = conn
                    .set(&key, json)
                    .await
                    .map_err(|e| Error::Internal(format!("Failed to set user in cache: {e}")))?;
            }

            tracing::debug!(
                user_id = %user_id.as_str(),
                ttl_seconds = ttl_with_jitter,
                "User cached"
            );
        }

        Ok(())
    }

    /// Set user data in cache only if it's newer than existing data
    ///
    /// Compares `updated_at` timestamps and only updates if the new data is newer.
    /// This prevents race conditions where stale data overwrites fresh data.
    pub async fn set_if_newer(&self, user_id: &UserId, user: CachedUser) -> Result<bool> {
        // Check L1 cache first
        if let Some(existing) = self.l1_cache.get(user_id).await {
            if user.updated_at <= existing.updated_at {
                tracing::debug!(
                    user_id = %user_id.as_str(),
                    existing_ts = %existing.updated_at,
                    new_ts = %user.updated_at,
                    "Skipping cache update - data is not newer"
                );
                return Ok(false);
            }
        }

        // Check L2 cache if L1 miss
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let key = format!("{}{}", self.key_prefix, user_id.as_str());

            if let Ok(Some(json)) = conn.get::<_, Option<String>>(&key).await {
                if let Ok(existing) = serde_json::from_str::<CachedUser>(&json) {
                    if user.updated_at <= existing.updated_at {
                        tracing::debug!(
                            user_id = %user_id.as_str(),
                            existing_ts = %existing.updated_at,
                            new_ts = %user.updated_at,
                            "Skipping cache update - L2 data is not newer"
                        );
                        return Ok(false);
                    }
                }
            }
        }

        // Data is newer, perform the update
        self.set(user_id, user).await?;
        Ok(true)
    }

    /// Add random jitter to TTL to prevent cache avalanche
    ///
    /// Returns TTL with ±10% random jitter
    fn add_ttl_jitter(&self, ttl_seconds: u64) -> u64 {
        if ttl_seconds == 0 {
            return 0;
        }

        let jitter_range = (ttl_seconds as f64 * 0.1) as u64; // ±10%
        if jitter_range == 0 {
            return ttl_seconds;
        }

        // Use nanosecond timestamp as pseudo-random source
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        let jitter = now % (jitter_range * 2 + 1);

        ttl_seconds.saturating_sub(jitter_range).saturating_add(jitter)
    }

    /// Invalidate user data from cache
    ///
    /// Removes from both L1 and L2 caches.
    /// L1 is invalidated first to ensure this replica immediately stops serving
    /// stale data, then L2 is cleared so other replicas don't re-populate from
    /// stale Redis data.
    pub async fn invalidate(&self, user_id: &UserId) -> Result<()> {
        // Remove from L1 (in-memory) FIRST so this replica stops serving stale data immediately
        self.l1_cache.invalidate(user_id).await;

        // Then remove from L2 (Redis) with retry logic
        if self.redis_conn.is_some() {
            let key = format!("{}{}", self.key_prefix, user_id.as_str());
            self.delete_from_redis_with_retry(&key, 3).await?;
        }

        crate::metrics::cache::CACHE_EVICTIONS
            .with_label_values(&["user"])
            .inc();
        tracing::debug!(user_id = %user_id.as_str(), "User cache invalidated (L1 then L2)");

        Ok(())
    }

    /// Delete a key from Redis with retry logic
    ///
    /// Attempts to delete up to `max_retries` times with exponential backoff.
    /// Returns error only if all retries fail.
    async fn delete_from_redis_with_retry(&self, key: &str, max_retries: u32) -> Result<()> {
        let Some(ref redis_conn) = self.redis_conn else {
            return Ok(());
        };

        for attempt in 0..max_retries {
            let mut conn = redis_conn.clone();

            match conn.del::<_, ()>(key).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let is_last_attempt = attempt == max_retries - 1;

                    if is_last_attempt {
                        // Last attempt failed, return error
                        crate::metrics::cache::CACHE_ERRORS
                            .with_label_values(&["user", "l2_delete"])
                            .inc();
                        tracing::error!(
                            key = %key,
                            error = %e,
                            attempts = max_retries,
                            "Failed to delete from Redis L2 cache after retries"
                        );
                        return Err(Error::Internal(format!("Failed to delete from Redis cache: {e}")));
                    } else {
                        // Retry with exponential backoff: 10ms, 50ms, 250ms
                        let backoff_ms = 10 * u64::pow(5, attempt);
                        tracing::warn!(
                            key = %key,
                            error = %e,
                            attempt = attempt + 1,
                            max_retries = max_retries,
                            backoff_ms = backoff_ms,
                            "Redis L2 cache delete failed, retrying"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    }
                }
            }
        }

        Ok(())
    }

    /// Get multiple users at once
    ///
    /// More efficient than calling `get()` multiple times.
    /// Returns a map of `user_id` -> `CachedUser`.
    ///
    /// # Performance
    /// - L1 (Moka): Sequential lookup is optimal for in-memory cache (no I/O bottleneck)
    /// - L2 (Redis): Uses pipeline for true batch operation (single round-trip)
    pub async fn get_batch(&self, user_ids: &[UserId]) -> Result<std::collections::HashMap<UserId, CachedUser>> {
        let mut result = std::collections::HashMap::new();
        let mut missing_ids = Vec::new();

        // Check L1 cache first (sequential is optimal for in-memory operations)
        // Note: Parallel lookup would add overhead without benefit for memory cache
        for user_id in user_ids {
            if let Some(user) = self.l1_cache.get(user_id).await {
                result.insert(user_id.clone(), user);
            } else {
                missing_ids.push(user_id.clone());
            }
        }

        // Check L2 cache for missing IDs
        if !missing_ids.is_empty() {
            if let Some(ref conn) = self.redis_conn {
                let mut conn = conn.clone();

                let mut pipe = redis::pipe();
                for user_id in &missing_ids {
                    let key = format!("{}{}", self.key_prefix, user_id.as_str());
                    pipe.get(&key);
                }

                let user_jsons: Vec<Option<String>> = pipe
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| Error::Internal(format!("Failed to batch get users: {e}")))?;

                // Update L1 cache and result
                for (user_id, user_json_opt) in missing_ids.iter().zip(user_jsons) {
                    if let Some(json) = user_json_opt {
                        if let Ok(user) = serde_json::from_str::<CachedUser>(&json) {
                            result.insert(user_id.clone(), user.clone());
                            self.l1_cache.insert(user_id.clone(), user).await;
                        }
                    }
                }
            }
        }

        tracing::debug!(
            total = user_ids.len(),
            found = result.len(),
            "Batch user lookup"
        );

        Ok(result)
    }

    /// Invalidate a specific user's cache entry by ID string (both L1 and L2)
    ///
    /// Used by the cross-replica invalidation listener to remove a single
    /// entry from the local in-memory cache and L2 Redis cache.
    /// L1 is cleared first so this replica stops serving stale data immediately,
    /// then L2 is cleared so other replicas don't re-populate from stale Redis data.
    /// An idempotent Redis DEL is safe even if the originating replica already
    /// cleared L2.
    pub async fn invalidate_by_id(&self, user_id: &str) {
        // Remove from L1 (in-memory) FIRST
        let id = UserId::from_string(user_id.to_string());
        self.l1_cache.invalidate(&id).await;

        // Then remove from L2 (Redis) with retry
        if self.redis_conn.is_some() {
            let key = format!("{}{}", self.key_prefix, user_id);
            // Use best-effort retry for cross-replica invalidation
            // Don't panic if Redis is temporarily unavailable
            if let Err(e) = self.delete_from_redis_with_retry(&key, 2).await {
                crate::metrics::cache::CACHE_ERRORS
                    .with_label_values(&["user", "cross_replica_invalidate"])
                    .inc();
                tracing::error!(
                    user_id = %user_id,
                    error = %e,
                    "Failed to delete user L2 cache during cross-replica invalidation after retries"
                );
            }
        }

        tracing::debug!(user_id = %user_id, "User cache invalidated by id (cross-replica, L1 then L2)");
    }

    /// Clear L1 cache (memory only)
    ///
    /// Useful for testing or manual cache clearing.
    /// Note: L2 cache is not cleared.
    pub async fn clear_l1(&self) {
        self.l1_cache.invalidate_all();
        tracing::debug!("L1 user cache cleared");
    }
}

impl std::fmt::Debug for UserCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserCache")
            .field("redis_enabled", &self.redis_conn.is_some())
            .field("l2_ttl_seconds", &self.l2_ttl_seconds)
            .field("key_prefix", &self.key_prefix)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_user_id(id: &str) -> UserId {
        UserId::from_string(id.to_string())
    }

    fn create_test_user(id: &str, username: &str) -> CachedUser {
        CachedUser {
            id: id.to_string(),
            username: username.to_string(),
            role: "user".to_string(),
            status: "active".to_string(),
            created_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_l1_cache_only() {
        let cache = UserCache::new(None, 100, 5, 0, "test:".to_string()).unwrap();

        let user_id = create_test_user_id("user1");
        let user = create_test_user("user1", "alice");

        // Cache miss
        assert!(cache.get(&user_id).await.unwrap().is_none());

        // Set and get
        cache.set(&user_id, user.clone()).await.unwrap();
        let retrieved = cache.get(&user_id).await.unwrap().unwrap();
        assert_eq!(retrieved.username, "alice");

        // Invalidate
        cache.invalidate(&user_id).await.unwrap();
        assert!(cache.get(&user_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_batch_lookup() {
        let cache = UserCache::new(None, 100, 5, 0, "test:".to_string()).unwrap();

        let user1 = create_test_user_id("user1");
        let user2 = create_test_user_id("user2");
        let user3 = create_test_user_id("user3");

        // Set some entries
        cache.set(&user1, create_test_user("user1", "alice")).await.unwrap();
        cache.set(&user3, create_test_user("user3", "charlie")).await.unwrap();

        // Batch lookup
        let result = cache
            .get_batch(&[user1.clone(), user2.clone(), user3.clone()])
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result.get(&user1).map(|u| &u.username), Some(&"alice".to_string()));
        assert_eq!(result.get(&user2), None);
        assert_eq!(result.get(&user3).map(|u| &u.username), Some(&"charlie".to_string()));
    }
}
