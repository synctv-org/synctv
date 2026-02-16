//! Distributed lock service using Redis
//!
//! Design reference: /Volumes/workspace/rust/synctv-rs-design/21-关键实现.md §12.2.3
//!
//! Provides distributed locking mechanism for multi-replica deployments.
//! Uses Redis SET NX EX for atomic lock acquisition.
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
//! ```ignore
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

/// Distributed lock service
///
/// Provides Redis-based distributed locking for cross-replica critical sections
/// with fencing token support for protection against split-brain scenarios.
#[derive(Clone)]
pub struct DistributedLock {
    redis: RedisConnectionManager,
}

impl DistributedLock {
    /// Create a new distributed lock service
    #[must_use]
    pub fn new(redis: RedisConnectionManager) -> Self {
        Self {
            redis,
        }
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
            r#"
            local val = redis.call('INCR', KEYS[1])
            redis.call('EXPIRE', KEYS[1], 86400)
            return val
            "#,
        );

        script
            .key(&token_key)
            .invoke_async::<u64>(&mut conn)
            .await
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
    /// ```ignore
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
    /// ```ignore
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
        let result: Option<String> = redis::cmd("SET")
            .arg(&lock_key)
            .arg(&lock_value)
            .arg("NX")
            .arg("EX")
            .arg(ttl_seconds)
            .query_async(&mut conn)
            .await
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

        let result: i32 = script
            .key(&lock_key)
            .arg(lock_value)
            .invoke_async::<i32>(&mut conn)
            .await
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
    /// ```ignore
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
    }

    /// Try to acquire a lock and execute an operation
    ///
    /// Returns None if lock is already held, Some(T) if operation succeeded
    ///
    /// # Example
    /// ```ignore
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
    /// ```ignore
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
    }

    /// Try to acquire a lock and execute an operation (with fencing token)
    ///
    /// Same as `try_with_lock` but passes the fencing token to the operation.
    ///
    /// # Example
    /// ```ignore
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

        let result: i32 = script
            .key(&lock_key)
            .arg(lock_value)
            .arg(ttl_seconds)
            .invoke_async::<i32>(&mut conn)
            .await
            .internal_with_err("Failed to extend lock")?;

        Ok(result == 1)
    }
}

/// RAII lock guard that releases on explicit `release()` or best-effort on Drop.
///
/// **Preferred usage**: Call `release()` explicitly for guaranteed lock release.
/// The `Drop` implementation is a safety net that uses `tokio::spawn` for
/// best-effort async release, but may fail if the runtime is shutting down.
///
/// # Example
/// ```ignore
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
}

impl LockGuard {
    /// Create a new lock guard (acquires lock without fencing token)
    pub async fn new(lock: DistributedLock, key: String, ttl_seconds: u64) -> Result<Self> {
        let value = lock
            .acquire(&key, ttl_seconds)
            .await?
            .ok_or_else(|| Error::Internal(format!("Failed to acquire lock: {key}")))?;

        Ok(Self {
            lock,
            key,
            value: Some(value),
            fencing_token: 0,
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

        Ok(Self {
            lock,
            key,
            value: Some(value),
            fencing_token,
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
            let lock = self.lock.clone();
            let key = self.key.clone();

            // Best-effort: try to spawn an async release task.
            // This may fail if the runtime is shutting down.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if let Err(e) = lock.release(&key, &value).await {
                        tracing::error!(
                            key = %key,
                            error = %e,
                            "Failed to release lock in Drop"
                        );
                    }
                });
            } else {
                tracing::warn!(
                    key = %key,
                    "Cannot release lock in Drop: no tokio runtime available (lock will expire after TTL)"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
}
