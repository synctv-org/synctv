use super::TokenBlacklistStore;
use async_trait::async_trait;
use redis::AsyncCommands;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{RedisConnectionRuntime, Result, SharedStateProfile};

fn ttl_secs_to_chrono_duration(ttl_secs: u64) -> Result<chrono::Duration> {
    let seconds = i64::try_from(ttl_secs).map_err(|_| {
        crate::Error::InvalidInput("token blacklist TTL exceeds i64::MAX seconds".to_string())
    })?;
    Ok(chrono::Duration::seconds(seconds))
}

// InMemoryTokenBlacklistStore

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
    pub(super) jti_blacklist: Arc<moka::future::Cache<String, Instant>>,
    /// `user_key` -> (`revoked_at` timestamp, expiry Instant)
    pub(super) family_revoked: Arc<moka::future::Cache<String, (i64, Instant)>>,
    /// Per-key mutex for atomic `blacklist_if_not_exists` operations
    /// Uses `DashMap` for O(1) lock acquisition without global contention
    pub(super) blacklist_locks: Arc<dashmap::DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
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

    pub(super) fn cleanup_blacklist_lock(&self, key: &str, mutex: &Arc<tokio::sync::Mutex<()>>) {
        if Arc::strong_count(mutex) != 2 {
            return;
        }
        let Ok(_cleanup_guard) = mutex.try_lock() else {
            return;
        };
        drop(
            self.blacklist_locks
                .remove_if(key, |_, stored_mutex| Arc::ptr_eq(stored_mutex, mutex)),
        );
    }
}

#[async_trait]
impl TokenBlacklistStore for InMemoryTokenBlacklistStore {
    async fn is_blacklisted_checked(&self, key: &str) -> Result<bool> {
        Ok(match self.jti_blacklist.get(key).await {
            Some(expiry) => Instant::now() < expiry,
            None => false,
        })
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
            if self.is_blacklisted_checked(key).await? {
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

    async fn get_family_revoked_at_checked(&self, key: &str) -> Result<Option<i64>> {
        Ok(match self.family_revoked.get(key).await {
            Some((timestamp, expiry)) if Instant::now() < expiry => Some(timestamp),
            _ => None,
        })
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) -> Result<()> {
        let expiry = Instant::now() + Duration::from_secs(ttl_secs);
        self.family_revoked
            .insert(key.to_string(), (timestamp, expiry))
            .await;
        Ok(())
    }
}

// PgTokenBlacklistStore

/// PostgreSQL-backed [`TokenBlacklistStore`].
///
/// Provides durable token blacklist storage that survives restarts, used as
/// the primary (durable) layer inside [`TieredTokenBlacklistStore`].
///
/// JTI blacklist entries are stored in the `token_blacklist` table with an
/// `expires_at` timestamp. Family revocation reuses the same primary key row
/// and stores the stable revocation timestamp in `family_revoked_at`.
/// Expired rows are cleaned up lazily (not returned by queries) and can be
/// purged periodically via `DELETE FROM auth_token_blacklist WHERE expires_at < NOW()`.
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
        let deleted_count = sqlx::query_scalar!(
            r#"
            WITH deleted AS (
                DELETE FROM auth_token_blacklist
                WHERE expires_at < CURRENT_TIMESTAMP
                RETURNING 1
            )
            SELECT COUNT(*)::BIGINT as "count!"
            "#,
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
        Ok(deleted_count.max(0).cast_unsigned())
    }
}

#[async_trait]
impl TokenBlacklistStore for PgTokenBlacklistStore {
    async fn is_blacklisted_checked(&self, key: &str) -> Result<bool> {
        sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM auth_token_blacklist WHERE jti = $1 AND expires_at > NOW()) as \"exists!\"",
            key,
        )
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
        let expires_at = crate::SystemClock.now() + ttl_secs_to_chrono_duration(ttl_secs)?;
        sqlx::query!(
            "INSERT INTO auth_token_blacklist (jti, expires_at) VALUES ($1, $2) \
             ON CONFLICT (jti) DO UPDATE SET expires_at = EXCLUDED.expires_at",
            key,
            expires_at,
        )
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
    /// Uses `PostgreSQL`'s `INSERT... ON CONFLICT DO NOTHING` with `xmax` check
    /// to atomically detect whether the insert was successful or the key existed.
    /// Returns:
    /// - `Ok(true)` if key already existed (replay detected)
    /// - `Ok(false)` if key was newly inserted (first use)
    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> Result<bool> {
        let expires_at = crate::SystemClock.now() + ttl_secs_to_chrono_duration(ttl_secs)?;

        // xmax = 0 means the row was inserted (no conflict)
        // xmax != 0 means the row already existed (conflict, nothing inserted)
        // See: https://www.postgresql.org/docs/current/functions-info.html
        let insert_outcome = sqlx::query_scalar!(
            "INSERT INTO auth_token_blacklist (jti, expires_at) VALUES ($1, $2) \
             ON CONFLICT (jti) DO NOTHING \
             RETURNING (xmax = 0) as \"inserted!\"",
            key,
            expires_at,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!(
                key = %key,
                error = %e,
                "Failed to blacklist refresh token JTI in PostgreSQL"
            );
            crate::Error::Internal("Failed to rotate refresh token".to_string())
        })?;

