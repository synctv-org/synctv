//! User information cache (L1: Moka in-memory, L2: Redis)
//!
//! Provides fast access to user profile data with a two-tier caching strategy:
//! - L1: In-memory Moka cache (very fast, local to the node)
//! - L2: Redis cache (fast, shared across nodes)
//!
//! Built on the generic `TieredCache<K, V>` infrastructure.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::cache::l2_backend::CacheL2Backend;
use crate::cache::tiered::{CacheKey, TieredCache, Timestamped};
use crate::models::{UserId, UserRole, UserStatus};
use crate::Result;

impl CacheKey for UserId {
    fn cache_key(&self) -> String {
        self.to_string()
    }
    fn try_from_id(id: &str) -> Result<Self> {
        id.parse()
            .map_err(|_| crate::Error::InvalidInput(format!("Invalid user cache key: {id}")))
    }
}

/// User cache with L1 (Moka) + L2 (Redis) strategy
#[derive(Clone)]
pub struct UserCache {
    inner: TieredCache<UserId, CachedUser>,
}

/// Cached user data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedUser {
    id: UserId,
    username: String,
    role: UserRole,
    status: UserStatus,
    created_at: chrono::DateTime<chrono::Utc>,
    /// Timestamp of last update - used to prevent stale data from overwriting fresh data
    updated_at: chrono::DateTime<chrono::Utc>,
    /// Independent global moderation ban flag.
    is_banned: bool,
    /// Whether the user has been soft-deleted (`deleted_at` IS NOT NULL)
    is_deleted: bool,
}

pub struct CachedUserSnapshot {
    pub id: UserId,
    pub username: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub is_banned: bool,
    pub is_deleted: bool,
}

impl CachedUser {
    /// Create a new `CachedUser`
    #[must_use]
    pub fn new(
        id: UserId,
        username: String,
        role: UserRole,
        status: UserStatus,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            id,
            username,
            role,
            status,
            created_at,
            updated_at: chrono::Utc::now(),
            is_banned: false,
            is_deleted: false,
        }
    }

    #[must_use]
    pub fn from_snapshot(snapshot: CachedUserSnapshot) -> Self {
        Self {
            id: snapshot.id,
            username: snapshot.username,
            role: snapshot.role,
            status: snapshot.status,
            created_at: snapshot.created_at,
            updated_at: snapshot.updated_at,
            is_banned: snapshot.is_banned,
            is_deleted: snapshot.is_deleted,
        }
    }

    /// Get the user's role
    #[must_use]
    pub const fn role(&self) -> UserRole {
        self.role
    }

    /// Get the user's status
    #[must_use]
    pub const fn status(&self) -> UserStatus {
        self.status
    }

    /// Get the `updated_at` timestamp
    #[must_use]
    pub const fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }

    /// Check if the user has been soft-deleted
    #[must_use]
    pub const fn is_deleted(&self) -> bool {
        self.is_deleted
    }

    /// Check if the user is globally banned.
    #[must_use]
    pub const fn is_banned(&self) -> bool {
        self.is_banned
    }
}

impl Timestamped for CachedUser {
    fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }
}

