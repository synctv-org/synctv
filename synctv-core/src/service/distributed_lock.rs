//! Distributed lock service using Redis
//!
//! Design reference: /Volumes/workspace/rust/synctv-rs-design/21-关键实现.md §12.2.3
//!
//! Provides distributed locking mechanism for multi-replica deployments.
//! Uses Redis SET NX EX for atomic lock acquisition.
//!
//! # Safety Warning: Single-Instance Only
//!
//! **This implementation operates on a single Redis instance and is NOT
//! Redlock-compliant.** It relies on a single Redis node for lock state, which
//! means:
//!
//! - **Standalone mode**: Safe. The single Redis instance is the source of truth.
//! - **Sentinel mode**: **Unsafe during failover.** When the Sentinel promotes a
//!   replica to master, any locks held on the old master are lost because Redis
//!   replication is asynchronous. Two clients may simultaneously believe they hold
//!   the same lock (split-brain). The fencing token mechanism mitigates this for
//!   database writes, but not for all use cases.
//! - **Cluster mode**: Not supported (rejected at config validation).
//!
//! For true distributed lock safety across failovers, consider implementing the
//! [Redlock algorithm](https://redis.io/docs/manual/patterns/distributed-locks/)
//! with multiple independent Redis masters.
//!
//! **Production recommendation**: If you are deploying with Redis Sentinel,
//! strongly consider using the Redlock algorithm with multiple independent Redis
//! masters (minimum 3). Single-instance locking behind Sentinel provides
//! *availability* (automatic failover) but NOT *correctness* (locks can be lost
//! during asynchronous replication). Fencing tokens mitigate this for database
//! writes, but non-idempotent side effects (e.g., sending notifications, billing)
//! cannot be fenced.
//!
//! # Fencing Token Support
//!
//! This implementation provides fencing tokens to handle the "split-brain" scenario
//! where a lock holder's operation outlasts the lock TTL (due to GC pause, network
//! partition, or slow processing). Each lock acquisition returns a monotonically
//! increasing token that can be used for CAS (Compare-And-Swap) operations.
//!
//! ## Usage Pattern
//!
//! ```text
//! let (lock_value, fencing_token) = lock.acquire_with_token("resource", 10).await?;
//! if let Some((value, token)) = lock_value {
//!     // Pass fencing_token to database write as CAS condition
//!     db.update_with_version(resource_id, data, token).await?;
//!     lock.release("resource", &value).await?;
//! }
//! ```
//!
//! ## Token Generation Strategy
//!
//! Tokens are generated using Redis INCR on a per-key counter, ensuring:
//! - Monotonic increase across all clients
//! - Uniqueness even during network partitions
//! - Simplicity without requiring clock synchronization

use crate::{Error, InternalExt, RedisConnectionRuntime, Result};
use redis::aio::ConnectionManager as RedisConnectionManager;
use redis::Script;
use std::future::Future;
use std::sync::Arc;

async fn run_distributed_lock_redis_op<T, F>(operation: impl Into<String>, future: F) -> Result<T>
where
    F: Future<Output = std::result::Result<T, redis::RedisError>>,
{
    let operation = operation.into();
    tokio::time::timeout(crate::resilience::timeout::REDIS_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| Error::Timeout(format!("Redis timeout: {operation}")))?
        .internal_with_err(&format!("Failed to {operation}"))
}

async fn run_distributed_lock_client_op<T, F>(
    key: &str,
    timeout: std::time::Duration,
    future: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| Error::Timeout(format!("Lock operation timed out for key: {key}")))?
}

/// Abstraction over a distributed migration lock.
///
/// Consumers that only need acquire/release semantics (e.g. `run_migrations`)
/// program against this trait instead of depending on Redis directly.
#[async_trait::async_trait]
pub trait MigrationLock: Send + Sync {
    /// Try to acquire the lock.
    ///
    /// Returns `Ok(Some(lock_value))` if acquired, `Ok(None)` if already held,
    /// or `Err` on infrastructure failure.
    async fn acquire(&self, key: &str, ttl_secs: u64) -> anyhow::Result<Option<String>>;

    /// Extend a previously acquired lock.
    ///
    /// Implementations without TTL semantics may treat this as a successful no-op
    /// while ownership is still held.
    async fn extend(&self, _key: &str, _lock_value: &str, _ttl_secs: u64) -> anyhow::Result<bool> {
        Ok(true)
    }

    /// Release a previously acquired lock.
    ///
    /// Returns `true` if the lock was released, `false` if not held or expired.
    async fn release(&self, key: &str, lock_value: &str) -> anyhow::Result<bool>;
}

/// Object-safe coordination lock for application-layer critical sections.
///
/// Unlike [`MigrationLock`], this trait uses the crate's native [`Result`]
/// type so business services can depend on it directly without knowing the
/// concrete coordination backend.
#[async_trait::async_trait]
pub trait CoordinationLock: Send + Sync {
    /// Try to acquire the lock.
    ///
    /// Returns `Ok(Some(lock_value))` if acquired, `Ok(None)` if already held,
    /// or `Err` on infrastructure failure.
    async fn acquire(&self, key: &str, ttl_secs: u64) -> Result<Option<String>>;

    /// Release a previously acquired lock.
    async fn release(&self, key: &str, lock_value: &str) -> Result<bool>;
}

/// Execute an operation under an acquired [`CoordinationLock`].
///
/// This preserves the same client-side timeout and best-effort unlock behavior
/// previously provided by [`DistributedLock::with_lock`], while allowing the
/// caller to depend on a trait object instead of a concrete Redis lock.
pub async fn with_coordination_lock<L, F, Fut, T>(
    lock: &L,
    key: &str,
    ttl_seconds: u64,
    operation: F,
) -> Result<T>
where
    L: CoordinationLock + ?Sized,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let client_timeout = std::time::Duration::from_secs(ttl_seconds + 5);

    run_distributed_lock_client_op(key, client_timeout, async {
        let lock_value = lock
            .acquire(key, ttl_seconds)
            .await?
            .ok_or_else(|| Error::LockConflict(format!("Lock already held: {key}")))?;

        let result = operation().await;

        if let Err(error) = lock.release(key, &lock_value).await {
            tracing::error!(
                key = %key,
                error = %error,
                "Failed to release lock after operation"
            );
        }

        result
    })
    .await
}

