//! Generic two-tier cache (L1: Moka in-memory, L2: Redis)
//!
//! Provides a reusable `TieredCache<K, V>` that encapsulates the L1+L2 caching
//! pattern shared by `UserCache`, `RoomCache`, and any future entity caches.
//!
//! Integrates `SingleFlight` to prevent cache stampede: when multiple concurrent
//! requests miss L1 and L2 simultaneously, only one request fetches from L2
//! (Redis), and the others wait for its result.

use redis::AsyncCommands;
use serde::{de::DeserializeOwned, Serialize};
use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::sync::Arc;

use crate::cache::singleflight::SingleFlight;
use crate::{Error, Result};

/// Trait for cache values that support conditional updates based on freshness.
///
/// Types implementing this trait can be compared by timestamp, allowing
/// `set_if_newer` to prevent stale data from overwriting fresh data.
pub trait Timestamped {
    fn updated_at(&self) -> chrono::DateTime<chrono::Utc>;
}

/// Trait for cache keys that can be converted to/from string representations.
///
/// This is needed for Redis key construction and for cross-replica
/// invalidation (which passes entity IDs as plain strings).
pub trait CacheKey: Hash + Eq + Clone + Debug + Display + Send + Sync + 'static {
    fn as_str(&self) -> &str;
    fn from_id(id: &str) -> Self;
}

/// Generic two-tier cache with L1 (Moka in-memory) and L2 (Redis).
///
/// Integrates `SingleFlight` to prevent cache stampede on L2 misses:
/// when many concurrent requests miss L1 for the same key, only one
/// proceeds to query L2 (Redis), and the rest wait for its result.
///
/// # Type Parameters
/// - `K`: Cache key type (e.g. `UserId`, `RoomId`)
/// - `V`: Cache value type (e.g. `CachedUser`, `CachedRoom`)
#[derive(Clone)]
pub struct TieredCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    redis_conn: Option<redis::aio::ConnectionManager>,
    l1_cache: Arc<moka::future::Cache<K, V>>,
    l2_ttl_seconds: u64,
    key_prefix: String,
    /// Label used for metrics (e.g. "user", "room")
    cache_type: String,
    /// `SingleFlight` to deduplicate concurrent L2 fetches for the same key.
    /// Uses `String` as both the key and error type: `Error` does not implement
    /// `Clone` (due to `sqlx::Error`), so we use `String` for the error type
    /// and convert back to `Error::Internal` at the call site.
    singleflight: SingleFlight<String, Option<V>, String>,
}

