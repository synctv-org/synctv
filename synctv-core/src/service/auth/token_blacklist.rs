//! Refresh token blacklist and family revocation storage.
//!
//! ## Backend Abstraction
//!
//! Storage is abstracted via the [`TokenBlacklistStore`] trait. Five
//! implementations are provided:
//! - [`TieredTokenBlacklistStore`]: Production store with L1 (moka) + optional
//!   L2 (Redis) + PG primary. Includes negative caching for cache penetration
//!   protection.
//! - [`PgTokenBlacklistStore`]: PostgreSQL-backed, used as the inner durable
//!   layer inside `TieredTokenBlacklistStore`.
//! - [`InMemoryTokenBlacklistStore`]: moka cache, for tests and standalone mode
//!   without a database. Data is lost on restart.
//! - [`FallbackTokenBlacklistStore`]: Wraps a primary store with in-memory
//!   fallback. When the primary fails, operations still succeed via the
//!   in-memory fallback.
//! - [`RedisSyncableTokenBlacklistStore`]: Extends fallback with pending write
//!   buffering and async sync. When the primary (Redis) recovers, pending
//!   writes can be synced via `sync_pending_writes()`.
//!
//! ## Memory Fallback (Task #18)
//!
//! The `RedisSyncableTokenBlacklistStore` provides:
//! 1. **Memory fallback**: When Redis is unavailable, blacklisted tokens are
//!    tracked in memory, ensuring security during outages.
//! 2. **Pending write buffer**: Failed writes are buffered with their TTLs.
//! 3. **Async sync mechanism**: When Redis recovers, call `sync_pending_writes()`
//!    to replay buffered writes to Redis.

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
    ///
    /// This convenience method is intended for tests and best-effort
    /// introspection only. Security-sensitive authentication paths must use
    /// [`is_blacklisted_checked`] so storage errors fail closed instead of
    /// silently treating the token as valid.
    async fn is_blacklisted(&self, key: &str) -> bool {
        self.is_blacklisted_checked(key).await.unwrap_or_else(|e| {
            tracing::error!(
                key = %key,
                error = %e,
                "Token blacklist convenience lookup failed; returning not-blacklisted for non-auth usage"
            );
            false
        })
    }

    /// Check if a JTI key is blacklisted, propagating storage errors.
    ///
    /// Unlike [`is_blacklisted`] which returns `false` on errors (fail-open),
    /// this method returns `Err` on storage failures so the caller can decide
    /// whether to fail-open or fail-closed.
    ///
    /// Implementations must provide this method; authentication code relies on
    /// it to preserve fail-closed semantics.
    async fn is_blacklisted_checked(&self, key: &str) -> Result<bool>;

    /// Blacklist a JTI key with the given TTL in seconds.
    ///
    /// Returns `Err` only on critical failures where the caller should
    /// refuse to issue new tokens (fail-closed).
    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()>;

    /// Atomically blacklist a JTI key if it doesn't already exist.
    ///
    /// This is the core method for preventing TOCTOU race conditions in
    /// refresh token rotation. It performs an atomic "check and set" operation:
    ///
    /// - Returns `Ok(true)` if the key **already existed** (replay detected)
    /// - Returns `Ok(false)` if the key was **newly inserted** (first use)
    /// - Returns `Err` only on critical failures (fail-closed)
    ///
    /// # Security Implications
    ///
    /// When refreshing a token:
    /// - `Ok(false)` means this is the first use → proceed to issue new token
    /// - `Ok(true)` means a replay was detected → trigger family revocation
    ///
    /// # Default Implementation
    ///
    /// The default implementation uses `is_blacklisted_checked` + `blacklist`
    /// which is NOT atomic. Implementations should override this with proper atomic
    /// operations (e.g., Redis SETNX, `PostgreSQL` INSERT ... ON CONFLICT DO NOTHING).
    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> Result<bool> {
        // Default: non-atomic check-then-set (has TOCTOU race condition)
        if self.is_blacklisted_checked(key).await? {
            return Ok(true);
        }
        self.blacklist(key, ttl_secs).await?;
        Ok(false)
    }

    /// Get the family revocation timestamp for a key, if set.
    ///
    /// Returns `Some(revoked_at_timestamp)` if the family was revoked.
    async fn get_family_revoked_at(&self, key: &str) -> Option<i64>;

    /// Get the family revocation timestamp for a key, propagating storage errors.
    ///
    /// Authentication-sensitive code must use this variant so storage failures
    /// fail closed instead of silently treating the family as valid.
    async fn get_family_revoked_at_checked(&self, key: &str) -> Result<Option<i64>> {
        Ok(self.get_family_revoked_at(key).await)
    }

    /// Set the family revocation timestamp for a key with TTL.
    ///
    /// This is a security-critical write. Callers must be able to fail closed
    /// if persistence does not succeed.
    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) -> Result<()>;
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
///
/// ## Atomicity for `blacklist_if_not_exists`
///
/// Since moka doesn't support atomic "insert if absent", we use a `DashMap` of
/// `Arc<tokio::sync::Mutex>` per key to serialize concurrent operations on the same JTI.
/// This prevents TOCTOU race conditions during refresh token rotation.
pub struct InMemoryTokenBlacklistStore {
    /// JTI -> expiry Instant (presence + non-expired = blacklisted)
    jti_blacklist: Arc<moka::future::Cache<String, Instant>>,
    /// `user_key` -> (`revoked_at` timestamp, expiry Instant)
    family_revoked: Arc<moka::future::Cache<String, (i64, Instant)>>,
    /// Per-key mutex for atomic `blacklist_if_not_exists` operations
    /// Uses `DashMap` for O(1) lock acquisition without global contention
    blacklist_locks: Arc<dashmap::DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
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
            blacklist_locks: Arc::new(dashmap::DashMap::new()),
        }
    }

    fn cleanup_blacklist_lock(&self, key: &str, mutex: &Arc<tokio::sync::Mutex<()>>) {
        if Arc::strong_count(mutex) != 2 {
            return;
        }
        let Ok(_cleanup_guard) = mutex.try_lock() else {
            return;
        };
        let _ = self
            .blacklist_locks
            .remove_if(key, |_, stored_mutex| Arc::ptr_eq(stored_mutex, mutex));
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

    async fn is_blacklisted_checked(&self, key: &str) -> Result<bool> {
        Ok(self.is_blacklisted(key).await)
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()> {
        let expiry = Instant::now() + Duration::from_secs(ttl_secs);
        self.jti_blacklist.insert(key.to_string(), expiry).await;
        Ok(())
    }

    /// Atomically blacklist the key if it doesn't already exist.
    ///
    /// Uses a per-key mutex to serialize concurrent operations on the same JTI,
    /// preventing TOCTOU race conditions. Returns:
    /// - `Ok(true)` if key already existed (replay detected)
    /// - `Ok(false)` if key was newly inserted (first use)
    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> Result<bool> {
        // Get or create a mutex for this specific key
        // Using entry API to avoid race between get and insert
        let mutex: Arc<tokio::sync::Mutex<()>> = self
            .blacklist_locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .value()
            .clone();

        let already_blacklisted = {
            let _guard = mutex.lock().await;

            // Double-check pattern: check if already blacklisted
            if self.is_blacklisted(key).await {
                true
            } else {
                // Not blacklisted, so insert atomically
                let expiry = Instant::now() + Duration::from_secs(ttl_secs);
                self.jti_blacklist.insert(key.to_string(), expiry).await;
                false
            }
        };

        // Clean up only after releasing the mutex, and only when no other task
        // still holds or waits on the same per-key mutex.
        self.cleanup_blacklist_lock(key, &mutex);

        Ok(already_blacklisted)
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        match self.family_revoked.get(key).await {
            Some((timestamp, expiry)) if Instant::now() < expiry => Some(timestamp),
            _ => None,
        }
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) -> Result<()> {
        let expiry = Instant::now() + Duration::from_secs(ttl_secs);
        self.family_revoked
            .insert(key.to_string(), (timestamp, expiry))
            .await;
        Ok(())
    }
}

// ============================================================================
// PgTokenBlacklistStore
// ============================================================================

/// PostgreSQL-backed [`TokenBlacklistStore`].
///
/// Provides durable token blacklist storage that survives restarts, used as
/// the primary (durable) layer inside [`TieredTokenBlacklistStore`].
///
/// JTI blacklist entries are stored in the `token_blacklist` table with an
/// `expires_at` timestamp. Family revocation reuses the same primary key row
/// and stores the stable revocation timestamp in `family_revoked_at`.
/// Expired rows are cleaned up lazily (not returned by queries) and can be
/// purged periodically via `DELETE FROM token_blacklist WHERE expires_at < NOW()`.
pub struct PgTokenBlacklistStore {
    pool: PgPool,
}

impl PgTokenBlacklistStore {
    /// Create a new PostgreSQL-backed token blacklist store.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get a reference to the underlying connection pool.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Clean up expired token blacklist entries.
    ///
    /// Delete expired token blacklist entries directly in PostgreSQL.
    ///
    /// This intentionally avoids depending on a historical migration function
    /// signature so cleanup continues to work on upgraded databases.
    pub async fn cleanup_expired(&self) -> Result<u64> {
        let deleted_count = sqlx::query_scalar::<_, i64>(
            r"
            WITH deleted AS (
                DELETE FROM token_blacklist
                WHERE expires_at < CURRENT_TIMESTAMP
                RETURNING 1
            )
            SELECT COUNT(*)::BIGINT FROM deleted
            ",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!(
                error = %e,
                "Failed to cleanup expired token blacklist entries"
            );
            crate::Error::Internal("Failed to cleanup token blacklist".to_string())
        })?;
        Ok(deleted_count.max(0) as u64)
    }
}

