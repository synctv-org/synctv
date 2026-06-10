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
use crate::models::room::RoomStatus;
use crate::models::RoomId;
use crate::Result;

impl CacheKey for RoomId {
    fn cache_key(&self) -> String {
        self.to_string()
    }
    fn try_from_id(id: &str) -> Result<Self> {
        id.parse()
            .map_err(|_| crate::Error::InvalidInput(format!("Invalid room cache key: {id}")))
    }
}

/// Room cache with L1 (Moka) + L2 (Redis) strategy
#[derive(Clone)]
pub struct RoomCache {
    inner: TieredCache<RoomId, CachedRoom>,
}

/// Cached room data
///
/// Mirrors the fields of [`crate::models::Room`] that are needed for cache
/// lookups and access-control checks. Fields like `description` and `version`
/// are intentionally omitted because they are not used in hot-path lookups.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedRoom {
    id: String,
    name: String,
    owner_id: String,
    is_public: bool,
    /// Room lifecycle status.
    status: RoomStatus,
    /// Ban flag - independent of status
    is_banned: bool,
    /// Soft-delete timestamp (None if the room is not deleted)
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
    /// Timestamp of last update - used to prevent stale data from overwriting fresh data
    updated_at: chrono::DateTime<chrono::Utc>,
}

pub struct CachedRoomSnapshot {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    pub is_public: bool,
    pub status: RoomStatus,
    pub is_banned: bool,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl CachedRoom {
    /// Create a new `CachedRoom` with default status (Active), not banned, not deleted.
    ///
    /// This is a convenience constructor for active, visible room snapshots.
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
            status: RoomStatus::Active,
            is_banned: false,
            deleted_at: None,
            created_at,
            updated_at: chrono::Utc::now(),
        }
    }

    /// Create a new `CachedRoom` with explicit `updated_at` timestamp
    ///
    /// Uses default status (Active), not banned, not deleted.
    #[must_use]
    pub const fn with_updated_at(
        id: String,
        name: String,
        owner_id: String,
        is_public: bool,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            id,
            name,
            owner_id,
            is_public,
            status: RoomStatus::Active,
            is_banned: false,
            deleted_at: None,
            created_at,
            updated_at,
        }
    }

    #[must_use]
    pub fn from_snapshot(snapshot: CachedRoomSnapshot) -> Self {
        Self {
            id: snapshot.id,
            name: snapshot.name,
            owner_id: snapshot.owner_id,
            is_public: snapshot.is_public,
            status: snapshot.status,
            is_banned: snapshot.is_banned,
            deleted_at: snapshot.deleted_at,
            created_at: snapshot.created_at,
            updated_at: snapshot.updated_at,
        }
    }

    /// Get the room ID
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the room name
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the owner (creator) user ID
    #[must_use]
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// Whether the room is publicly listed
    #[must_use]
    pub const fn is_public(&self) -> bool {
        self.is_public
    }

    /// Get the room lifecycle status
    #[must_use]
    pub const fn status(&self) -> RoomStatus {
        self.status
    }

    /// Whether the room is banned
    #[must_use]
    pub const fn is_banned(&self) -> bool {
        self.is_banned
    }

    /// Get the soft-delete timestamp (None if not deleted)
    #[must_use]
    pub const fn deleted_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.deleted_at
    }

    /// Get the `created_at` timestamp
    #[must_use]
    pub const fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.created_at
    }

    /// Get the `updated_at` timestamp
    #[must_use]
    pub const fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }

    /// Check if room is usable (active status, not banned, not deleted)
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == RoomStatus::Active && !self.is_banned && self.deleted_at.is_none()
    }
}

