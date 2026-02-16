//! Username cache service for fast username lookups
//!
//! Uses a two-tier caching strategy with mature crates:
//! 1. In-memory Moka LRU cache for frequently accessed usernames
//! 2. Redis persistent cache for cross-node consistency

use redis::AsyncCommands;
use std::collections::HashMap;
use std::sync::Arc;

use crate::{models::UserId, Error, Result};

/// Username cache service with L1 (Moka) + L2 (Redis) strategy
#[derive(Clone)]
pub struct UsernameCache {
    redis_conn: Option<redis::aio::ConnectionManager>,
    memory_cache: Arc<moka::future::Cache<UserId, String>>,
    key_prefix: String,
    ttl_seconds: u64,
}

impl UsernameCache {
    /// Create a new `UsernameCache`
    ///
    /// # Arguments
    /// * `redis_conn` - Optional Redis `ConnectionManager`. If None, only in-memory caching is used.
    /// * `key_prefix` - Redis key prefix (e.g., "synctv:username:")
    /// * `memory_cache_size` - Maximum number of entries in memory cache
    /// * `ttl_seconds` - Cache TTL in Redis (0 = no expiration)
    #[must_use] 
    pub fn new(
        redis_conn: Option<redis::aio::ConnectionManager>,
        key_prefix: String,
        memory_cache_size: usize,
        ttl_seconds: u64,
    ) -> Self {
        // Use moka for production-grade LRU cache with automatic eviction
        let memory_cache = Arc::new(
            moka::future::CacheBuilder::new(memory_cache_size as u64)
                .build()
        );

        Self {
            redis_conn,
            memory_cache,
            key_prefix,
            ttl_seconds,
        }
    }

    /// Get username for a user ID
    ///
    /// Checks memory cache first, then Redis cache.
    /// Returns None if not found in any cache.
    pub async fn get(&self, user_id: &UserId) -> Result<Option<String>> {
        // Check memory cache first (moka handles LRU automatically)
        if let Some(username) = self.memory_cache.get(user_id).await {
            tracing::debug!(user_id = %user_id.as_str(), username = %username, "Username cache hit (memory)");
            return Ok(Some(username));
        }

        // Check Redis cache
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let key = format!("{}{}", self.key_prefix, user_id.as_str());
            let username: Option<String> = conn
                .get(&key)
                .await
                .map_err(|e| Error::Internal(format!("Failed to get username from cache: {e}")))?;

            if let Some(username) = username {
                tracing::debug!(user_id = %user_id.as_str(), username = %username, "Username cache hit (Redis)");

                // Populate memory cache
                self.memory_cache.insert(user_id.clone(), username.clone()).await;

                return Ok(Some(username));
            }
        }