impl<K, V> TieredCache<K, V>
where
    K: CacheKey,
    V: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// Create a new `TieredCache`.
    ///
    /// # Arguments
    /// * `redis_conn` - Optional Redis `ConnectionManager`. If None, only L1 caching is used.
    /// * `l1_max_capacity` - Maximum number of entries in L1 cache
    /// * `l1_ttl_minutes` - TTL for L1 cache entries in minutes
    /// * `l2_ttl_seconds` - TTL for L2 (Redis) cache entries in seconds
    /// * `key_prefix` - Redis key prefix (e.g., "synctv:user:")
    /// * `cache_type` - Label for metrics (e.g., "user", "room")
    /// Minimum L2 TTL in seconds. Prevents persistent keys in Redis from
    /// unbounded memory growth when `l2_ttl_seconds` is misconfigured as 0.
    const MIN_L2_TTL_SECONDS: u64 = 60;

    pub fn new(
        redis_conn: Option<redis::aio::ConnectionManager>,
        l1_max_capacity: u64,
        l1_ttl_minutes: u64,
        l2_ttl_seconds: u64,
        key_prefix: String,
        cache_type: String,
    ) -> Result<Self> {
        // Enforce minimum L2 TTL when Redis is configured to prevent
        // persistent keys that cause unbounded memory growth.
        let l2_ttl_seconds = if redis_conn.is_some() && l2_ttl_seconds < Self::MIN_L2_TTL_SECONDS {
            tracing::warn!(
                cache_type = %cache_type,
                configured_ttl = l2_ttl_seconds,
                enforced_ttl = Self::MIN_L2_TTL_SECONDS,
                "L2 TTL too low, enforcing minimum to prevent unbounded Redis memory growth"
            );
            Self::MIN_L2_TTL_SECONDS
        } else {
            l2_ttl_seconds
        };

        let l1_cache = moka::future::CacheBuilder::new(l1_max_capacity)
            .time_to_live(std::time::Duration::from_secs(l1_ttl_minutes * 60))
            .build();

        Ok(Self {
            redis_conn,
            l1_cache: Arc::new(l1_cache),
            l2_ttl_seconds,
            key_prefix,
            cache_type,
            singleflight: SingleFlight::new(),
        })
    }

    /// Get a value from cache.
    ///
    /// Checks L1 first, then L2. Returns None if not found in either cache.
    ///
    /// Uses `SingleFlight` for L2 lookups to prevent cache stampede: when
    /// multiple concurrent requests miss L1 for the same key, only one
    /// proceeds to query Redis while the rest wait for its result.
    pub async fn get(&self, key: &K) -> Result<Option<V>> {
        let start = std::time::Instant::now();

        // Check L1 (in-memory) cache first
        if let Some(value) = self.l1_cache.get(key).await {
            crate::metrics::cache::CACHE_HITS
                .with_label_values(&[&self.cache_type, "l1"])
                .inc();
            crate::metrics::cache::CACHE_OPERATION_DURATION
                .with_label_values(&["get"])
                .observe(start.elapsed().as_secs_f64());
            tracing::debug!(
                key = %key,
                cache_type = %self.cache_type,
                "Cache hit (L1)"
            );
            return Ok(Some(value));
        }

        // Check L2 (Redis) cache via SingleFlight to prevent stampede
        if self.redis_conn.is_some() {
            let sf_key = key.as_str().to_string();
            let conn = self.redis_conn.clone().unwrap();
            let key_prefix = self.key_prefix.clone();
            let key_str = key.as_str().to_string();
            let cache_type = self.cache_type.clone();

            let result = self.singleflight.do_work_with_fallback(
                sf_key,
                {
                    async move {
                        let mut conn = conn;
                        let redis_key = format!("{key_prefix}{key_str}");
                        let json: Option<String> = conn
                            .get(&redis_key)
                            .await
                            .map_err(|e| format!("Failed to get {cache_type} from cache: {e}"))?;

                        match json {
                            Some(json) => {
                                let value: V = serde_json::from_str(&json).map_err(|e| {
                                    format!("Failed to deserialize cached {cache_type}: {e}")
                                })?;
                                Ok(Some(value))
                            }
                            None => Ok(None),
                        }
                    }
                },
                || "SingleFlight worker failed during L2 cache fetch".to_string(),
            ).await.map_err(Error::Internal)?;

            if let Some(ref value) = result {
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&[&self.cache_type, "l2"])
                    .inc();
                tracing::debug!(
                    key = %key,
                    cache_type = %self.cache_type,
                    "Cache hit (L2)"
                );

                // Populate L1 cache
                self.l1_cache.insert(key.clone(), value.clone()).await;

                crate::metrics::cache::CACHE_OPERATION_DURATION
                    .with_label_values(&["get"])
                    .observe(start.elapsed().as_secs_f64());
                return Ok(result);
            }
        }

        crate::metrics::cache::CACHE_MISSES
            .with_label_values(&[&self.cache_type, "l1_l2"])
            .inc();
        crate::metrics::cache::CACHE_OPERATION_DURATION
            .with_label_values(&["get"])
            .observe(start.elapsed().as_secs_f64());
        tracing::debug!(key = %key, cache_type = %self.cache_type, "Cache miss");
        Ok(None)
    }

    /// Set a value in cache.
    ///
    /// Updates both L1 and L2 caches.
    pub async fn set(&self, key: &K, value: V) -> Result<()> {
        let start = std::time::Instant::now();

        // Update L1 cache
        self.l1_cache.insert(key.clone(), value.clone()).await;

        // Update L2 cache
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let redis_key = format!("{}{}", self.key_prefix, key.as_str());
            let json = serde_json::to_string(&value).map_err(|e| {
                Error::Internal(format!("Failed to serialize {} for caching: {e}", self.cache_type))
            })?;

            // Add TTL jitter to prevent cache avalanche (+-10% random jitter).
            // l2_ttl_seconds is guaranteed >= MIN_L2_TTL_SECONDS by the constructor,
            // so ttl_with_jitter is always > 0. We use max() as defense-in-depth.
            let ttl_with_jitter = add_ttl_jitter(self.l2_ttl_seconds).max(Self::MIN_L2_TTL_SECONDS);

            let _: () = conn
                .set_ex(&redis_key, json, ttl_with_jitter)
                .await
                .map_err(|e| Error::Internal(format!("Failed to set {} in cache: {e}", self.cache_type)))?;

            tracing::debug!(
                key = %key,
                ttl_seconds = ttl_with_jitter,
                cache_type = %self.cache_type,
                "Cached"
            );
        }

        crate::metrics::cache::CACHE_OPERATION_DURATION
            .with_label_values(&["set"])
            .observe(start.elapsed().as_secs_f64());
        Ok(())
    }

    /// Invalidate a value from cache.
    ///
    /// Removes from both L1 and L2 caches.
    /// L1 is invalidated first to ensure this replica immediately stops serving
    /// stale data, then L2 is cleared so other replicas don't re-populate from
    /// stale Redis data.
    pub async fn invalidate(&self, key: &K) -> Result<()> {
        let start = std::time::Instant::now();

        // Remove from L1 (in-memory) FIRST so this replica stops serving stale data immediately
        self.l1_cache.invalidate(key).await;

        // Then remove from L2 (Redis) with retry logic
        if self.redis_conn.is_some() {
            let redis_key = format!("{}{}", self.key_prefix, key.as_str());
            self.delete_from_redis_with_retry(&redis_key, 3).await?;
        }

        crate::metrics::cache::CACHE_EVICTIONS
            .with_label_values(&[&self.cache_type])
            .inc();
        crate::metrics::cache::CACHE_INVALIDATIONS
            .with_label_values(&[&self.cache_type])
            .inc();
        crate::metrics::cache::CACHE_OPERATION_DURATION
            .with_label_values(&["invalidate"])
            .observe(start.elapsed().as_secs_f64());
        tracing::debug!(key = %key, cache_type = %self.cache_type, "Cache invalidated (L1 then L2)");

        Ok(())
    }

    /// Invalidate a cache entry by raw ID string (both L1 and L2).
    ///
    /// Used by the cross-replica invalidation listener to remove a single
    /// entry from the local in-memory cache and L2 Redis cache.
    /// L1 is cleared first so this replica stops serving stale data immediately,
    /// then L2 is cleared so other replicas don't re-populate from stale Redis data.
    pub async fn invalidate_by_id(&self, id: &str) {
        // Remove from L1 (in-memory) FIRST
        let key = K::from_id(id);
        self.l1_cache.invalidate(&key).await;

        // Then remove from L2 (Redis) with retry
        if self.redis_conn.is_some() {
            let redis_key = format!("{}{}", self.key_prefix, id);
            // Use best-effort retry for cross-replica invalidation
            // Don't panic if Redis is temporarily unavailable
            if let Err(e) = self.delete_from_redis_with_retry(&redis_key, 2).await {
                crate::metrics::cache::CACHE_ERRORS
                    .with_label_values(&[&self.cache_type, "cross_replica_invalidate"])
                    .inc();
                tracing::error!(
                    id = %id,
                    cache_type = %self.cache_type,
                    error = %e,
                    "Failed to delete L2 cache during cross-replica invalidation after retries"
                );
            }
        }

        tracing::debug!(id = %id, cache_type = %self.cache_type, "Cache invalidated by id (cross-replica, L1 then L2)");
    }

    /// Get multiple values at once.
    ///
    /// More efficient than calling `get()` multiple times.
    /// Returns a map of key -> value.
    ///
    /// # Performance
    /// - L1 (Moka): Sequential lookup is optimal for in-memory cache (no I/O bottleneck)
    /// - L2 (Redis): Uses pipeline for true batch operation (single round-trip)
    pub async fn get_batch(&self, keys: &[K]) -> Result<std::collections::HashMap<K, V>> {
        let mut result = std::collections::HashMap::new();
        let mut missing_keys = Vec::new();

        // Check L1 cache first (sequential is optimal for in-memory operations)
        for key in keys {
            if let Some(value) = self.l1_cache.get(key).await {
                result.insert(key.clone(), value);
            } else {
                missing_keys.push(key.clone());
            }
        }

        // Check L2 cache for missing keys
        if !missing_keys.is_empty() {
            if let Some(ref conn) = self.redis_conn {
                let mut conn = conn.clone();

                let mut pipe = redis::pipe();
                for key in &missing_keys {
                    let redis_key = format!("{}{}", self.key_prefix, key.as_str());
                    pipe.get(&redis_key);
                }

                let jsons: Vec<Option<String>> = pipe
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| Error::Internal(format!("Failed to batch get {}s: {e}", self.cache_type)))?;

                // Update L1 cache and result
                for (key, json_opt) in missing_keys.iter().zip(jsons) {
                    if let Some(json) = json_opt {
                        if let Ok(value) = serde_json::from_str::<V>(&json) {
                            result.insert(key.clone(), value.clone());
                            self.l1_cache.insert(key.clone(), value).await;
                        }
                    }
                }
            }
        }

        tracing::debug!(
            total = keys.len(),
            found = result.len(),
            cache_type = %self.cache_type,
            "Batch lookup"
        );

        Ok(result)
    }

    /// Clear L1 cache (memory only).
    ///
    /// Useful for testing or manual cache clearing.
    /// Note: L2 cache is not cleared.
    pub async fn clear_l1(&self) {
        self.l1_cache.invalidate_all();
        tracing::debug!(cache_type = %self.cache_type, "L1 cache cleared");
    }

    /// Delete a key from Redis with retry logic.
    ///
    /// Attempts to delete up to `max_retries` times with exponential backoff.
    /// Returns error only if all retries fail.
    async fn delete_from_redis_with_retry(&self, key: &str, max_retries: u32) -> Result<()> {
        let Some(ref redis_conn) = self.redis_conn else {
            return Ok(());
        };

        for attempt in 0..max_retries {
            let mut conn = redis_conn.clone();

            match conn.del::<_, ()>(key).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let is_last_attempt = attempt == max_retries - 1;

                    if is_last_attempt {
                        crate::metrics::cache::CACHE_ERRORS
                            .with_label_values(&[&self.cache_type, "l2_delete"])
                            .inc();
                        tracing::error!(
                            key = %key,
                            error = %e,
                            attempts = max_retries,
                            cache_type = %self.cache_type,
                            "Failed to delete from Redis L2 cache after retries"
                        );
                        return Err(Error::Internal(format!("Failed to delete from Redis cache: {e}")));
                    } else {
                        // Retry with exponential backoff: 10ms, 50ms, 250ms
                        let backoff_ms = 10 * u64::pow(5, attempt);
                        tracing::warn!(
                            key = %key,
                            error = %e,
                            attempt = attempt + 1,
                            max_retries = max_retries,
                            backoff_ms = backoff_ms,
                            cache_type = %self.cache_type,
                            "Redis L2 cache delete failed, retrying"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    }
                }
            }
        }

        Ok(())
    }
}

