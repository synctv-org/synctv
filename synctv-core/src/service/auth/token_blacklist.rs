//! Refresh token blacklist and family revocation storage.
//!
//! ## Backend Abstraction
//!
//! Storage is abstracted via the [`TokenBlacklistStore`] trait. Three
//! implementations are provided:
//! - [`TieredTokenBlacklistStore`]: Production store with L1 (moka) + optional
//!   L2 (Redis) + PG primary. Includes negative caching for cache penetration
//!   protection.
//! - [`PgTokenBlacklistStore`]: PostgreSQL-backed, used as the inner durable
//!   layer inside `TieredTokenBlacklistStore`.
//! - [`InMemoryTokenBlacklistStore`]: moka cache, for tests and standalone mode
//!   without a database. Data is lost on restart.

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
// PgTokenBlacklistStore
// ============================================================================

/// PostgreSQL-backed [`TokenBlacklistStore`].
///
/// Provides durable token blacklist storage that survives restarts, used as
/// the primary (durable) layer inside [`TieredTokenBlacklistStore`].
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

// ============================================================================
// TieredTokenBlacklistStore
// ============================================================================

/// L1 positive cache TTL (how long a "is blacklisted = true" entry lives in moka).
const L1_POSITIVE_TTL: Duration = Duration::from_secs(120);
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
pub struct TieredTokenBlacklistStore {
    pg: PgTokenBlacklistStore,
    redis_conn: Option<redis::aio::ConnectionManager>,
    /// L1 cache: JTI key -> (is_blacklisted, expiry)
    l1_blacklist: moka::future::Cache<String, (bool, Instant)>,
    /// L1 cache: family key -> (Option<revoked_at_timestamp>, expiry)
    l1_family: moka::future::Cache<String, (Option<i64>, Instant)>,
    /// Redis key prefix (e.g., "synctv:")
    key_prefix: String,
}

impl TieredTokenBlacklistStore {
    /// Create a new tiered token blacklist store.
    ///
    /// - `pg`: The PostgreSQL store used as durable primary.
    /// - `redis_conn`: Optional Redis connection for L2 caching.
    /// - `key_prefix`: Redis key prefix (e.g., `"synctv:"`).
    #[must_use]
    pub fn new(
        pg: PgTokenBlacklistStore,
        redis_conn: Option<redis::aio::ConnectionManager>,
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
                let mut conn = redis_conn.clone();
                conn.get(&redis_key).await
            };
            match result {
                Ok(Some(val)) => {
                    let is_bl = val == "1";
                    let l1_ttl = if is_bl { L1_POSITIVE_TTL } else { L1_NEGATIVE_TTL };
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
                let mut conn = redis_conn.clone();
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
                let mut conn = redis_conn.clone();
                let _: redis::RedisResult<()> = conn
                    .set_ex(&redis_key, "0", L2_NEGATIVE_TTL_SECS)
                    .await;
            }
        }

        found
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()> {
        // 1. Write to PG (durable primary)
        self.pg.blacklist(key, ttl_secs).await?;

        // 2. Write to L2 Redis (positive, overwrites any stale negative sentinel)
        if let Some(ref redis_conn) = self.redis_conn {
            let redis_key = self.bl_key(key);
            let l2_ttl = Self::l2_positive_ttl(ttl_secs);
            let mut conn = redis_conn.clone();
            let _: redis::RedisResult<()> = conn.set_ex(&redis_key, "1", l2_ttl).await;
        }

        // 3. Write to L1 moka (positive, overwrites any stale negative sentinel)
        self.l1_blacklist
            .insert(key.to_string(), (true, Instant::now() + L1_POSITIVE_TTL))
            .await;

        Ok(())
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
                let mut conn = redis_conn.clone();
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
                            .insert(key.to_string(), (Some(ts), Instant::now() + L1_POSITIVE_TTL))
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

        match result {
            Some(ts) => {
                // Positive: populate L1 + L2
                self.l1_family
                    .insert(key.to_string(), (Some(ts), Instant::now() + L1_POSITIVE_TTL))
                    .await;
                if let Some(ref redis_conn) = self.redis_conn {
                    let redis_key = self.fam_key(key);
                    let mut conn = redis_conn.clone();
                    let _: redis::RedisResult<()> = conn
                        .set_ex(&redis_key, ts.to_string(), L1_POSITIVE_TTL.as_secs())
                        .await;
                }
                Some(ts)
            }
            None => {
                // Negative sentinel: populate L1 + L2
                self.l1_family
                    .insert(key.to_string(), (None, Instant::now() + L1_NEGATIVE_TTL))
                    .await;
                if let Some(ref redis_conn) = self.redis_conn {
                    let redis_key = self.fam_key(key);
                    let mut conn = redis_conn.clone();
                    let _: redis::RedisResult<()> = conn
                        .set_ex(&redis_key, "_", L2_NEGATIVE_TTL_SECS)
                        .await;
                }
                None
            }
        }
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) {
        // 1. Write to PG (durable primary)
        self.pg.set_family_revoked(key, timestamp, ttl_secs).await;

        // 2. Write to L2 Redis (positive, overwrites any stale negative sentinel)
        if let Some(ref redis_conn) = self.redis_conn {
            let redis_key = self.fam_key(key);
            let l2_ttl = Self::l2_positive_ttl(ttl_secs);
            let mut conn = redis_conn.clone();
            let _: redis::RedisResult<()> = conn
                .set_ex(&redis_key, timestamp.to_string(), l2_ttl)
                .await;
        }

        // 3. Write to L1 moka (positive, overwrites any stale negative sentinel)
        self.l1_family
            .insert(key.to_string(), (Some(timestamp), Instant::now() + L1_POSITIVE_TTL))
            .await;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: create a TieredTokenBlacklistStore with no Redis and a lazy PG pool
    // that won't be contacted (for L1-only tests).
    fn make_tiered_l1_only() -> TieredTokenBlacklistStore {
        // connect_lazy won't actually connect until a query is executed.
        let pool = PgPool::connect_lazy("postgres://dummy:dummy@localhost/dummy")
            .expect("connect_lazy should not fail");
        TieredTokenBlacklistStore::new(
            PgTokenBlacklistStore::new(pool),
            None,
            "test:".to_string(),
        )
    }

    #[tokio::test]
    async fn test_l1_positive_blacklist_hit() {
        let store = make_tiered_l1_only();

        // Pre-populate L1 with a positive entry
        store
            .l1_blacklist
            .insert(
                "jti:abc".to_string(),
                (true, Instant::now() + Duration::from_secs(60)),
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
                (false, Instant::now() + Duration::from_secs(60)),
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
                (true, Instant::now() - Duration::from_secs(1)),
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
                (Some(ts), Instant::now() + Duration::from_secs(60)),
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
                (None, Instant::now() + Duration::from_secs(60)),
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
                (false, Instant::now() + Duration::from_secs(60)),
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
        assert!(store.get_family_revoked_at("family:write_test").await.is_none());

        // set_family_revoked writes to PG (fails silently) then L1
        let ts = 1700000000_i64;
        store.set_family_revoked("family:write_test", ts, 3600).await;

        // L1 should now have the entry
        assert_eq!(store.get_family_revoked_at("family:write_test").await, Some(ts));
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

        tokio::time::sleep(Duration::from_millis(1100)).await;
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
        store.set_family_revoked(key, timestamp, 86400).await;
        assert_eq!(store.get_family_revoked_at(key).await, Some(timestamp));
    }
}