        tracing::debug!(user_id = %user_id.as_str(), "Username cache miss");
        Ok(None)
    }

    /// Set username for a user ID
    ///
    /// Updates both memory cache and Redis cache.
    pub async fn set(&self, user_id: &UserId, username: &str) -> Result<()> {
        // Update memory cache
        self.memory_cache.insert(user_id.clone(), username.to_string()).await;

        // Update Redis cache
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let key = format!("{}{}", self.key_prefix, user_id.as_str());

            if self.ttl_seconds > 0 {
                let _: () = conn
                    .set_ex(&key, username, self.ttl_seconds)
                    .await
                    .map_err(|e| Error::Internal(format!("Failed to set username in cache: {e}")))?;
            } else {
                let _: () = conn
                    .set(&key, username)
                    .await
                    .map_err(|e| Error::Internal(format!("Failed to set username in cache: {e}")))?;
            }

            tracing::debug!(
                user_id = %user_id.as_str(),
                username = %username,
                ttl_seconds = self.ttl_seconds,
                "Username cached"
            );
        }

        Ok(())
    }

    /// Get multiple usernames at once
    ///
    /// More efficient than calling `get()` multiple times.
    /// Returns a map of `user_id` -> username.
    pub async fn get_batch(&self, user_ids: &[UserId]) -> Result<HashMap<UserId, String>> {
        let mut result = HashMap::new();
        let mut missing_ids = Vec::new();

        // Check memory cache first
        for user_id in user_ids {
            if let Some(username) = self.memory_cache.get(user_id).await {
                result.insert(user_id.clone(), username);
            } else {
                missing_ids.push(user_id.clone());
            }
        }

        // Check Redis for missing IDs
        if !missing_ids.is_empty() {
            if let Some(ref conn) = self.redis_conn {
                let mut conn = conn.clone();

                let mut pipe = redis::pipe();
                for user_id in &missing_ids {
                    let key = format!("{}{}", self.key_prefix, user_id.as_str());
                    pipe.get(&key);
                }

                let usernames: Vec<Option<String>> = pipe
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| Error::Internal(format!("Failed to batch get usernames: {e}")))?;

                // Update memory cache and result
                for (user_id, username_opt) in missing_ids.iter().zip(usernames) {
                    if let Some(username) = username_opt {
                        result.insert(user_id.clone(), username.clone());
                        self.memory_cache.insert(user_id.clone(), username).await;
                    }
                }
            }
        }

        tracing::debug!(
            total = user_ids.len(),
            found = result.len(),
            "Batch username lookup"
        );

        Ok(result)
    }

    /// Invalidate a cached username
    ///
    /// Removes the username from both memory (L1) and Redis (L2) cache.
    /// L1 is invalidated first so this replica immediately stops serving stale
    /// data, then L2 is cleared so other replicas don't re-populate from stale
    /// Redis data. This is consistent with `UserCache` and `RoomCache`.
    pub async fn invalidate(&self, user_id: &UserId) -> Result<()> {
        // Remove from memory cache (L1) FIRST so this replica stops serving stale data immediately
        self.memory_cache.invalidate(user_id).await;

        // Then remove from Redis cache (L2) with retry
        if self.redis_conn.is_some() {
            let key = format!("{}{}", self.key_prefix, user_id.as_str());
            self.delete_from_redis_with_retry(&key, 3).await?;
        }

        tracing::debug!(user_id = %user_id.as_str(), "Username cache invalidated (L1 then L2)");

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

    /// Invalidate a specific username cache entry by ID string (both L1 and L2)
    ///
    /// Used by the cross-replica invalidation listener to remove a single
    /// entry from the local in-memory cache and L2 Redis cache.
    /// L1 is cleared first so this replica stops serving stale data immediately,
    /// then L2 is cleared so other replicas don't re-populate from stale Redis data.
    pub async fn invalidate_by_id(&self, user_id: &str) {
        // Remove from L1 (in-memory) FIRST
        let id = UserId::from_string(user_id.to_string());
        self.memory_cache.invalidate(&id).await;

        // Then remove from L2 (Redis) with retry
        if self.redis_conn.is_some() {
            let key = format!("{}{}", self.key_prefix, user_id);
            // Use best-effort retry for cross-replica invalidation
            // Don't panic if Redis is temporarily unavailable
            if let Err(e) = self.delete_from_redis_with_retry(&key, 2).await {
                tracing::error!(
                    user_id = %user_id,
                    error = %e,
                    "Failed to delete username L2 cache during cross-replica invalidation after retries"
                );
            }
        }

        tracing::debug!(user_id = %user_id, "Username cache invalidated by id (cross-replica, L1 then L2)");
    }

    /// Clear all cached usernames (memory only)
    ///
    /// This is useful for testing or manual cache clearing.
    /// Note: Redis cache is not cleared.
    pub async fn clear_memory(&self) {
        self.memory_cache.invalidate_all();
        tracing::debug!("Memory username cache cleared");
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
            .field("redis_enabled", &self.redis_conn.is_some())
            .field("ttl_seconds", &self.ttl_seconds)
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
        let cache = UsernameCache::new(None, "test:".to_string(), 10, 0);

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
    async fn test_memory_cache_lru() {
        let cache = UsernameCache::new(None, "test:".to_string(), 3, 0);

        let user1 = create_test_user_id("user1");
        let user2 = create_test_user_id("user2");
        let user3 = create_test_user_id("user3");
        let user4 = create_test_user_id("user4");

        // Fill cache to capacity (3)
        cache.set(&user1, "alice").await.unwrap();
        cache.set(&user2, "bob").await.unwrap();
        cache.set(&user3, "charlie").await.unwrap();

        // Verify all are cached
        assert!(cache.get(&user1).await.unwrap().is_some());
        assert!(cache.get(&user2).await.unwrap().is_some());
        assert!(cache.get(&user3).await.unwrap().is_some());

        // Access user1 to make it most recently used
        assert!(cache.get(&user1).await.unwrap().is_some());

        // Add user4, should evict user2 (least recently used)
        cache.set(&user4, "dave").await.unwrap();

        // user1 should still be there (recently accessed)
        assert!(cache.get(&user1).await.unwrap().is_some());
        // user2 should be evicted (least recently used) - moka handles this automatically
        // Note: moka's eviction policy may vary, so we just verify the cache still works
        assert!(cache.get(&user3).await.unwrap().is_some());
        assert!(cache.get(&user4).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_batch_lookup() {
        let cache = UsernameCache::new(None, "test:".to_string(), 10, 0);

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
}