/// `MigrationLock` implementation backed by the existing Redis `DistributedLock`.
#[async_trait::async_trait]
impl MigrationLock for DistributedLock {
    async fn acquire(&self, key: &str, ttl_secs: u64) -> anyhow::Result<Option<String>> {
        Self::acquire(self, key, ttl_secs)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    async fn release(&self, key: &str, lock_value: &str) -> anyhow::Result<bool> {
        Self::release(self, key, lock_value)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    async fn extend(&self, key: &str, lock_value: &str, ttl_secs: u64) -> anyhow::Result<bool> {
        Self::extend(self, key, lock_value, ttl_secs)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
}

#[async_trait::async_trait]
impl CoordinationLock for DistributedLock {
    async fn acquire(&self, key: &str, ttl_secs: u64) -> Result<Option<String>> {
        Self::acquire(self, key, ttl_secs).await
    }

    async fn release(&self, key: &str, lock_value: &str) -> Result<bool> {
        Self::release(self, key, lock_value).await
    }
}

/// `PostgreSQL` advisory lock-based `MigrationLock`.
///
/// Used as a fallback when the Redis lock fails. This implementation performs
/// a single non-blocking `pg_try_advisory_lock` attempt and reports
/// contention via `Ok(None)`, matching the `MigrationLock` contract. Waiting
/// and retry orchestration stays in the migration runner so all lock
/// implementations share the same state machine.
///
/// The advisory lock is session-scoped, so we must release it on the same
/// connection that acquired it. The `lock_conn` field holds that connection.
pub struct PgAdvisoryMigrationLock {
    pool: sqlx::PgPool,
    lock_conn: tokio::sync::Mutex<Option<sqlx::pool::PoolConnection<sqlx::Postgres>>>,
}

/// Stable advisory lock key for migration coordination (hash of "`synctv_migration`").
const PG_ADVISORY_LOCK_KEY: i64 = 0x7379_6E63_7476_6D69_i64;

impl PgAdvisoryMigrationLock {
    #[must_use]
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            lock_conn: tokio::sync::Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl MigrationLock for PgAdvisoryMigrationLock {
    async fn acquire(&self, _key: &str, _ttl_secs: u64) -> anyhow::Result<Option<String>> {
        let mut conn = self.pool.acquire().await.map_err(|e| {
            anyhow::anyhow!("Failed to acquire DB connection for PG advisory lock: {e}")
        })?;

        let acquired: (bool,) = sqlx::query_as("SELECT pg_try_advisory_lock($1)")
            .bind(PG_ADVISORY_LOCK_KEY)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to attempt PostgreSQL advisory lock: {e}"))?;

        if acquired.0 {
            // Store the connection so release() can use the same session.
            *self.lock_conn.lock().await = Some(conn);
            Ok(Some("pg_advisory".to_string()))
        } else {
            Ok(None)
        }
    }

    async fn release(&self, _key: &str, _lock_value: &str) -> anyhow::Result<bool> {
        // Release the advisory lock on the same connection that acquired it.
        // Session-scoped advisory locks cannot be released from a different connection.
        let mut guard = self.lock_conn.lock().await;
        if let Some(ref mut conn) = *guard {
            let result: (bool,) = sqlx::query_as("SELECT pg_advisory_unlock($1)")
                .bind(PG_ADVISORY_LOCK_KEY)
                .fetch_one(&mut **conn)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to release PG advisory lock: {e}"))?;
            // Return the connection to the pool
            *guard = None;
            Ok(result.0)
        } else {
            // No connection held — lock was never acquired or already released
            Ok(false)
        }
    }
}

/// Distributed lock service (single Redis instance)
///
/// Provides Redis-based distributed locking for cross-replica critical sections
/// with fencing token support for protection against split-brain scenarios.
///
/// **Warning:** This lock is NOT safe during Redis Sentinel failovers. Locks held
/// on the old master are lost when a replica is promoted. Use fencing tokens for
/// any database writes protected by this lock. See module-level documentation for
/// details.
#[derive(Clone)]
pub struct DistributedLock {
    redis_runtime: Arc<dyn RedisConnectionRuntime>,
}

impl DistributedLock {
    #[must_use]
    pub fn from_runtime(redis_runtime: Arc<dyn RedisConnectionRuntime>) -> Self {
        Self { redis_runtime }
    }

    fn log_sentinel_warning() {
        tracing::warn!(
            "Distributed lock is running behind Redis Sentinel. \
             During a Sentinel failover, there is a brief split-brain window where \
             locks held on the old master may be lost because Redis replication is \
             asynchronous. Fencing tokens mitigate this for database writes, but \
             non-idempotent side effects (notifications, billing) cannot be fenced. \
             For production Sentinel deployments, consider using the Redlock algorithm \
             with multiple independent Redis masters."
        );
    }

    #[must_use]
    pub fn from_runtime_with_mode(
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        is_sentinel: bool,
    ) -> Self {
        if is_sentinel {
            Self::log_sentinel_warning();
        }
        Self::from_runtime(redis_runtime)
    }

    /// Create a new distributed lock service.
    ///
    /// If `redis_url` is provided, checks whether it uses Sentinel (contains
    /// "sentinel" in the URL) and emits a startup warning about lock safety.
    #[must_use]
    pub fn new(redis: RedisConnectionManager) -> Self {
        Self::from_runtime(crate::direct_runtime(redis))
    }

    /// Create a distributed lock service from the shared Redis handle used by
    /// the rest of the application.
    ///
    /// This keeps the lock service aligned with Sentinel failover hot-swaps so
    /// it does not keep talking to a stale master after reconnection.
    #[must_use]
    pub fn new_shared(redis: Arc<tokio::sync::RwLock<RedisConnectionManager>>) -> Self {
        Self::from_runtime(crate::shared_runtime(redis))
    }

    /// Create a new distributed lock service and log a warning if the Redis URL
    /// indicates Sentinel mode. Call this at startup instead of `new()` when the
    /// Redis URL is available.
    pub fn new_with_sentinel_check(redis: RedisConnectionManager, redis_url: &str) -> Self {
        if redis_url.contains("sentinel") || redis_url.contains("SENTINEL") {
            Self::log_sentinel_warning();
        }
        Self::new(redis)
    }

    /// Create a new distributed lock service with deployment mode awareness.
    ///
    /// When `is_sentinel` is true, emits a startup warning about the split-brain
    /// window during Sentinel failover. This is more reliable than URL-based
    /// detection in `new_with_sentinel_check`.
    pub fn new_with_mode(redis: RedisConnectionManager, is_sentinel: bool) -> Self {
        Self::from_runtime_with_mode(crate::direct_runtime(redis), is_sentinel)
    }

    /// Create a distributed lock service from the shared Redis handle with
    /// deployment-mode awareness.
    pub fn new_shared_with_mode(redis: Arc<tokio::sync::RwLock<RedisConnectionManager>>, is_sentinel: bool) -> Self {
        Self::from_runtime_with_mode(crate::shared_runtime(redis), is_sentinel)
    }

    async fn conn(&self) -> RedisConnectionManager {
        self.redis_runtime.snapshot().await
    }

    /// Generate a fencing token for a lock key using Redis INCR
    ///
    /// Uses Redis INCR on a per-key counter to ensure monotonic tokens
    /// across all clients. Returns an error if Redis INCR fails, since
    /// the lock itself requires Redis and a local fallback would break
    /// monotonicity across replicas.
    async fn generate_fencing_token(&self, key: &str) -> crate::Result<u64> {
        let token_key = format!("lock:token:{key}");
        let mut conn = self.conn().await;

        // Atomically INCR and set a 24-hour TTL to prevent unbounded key accumulation
        let script = Script::new(
            r"
            local val = redis.call('INCR', KEYS[1])
            redis.call('EXPIRE', KEYS[1], 86400)
            return val
            ",
        );

        run_distributed_lock_redis_op(
            format!("generate fencing token for lock '{key}'"),
            script.key(&token_key).invoke_async::<u64>(&mut conn),
        )
        .await
    }

    /// Acquire a lock (using SET NX EX atomic operation)
    ///
    /// Returns the lock value if acquired successfully, None if lock is already held.
    /// For fencing token support, use [`acquire_with_token`](Self::acquire_with_token).
    ///
    /// # Arguments
    /// * `key` - Lock key (without "lock:" prefix)
    /// * `ttl_seconds` - Lock expiration time in seconds
    ///
    /// # Example
    /// ```text
    /// let lock_value = lock.acquire("create_room:user123", 10).await?;
    /// if let Some(value) = lock_value {
    ///     // Lock acquired, perform operation
    ///     // ...
    ///     lock.release("create_room:user123", &value).await?;
    /// } else {
    ///     // Lock already held by another process
    /// }
    /// ```
    pub async fn acquire(&self, key: &str, ttl_seconds: u64) -> Result<Option<String>> {
        let result = self.acquire_internal(key, ttl_seconds, false).await?;
        Ok(result.map(|(value, _token)| value))
    }

    /// Acquire a lock with fencing token
    ///
    /// Returns the lock value and fencing token if acquired successfully.
    /// The fencing token is monotonically increasing and can be used for
    /// CAS (Compare-And-Swap) operations to protect against split-brain scenarios.
    ///
    /// # Arguments
    /// * `key` - Lock key (without "lock:" prefix)
    /// * `ttl_seconds` - Lock expiration time in seconds
    ///
    /// # Returns
    /// * `Some((lock_value, fencing_token))` if lock was acquired
    /// * `None` if lock is already held by another process
    ///
    /// # Example
    /// ```text
    /// match lock.acquire_with_token("create_room:user123", 10).await? {
    ///     Some((lock_value, fencing_token)) => {
    ///         // Pass fencing_token to protected operation for CAS validation
    ///         room_service.create_room_with_token(request, fencing_token).await?;
    ///         lock.release("create_room:user123", &lock_value).await?;
    ///     }
    ///     None => {
    ///         // Lock already held by another process
    ///     }
    /// }
    /// ```
    pub async fn acquire_with_token(
        &self,
        key: &str,
        ttl_seconds: u64,
    ) -> Result<Option<(String, u64)>> {
        self.acquire_internal(key, ttl_seconds, true).await
    }

    /// Internal acquire implementation
    async fn acquire_internal(
        &self,
        key: &str,
        ttl_seconds: u64,
        with_token: bool,
    ) -> Result<Option<(String, u64)>> {
        let lock_key = format!("lock:{key}");
        let lock_value = crate::models::generate_id(); // base62 ID (12 chars)

        let mut conn = self.conn().await;

        // SET key value NX EX ttl
        // NX: Only set if not exists
        // EX: Set expiration time
        let result: Option<String> = run_distributed_lock_redis_op(
            "acquire lock",
            redis::cmd("SET")
                .arg(&lock_key)
                .arg(&lock_value)
                .arg("NX")
                .arg("EX")
                .arg(ttl_seconds)
                .query_async(&mut conn),
        )
        .await?;

        if result.is_some() {
            // Generate fencing token only if requested (saves Redis round-trip)
            let fencing_token = if with_token {
                self.generate_fencing_token(key).await?
            } else {
                0 // Dummy token when not requested
            };

            tracing::debug!(
                lock_key = %lock_key,
                lock_value = %lock_value,
                fencing_token = %fencing_token,
                ttl_seconds = %ttl_seconds,
                "Lock acquired"
            );
            Ok(Some((lock_value, fencing_token)))
        } else {
            tracing::debug!(
                lock_key = %lock_key,
                "Lock already held by another process"
            );
            Ok(None)
        }
    }

    /// Release a lock (using Lua script for atomicity)
    ///
    /// Only the lock holder (matching `lock_value`) can release the lock
    ///
    /// # Arguments
    /// * `key` - Lock key (without "lock:" prefix)
    /// * `lock_value` - The value returned by `acquire()`
    ///
    /// # Returns
    /// * `true` if lock was released successfully
    /// * `false` if lock was not held or already expired
    pub async fn release(&self, key: &str, lock_value: &str) -> Result<bool> {
        let lock_key = format!("lock:{key}");

        // Lua script: Only delete if the value matches
        // This prevents releasing a lock that was already expired and reacquired
        let script = Script::new(
            r#"
            if redis.call("GET", KEYS[1]) == ARGV[1] then
                return redis.call("DEL", KEYS[1])
            else
                return 0
            end
            "#,
        );

        let mut conn = self.conn().await;

        let result: i32 = run_distributed_lock_redis_op(
            "release lock",
            script
                .key(&lock_key)
                .arg(lock_value)
                .invoke_async::<i32>(&mut conn),
        )
        .await?;

        let released = result == 1;
        if released {
            tracing::debug!(
                lock_key = %lock_key,
                "Lock released"
            );
        } else {
            tracing::warn!(
                lock_key = %lock_key,
                "Lock release failed: value mismatch or already expired"
            );
        }

        Ok(released)
    }

    /// Execute an operation with automatic lock acquisition and release
    ///
    /// Uses RAII pattern to ensure lock is always released
    ///
    /// # Arguments
    /// * `key` - Lock key (without "lock:" prefix)
    /// * `ttl_seconds` - Lock expiration time in seconds
    /// * `operation` - Async function to execute while holding the lock
    ///
    /// # Returns
    /// * `Ok(T)` if lock was acquired and operation succeeded
    /// * `Err(Error::LockAcquisitionFailed)` if lock is already held
    /// * `Err(...)` if operation failed
    ///
    /// # Example
    /// ```text
    /// let result = lock.with_lock("create_room:user123", 10, || async {
    ///     // This code runs with lock held
    ///     room_service.create_room(request).await
    /// }).await?;
    /// ```
    pub async fn with_lock<F, Fut, T>(&self, key: &str, ttl_seconds: u64, operation: F) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        // Client-side timeout slightly longer than lock TTL to prevent infinite
        // waits during Redis partitions. The lock will expire server-side after
        // ttl_seconds, so we allow ttl + 5s for network round-trips.
        let client_timeout = std::time::Duration::from_secs(ttl_seconds + 5);

        run_distributed_lock_client_op(key, client_timeout, async {
            // Try to acquire lock
            let lock_value = self
                .acquire(key, ttl_seconds)
                .await?
                .ok_or_else(|| Error::LockConflict(format!("Lock already held: {key}")))?;

            // Execute operation
            let result = operation().await;

            // Always release lock, even if operation failed
            if let Err(e) = self.release(key, &lock_value).await {
                tracing::error!(
                    key = %key,
                    error = %e,
                    "Failed to release lock after operation"
                );
            }

            result
        })
        .await
    }

    /// Try to acquire a lock and execute an operation
    ///
    /// Returns None if lock is already held, Some(T) if operation succeeded
    ///
    /// # Example
    /// ```text
    /// match lock.try_with_lock("update_settings:room123", 10, || async {
    ///     room_service.update_settings(settings).await
    /// }).await? {
    ///     Some(result) => println!("Updated: {:?}", result),
    ///     None => println!("Lock already held, skipping update"),
    /// }
    /// ```
    pub async fn try_with_lock<F, Fut, T>(
        &self,
        key: &str,
        ttl_seconds: u64,
        operation: F,
    ) -> Result<Option<T>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let client_timeout = std::time::Duration::from_secs(ttl_seconds + 5);

        run_distributed_lock_client_op(key, client_timeout, async {
            // Try to acquire lock
            let Some(lock_value) = self.acquire(key, ttl_seconds).await? else {
                return Ok(None);
            };

            // Execute operation
            let result = operation().await;

            // Always release lock
            if let Err(e) = self.release(key, &lock_value).await {
                tracing::error!(
                    key = %key,
                    error = %e,
                    "Failed to release lock after operation"
                );
            }

            result.map(Some)
        })
        .await
    }

    /// Execute an operation with automatic lock acquisition and release (with fencing token)
    ///
    /// Same as `with_lock` but passes the fencing token to the operation.
    /// The fencing token can be used for CAS operations in the database layer.
    ///
    /// # Arguments
    /// * `key` - Lock key (without "lock:" prefix)
    /// * `ttl_seconds` - Lock expiration time in seconds
    /// * `operation` - Async function that receives the fencing token
    ///
    /// # Example
    /// ```text
    /// let result = lock.with_lock_token("create_room:user123", 10, |token| async move {
    ///     // Pass token to database write for CAS validation
    ///     room_service.create_room_with_token(request, token).await
    /// }).await?;
    /// ```
    pub async fn with_lock_token<F, Fut, T>(
        &self,
        key: &str,
        ttl_seconds: u64,
        operation: F,
    ) -> Result<T>
    where
        F: FnOnce(u64) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let client_timeout = std::time::Duration::from_secs(ttl_seconds + 5);

        run_distributed_lock_client_op(key, client_timeout, async {
            // Try to acquire lock with token
            let (lock_value, fencing_token) = self
                .acquire_with_token(key, ttl_seconds)
                .await?
                .ok_or_else(|| Error::LockConflict(format!("Lock already held: {key}")))?;

            // Execute operation with fencing token
            let result = operation(fencing_token).await;

            // Always release lock, even if operation failed
            if let Err(e) = self.release(key, &lock_value).await {
                tracing::error!(
                    key = %key,
                    error = %e,
                    "Failed to release lock after operation"
                );
            }

            result
        })
        .await
    }

    /// Try to acquire a lock and execute an operation (with fencing token)
    ///
    /// Same as `try_with_lock` but passes the fencing token to the operation.
    ///
    /// # Example
    /// ```text
    /// match lock.try_with_lock_token("update_settings:room123", 10, |token| async move {
    ///     room_service.update_settings_with_token(settings, token).await
    /// }).await? {
    ///     Some(result) => println!("Updated: {:?}", result),
    ///     None => println!("Lock already held, skipping update"),
    /// }
    /// ```
    pub async fn try_with_lock_token<F, Fut, T>(
        &self,
        key: &str,
        ttl_seconds: u64,
        operation: F,
    ) -> Result<Option<T>>
    where
        F: FnOnce(u64) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let client_timeout = std::time::Duration::from_secs(ttl_seconds + 5);

        run_distributed_lock_client_op(key, client_timeout, async {
            // Try to acquire lock with token
            let Some((lock_value, fencing_token)) =
                self.acquire_with_token(key, ttl_seconds).await?
            else {
                return Ok(None);
            };

            // Execute operation with fencing token
            let result = operation(fencing_token).await;

            // Always release lock
            if let Err(e) = self.release(key, &lock_value).await {
                tracing::error!(
                    key = %key,
                    error = %e,
                    "Failed to release lock after operation"
                );
            }

            result.map(Some)
        })
        .await
    }