/// Convert a `Room` model to a `CachedRoom`.
///
/// `is_public` defaults to `false` because the `Room` model does not have
/// an `is_public` field -- it is determined by room settings. Callers with
/// a complete room/settings snapshot should use [`CachedRoom::from_snapshot`].
impl From<&crate::models::Room> for CachedRoom {
    fn from(room: &crate::models::Room) -> Self {
        Self {
            id: room.id.to_string(),
            name: room.name.clone(),
            owner_id: room.created_by.to_string(),
            is_public: false, // determined by room settings, not the Room model
            status: room.status,
            is_banned: room.is_banned,
            deleted_at: room.deleted_at,
            created_at: room.created_at,
            updated_at: room.updated_at,
        }
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
    /// * `l1_ttl_seconds` - TTL for L1 cache entries in seconds
    /// * `l2_ttl_seconds` - TTL for L2 cache entries in seconds
    /// * `key_prefix` - L2 key prefix (e.g., "synctv:room:")
    pub fn new(
        l2: Arc<dyn CacheL2Backend>,
        l1_max_capacity: u64,
        l1_ttl_seconds: u64,
        l2_ttl_seconds: u64,
        key_prefix: String,
    ) -> Self {
        let inner = TieredCache::new(
            l2,
            l1_max_capacity,
            l1_ttl_seconds,
            l2_ttl_seconds,
            key_prefix,
            "room".to_string(),
        );
        Self { inner }
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
    pub async fn invalidate_by_id(&self, room_id: &str) -> Result<()> {
        self.inner.invalidate_by_id(room_id).await
    }

    /// Get multiple rooms at once
    ///
    /// More efficient than calling `get()` multiple times.
    /// Returns a map of `room_id` -> `CachedRoom`.
    pub async fn get_batch(
        &self,
        room_ids: &[RoomId],
    ) -> Result<std::collections::HashMap<RoomId, CachedRoom>> {
        self.inner.get_batch(room_ids).await
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
    use crate::test_helpers::{TestOptionExt, TestResultExt};

    fn create_test_room_id(id: &str) -> RoomId {
        RoomId::expect_positive(match id {
            "r1" => 1,
            "r2" => 2,
            "room1" => 11,
            "room2" => 12,
            "room3" => 13,
            "room_closed" => 14,
            "room_banned" => 15,
            "room_deleted" => 16,
            "r_from_room" => 3,
            _ => 4,
        })
    }

    fn create_test_room(id: &str, name: &str, owner_id: &str) -> CachedRoom {
        CachedRoom::new(
            id.to_string(),
            name.to_string(),
            owner_id.to_string(),
            true,
            chrono::Utc::now(),
        )
    }

    #[tokio::test]
    async fn test_l1_cache_only() {
        let cache = RoomCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:".to_string(),
        );

        let room_id = create_test_room_id("room1");
        let room = create_test_room("room1", "Test Room", "user1");

        // Cache miss
        assert!(cache
            .get(&room_id)
            .await
            .checked("operation should succeed")
            .is_none());

        // Set and get
        cache
            .set(&room_id, room.clone())
            .await
            .checked("operation should succeed");
        let retrieved = cache
            .get(&room_id)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(retrieved.name(), "Test Room");

        // Invalidate
        cache
            .invalidate(&room_id)
            .await
            .checked("operation should succeed");
        assert!(cache
            .get(&room_id)
            .await
            .checked("operation should succeed")
            .is_none());
    }

    #[tokio::test]
    async fn test_invalidate_by_id_rejects_malformed_id() {
        let cache = RoomCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:".to_string(),
        );

        let result = cache.invalidate_by_id("not-a-room-id").await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_batch_lookup() {
        let cache = RoomCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:".to_string(),
        );

        let room1 = create_test_room_id("room1");
        let room2 = create_test_room_id("room2");
        let room3 = create_test_room_id("room3");

        // Set some entries
        cache
            .set(&room1, create_test_room("room1", "Room 1", "user1"))
            .await
            .checked("operation should succeed");
        cache
            .set(&room3, create_test_room("room3", "Room 3", "user1"))
            .await
            .checked("operation should succeed");

        // Batch lookup
        let result = cache
            .get_batch(&[room1, room2, room3])
            .await
            .checked("operation should succeed");

        assert_eq!(result.len(), 2);
        assert_eq!(
            result.get(&room1).map(super::CachedRoom::name),
            Some("Room 1")
        );
        assert_eq!(result.get(&room2), None);
        assert_eq!(
            result.get(&room3).map(super::CachedRoom::name),
            Some("Room 3")
        );
    }

    /// CachedRoom must include status field from the Room model
    #[tokio::test]
    async fn test_cached_room_status_field() {
        use crate::models::RoomStatus;

        let cache = RoomCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:status:".to_string(),
        );

        let room_id = create_test_room_id("room_closed");
        let now = chrono::Utc::now();
        let room = CachedRoom::from_snapshot(CachedRoomSnapshot {
            id: "room_closed".to_string(),
            name: "Closed Room".to_string(),
            owner_id: "owner1".to_string(),
            is_public: true,
            status: RoomStatus::Closed,
            is_banned: false,
            deleted_at: None,
            created_at: now,
            updated_at: now,
        });

        cache
            .set(&room_id, room)
            .await
            .checked("operation should succeed");
        let retrieved = cache
            .get(&room_id)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(retrieved.status(), RoomStatus::Closed);
        assert_eq!(retrieved.name(), "Closed Room");
    }