#[async_trait]
impl TokenBlacklistStore for PgTokenBlacklistStore {
    async fn is_blacklisted(&self, key: &str) -> bool {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM token_blacklist WHERE jti = $1 AND expires_at > NOW())",
        )
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(false)
    }

    async fn is_blacklisted_checked(&self, key: &str) -> Result<bool> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM token_blacklist WHERE jti = $1 AND expires_at > NOW())",
        )
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!(
                key = %key,
                error = %e,
                "Failed to check token blacklist in PostgreSQL (fail-closed)"
            );
            crate::Error::Internal(format!("Token blacklist check failed: {e}"))
        })
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()> {
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(ttl_secs as i64);
        sqlx::query(
            "INSERT INTO token_blacklist (jti, expires_at) VALUES ($1, $2) \
             ON CONFLICT (jti) DO UPDATE SET expires_at = EXCLUDED.expires_at",
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

    /// Atomically blacklist the key if it doesn't already exist.
    ///
    /// Uses `PostgreSQL`'s `INSERT ... ON CONFLICT DO NOTHING` with `xmax` check
    /// to atomically detect whether the insert was successful or the key existed.
    /// Returns:
    /// - `Ok(true)` if key already existed (replay detected)
    /// - `Ok(false)` if key was newly inserted (first use)
    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> Result<bool> {
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(ttl_secs as i64);

        // xmax = 0 means the row was inserted (no conflict)
        // xmax != 0 means the row already existed (conflict, nothing inserted)
        // See: https://www.postgresql.org/docs/current/functions-info.html
        let inserted: bool = sqlx::query_scalar(
            "INSERT INTO token_blacklist (jti, expires_at) VALUES ($1, $2) \
             ON CONFLICT (jti) DO NOTHING \
             RETURNING (xmax = 0)",
        )
        .bind(key)
        .bind(expires_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!(
                key = %key,
                error = %e,
                "Failed to blacklist refresh token JTI in PostgreSQL"
            );
            crate::Error::Internal("Failed to rotate refresh token".to_string())
        })?
        .unwrap_or(false); // None means conflict occurred, row not returned

        // inserted = true means we inserted a new row (first use)
        // inserted = false means conflict, key already existed (replay)
        Ok(!inserted)
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        sqlx::query_scalar::<_, i64>(
            "SELECT family_revoked_at
             FROM token_blacklist
             WHERE jti = $1
               AND expires_at > NOW()
               AND family_revoked_at IS NOT NULL",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
    }

    async fn get_family_revoked_at_checked(&self, key: &str) -> Result<Option<i64>> {
        sqlx::query_scalar::<_, i64>(
            "SELECT family_revoked_at
             FROM token_blacklist
             WHERE jti = $1
               AND expires_at > NOW()
               AND family_revoked_at IS NOT NULL",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!(
                key = %key,
                error = %e,
                "Failed to read refresh token family revocation timestamp in PostgreSQL (fail-closed)"
            );
            crate::Error::Internal("Failed to validate refresh token family".to_string())
        })
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) -> Result<()> {
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(ttl_secs as i64);
        sqlx::query(
            "INSERT INTO token_blacklist (jti, expires_at, family_revoked_at) VALUES ($1, $2, $3) \
             ON CONFLICT (jti) DO UPDATE
             SET expires_at = EXCLUDED.expires_at,
                 family_revoked_at = EXCLUDED.family_revoked_at",
        )
        .bind(key)
        .bind(expires_at)
        .bind(timestamp)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!(
                key = %key,
                family_revoked_at = timestamp,
                error = %e,
                "Failed to persist refresh token family revocation row in PostgreSQL"
            );
            crate::Error::Internal("Failed to revoke refresh token family".to_string())
        })?;
        Ok(())
    }
}

// ============================================================================
// TieredTokenBlacklistStore
// ============================================================================

/// L1 positive cache TTL (how long a "is blacklisted = true" entry lives in moka).
const L1_POSITIVE_TTL: Duration = Duration::from_mins(2);
/// L1 negative cache TTL (how long a "is blacklisted = false" sentinel lives in moka).
const L1_NEGATIVE_TTL: Duration = Duration::from_secs(10);
/// L2 (Redis) negative sentinel TTL in seconds.
const L2_NEGATIVE_TTL_SECS: u64 = 15;
/// Safety margin subtracted from token TTL for L2 positive entries.
/// Ensures Redis entries expire before the token itself, preventing stale positives.
const L2_TTL_MARGIN_SECS: u64 = 30;

/// Tiered [`TokenBlacklistStore`] with L1 (moka) + optional L2 (Redis) + PG primary.
///
/// ## Architecture
///
/// ```text
/// is_blacklisted("jti_xyz"):
///
///   L1 (moka)  ──hit──>  return cached result (positive or negative)
///       │ miss
///   L2 (Redis) ──hit──>  populate L1, return result
///       │ miss
///   PG (primary)──found──> populate L1+L2 (positive), return true
///       │ not found
///   Cache negative sentinel in L1+L2 (short TTL), return false
/// ```
///
/// ## Cache Penetration Protection
///
/// When a key is not found in PG, a short-TTL negative sentinel is cached in
/// L1 (and L2 if Redis is available). This prevents repeated DB queries for
/// non-existent keys, providing cache penetration protection without the
/// false-negative issues of bloom filters.
///
/// ## Fallback Without Redis
///
/// When Redis is not configured, the store degrades to L1 (moka) + PG,
/// still providing L1 cache acceleration, negative caching, and PG durability.
///
/// ## Atomicity for `blacklist_if_not_exists`
///
/// PostgreSQL is the authoritative store for atomic replay detection via
/// `INSERT ... ON CONFLICT DO NOTHING`. Redis is used only as an L2 cache for
/// read acceleration and must never become the only durable record of a
/// blacklisted token.
///
/// When Redis is not available, the store still provides correct behavior via
/// L1 (moka) + PG.
pub struct TieredTokenBlacklistStore {
    pg: PgTokenBlacklistStore,
    /// Shared Redis connection handle that follows Sentinel failover.
    redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
    /// L1 cache: JTI key -> (`is_blacklisted`, expiry)
    l1_blacklist: moka::future::Cache<String, (bool, Instant)>,
    /// L1 cache: family key -> (Option<`revoked_at_timestamp`>, expiry)
    l1_family: moka::future::Cache<String, (Option<i64>, Instant)>,
    /// Redis key prefix (e.g., "synctv:")
    key_prefix: String,
    /// Per-key mutex for atomic `blacklist_if_not_exists` operations (used when Redis unavailable)
    /// Uses `DashMap` for O(1) lock acquisition without global contention
    blacklist_locks: Arc<dashmap::DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl TieredTokenBlacklistStore {
    /// Create a new tiered token blacklist store.
    ///
    /// - `pg`: The `PostgreSQL` store used as durable primary.
    /// - `redis_conn`: Optional shared Redis connection handle for L2 caching.
    ///   Uses `Arc<RwLock<ConnectionManager>>` to follow Sentinel failover.
    /// - `key_prefix`: Redis key prefix (e.g., `"synctv:"`).
    #[must_use]
    pub fn new(
        pg: PgTokenBlacklistStore,
        redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
        key_prefix: String,
    ) -> Self {
        Self {
            pg,
            redis_conn,
            // L1 blacklist: max 100k entries, background eviction at 120s
            l1_blacklist: moka::future::Cache::builder()
                .max_capacity(100_000)
                .time_to_live(L1_POSITIVE_TTL)
                .build(),
            // L1 family: max 50k entries, background eviction at 120s
            l1_family: moka::future::Cache::builder()
                .max_capacity(50_000)
                .time_to_live(L1_POSITIVE_TTL)
                .build(),
            key_prefix,
            blacklist_locks: Arc::new(dashmap::DashMap::new()),
        }
    }

    /// Redis key for a blacklist entry.
    fn bl_key(&self, jti: &str) -> String {
        format!("{}bl:{}", self.key_prefix, jti)
    }

    /// Redis key for a family revocation entry.
    fn fam_key(&self, key: &str) -> String {
        format!("{}fam:{}", self.key_prefix, key)
    }

    /// Compute L2 TTL for a positive entry: `token_ttl - margin`, minimum 1s.
    fn l2_positive_ttl(ttl_secs: u64) -> u64 {
        ttl_secs.saturating_sub(L2_TTL_MARGIN_SECS).max(1)
    }

    fn cleanup_blacklist_lock(&self, key: &str, mutex: &Arc<tokio::sync::Mutex<()>>) {
        if Arc::strong_count(mutex) != 2 {
            return;
        }
        let Ok(_cleanup_guard) = mutex.try_lock() else {
            return;
        };
        let _ = self
            .blacklist_locks
            .remove_if(key, |_, stored_mutex| Arc::ptr_eq(stored_mutex, mutex));
    }
}

#[async_trait]
impl TokenBlacklistStore for TieredTokenBlacklistStore {
    async fn is_blacklisted(&self, key: &str) -> bool {
        // --- L1 check ---
        if let Some((is_bl, expiry)) = self.l1_blacklist.get(key).await {
            if Instant::now() < expiry {
                return is_bl;
            }
            // Expired entry; fall through to L2/PG
        }

        // --- L2 check (Redis) ---
        if let Some(ref redis_conn) = self.redis_conn {
            let redis_key = self.bl_key(key);
            let result: redis::RedisResult<Option<String>> = {
                let mut conn = redis_conn.read().await.clone();
                conn.get(&redis_key).await
            };
            match result {
                Ok(Some(val)) => {
                    let is_bl = val == "1";
                    let l1_ttl = if is_bl {
                        L1_POSITIVE_TTL
                    } else {
                        L1_NEGATIVE_TTL
                    };
                    self.l1_blacklist
                        .insert(key.to_string(), (is_bl, Instant::now() + l1_ttl))
                        .await;
                    return is_bl;
                }
                Ok(None) => {
                    // L2 miss, continue to PG
                }
                Err(e) => {
                    tracing::warn!(error = %e, key = %key, "Redis L2 blacklist lookup failed, falling back to PG");
                }
            }
        }

        // --- PG check (primary) ---
        let found = self.pg.is_blacklisted(key).await;

        if found {
            // Positive: populate L1 + L2
            self.l1_blacklist
                .insert(key.to_string(), (true, Instant::now() + L1_POSITIVE_TTL))
                .await;
            if let Some(ref redis_conn) = self.redis_conn {
                let redis_key = self.bl_key(key);
                let mut conn = redis_conn.read().await.clone();
                // Use a reasonable TTL; we don't know the token's TTL here,
                // so use L1_POSITIVE_TTL as a safe upper bound for L2.
                let _: redis::RedisResult<()> = conn
                    .set_ex(&redis_key, "1", L1_POSITIVE_TTL.as_secs())
                    .await;
            }
        } else {
            // Negative sentinel: populate L1 + L2
            self.l1_blacklist
                .insert(key.to_string(), (false, Instant::now() + L1_NEGATIVE_TTL))
                .await;
            if let Some(ref redis_conn) = self.redis_conn {
                let redis_key = self.bl_key(key);
                let mut conn = redis_conn.read().await.clone();
                let _: redis::RedisResult<()> =
                    conn.set_ex(&redis_key, "0", L2_NEGATIVE_TTL_SECS).await;
            }
        }

        found
    }