    /// Extend lock TTL (refresh expiration)
    ///
    /// Useful for long-running operations that need to keep the lock
    ///
    /// # Returns
    /// * `true` if lock TTL was extended
    /// * `false` if lock doesn't exist or value mismatch
    pub async fn extend(&self, key: &str, lock_value: &str, ttl_seconds: u64) -> Result<bool> {
        let lock_key = format!("lock:{key}");

        // Lua script: Only extend if the value matches
        let script = Script::new(
            r#"
            if redis.call("GET", KEYS[1]) == ARGV[1] then
                return redis.call("EXPIRE", KEYS[1], ARGV[2])
            else
                return 0
            end
            "#,
        );

        let mut conn = self.conn().await;

        let result: i32 = run_distributed_lock_redis_op(
            "extend lock",
            script
                .key(&lock_key)
                .arg(lock_value)
                .arg(ttl_seconds)
                .invoke_async::<i32>(&mut conn),
        )
        .await?;

        Ok(result == 1)
    }
}

/// RAII lock guard that releases on explicit `release()` or best-effort on Drop.
///
/// **Preferred usage**: Call `release()` explicitly for guaranteed lock release.
/// The `Drop` implementation signals a dedicated background task (spawned at
/// construction time) to perform the unlock, so it works even if the Tokio
/// runtime is in the process of shutting down — the task is already running and
/// the oneshot send is non-blocking.
///
/// # Example
/// ```text
/// let guard = LockGuard::new(&lock, "create_room:user123".to_string(), 10).await?;
/// // Lock is held
/// let result = room_service.create_room(request).await;
/// // Explicitly release for guaranteed cleanup
/// guard.release().await;
/// result?;
/// ```
#[must_use = "lock guard must be explicitly released via .release() for reliable unlock"]
pub struct LockGuard {
    lock: DistributedLock,
    key: String,
    value: Option<String>,
    /// Fencing token for CAS operations (0 if not requested)
    fencing_token: u64,
    /// Sender half of the oneshot channel used to trigger the background
    /// unlock task from `Drop`. Wrapped in `Option` so `release()` can take
    /// it to prevent a double-signal.
    drop_tx: Option<tokio::sync::oneshot::Sender<(String, String)>>,
}

