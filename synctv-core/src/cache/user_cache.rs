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

// --- CacheKey implementation for UserId ---

impl CacheKey for UserId {
    fn as_str(&self) -> &str {
        self.as_str()
    }
    fn from_id(id: &str) -> Self {
        Self::from_string(id.to_string())
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
    id: String,
    username: String,
    role: UserRole,
    status: UserStatus,
    created_at: chrono::DateTime<chrono::Utc>,
    /// Timestamp of last update - used to prevent stale data from overwriting fresh data
    updated_at: chrono::DateTime<chrono::Utc>,
    /// Password version counter for JWT invalidation
    password_version: i32,
}

impl CachedUser {
    /// Create a new `CachedUser`
    #[must_use]
    pub fn new(
        id: String,
        username: String,
        role: UserRole,
        status: UserStatus,
        created_at: chrono::DateTime<chrono::Utc>,
        password_version: i32,
    ) -> Self {
        Self {
            id,
            username,
            role,
            status,
            created_at,
            updated_at: chrono::Utc::now(),
            password_version,
        }
    }

    /// Create a new `CachedUser` with explicit `updated_at` timestamp
    #[must_use]
    pub const fn with_updated_at(
        id: String,
        username: String,
        role: UserRole,
        status: UserStatus,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
        password_version: i32,
    ) -> Self {
        Self { id, username, role, status, created_at, updated_at, password_version }
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

    /// Get the password version counter
    #[must_use]
    pub const fn password_version(&self) -> i32 {
        self.password_version
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
    /// * `l1_ttl_minutes` - TTL for L1 cache entries in minutes
    /// * `l2_ttl_seconds` - TTL for L2 cache entries in seconds
    /// * `key_prefix` - L2 key prefix (e.g., "synctv:user:")
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
            "user".to_string(),
        )?;
        Ok(Self { inner })
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
    pub async fn invalidate_by_id(&self, user_id: &str) {
        self.inner.invalidate_by_id(user_id).await;
    }

    /// Get multiple users at once
    ///
    /// More efficient than calling `get()` multiple times.
    /// Returns a map of `user_id` -> `CachedUser`.
    pub async fn get_batch(&self, user_ids: &[UserId]) -> Result<std::collections::HashMap<UserId, CachedUser>> {
        self.inner.get_batch(user_ids).await
    }

    /// Clear L1 cache (memory only)
    ///
    /// Useful for testing or manual cache clearing.
    /// Note: L2 cache is not cleared.
    pub async fn clear_l1(&self) {
        self.inner.clear_l1().await;
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

    fn create_test_user_id(id: &str) -> UserId {
        UserId::from_string(id.to_string())
    }

    fn create_test_user(id: &str, username: &str) -> CachedUser {
        CachedUser {
            id: id.to_string(),
            username: username.to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            password_version: 0,
        }
    }

    #[tokio::test]
    async fn test_l1_cache_only() {
        let cache = UserCache::new(Arc::new(crate::cache::NoopCacheL2), 100, 5, 0, "test:".to_string()).unwrap();

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
        let cache = UserCache::new(Arc::new(crate::cache::NoopCacheL2), 100, 5, 0, "test:".to_string()).unwrap();

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
