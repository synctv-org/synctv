//! L2 cache backend trait for `TieredCache`.
//!
//! Provides pluggable L2 storage behind the L1 Moka in-memory cache.
//!
//! ## Implementations
//!
//! - `RedisCacheL2`: Redis-backed L2 with TTL, retry logic, and atomic set-if-newer.
//! - `NoopCacheL2`: No-op backend (L1-only mode). All reads return None, all writes are no-ops.

use crate::resilience::timeout::REDIS_OPERATION_TIMEOUT;
use crate::{Error, Result};
use async_trait::async_trait;

/// Backend for the L2 (remote) cache layer in `TieredCache`.
///
/// All values are passed as serialized JSON strings. Serialization/deserialization
/// is handled by the `TieredCache` itself.
#[async_trait]
pub trait CacheL2Backend: Send + Sync {
    /// Get a JSON value by key. Returns `None` if not found or expired.
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Set a JSON value with TTL in seconds.
    async fn set(&self, key: &str, json: &str, ttl_secs: u64) -> Result<()>;

    /// Delete a key.
    async fn delete(&self, key: &str) -> Result<()>;

    /// Delete a key with retry logic and exponential backoff.
    async fn delete_with_retry(&self, key: &str, max_retries: u32, cache_type: &str) -> Result<()>;

    /// Get multiple values by key. Returns a `Vec` of the same length as `keys`.
    async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>>;

    /// Atomically set a value only if it's newer than the existing value.
    ///
    /// `new_ts_iso` is the ISO-8601 timestamp string of the new value's `updated_at` field.
    /// Returns `true` if the value was set (new is newer), `false` if skipped.
    async fn set_if_newer(
        &self,
        key: &str,
        json: &str,
        ttl_secs: u64,
        new_ts_iso: &str,
    ) -> Result<bool>;

    /// Delete all keys matching the given prefix using a Redis SCAN + DEL loop.
    ///
    /// Used during lag-triggered full cache flushes to also evict stale L2
    /// entries, preventing other replicas from re-populating L1 from stale data.
    async fn delete_by_prefix(&self, prefix: &str) -> Result<()>;

    /// Whether this backend is active (i.e., has a real remote store).
    /// Used for metrics and TTL enforcement decisions.
    fn is_active(&self) -> bool;

    /// A label for logging/debug purposes.
    fn backend_name(&self) -> &'static str;
}

// ============================================================================
// Redis implementation
// ============================================================================

/// Redis-backed L2 cache backend.
///
/// In Sentinel mode, the inner connection is hot-swapped on failover via
/// the shared `Arc<RwLock<ConnectionManager>>`. Each operation reads the
/// latest connection handle, so it automatically follows Sentinel failover.
///
/// In standalone mode (or when constructed with a plain `ConnectionManager`),
/// the `RwLock` always holds the same handle, and `ConnectionManager` handles
/// transient reconnections internally.
pub struct RedisCacheL2 {
    conn: std::sync::Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
}

impl RedisCacheL2 {
    /// Create from a shared, hot-swappable connection (recommended for Sentinel mode).
    #[must_use]
    pub const fn new_shared(
        conn: std::sync::Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
    ) -> Self {
        Self { conn }
    }

    /// Create from a plain `ConnectionManager` snapshot.
    ///
    /// Wraps it in an `Arc<RwLock<>>` internally for API uniformity. Suitable
    /// for standalone mode where the connection is never hot-swapped.
    #[must_use]
    pub fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self {
            conn: std::sync::Arc::new(tokio::sync::RwLock::new(conn)),
        }
    }

    /// Get a clone of the current `ConnectionManager` for use in an operation.
    async fn conn(&self) -> redis::aio::ConnectionManager {
        self.conn.read().await.clone()
    }
}

#[async_trait]
impl CacheL2Backend for RedisCacheL2 {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        use redis::AsyncCommands;
        let mut conn = self.conn().await;