impl LockGuard {
    /// Spawn the background unlock task and return the oneshot sender.
    ///
    /// The task waits for either:
    /// - A `(key, value)` tuple sent by `Drop` (or `release()`), in which case
    ///   it calls `lock.release()` asynchronously; or
    /// - The sender to be dropped without sending (the guard was already
    ///   released explicitly), in which case it does nothing.
    fn spawn_drop_task(lock: DistributedLock) -> tokio::sync::oneshot::Sender<(String, String)> {
        let (tx, rx) = tokio::sync::oneshot::channel::<(String, String)>();
        tokio::spawn(async move {
            if let Ok((key, value)) = rx.await {
                if let Err(e) = lock.release(&key, &value).await {
                    tracing::error!(
                        key = %key,
                        error = %e,
                        "Background task failed to release lock"
                    );
                }
            } else {
                // Sender was dropped without sending — lock was already
                // released explicitly via release(). Nothing to do.
            }
        });
        tx
    }

    /// Create a new lock guard (acquires lock without fencing token)
    pub async fn new(lock: DistributedLock, key: String, ttl_seconds: u64) -> Result<Self> {
        let value = lock
            .acquire(&key, ttl_seconds)
            .await?
            .ok_or_else(|| Error::LockConflict(format!("Lock already held: {key}")))?;

        let drop_tx = Some(Self::spawn_drop_task(lock.clone()));

        Ok(Self {
            lock,
            key,
            value: Some(value),
            fencing_token: 0,
            drop_tx,
        })
    }

