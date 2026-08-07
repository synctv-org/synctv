//! Redis-based distributed lock with fencing tokens.
//!
//! Single-instance Redis provides the lock state. Sentinel failover can drop
//! in-flight locks during master promotion, so fencing tokens stay part of the
//! API for protected writes. Cluster mode stays rejected at config validation.

use crate::{Error, InternalExt, RedisConnectionRuntime, Result};
use redis::aio::ConnectionManager as RedisConnectionManager;
use redis::Script;
use std::future::Future;
use std::sync::{Arc, LazyLock};

mod guard;
#[cfg(test)]
mod tests;

pub use guard::LockGuard;

static GENERATE_FENCING_TOKEN_SCRIPT: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r"
        local val = redis.call('INCR', KEYS[1])
        redis.call('EXPIRE', KEYS[1], 86400)
        return val
        ",
    )
});

static RELEASE_LOCK_SCRIPT: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r#"
        if redis.call("GET", KEYS[1]) == ARGV[1] then
            return redis.call("DEL", KEYS[1])
        else
            return 0
        end
        "#,
    )
});

static EXTEND_LOCK_SCRIPT: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r#"
        if redis.call("GET", KEYS[1]) == ARGV[1] then
            return redis.call("EXPIRE", KEYS[1], ARGV[2])
        else
            return 0
        end
        "#,
    )
});

async fn run_distributed_lock_redis_op<T, F>(
    timeout: std::time::Duration,
    operation: impl Into<String>,
    future: F,
) -> Result<T>
where
    F: Future<Output = std::result::Result<T, redis::RedisError>>,
{
    let operation = operation.into();
    tokio::time::timeout(timeout, future)
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

/// Object-safe coordination lock for application-layer critical sections.
///
/// This trait uses the crate's native [`Result`] type so business services can
/// depend on it directly without knowing the concrete coordination backend.
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
/// Applies a client-side timeout and best-effort unlock around the protected
/// operation while keeping callers independent from the concrete backend.
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

#[async_trait::async_trait]
impl CoordinationLock for DistributedLock {
    async fn acquire(&self, key: &str, ttl_secs: u64) -> Result<Option<String>> {
        Self::acquire(self, key, ttl_secs).await
    }

    async fn release(&self, key: &str, lock_value: &str) -> Result<bool> {
        Self::release(self, key, lock_value).await
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
            "Distributed lock is running behind Redis Sentinel. Failover can drop locks held on the old master. Fencing tokens stay required for protected writes."
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
    pub fn new_shared_with_mode(
        redis: Arc<tokio::sync::RwLock<RedisConnectionManager>>,
        is_sentinel: bool,
    ) -> Self {
        Self::from_runtime_with_mode(crate::shared_runtime(redis), is_sentinel)
    }

    async fn conn(&self, operation: impl Into<String>) -> crate::Result<RedisConnectionManager> {
        crate::redis_runtime_snapshot(&*self.redis_runtime, operation).await
    }

    /// Generate a fencing token for a lock key using Redis INCR
    ///
    /// Uses Redis INCR on a per-key counter to ensure monotonic tokens
    /// across all clients. Returns an error if Redis INCR fails, since
    /// the lock itself requires Redis and a local fallback would break
    /// monotonicity across replicas.
    async fn generate_fencing_token(&self, key: &str) -> crate::Result<u64> {
        let token_key = format!("lock:token:{key}");
        let mut conn = self
            .conn(format!("generate fencing token for lock '{key}'"))
            .await?;

        run_distributed_lock_redis_op(
            self.redis_runtime.operation_timeout(),
            format!("generate fencing token for lock '{key}'"),
            GENERATE_FENCING_TOKEN_SCRIPT
                .key(&token_key)
                .invoke_async::<u64>(&mut conn),
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
        match self.acquire_internal(key, ttl_seconds, true).await? {
            Some((value, Some(token))) => Ok(Some((value, token))),
            Some((_value, None)) => Err(Error::Internal(
                "Distributed lock acquired without required fencing token".to_string(),
            )),
            None => Ok(None),
        }
    }

    /// Internal acquire implementation
    async fn acquire_internal(
        &self,
        key: &str,
        ttl_seconds: u64,
        with_token: bool,
    ) -> Result<Option<(String, Option<u64>)>> {
        let lock_key = format!("lock:{key}");
        let lock_value = synctv_common::snanoid!(16);

        let mut conn = self.conn(format!("acquire lock '{key}'")).await?;

        // SET key value NX EX ttl
        // NX: Only set if not exists
        // EX: Set expiration time
        let result: Option<String> = run_distributed_lock_redis_op(
            self.redis_runtime.operation_timeout(),
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
            let fencing_token = if with_token {
                Some(self.generate_fencing_token(key).await?)
            } else {
                None
            };

            tracing::debug!(
                lock_key = %lock_key,
                lock_value = %lock_value,
                fencing_token = ?fencing_token,
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

        let mut conn = self.conn(format!("release lock '{key}'")).await?;

        let result: i32 = run_distributed_lock_redis_op(
            self.redis_runtime.operation_timeout(),
            "release lock",
            RELEASE_LOCK_SCRIPT
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
    /// let updated = match lock.try_with_lock("update_settings:room123", 10, || async {
    ///     room_service.update_settings(settings).await
    /// }).await? {
    ///     Some(result) => result,
    ///     None => return Ok(()),
    /// };
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
    /// let updated = match lock.try_with_lock_token("update_settings:room123", 10, |token| async move {
    ///     room_service.update_settings_with_token(settings, token).await
    /// }).await? {
    ///     Some(result) => result,
    ///     None => return Ok(()),
    /// };
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

        let mut conn = self.conn(format!("extend lock '{key}'")).await?;

        let result: i32 = run_distributed_lock_redis_op(
            self.redis_runtime.operation_timeout(),
            "extend lock",
            EXTEND_LOCK_SCRIPT
                .key(&lock_key)
                .arg(lock_value)
                .arg(ttl_seconds)
                .invoke_async::<i32>(&mut conn),
        )
        .await?;

        Ok(result == 1)
    }
}