/// Additional methods for `TieredCache` when the value type supports timestamp comparison.
impl<K, V> TieredCache<K, V>
where
    K: CacheKey,
    V: Clone + Serialize + DeserializeOwned + Timestamped + Send + Sync + 'static,
{
    /// Set a value in cache only if it's newer than existing data.
    ///
    /// Uses a Redis Lua script for fully atomic GET+compare+SET on L2, preventing
    /// TOCTOU races where concurrent updates could overwrite newer data.
    /// The Lua script parses the existing value's `updated_at` field directly,
    /// so no separate Rust-side GET is needed (eliminating the race window between
    /// a Rust GET and the Lua execution).
    /// L1 is always updated after a successful L2 write (or when Redis is absent).
    pub async fn set_if_newer(&self, key: &K, value: V) -> Result<bool> {
        let new_ts = value.updated_at().timestamp_millis();

        // When Redis is available, use an atomic Lua script for L2
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let redis_key = format!("{}{}", self.key_prefix, key.as_str());

            let new_json = serde_json::to_string(&value).map_err(|e| {
                Error::Internal(format!("Failed to serialize {} for caching: {e}", self.cache_type))
            })?;

            // l2_ttl_seconds is guaranteed >= MIN_L2_TTL_SECONDS by the constructor.
            let ttl_seconds = add_ttl_jitter(self.l2_ttl_seconds).max(Self::MIN_L2_TTL_SECONDS) as i64;

            // Lua script: atomically GET existing JSON, parse its updated_at inside
            // Lua via cjson, compare with the new timestamp (passed as millis),
            // and SET only if the new value is strictly newer.
            // Returns 1 if updated, 0 if skipped.
            //
            // The updated_at field is an ISO-8601 string in the JSON. We extract
            // it via cjson.decode and compare lexicographically, which is correct
            // for ISO-8601 timestamps in the same timezone (UTC).
            // ARGV[1] = new JSON value
            // ARGV[2] = TTL in seconds (always > 0)
            // ARGV[3] = new timestamp as ISO-8601 string for comparison
            let script = redis::Script::new(
                r"
                local existing = redis.call('GET', KEYS[1])
                if existing then
                    local ok, obj = pcall(cjson.decode, existing)
                    if ok and obj and obj.updated_at then
                        local existing_ts = obj.updated_at
                        local new_ts = ARGV[3]
                        -- Lexicographic comparison works for ISO-8601 UTC timestamps
                        if new_ts <= existing_ts then
                            return 0
                        end
                    end
                end
                redis.call('SET', KEYS[1], ARGV[1], 'EX', ARGV[2])
                return 1
                ",
            );

            // Pass the new updated_at as ISO-8601 string for Lua-side comparison.
            // This avoids any Rust-side GET, eliminating the TOCTOU window entirely.
            let new_ts_iso = value.updated_at().to_rfc3339();

            let result: i64 = script
                .key(&redis_key)
                .arg(&new_json)
                .arg(ttl_seconds)
                .arg(&new_ts_iso)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| {
                    Error::Internal(format!(
                        "Failed to run set_if_newer Lua script for {}: {e}",
                        self.cache_type
                    ))
                })?;

            if result == 0 {
                tracing::debug!(
                    key = %key,
                    new_ts = new_ts,
                    cache_type = %self.cache_type,
                    "Skipping cache update - L2 data is not newer (atomic check)"
                );
                return Ok(false);
            }

            // L2 updated successfully; update L1 as well
            self.l1_cache.insert(key.clone(), value).await;
            return Ok(true);
        }

        // No Redis: fall back to L1-only check (single-process, no TOCTOU issue)
        if let Some(existing) = self.l1_cache.get(key).await {
            if new_ts <= existing.updated_at().timestamp_millis() {
                tracing::debug!(
                    key = %key,
                    existing_ts = %existing.updated_at(),
                    new_ts = new_ts,
                    cache_type = %self.cache_type,
                    "Skipping cache update - L1 data is not newer"
                );
                return Ok(false);
            }
        }

        self.l1_cache.insert(key.clone(), value).await;
        Ok(true)
    }
}