    /// Create a new lock guard with fencing token
    ///
    /// The fencing token can be used for CAS operations in the database layer.
    pub async fn new_with_token(
        lock: DistributedLock,
        key: String,
        ttl_seconds: u64,
    ) -> Result<Self> {
        let (value, fencing_token) = lock
            .acquire_with_token(&key, ttl_seconds)
            .await?
            .ok_or_else(|| Error::LockConflict(format!("Lock already held: {key}")))?;

        let drop_tx = Some(Self::spawn_drop_task(lock.clone()));

        Ok(Self {
            lock,
            key,
            value: Some(value),
            fencing_token,
            drop_tx,
        })
    }

    /// Get the fencing token for this lock guard
    ///
    /// Returns 0 if the guard was created without requesting a token.
    #[must_use]
    pub const fn fencing_token(&self) -> u64 {
        self.fencing_token
    }

    /// Extend the lock TTL
    pub async fn extend(&self, ttl_seconds: u64) -> Result<bool> {
        if let Some(ref value) = self.value {
            self.lock.extend(&self.key, value, ttl_seconds).await
        } else {
            Ok(false)
        }
    }

    /// Explicitly release the lock (preferred over relying on Drop)
    pub async fn release(mut self) -> Result<bool> {
        // Disarm the background drop task by dropping the sender without
        // sending, then perform the release directly on the current task.
        let _ = self.drop_tx.take();

        if let Some(value) = self.value.take() {
            self.lock.release(&self.key, &value).await
        } else {
            Ok(false)
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Only attempt release if not already explicitly released
        if let Some(value) = self.value.take() {
            // Signal the already-running background task to perform the
            // unlock. The send is non-blocking and safe even if the Tokio
            // runtime is shutting down, because the task was spawned earlier
            // while the runtime was healthy.
            if let Some(tx) = self.drop_tx.take() {
                let key = self.key.clone();
                if tx.send((key.clone(), value)).is_err() {
                    // The background task exited prematurely (should not
                    // happen in normal operation).
                    tracing::warn!(
                        key = %key,
                        "Lock drop task exited before receiving unlock signal; \
                         lock will expire after TTL"
                    );
                }
            }
        }
    }
}

// ========== Redlock Implementation ==========
//
// The Redlock algorithm provides distributed lock safety across Redis failovers
// by requiring quorum across multiple independent Redis masters.
//
// Key properties:
// - Acquire lock on N/2+1 Redis masters (quorum)
// - Use short TTL to minimize split-brain window
// - Random value identifies lock holder (prevents accidental release)
// - Release on all instances (best-effort)

/// Configuration for Redlock with multiple independent Redis masters.
///
/// Redlock requires at least 3 independent Redis masters for proper fault tolerance.
/// The recommended setup is 5 masters for production.
#[derive(Clone, Debug)]
pub struct RedlockConfig {
    /// URLs of independent Redis masters (minimum 3)
    pub master_urls: Vec<String>,
    /// Lock TTL in milliseconds
    pub ttl_ms: u64,
    /// Maximum time to wait for lock acquisition in milliseconds
    pub acquire_timeout_ms: u64,
    /// Retry interval between acquisition attempts in milliseconds
    pub retry_interval_ms: u64,
}

impl Default for RedlockConfig {
    fn default() -> Self {
        Self {
            master_urls: Vec::new(),
            ttl_ms: 10_000,            // 10 seconds
            acquire_timeout_ms: 5_000, // 5 seconds
            retry_interval_ms: 50,     // 50ms
        }
    }
}

/// Redlock implementation using multiple independent Redis masters.
///
/// Provides true distributed lock safety across Redis failovers by requiring
/// quorum (N/2+1) across multiple independent Redis instances.
///
/// # Safety Guarantee
///
/// Unlike single-instance locks, Redlock guarantees that during a Sentinel
/// failover or network partition, at most one client can hold the lock at
/// any given time, assuming:
/// - At least N/2+1 Redis masters are available
/// - Clocks are reasonably synchronized (within 1 second)
///
/// # Trade-offs
///
/// - **Latency**: Higher latency due to multiple Redis round-trips
/// - **Complexity**: Requires managing multiple Redis connections
/// - **Correctness**: True distributed lock safety during failovers
///
/// # Example
///
/// ```text
/// let config = RedlockConfig {
///     master_urls: vec![
///         "redis://redis1:6379".to_string(),
///         "redis://redis2:6379".to_string(),
///         "redis://redis3:6379".to_string(),
///     ],
///     ..Default::default()
/// };
/// let redlock = Redlock::new(config).await?;
///
/// if let Some(guard) = redlock.acquire("my_resource").await? {
///     // Critical section - only one client at a time
///     // Guard releases lock on drop
/// }
/// ```
pub struct Redlock {
    /// Connections to independent Redis masters
    connections: Vec<RedisConnectionManager>,
    config: RedlockConfig,
}

impl Redlock {
    /// Create a new Redlock instance with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Fewer than 3 master URLs are provided
    /// - Failed to connect to any Redis master
    pub async fn new(config: RedlockConfig) -> crate::Result<Self> {
        if config.master_urls.len() < 3 {
            return Err(Error::Internal(
                "Redlock requires at least 3 independent Redis masters".to_string(),
            ));
        }

        let mut connections = Vec::with_capacity(config.master_urls.len());
        for url in &config.master_urls {
            let client = redis::Client::open(url.as_str())
                .internal_with_err(&format!("Failed to create Redis client for {url}"))?;
            let conn = RedisConnectionManager::new(client)
                .await
                .internal_with_err(&format!("Failed to connect to Redis at {url}"))?;
            connections.push(conn);
        }

        Ok(Self {
            connections,
            config,
        })
    }