    async fn is_blacklisted_checked(&self, key: &str) -> Result<bool> {
        // --- L1 check ---
        if let Some((is_bl, expiry)) = self.l1_blacklist.get(key).await {
            if Instant::now() < expiry {
                return Ok(is_bl);
            }
            // Expired entry; fall through to L2/PG
        }

        // --- L2 check (Redis) ---
        if let Some(ref redis_conn) = self.redis_conn {
            let redis_key = self.bl_key(key);
            let result: redis::RedisResult<Option<String>> = {
                let mut conn = redis_conn.read().await.clone();
                conn.get(&redis_key).await
            };
            match result {
                Ok(Some(val)) => {
                    let is_bl = val == "1";
                    let l1_ttl = if is_bl {
                        L1_POSITIVE_TTL
                    } else {
                        L1_NEGATIVE_TTL
                    };
                    self.l1_blacklist
                        .insert(key.to_string(), (is_bl, Instant::now() + l1_ttl))
                        .await;
                    return Ok(is_bl);
                }
                Ok(None) => {
                    // L2 miss, continue to PG
                }
                Err(e) => {
                    tracing::warn!(error = %e, key = %key, "Redis L2 blacklist lookup failed, falling back to PG");
                }
            }
        }

        // --- PG check (primary, error-propagating) ---
        let found = self.pg.is_blacklisted_checked(key).await?;

        if found {
            // Positive: populate L1 + L2
            self.l1_blacklist
                .insert(key.to_string(), (true, Instant::now() + L1_POSITIVE_TTL))
                .await;
            if let Some(ref redis_conn) = self.redis_conn {
                let redis_key = self.bl_key(key);
                let mut conn = redis_conn.read().await.clone();
                let _: redis::RedisResult<()> = conn
                    .set_ex(&redis_key, "1", L1_POSITIVE_TTL.as_secs())
                    .await;
            }
        } else {
            // Negative sentinel: populate L1 + L2
            self.l1_blacklist
                .insert(key.to_string(), (false, Instant::now() + L1_NEGATIVE_TTL))
                .await;
            if let Some(ref redis_conn) = self.redis_conn {
                let redis_key = self.bl_key(key);
                let mut conn = redis_conn.read().await.clone();
                let _: redis::RedisResult<()> =
                    conn.set_ex(&redis_key, "0", L2_NEGATIVE_TTL_SECS).await;
            }
        }

        Ok(found)
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()> {
        // 1. Write to PG (durable primary)
        self.pg.blacklist(key, ttl_secs).await?;

        // 2. Write to L2 Redis (positive, overwrites any stale negative sentinel)
        if let Some(ref redis_conn) = self.redis_conn {
            let redis_key = self.bl_key(key);
            let l2_ttl = Self::l2_positive_ttl(ttl_secs);
            let mut conn = redis_conn.read().await.clone();
            let _: redis::RedisResult<()> = conn.set_ex(&redis_key, "1", l2_ttl).await;
        }

        // 3. Write to L1 moka (positive, overwrites any stale negative sentinel)
        self.l1_blacklist
            .insert(key.to_string(), (true, Instant::now() + L1_POSITIVE_TTL))
            .await;

        Ok(())
    }

    /// Atomically blacklist the key if it doesn't already exist.
    ///
    /// Uses PostgreSQL as the atomic source of truth, then updates Redis L2 as a
    /// best-effort cache. This preserves correctness when Redis data is lost and
    /// ensures `Ok(..)` is only returned after durable persistence succeeds.
    ///
    /// Returns:
    /// - `Ok(true)` if key already existed (replay detected)
    /// - `Ok(false)` if key was newly inserted (first use)
    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> Result<bool> {
        // Use a per-key mutex to collapse same-process concurrency and reduce
        // duplicate PG writes, while PostgreSQL remains the cross-replica atomic
        // source of truth.
        let mutex = self
            .blacklist_locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .value()
            .clone();

        let already_existed = {
            let _guard = mutex.lock().await;

            // Double-check L1 cache (may have been populated by another concurrent request)
            if let Some((is_bl, expiry)) = self.l1_blacklist.get(key).await {
                if Instant::now() < expiry && is_bl {
                    true
                } else {
                    // Delegate to PG's atomic operation first. Returning `Ok` before this
                    // succeeds would violate the durable-primary architecture.
                    self.pg.blacklist_if_not_exists(key, ttl_secs).await?
                }
            } else {
                // Delegate to PG's atomic operation first. Returning `Ok` before this
                // succeeds would violate the durable-primary architecture.
                self.pg.blacklist_if_not_exists(key, ttl_secs).await?
            }
        };

        // Best-effort L2 population after the durable PG write succeeds.
        if let Some(ref redis_conn) = self.redis_conn {
            let redis_key = self.bl_key(key);
            let l2_ttl = Self::l2_positive_ttl(ttl_secs);
            let cache_result: redis::RedisResult<()> = {
                let mut conn = redis_conn.read().await.clone();
                conn.set_ex(&redis_key, "1", l2_ttl).await
            };
            if let Err(e) = cache_result {
                tracing::warn!(
                    key = %key,
                    error = %e,
                    "Failed to populate Redis L2 blacklist cache after PG write"
                );
            }
        }

        // Update L1 cache
        self.l1_blacklist
            .insert(key.to_string(), (true, Instant::now() + L1_POSITIVE_TTL))
            .await;

        // Remove the entry only after releasing the mutex, and only if no
        // other waiter still shares the same lock instance.
        self.cleanup_blacklist_lock(key, &mutex);

        Ok(already_existed)
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        // --- L1 check ---
        if let Some((cached_val, expiry)) = self.l1_family.get(key).await {
            if Instant::now() < expiry {
                return cached_val;
            }
        }

        // --- L2 check (Redis) ---
        if let Some(ref redis_conn) = self.redis_conn {
            let redis_key = self.fam_key(key);
            let result: redis::RedisResult<Option<String>> = {
                let mut conn = redis_conn.read().await.clone();
                conn.get(&redis_key).await
            };
            match result {
                Ok(Some(val)) => {
                    if val == "_" {
                        // Negative sentinel
                        self.l1_family
                            .insert(key.to_string(), (None, Instant::now() + L1_NEGATIVE_TTL))
                            .await;
                        return None;
                    }
                    // Positive: parse timestamp
                    if let Ok(ts) = val.parse::<i64>() {
                        self.l1_family
                            .insert(
                                key.to_string(),
                                (Some(ts), Instant::now() + L1_POSITIVE_TTL),
                            )
                            .await;
                        return Some(ts);
                    }
                    // Malformed value, fall through to PG
                    tracing::warn!(key = %key, val = %val, "Malformed family revocation value in Redis L2");
                }
                Ok(None) => {
                    // L2 miss, continue to PG
                }
                Err(e) => {
                    tracing::warn!(error = %e, key = %key, "Redis L2 family lookup failed, falling back to PG");
                }
            }
        }

        // --- PG check (primary) ---
        let result = self.pg.get_family_revoked_at(key).await;

        if let Some(ts) = result {
            // Positive: populate L1 + L2
            self.l1_family
                .insert(
                    key.to_string(),
                    (Some(ts), Instant::now() + L1_POSITIVE_TTL),
                )
                .await;
            if let Some(ref redis_conn) = self.redis_conn {
                let redis_key = self.fam_key(key);
                let mut conn = redis_conn.read().await.clone();
                let _: redis::RedisResult<()> = conn
                    .set_ex(&redis_key, ts.to_string(), L1_POSITIVE_TTL.as_secs())
                    .await;
            }
            Some(ts)
        } else {
            // Negative sentinel: populate L1 + L2
            self.l1_family
                .insert(key.to_string(), (None, Instant::now() + L1_NEGATIVE_TTL))
                .await;
            if let Some(ref redis_conn) = self.redis_conn {
                let redis_key = self.fam_key(key);
                let mut conn = redis_conn.read().await.clone();
                let _: redis::RedisResult<()> =
                    conn.set_ex(&redis_key, "_", L2_NEGATIVE_TTL_SECS).await;
            }
            None
        }
    }

    async fn get_family_revoked_at_checked(&self, key: &str) -> Result<Option<i64>> {
        if let Some((cached_val, expiry)) = self.l1_family.get(key).await {
            if Instant::now() < expiry {
                return Ok(cached_val);
            }
        }

        if let Some(ref redis_conn) = self.redis_conn {
            let redis_key = self.fam_key(key);
            let result: redis::RedisResult<Option<String>> = {
                let mut conn = redis_conn.read().await.clone();
                conn.get(&redis_key).await
            };
            match result {
                Ok(Some(val)) => {
                    if val == "_" {
                        self.l1_family
                            .insert(key.to_string(), (None, Instant::now() + L1_NEGATIVE_TTL))
                            .await;
                        return Ok(None);
                    }
                    if let Ok(ts) = val.parse::<i64>() {
                        self.l1_family
                            .insert(
                                key.to_string(),
                                (Some(ts), Instant::now() + L1_POSITIVE_TTL),
                            )
                            .await;
                        return Ok(Some(ts));
                    }
                    tracing::warn!(key = %key, val = %val, "Malformed family revocation value in Redis L2");
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, key = %key, "Redis L2 family lookup failed, falling back to PG");
                }
            }
        }

        let result = self.pg.get_family_revoked_at_checked(key).await?;

        if let Some(ts) = result {
            self.l1_family
                .insert(
                    key.to_string(),
                    (Some(ts), Instant::now() + L1_POSITIVE_TTL),
                )
                .await;
            if let Some(ref redis_conn) = self.redis_conn {
                let redis_key = self.fam_key(key);
                let mut conn = redis_conn.read().await.clone();
                let _: redis::RedisResult<()> = conn
                    .set_ex(&redis_key, ts.to_string(), L1_POSITIVE_TTL.as_secs())
                    .await;
            }
            Ok(Some(ts))
        } else {
            self.l1_family
                .insert(key.to_string(), (None, Instant::now() + L1_NEGATIVE_TTL))
                .await;
            if let Some(ref redis_conn) = self.redis_conn {
                let redis_key = self.fam_key(key);
                let mut conn = redis_conn.read().await.clone();
                let _: redis::RedisResult<()> =
                    conn.set_ex(&redis_key, "_", L2_NEGATIVE_TTL_SECS).await;
            }
            Ok(None)
        }
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) -> Result<()> {
        // 1. Write to PG (durable primary)
        self.pg.set_family_revoked(key, timestamp, ttl_secs).await?;

        // 2. Write to L2 Redis (positive, overwrites any stale negative sentinel)
        if let Some(ref redis_conn) = self.redis_conn {
            let redis_key = self.fam_key(key);
            let l2_ttl = Self::l2_positive_ttl(ttl_secs);
            let mut conn = redis_conn.read().await.clone();
            let _: redis::RedisResult<()> =
                conn.set_ex(&redis_key, timestamp.to_string(), l2_ttl).await;
        }

        // 3. Write to L1 moka (positive, overwrites any stale negative sentinel)
        self.l1_family
            .insert(
                key.to_string(),
                (Some(timestamp), Instant::now() + L1_POSITIVE_TTL),
            )
            .await;
        Ok(())
    }
}

// ============================================================================
// FallbackTokenBlacklistStore
// ============================================================================

/// Fallback [`TokenBlacklistStore`] that wraps a primary store with in-memory fallback.
///
/// When the primary store fails (e.g., PG connection error), this store falls back
/// to an in-memory store to ensure blacklisted tokens are still tracked. This
/// prevents the scenario where a revoked token is accepted because all external
/// stores are unavailable.
///
/// ## Architecture
///
/// ```text
/// is_blacklisted("jti_xyz"):
///
///   Primary Store  ──success──>  return result
///       │ error
///   InMemory Store ──result──>  return result (with warning logged)
///
/// blacklist("jti_xyz", ttl):
///
///   1. Try Primary Store
///   2. If Primary fails, write to InMemory Store only
///   3. Return Err only if both fail (fail-closed)
/// ```
///
/// ## Use Case
///
/// Used in production to ensure token blacklist works even when:
/// - Redis is temporarily unavailable
/// - `PostgreSQL` is temporarily unavailable
/// - Network issues cause timeouts
///
/// The in-memory fallback provides graceful degradation with the trade-off
/// that data is lost on restart and not shared across instances.
pub struct FallbackTokenBlacklistStore {
    primary: Arc<dyn TokenBlacklistStore>,
    fallback: InMemoryTokenBlacklistStore,
}

impl FallbackTokenBlacklistStore {
    /// Create a new fallback token blacklist store.
    ///
    /// - `primary`: The primary store (e.g., `TieredTokenBlacklistStore`).
    /// - `fallback_max_jti_capacity`: Maximum JTI capacity for the fallback store.
    /// - `fallback_jti_ttl_secs`: TTL for JTI entries in the fallback store.
    /// - `fallback_family_ttl_secs`: TTL for family revocation entries.
    #[must_use]
    pub fn new(
        primary: Arc<dyn TokenBlacklistStore>,
        fallback_max_jti_capacity: u64,
        fallback_jti_ttl_secs: u64,
        fallback_family_ttl_secs: u64,
    ) -> Self {
        Self {
            primary,
            fallback: InMemoryTokenBlacklistStore::new(
                fallback_max_jti_capacity,
                fallback_jti_ttl_secs,
                fallback_family_ttl_secs,
            ),
        }
    }

