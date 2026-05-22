//! Playback state cache (L1: Moka in-memory, L2: Redis)
//!
//! Provides fast access to playback state data with a two-tier caching strategy:
//! - L1: In-memory Moka cache (very fast, local to the node)
//! - L2: Redis cache (fast, shared across nodes)
//!
//! Built on the generic `TieredCache<K, V>` infrastructure.
//!
//! # Cross-Replica Consistency
//!
//! Playback state changes frequently. The L2 cache provides a fallback when
//! PubSub invalidation messages are lost or delayed. The `version` field
//! in `RoomPlaybackState` is used to detect and reject stale overwrites.

use std::sync::Arc;

use crate::cache::l2_backend::CacheL2Backend;
use crate::cache::tiered::{TieredCache, Timestamped, Versioned};
use crate::models::{RoomId, RoomPlaybackState};
use crate::Result;

// RoomId's CacheKey impl is defined once in room_cache.rs to avoid duplicate impl errors.

impl Timestamped for RoomPlaybackState {
    fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }
}

impl Versioned for RoomPlaybackState {
    fn cache_version(&self) -> i64 {
        self.version
    }
}

/// Playback state cache with L1 (Moka) + L2 (Redis) strategy
#[derive(Clone)]
pub struct PlaybackStateCache {
    inner: TieredCache<RoomId, RoomPlaybackState>,
}

impl PlaybackStateCache {
    /// Create a new `PlaybackStateCache`
    ///
    /// # Arguments
    /// * `l2` - L2 cache backend (e.g. `RedisCacheL2` or `NoopCacheL2`)
    /// * `l1_max_capacity` - Maximum number of entries in L1 cache
    /// * `l1_ttl_seconds` - TTL for L1 cache entries in seconds (short for playback)
    /// * `l2_ttl_seconds` - TTL for L2 cache entries in seconds
    /// * `key_prefix` - L2 key prefix (e.g., "synctv:playback:")
    pub fn new(
        l2: Arc<dyn CacheL2Backend>,
        l1_max_capacity: u64,
        l1_ttl_seconds: u64,
        l2_ttl_seconds: u64,
        key_prefix: String,
    ) -> Result<Self> {
        let inner = TieredCache::new(
            l2,
            l1_max_capacity,
            l1_ttl_seconds,
            l2_ttl_seconds,
            key_prefix,
            "playback".to_string(),
        )?;
        Ok(Self { inner })
    }

    /// Get playback state from cache
    ///
    /// Checks L1 first, then L2. Returns None if not found in either cache.
    pub async fn get(&self, room_id: &RoomId) -> Result<Option<RoomPlaybackState>> {
        self.inner.get(room_id).await
    }

    pub async fn get_l1(&self, room_id: &RoomId) -> Option<RoomPlaybackState> {
        self.inner.get_l1(room_id).await
    }

    pub async fn get_l2(&self, room_id: &RoomId) -> Result<Option<RoomPlaybackState>> {
        self.inner.get_l2(room_id).await
    }

    /// Set playback state in cache
    ///
    /// Updates both L1 and L2 caches.
    pub async fn set(&self, room_id: &RoomId, state: RoomPlaybackState) -> Result<()> {
        self.inner.set(room_id, state).await
    }

    /// Set playback state in cache only if it's newer than existing data
    ///
    /// Compares `updated_at` timestamps and only updates if the new data is newer.
    /// This prevents race conditions where stale data overwrites fresh data.
    pub async fn set_if_newer(&self, room_id: &RoomId, state: RoomPlaybackState) -> Result<bool> {
        self.inner.set_if_newer(room_id, state).await
    }

    /// Set playback state using the optimistic-lock version as the freshness
    /// token. This is the strong-consistency write path used with version fences.
    pub async fn set_if_version_at_least(
        &self,
        room_id: &RoomId,
        state: RoomPlaybackState,
    ) -> Result<bool> {
        self.inner.set_if_version_at_least(room_id, state).await
    }

    /// Invalidate playback state from cache
    ///
    /// Removes from both L1 and L2 caches.
    pub async fn invalidate(&self, room_id: &RoomId) -> Result<()> {
        self.inner.invalidate(room_id).await
    }