impl<K, V> std::fmt::Debug for TieredCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TieredCache")
            .field("cache_type", &self.cache_type)
            .field("redis_enabled", &self.redis_conn.is_some())
            .field("l2_ttl_seconds", &self.l2_ttl_seconds)
            .field("key_prefix", &self.key_prefix)
            .finish()
    }
}

/// Add random jitter to TTL to prevent cache avalanche.
///
/// Returns TTL with +-10% random jitter.
fn add_ttl_jitter(ttl_seconds: u64) -> u64 {
    use rand::RngExt;

    if ttl_seconds == 0 {
        return 0;
    }

    let jitter_range = (ttl_seconds as f64 * 0.1) as u64; // +-10%
    if jitter_range == 0 {
        return ttl_seconds;
    }

    let jitter = rand::rng().random_range(0..=(jitter_range * 2));

    ttl_seconds.saturating_sub(jitter_range).saturating_add(jitter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    /// Test key type
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct TestId(String);

    impl Display for TestId {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl CacheKey for TestId {
        fn as_str(&self) -> &str {
            &self.0
        }
        fn from_id(id: &str) -> Self {
            Self(id.to_string())
        }
    }

    /// Test value type
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct TestValue {
        name: String,
        updated_at: chrono::DateTime<chrono::Utc>,
    }

    impl Timestamped for TestValue {
        fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
            self.updated_at
        }
    }

    fn make_cache() -> TieredCache<TestId, TestValue> {
        TieredCache::new(None, 100, 5, 0, "test:".to_string(), "test".to_string()).unwrap()
    }

    fn make_value(name: &str) -> TestValue {
        TestValue {
            name: name.to_string(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_l1_cache_only() {
        let cache = make_cache();

        let key = TestId("k1".to_string());
        let value = make_value("alice");

        // Cache miss
        assert!(cache.get(&key).await.unwrap().is_none());

        // Set and get
        cache.set(&key, value.clone()).await.unwrap();
        let retrieved = cache.get(&key).await.unwrap().unwrap();
        assert_eq!(retrieved.name, "alice");

        // Invalidate
        cache.invalidate(&key).await.unwrap();
        assert!(cache.get(&key).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_batch_lookup() {
        let cache = make_cache();

        let k1 = TestId("k1".to_string());
        let k2 = TestId("k2".to_string());
        let k3 = TestId("k3".to_string());

        cache.set(&k1, make_value("alice")).await.unwrap();
        cache.set(&k3, make_value("charlie")).await.unwrap();

        let result = cache.get_batch(&[k1.clone(), k2.clone(), k3.clone()]).await.unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result.get(&k1).map(|v| &v.name), Some(&"alice".to_string()));
        assert_eq!(result.get(&k2), None);
        assert_eq!(result.get(&k3).map(|v| &v.name), Some(&"charlie".to_string()));
    }

    #[tokio::test]
    async fn test_invalidate_by_id() {
        let cache = make_cache();

        let key = TestId("k1".to_string());
        cache.set(&key, make_value("alice")).await.unwrap();
        assert!(cache.get(&key).await.unwrap().is_some());

        cache.invalidate_by_id("k1").await;
        assert!(cache.get(&key).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_clear_l1() {
        let cache = make_cache();

        let k1 = TestId("k1".to_string());
        let k2 = TestId("k2".to_string());

        cache.set(&k1, make_value("alice")).await.unwrap();
        cache.set(&k2, make_value("bob")).await.unwrap();

        cache.clear_l1().await;

        assert!(cache.get(&k1).await.unwrap().is_none());
        assert!(cache.get(&k2).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_set_if_newer() {
        let cache = make_cache();

        let key = TestId("k1".to_string());
        let now = chrono::Utc::now();

        let old_value = TestValue {
            name: "old".to_string(),
            updated_at: now - chrono::Duration::seconds(10),
        };
        let new_value = TestValue {
            name: "new".to_string(),
            updated_at: now,
        };

        // Set initial value
        cache.set(&key, new_value.clone()).await.unwrap();

        // Trying to set older value should return false
        let updated = cache.set_if_newer(&key, old_value).await.unwrap();
        assert!(!updated);

        // Value should still be the new one
        let retrieved = cache.get(&key).await.unwrap().unwrap();
        assert_eq!(retrieved.name, "new");
    }

    #[test]
    fn test_add_ttl_jitter() {
        // Zero TTL should remain zero
        assert_eq!(add_ttl_jitter(0), 0);

        // Small TTL where jitter range rounds to 0
        assert_eq!(add_ttl_jitter(5), 5);

        // Normal TTL should be within +-10%
        let ttl = 100;
        let result = add_ttl_jitter(ttl);
        assert!(result >= 90 && result <= 110, "TTL jitter out of range: {result}");
    }
}