impl UserCache {
    /// Create a new `UserCache`
    ///
    /// # Arguments
    /// * `l2` - L2 cache backend (e.g. `RedisCacheL2` or `NoopCacheL2`)
    /// * `l1_max_capacity` - Maximum number of entries in L1 cache
    /// * `l1_ttl_seconds` - TTL for L1 cache entries in seconds
    /// * `l2_ttl_seconds` - TTL for L2 cache entries in seconds
    /// * `key_prefix` - L2 key prefix (e.g., "synctv:user:")
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
            "user".to_string(),
        );
        Self { inner }
    }

    /// Create a user cache for local-only operation without shared L2 state.
    pub fn local_only(
        l1_max_capacity: u64,
        l1_ttl_seconds: u64,
        l2_ttl_seconds: u64,
        key_prefix: String,
    ) -> Self {
        Self::new(
            crate::cache::local_l2_cache_backend(),
            l1_max_capacity,
            l1_ttl_seconds,
            l2_ttl_seconds,
            key_prefix,
        )
    }

    /// Get user data from cache
    ///
    /// Checks L1 first, then L2. Returns None if not found in either cache.
    pub async fn get(&self, user_id: &UserId) -> Result<Option<CachedUser>> {
        self.inner.get(user_id).await
    }

    /// Set user data in cache
    ///
    /// Updates both L1 and L2 caches.
    pub async fn set(&self, user_id: &UserId, user: CachedUser) -> Result<()> {
        self.inner.set(user_id, user).await
    }

    /// Set user data in cache only if it's newer than existing data
    ///
    /// Compares `updated_at` timestamps and only updates if the new data is newer.
    /// This prevents race conditions where stale data overwrites fresh data.
    pub async fn set_if_newer(&self, user_id: &UserId, user: CachedUser) -> Result<bool> {
        self.inner.set_if_newer(user_id, user).await
    }

    /// Invalidate user data from cache
    ///
    /// Removes from both L1 and L2 caches.
    pub async fn invalidate(&self, user_id: &UserId) -> Result<()> {
        self.inner.invalidate(user_id).await
    }

    /// Invalidate a specific user's cache entry by ID string (both L1 and L2)
    ///
    /// Used by the cross-replica invalidation listener to remove a single
    /// entry from the local in-memory cache and L2 Redis cache.
    pub async fn invalidate_by_id(&self, user_id: &str) -> Result<()> {
        self.inner.invalidate_by_id(user_id).await
    }

    /// Get multiple users at once
    ///
    /// More efficient than calling `get()` multiple times.
    /// Returns a map of `user_id` -> `CachedUser`.
    pub async fn get_batch(
        &self,
        user_ids: &[UserId],
    ) -> Result<std::collections::HashMap<UserId, CachedUser>> {
        self.inner.get_batch(user_ids).await
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

impl std::fmt::Debug for UserCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserCache")
            .field("inner", &self.inner)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{TestOptionExt, TestResultExt};

    fn create_test_user_id(id: i64) -> UserId {
        UserId::expect_positive(id)
    }

    fn create_test_user(id: UserId, username: &str) -> CachedUser {
        CachedUser {
            id,
            username: username.to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            is_deleted: false,
            is_banned: false,
        }
    }

    #[tokio::test]
    async fn test_l1_cache_only() {
        let cache = UserCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:".to_string(),
        );

        let user_id = create_test_user_id(96_001);
        let user = create_test_user(user_id, "alice");

        // Cache miss
        assert!(cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .is_none());

        // Set and get
        cache
            .set(&user_id, user.clone())
            .await
            .checked("operation should succeed");
        let retrieved = cache
            .get(&user_id)
            .await
            .checked("operation should succeed")
            .checked("operation should succeed");
        assert_eq!(retrieved.username, "alice");

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
    async fn test_invalidate_by_id_rejects_malformed_id() {
        let cache = UserCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:".to_string(),
        );

        let result = cache.invalidate_by_id("not-a-user-id").await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_batch_lookup() {
        let cache = UserCache::new(
            Arc::new(crate::cache::NoopCacheL2),
            100,
            5,
            0,
            "test:".to_string(),
        );

        let user1 = create_test_user_id(96_002);
        let user2 = create_test_user_id(96_003);
        let user3 = create_test_user_id(96_004);

        // Set some entries
        cache
            .set(&user1, create_test_user(user1, "alice"))
            .await
            .checked("operation should succeed");
        cache
            .set(&user3, create_test_user(user3, "charlie"))
            .await
            .checked("operation should succeed");

        // Batch lookup
        let result = cache
            .get_batch(&[user1, user2, user3])
            .await
            .checked("operation should succeed");

        assert_eq!(result.len(), 2);
        assert_eq!(
            result.get(&user1).map(|u| &u.username),
            Some(&"alice".to_string())
        );
        assert_eq!(result.get(&user2), None);
        assert_eq!(
            result.get(&user3).map(|u| &u.username),
            Some(&"charlie".to_string())
        );
    }
}
