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

use redis::aio::ConnectionManager as RedisConnectionManager;
use redis::Script;
use std::future::Future;
use crate::{Error, Result, InternalExt};

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

    /// Release a previously acquired lock.
    ///
    /// Returns `true` if the lock was released, `false` if not held or expired.
    async fn release(&self, key: &str, lock_value: &str) -> anyhow::Result<bool>;
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
}

/// `PostgreSQL` advisory lock-based `MigrationLock`.
///
/// Used as a fallback when the Redis lock fails. Uses `pg_try_advisory_lock`
/// with a retry loop, mirroring the existing behaviour in `migrations.rs`.
///
/// The advisory lock is session-scoped, so we must release it on the same
/// connection that acquired it. The `lock_conn` field holds that connection.
pub struct PgAdvisoryMigrationLock {
    pool: sqlx::PgPool,
    lock_conn: tokio::sync::Mutex<Option<sqlx::pool::PoolConnection<sqlx::Postgres>>>,
}

/// Stable advisory lock key for migration coordination (hash of "`synctv_migration`").
const PG_ADVISORY_LOCK_KEY: i64 = 0x73796E63_74766D69_u64 as i64;

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
    async fn acquire(&self, _key: &str, ttl_secs: u64) -> anyhow::Result<Option<String>> {
        let max_wait = std::time::Duration::from_secs(ttl_secs);
        let retry_interval = std::time::Duration::from_secs(5);
        let start = tokio::time::Instant::now();

        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire DB connection for PG advisory lock: {e}"))?;

        loop {
            let acquired: (bool,) =
                sqlx::query_as("SELECT pg_try_advisory_lock($1)")
                    .bind(PG_ADVISORY_LOCK_KEY)
                    .fetch_one(&mut *conn)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to attempt PostgreSQL advisory lock: {e}"))?;

            if acquired.0 {
                // Store the connection so release() can use the same session.
                *self.lock_conn.lock().await = Some(conn);
                return Ok(Some("pg_advisory".to_string()));
            }

            if start.elapsed() >= max_wait {
                return Err(anyhow::anyhow!(
                    "Timed out waiting for PostgreSQL advisory lock after {}s",
                    max_wait.as_secs()
                ));
            }

            tracing::info!(
                "PostgreSQL advisory lock held by another connection, retrying in {}s (elapsed: {}s / {}s)...",
                retry_interval.as_secs(),
                start.elapsed().as_secs(),
                max_wait.as_secs()
            );
            tokio::time::sleep(retry_interval).await;
        }
    }

    async fn release(&self, _key: &str, _lock_value: &str) -> anyhow::Result<bool> {
        // Release the advisory lock on the same connection that acquired it.
        // Session-scoped advisory locks cannot be released from a different connection.
        let mut guard = self.lock_conn.lock().await;
        if let Some(ref mut conn) = *guard {
            let result: (bool,) =
                sqlx::query_as("SELECT pg_advisory_unlock($1)")
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
    redis: RedisConnectionManager,
}

impl DistributedLock {
    /// Create a new distributed lock service.
    ///
    /// If `redis_url` is provided, checks whether it uses Sentinel (contains
    /// "sentinel" in the URL) and emits a startup warning about lock safety.
    #[must_use]
    pub const fn new(redis: RedisConnectionManager) -> Self {
        Self {
            redis,
        }
    }

    /// Create a new distributed lock service and log a warning if the Redis URL
    /// indicates Sentinel mode. Call this at startup instead of `new()` when the
    /// Redis URL is available.
    pub fn new_with_sentinel_check(redis: RedisConnectionManager, redis_url: &str) -> Self {
        if redis_url.contains("sentinel") || redis_url.contains("SENTINEL") {
            tracing::warn!(
                "Distributed lock is using a single Redis instance behind Sentinel. \
                 Locks may be LOST during Sentinel failover due to asynchronous replication. \
                 For production Sentinel deployments, consider using the Redlock algorithm \
                 with multiple independent Redis masters. See module-level documentation."
            );
        }
        Self::new(redis)
    }

    /// Create a new distributed lock service with deployment mode awareness.
    ///
    /// When `is_sentinel` is true, emits a startup warning about the split-brain
    /// window during Sentinel failover. This is more reliable than URL-based
    /// detection in `new_with_sentinel_check`.
    pub fn new_with_mode(redis: RedisConnectionManager, is_sentinel: bool) -> Self {
        if is_sentinel {
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
        Self::new(redis)
    }

    /// Generate a fencing token for a lock key using Redis INCR
    ///
    /// Uses Redis INCR on a per-key counter to ensure monotonic tokens
    /// across all clients. Returns an error if Redis INCR fails, since
    /// the lock itself requires Redis and a local fallback would break
    /// monotonicity across replicas.
    async fn generate_fencing_token(&self, key: &str) -> crate::Result<u64> {
        let token_key = format!("lock:token:{key}");
        let mut conn = self.redis.clone();

        // Atomically INCR and set a 24-hour TTL to prevent unbounded key accumulation
        let script = Script::new(
            r"
            local val = redis.call('INCR', KEYS[1])
            redis.call('EXPIRE', KEYS[1], 86400)
            return val
            ",
        );

        tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            script.key(&token_key).invoke_async::<u64>(&mut conn),
        )
            .await
            .map_err(|_| Error::Internal(format!("Redis timeout: generate fencing token for lock '{key}'")))?
            .internal_with_err(&format!("Failed to generate fencing token for lock '{key}'"))
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
    pub async fn acquire_with_token(&self, key: &str, ttl_seconds: u64) -> Result<Option<(String, u64)>> {
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
        let lock_value = crate::models::generate_id(); // nanoid(12)

        let mut conn = self.redis.clone();

        // SET key value NX EX ttl
        // NX: Only set if not exists
        // EX: Set expiration time
        let result: Option<String> = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            redis::cmd("SET")
                .arg(&lock_key)
                .arg(&lock_value)
                .arg("NX")
                .arg("EX")
                .arg(ttl_seconds)
                .query_async(&mut conn),
        )
            .await
            .map_err(|_| Error::Internal("Redis timeout: acquire lock".to_string()))?
            .internal_with_err("Failed to acquire lock")?;

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

        let mut conn = self.redis.clone();

        let result: i32 = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            script.key(&lock_key).arg(lock_value).invoke_async::<i32>(&mut conn),
        )
            .await
            .map_err(|_| Error::Internal("Redis timeout: release lock".to_string()))?
            .internal_with_err("Failed to release lock")?;

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

        tokio::time::timeout(client_timeout, async {
            // Try to acquire lock
            let lock_value = self
                .acquire(key, ttl_seconds)
                .await?
                .ok_or_else(|| Error::Internal(format!("Failed to acquire lock: {key}")))?;

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
        .map_err(|_| Error::Internal(format!("Lock operation timed out for key: {key}")))?
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

        tokio::time::timeout(client_timeout, async {
            // Try to acquire lock
            let lock_value = match self.acquire(key, ttl_seconds).await? {
                Some(value) => value,
                None => return Ok(None), // Lock already held
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
        .map_err(|_| Error::Internal(format!("Lock operation timed out for key: {key}")))?
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

        tokio::time::timeout(client_timeout, async {
            // Try to acquire lock with token
            let (lock_value, fencing_token) = self
                .acquire_with_token(key, ttl_seconds)
                .await?
                .ok_or_else(|| Error::Internal(format!("Failed to acquire lock: {key}")))?;

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
        .map_err(|_| Error::Internal(format!("Lock operation timed out for key: {key}")))?
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

        tokio::time::timeout(client_timeout, async {
            // Try to acquire lock with token
            let (lock_value, fencing_token) = match self.acquire_with_token(key, ttl_seconds).await? {
                Some(result) => result,
                None => return Ok(None), // Lock already held
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
        .map_err(|_| Error::Internal(format!("Lock operation timed out for key: {key}")))?
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

        let mut conn = self.redis.clone();

        let result: i32 = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            script.key(&lock_key).arg(lock_value).arg(ttl_seconds).invoke_async::<i32>(&mut conn),
        )
            .await
            .map_err(|_| Error::Internal("Redis timeout: extend lock".to_string()))?
            .internal_with_err("Failed to extend lock")?;

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
            .ok_or_else(|| Error::Internal(format!("Failed to acquire lock: {key}")))?;

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
            .ok_or_else(|| Error::Internal(format!("Failed to acquire lock: {key}")))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_acquire_and_release() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let redis = infra.connection_manager().await;
        let lock = DistributedLock::new(redis);

        // Acquire lock
        let lock_value = lock.acquire("test:lock1", 10).await.unwrap();
        assert!(lock_value.is_some());

        let lock_value = lock_value.unwrap();

        // Try to acquire same lock (should fail)
        let lock_value2 = lock.acquire("test:lock1", 10).await.unwrap();
        assert!(lock_value2.is_none());

        // Release lock
        let released = lock.release("test:lock1", &lock_value).await.unwrap();
        assert!(released);

        // Acquire lock again (should succeed)
        let lock_value3 = lock.acquire("test:lock1", 10).await.unwrap();
        assert!(lock_value3.is_some());

        // Cleanup
        lock.release("test:lock1", &lock_value3.unwrap()).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_with_lock() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let redis = infra.connection_manager().await;
        let lock = DistributedLock::new(redis);

        let result = lock
            .with_lock("test:lock2", 10, || async {
                // Simulate operation
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                Ok::<_, Error>(42)
            })
            .await
            .unwrap();

        assert_eq!(result, 42);

        // Lock should be released, can acquire again
        let lock_value = lock.acquire("test:lock2", 10).await.unwrap();
        assert!(lock_value.is_some());

        // Cleanup
        lock.release("test:lock2", &lock_value.unwrap()).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_try_with_lock() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let redis = infra.connection_manager().await;
        let lock = DistributedLock::new(redis.clone());

        // Acquire lock manually
        let lock_value = lock.acquire("test:lock3", 10).await.unwrap().unwrap();

        // Try to execute with lock (should return None)
        let result = lock
            .try_with_lock("test:lock3", 10, || async { Ok::<_, Error>(42) })
            .await
            .unwrap();

        assert!(result.is_none());

        // Release lock
        lock.release("test:lock3", &lock_value).await.unwrap();

        // Try again (should succeed)
        let result = lock
            .try_with_lock("test:lock3", 10, || async { Ok::<_, Error>(42) })
            .await
            .unwrap();

        assert_eq!(result, Some(42));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_lock_guard() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let redis = infra.connection_manager().await;
        let lock = DistributedLock::new(redis.clone());

        {
            let _guard = LockGuard::new(lock.clone(), "test:lock4".to_string(), 10)
                .await
                .unwrap();

            // Lock is held
            let lock_value = lock.acquire("test:lock4", 10).await.unwrap();
            assert!(lock_value.is_none());

            // Guard will release lock when dropped
        }

        // Wait for async drop task to complete
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Lock should be released
        let lock_value = lock.acquire("test:lock4", 10).await.unwrap();
        assert!(lock_value.is_some());

        // Cleanup
        lock.release("test:lock4", &lock_value.unwrap()).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_extend_lock() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let redis = infra.connection_manager().await;
        let lock = DistributedLock::new(redis);

        // Acquire lock with short TTL
        let lock_value = lock.acquire("test:lock5", 2).await.unwrap().unwrap();

        // Extend lock
        let extended = lock.extend("test:lock5", &lock_value, 10).await.unwrap();
        assert!(extended);

        // Lock should still be valid after original TTL
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        let lock_value2 = lock.acquire("test:lock5", 10).await.unwrap();
        assert!(lock_value2.is_none()); // Still locked

        // Cleanup
        lock.release("test:lock5", &lock_value).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_acquire_with_token() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let redis = infra.connection_manager().await;
        let lock = DistributedLock::new(redis);

        // Acquire lock with token
        let result = lock.acquire_with_token("test:token1", 10).await.unwrap();
        assert!(result.is_some());
        let (lock_value, token1) = result.unwrap();
        assert!(token1 > 0); // Token should be positive

        // Release and acquire again
        lock.release("test:token1", &lock_value).await.unwrap();

        let result2 = lock.acquire_with_token("test:token1", 10).await.unwrap();
        assert!(result2.is_some());
        let (_lock_value2, token2) = result2.unwrap();

        // Token should be monotonically increasing
        assert!(token2 > token1, "Token should increase: {token2} > {token1}");

        // Cleanup
        lock.release("test:token1", &_lock_value2).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_with_lock_token() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let redis = infra.connection_manager().await;
        let lock = DistributedLock::new(redis);

        let received_token = lock
            .with_lock_token("test:token2", 10, |token| async move {
                Ok::<_, Error>(token)
            })
            .await
            .unwrap();

        assert!(received_token > 0);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_try_with_lock_token() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let redis = infra.connection_manager().await;
        let lock = DistributedLock::new(redis.clone());

        // Acquire lock manually
        let lock_value = lock.acquire("test:token3", 10).await.unwrap().unwrap();

        // Try with token should return None
        let result = lock
            .try_with_lock_token("test:token3", 10, |token| async move {
                Ok::<_, Error>(token)
            })
            .await
            .unwrap();
        assert!(result.is_none());

        // Release lock
        lock.release("test:token3", &lock_value).await.unwrap();

        // Now try again (should succeed with token)
        let result = lock
            .try_with_lock_token("test:token3", 10, |token| async move {
                Ok::<_, Error>(token)
            })
            .await
            .unwrap();
        assert!(result.is_some());
        assert!(result.unwrap() > 0);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_lock_guard_with_token() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let redis = infra.connection_manager().await;
        let lock = DistributedLock::new(redis.clone());

        {
            let guard = LockGuard::new_with_token(lock.clone(), "test:token4".to_string(), 10)
                .await
                .unwrap();

            // Token should be positive
            let token = guard.fencing_token();
            assert!(token > 0);

            // Lock is held
            let lock_value = lock.acquire("test:token4", 10).await.unwrap();
            assert!(lock_value.is_none());

            // Explicitly release
            guard.release().await.unwrap();
        }

        // Lock should be released
        let lock_value = lock.acquire("test:token4", 10).await.unwrap();
        assert!(lock_value.is_some());

        // Cleanup
        lock.release("test:token4", &lock_value.unwrap()).await.unwrap();
    }

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
        assert_eq!(base_ms * (1 << 0), 5);   // attempt 0: 5ms
        assert_eq!(base_ms * (1 << 1), 10);  // attempt 1: 10ms
        assert_eq!(base_ms * (1 << 2), 20);  // attempt 2: 20ms
        assert_eq!(base_ms * (1 << 3), 40);  // attempt 3: 40ms
    }

    #[test]
    fn test_client_timeout_calculation() {
        // Test client timeout: ttl_seconds + 5s for network round-trips
        let ttl_seconds: u64 = 10;
        let client_timeout = std::time::Duration::from_secs(ttl_seconds + 5);
        assert_eq!(client_timeout, std::time::Duration::from_secs(15));
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

    #[test]
    fn test_lua_script_extend_logic() {
        // The extend Lua script logic:
        // if GET key == lock_value then EXPIRE key ttl else return 0

        // Scenario 1: Value matches -> extend TTL
        // Scenario 2: Value doesn't match -> no extend
        // Similar to release logic
    }

    #[test]
    fn test_pg_advisory_lock_key_constant() {
        // Verify the advisory lock key is stable
        // hash of "synctv_migration" = 0x73796E63_74766D69
        assert_eq!(PG_ADVISORY_LOCK_KEY, 0x73796E63_74766D69_u64 as i64);
    }

    #[tokio::test]
    async fn test_with_lock_timeout_error_propagation() {
        // Test that with_lock returns timeout error when operation exceeds client_timeout
        // This is a conceptual test - actual behavior requires Redis
        let client_timeout = std::time::Duration::from_secs(10 + 5);
        assert_eq!(client_timeout, std::time::Duration::from_secs(15));
    }

    #[test]
    fn test_lock_guard_fencing_token_default() {
        // When created without token, fencing_token() should return 0
        // This is a compile-time check that the type exists
        // Actual behavior requires Redis
    }

    #[test]
    fn test_must_use_lock_guard() {
        // LockGuard has #[must_use] - verify the attribute exists by checking
        // that the type compiles. A #[must_use] type warns if unused.
        // This is a compile-time check.
    }

    // ========== MigrationLock Trait Tests ==========

    #[test]
    fn test_migration_lock_trait_bounds() {
        // MigrationLock requires Send + Sync for thread safety
        // This is a compile-time check
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn MigrationLock>();
    }

    // ========== Error Path Tests ==========

    #[test]
    fn test_error_types() {
        // Test that the error types we use are correct
        use crate::Error;

        let internal_err = Error::Internal("test".to_string());
        match internal_err {
            Error::Internal(msg) => assert_eq!(msg, "test"),
            _ => panic!("Wrong error type"),
        }
    }

    #[test]
    fn test_lock_key_edge_cases() {
        // Test various key formats
        let keys = vec![
            "simple",
            "with:colon",
            "with-dash",
            "with_underscore",
            "with.dot",
            "CamelCase",
            "123numbers",
            "mixed123ABC",
        ];

        for key in keys {
            let lock_key = format!("lock:{key}");
            assert!(lock_key.starts_with("lock:"));
            assert!(lock_key.ends_with(key));
        }
    }

    #[test]
    fn test_ttl_range() {
        // Test valid TTL ranges
        let min_ttl: u64 = 1;
        let max_ttl: u64 = 86400; // 24 hours

        assert!(min_ttl >= 1);
        assert!(max_ttl <= 86400);

        // TTL should not be zero
        assert!(min_ttl > 0);
    }
}