    /// CachedRoom must include is_banned field from the Room model
    #[tokio::test]
    async fn test_cached_room_is_banned_field() {
        use crate::models::RoomStatus;

        let cache = RoomCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:banned:".to_string(),
        );

        let room_id = create_test_room_id("room_banned");
        let now = chrono::Utc::now();
        let room = CachedRoom::from_snapshot(CachedRoomSnapshot {
            id: "room_banned".to_string(),
            name: "Banned Room".to_string(),
            owner_id: "owner1".to_string(),
            is_public: true,
            status: RoomStatus::Active,
            is_banned: true,
            deleted_at: // is_banned
            None,
            created_at: now,
            updated_at: now,
        });

        cache
            .set(&room_id, room)
            .await
            .checked("operation should succeed");
        let retrieved = cache
            .get(&room_id)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert!(
            retrieved.is_banned(),
            "CachedRoom must preserve is_banned=true"
        );
    }

    /// CachedRoom must include deleted_at field from the Room model
    #[tokio::test]
    async fn test_cached_room_deleted_at_field() {
        use crate::models::RoomStatus;

        let cache = RoomCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:deleted:".to_string(),
        );

        let room_id = create_test_room_id("room_deleted");
        let now = chrono::Utc::now();
        let deleted_time = now - chrono::Duration::hours(1);
        let room = CachedRoom::from_snapshot(CachedRoomSnapshot {
            id: "room_deleted".to_string(),
            name: "Deleted Room".to_string(),
            owner_id: "owner1".to_string(),
            is_public: false,
            status: RoomStatus::Active,
            is_banned: false,
            deleted_at: Some(deleted_time),
            created_at: now,
            updated_at: now,
        });

        cache
            .set(&room_id, room)
            .await
            .checked("operation should succeed");
        let retrieved = cache
            .get(&room_id)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert!(
            retrieved.deleted_at().is_some(),
            "CachedRoom must preserve deleted_at"
        );
        assert_eq!(
            retrieved.deleted_at().checked("operation should succeed"),
            deleted_time,
            "CachedRoom must preserve exact deleted_at timestamp"
        );
    }

    /// CachedRoom::from(Room) must correctly populate all fields
    #[tokio::test]
    async fn test_cached_room_from_room_model() {
        use crate::models::UserId;
        use crate::models::{Room, RoomStatus};

        let now = chrono::Utc::now();
        let room = Room {
            id: crate::models::RoomId::expect_positive(3),
            name: "From Room".to_string(),
            description: "A room for testing From impl".to_string(),
            cover_file_reference_id: None,
            created_by: UserId::expect_positive(1),
            status: RoomStatus::Closed,
            is_banned: true,
            closed_at: Some(now),
            created_at: now,
            updated_at: now,
            deleted_at: Some(now),
            version: 5,
            last_activity_at: now,
        };

        let cached: CachedRoom = CachedRoom::from(&room);

        assert_eq!(cached.id(), "3");
        assert_eq!(cached.name(), "From Room");
        assert_eq!(cached.owner_id(), "1");
        assert!(!cached.is_public()); // default false for from()
        assert_eq!(cached.status(), RoomStatus::Closed);
        assert!(cached.is_banned());
        assert_eq!(cached.deleted_at(), Some(now));
        assert_eq!(cached.updated_at(), now);
    }
}