        let already_blacklisted = match insert_outcome {
            Some(inserted) => !inserted,
            None => true,
        };

        Ok(already_blacklisted)
    }

    async fn get_family_revoked_at_checked(&self, key: &str) -> Result<Option<i64>> {
        sqlx::query_scalar!(
            r#"SELECT family_revoked_at as "family_revoked_at!"
             FROM auth_token_blacklist
             WHERE jti = $1
               AND expires_at > NOW()
               AND family_revoked_at IS NOT NULL"#,
            key,
        )
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
        let expires_at = crate::SystemClock.now() + ttl_secs_to_chrono_duration(ttl_secs)?;
        sqlx::query!(
            "INSERT INTO auth_token_blacklist (jti, expires_at, family_revoked_at) VALUES ($1, $2, $3) \
             ON CONFLICT (jti) DO UPDATE
             SET expires_at = EXCLUDED.expires_at,
                 family_revoked_at = EXCLUDED.family_revoked_at",
            key,
            expires_at,
            timestamp,
        )
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

// TieredTokenBlacklistStore

/// L1 positive cache TTL (how long a "is blacklisted = true" entry lives in moka).
const L1_POSITIVE_TTL: Duration = Duration::from_mins(2);
/// L1 negative cache TTL (how long a "is blacklisted = false" sentinel lives in moka).
const L1_NEGATIVE_TTL: Duration = Duration::from_secs(10);
/// L2 (Redis) negative sentinel TTL in seconds.
const L2_NEGATIVE_TTL_SECS: u64 = 15;
/// Safety margin subtracted from token TTL for L2 positive entries.
/// Ensures Redis entries expire before the token itself, preventing stale positives.
const L2_TTL_MARGIN_SECS: u64 = 30;

pub(super) fn parse_l2_blacklist_value(key: &str, value: &str) -> Option<bool> {
    match value {
        "1" => Some(true),
        "0" => Some(false),
        _ => {
            tracing::warn!(
                key = %key,
                value = %value,
                "Malformed blacklist value in Redis L2"
            );
            None
        }
    }
}

/// Tiered [`TokenBlacklistStore`] with L1 (moka) + optional L2 (Redis) + PG primary.
///
/// ## Architecture
///
/// ```text
/// is_blacklisted_checked("jti_xyz"):
///
/// L1 (moka) ──hit──> return cached result (positive or negative)
/// │ miss
/// L2 (Redis) ──hit──> populate L1, return result
/// │ miss
/// PG (primary)──found──> populate L1+L2 (positive), return true
/// │ not found
/// Cache negative sentinel in L1+L2 (short TTL), return false
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
/// `INSERT... ON CONFLICT DO NOTHING`. Redis is used only as an L2 cache for
/// read acceleration and must never become the only durable record of a
/// blacklisted token.
///
/// When Redis is not available, the store still provides correct behavior via
/// L1 (moka) + PG.
pub struct TieredTokenBlacklistStore {
    durable: Arc<dyn TokenBlacklistStore>,
    /// Optional Redis runtime for L2 caching.
    pub(super) redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    /// L1 cache: JTI key -> (`is_blacklisted`, expiry)
    pub(super) l1_blacklist: moka::future::Cache<String, (bool, Instant)>,
    /// L1 cache: family key -> (Option<`revoked_at_timestamp`>, expiry)
    pub(super) l1_family: moka::future::Cache<String, (Option<i64>, Instant)>,
    /// Redis key prefix (e.g., "synctv:")
    key_prefix: String,
    /// Per-key mutex for atomic `blacklist_if_not_exists` operations (used when Redis unavailable)
    /// Uses `DashMap` for O(1) lock acquisition without global contention
    pub(super) blacklist_locks: Arc<dashmap::DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl TieredTokenBlacklistStore {
    #[must_use]
    pub fn from_runtime(
        durable: impl TokenBlacklistStore + 'static,
        redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        key_prefix: String,
    ) -> Self {
        Self {
            durable: Arc::new(durable),
            redis_runtime,
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

    #[must_use]
    pub fn from_shared_state_profile(
        durable: impl TokenBlacklistStore + 'static,
        profile: &SharedStateProfile,
    ) -> Self {
        Self::from_runtime(
            durable,
            profile.shared_runtime(),
            profile.key_prefix().to_string(),
        )
    }

    /// Redis key for a blacklist entry.
    pub(super) fn bl_key(&self, jti: &str) -> String {
        format!("{}bl:{}", self.key_prefix, jti)
    }

    /// Redis key for a family revocation entry.
    pub(super) fn fam_key(&self, key: &str) -> String {
        format!("{}fam:{}", self.key_prefix, key)
    }

    /// Compute L2 TTL for a positive entry: `token_ttl - margin`, minimum 1s.
    pub(super) fn l2_positive_ttl(ttl_secs: u64) -> u64 {
        ttl_secs.saturating_sub(L2_TTL_MARGIN_SECS).max(1)
    }

    pub(super) fn cleanup_blacklist_lock(&self, key: &str, mutex: &Arc<tokio::sync::Mutex<()>>) {
        if Arc::strong_count(mutex) != 2 {
            return;
        }
        let Ok(_cleanup_guard) = mutex.try_lock() else {
            return;
        };
        drop(
            self.blacklist_locks
                .remove_if(key, |_, stored_mutex| Arc::ptr_eq(stored_mutex, mutex)),
        );
    }

    async fn redis_conn_snapshot(&self) -> Option<redis::aio::ConnectionManager> {
        self.redis_conn_snapshot_result().await
    }

    async fn redis_conn_snapshot_result(&self) -> Option<redis::aio::ConnectionManager> {
        let runtime = self.redis_runtime.as_ref()?;
        match tokio::time::timeout(runtime.operation_timeout(), runtime.snapshot()).await {
            Ok(Ok(conn)) => Some(conn),
            Ok(Err(error)) => {
                tracing::warn!(
                    error = %error,
                    "Redis L2 token blacklist connection snapshot failed"
                );
                None
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = runtime.operation_timeout().as_millis(),
                    "Redis L2 token blacklist connection snapshot timed out"
                );
                None
            }
        }
    }

    async fn run_redis_op<T, F>(&self, operation: &'static str, future: F) -> Option<T>
    where
        F: std::future::Future<Output = redis::RedisResult<T>>,
    {
        let timeout = self.redis_runtime.as_ref().map_or(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            |runtime| runtime.operation_timeout(),
        );

        match tokio::time::timeout(timeout, future).await {
            Ok(Ok(value)) => Some(value),
            Ok(Err(error)) => {
                tracing::warn!(
                    operation,
                    error = %error,
                    "Redis L2 token blacklist operation failed"
                );
                None
            }
            Err(_) => {
                tracing::warn!(
                    operation,
                    timeout_ms = timeout.as_millis(),
                    "Redis L2 token blacklist operation timed out"
                );
                None
            }
        }
    }

    async fn redis_get_string(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        key: &str,
        operation: &'static str,
    ) -> Option<Option<String>> {
        self.run_redis_op(operation, conn.get(key)).await
    }

    async fn redis_set_ex(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        key: &str,
        value: impl redis::ToSingleRedisArg + Send + Sync,
        ttl_secs: u64,
        operation: &'static str,
    ) -> bool {
        self.run_redis_op(operation, conn.set_ex::<_, _, ()>(key, value, ttl_secs))
            .await
            .is_some()
    }
}

#[async_trait]
impl TokenBlacklistStore for TieredTokenBlacklistStore {
    async fn is_blacklisted_checked(&self, key: &str) -> Result<bool> {
        if let Some((is_bl, expiry)) = self.l1_blacklist.get(key).await {
            if Instant::now() < expiry {
                return Ok(is_bl);
            }
            // Expired entry; fall through to L2/PG
        }

        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let redis_key = self.bl_key(key);
            if let Some(Some(val)) = self
                .redis_get_string(&mut conn, &redis_key, "checked blacklist lookup")
                .await
            {
                if let Some(is_bl) = parse_l2_blacklist_value(key, &val) {
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
            }
        }

        let found = self.durable.is_blacklisted_checked(key).await?;

        if found {
            // Positive: populate L1 + L2
            self.l1_blacklist
                .insert(key.to_string(), (true, Instant::now() + L1_POSITIVE_TTL))
                .await;
            if let Some(mut conn) = self.redis_conn_snapshot().await {
                let redis_key = self.bl_key(key);
                self.redis_set_ex(
                    &mut conn,
                    &redis_key,
                    "1",
                    L1_POSITIVE_TTL.as_secs(),
                    "checked blacklist positive cache write",
                )
                .await;
            }
        } else {
            // Negative sentinel: populate L1 + L2
            self.l1_blacklist
                .insert(key.to_string(), (false, Instant::now() + L1_NEGATIVE_TTL))
                .await;
            if let Some(mut conn) = self.redis_conn_snapshot().await {
                let redis_key = self.bl_key(key);
                self.redis_set_ex(
                    &mut conn,
                    &redis_key,
                    "0",
                    L2_NEGATIVE_TTL_SECS,
                    "checked blacklist negative cache write",
                )
                .await;
            }
        }

        Ok(found)
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()> {
        // 1. Write to PG (durable primary)
        self.durable.blacklist(key, ttl_secs).await?;

        // 2. Write to L2 Redis (positive, overwrites any stale negative sentinel)
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let redis_key = self.bl_key(key);
            let l2_ttl = Self::l2_positive_ttl(ttl_secs);
            self.redis_set_ex(
                &mut conn,
                &redis_key,
                "1",
                l2_ttl,
                "blacklist write-through",
            )
            .await;
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
                    self.durable.blacklist_if_not_exists(key, ttl_secs).await?
                }
            } else {
                // Delegate to PG's atomic operation first. Returning `Ok` before this
                // succeeds would violate the durable-primary architecture.
                self.durable.blacklist_if_not_exists(key, ttl_secs).await?
            }
        };

        // Best-effort L2 population after the durable PG write succeeds.
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let redis_key = self.bl_key(key);
            let l2_ttl = Self::l2_positive_ttl(ttl_secs);
            if !self
                .redis_set_ex(
                    &mut conn,
                    &redis_key,
                    "1",
                    l2_ttl,
                    "blacklist-if-not-exists cache write",
                )
                .await
            {
                tracing::warn!(
                    key = %key,
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

    async fn get_family_revoked_at_checked(&self, key: &str) -> Result<Option<i64>> {
        if let Some((cached_val, expiry)) = self.l1_family.get(key).await {
            if Instant::now() < expiry {
                return Ok(cached_val);
            }
        }

        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let redis_key = self.fam_key(key);
            if let Some(Some(val)) = self
                .redis_get_string(&mut conn, &redis_key, "checked family lookup")
                .await
            {
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
        }

        let result = self.durable.get_family_revoked_at_checked(key).await?;

        if let Some(ts) = result {
            self.l1_family
                .insert(
                    key.to_string(),
                    (Some(ts), Instant::now() + L1_POSITIVE_TTL),
                )
                .await;
            if let Some(mut conn) = self.redis_conn_snapshot().await {
                let redis_key = self.fam_key(key);
                self.redis_set_ex(
                    &mut conn,
                    &redis_key,
                    ts.to_string(),
                    L1_POSITIVE_TTL.as_secs(),
                    "checked family positive cache write",
                )
                .await;
            }
            Ok(Some(ts))
        } else {
            self.l1_family
                .insert(key.to_string(), (None, Instant::now() + L1_NEGATIVE_TTL))
                .await;
            if let Some(mut conn) = self.redis_conn_snapshot().await {
                let redis_key = self.fam_key(key);
                self.redis_set_ex(
                    &mut conn,
                    &redis_key,
                    "_",
                    L2_NEGATIVE_TTL_SECS,
                    "checked family negative cache write",
                )
                .await;
            }
            Ok(None)
        }
    }

    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) -> Result<()> {
        // 1. Write to PG (durable primary)
        self.durable
            .set_family_revoked(key, timestamp, ttl_secs)
            .await?;

        // 2. Write to L2 Redis (positive, overwrites any stale negative sentinel)
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let redis_key = self.fam_key(key);
            let l2_ttl = Self::l2_positive_ttl(ttl_secs);
            self.redis_set_ex(
                &mut conn,
                &redis_key,
                timestamp.to_string(),
                l2_ttl,
                "family write-through",
            )
            .await;
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