        let result =
            tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.get::<_, Option<String>>(key))
                .await
                .map_err(|_| Error::Internal("L2 cache get operation timed out".to_string()))?
                .map_err(|e| Error::Internal(format!("Failed to get from L2 cache: {e}")))?;
        Ok(result)
    }

    async fn set(&self, key: &str, json: &str, ttl_secs: u64) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.conn().await;

        tokio::time::timeout(
            REDIS_OPERATION_TIMEOUT,
            conn.set_ex::<_, _, ()>(key, json, ttl_secs),
        )
        .await
        .map_err(|_| Error::Internal("L2 cache set operation timed out".to_string()))?
        .map_err(|e| Error::Internal(format!("Failed to set in L2 cache: {e}")))?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.conn().await;

        tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.del::<_, ()>(key))
            .await
            .map_err(|_| Error::Internal("L2 cache delete operation timed out".to_string()))?
            .map_err(|e| Error::Internal(format!("Failed to delete from L2 cache: {e}")))?;
        Ok(())
    }

    async fn delete_with_retry(&self, key: &str, max_retries: u32, cache_type: &str) -> Result<()> {
        use redis::AsyncCommands;
        for attempt in 0..max_retries {
            let mut conn = self.conn().await;
            let result =
                tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.del::<_, ()>(key)).await;
            match result {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(e)) => {
                    let is_last_attempt = attempt == max_retries - 1;
                    if is_last_attempt {
                        crate::metrics::cache::CACHE_ERRORS
                            .with_label_values(&[cache_type, "l2_delete"])
                            .inc();
                        tracing::error!(
                            key = %key,
                            error = %e,
                            attempts = max_retries,
                            cache_type = %cache_type,
                            "Failed to delete from Redis L2 cache after retries"
                        );
                        return Err(Error::Internal(format!(
                            "Failed to delete from Redis cache: {e}"
                        )));
                    } else {
                        let backoff_ms = 10 * u64::pow(5, attempt);
                        tracing::warn!(
                            key = %key,
                            error = %e,
                            attempt = attempt + 1,
                            max_retries = max_retries,
                            backoff_ms = backoff_ms,
                            cache_type = %cache_type,
                            "Redis L2 cache delete failed, retrying"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    }
                }
                Err(_) => {
                    let is_last_attempt = attempt == max_retries - 1;
                    if is_last_attempt {
                        crate::metrics::cache::CACHE_ERRORS
                            .with_label_values(&[cache_type, "l2_delete"])
                            .inc();
                        tracing::error!(
                            key = %key,
                            attempts = max_retries,
                            cache_type = %cache_type,
                            "Redis L2 cache delete timed out after retries"
                        );
                        return Err(Error::Internal(
                            "Failed to delete from Redis cache: operation timed out".to_string(),
                        ));
                    } else {
                        let backoff_ms = 10 * u64::pow(5, attempt);
                        tracing::warn!(
                            key = %key,
                            attempt = attempt + 1,
                            max_retries = max_retries,
                            backoff_ms = backoff_ms,
                            cache_type = %cache_type,
                            "Redis L2 cache delete timed out, retrying"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    }
                }
            }
        }
        Ok(())
    }

    async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>> {
        let mut conn = self.conn().await;
        let mut pipe = redis::pipe();
        for key in keys {
            pipe.get(key);
        }

        let results: Vec<Option<String>> =
            tokio::time::timeout(REDIS_OPERATION_TIMEOUT, pipe.query_async(&mut conn))
                .await
                .map_err(|_| Error::Internal("L2 cache batch get operation timed out".to_string()))?
                .map_err(|e| Error::Internal(format!("Failed to batch get from L2: {e}")))?;
        Ok(results)
    }

    async fn set_if_newer(
        &self,
        key: &str,
        json: &str,
        ttl_secs: u64,
        new_ts_iso: &str,
    ) -> Result<bool> {
        let mut conn = self.conn().await;

        // Lua script: atomically GET existing JSON, parse its updated_at inside
        // Lua via cjson, compare with the new timestamp, and SET only if newer.
        let script = redis::Script::new(
            r"
            local existing = redis.call('GET', KEYS[1])
            if existing then
                local ok, obj = pcall(cjson.decode, existing)
                if ok and obj and obj.updated_at then
                    local existing_ts = obj.updated_at
                    local new_ts = ARGV[3]
                    if new_ts <= existing_ts then
                        return 0
                    end
                end
            end
            redis.call('SET', KEYS[1], ARGV[1], 'EX', ARGV[2])
            return 1
            ",
        );

        let result: i64 = tokio::time::timeout(
            REDIS_OPERATION_TIMEOUT,
            script
                .key(key)
                .arg(json)
                .arg(ttl_secs as i64)
                .arg(new_ts_iso)
                .invoke_async(&mut conn),
        )
        .await
        .map_err(|_| Error::Internal("L2 cache set_if_newer operation timed out".to_string()))?
        .map_err(|e| Error::Internal(format!("Failed to run set_if_newer Lua script: {e}")))?;

        Ok(result == 1)
    }

    async fn delete_by_prefix(&self, prefix: &str) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.conn().await;

        // Use SCAN to iterate keys matching the prefix pattern, then DEL in batches.
        // This avoids blocking Redis with KEYS * on large keyspaces.
        let pattern = format!("{prefix}*");
        let mut cursor: u64 = 0;
        loop {
            let scan_result: (u64, Vec<String>) = tokio::time::timeout(
                REDIS_OPERATION_TIMEOUT,
                redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(&pattern)
                    .arg("COUNT")
                    .arg(100u64)
                    .query_async(&mut conn),
            )
            .await
            .map_err(|_| Error::Internal(format!("SCAN timed out for prefix '{prefix}'")))?
            .map_err(|e| Error::Internal(format!("SCAN failed for prefix '{prefix}': {e}")))?;

            let (next_cursor, keys) = scan_result;

            if !keys.is_empty() {
                tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.del::<_, ()>(keys.as_slice()))
                    .await
                    .map_err(|_| Error::Internal(format!("DEL timed out for prefix '{prefix}'")))?
                    .map_err(|e| {
                        Error::Internal(format!("DEL failed for prefix '{prefix}': {e}"))
                    })?;
            }

            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }
        Ok(())
    }

    fn is_active(&self) -> bool {
        true
    }

    fn backend_name(&self) -> &'static str {
        "redis"
    }
}

