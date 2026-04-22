//! L2 cache backend trait for `TieredCache`.
//!
//! Provides pluggable L2 storage behind the L1 Moka in-memory cache.
//!
//! ## Implementations
//!
//! - `RedisCacheL2`: Redis-backed L2 with TTL, retry logic, and atomic set-if-newer.
//! - `NoopCacheL2`: No-op backend (L1-only mode). All reads return None, all writes are no-ops.

use crate::resilience::timeout::REDIS_OPERATION_TIMEOUT;
use crate::{Error, RedisConnectionRuntime, Result, SharedStateProfile};
use async_trait::async_trait;
use std::future::Future;
use std::time::{SystemTime, UNIX_EPOCH};

enum L2RedisAttemptError {
    Redis(redis::RedisError),
    Timeout,
}

async fn run_l2_redis_attempt<T, F>(future: F) -> std::result::Result<T, L2RedisAttemptError>
where
    F: Future<Output = std::result::Result<T, redis::RedisError>>,
{
    match tokio::time::timeout(REDIS_OPERATION_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(L2RedisAttemptError::Redis(err)),
        Err(_) => Err(L2RedisAttemptError::Timeout),
    }
}

async fn run_l2_redis_op<T, F>(operation: impl Into<String>, future: F) -> Result<T>
where
    F: Future<Output = std::result::Result<T, redis::RedisError>>,
{
    let operation = operation.into();
    match run_l2_redis_attempt(future).await {
        Ok(value) => Ok(value),
        Err(L2RedisAttemptError::Timeout) => {
            Err(Error::Timeout(format!("L2 cache timeout: {operation}")))
        }
        Err(L2RedisAttemptError::Redis(err)) => {
            Err(Error::Internal(format!("Failed to {operation}: {err}")))
        }
    }
}

/// Backend for the L2 (remote) cache layer in `TieredCache`.
///
/// All values are passed as serialized JSON strings. Serialization/deserialization
/// is handled by the `TieredCache` itself.
#[async_trait]
pub trait CacheL2Backend: Send + Sync {
    /// Get a JSON value by key. Returns `None` if not found or expired.
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Get a JSON value by key within a logical namespace.
    ///
    /// The namespace is used by backends that maintain auxiliary indexes for
    /// efficient prefix invalidation. Backends without namespace tracking can
    /// ignore it and delegate to [`CacheL2Backend::get`].
    async fn get_scoped(&self, _prefix: &str, key: &str) -> Result<Option<String>> {
        self.get(key).await
    }

    /// Set a JSON value with TTL in seconds.
    async fn set(&self, key: &str, json: &str, ttl_secs: u64) -> Result<()>;

    /// Set a JSON value with TTL in seconds within a logical namespace.
    async fn set_scoped(&self, _prefix: &str, key: &str, json: &str, ttl_secs: u64) -> Result<()> {
        self.set(key, json, ttl_secs).await
    }

    /// Delete a key.
    async fn delete(&self, key: &str) -> Result<()>;

    /// Delete a key within a logical namespace.
    async fn delete_scoped(&self, _prefix: &str, key: &str) -> Result<()> {
        self.delete(key).await
    }

    /// Delete a key with retry logic and exponential backoff.
    async fn delete_with_retry(&self, key: &str, max_retries: u32, cache_type: &str) -> Result<()>;

    /// Delete a key with retry logic within a logical namespace.
    async fn delete_with_retry_scoped(
        &self,
        _prefix: &str,
        key: &str,
        max_retries: u32,
        cache_type: &str,
    ) -> Result<()> {
        self.delete_with_retry(key, max_retries, cache_type).await
    }

    /// Get multiple values by key. Returns a `Vec` of the same length as `keys`.
    async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>>;

    /// Get multiple values by key within a logical namespace.
    async fn get_batch_scoped(
        &self,
        _prefix: &str,
        keys: &[String],
    ) -> Result<Vec<Option<String>>> {
        self.get_batch(keys).await
    }

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

    /// Atomically set a value only if it's newer than the existing value within
    /// a logical namespace.
    async fn set_if_newer_scoped(
        &self,
        _prefix: &str,
        key: &str,
        json: &str,
        ttl_secs: u64,
        new_ts_iso: &str,
    ) -> Result<bool> {
        self.set_if_newer(key, json, ttl_secs, new_ts_iso).await
    }

    /// Delete all keys matching the given prefix.
    ///
    /// Used during lag-triggered full cache flushes to also evict stale L2
    /// entries, preventing other replicas from re-populating L1 from stale data.
    async fn delete_by_prefix(&self, prefix: &str) -> Result<()>;

