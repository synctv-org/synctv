//! Generic two-tier cache (L1: Moka in-memory, L2: pluggable backend)
//!
//! Provides a reusable `TieredCache<K, V>` that encapsulates the L1+L2 caching
//! pattern shared by `UserCache`, `RoomCache`, and any future entity caches.
//!
//! Integrates `SingleFlight` to prevent cache stampede: when multiple concurrent
//! requests miss L1 and L2 simultaneously, only one request fetches from L2,
//! and the others wait for its result.

use serde::{de::DeserializeOwned, Serialize};
use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::cache::l2_backend::CacheL2Backend;
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
/// This is needed for L2 key construction and for cross-replica
/// invalidation (which passes entity IDs as plain strings).
pub trait CacheKey: Hash + Eq + Clone + Debug + Display + Send + Sync + 'static {
    fn as_str(&self) -> &str;
    fn from_id(id: &str) -> Self;
}

/// Generic two-tier cache with L1 (Moka in-memory) and L2 (pluggable backend).
///
/// Integrates `SingleFlight` to prevent cache stampede on L2 misses:
/// when many concurrent requests miss L1 for the same key, only one
/// proceeds to query L2, and the rest wait for its result.
///
/// # Generation Counter/// Each invalidation increments a global `epoch` counter for this cache
/// instance. When a `SingleFlight` worker finishes a fetch, it compares the
/// epoch at the time the fetch started with the current epoch. If they differ,
/// an invalidation arrived while the fetch was in-flight, which means the
/// fetched value may be stale. In that case the result is NOT written to L1
/// cache (we let the next request re-fetch from L2/DB instead).
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
    l2: Arc<dyn CacheL2Backend>,
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
    /// `SingleFlight` for batch L2 fetches. Keyed on a stable string derived
    /// from the sorted set of cache keys being fetched. Deduplicates concurrent
    /// `get_batch()` calls that have the same missing-key set, preventing the
    /// thundering-herd problem when many requests simultaneously miss L1 and L2
    /// for the same batch of keys.
    batch_singleflight: SingleFlight<String, Vec<Option<String>>, String>,
    /// Per-key generation counters. Incremented on single-key invalidation.
    ///
    /// Before writing a `SingleFlight` result back to L1, we compare
    /// the epoch snapshot taken at fetch-start with the current value. If they
    /// differ, the fetch result is discarded to avoid re-populating L1 with
    /// potentially stale data after an invalidation.
    ///
    /// Using per-key epochs prevents cross-key eviction storms: invalidating
    /// key X only affects in-flight fetches for key X, not unrelated key Y.
    key_epochs: Arc<DashMap<K, u64>>,
    /// Global epoch counter for full-cache invalidation (`clear()`).
    /// Incremented when ALL entries are invalidated, not on per-key invalidation.
    global_epoch: Arc<AtomicU64>,
    /// Maximum number of tracked key epochs before cleanup.
    /// Prevents unbounded memory growth when many distinct keys are invalidated.
    max_epoch_entries: usize,
}