    /// Invalidate a specific room's playback cache entry by ID string (both L1 and L2)
    ///
    /// Used by the cross-replica invalidation listener to remove a single
    /// entry from the local in-memory cache and L2 Redis cache.
    pub async fn invalidate_by_id(&self, room_id: &str) -> Result<()> {
        self.inner.invalidate_by_id(room_id).await
    }

    /// Clear L1 cache (memory only)
    ///
    /// Useful for testing or manual cache clearing.
    /// Note: L2 cache is not cleared.
    pub fn clear_l1(&self) {
        self.inner.clear_l1();
    }

    /// Clear both L1 (in-memory) and L2 (Redis) caches.
    ///
    /// Used during lag-triggered full flushes to prevent stale L2 entries from
    /// re-populating L1 on this or other replicas.
    pub async fn clear(&self) {
        self.inner.clear().await;
    }
}

impl std::fmt::Debug for PlaybackStateCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackStateCache")
            .field("inner", &self.inner)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn create_test_room_id(id: &str) -> RoomId {
        RoomId::expect_positive(match id {
            "r1" => 1,
            "r2" => 2,
            _ => 3,
        })
    }

    fn create_test_state(id: &str) -> RoomPlaybackState {
        RoomPlaybackState::new(create_test_room_id(id))
    }

    #[tokio::test]
    async fn test_l1_cache_only() {
        let cache = PlaybackStateCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:".to_string(),
        )
        .unwrap();

        let room_id = create_test_room_id("room1");
        let state = create_test_state("room1");

        // Cache miss
        assert!(cache.get(&room_id).await.unwrap().is_none());

        // Set and get
        cache.set(&room_id, state.clone()).await.unwrap();
        let retrieved = cache.get(&room_id).await.unwrap().unwrap();
        assert_eq!(retrieved.room_id, state.room_id);

        // Invalidate
        cache.invalidate(&room_id).await.unwrap();
        assert!(cache.get(&room_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_set_if_newer() {
        let cache = PlaybackStateCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:".to_string(),
        )
        .unwrap();

        let room_id = create_test_room_id("room1");
        let mut state1 = create_test_state("room1");
        state1.version = 5;
        state1.updated_at = chrono::Utc::now();

        let mut state2 = create_test_state("room1");
        state2.version = 10;
        state2.updated_at = chrono::Utc::now() + chrono::Duration::seconds(10);

        // Set initial state
        cache.set(&room_id, state1.clone()).await.unwrap();

        // Try to set older state - should be rejected
        let older_state = {
            let mut s = state1.clone();
            s.version = 3;
            s.updated_at = chrono::Utc::now() - chrono::Duration::seconds(10);
            s
        };
        let was_set = cache.set_if_newer(&room_id, older_state).await.unwrap();
        assert!(!was_set, "Older state should be rejected");

        // Set newer state - should succeed
        let was_set = cache.set_if_newer(&room_id, state2.clone()).await.unwrap();
        assert!(was_set, "Newer state should be accepted");

        // Verify cache has the newer state
        let retrieved = cache.get(&room_id).await.unwrap().unwrap();
        assert_eq!(retrieved.version, 10);
    }

    #[tokio::test]
    async fn test_set_if_version_at_least_rejects_lower_version() {
        let cache = PlaybackStateCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:".to_string(),
        )
        .unwrap();

        let room_id = create_test_room_id("room1");
        let mut state1 = create_test_state("room1");
        state1.version = 10;
        cache
            .set_if_version_at_least(&room_id, state1.clone())
            .await
            .unwrap();

        let mut older_state = state1.clone();
        older_state.version = 9;
        older_state.updated_at = chrono::Utc::now() + chrono::Duration::seconds(60);

        let was_set = cache
            .set_if_version_at_least(&room_id, older_state)
            .await
            .unwrap();
        assert!(!was_set, "lower version must be rejected");

        let retrieved = cache.get(&room_id).await.unwrap().unwrap();
        assert_eq!(retrieved.version, 10);
    }
}