    /// Whether this backend is active (i.e., has a real remote store).
    /// Used for metrics and TTL enforcement decisions.
    fn is_active(&self) -> bool;
}

// Redis implementation

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
    conn: std::sync::Arc<dyn RedisConnectionRuntime>,
}

impl RedisCacheL2 {
    #[must_use]
    pub fn from_runtime(conn: std::sync::Arc<dyn RedisConnectionRuntime>) -> Self {
        Self { conn }
    }

    /// Get a clone of the current `ConnectionManager` for use in an operation.
    async fn conn(&self) -> redis::aio::ConnectionManager {
        self.conn.snapshot().await
    }

    fn namespace_index_key(prefix: &str) -> String {
        format!("{prefix}__l2_index")
    }

    fn now_unix_seconds() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .try_into()
            .unwrap_or(i64::MAX)
    }

    fn expiry_timestamp(ttl_secs: u64) -> i64 {
        Self::now_unix_seconds().saturating_add(ttl_secs.try_into().unwrap_or(i64::MAX))
    }
}

#[must_use]
pub fn build_l2_cache_backend(
    redis_runtime: Option<std::sync::Arc<dyn RedisConnectionRuntime>>,
) -> std::sync::Arc<dyn CacheL2Backend> {
    match redis_runtime {
        Some(runtime) => std::sync::Arc::new(RedisCacheL2::from_runtime(runtime)),
        None => std::sync::Arc::new(NoopCacheL2),
    }
}

#[must_use]
pub fn local_l2_cache_backend() -> std::sync::Arc<dyn CacheL2Backend> {
    std::sync::Arc::new(NoopCacheL2)
}

#[must_use]
pub fn build_l2_cache_backend_from_profile(
    profile: &SharedStateProfile,
) -> std::sync::Arc<dyn CacheL2Backend> {
    build_l2_cache_backend(profile.shared_runtime())
}

#[async_trait]
impl CacheL2Backend for RedisCacheL2 {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        use redis::AsyncCommands;
        let mut conn = self.conn().await;

