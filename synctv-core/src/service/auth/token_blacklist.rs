//! Refresh token blacklist and family revocation storage.
//!
//! ## Backend Abstraction
//!
//! Storage is abstracted via the [`TokenBlacklistStore`] trait. Three
//! implementations are provided:
//! - [`RedisTokenBlacklistStore`]: Redis-backed, for production deployments
//!   with Redis configured.
//! - [`PgTokenBlacklistStore`]: PostgreSQL-backed, durable fallback for
//!   standalone deployments without Redis. Data survives restarts.
//! - [`InMemoryTokenBlacklistStore`]: moka cache, for standalone mode without
//!   Redis. Data is lost on restart but the security invariants (fail-closed
//!   blacklisting) are preserved.

use async_trait::async_trait;
use redis::AsyncCommands;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// In-memory [`TokenBlacklistStore`] using moka caches with per-entry TTL tracking.
///
/// Used in standalone mode without Redis. Moka provides capacity-based eviction
/// and a long background TTL for memory bounds; per-entry expiry is tracked via
/// stored `(value, expiry: Instant)` pairs and checked on every read so the
/// caller-supplied `ttl_secs` is honoured exactly.
/// Data is lost on restart.
pub struct InMemoryTokenBlacklistStore {
    /// JTI -> expiry Instant (presence + non-expired = blacklisted)
    jti_blacklist: Arc<moka::future::Cache<String, Instant>>,
    /// `user_key` -> (`revoked_at` timestamp, expiry Instant)
    family_revoked: Arc<moka::future::Cache<String, (i64, Instant)>>,
}

impl InMemoryTokenBlacklistStore {
    /// Create a new in-memory token blacklist store.
    ///
    /// - `max_jti_capacity`: maximum number of blacklisted JTIs to track
    /// - `jti_ttl_secs`: upper-bound TTL used for moka's background eviction
    /// - `family_ttl_secs`: upper-bound TTL used for moka's background eviction
    ///
    /// The exact per-entry TTL is enforced at read time using the `ttl_secs`
    /// argument passed to `blacklist()` / `set_family_revoked()`.
    #[must_use]
    pub fn new(max_jti_capacity: u64, jti_ttl_secs: u64, family_ttl_secs: u64) -> Self {
        Self {
            jti_blacklist: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(max_jti_capacity)
                    // Use the upper-bound TTL for background memory reclamation.
                    .time_to_live(Duration::from_secs(jti_ttl_secs))
                    .build(),
            ),
            family_revoked: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(50_000)
                    .time_to_live(Duration::from_secs(family_ttl_secs))
                    .build(),
            ),
        }
    }
}

#[async_trait]
impl TokenBlacklistStore for InMemoryTokenBlacklistStore {
    async fn is_blacklisted(&self, key: &str) -> bool {
        match self.jti_blacklist.get(key).await {
            Some(expiry) => Instant::now() < expiry,
            None => false,
        }
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()> {
        let expiry = Instant::now() + Duration::from_secs(ttl_secs);
        self.jti_blacklist.insert(key.to_string(), expiry).await;
        Ok(())
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        match self.family_revoked.get(key).await {
            Some((timestamp, expiry)) if Instant::now() < expiry => Some(timestamp),
            _ => None,
        }
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) {
        let expiry = Instant::now() + Duration::from_secs(ttl_secs);
        self.family_revoked
            .insert(key.to_string(), (timestamp, expiry))
            .await;
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
    pub const fn new(conn: redis::aio::ConnectionManager) -> Self {
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

// ============================================================================
// PgTokenBlacklistStore
// ============================================================================

/// PostgreSQL-backed [`TokenBlacklistStore`].
///
/// Provides durable token blacklist storage that survives restarts, used as
/// a fallback when Redis is unavailable (standalone deployments).
///
/// JTI blacklist entries are stored in the `token_blacklist` table with an
/// `expires_at` timestamp. Expired rows are cleaned up lazily (not returned
/// by queries) and can be purged periodically via
/// `DELETE FROM token_blacklist WHERE expires_at < NOW()`.
///
/// Family revocation is stored in the same table with a `family:` key prefix
/// and the revocation timestamp as the JTI value.
pub struct PgTokenBlacklistStore {
    pool: PgPool,
}

impl PgTokenBlacklistStore {
    /// Create a new PostgreSQL-backed token blacklist store.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TokenBlacklistStore for PgTokenBlacklistStore {
    async fn is_blacklisted(&self, key: &str) -> bool {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM token_blacklist WHERE jti = $1 AND expires_at > NOW())"
        )
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(false)
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()> {
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(ttl_secs as i64);
        sqlx::query(
            "INSERT INTO token_blacklist (jti, expires_at) VALUES ($1, $2) \
             ON CONFLICT (jti) DO UPDATE SET expires_at = EXCLUDED.expires_at"
        )
        .bind(key)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!(
                key = %key,
                error = %e,
                "Failed to blacklist refresh token JTI in PostgreSQL"
            );
            crate::Error::Internal("Failed to rotate refresh token".to_string())
        })?;
        Ok(())
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        // Family revocation timestamp is stored as the expires_at field of a
        // special `family:<key>` entry. The actual revocation timestamp is
        // stored by encoding it in a separate query.
        let row: Option<(chrono::DateTime<chrono::Utc>,)> = sqlx::query_as(
            "SELECT expires_at FROM token_blacklist WHERE jti = $1 AND expires_at > NOW()"
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .ok()?;

        // We store family revocation as: jti = "family:<user_key>", expires_at = actual expiry.
        // The revocation timestamp is stored as a second entry with jti = "family_ts:<user_key>".
        let ts_key = format!("_ts:{key}");
        let ts_row: Option<(chrono::DateTime<chrono::Utc>,)> = sqlx::query_as(
            "SELECT expires_at FROM token_blacklist WHERE jti = $1"
        )
        .bind(&ts_key)
        .fetch_optional(&self.pool)
        .await
        .ok()?;

        // If the main family key exists (not expired) and we have a timestamp entry,
        // return the timestamp. The ts entry stores the revoked_at as epoch seconds
        // in the expires_at column (reusing the column for storage).
        if row.is_some() {
            ts_row.map(|(ts,)| ts.timestamp())
        } else {
            None
        }
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) {
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(ttl_secs as i64);

        // Store the family revocation marker
        let _ = sqlx::query(
            "INSERT INTO token_blacklist (jti, expires_at) VALUES ($1, $2) \
             ON CONFLICT (jti) DO UPDATE SET expires_at = EXCLUDED.expires_at"
        )
        .bind(key)
        .bind(expires_at)
        .execute(&self.pool)
        .await;

        // Store the revocation timestamp in a companion entry.
        // We encode the timestamp as a DateTime for storage.
        let ts_key = format!("_ts:{key}");
        let revoked_at = chrono::DateTime::from_timestamp(timestamp, 0)
            .unwrap_or_else(|| chrono::Utc::now());
        let _ = sqlx::query(
            "INSERT INTO token_blacklist (jti, expires_at) VALUES ($1, $2) \
             ON CONFLICT (jti) DO UPDATE SET expires_at = EXCLUDED.expires_at"
        )
        .bind(&ts_key)
        .bind(revoked_at)
        .execute(&self.pool)
        .await;
    }
}