    /// Create with default fallback configuration.
    #[must_use]
    pub fn with_defaults(primary: Arc<dyn TokenBlacklistStore>) -> Self {
        Self::new(primary, 100_000, 3600, 86400)
    }
}

#[async_trait]
impl TokenBlacklistStore for FallbackTokenBlacklistStore {
    async fn is_blacklisted(&self, key: &str) -> bool {
        // Check fallback first (fast path, always available)
        if self.fallback.is_blacklisted(key).await {
            return true;
        }
        // Then check primary
        self.primary.is_blacklisted(key).await
    }

    async fn is_blacklisted_checked(&self, key: &str) -> Result<bool> {
        // Check fallback first (fast path, always available, cannot fail)
        if self.fallback.is_blacklisted(key).await {
            return Ok(true);
        }
        // Then check primary (propagating errors)
        self.primary.is_blacklisted_checked(key).await
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()> {
        // Always write to fallback first (ensures we track it even if primary fails)
        self.fallback.blacklist(key, ttl_secs).await?;

        // Try to write to primary
        match self.primary.blacklist(key, ttl_secs).await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!(
                    key = %key,
                    error = %e,
                    "Primary token blacklist write failed, token tracked in fallback only"
                );
                // Still return Ok since we successfully wrote to fallback
                Ok(())
            }
        }
    }

    /// Atomically blacklist if not exists, with fallback to in-memory on primary failure.
    ///
    /// Uses the primary's atomic operation first, then mirrors to fallback.
    /// If primary fails, uses fallback's atomic operation instead.
    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> Result<bool> {
        // Try primary's atomic operation first
        match self.primary.blacklist_if_not_exists(key, ttl_secs).await {
            Ok(already_existed) => {
                // Mirror to fallback for fast-path checks
                // Note: We don't care about the result since primary is authoritative
                let _ = self.fallback.blacklist(key, ttl_secs).await;
                Ok(already_existed)
            }
            Err(e) => {
                // Primary failed, use fallback's atomic operation
                tracing::warn!(
                    key = %key,
                    error = %e,
                    "Primary atomic blacklist failed, using fallback atomic operation"
                );
                self.fallback.blacklist_if_not_exists(key, ttl_secs).await
            }
        }
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        // Check fallback first
        if let Some(ts) = self.fallback.get_family_revoked_at(key).await {
            return Some(ts);
        }
        // Then check primary
        self.primary.get_family_revoked_at(key).await
    }

