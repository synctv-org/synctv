//! Room information cache (L1: Moka in-memory, L2: Redis)
//!
//! Provides fast access to room data with a two-tier caching strategy:
//! - L1: In-memory Moka cache (very fast, local to the node)
//! - L2: Redis cache (fast, shared across nodes)

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{models::RoomId, Error, Result};

/// Room cache with L1 (Moka) + L2 (Redis) strategy
#[derive(Clone)]
pub struct RoomCache {
    redis_conn: Option<redis::aio::ConnectionManager>,
    l1_cache: Arc<moka::future::Cache<RoomId, CachedRoom>>,
    l2_ttl_seconds: u64,
    key_prefix: String,
}

/// Cached room data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedRoom {
    id: String,
    name: String,
    owner_id: String,
    is_public: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    /// Timestamp of last update - used to prevent stale data from overwriting fresh data
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl CachedRoom {
    /// Create a new `CachedRoom`
    #[must_use]
    pub fn new(
        id: String,
        name: String,
        owner_id: String,
        is_public: bool,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            id,
            name,
            owner_id,
            is_public,
            created_at,
            updated_at: chrono::Utc::now(),
        }
    }

    /// Create a new `CachedRoom` with explicit updated_at timestamp
    #[must_use]
    pub fn with_updated_at(
        id: String,
        name: String,
        owner_id: String,
        is_public: bool,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self { id, name, owner_id, is_public, created_at, updated_at }
    }

    /// Get the updated_at timestamp
    #[must_use]
    pub const fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }
}

impl RoomCache {
    /// Create a new `RoomCache`
    ///
    /// # Arguments
    /// * `redis_conn` - Optional Redis `ConnectionManager`. If None, only L1 caching is used.
    /// * `l1_max_capacity` - Maximum number of entries in L1 cache
    /// * `l1_ttl_minutes` - TTL for L1 cache entries in minutes
    /// * `l2_ttl_seconds` - TTL for L2 (Redis) cache entries in seconds
    /// * `key_prefix` - Redis key prefix (e.g., "synctv:room:")
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

