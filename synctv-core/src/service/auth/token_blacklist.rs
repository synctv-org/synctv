//! Refresh token blacklist and family revocation storage.
//!
//! ## Backend Abstraction
//!
//! Storage is abstracted via the [`TokenBlacklistStore`] trait. Two
//! implementations are provided:
//! - [`RedisTokenBlacklistStore`]: Redis-backed, for production deployments
//!   with Redis configured.
//! - [`InMemoryTokenBlacklistStore`]: moka cache, for standalone mode without
//!   Redis. Data is lost on restart but the security invariants (fail-closed
//!   blacklisting) are preserved.

use async_trait::async_trait;
use redis::AsyncCommands;
use std::sync::Arc;

use crate::Result;

// ============================================================================
// TokenBlacklistStore trait
// ============================================================================

/// Storage backend for refresh token blacklist and family revocation.
///
/// Used by `UserService` for Refresh Token Rotation:
/// 1. **JTI blacklist**: Each used refresh token's JTI is recorded so replays
///    are detected.
/// 2. **Family revocation**: When a blacklisted JTI is replayed (indicating
///    token theft), the entire token family for the user is revoked.
#[async_trait]
pub trait TokenBlacklistStore: Send + Sync {
    /// Check if a JTI key is blacklisted (already used).
    async fn is_blacklisted(&self, key: &str) -> bool;

    /// Blacklist a JTI key with the given TTL in seconds.
    ///
    /// Returns `Err` only on critical failures where the caller should
    /// refuse to issue new tokens (fail-closed).
    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()>;

    /// Get the family revocation timestamp for a key, if set.
    ///
    /// Returns `Some(revoked_at_timestamp)` if the family was revoked.
    async fn get_family_revoked_at(&self, key: &str) -> Option<i64>;

    /// Set the family revocation timestamp for a key with TTL.
    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64);
}

// ============================================================================
// InMemoryTokenBlacklistStore
// ============================================================================

/// In-memory [`TokenBlacklistStore`] using moka caches with TTL-based expiry.
///
/// Used in standalone mode without Redis. The moka cache provides
/// TTL-based eviction, so blacklisted JTIs and family revocations expire
/// naturally. Data is lost on restart.
pub struct InMemoryTokenBlacklistStore {
    /// JTI -> () (presence = blacklisted)
    jti_blacklist: Arc<moka::future::Cache<String, ()>>,
    /// user_key -> revoked_at timestamp
    family_revoked: Arc<moka::future::Cache<String, i64>>,
}

impl InMemoryTokenBlacklistStore {
    /// Create a new in-memory token blacklist store.
    ///
    /// - `max_jti_capacity`: maximum number of blacklisted JTIs to track
    /// - `jti_ttl_secs`: TTL for blacklisted JTIs (should match refresh token lifetime)
    /// - `family_ttl_secs`: TTL for family revocations (refresh token lifetime + buffer)
    #[must_use]
    pub fn new(max_jti_capacity: u64, jti_ttl_secs: u64, family_ttl_secs: u64) -> Self {
        Self {
            jti_blacklist: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(max_jti_capacity)
                    .time_to_live(std::time::Duration::from_secs(jti_ttl_secs))
                    .build(),
            ),
            family_revoked: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(50_000)
                    .time_to_live(std::time::Duration::from_secs(family_ttl_secs))
                    .build(),
            ),
        }
    }
}

#[async_trait]
impl TokenBlacklistStore for InMemoryTokenBlacklistStore {
    async fn is_blacklisted(&self, key: &str) -> bool {
        self.jti_blacklist.get(key).await.is_some()
    }

    async fn blacklist(&self, key: &str, _ttl_secs: u64) -> Result<()> {
        // In-memory blacklist is infallible
        self.jti_blacklist.insert(key.to_string(), ()).await;
        Ok(())
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        self.family_revoked.get(key).await
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, _ttl_secs: u64) {
        self.family_revoked.insert(key.to_string(), timestamp).await;
    }
}

// ============================================================================
// RedisTokenBlacklistStore
// ============================================================================

/// Redis-backed [`TokenBlacklistStore`].
///
/// Uses Redis `SET EX` for TTL-based expiry and `EXISTS`/`GET` for lookups.
pub struct RedisTokenBlacklistStore {
    conn: redis::aio::ConnectionManager,
}

impl RedisTokenBlacklistStore {
    /// Create a new Redis-backed token blacklist store.
    #[must_use]
    pub fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl TokenBlacklistStore for RedisTokenBlacklistStore {
    async fn is_blacklisted(&self, key: &str) -> bool {
        let mut conn = self.conn.clone();
        conn.exists(key).await.unwrap_or(false)
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()> {
        let mut conn = self.conn.clone();
        conn.set_ex::<_, _, ()>(key, "1", ttl_secs)
            .await
            .map_err(|e| {
                tracing::error!(
                    key = %key,
                    error = %e,
                    "Failed to blacklist refresh token JTI in Redis"
                );
                crate::Error::Internal("Failed to rotate refresh token".to_string())
            })
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        let mut conn = self.conn.clone();
        conn.get(key).await.unwrap_or(None)
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) {
        let mut conn = self.conn.clone();
        let _: std::result::Result<(), _> = conn.set_ex(key, timestamp, ttl_secs).await;
    }
}