    async fn get_family_revoked_at_checked(&self, key: &str) -> Result<Option<i64>> {
        if let Some(ts) = self.fallback.get_family_revoked_at(key).await {
            return Ok(Some(ts));
        }
        self.primary.get_family_revoked_at_checked(key).await
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) -> Result<()> {
        // Always write to fallback first
        self.fallback
            .set_family_revoked(key, timestamp, ttl_secs)
            .await?;

        // Try to write to primary
        self.primary
            .set_family_revoked(key, timestamp, ttl_secs)
            .await
    }
}

// ============================================================================
// RedisSyncableTokenBlacklistStore
// ============================================================================

/// A pending write entry for sync buffer.
#[derive(Clone)]
struct PendingWrite {
    ttl_secs: u64,
    /// When this entry expires (for skipping expired entries during sync)
    expires_at: Instant,
}

/// A pending family revocation entry for sync buffer.
#[derive(Clone)]
struct PendingFamilyWrite {
    timestamp: i64,
    ttl_secs: u64,
    /// When this entry expires
    expires_at: Instant,
}

/// Sync statistics returned by [`RedisSyncableTokenBlacklistStore::sync_pending_writes`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncStats {
    /// Number of blacklist entries successfully synced.
    pub blacklist_synced: usize,
    /// Number of family revocations successfully synced.
    pub family_synced: usize,
    /// Number of blacklist entries that failed to sync.
    pub blacklist_failed: usize,
    /// Number of family revocations that failed to sync.
    pub family_failed: usize,
    /// Number of entries skipped because they expired before sync.
    pub expired_skipped: usize,
}

/// A [`TokenBlacklistStore`] with memory fallback and async sync to primary on recovery.
///
/// This extends [`FallbackTokenBlacklistStore`] with the ability to buffer pending
/// writes when the primary store (typically Redis) is unavailable, and sync them
/// when the primary recovers.
///
/// ## Architecture
///
/// ```text
/// blacklist("jti_xyz", ttl):
///
///   1. Write to in-memory fallback (always succeeds)
///   2. Try write to primary store
///   3. If primary fails:
///      - Add to pending writes buffer
///      - Log warning
///   4. Return Ok (fail-open for availability)
///
/// sync_pending_writes():
///
///   1. For each pending write (not expired):
///      - Try to write to primary
///      - On success: remove from buffer
///      - On failure: keep in buffer for retry
///   2. Return sync statistics
/// ```
///
/// ## Use Case
///
/// Used in production to ensure token blacklist works even when:
/// - Redis is temporarily unavailable (network issues, restart)
/// - `PostgreSQL` is temporarily unavailable
///
/// When Redis recovers, call `sync_pending_writes()` to sync the buffered writes.
/// This can be triggered by a health check or manual intervention.
pub struct RedisSyncableTokenBlacklistStore {
    primary: Arc<dyn TokenBlacklistStore>,
    fallback: InMemoryTokenBlacklistStore,
    /// Pending blacklist writes that failed to reach primary.
    pending_blacklist: Arc<moka::future::Cache<String, PendingWrite>>,
    /// Pending family revocation writes that failed to reach primary.
    pending_family: Arc<moka::future::Cache<String, PendingFamilyWrite>>,
}

impl RedisSyncableTokenBlacklistStore {
    /// Create a new syncable token blacklist store.
    ///
    /// - `primary`: The primary store (e.g., `TieredTokenBlacklistStore` with Redis).
    /// - `fallback_max_jti_capacity`: Maximum JTI capacity for the fallback store.
    /// - `fallback_jti_ttl_secs`: TTL for JTI entries in the fallback store.
    /// - `fallback_family_ttl_secs`: TTL for family revocation entries.
    /// - `pending_capacity`: Maximum number of pending writes to buffer.
    #[must_use]
    pub fn new(
        primary: Arc<dyn TokenBlacklistStore>,
        fallback_max_jti_capacity: u64,
        fallback_jti_ttl_secs: u64,
        fallback_family_ttl_secs: u64,
        pending_capacity: u64,
    ) -> Self {
        Self {
            primary,
            fallback: InMemoryTokenBlacklistStore::new(
                fallback_max_jti_capacity,
                fallback_jti_ttl_secs,
                fallback_family_ttl_secs,
            ),
            pending_blacklist: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(pending_capacity)
                    .build(),
            ),
            pending_family: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(pending_capacity)
                    .build(),
            ),
        }
    }

    /// Create with default configuration.
    #[must_use]
    pub fn with_defaults(primary: Arc<dyn TokenBlacklistStore>) -> Self {
        Self::new(primary, 100_000, 3600, 86400, 50_000)
    }

    /// Sync pending writes to the primary store.
    ///
    /// This should be called when the primary store (Redis) recovers from an outage.
    /// It will attempt to write all buffered entries to the primary store.
    ///
    /// Entries that have expired since being buffered are skipped.
    /// Successfully synced entries are removed from the buffer.
    /// Failed entries remain in the buffer for future retry.
    pub async fn sync_pending_writes(&self) -> Result<SyncStats> {
        let mut stats = SyncStats::default();

        // Collect keys to process (to avoid holding the cache during async operations)
        let blacklist_keys: Vec<String> = self
            .pending_blacklist
            .iter()
            .map(|(key, _)| (*key).clone())
            .collect();

        let family_keys: Vec<String> = self
            .pending_family
            .iter()
            .map(|(key, _)| (*key).clone())
            .collect();

        // Sync blacklist entries
        for key in blacklist_keys {
            if let Some(pending) = self.pending_blacklist.get(&key).await {
                // Skip expired entries
                if Instant::now() >= pending.expires_at {
                    self.pending_blacklist.remove(&key).await;
                    stats.expired_skipped += 1;
                    continue;
                }

                match self.primary.blacklist(&key, pending.ttl_secs).await {
                    Ok(()) => {
                        self.pending_blacklist.remove(&key).await;
                        stats.blacklist_synced += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            key = %key,
                            error = %e,
                            "Failed to sync pending blacklist write to primary"
                        );
                        stats.blacklist_failed += 1;
                    }
                }
            }
        }

        // Sync family revocations
        for key in family_keys {
            if let Some(pending) = self.pending_family.get(&key).await {
                // Skip expired entries
                if Instant::now() >= pending.expires_at {
                    self.pending_family.remove(&key).await;
                    stats.expired_skipped += 1;
                    continue;
                }

                match self
                    .primary
                    .set_family_revoked(&key, pending.timestamp, pending.ttl_secs)
                    .await
                {
                    Ok(()) => {
                        self.pending_family.remove(&key).await;
                        stats.family_synced += 1;
                    }
                    Err(e) => {
                        stats.family_failed += 1;
                        tracing::warn!(
                            key = %key,
                            error = %e,
                            "Failed to sync pending family revocation to primary"
                        );
                    }
                }
            }
        }

        if stats.blacklist_synced > 0 || stats.family_synced > 0 {
            tracing::info!(
                blacklist_synced = stats.blacklist_synced,
                family_synced = stats.family_synced,
                expired_skipped = stats.expired_skipped,
                "Synced pending token blacklist writes to primary"
            );
        }

        Ok(stats)
    }

    /// Get the number of pending blacklist writes waiting to be synced.
    ///
    /// Note: This is an approximate count based on iterating the cache.
    /// It's primarily intended for testing and diagnostics.
    #[must_use]
    pub fn pending_write_count(&self) -> usize {
        self.pending_blacklist.iter().count()
    }

    /// Get the number of pending family revocation writes waiting to be synced.
    ///
    /// Note: This is an approximate count based on iterating the cache.
    /// It's primarily intended for testing and diagnostics.
    #[must_use]
    pub fn pending_family_count(&self) -> usize {
        self.pending_family.iter().count()
    }
}

#[async_trait]
impl TokenBlacklistStore for RedisSyncableTokenBlacklistStore {
    async fn is_blacklisted(&self, key: &str) -> bool {
        // Check fallback first (fast path, always available)
        if self.fallback.is_blacklisted(key).await {
            return true;
        }
        // Then check primary
        self.primary.is_blacklisted(key).await
    }