    /// Get room data from cache
    ///
    /// Checks L1 first, then L2. Returns None if not found in either cache.
    pub async fn get(&self, room_id: &RoomId) -> Result<Option<CachedRoom>> {
        // Check L1 (in-memory) cache first
        if let Some(room) = self.l1_cache.get(room_id).await {
            crate::metrics::cache::CACHE_HITS
                .with_label_values(&["room", "l1"])
                .inc();
            tracing::debug!(
                room_id = %room_id.0,
                "Room cache hit (L1)"
            );
            return Ok(Some(room));
        }

        // Check L2 (Redis) cache
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let key = format!("{}{}", self.key_prefix, room_id.0);
            let room_json: Option<String> = conn
                .get(&key)
                .await
                .map_err(|e| Error::Internal(format!("Failed to get room from cache: {e}")))?;

            if let Some(json) = room_json {
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&["room", "l2"])
                    .inc();
                tracing::debug!(
                    room_id = %room_id.0,
                    "Room cache hit (L2)"
                );

                let room: CachedRoom = serde_json::from_str(&json).map_err(|e| {
                    Error::Internal(format!("Failed to deserialize cached room: {e}"))
                })?;

                // Populate L1 cache
                self.l1_cache.insert(room_id.clone(), room.clone()).await;

                return Ok(Some(room));
            }
        }

        crate::metrics::cache::CACHE_MISSES
            .with_label_values(&["room", "l1"])
            .inc();
        tracing::debug!(room_id = %room_id.0, "Room cache miss");
        Ok(None)
    }

    /// Set room data in cache
    ///
    /// Updates both L1 and L2 caches.
    pub async fn set(&self, room_id: &RoomId, room: CachedRoom) -> Result<()> {
        // Update L1 cache
        self.l1_cache.insert(room_id.clone(), room.clone()).await;

        // Update L2 cache
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let key = format!("{}{}", self.key_prefix, room_id.0);
            let json = serde_json::to_string(&room).map_err(|e| {
                Error::Internal(format!("Failed to serialize room for caching: {e}"))
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
                    .map_err(|e| Error::Internal(format!("Failed to set room in cache: {e}")))?;
            } else {
                let _: () = conn
                    .set(&key, json)
                    .await
                    .map_err(|e| Error::Internal(format!("Failed to set room in cache: {e}")))?;
            }

            tracing::debug!(
                room_id = %room_id.0,
                ttl_seconds = ttl_with_jitter,
                "Room cached"
            );
        }

        Ok(())
    }

    /// Set room data in cache only if it's newer than existing data
    ///
    /// Compares `updated_at` timestamps and only updates if the new data is newer.
    /// This prevents race conditions where stale data overwrites fresh data.
    pub async fn set_if_newer(&self, room_id: &RoomId, room: CachedRoom) -> Result<bool> {
        // Check L1 cache first
        if let Some(existing) = self.l1_cache.get(room_id).await {
            if room.updated_at <= existing.updated_at {
                tracing::debug!(
                    room_id = %room_id.0,
                    existing_ts = %existing.updated_at,
                    new_ts = %room.updated_at,
                    "Skipping cache update - data is not newer"
                );
                return Ok(false);
            }
        }

        // Check L2 cache if L1 miss
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let key = format!("{}{}", self.key_prefix, room_id.0);

            if let Ok(Some(json)) = conn.get::<_, Option<String>>(&key).await {
                if let Ok(existing) = serde_json::from_str::<CachedRoom>(&json) {
                    if room.updated_at <= existing.updated_at {
                        tracing::debug!(
                            room_id = %room_id.0,
                            existing_ts = %existing.updated_at,
                            new_ts = %room.updated_at,
                            "Skipping cache update - L2 data is not newer"
                        );
                        return Ok(false);
                    }
                }
            }
        }

        // Data is newer, perform the update
        self.set(room_id, room).await?;
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

    /// Invalidate room data from cache
    ///
    /// Removes from both L1 and L2 caches.
    /// L1 is invalidated first to ensure this replica immediately stops serving
    /// stale data, then L2 is cleared so other replicas don't re-populate from
    /// stale Redis data.
    pub async fn invalidate(&self, room_id: &RoomId) -> Result<()> {
        // Remove from L1 (in-memory) FIRST so this replica stops serving stale data immediately
        self.l1_cache.invalidate(room_id).await;

        // Then remove from L2 (Redis) with retry logic
        if self.redis_conn.is_some() {
            let key = format!("{}{}", self.key_prefix, room_id.0);
            self.delete_from_redis_with_retry(&key, 3).await?;
        }

        crate::metrics::cache::CACHE_EVICTIONS
            .with_label_values(&["room"])
            .inc();
        tracing::debug!(room_id = %room_id.0, "Room cache invalidated (L1 then L2)");

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
                            .with_label_values(&["room", "l2_delete"])
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

    /// Get multiple rooms at once
    ///
    /// More efficient than calling `get()` multiple times.
    /// Returns a map of `room_id` -> `CachedRoom`.
    ///
    /// # Performance
    /// - L1 (Moka): Sequential lookup is optimal for in-memory cache (no I/O bottleneck)
    /// - L2 (Redis): Uses pipeline for true batch operation (single round-trip)
    pub async fn get_batch(&self, room_ids: &[RoomId]) -> Result<std::collections::HashMap<RoomId, CachedRoom>> {
        let mut result = std::collections::HashMap::new();
        let mut missing_ids = Vec::new();

        // Check L1 cache first (sequential is optimal for in-memory operations)
        // Note: Parallel lookup would add overhead without benefit for memory cache
        for room_id in room_ids {
            if let Some(room) = self.l1_cache.get(room_id).await {
                result.insert(room_id.clone(), room);
            } else {
                missing_ids.push(room_id.clone());
            }
        }

        // Check L2 cache for missing IDs
        if !missing_ids.is_empty() {
            if let Some(ref conn) = self.redis_conn {
                let mut conn = conn.clone();

                let mut pipe = redis::pipe();
                for room_id in &missing_ids {
                    let key = format!("{}{}", self.key_prefix, room_id.0);
                    pipe.get(&key);
                }

                let room_jsons: Vec<Option<String>> = pipe
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| Error::Internal(format!("Failed to batch get rooms: {e}")))?;

                // Update L1 cache and result
                for (room_id, room_json_opt) in missing_ids.iter().zip(room_jsons) {
                    if let Some(json) = room_json_opt {
                        if let Ok(room) = serde_json::from_str::<CachedRoom>(&json) {
                            result.insert(room_id.clone(), room.clone());
                            self.l1_cache.insert(room_id.clone(), room).await;
                        }
                    }
                }
            }
        }

        tracing::debug!(
            total = room_ids.len(),
            found = result.len(),
            "Batch room lookup"
        );

        Ok(result)
    }

    /// Invalidate a specific room's cache entry by ID string (both L1 and L2)
    ///
    /// Used by the cross-replica invalidation listener to remove a single
    /// entry from the local in-memory cache and L2 Redis cache.
    /// L1 is cleared first so this replica stops serving stale data immediately,
    /// then L2 is cleared so other replicas don't re-populate from stale Redis data.
    /// An idempotent Redis DEL is safe even if the originating replica already
    /// cleared L2.
    pub async fn invalidate_by_id(&self, room_id: &str) {
        // Remove from L1 (in-memory) FIRST
        let id = RoomId(room_id.to_string());
        self.l1_cache.invalidate(&id).await;

        // Then remove from L2 (Redis) with retry
        if self.redis_conn.is_some() {
            let key = format!("{}{}", self.key_prefix, room_id);
            // Use best-effort retry for cross-replica invalidation
            // Don't panic if Redis is temporarily unavailable
            if let Err(e) = self.delete_from_redis_with_retry(&key, 2).await {
                crate::metrics::cache::CACHE_ERRORS
                    .with_label_values(&["room", "cross_replica_invalidate"])
                    .inc();
                tracing::error!(
                    room_id = %room_id,
                    error = %e,
                    "Failed to delete room L2 cache during cross-replica invalidation after retries"
                );
            }
        }

        tracing::debug!(room_id = %room_id, "Room cache invalidated by id (cross-replica, L1 then L2)");
    }

    /// Clear L1 cache (memory only)
    ///
    /// Useful for testing or manual cache clearing.
    /// Note: L2 cache is not cleared.
    pub async fn clear_l1(&self) {
        self.l1_cache.invalidate_all();
        tracing::debug!("L1 room cache cleared");
    }
}