    /// Generate a unique lock value using the shared base62 ID generator.
    fn generate_lock_value() -> String {
        crate::models::generate_id()
    }

    /// Get current time in milliseconds since Unix epoch.
    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            })
    }

    /// Release lock on a single Redis instance (best-effort).
    async fn release_single(
        &self,
        conn: &mut RedisConnectionManager,
        lock_key: &str,
        lock_value: &str,
    ) {
        let script = Script::new(
            r#"
            if redis.call("GET", KEYS[1]) == ARGV[1] then
                return redis.call("DEL", KEYS[1])
            else
                return 0
            end
            "#,
        );

        let _ = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            script
                .key(lock_key)
                .arg(lock_value)
                .invoke_async::<i32>(conn),
        )
        .await;
    }

    /// Acquire a distributed lock using Redlock algorithm.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(RedlockGuard))` if lock was acquired
    /// - `Ok(None)` if lock could not be acquired (quorum not reached)
    /// - `Err(...)` on infrastructure failure
    pub async fn acquire(&self, key: &str) -> crate::Result<Option<RedlockGuard>> {
        let lock_key = format!("lock:{key}");
        let lock_value = Self::generate_lock_value();
        let quorum = (self.connections.len() / 2) + 1;
        let start_time = Self::now_ms();

        loop {
            let mut acquired = 0;
            let mut conns_to_release: Vec<usize> = Vec::new();

            // Try to acquire lock on all instances in parallel
            let acquire_tasks: Vec<_> = self
                .connections
                .iter()
                .enumerate()
                .map(|(i, conn)| {
                    let mut conn = conn.clone();
                    let lock_key = lock_key.clone();
                    let lock_value = lock_value.clone();
                    let ttl_ms = self.config.ttl_ms;
                    async move {
                        (
                            i,
                            Self::try_acquire_single_static(
                                &mut conn,
                                &lock_key,
                                &lock_value,
                                ttl_ms,
                            )
                            .await,
                        )
                    }
                })
                .collect();

            let results = futures::future::join_all(acquire_tasks).await;

            for (i, success) in results {
                if success {
                    acquired += 1;
                    conns_to_release.push(i);
                }
            }

            // Check if we have quorum
            if acquired >= quorum {
                // Calculate time remaining for validity
                let elapsed = Self::now_ms() - start_time;
                let remaining_ttl = self.config.ttl_ms.saturating_sub(elapsed);

                if remaining_ttl > 0 {
                    tracing::debug!(
                        lock_key = %lock_key,
                        quorum = %acquired,
                        total = %self.connections.len(),
                        remaining_ttl_ms = %remaining_ttl,
                        "Redlock acquired"
                    );

                    return Ok(Some(RedlockGuard {
                        redlock: self.clone_ref(),
                        lock_key,
                        lock_value,
                        connections_to_release: conns_to_release,
                    }));
                }
                // Lock already expired during acquisition
                tracing::warn!(
                    lock_key = %lock_key,
                    elapsed_ms = %elapsed,
                    "Redlock acquisition took longer than TTL"
                );
                // Release any locks we acquired
                self.release_on_connections(&lock_key, &lock_value, &conns_to_release)
                    .await;
            } else {
                // Didn't get quorum - release any locks we did acquire
                self.release_on_connections(&lock_key, &lock_value, &conns_to_release)
                    .await;
            }

            // Check if we've exceeded acquire timeout
            let elapsed = Self::now_ms() - start_time;
            if elapsed >= self.config.acquire_timeout_ms {
                tracing::debug!(
                    lock_key = %lock_key,
                    acquired = %acquired,
                    needed = %quorum,
                    "Redlock acquisition timed out"
                );
                return Ok(None);
            }

            // Wait before retry
            tokio::time::sleep(std::time::Duration::from_millis(
                self.config.retry_interval_ms,
            ))
            .await;
        }
    }

    /// Helper for static context (no &self)
    async fn try_acquire_single_static(
        conn: &mut RedisConnectionManager,
        lock_key: &str,
        lock_value: &str,
        ttl_ms: u64,
    ) -> bool {
        let result: Option<String> = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            redis::cmd("SET")
                .arg(lock_key)
                .arg(lock_value)
                .arg("NX")
                .arg("PX")
                .arg(ttl_ms)
                .query_async(conn),
        )
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .flatten();

        result.is_some()
    }

    /// Release lock on specific connections.
    async fn release_on_connections(
        &self,
        lock_key: &str,
        lock_value: &str,
        connection_indices: &[usize],
    ) {
        for &i in connection_indices {
            if let Some(conn) = self.connections.get(i) {
                let mut conn = conn.clone();
                self.release_single(&mut conn, lock_key, lock_value).await;
            }
        }
    }

    /// Get a clone reference for the guard.
    fn clone_ref(&self) -> RedlockRef {
        RedlockRef {
            connections: self.connections.clone(),
        }
    }
}