impl<K, V> TieredCache<K, V>
where
    K: CacheKey,
    V: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// Create a new `TieredCache`.
    ///
    /// # Arguments
    /// * `l2` - L2 cache backend (e.g. `RedisCacheL2` or `NoopCacheL2`)
    /// * `l1_max_capacity` - Maximum number of entries in L1 cache
    /// * `l1_ttl_seconds` - TTL for L1 cache entries in seconds
    /// * `l2_ttl_seconds` - TTL for L2 cache entries in seconds
    /// * `key_prefix` - L2 key prefix (e.g., "synctv:user:")
    /// * `cache_type` - Label for metrics (e.g., "user", "room")
    ///
    /// Minimum L2 TTL in seconds. Prevents persistent keys in L2 from
    /// unbounded memory growth when `l2_ttl_seconds` is misconfigured as 0.
    const MIN_L2_TTL_SECONDS: u64 = 60;

    pub fn new(
        l2: Arc<dyn CacheL2Backend>,
        l1_max_capacity: u64,
        l1_ttl_seconds: u64,
        l2_ttl_seconds: u64,
        key_prefix: String,
        cache_type: String,
    ) -> Result<Self> {
        // Enforce minimum L2 TTL when L2 is active to prevent
        // persistent keys that cause unbounded memory growth.
        let l2_ttl_seconds = if l2.is_active() && l2_ttl_seconds < Self::MIN_L2_TTL_SECONDS {
            tracing::warn!(
                cache_type = %cache_type,
                configured_ttl = l2_ttl_seconds,
                enforced_ttl = Self::MIN_L2_TTL_SECONDS,
                "L2 TTL too low, enforcing minimum to prevent unbounded L2 memory growth"
            );
            Self::MIN_L2_TTL_SECONDS
        } else {
            l2_ttl_seconds
        };

        let l1_cache = moka::future::CacheBuilder::new(l1_max_capacity)
            .time_to_live(std::time::Duration::from_secs(l1_ttl_seconds))
            .build();

        Ok(Self {
            l2,
            l1_cache: Arc::new(l1_cache),
            l2_ttl_seconds,
            key_prefix,
            cache_type,
            singleflight: SingleFlight::new(),
            batch_singleflight: SingleFlight::new(),
            key_epochs: Arc::new(DashMap::new()),
            global_epoch: Arc::new(AtomicU64::new(0)),
            max_epoch_entries: usize::try_from(l1_max_capacity)
                .unwrap_or(usize::MAX / 2)
                .saturating_mul(2),
        })
    }

    /// Get a value from cache.
    ///
    /// Checks L1 first, then L2. Returns None if not found in either cache.
    ///
    /// Uses `SingleFlight` for L2 lookups to prevent cache stampede: when
    /// multiple concurrent requests miss L1 for the same key, only one
    /// proceeds to query L2, and the rest wait for its result.
    pub async fn get(&self, key: &K) -> Result<Option<V>> {
        let start = std::time::Instant::now();

        // Check L1 (in-memory) cache first
        if let Some(value) = self.l1_cache.get(key).await {
            let l1 = String::from("l1");
            crate::metrics::cache::CACHE_HITS
                .with_label_values(&[&self.cache_type, &l1])
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

        // Check L2 cache via SingleFlight to prevent stampede
        if self.l2.is_active() {
            let sf_key = key.as_str().to_string();
            let l2 = self.l2.clone();
            let l2_prefix = self.key_prefix.clone();
            let redis_key = format!("{}{}", self.key_prefix, key.as_str());
            let cache_type = self.cache_type.clone();

            // Snapshot per-key epoch + global epoch before the async
            // fetch begins. After the fetch completes we re-check; if either
            // changed, an invalidation arrived mid-flight and the result is stale.
            let key_epoch_before = self.key_epochs.get(key).map_or(0, |v| *v);
            let global_epoch_before = self.global_epoch.load(Ordering::Acquire);
            let key_epochs_arc = self.key_epochs.clone();
            let global_epoch_arc = self.global_epoch.clone();
            let epoch_key = key.clone();

            let result = self
                .singleflight
                .do_work(sf_key, {
                    async move {
                        let json = l2
                            .get_scoped(&l2_prefix, &redis_key)
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
                })
                .await
                .map_err(|error| match error {
                    super::SingleFlightError::WorkerFailed => Error::Internal(
                        "SingleFlight worker failed during L2 cache fetch".to_string(),
                    ),
                    super::SingleFlightError::Inner(message) => Error::Internal(message),
                })?;

            if let Some(ref value) = result {
                let l2 = String::from("l2");
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&[&self.cache_type, &l2])
                    .inc();
                tracing::debug!(
                    key = %key,
                    cache_type = %self.cache_type,
                    "Cache hit (L2)"
                );

                // Only populate L1 if no invalidation arrived while
                // this fetch was in-flight. Check both the per-key epoch and the
                // global epoch; if either changed, the data may be stale.
                let key_epoch_after = key_epochs_arc.get(&epoch_key).map_or(0, |v| *v);
                let global_epoch_after = global_epoch_arc.load(Ordering::Acquire);
                if key_epoch_after == key_epoch_before && global_epoch_after == global_epoch_before
                {
                    self.l1_cache.insert(key.clone(), value.clone()).await;
                } else {
                    tracing::debug!(
                        key = %key,
                        cache_type = %self.cache_type,
                        key_epoch_before,
                        key_epoch_after,
                        global_epoch_before,
                        global_epoch_after,
                        "Skipping L1 write: invalidation arrived mid-flight (epoch changed)"
                    );
                }

                crate::metrics::cache::CACHE_OPERATION_DURATION
                    .with_label_values(&["get"])
                    .observe(start.elapsed().as_secs_f64());
                return Ok(result);
            }
        }

        let l1_l2 = String::from("l1_l2");
        crate::metrics::cache::CACHE_MISSES
            .with_label_values(&[&self.cache_type, &l1_l2])
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
        if self.l2.is_active() {
            let redis_key = format!("{}{}", self.key_prefix, key.as_str());
            let json = serde_json::to_string(&value).map_err(|e| {
                Error::Internal(format!(
                    "Failed to serialize {} for caching: {e}",
                    self.cache_type
                ))
            })?;

            // Add TTL jitter to prevent cache avalanche (+-10% random jitter).
            // l2_ttl_seconds is guaranteed >= MIN_L2_TTL_SECONDS by the constructor,
            // so ttl_with_jitter is always > 0. We use max() as defense-in-depth.
            let ttl_with_jitter = add_ttl_jitter(self.l2_ttl_seconds).max(Self::MIN_L2_TTL_SECONDS);

            self.l2
                .set_scoped(&self.key_prefix, &redis_key, &json, ttl_with_jitter)
                .await?;

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
    /// stale L2 data.
    ///
    /// Also increments the epoch counter so that any in-flight
    /// `SingleFlight` fetches know not to write their result to L1 cache.
    pub async fn invalidate(&self, key: &K) -> Result<()> {
        let start = std::time::Instant::now();

        // Increment per-key epoch BEFORE removing from L1. Any
        // concurrent SingleFlight fetch for THIS key that started before this
        // point will see the new epoch after completing and skip the L1 write.
        // This does NOT affect in-flight fetches for unrelated keys.
        self.key_epochs
            .entry(key.clone())
            .and_modify(|v| *v += 1)
            .or_insert(1);
        self.maybe_cleanup_epochs();

        // Remove from L1 (in-memory) FIRST so this replica stops serving stale data immediately
        self.l1_cache.invalidate(key).await;

        // Then remove from L2 with retry logic
        if self.l2.is_active() {
            let redis_key = format!("{}{}", self.key_prefix, key.as_str());
            self.l2
                .delete_with_retry_scoped(&self.key_prefix, &redis_key, 3, &self.cache_type)
                .await?;
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
    /// entry from the local in-memory cache and L2 cache.
    /// L1 is cleared first so this replica stops serving stale data immediately,
    /// then L2 is cleared so other replicas don't re-populate from stale L2 data.
    ///
    /// Also increments the epoch counter.
    pub async fn invalidate_by_id(&self, id: &str) {
        // Increment per-key epoch before evicting from L1
        let key = K::from_id(id);
        self.key_epochs
            .entry(key.clone())
            .and_modify(|v| *v += 1)
            .or_insert(1);
        self.maybe_cleanup_epochs();
        self.l1_cache.invalidate(&key).await;

        // Then remove from L2 with retry
        if self.l2.is_active() {
            let redis_key = format!("{}{}", self.key_prefix, id);
            // Use best-effort retry for cross-replica invalidation
            // Don't panic if L2 is temporarily unavailable
            if let Err(e) = self
                .l2
                .delete_with_retry_scoped(&self.key_prefix, &redis_key, 2, &self.cache_type)
                .await
            {
                let cross_replica = String::from("cross_replica_invalidate");
                crate::metrics::cache::CACHE_ERRORS
                    .with_label_values(&[&self.cache_type, &cross_replica])
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
    /// - L2: Uses backend batch operation (e.g. Redis pipeline for single round-trip)
    ///
    /// Uses `SingleFlight` to prevent cache stampede: when many concurrent
    /// requests have the same set of L2 misses, only one proceeds to query L2
    /// while the others wait for its result.
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
        if !missing_keys.is_empty() && self.l2.is_active() {
            // Snapshot global epoch before the batch fetch begins.
            // After the fetch completes we re-check; if the global epoch changed
            // (full cache clear), all results are stale.
            let global_epoch_before = self.global_epoch.load(Ordering::Acquire);
            let global_epoch_arc = self.global_epoch.clone();
            // Also snapshot per-key epochs for each missing key.
            let per_key_epochs_before: Vec<u64> = missing_keys
                .iter()
                .map(|k| self.key_epochs.get(k).map_or(0, |v| *v))
                .collect();
            let key_epochs_arc = self.key_epochs.clone();
            let missing_keys_for_epoch = missing_keys.clone();

            // Build a stable singleflight key from the sorted missing key IDs.
            // Sorting ensures that {"a","b"} and {"b","a"} resolve to the same
            // in-flight request, deduplicating concurrent batch stampedes.
            let mut sf_key_parts: Vec<&str> = missing_keys.iter().map(CacheKey::as_str).collect();
            sf_key_parts.sort_unstable();
            let sf_key = format!("batch:{}:{}", self.cache_type, sf_key_parts.join(","));

            let full_keys: Vec<String> = missing_keys
                .iter()
                .map(|k| format!("{}{}", self.key_prefix, k.as_str()))
                .collect();
            let l2 = self.l2.clone();
            let l2_prefix = self.key_prefix.clone();
            let cache_type = self.cache_type.clone();

            let jsons: Vec<Option<String>> = self
                .batch_singleflight
                .do_work(sf_key, async move {
                    l2.get_batch_scoped(&l2_prefix, &full_keys)
                        .await
                        .map_err(|e| format!("Failed to batch get {cache_type} from L2: {e}"))
                })
                .await
                .map_err(|error| match error {
                    super::SingleFlightError::WorkerFailed => Error::Internal(
                        "SingleFlight worker failed during L2 batch cache fetch".to_string(),
                    ),
                    super::SingleFlightError::Inner(message) => Error::Internal(message),
                })?;

            // Check global epoch first. If it changed, all results
            // are stale — skip all L1 writes.
            let global_epoch_after = global_epoch_arc.load(Ordering::Acquire);
            let global_epoch_changed = global_epoch_after != global_epoch_before;

            if global_epoch_changed {
                tracing::debug!(
                    cache_type = %self.cache_type,
                    global_epoch_before,
                    global_epoch_after,
                    "Skipping all L1 writes in get_batch: global invalidation arrived mid-flight"
                );
            }

            // Update result (always) and L1 cache (only if no invalidation for that key)
            for (i, (key, json_opt)) in missing_keys.iter().zip(jsons).enumerate() {
                if let Some(json) = json_opt {
                    if let Ok(value) = serde_json::from_str::<V>(&json) {
                        result.insert(key.clone(), value.clone());
                        if !global_epoch_changed {
                            // Per-key epoch check: only skip L1 for keys that were
                            // specifically invalidated during the batch fetch.
                            let key_epoch_after = key_epochs_arc
                                .get(&missing_keys_for_epoch[i])
                                .map_or(0, |v| *v);
                            if key_epoch_after == per_key_epochs_before[i] {
                                self.l1_cache.insert(key.clone(), value).await;
                            } else {
                                tracing::debug!(
                                    key = %key,
                                    cache_type = %self.cache_type,
                                    "Skipping L1 write for key in get_batch: per-key epoch changed"
                                );
                            }
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
    pub fn clear_l1(&self) {
        self.l1_cache.invalidate_all();
        tracing::debug!(cache_type = %self.cache_type, "L1 cache cleared");
    }

    /// Prevent unbounded growth of the per-key epoch map.
    ///
    /// When the map exceeds `max_epoch_entries`, we clear the entire map.
    /// This is safe because clearing epochs only means that a concurrent
    /// in-flight fetch MAY populate L1 with slightly stale data — the same
    /// risk that existed before per-key epochs were introduced. The L1 TTL
    /// will evict such entries shortly.
    fn maybe_cleanup_epochs(&self) {
        if self.key_epochs.len() > self.max_epoch_entries {
            tracing::debug!(
                cache_type = %self.cache_type,
                entries = self.key_epochs.len(),
                max = self.max_epoch_entries,
                "Per-key epoch map exceeded limit, clearing to bound memory"
            );
            self.key_epochs.clear();
        }
    }

    #[cfg(test)]
    pub(crate) fn l1_ttl(&self) -> Option<std::time::Duration> {
        self.l1_cache.policy().time_to_live()
    }

    /// Clear both L1 (in-memory) and L2 (Redis) caches for this cache type.
    ///
    /// Used during lag-triggered full flushes so that stale L2 entries cannot
    /// re-populate L1 on other replicas after the flush. L1 is cleared first.
    ///
    /// The epoch counter is incremented so that any in-flight `SingleFlight`
    /// fetches that started before this call know not to write their result
    /// back to L1.
    pub async fn clear(&self) {
        // Increment global epoch so ALL in-flight fetches discard
        // their results. Also clear per-key epochs since L1 is being wiped.
        self.global_epoch.fetch_add(1, Ordering::Release);
        self.key_epochs.clear();

        // Clear L1 first so this replica immediately stops serving stale data.
        self.l1_cache.invalidate_all();

        // Then remove all L2 entries for this cache's key prefix.
        if self.l2.is_active() {
            if let Err(e) = self.l2.delete_by_prefix(&self.key_prefix).await {
                tracing::error!(
                    cache_type = %self.cache_type,
                    prefix = %self.key_prefix,
                    error = %e,
                    "Failed to delete L2 entries by prefix during cache flush"
                );
            }
        }

        tracing::debug!(cache_type = %self.cache_type, "L1 and L2 caches cleared");
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
    /// Uses the L2 backend's atomic set-if-newer operation (e.g. Redis Lua script)
    /// to prevent TOCTOU races where concurrent updates could overwrite newer data.
    /// L1 is always updated after a successful L2 write (or when L2 is inactive).
    pub async fn set_if_newer(&self, key: &K, value: V) -> Result<bool> {
        let new_ts = value.updated_at().timestamp_millis();

        // When L2 is active, use atomic set-if-newer
        if self.l2.is_active() {
            let redis_key = format!("{}{}", self.key_prefix, key.as_str());

            let new_json = serde_json::to_string(&value).map_err(|e| {
                Error::Internal(format!(
                    "Failed to serialize {} for caching: {e}",
                    self.cache_type
                ))
            })?;

            // l2_ttl_seconds is guaranteed >= MIN_L2_TTL_SECONDS by the constructor.
            let ttl_seconds = add_ttl_jitter(self.l2_ttl_seconds).max(Self::MIN_L2_TTL_SECONDS);

            // Pass the new updated_at as ISO-8601 string for L2-side comparison.
            let new_ts_iso = value.updated_at().to_rfc3339();

            let was_set = self
                .l2
                .set_if_newer_scoped(
                    &self.key_prefix,
                    &redis_key,
                    &new_json,
                    ttl_seconds,
                    &new_ts_iso,
                )
                .await?;

            if !was_set {
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

        // No active L2: fall back to L1-only check (single-process, no TOCTOU issue)
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
            .field("l2_active", &self.l2.is_active())
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

    let jitter_range = ttl_seconds / 10; // +-10%
    if jitter_range == 0 {
        return ttl_seconds;
    }

    let jitter = rand::rng().random_range(0..=(jitter_range * 2));

    ttl_seconds
        .saturating_sub(jitter_range)
        .saturating_add(jitter)
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
        TieredCache::new(
            Arc::new(crate::cache::l2_backend::NoopCacheL2),
            100,
            5,
            0,
            "test:".to_string(),
            "test".to_string(),
        )
        .unwrap()
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

        let result = cache
            .get_batch(&[k1.clone(), k2.clone(), k3.clone()])
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result.get(&k1).map(|v| &v.name), Some(&"alice".to_string()));
        assert_eq!(result.get(&k2), None);
        assert_eq!(
            result.get(&k3).map(|v| &v.name),
            Some(&"charlie".to_string())
        );
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

        cache.clear_l1();

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
        assert!(
            (90..=110).contains(&result),
            "TTL jitter out of range: {result}"
        );
    }
}
