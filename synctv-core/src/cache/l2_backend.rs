//! L2 cache backend trait for TieredCache.
//!
//! Provides pluggable L2 storage behind the L1 Moka in-memory cache.
//!
//! ## Implementations
//!
//! - `RedisCacheL2`: Redis-backed L2 with TTL, retry logic, and atomic set-if-newer.
//! - `NoopCacheL2`: No-op backend (L1-only mode). All reads return None, all writes are no-ops.

use async_trait::async_trait;
use crate::{Error, Result};

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
    async fn set_if_newer(&self, key: &str, json: &str, ttl_secs: u64, new_ts_iso: &str) -> Result<bool>;

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
pub struct RedisCacheL2 {
    conn: redis::aio::ConnectionManager,
}

impl RedisCacheL2 {
    pub fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl CacheL2Backend for RedisCacheL2 {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let json: Option<String> = conn
            .get(key)
            .await
            .map_err(|e| Error::Internal(format!("Failed to get from L2 cache: {e}")))?;
        Ok(json)
    }

    async fn set(&self, key: &str, json: &str, ttl_secs: u64) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let _: () = conn
            .set_ex(key, json, ttl_secs)
            .await
            .map_err(|e| Error::Internal(format!("Failed to set in L2 cache: {e}")))?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let _: () = conn.del(key).await
            .map_err(|e| Error::Internal(format!("Failed to delete from L2 cache: {e}")))?;
        Ok(())
    }

    async fn delete_with_retry(&self, key: &str, max_retries: u32, cache_type: &str) -> Result<()> {
        use redis::AsyncCommands;
        for attempt in 0..max_retries {
            let mut conn = self.conn.clone();
            match conn.del::<_, ()>(key).await {
                Ok(()) => return Ok(()),
                Err(e) => {
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
                        return Err(Error::Internal(format!("Failed to delete from Redis cache: {e}")));
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
            }
        }
        Ok(())
    }

    async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>> {
        let mut conn = self.conn.clone();
        let mut pipe = redis::pipe();
        for key in keys {
            pipe.get(key);
        }
        let results: Vec<Option<String>> = pipe
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Internal(format!("Failed to batch get from L2: {e}")))?;
        Ok(results)
    }

    async fn set_if_newer(&self, key: &str, json: &str, ttl_secs: u64, new_ts_iso: &str) -> Result<bool> {
        let mut conn = self.conn.clone();

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

        let result: i64 = script
            .key(key)
            .arg(json)
            .arg(ttl_secs as i64)
            .arg(new_ts_iso)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| {
                Error::Internal(format!("Failed to run set_if_newer Lua script: {e}"))
            })?;

        Ok(result == 1)
    }

    async fn delete_by_prefix(&self, prefix: &str) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();

        // Use SCAN to iterate keys matching the prefix pattern, then DEL in batches.
        // This avoids blocking Redis with KEYS * on large keyspaces.
        let pattern = format!("{prefix}*");
        let mut cursor: u64 = 0;
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100u64)
                .query_async(&mut conn)
                .await
                .map_err(|e| Error::Internal(format!("SCAN failed for prefix '{prefix}': {e}")))?;

            if !keys.is_empty() {
                let _: () = conn
                    .del(keys.as_slice())
                    .await
                    .map_err(|e| Error::Internal(format!("DEL failed for prefix '{prefix}': {e}")))?;
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
/// Used when Redis is not configured — TieredCache runs in L1-only mode.
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

    async fn delete_with_retry(&self, _key: &str, _max_retries: u32, _cache_type: &str) -> Result<()> {
        Ok(())
    }

    async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>> {
        Ok(vec![None; keys.len()])
    }

    async fn set_if_newer(&self, _key: &str, _json: &str, _ttl_secs: u64, _new_ts_iso: &str) -> Result<bool> {
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