        let result =
            run_l2_redis_op("get from L2 cache", conn.get::<_, Option<String>>(key)).await?;
        Ok(result)
    }

    async fn get_scoped(&self, prefix: &str, key: &str) -> Result<Option<String>> {
        use redis::AsyncCommands;

        let mut conn = self.conn().await;
        let result =
            run_l2_redis_op("get from L2 cache", conn.get::<_, Option<String>>(key)).await?;

        if result.is_none() {
            let index_key = Self::namespace_index_key(prefix);
            if let Err(error) = conn.zrem::<_, _, usize>(&index_key, key).await {
                tracing::debug!(
                    prefix = %prefix,
                    key = %key,
                    error = %error,
                    "Failed to prune missing L2 key from namespace index"
                );
            }
        }

        Ok(result)
    }

    async fn set(&self, key: &str, json: &str, ttl_secs: u64) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.conn().await;

        run_l2_redis_op(
            "set in L2 cache",
            conn.set_ex::<_, _, ()>(key, json, ttl_secs),
        )
        .await?;
        Ok(())
    }

    async fn set_scoped(&self, prefix: &str, key: &str, json: &str, ttl_secs: u64) -> Result<()> {
        let mut conn = self.conn().await;
        let index_key = Self::namespace_index_key(prefix);
        let expires_at = Self::expiry_timestamp(ttl_secs);
        let now = Self::now_unix_seconds();

        let mut pipe = redis::pipe();
        pipe.atomic()
            .cmd("SET")
            .arg(key)
            .arg(json)
            .arg("EX")
            .arg(ttl_secs)
            .ignore()
            .cmd("ZADD")
            .arg(&index_key)
            .arg(expires_at)
            .arg(key)
            .ignore()
            .cmd("ZREMRANGEBYSCORE")
            .arg(&index_key)
            .arg("-inf")
            .arg(now)
            .ignore();

        run_l2_redis_op(
            format!("set in L2 cache namespace '{prefix}'"),
            pipe.query_async::<()>(&mut conn),
        )
        .await?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.conn().await;

        run_l2_redis_op("delete from L2 cache", conn.del::<_, ()>(key)).await?;
        Ok(())
    }

    async fn delete_scoped(&self, prefix: &str, key: &str) -> Result<()> {
        let mut conn = self.conn().await;
        let index_key = Self::namespace_index_key(prefix);
        let mut pipe = redis::pipe();
        pipe.atomic()
            .cmd("DEL")
            .arg(key)
            .ignore()
            .cmd("ZREM")
            .arg(&index_key)
            .arg(key)
            .ignore();

        run_l2_redis_op(
            format!("delete from L2 cache namespace '{prefix}'"),
            pipe.query_async::<()>(&mut conn),
        )
        .await?;
        Ok(())
    }

    async fn delete_with_retry(&self, key: &str, max_retries: u32, cache_type: &str) -> Result<()> {
        use redis::AsyncCommands;
        for attempt in 0..max_retries {
            let mut conn = self.conn().await;
            match run_l2_redis_attempt(conn.del::<_, ()>(key)).await {
                Ok(()) => return Ok(()),
                Err(L2RedisAttemptError::Redis(e)) => {
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
                    }
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
                Err(L2RedisAttemptError::Timeout) => {
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
                        return Err(Error::Timeout(
                            "L2 cache timeout: delete from Redis cache".to_string(),
                        ));
                    }
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
        Ok(())
    }

    async fn delete_with_retry_scoped(
        &self,
        prefix: &str,
        key: &str,
        max_retries: u32,
        cache_type: &str,
    ) -> Result<()> {
        for attempt in 0..max_retries {
            let mut conn = self.conn().await;
            let index_key = Self::namespace_index_key(prefix);
            let mut pipe = redis::pipe();
            pipe.atomic()
                .cmd("DEL")
                .arg(key)
                .ignore()
                .cmd("ZREM")
                .arg(&index_key)
                .arg(key)
                .ignore();

            match run_l2_redis_attempt(pipe.query_async::<()>(&mut conn)).await {
                Ok(()) => return Ok(()),
                Err(L2RedisAttemptError::Redis(e)) => {
                    let is_last_attempt = attempt == max_retries - 1;
                    if is_last_attempt {
                        crate::metrics::cache::CACHE_ERRORS
                            .with_label_values(&[cache_type, "l2_delete"])
                            .inc();
                        tracing::error!(
                            key = %key,
                            prefix = %prefix,
                            error = %e,
                            attempts = max_retries,
                            cache_type = %cache_type,
                            "Failed to delete from Redis L2 cache namespace after retries"
                        );
                        return Err(Error::Internal(format!(
                            "Failed to delete from Redis cache namespace: {e}"
                        )));
                    }
                    let backoff_ms = 10 * u64::pow(5, attempt);
                    tracing::warn!(
                        key = %key,
                        prefix = %prefix,
                        error = %e,
                        attempt = attempt + 1,
                        max_retries = max_retries,
                        backoff_ms = backoff_ms,
                        cache_type = %cache_type,
                        "Redis L2 cache namespaced delete failed, retrying"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                }
                Err(L2RedisAttemptError::Timeout) => {
                    let is_last_attempt = attempt == max_retries - 1;
                    if is_last_attempt {
                        crate::metrics::cache::CACHE_ERRORS
                            .with_label_values(&[cache_type, "l2_delete"])
                            .inc();
                        tracing::error!(
                            key = %key,
                            prefix = %prefix,
                            attempts = max_retries,
                            cache_type = %cache_type,
                            "Redis L2 cache namespaced delete timed out after retries"
                        );
                        return Err(Error::Timeout(
                            "L2 cache timeout: delete from Redis cache namespace".to_string(),
                        ));
                    }
                    let backoff_ms = 10 * u64::pow(5, attempt);
                    tracing::warn!(
                        key = %key,
                        prefix = %prefix,
                        attempt = attempt + 1,
                        max_retries = max_retries,
                        backoff_ms = backoff_ms,
                        cache_type = %cache_type,
                        "Redis L2 cache namespaced delete timed out, retrying"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
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
            run_l2_redis_op("batch get from L2 cache", pipe.query_async(&mut conn)).await?;
        Ok(results)
    }

    async fn get_batch_scoped(&self, prefix: &str, keys: &[String]) -> Result<Vec<Option<String>>> {
        let mut conn = self.conn().await;
        let mut pipe = redis::pipe();
        for key in keys {
            pipe.get(key);
        }

        let results: Vec<Option<String>> =
            run_l2_redis_op("batch get from L2 cache", pipe.query_async(&mut conn)).await?;

        let missing_keys: Vec<&String> = keys
            .iter()
            .zip(results.iter())
            .filter_map(|(key, value)| value.is_none().then_some(key))
            .collect();
        if !missing_keys.is_empty() {
            let index_key = Self::namespace_index_key(prefix);
            let mut prune_pipe = redis::pipe();
            for key in missing_keys {
                prune_pipe.cmd("ZREM").arg(&index_key).arg(key).ignore();
            }
            if let Err(error) = prune_pipe.query_async::<()>(&mut conn).await {
                tracing::debug!(
                    prefix = %prefix,
                    error = %error,
                    "Failed to prune missing L2 batch keys from namespace index"
                );
            }
        }

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

        let result: i64 = run_l2_redis_op(
            "run set_if_newer Lua script",
            script
                .key(key)
                .arg(json)
                .arg(ttl_secs.cast_signed())
                .arg(new_ts_iso)
                .invoke_async(&mut conn),
        )
        .await?;

        Ok(result == 1)
    }

    async fn set_if_newer_scoped(
        &self,
        prefix: &str,
        key: &str,
        json: &str,
        ttl_secs: u64,
        new_ts_iso: &str,
    ) -> Result<bool> {
        let mut conn = self.conn().await;
        let index_key = Self::namespace_index_key(prefix);
        let expires_at = Self::expiry_timestamp(ttl_secs);
        let now = Self::now_unix_seconds();

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
            redis.call('ZADD', KEYS[2], ARGV[4], KEYS[1])
            redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', ARGV[5])
            return 1
            ",
        );

        let result: i64 = run_l2_redis_op(
            "run set_if_newer Lua script",
            script
                .key(key)
                .key(&index_key)
                .arg(json)
                .arg(ttl_secs.cast_signed())
                .arg(new_ts_iso)
                .arg(expires_at)
                .arg(now)
                .invoke_async(&mut conn),
        )
        .await?;

        Ok(result == 1)
    }

    async fn delete_by_prefix(&self, prefix: &str) -> Result<()> {
        let mut conn = self.conn().await;
        let index_key = Self::namespace_index_key(prefix);
        let keys: Vec<String> = run_l2_redis_op(
            format!("load L2 cache namespace index for prefix '{prefix}'"),
            redis::cmd("ZRANGE")
                .arg(&index_key)
                .arg(0)
                .arg(-1)
                .query_async(&mut conn),
        )
        .await?;

        for chunk in keys.chunks(256) {
            let mut pipe = redis::pipe();
            pipe.cmd("DEL");
            for key in chunk {
                pipe.arg(key);
            }
            pipe.ignore();

            run_l2_redis_op(
                format!("delete indexed L2 cache keys for prefix '{prefix}'"),
                pipe.query_async::<()>(&mut conn),
            )
            .await?;
        }

        run_l2_redis_op(
            format!("delete L2 cache namespace index for prefix '{prefix}'"),
            redis::cmd("DEL")
                .arg(&index_key)
                .query_async::<()>(&mut conn),
        )
        .await?;

        Ok(())
    }

    fn is_active(&self) -> bool {
        true
    }
}

// No-op implementation (L1-only mode)

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RedisConnectionRuntime;
    use async_trait::async_trait;
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

    #[tokio::test]
    async fn test_redis_cache_l2_accepts_trait_object_runtime() {
        #[derive(Clone)]
        struct FakeRedisRuntime;

        #[async_trait]
        impl RedisConnectionRuntime for FakeRedisRuntime {
            async fn snapshot(&self) -> redis::aio::ConnectionManager {
                panic!("snapshot should not be called in constructor-only test");
            }
        }

        let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
        let cache = RedisCacheL2::from_runtime(runtime.clone());

        assert!(
            Arc::ptr_eq(&cache.conn, &runtime),
            "L2 cache should retain the injected Redis runtime object"
        );
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
    }

    /// Short timeout for tests — just needs to prove the timeout mechanism works.
    const TEST_TIMEOUT: Duration = Duration::from_millis(200);

    #[tokio::test(start_paused = true)]
    async fn test_l2_redis_timeout_maps_to_timeout_error() {
        let timeout_future = run_l2_redis_op("get from L2 cache", async {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok::<(), redis::RedisError>(())
        });

        tokio::pin!(timeout_future);
        tokio::task::yield_now().await;
        tokio::time::advance(REDIS_OPERATION_TIMEOUT).await;

        let err = timeout_future.await.expect_err("operation should time out");
        assert!(matches!(
            err,
            Error::Timeout(ref msg) if msg == "L2 cache timeout: get from L2 cache"
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn test_l2_redis_retry_attempt_reports_timeout() {
        let timeout_future = run_l2_redis_attempt(async {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok::<(), redis::RedisError>(())
        });

        tokio::pin!(timeout_future);
        tokio::task::yield_now().await;
        tokio::time::advance(REDIS_OPERATION_TIMEOUT).await;

        let err = timeout_future
            .await
            .expect_err("retryable redis operation should time out");
        assert!(matches!(err, L2RedisAttemptError::Timeout));
    }

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
}