/// Reference to Redlock for use in guard.
#[derive(Clone)]
struct RedlockRef {
    connections: Vec<RedisConnectionManager>,
}

impl RedlockRef {
    /// Release lock on all connections that hold it.
    async fn release(&self, lock_key: &str, lock_value: &str) {
        let release_tasks: Vec<_> = self
            .connections
            .iter()
            .map(|conn| {
                let mut conn = conn.clone();
                let lock_key = lock_key.to_string();
                let lock_value = lock_value.to_string();
                async move {
                    let script = Script::new(
                        r#"
                        if redis.call("GET", KEYS[1]) == ARGV[1] then
                            return redis.call("DEL", KEYS[1])
                        else
                            return 0
                        end
                        "#,
                    );
                    let _ = tokio::time::timeout(
                        crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
                        script
                            .key(&lock_key)
                            .arg(&lock_value)
                            .invoke_async::<i32>(&mut conn),
                    )
                    .await;
                }
            })
            .collect();

        futures::future::join_all(release_tasks).await;
    }
}

/// RAII guard for Redlock that releases on drop.
#[must_use = "RedlockGuard releases lock on drop"]
pub struct RedlockGuard {
    redlock: RedlockRef,
    lock_key: String,
    lock_value: String,
    connections_to_release: Vec<usize>,
}

impl RedlockGuard {
    /// Explicitly release the lock.
    pub async fn release(mut self) {
        self.redlock.release(&self.lock_key, &self.lock_value).await;
        // Prevent Drop from running
        self.connections_to_release.clear();
    }
}

