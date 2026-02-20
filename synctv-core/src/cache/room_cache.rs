//! Room information cache (L1: Moka in-memory, L2: Redis)
//!
//! Provides fast access to room data with a two-tier caching strategy:
//! - L1: In-memory Moka cache (very fast, local to the node)
//! - L2: Redis cache (fast, shared across nodes)
//!
//! Built on the generic `TieredCache<K, V>` infrastructure.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::cache::l2_backend::CacheL2Backend;
use crate::cache::tiered::{CacheKey, TieredCache, Timestamped};
use crate::models::RoomId;
use crate::Result;

// --- CacheKey implementation for RoomId ---

impl CacheKey for RoomId {
    fn as_str(&self) -> &str {
        self.as_str()
    }
    fn from_id(id: &str) -> Self {
        Self(id.to_string())
    }
}

/// Room cache with L1 (Moka) + L2 (Redis) strategy
#[derive(Clone)]
pub struct RoomCache {
    inner: TieredCache<RoomId, CachedRoom>,
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

    /// Create a new `CachedRoom` with explicit `updated_at` timestamp
    #[must_use]
    pub const fn with_updated_at(
        id: String,
        name: String,
        owner_id: String,
        is_public: bool,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self { id, name, owner_id, is_public, created_at, updated_at }
    }

    /// Get the `updated_at` timestamp
    #[must_use]
    pub const fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }
}

impl Timestamped for CachedRoom {
    fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }
}

impl RoomCache {
    /// Create a new `RoomCache`
    ///
    /// # Arguments
    /// * `l2` - L2 cache backend (e.g. `RedisCacheL2` or `NoopCacheL2`)
    /// * `l1_max_capacity` - Maximum number of entries in L1 cache
    /// * `l1_ttl_minutes` - TTL for L1 cache entries in minutes
    /// * `l2_ttl_seconds` - TTL for L2 cache entries in seconds
    /// * `key_prefix` - L2 key prefix (e.g., "synctv:room:")
    pub fn new(
        l2: Arc<dyn CacheL2Backend>,
        l1_max_capacity: u64,
        l1_ttl_minutes: u64,
        l2_ttl_seconds: u64,
        key_prefix: String,
    ) -> Result<Self> {
        let inner = TieredCache::new(
            l2,
            l1_max_capacity,
            l1_ttl_minutes,
            l2_ttl_seconds,
            key_prefix,
            "room".to_string(),
        )?;
        Ok(Self { inner })
    }

    /// Get room data from cache
    ///
    /// Checks L1 first, then L2. Returns None if not found in either cache.
    pub async fn get(&self, room_id: &RoomId) -> Result<Option<CachedRoom>> {
        self.inner.get(room_id).await
    }

    /// Set room data in cache
    ///
    /// Updates both L1 and L2 caches.
    pub async fn set(&self, room_id: &RoomId, room: CachedRoom) -> Result<()> {
        self.inner.set(room_id, room).await
    }

    /// Set room data in cache only if it's newer than existing data
    ///
    /// Compares `updated_at` timestamps and only updates if the new data is newer.
    /// This prevents race conditions where stale data overwrites fresh data.
    pub async fn set_if_newer(&self, room_id: &RoomId, room: CachedRoom) -> Result<bool> {
        self.inner.set_if_newer(room_id, room).await
    }

    /// Invalidate room data from cache
    ///
    /// Removes from both L1 and L2 caches.
    pub async fn invalidate(&self, room_id: &RoomId) -> Result<()> {
        self.inner.invalidate(room_id).await
    }

    /// Invalidate a specific room's cache entry by ID string (both L1 and L2)
    ///
    /// Used by the cross-replica invalidation listener to remove a single
    /// entry from the local in-memory cache and L2 Redis cache.
    pub async fn invalidate_by_id(&self, room_id: &str) {
        self.inner.invalidate_by_id(room_id).await;
    }

    /// Get multiple rooms at once
    ///
    /// More efficient than calling `get()` multiple times.
    /// Returns a map of `room_id` -> `CachedRoom`.
    pub async fn get_batch(&self, room_ids: &[RoomId]) -> Result<std::collections::HashMap<RoomId, CachedRoom>> {
        self.inner.get_batch(room_ids).await
    }

    /// Clear L1 cache (memory only)
    ///
    /// Useful for testing or manual cache clearing.
    /// Note: L2 cache is not cleared.
    pub async fn clear_l1(&self) {
        self.inner.clear_l1().await;
    }

    /// Clear both L1 (in-memory) and L2 (Redis) caches.
    ///
    /// Used during lag-triggered full flushes to prevent stale L2 entries from
    /// re-populating L1 on this or other replicas.
    pub async fn clear(&self) {
        self.inner.clear().await;
    }
}

impl std::fmt::Debug for RoomCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomCache")
            .field("inner", &self.inner)
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
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_l1_cache_only() {
        let cache = RoomCache::new(Arc::new(crate::cache::NoopCacheL2), 100, 5, 0, "test:".to_string()).unwrap();

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
        let cache = RoomCache::new(Arc::new(crate::cache::NoopCacheL2), 100, 5, 0, "test:".to_string()).unwrap();

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