// ============================================================================
// No-op implementation (L1-only mode)
// ============================================================================

/// No-op L2 backend. All reads return `None`, all writes are no-ops.
///
/// Used when Redis is not configured — `TieredCache` runs in L1-only mode.
pub struct NoopCacheL2;

#[async_trait]
impl CacheL2Backend for NoopCacheL2 {
    async fn get(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn set(&self, _key: &str, _json: &str, _ttl_secs: u64) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, _key: &str) -> Result<()> {
        Ok(())
    }

    async fn delete_with_retry(
        &self,
        _key: &str,
        _max_retries: u32,
        _cache_type: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>> {
        Ok(vec![None; keys.len()])
    }

    async fn set_if_newer(
        &self,
        _key: &str,
        _json: &str,
        _ttl_secs: u64,
        _new_ts_iso: &str,
    ) -> Result<bool> {
        // No L2 — always allow the caller to proceed with L1 update
        Ok(true)
    }

    async fn delete_by_prefix(&self, _prefix: &str) -> Result<()> {
        Ok(())
    }

    fn is_active(&self) -> bool {
        false
    }

    fn backend_name(&self) -> &'static str {
        "noop"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// A mock L2 backend that can simulate slow operations for testing timeouts.
    struct SlowMockBackend {
        delay: Duration,
        get_count: Arc<AtomicU64>,
        set_count: Arc<AtomicU64>,
        delete_count: Arc<AtomicU64>,
    }

    impl SlowMockBackend {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                get_count: Arc::new(AtomicU64::new(0)),
                set_count: Arc::new(AtomicU64::new(0)),
                delete_count: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    #[async_trait]
    impl CacheL2Backend for SlowMockBackend {
        async fn get(&self, _key: &str) -> Result<Option<String>> {
            self.get_count.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Ok(Some(r#"{"value":"test"}"#.to_string()))
        }

        async fn set(&self, _key: &str, _json: &str, _ttl_secs: u64) -> Result<()> {
            self.set_count.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Ok(())
        }

        async fn delete(&self, _key: &str) -> Result<()> {
            self.delete_count.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Ok(())
        }

        async fn delete_with_retry(
            &self,
            key: &str,
            _max_retries: u32,
            _cache_type: &str,
        ) -> Result<()> {
            self.delete(key).await
        }

        async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>> {
            // Simulate slow batch operation
            tokio::time::sleep(self.delay).await;
            Ok(keys
                .iter()
                .map(|_| Some(r#"{"value":"test"}"#.to_string()))
                .collect())
        }

        async fn set_if_newer(
            &self,
            _key: &str,
            _json: &str,
            _ttl_secs: u64,
            _new_ts_iso: &str,
        ) -> Result<bool> {
            tokio::time::sleep(self.delay).await;
            Ok(true)
        }

        async fn delete_by_prefix(&self, _prefix: &str) -> Result<()> {
            tokio::time::sleep(self.delay).await;
            Ok(())
        }

        fn is_active(&self) -> bool {
            true
        }

        fn backend_name(&self) -> &'static str {
            "slow_mock"
        }
    }

    /// Short timeout for tests — just needs to prove the timeout mechanism works.
    const TEST_TIMEOUT: Duration = Duration::from_millis(200);

    /// Test that get() times out when operation takes too long
    #[tokio::test]
    async fn test_get_timeout() {
        let backend = SlowMockBackend::new(Duration::from_secs(2));
        let start = std::time::Instant::now();

        let result = tokio::time::timeout(TEST_TIMEOUT, backend.get("test_key")).await;

        let elapsed = start.elapsed();

        assert!(result.is_err(), "Expected timeout error");
        assert!(
            elapsed < Duration::from_millis(500),
            "Timeout should occur around 200ms, took {elapsed:?}"
        );
    }

    /// Test that set() times out when operation takes too long
    #[tokio::test]
    async fn test_set_timeout() {
        let backend = SlowMockBackend::new(Duration::from_secs(2));
        let start = std::time::Instant::now();

        let result = tokio::time::timeout(TEST_TIMEOUT, backend.set("key", "{}", 60)).await;

        let elapsed = start.elapsed();

        assert!(result.is_err(), "Expected timeout error");
        assert!(
            elapsed < Duration::from_millis(500),
            "Timeout should occur around 200ms, took {elapsed:?}"
        );
    }

    /// Test that delete() times out when operation takes too long
    #[tokio::test]
    async fn test_delete_timeout() {
        let backend = SlowMockBackend::new(Duration::from_secs(2));
        let start = std::time::Instant::now();

        let result = tokio::time::timeout(TEST_TIMEOUT, backend.delete("key")).await;

        let elapsed = start.elapsed();

        assert!(result.is_err(), "Expected timeout error");
        assert!(
            elapsed < Duration::from_millis(500),
            "Timeout should occur around 200ms, took {elapsed:?}"
        );
    }

    /// Test that get_batch() times out when operation takes too long
    #[tokio::test]
    async fn test_get_batch_timeout() {
        let backend = SlowMockBackend::new(Duration::from_secs(2));
        let keys: Vec<String> = vec!["key1".to_string(), "key2".to_string()];
        let start = std::time::Instant::now();

        let result = tokio::time::timeout(TEST_TIMEOUT, backend.get_batch(&keys)).await;

        let elapsed = start.elapsed();

        assert!(result.is_err(), "Expected timeout error");
        assert!(
            elapsed < Duration::from_millis(500),
            "Timeout should occur around 200ms, took {elapsed:?}"
        );
    }

    /// Test that set_if_newer() times out when operation takes too long
    #[tokio::test]
    async fn test_set_if_newer_timeout() {
        let backend = SlowMockBackend::new(Duration::from_secs(2));
        let start = std::time::Instant::now();

        let result = tokio::time::timeout(
            TEST_TIMEOUT,
            backend.set_if_newer("key", "{}", 60, "2024-01-01T00:00:00Z"),
        )
        .await;

        let elapsed = start.elapsed();

        assert!(result.is_err(), "Expected timeout error");
        assert!(
            elapsed < Duration::from_millis(500),
            "Timeout should occur around 200ms, took {elapsed:?}"
        );
    }

    /// Test that delete_by_prefix() times out when operation takes too long
    #[tokio::test]
    async fn test_delete_by_prefix_timeout() {
        let backend = SlowMockBackend::new(Duration::from_secs(2));
        let start = std::time::Instant::now();

        let result = tokio::time::timeout(TEST_TIMEOUT, backend.delete_by_prefix("prefix")).await;

        let elapsed = start.elapsed();

        assert!(result.is_err(), "Expected timeout error");
        assert!(
            elapsed < Duration::from_millis(500),
            "Timeout should occur around 200ms, took {elapsed:?}"
        );
    }

    /// Test that fast operations complete successfully within timeout
    #[tokio::test]
    async fn test_fast_operations_succeed() {
        let backend = SlowMockBackend::new(Duration::from_millis(10));

        // All these should complete quickly
        let get_result = tokio::time::timeout(TEST_TIMEOUT, backend.get("key")).await;
        assert!(get_result.is_ok());
        assert!(get_result.unwrap().is_ok());

        let set_result = tokio::time::timeout(TEST_TIMEOUT, backend.set("key", "{}", 60)).await;
        assert!(set_result.is_ok());
        assert!(set_result.unwrap().is_ok());

        let delete_result = tokio::time::timeout(TEST_TIMEOUT, backend.delete("key")).await;
        assert!(delete_result.is_ok());
        assert!(delete_result.unwrap().is_ok());
    }

    /// Test NoopCacheL2 always succeeds (instant operations)
    #[tokio::test]
    async fn test_noop_backend_fast() {
        let backend = NoopCacheL2;

        // All operations should be instant
        let start = std::time::Instant::now();

        let result = backend.get("key").await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());

        let result = backend.set("key", "{}", 60).await;
        assert!(result.is_ok());

        let result = backend.delete("key").await;
        assert!(result.is_ok());

        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(10),
            "NoopCacheL2 operations should be instant, took {elapsed:?}"
        );
    }

    /// Test NoopCacheL2 backend_name and is_active
    #[test]
    fn test_noop_backend_metadata() {
        let backend = NoopCacheL2;
        assert_eq!(backend.backend_name(), "noop");
        assert!(!backend.is_active());
    }
}
