//! Memory-based cache backend using moka.
//!
//! Extracted from the original moka-backed `SliceCache` to serve as the
//! default in-memory backend behind the [`SliceCacheBackend`] trait.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rayon::prelude::*;

use super::SliceCacheBackend;
use crate::slice_cache::etag::StoredEntry;

/// In-memory cache backend backed by [`moka::future::Cache`].
///
/// An [`AtomicU64`] tracks the approximate total data bytes stored.  Both
/// explicit removals and moka-internal evictions keep this counter accurate
/// via the eviction listener.
pub struct MemoryBackend {
    cache: moka::future::Cache<String, StoredEntry>,
    /// Approximate total data bytes stored.
    ///
    /// Wrapped in `Arc` so the moka eviction listener can decrement it when
    /// entries are automatically evicted.
    total_bytes: Arc<AtomicU64>,
    /// Per-key last-access timestamps for true LRU eviction ordering.
    /// Updated on every `get()` so `evict_to_size` sorts by actual recency
    /// instead of insertion time (which would be FIFO). Cleaned up by the
    /// eviction listener when entries are removed.
    access_times: Arc<dashmap::DashMap<String, std::time::SystemTime>>,
}

impl MemoryBackend {
    /// Create a new memory backend.
    ///
    /// - `max_capacity`: maximum cache size in bytes (moka's weighted capacity).
    /// - `time_to_idle`: hard upper bound TTL for moka's internal eviction.
    #[must_use]
    pub fn new(max_capacity: u64, time_to_idle: Duration) -> Self {
        let total_bytes = Arc::new(AtomicU64::new(0));
        let access_times: Arc<dashmap::DashMap<String, std::time::SystemTime>> =
            Arc::new(dashmap::DashMap::new());

        // Clones for the eviction listener closure.
        let total_bytes_clone = Arc::clone(&total_bytes);
        let access_times_clone = Arc::clone(&access_times);

        let cache = moka::future::Cache::builder()
            .max_capacity(max_capacity)
            .weigher(|_key: &String, entry: &StoredEntry| -> u32 {
                u32::try_from(entry.data_size()).unwrap_or(u32::MAX)
            })
            .time_to_idle(time_to_idle)
            .eviction_listener(move |key: Arc<String>, value, cause| {
                // Handle ALL eviction causes for accurate size tracking.
                // Previously we only handled `was_evicted()` (Expired | Size)
                // and manually adjusted sizes in `put()` and `remove()`.  This
                // created a race: between `get(key)` and `insert(key, new)` in
                // `put()`, moka could fire a Size eviction for the same key,
                // causing a double-subtract of the old size.
                // Now the listener is the single source of truth for size
                // decrements.  `put()` only adds the new size, and `remove()`
                // delegates entirely to the listener via `run_pending_tasks`.
                let size = value.data_size();
                if total_bytes_clone
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                        Some(cur.saturating_sub(size))
                    })
                    .is_err()
                {
                    tracing::debug!(
                        size,
                        "failed to update in-memory slice cache size during eviction"
                    );
                }

                if !matches!(cause, moka::notification::RemovalCause::Replaced) {
                    access_times_clone.remove(key.as_ref());
                }
            })
            .build();

        Self {
            cache,
            total_bytes,
            access_times,
        }
    }

    /// Run pending moka maintenance tasks (makes `entry_count` accurate).
    pub async fn run_pending_tasks(&self) {
        self.cache.run_pending_tasks().await;
    }
}

#[async_trait]
impl SliceCacheBackend for MemoryBackend {
    async fn get(&self, key: &str) -> Option<StoredEntry> {
        let entry = self.cache.get(key).await;
        if entry.is_some() {
            // Record access time for true LRU eviction ordering.
            self.access_times
                .insert(key.to_string(), std::time::SystemTime::now());
        }
        entry
    }

    async fn put(&self, key: &str, entry: StoredEntry) -> anyhow::Result<()> {
        let new_size = entry.data_size();

        // Record initial access time for LRU tracking.
        self.access_times
            .insert(key.to_string(), std::time::SystemTime::now());

        // Insert into moka.  If a previous value existed, moka will fire the
        // eviction listener with `Replaced` cause, which decrements total_bytes
        // by the old size.  We only add the new size here -- no manual
        // get-then-subtract, which was racy with moka's internal Size eviction.
        self.cache.insert(key.to_string(), entry).await;

        // Ensure the `Replaced` listener fires before we add the new size,
        // so total_bytes transitions: old -> (old - old_size) -> (new_size).
        self.cache.run_pending_tasks().await;
        self.total_bytes.fetch_add(new_size, Ordering::Relaxed);

        Ok(())
    }

    async fn remove(&self, key: &str) {
        // Invalidate in moka.  The eviction listener (with `Explicit` cause)
        // will decrement total_bytes and remove the access timestamp.
        self.cache.remove(key).await;
        // Run pending tasks to ensure the eviction listener fires immediately,
        // so size tracking is accurate for callers that check current_size()
        // right after remove().
        self.cache.run_pending_tasks().await;
    }

    fn current_size(&self) -> u64 {
        self.total_bytes.load(Ordering::Relaxed)
    }

    fn entry_count(&self) -> u64 {
        self.cache.entry_count()
    }

    async fn evict_to_size(&self, target_bytes: u64) -> u64 {
        if self.current_size() <= target_bytes {
            return 0;
        }

        // Use iter() instead of get() to avoid updating moka's internal access times.
        // Use access_times DashMap for true LRU ordering (updated on every get()),
        // falling back to StoredEntry.last_accessed (insertion time) for entries
        // never read via get().
        let mut entries: Vec<(String, std::time::SystemTime, u64)> = self
            .cache
            .iter()
            .map(|(key, entry)| {
                let access_time = self
                    .access_times
                    .get(key.as_ref())
                    .map_or(entry.last_accessed, |r| *r.value());
                (key.as_ref().clone(), access_time, entry.data_size())
            })
            .collect();

        // Sort by last access time ascending (oldest first = LRU).
        entries.par_sort_by_key(|entry| entry.1);

        let mut freed = 0u64;
        for (key, _, size) in entries {
            if self.current_size() <= target_bytes {
                break;
            }
            self.remove(&key).await;
            freed += size;
        }

        freed
    }

    async fn evict_expired(&self) -> u64 {
        // Use iter() instead of get() to avoid updating moka's internal access times.
        // iter() does not update the historic popularity estimator or reset idle timers.
        let expired_keys: Vec<String> = self
            .cache
            .iter()
            .filter(|(_, entry)| entry.is_expired())
            .map(|(key, _)| key.as_ref().clone())
            .collect();

        let count = expired_keys.len() as u64;
        for key in expired_keys {
            self.remove(&key).await;
        }
        count
    }

    async fn keys(&self) -> Vec<String> {
        self.cache
            .iter()
            .map(|(key, _)| key.as_ref().clone())
            .collect()
    }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