impl std::fmt::Debug for RoomCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomCache")
            .field("redis_enabled", &self.redis_conn.is_some())
            .field("l2_ttl_seconds", &self.l2_ttl_seconds)
            .field("key_prefix", &self.key_prefix)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_room_id(id: &str) -> RoomId {
        RoomId(id.to_string())
    }

    fn create_test_room(id: &str, name: &str, owner_id: &str) -> CachedRoom {
        CachedRoom {
            id: id.to_string(),
            name: name.to_string(),
            owner_id: owner_id.to_string(),
            is_public: true,
            created_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_l1_cache_only() {
        let cache = RoomCache::new(None, 100, 5, 0, "test:".to_string()).unwrap();

        let room_id = create_test_room_id("room1");
        let room = create_test_room("room1", "Test Room", "user1");

        // Cache miss
        assert!(cache.get(&room_id).await.unwrap().is_none());

        // Set and get
        cache.set(&room_id, room.clone()).await.unwrap();
        let retrieved = cache.get(&room_id).await.unwrap().unwrap();
        assert_eq!(retrieved.name, "Test Room");

        // Invalidate
        cache.invalidate(&room_id).await.unwrap();
        assert!(cache.get(&room_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_batch_lookup() {
        let cache = RoomCache::new(None, 100, 5, 0, "test:".to_string()).unwrap();

        let room1 = create_test_room_id("room1");
        let room2 = create_test_room_id("room2");
        let room3 = create_test_room_id("room3");

        // Set some entries
        cache.set(&room1, create_test_room("room1", "Room 1", "user1")).await.unwrap();
        cache.set(&room3, create_test_room("room3", "Room 3", "user1")).await.unwrap();

        // Batch lookup
        let result = cache
            .get_batch(&[room1.clone(), room2.clone(), room3.clone()])
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result.get(&room1).map(|r| &r.name), Some(&"Room 1".to_string()));
        assert_eq!(result.get(&room2), None);
        assert_eq!(result.get(&room3).map(|r| &r.name), Some(&"Room 3".to_string()));
    }
}