impl Drop for RedlockGuard {
    fn drop(&mut self) {
        if self.connections_to_release.is_empty() {
            // Already released
            return;
        }

        // Best-effort async release via spawn
        let redlock = self.redlock.clone();
        let lock_key = self.lock_key.clone();
        let lock_value = self.lock_value.clone();

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                redlock.release(&lock_key, &lock_value).await;
            });
        } else {
            tracing::warn!(
                lock_key = %self.lock_key,
                "Skipping Redlock async release because no Tokio runtime is available; lock will expire via TTL"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;
    use async_trait::async_trait;

    // ========== Unit Tests (No Docker Required) ==========

    #[test]
    fn test_lock_key_format() {
        // Test that lock key is properly formatted with "lock:" prefix
        let key = "my_resource";
        let lock_key = format!("lock:{key}");
        assert_eq!(lock_key, "lock:my_resource");
    }

    #[test]
    fn test_token_key_format() {
        // Test that token key is properly formatted
        let key = "my_resource";
        let token_key = format!("lock:token:{key}");
        assert_eq!(token_key, "lock:token:my_resource");
    }

    #[test]
    fn test_backoff_calculation() {
        // Test exponential backoff calculation: base_ms * 2^attempt
        let base_ms: u64 = 5;
        assert_eq!(base_ms, 5); // attempt 0: 5ms
        assert_eq!(base_ms * (1 << 1), 10); // attempt 1: 10ms
        assert_eq!(base_ms * (1 << 2), 20); // attempt 2: 20ms
        assert_eq!(base_ms * (1 << 3), 40); // attempt 3: 40ms
    }

    #[test]
    fn test_client_timeout_calculation() {
        // Test client timeout: ttl_seconds + 5s for network round-trips
        let ttl_seconds: u64 = 10;
        let client_timeout = std::time::Duration::from_secs(ttl_seconds + 5);
        assert_eq!(client_timeout, std::time::Duration::from_secs(15));
    }

    #[tokio::test]
    async fn test_distributed_lock_accepts_trait_object_runtime() {
        #[derive(Clone)]
        struct FakeRedisRuntime;

        #[async_trait]
        impl RedisConnectionRuntime for FakeRedisRuntime {
            async fn snapshot(&self) -> RedisConnectionManager {
                panic!("snapshot should not be called in constructor-only test");
            }
        }

        let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
        let lock = DistributedLock::from_runtime(runtime.clone());

        assert!(
            Arc::ptr_eq(&lock.redis_runtime, &runtime),
            "distributed lock should retain the injected Redis runtime object"
        );
    }

    #[test]
    fn test_distributed_lock_from_runtime_with_mode_retains_injected_runtime() {
        #[derive(Clone)]
        struct FakeRedisRuntime;

        #[async_trait]
        impl RedisConnectionRuntime for FakeRedisRuntime {
            async fn snapshot(&self) -> RedisConnectionManager {
                panic!("snapshot should not be called in constructor-only test");
            }
        }

        let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
        let lock = DistributedLock::from_runtime_with_mode(runtime.clone(), false);

        assert!(
            Arc::ptr_eq(&lock.redis_runtime, &runtime),
            "distributed lock should retain the injected runtime even in deployment-aware mode"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_distributed_lock_redis_timeout_maps_to_timeout_error() {
        let timeout_future = run_distributed_lock_redis_op("acquire lock", async {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok::<(), redis::RedisError>(())
        });

        tokio::pin!(timeout_future);
        tokio::task::yield_now().await;
        tokio::time::advance(crate::resilience::timeout::REDIS_OPERATION_TIMEOUT).await;

        let err = timeout_future.await.expect_err("operation should time out");
        assert!(matches!(
            err,
            Error::Timeout(ref msg) if msg == "Redis timeout: acquire lock"
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn test_distributed_lock_client_timeout_maps_to_timeout_error() {
        let timeout_future =
            run_distributed_lock_client_op("test-key", std::time::Duration::from_secs(15), async {
                std::future::pending::<()>().await;
                #[allow(unreachable_code)]
                Ok::<(), Error>(())
            });

        tokio::pin!(timeout_future);
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(15)).await;

        let err = timeout_future.await.expect_err("operation should time out");
        assert!(matches!(
            err,
            Error::Timeout(ref msg) if msg == "Lock operation timed out for key: test-key"
        ));
    }

    #[test]
    fn test_lua_script_release_logic() {
        // The release Lua script logic:
        // if GET key == lock_value then DEL key else return 0
        // This test verifies the expected behavior conceptually

        // Scenario 1: Value matches -> delete (return 1)
        let stored_value = "abc123";
        let provided_value = "abc123";
        assert_eq!(stored_value, provided_value); // Would delete

        // Scenario 2: Value doesn't match -> no delete (return 0)
        let stored_value = "abc123";
        let provided_value = "xyz789";
        assert_ne!(stored_value, provided_value); // Would not delete

        // Scenario 3: Key doesn't exist -> no delete (return 0)
        // This is handled by GET returning nil
    }

    // ========== MigrationLock Trait Tests ==========

    // ========== Redlock Unit Tests ==========

    #[test]
    fn test_redlock_quorum_calculation() {
        // Redlock requires N/2 + 1 quorum
        // For 3 masters: quorum = 2
        // For 5 masters: quorum = 3
        assert_eq!((3 / 2) + 1, 2);
        assert_eq!((5 / 2) + 1, 3);
        assert_eq!((7 / 2) + 1, 4);
    }

    #[test]
    fn test_redlock_minimum_masters() {
        // Redlock requires at least 3 masters
        // This is enforced in Redlock::new()
        let insufficient_masters = ["redis://host1".to_string(), "redis://host2".to_string()];
        assert!(insufficient_masters.len() < 3);

        let sufficient_masters = [
            "redis://host1".to_string(),
            "redis://host2".to_string(),
            "redis://host3".to_string(),
        ];
        assert!(sufficient_masters.len() >= 3);
    }

    #[test]
    fn test_redlock_time_calculation() {
        // Test that time calculation works
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            });
        assert!(now > 0);
    }

    #[test]
    fn test_redlock_lock_value_generation() {
        // Lock values should be unique
        let val1 = crate::models::generate_id();
        let val2 = crate::models::generate_id();
        assert_ne!(val1, val2);
        assert!(!val1.is_empty());
    }

    #[test]
    fn test_redlock_validity_remaining() {
        // If acquisition takes 3ms with 10ms TTL, 7ms remains
        let ttl_ms: u64 = 10;
        let elapsed_ms: u64 = 3;
        let remaining = ttl_ms.saturating_sub(elapsed_ms);
        assert_eq!(remaining, 7);

        // If acquisition takes longer than TTL, remaining is 0
        let elapsed_ms: u64 = 15;
        let remaining = ttl_ms.saturating_sub(elapsed_ms);
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn test_redlock_rejects_insufficient_masters() {
        let config = RedlockConfig {
            master_urls: vec!["redis://host1".to_string(), "redis://host2".to_string()],
            ..Default::default()
        };

        // Should fail because only 2 masters provided (need at least 3)
        let result = Redlock::new(config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_redlock_accepts_three_masters() {
        // This test will fail to connect but validates the count check works.
        // Use 127.0.0.1 with unused ports — connection is refused instantly
        // (no DNS lookup or connect timeout delay).
        let config = RedlockConfig {
            master_urls: vec![
                "redis://127.0.0.1:1".to_string(),
                "redis://127.0.0.1:2".to_string(),
                "redis://127.0.0.1:3".to_string(),
            ],
            ..Default::default()
        };

        // Should fail at connection, not at master count validation.
        // Wrap with timeout since ConnectionManager has internal retry logic.
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), Redlock::new(config)).await;
        // Either timed out or got a connection error — both mean count validation passed
        assert!(
            result.is_err() || result.unwrap().is_err(),
            "Should fail due to unreachable Redis, not count validation"
        );
    }

    #[test]
    fn test_redlock_guard_drop_without_runtime_does_not_panic() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let guard = RedlockGuard {
                redlock: RedlockRef {
                    connections: Vec::new(),
                },
                lock_key: "lock:test".to_string(),
                lock_value: "value".to_string(),
                connections_to_release: vec![0],
            };
            drop(guard);
        }));

        assert!(
            result.is_ok(),
            "RedlockGuard::drop must not panic without a Tokio runtime"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn pg_advisory_migration_lock_acquire_returns_none_when_lock_is_held() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let lock1 = PgAdvisoryMigrationLock::new(pool.clone());
        let lock2 = PgAdvisoryMigrationLock::new(pool);

        let first = MigrationLock::acquire(&lock1, "migration", 30)
            .await
            .expect("first advisory lock acquisition should succeed")
            .expect("first advisory lock acquisition should own the lock");

        let second = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            MigrationLock::acquire(&lock2, "migration", 30),
        )
        .await
        .expect("second acquire should not block waiting for the advisory lock")
        .expect("second acquire should not error while the advisory lock is held");

        assert!(
            second.is_none(),
            "held advisory lock must report contention via Ok(None), not wait or error"
        );

        assert!(
            MigrationLock::release(&lock1, "migration", &first)
                .await
                .expect("first advisory lock should release cleanly"),
            "first advisory lock release should report success"
        );
    }
}