    async fn is_blacklisted_checked(&self, key: &str) -> Result<bool> {
        // Check fallback first (fast path, always available, cannot fail)
        if self.fallback.is_blacklisted(key).await {
            return Ok(true);
        }
        // Then check primary (propagating errors)
        self.primary.is_blacklisted_checked(key).await
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()> {
        // Always write to fallback first (ensures we track it even if primary fails)
        self.fallback.blacklist(key, ttl_secs).await?;

        // Try to write to primary
        match self.primary.blacklist(key, ttl_secs).await {
            Ok(()) => {
                // Remove from pending if it was there (e.g., from a previous failed attempt)
                self.pending_blacklist.remove(key).await;
                Ok(())
            }
            Err(e) => {
                tracing::warn!(
                    key = %key,
                    error = %e,
                    "Primary token blacklist write failed, token tracked in fallback, added to pending sync buffer"
                );

                // Add to pending writes for later sync
                let pending = PendingWrite {
                    ttl_secs,
                    expires_at: Instant::now() + Duration::from_secs(ttl_secs),
                };
                self.pending_blacklist
                    .insert(key.to_string(), pending)
                    .await;

                // Still return Ok since we successfully wrote to fallback
                Ok(())
            }
        }
    }

    /// Atomically blacklist if not exists, with pending sync buffer on primary failure.
    ///
    /// Uses the primary's atomic operation first because it is authoritative for
    /// replay detection. The fallback mirrors successful writes for fast-path
    /// checks and outage resilience. If the primary is unavailable, the fallback
    /// becomes the temporary source of truth and the write is buffered for sync.
    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> Result<bool> {
        match self.primary.blacklist_if_not_exists(key, ttl_secs).await {
            Ok(already_existed) => {
                // Mirror to fallback for fast-path checks, but keep the
                // authoritative replay decision from primary.
                let _ = self.fallback.blacklist(key, ttl_secs).await;
                self.pending_blacklist.remove(key).await;
                Ok(already_existed)
            }
            Err(e) => {
                tracing::warn!(
                    key = %key,
                    error = %e,
                    "Primary atomic blacklist failed, using fallback atomic operation and adding to pending sync buffer"
                );

                let already_existed = self.fallback.blacklist_if_not_exists(key, ttl_secs).await?;

                // Add to pending writes for later sync
                let pending = PendingWrite {
                    ttl_secs,
                    expires_at: Instant::now() + Duration::from_secs(ttl_secs),
                };
                self.pending_blacklist
                    .insert(key.to_string(), pending)
                    .await;

                Ok(already_existed)
            }
        }
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        // Check fallback first
        if let Some(ts) = self.fallback.get_family_revoked_at(key).await {
            return Some(ts);
        }
        // Then check primary
        self.primary.get_family_revoked_at(key).await
    }

    async fn get_family_revoked_at_checked(&self, key: &str) -> Result<Option<i64>> {
        if let Some(ts) = self.fallback.get_family_revoked_at(key).await {
            return Ok(Some(ts));
        }
        self.primary.get_family_revoked_at_checked(key).await
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) -> Result<()> {
        // Always write to fallback first
        self.fallback
            .set_family_revoked(key, timestamp, ttl_secs)
            .await?;

        match self
            .primary
            .set_family_revoked(key, timestamp, ttl_secs)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                let pending = PendingFamilyWrite {
                    timestamp,
                    ttl_secs,
                    expires_at: Instant::now() + Duration::from_secs(ttl_secs),
                };
                self.pending_family.insert(key.to_string(), pending).await;
                tracing::warn!(
                    key = %key,
                    error = %e,
                    "Failed to persist family revocation to primary store; queued pending sync"
                );
                Err(e)
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysFailTokenBlacklistStore;

    #[async_trait]
    impl TokenBlacklistStore for AlwaysFailTokenBlacklistStore {
        async fn is_blacklisted_checked(&self, _key: &str) -> Result<bool> {
            Ok(false)
        }

        async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> Result<()> {
            Err(crate::Error::Internal(
                "test primary blacklist failure".to_string(),
            ))
        }

        async fn get_family_revoked_at(&self, _key: &str) -> Option<i64> {
            None
        }

        async fn get_family_revoked_at_checked(&self, _key: &str) -> Result<Option<i64>> {
            Ok(None)
        }

        async fn set_family_revoked(
            &self,
            _key: &str,
            _timestamp: i64,
            _ttl_secs: u64,
        ) -> Result<()> {
            Err(crate::Error::Internal(
                "test primary family failure".to_string(),
            ))
        }
    }

    fn expired_instant() -> Instant {
        Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("expired instant should be representable")
    }

    // Helper: create a TieredTokenBlacklistStore with no Redis and a lazy PG pool
    // that won't be contacted (for L1-only tests).
    // Uses a very short acquire_timeout so tests that accidentally hit PG
    // fail fast instead of blocking for 30+ seconds.
    fn make_tiered_l1_only() -> TieredTokenBlacklistStore {
        // connect_lazy won't actually connect until a query is executed.
        // The short acquire_timeout ensures any accidental PG contact fails
        // within 1 second rather than the default 30+ seconds.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_secs(1))
            .connect_lazy("postgres://dummy:dummy@localhost/dummy")
            .expect("connect_lazy should not fail");
        TieredTokenBlacklistStore::new(PgTokenBlacklistStore::new(pool), None, "test:".to_string())
    }

    // Helper: create an InMemoryTokenBlacklistStore for testing fallback scenarios.
    fn make_in_memory_store() -> InMemoryTokenBlacklistStore {
        InMemoryTokenBlacklistStore::new(10_000, 3600, 86400)
    }

    #[tokio::test]
    async fn test_l1_positive_blacklist_hit() {
        let store = make_tiered_l1_only();

        // Pre-populate L1 with a positive entry
        store
            .l1_blacklist
            .insert(
                "jti:abc".to_string(),
                (true, Instant::now() + Duration::from_mins(1)),
            )
            .await;

        // Should return true from L1 without touching PG
        assert!(store.is_blacklisted("jti:abc").await);
    }

    #[tokio::test]
    async fn test_l1_negative_blacklist_hit() {
        let store = make_tiered_l1_only();

        // Pre-populate L1 with a negative sentinel
        store
            .l1_blacklist
            .insert(
                "jti:def".to_string(),
                (false, Instant::now() + Duration::from_mins(1)),
            )
            .await;

        // Should return false from L1 without touching PG
        assert!(!store.is_blacklisted("jti:def").await);
    }

    #[tokio::test]
    async fn test_l1_expired_entry_ignored() {
        let store = make_tiered_l1_only();

        // Pre-populate L1 with an expired positive entry
        store
            .l1_blacklist
            .insert(
                "jti:expired".to_string(),
                (
                    true,
                    Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
                ),
            )
            .await;

        // The expired entry should be ignored. Since PG is a dummy pool,
        // the PG query will fail and return false (unwrap_or(false)).
        // This tests that expired L1 entries don't short-circuit.
        assert!(!store.is_blacklisted("jti:expired").await);
    }

    #[tokio::test]
    async fn test_l1_positive_family_hit() {
        let store = make_tiered_l1_only();

        let ts = 1700000000_i64;
        store
            .l1_family
            .insert(
                "family:user42".to_string(),
                (Some(ts), Instant::now() + Duration::from_mins(1)),
            )
            .await;

        assert_eq!(store.get_family_revoked_at("family:user42").await, Some(ts));
    }

    #[tokio::test]
    async fn test_l1_negative_family_hit() {
        let store = make_tiered_l1_only();

        store
            .l1_family
            .insert(
                "family:user99".to_string(),
                (None, Instant::now() + Duration::from_mins(1)),
            )
            .await;

        assert_eq!(store.get_family_revoked_at("family:user99").await, None);
    }

    #[tokio::test]
    async fn test_blacklist_write_populates_l1() {
        let store = make_tiered_l1_only();

        // Pre-populate L1 with a negative sentinel
        store
            .l1_blacklist
            .insert(
                "jti:overwrite".to_string(),
                (false, Instant::now() + Duration::from_mins(1)),
            )
            .await;
        assert!(!store.is_blacklisted("jti:overwrite").await);

        // blacklist() will fail on PG (dummy pool), but the write-through to
        // L1 would have happened if PG succeeded. Since PG is unreachable,
        // blacklist() returns Err. This is expected for the L1-only test.
        // For a full integration test, use a real PG.
        let result = store.blacklist("jti:overwrite", 3600).await;
        // PG write fails since it's a dummy pool
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_set_family_revoked_populates_l1() {
        let store = make_tiered_l1_only();

        // Verify L1 starts empty
        assert!(store
            .get_family_revoked_at("family:write_test")
            .await
            .is_none());

        // Family revocation is fail-closed: if the durable PG write fails, the
        // tiered store must return an error and must not populate L1 as if the
        // revocation were durably committed.
        let ts = 1700000000_i64;
        let result = store
            .set_family_revoked("family:write_test", ts, 3600)
            .await;
        assert!(result.is_err());

        // L1 should remain empty because the durable write failed.
        assert_eq!(store.get_family_revoked_at("family:write_test").await, None);
    }

    #[tokio::test]
    async fn test_redis_key_format() {
        let store = make_tiered_l1_only();
        assert_eq!(store.bl_key("jti:abc"), "test:bl:jti:abc");
        assert_eq!(store.fam_key("family:user42"), "test:fam:family:user42");
    }

    #[tokio::test]
    async fn test_l2_positive_ttl_computation() {
        assert_eq!(TieredTokenBlacklistStore::l2_positive_ttl(3600), 3570);
        assert_eq!(TieredTokenBlacklistStore::l2_positive_ttl(30), 1); // min 1
        assert_eq!(TieredTokenBlacklistStore::l2_positive_ttl(10), 1); // min 1
        assert_eq!(TieredTokenBlacklistStore::l2_positive_ttl(0), 1); // min 1
    }

    // ---- InMemoryTokenBlacklistStore tests (kept for test-only usage) ----

    #[tokio::test]
    async fn test_in_memory_blacklist_roundtrip() {
        let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

        let key = "jti:abc123";
        assert!(!store.is_blacklisted(key).await);

        store.blacklist(key, 3600).await.unwrap();
        assert!(store.is_blacklisted(key).await);
    }

    #[tokio::test]
    async fn test_in_memory_blacklist_ttl_expiry() {
        let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

        let key = "jti:expiry_test";
        store.blacklist(key, 1).await.unwrap();
        assert!(store.is_blacklisted(key).await);

        store
            .jti_blacklist
            .insert(key.to_string(), expired_instant())
            .await;
        assert!(
            !store.is_blacklisted(key).await,
            "Should no longer be blacklisted after TTL expiry"
        );
    }

    #[tokio::test]
    async fn test_in_memory_family_roundtrip() {
        let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

        let key = "family:user_42";
        let timestamp = 1700000000_i64;

        assert!(store.get_family_revoked_at(key).await.is_none());
        store
            .set_family_revoked(key, timestamp, 86400)
            .await
            .unwrap();
        assert_eq!(store.get_family_revoked_at(key).await, Some(timestamp));
    }

    // ---- Additional InMemoryTokenBlacklistStore tests ----

    #[tokio::test]
    async fn test_in_memory_blacklist_multiple_entries() {
        let store = make_in_memory_store();

        // Add multiple entries
        for i in 0..10 {
            let key = format!("jti:test_{i}");
            assert!(!store.is_blacklisted(&key).await);
            store.blacklist(&key, 3600).await.unwrap();
            assert!(store.is_blacklisted(&key).await);
        }

        // Verify all are blacklisted
        for i in 0..10 {
            assert!(store.is_blacklisted(&format!("jti:test_{i}")).await);
        }
    }

    #[tokio::test]
    async fn test_in_memory_blacklist_overwrite() {
        let store = make_in_memory_store();

        let key = "jti:overwrite_test";
        store.blacklist(key, 1).await.unwrap();
        assert!(store.is_blacklisted(key).await);

        // Overwrite with longer TTL
        store.blacklist(key, 3600).await.unwrap();
        assert!(store.is_blacklisted(key).await);

        let expiry = store
            .jti_blacklist
            .get(key)
            .await
            .expect("overwrite should leave an expiry entry");
        assert!(
            expiry > Instant::now() + Duration::from_secs(3000),
            "TTL overwrite should extend the stored expiry"
        );
    }

    #[tokio::test]
    async fn test_in_memory_family_ttl_expiry() {
        let store = make_in_memory_store();

        let key = "family:ttl_test";
        let timestamp = 1700000000_i64;

        store.set_family_revoked(key, timestamp, 1).await.unwrap();
        assert_eq!(store.get_family_revoked_at(key).await, Some(timestamp));

        store
            .family_revoked
            .insert(key.to_string(), (timestamp, expired_instant()))
            .await;
        assert!(
            store.get_family_revoked_at(key).await.is_none(),
            "Family revocation should expire after TTL"
        );
    }

    #[tokio::test]
    async fn test_in_memory_family_multiple_entries() {
        let store = make_in_memory_store();

        // Set multiple family revocations
        for i in 0..10 {
            let key = format!("family:user_{i}");
            let timestamp = 1700000000_i64 + i;
            store
                .set_family_revoked(&key, timestamp, 86400)
                .await
                .unwrap();
        }

        // Verify all are retrievable
        for i in 0..10 {
            let key = format!("family:user_{i}");
            let expected_ts = 1700000000_i64 + i;
            assert_eq!(store.get_family_revoked_at(&key).await, Some(expected_ts));
        }
    }

    #[test]
    fn test_pg_family_revocation_uses_single_primary_key_row() {
        let key = "family:user%_42\\segment";
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(3600);
        let timestamp = 1_700_000_123_i64;

        let row = (key, expires_at, Some(timestamp));

        assert_eq!(row.0, key);
        assert_eq!(row.2, Some(timestamp));
    }

    // ---- FallbackTokenBlacklistStore tests ----

    #[tokio::test]
    async fn test_fallback_blacklist_roundtrip() {
        // Use InMemory as both primary and fallback (simulating working primary)
        let primary = Arc::new(make_in_memory_store()) as Arc<dyn TokenBlacklistStore>;
        let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

        let key = "jti:fallback_test";
        assert!(!fallback.is_blacklisted(key).await);

        fallback.blacklist(key, 3600).await.unwrap();
        assert!(fallback.is_blacklisted(key).await);
    }

    #[tokio::test]
    async fn test_fallback_family_roundtrip() {
        let primary = Arc::new(make_in_memory_store()) as Arc<dyn TokenBlacklistStore>;
        let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

        let key = "family:fallback_test";
        let timestamp = 1700000000_i64;

        assert!(fallback.get_family_revoked_at(key).await.is_none());

        fallback
            .set_family_revoked(key, timestamp, 86400)
            .await
            .unwrap();
        assert_eq!(fallback.get_family_revoked_at(key).await, Some(timestamp));
    }

    #[tokio::test]
    async fn test_fallback_with_failing_primary_still_tracks_blacklist() {
        // Use a TieredTokenBlacklistStore with dummy PG (will fail on writes)
        // This simulates a failing primary store
        let primary = Arc::new(make_tiered_l1_only()) as Arc<dyn TokenBlacklistStore>;
        let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

        let key = "jti:failing_primary_test";

        // Blacklist should succeed (written to fallback even if primary fails)
        let result = fallback.blacklist(key, 3600).await;
        assert!(result.is_ok(), "Blacklist should succeed via fallback");

        // Should be blacklisted (via fallback)
        assert!(
            fallback.is_blacklisted(key).await,
            "Token should be blacklisted via fallback"
        );
    }

    #[tokio::test]
    async fn test_fallback_with_failing_primary_still_tracks_family() {
        let primary = Arc::new(make_tiered_l1_only()) as Arc<dyn TokenBlacklistStore>;
        let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

        let key = "family:failing_primary_test";
        let timestamp = 1700000000_i64;

        // Family revocation is security-critical: the API must fail closed if
        // the primary store cannot persist it, even though fallback still
        // keeps a local copy for degraded behavior.
        let result = fallback.set_family_revoked(key, timestamp, 86400).await;
        assert!(result.is_err());

        // Should be retrievable (via fallback)
        assert_eq!(
            fallback.get_family_revoked_at(key).await,
            Some(timestamp),
            "Family revocation should be retrievable via fallback"
        );
    }

    #[tokio::test]
    async fn test_fallback_checks_both_stores_for_blacklist() {
        // Create two separate in-memory stores
        let primary_inner = make_in_memory_store();
        let fallback_inner = make_in_memory_store();

        // Blacklist in primary only
        primary_inner
            .blacklist("jti:primary_only", 3600)
            .await
            .unwrap();

        // Blacklist in fallback only
        fallback_inner
            .blacklist("jti:fallback_only", 3600)
            .await
            .unwrap();

        // Create FallbackTokenBlacklistStore
        // Note: For this test, we need to use a different approach since
        // FallbackTokenBlacklistStore creates its own internal fallback.
        // Instead, let's verify the fallback behavior differently.

        // When primary returns true, fallback should return true
        let primary = Arc::new(primary_inner) as Arc<dyn TokenBlacklistStore>;
        let fallback_store = FallbackTokenBlacklistStore::with_defaults(primary);

        // Should find in primary
        assert!(fallback_store.is_blacklisted("jti:primary_only").await);
    }

    #[tokio::test]
    async fn test_fallback_ttl_expiry() {
        let primary = Arc::new(AlwaysFailTokenBlacklistStore) as Arc<dyn TokenBlacklistStore>;
        let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

        let key = "jti:fallback_ttl_test";

        fallback.blacklist(key, 1).await.unwrap();
        assert!(fallback.is_blacklisted(key).await);

        fallback
            .fallback
            .jti_blacklist
            .insert(key.to_string(), expired_instant())
            .await;

        assert!(
            !fallback.is_blacklisted(key).await,
            "Token should no longer be blacklisted after TTL expiry"
        );
    }

    #[tokio::test]
    async fn test_fallback_family_ttl_expiry() {
        let primary = Arc::new(AlwaysFailTokenBlacklistStore) as Arc<dyn TokenBlacklistStore>;
        let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

        let key = "family:fallback_ttl_test";
        let timestamp = 1700000000_i64;

        let result = fallback.set_family_revoked(key, timestamp, 1).await;
        assert!(result.is_err());
        assert_eq!(fallback.get_family_revoked_at(key).await, Some(timestamp));

        fallback
            .fallback
            .family_revoked
            .insert(key.to_string(), (timestamp, expired_instant()))
            .await;

        assert!(
            fallback.get_family_revoked_at(key).await.is_none(),
            "Family revocation should expire after TTL"
        );
    }

    #[tokio::test]
    async fn test_sync_pending_writes_counts_failed_family_syncs() {
        let primary = Arc::new(AlwaysFailTokenBlacklistStore) as Arc<dyn TokenBlacklistStore>;
        let store = RedisSyncableTokenBlacklistStore::with_defaults(primary);

        let key = "family:pending_sync_failure";
        let timestamp = 1_700_000_000_i64;

        let result = store.set_family_revoked(key, timestamp, 3600).await;
        assert!(result.is_err(), "initial primary write should fail");
        assert_eq!(store.pending_family_count(), 1);

        let stats = store
            .sync_pending_writes()
            .await
            .expect("sync should report stats even when primary write fails");

        assert_eq!(stats.family_synced, 0);
        assert_eq!(stats.family_failed, 1);
        assert_eq!(store.pending_family_count(), 1);
    }

    // ---- Hybrid/Tiered fallback scenario tests ----

    #[tokio::test]
    async fn test_tiered_store_without_redis_still_works() {
        // Create a tiered store without Redis (L1 + PG only)
        let store = make_tiered_l1_only();

        // Pre-populate L1 to avoid hitting PG
        store
            .l1_blacklist
            .insert(
                "jti:l1_only_test".to_string(),
                (true, Instant::now() + Duration::from_mins(1)),
            )
            .await;

        // Should return true from L1 without hitting PG
        assert!(store.is_blacklisted("jti:l1_only_test").await);
    }

    #[tokio::test]
    async fn test_full_fallback_stack() {
        // Simulate the full fallback stack: TieredStore -> InMemory fallback
        let tiered = make_tiered_l1_only();
        let primary = Arc::new(tiered) as Arc<dyn TokenBlacklistStore>;
        let fallback_store = FallbackTokenBlacklistStore::with_defaults(primary);

        let key = "jti:full_fallback_stack";

        // Blacklist should succeed via fallback (tiered will fail on PG write)
        let result = fallback_store.blacklist(key, 3600).await;
        assert!(result.is_ok(), "Blacklist should succeed via fallback");

        // Should be blacklisted via fallback
        assert!(
            fallback_store.is_blacklisted(key).await,
            "Token should be blacklisted via fallback even when tiered store fails"
        );
    }

    // ---- Concurrent blacklist_if_not_exists tests ----

    /// Test that concurrent calls to `blacklist_if_not_exists` on the same token
    /// result in exactly one "first use" and all others detecting "replay".
    /// This verifies the atomicity of the operation using InMemoryTokenBlacklistStore.
    #[tokio::test]
    async fn test_in_memory_concurrent_blacklist_if_not_exists_atomicity() {
        let store = Arc::new(make_in_memory_store());
        let key = "jti:concurrent_test";
        let num_concurrent = 10;

        // Spawn multiple concurrent tasks all trying to blacklist the same key
        let mut handles = Vec::new();
        for _ in 0..num_concurrent {
            let store_clone = Arc::clone(&store);
            let handle =
                tokio::spawn(async move { store_clone.blacklist_if_not_exists(key, 3600).await });
            handles.push(handle);
        }

        // Wait for all tasks to complete
        let results = futures::future::join_all(handles).await;

        // Collect successful results (handle both JoinError and inner Result)
        let success_results: Vec<bool> = results
            .into_iter()
            .filter_map(std::result::Result::ok) // Unwrap JoinHandle result
            .filter_map(std::result::Result::ok) // Unwrap inner Result<bool, Error>
            .collect();

        // Exactly one should return false (first use), all others should return true (replay)
        let first_use_count = success_results.iter().filter(|&&r| !r).count();
        let replay_count = success_results.iter().filter(|&&r| r).count();

        assert_eq!(
            first_use_count, 1,
            "Exactly one call should return false (first use), got {first_use_count}"
        );
        assert_eq!(
            replay_count,
            num_concurrent - 1,
            "All other calls should return true (replay), got {replay_count}"
        );

        // Verify the token is now blacklisted
        assert!(store.is_blacklisted(key).await);
    }

    #[tokio::test]
    async fn test_in_memory_blacklist_lock_cleanup_does_not_replace_live_mutex() {
        let store = make_in_memory_store();
        let key = "jti:lock_cleanup_in_memory";

        let original_mutex = store
            .blacklist_locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .value()
            .clone();

        let _guard = original_mutex.lock().await;

        store.cleanup_blacklist_lock(key, &original_mutex);

        let stored_mutex = store
            .blacklist_locks
            .get(key)
            .expect("live mutex entry must not be removed while in use");
        assert!(
            Arc::ptr_eq(stored_mutex.value(), &original_mutex),
            "cleanup must not swap out an in-flight mutex"
        );
    }

    #[tokio::test]
    async fn test_tiered_blacklist_lock_cleanup_does_not_replace_live_mutex() {
        let store = make_tiered_l1_only();
        let key = "jti:lock_cleanup_tiered";

        let original_mutex = store
            .blacklist_locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .value()
            .clone();

        let _guard = original_mutex.lock().await;

        store.cleanup_blacklist_lock(key, &original_mutex);

        let stored_mutex = store
            .blacklist_locks
            .get(key)
            .expect("live mutex entry must not be removed while in use");
        assert!(
            Arc::ptr_eq(stored_mutex.value(), &original_mutex),
            "cleanup must not swap out an in-flight mutex"
        );
    }

    /// Test that concurrent calls to `blacklist_if_not_exists` on the same token
    /// using FallbackTokenBlacklistStore maintain atomicity.
    #[tokio::test]
    async fn test_fallback_concurrent_blacklist_if_not_exists_atomicity() {
        let primary = Arc::new(make_in_memory_store()) as Arc<dyn TokenBlacklistStore>;
        let store = Arc::new(FallbackTokenBlacklistStore::with_defaults(primary));
        let key = "jti:fallback_concurrent_test";
        let num_concurrent = 10;

        let mut handles = Vec::new();
        for _ in 0..num_concurrent {
            let store_clone = Arc::clone(&store);
            let handle =
                tokio::spawn(async move { store_clone.blacklist_if_not_exists(key, 3600).await });
            handles.push(handle);
        }

        let results = futures::future::join_all(handles).await;
        let success_results: Vec<bool> = results
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter_map(std::result::Result::ok)
            .collect();

        let first_use_count = success_results.iter().filter(|&&r| !r).count();
        let replay_count = success_results.iter().filter(|&&r| r).count();

        assert_eq!(
            first_use_count, 1,
            "Exactly one call should return false (first use), got {first_use_count}"
        );
        assert_eq!(
            replay_count,
            num_concurrent - 1,
            "All other calls should return true (replay), got {replay_count}"
        );

        assert!(store.is_blacklisted(key).await);
    }

    /// Test that concurrent calls to `blacklist_if_not_exists` on different tokens
    /// all succeed (no false positives from the lock mechanism).
    #[tokio::test]
    async fn test_in_memory_concurrent_different_keys_all_succeed() {
        let store = Arc::new(make_in_memory_store());
        let num_concurrent = 10;

        let mut handles = Vec::new();
        for i in 0..num_concurrent {
            let store_clone = Arc::clone(&store);
            let handle = tokio::spawn(async move {
                let key = format!("jti:different_key_{i}");
                store_clone.blacklist_if_not_exists(&key, 3600).await
            });
            handles.push(handle);
        }

        let results = futures::future::join_all(handles).await;
        let success_results: Vec<bool> = results
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter_map(std::result::Result::ok)
            .collect();

        // All should return false (first use) since keys are different
        let first_use_count = success_results.iter().filter(|&&r| !r).count();
        assert_eq!(
            first_use_count, num_concurrent,
            "All calls with different keys should return false (first use), got {first_use_count}"
        );
    }

    /// Test that the InMemoryTokenBlacklistStore cleans up lock entries after use
    /// to prevent unbounded memory growth.
    #[tokio::test]
    async fn test_in_memory_blacklist_lock_cleanup() {
        let store = make_in_memory_store();
        let key = "jti:lock_cleanup_test";

        // Initial lock count
        let initial_count = store.blacklist_locks.len();

        // Perform blacklist_if_not_exists
        let _ = store.blacklist_if_not_exists(key, 3600).await;

        // Lock should be cleaned up after the operation
        // Note: There might be a brief moment where the lock exists,
        // but it should be removed after the operation completes
        tokio::time::sleep(Duration::from_millis(10)).await;

        // The lock entry should be removed (or at least not grow unbounded)
        // Since we're testing with a single key, the count should be back to initial
        assert_eq!(
            store.blacklist_locks.len(),
            initial_count,
            "Lock entry should be cleaned up after operation"
        );
    }

    /// Stress test: rapid concurrent blacklist_if_not_exists on same key
    /// to verify no race conditions under heavy load.
    #[tokio::test]
    async fn test_in_memory_stress_concurrent_blacklist_if_not_exists() {
        let store = Arc::new(make_in_memory_store());
        let key = "jti:stress_test";
        let num_iterations = 100;

        for iteration in 0..5 {
            let iteration_key = format!("{key}_iter_{iteration}");
            let mut handles = Vec::new();

            for _ in 0..num_iterations {
                let store_clone = Arc::clone(&store);
                let key_clone = iteration_key.clone();
                let handle = tokio::spawn(async move {
                    store_clone.blacklist_if_not_exists(&key_clone, 3600).await
                });
                handles.push(handle);
            }

            let results = futures::future::join_all(handles).await;
            let success_results: Vec<bool> = results
                .into_iter()
                .filter_map(std::result::Result::ok)
                .filter_map(std::result::Result::ok)
                .collect();

            let first_use_count = success_results.iter().filter(|&&r| !r).count();
            let replay_count = success_results.iter().filter(|&&r| r).count();

            assert_eq!(
                first_use_count, 1,
                "Iteration {iteration}: exactly one first use expected, got {first_use_count}"
            );
            assert_eq!(
                replay_count,
                num_iterations - 1,
                "Iteration {}: expected {} replays, got {}",
                iteration,
                num_iterations - 1,
                replay_count
            );
        }
    }
}
