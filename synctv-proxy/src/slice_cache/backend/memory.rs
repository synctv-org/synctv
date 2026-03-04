//! Memory-based cache backend using moka.
//!
//! Extracted from the original moka-backed `SliceCache` to serve as the
//! default in-memory backend behind the [`SliceCacheBackend`] trait.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashSet;

use super::SliceCacheBackend;
use crate::slice_cache::etag::StoredEntry;

/// In-memory cache backend backed by [`moka::future::Cache`].
///
/// Uses moka's built-in size-weighted eviction plus a parallel [`DashSet`]
/// for key iteration (moka doesn't natively support iteration).
///
/// An [`AtomicU64`] tracks the approximate total data bytes stored.  Both
/// explicit removals and moka-internal evictions keep this counter accurate
/// via the eviction listener.
pub struct MemoryBackend {
    cache: moka::future::Cache<String, StoredEntry>,
    /// Shadow set for key enumeration (moka doesn't support iteration).
    key_set: DashSet<String>,
    /// Approximate total data bytes stored.
    ///
    /// Wrapped in `Arc` so the moka eviction listener can decrement it when
    /// entries are automatically evicted.
    total_bytes: Arc<AtomicU64>,
}

impl MemoryBackend {
    /// Create a new memory backend.
    ///
    /// - `max_capacity`: maximum cache size in bytes (moka's weighted capacity).
    /// - `time_to_idle`: hard upper bound TTL for moka's internal eviction.
    #[must_use]
    pub fn new(max_capacity: u64, time_to_idle: Duration) -> Self {
        let key_set = DashSet::new();
        let total_bytes = Arc::new(AtomicU64::new(0));

        // Clones for the eviction listener closure.
        let key_set_clone = key_set.clone();
        let total_bytes_clone = Arc::clone(&total_bytes);

        let cache = moka::future::Cache::builder()
            .max_capacity(max_capacity)
            .weigher(|_key: &String, entry: &StoredEntry| -> u32 {
                u32::try_from(entry.data_size()).unwrap_or(u32::MAX)
            })
            .time_to_idle(time_to_idle)
            .eviction_listener(move |key: Arc<String>, value, cause| {
                // Handle ALL eviction causes for accurate size tracking.
                //
                // Previously we only handled `was_evicted()` (Expired | Size)
                // and manually adjusted sizes in `put()` and `remove()`.  This
                // created a race: between `get(key)` and `insert(key, new)` in
                // `put()`, moka could fire a Size eviction for the same key,
                // causing a double-subtract of the old size.
                //
                // Now the listener is the single source of truth for size
                // decrements.  `put()` only adds the new size, and `remove()`
                // delegates entirely to the listener via `run_pending_tasks`.
                let size = value.data_size();
                total_bytes_clone
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                        Some(cur.saturating_sub(size))
                    })
                    .ok();

                // Only remove from key_set when the key truly no longer exists
                // in the cache.  For `Replaced`, a new value was just inserted,
                // so the key is still live.
                if !matches!(cause, moka::notification::RemovalCause::Replaced) {
                    key_set_clone.remove(key.as_ref());
                }
            })
            .build();

        Self {
            cache,
            key_set,
            total_bytes,
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
        self.cache.get(key).await
    }

    async fn put(&self, key: &str, entry: StoredEntry) -> anyhow::Result<()> {
        let new_size = entry.data_size();

        // Insert into moka.  If a previous value existed, moka will fire the
        // eviction listener with `Replaced` cause, which decrements total_bytes
        // by the old size.  We only add the new size here -- no manual
        // get-then-subtract, which was racy with moka's internal Size eviction.
        self.cache.insert(key.to_string(), entry).await;
        self.key_set.insert(key.to_string());

        // Ensure the `Replaced` listener fires before we add the new size,
        // so total_bytes transitions: old -> (old - old_size) -> (new_size).
        self.cache.run_pending_tasks().await;
        self.total_bytes.fetch_add(new_size, Ordering::Relaxed);

        Ok(())
    }

    async fn remove(&self, key: &str) {
        // Invalidate in moka.  The eviction listener (with `Explicit` cause)
        // will decrement total_bytes and remove the key from key_set.
        self.cache.remove(key).await;
        // Run pending tasks to ensure the eviction listener fires immediately,
        // so size tracking is accurate for callers that check current_size()
        // right after remove().
        self.cache.run_pending_tasks().await;
    }

    async fn contains(&self, key: &str) -> bool {
        self.cache.contains_key(key)
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
        // iter() does not update the historic popularity estimator or reset idle timers.
        // Collect entries with their last_accessed times for LRU ordering.
        let mut entries: Vec<(String, std::time::SystemTime, u64)> = self
            .cache
            .iter()
            .map(|(key, entry)| {
                (
                    key.as_ref().clone(),
                    entry.last_accessed,
                    entry.data_size(),
                )
            })
            .collect();

        // Sort by last_accessed ascending (oldest first = LRU).
        entries.sort_by_key(|e| e.1);

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
        self.key_set.iter().map(|k| k.key().clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use bytes::Bytes;

    /// Helper: create a [`StoredEntry`] with the given data and TTL.
    fn make_entry(data: &[u8], ttl: Duration) -> StoredEntry {
        StoredEntry::new(Bytes::from(data.to_vec()), ttl)
    }

    /// Shorthand for a backend with generous limits.
    fn default_backend() -> MemoryBackend {
        MemoryBackend::new(64 * 1024 * 1024, Duration::from_hours(1))
    }

    #[tokio::test]
    async fn test_memory_backend_put_get() {
        let backend = default_backend();
        let entry = make_entry(b"hello world", Duration::from_mins(1));

        backend.put("k1", entry.clone()).await.unwrap();
        let got = backend.get("k1").await;
        assert!(got.is_some());
        assert_eq!(got.unwrap().data, Bytes::from_static(b"hello world"));
    }

    #[tokio::test]
    async fn test_memory_backend_get_miss() {
        let backend = default_backend();
        assert!(backend.get("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn test_memory_backend_remove() {
        let backend = default_backend();
        backend
            .put("k1", make_entry(b"data", Duration::from_mins(1)))
            .await
            .unwrap();

        backend.remove("k1").await;
        assert!(backend.get("k1").await.is_none());
    }

    #[tokio::test]
    async fn test_memory_backend_remove_nonexistent() {
        let backend = default_backend();
        // Should not panic.
        backend.remove("ghost").await;
    }

    #[tokio::test]
    async fn test_memory_backend_contains() {
        let backend = default_backend();
        assert!(!backend.contains("k1").await);

        backend
            .put("k1", make_entry(b"x", Duration::from_mins(1)))
            .await
            .unwrap();
        // moka's contains_key may be eventually consistent; run pending tasks.
        backend.run_pending_tasks().await;
        assert!(backend.contains("k1").await);
    }

    #[tokio::test]
    async fn test_memory_backend_current_size() {
        let backend = default_backend();
        assert_eq!(backend.current_size(), 0);

        backend
            .put("k1", make_entry(b"abcde", Duration::from_mins(1)))
            .await
            .unwrap();
        assert_eq!(backend.current_size(), 5);

        backend
            .put("k2", make_entry(b"12345678", Duration::from_mins(1)))
            .await
            .unwrap();
        assert_eq!(backend.current_size(), 13);

        backend.remove("k1").await;
        assert_eq!(backend.current_size(), 8);
    }

    #[tokio::test]
    async fn test_memory_backend_entry_count() {
        let backend = default_backend();
        backend.run_pending_tasks().await;
        assert_eq!(backend.entry_count(), 0);

        backend
            .put("k1", make_entry(b"a", Duration::from_mins(1)))
            .await
            .unwrap();
        backend
            .put("k2", make_entry(b"b", Duration::from_mins(1)))
            .await
            .unwrap();
        backend.run_pending_tasks().await;
        assert_eq!(backend.entry_count(), 2);
    }

    #[tokio::test]
    async fn test_memory_backend_keys() {
        let backend = default_backend();
        backend
            .put("alpha", make_entry(b"1", Duration::from_mins(1)))
            .await
            .unwrap();
        backend
            .put("beta", make_entry(b"2", Duration::from_mins(1)))
            .await
            .unwrap();

        let mut keys = backend.keys().await;
        keys.sort();
        assert_eq!(keys, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[tokio::test]
    async fn test_memory_backend_evict_expired() {
        let backend = default_backend();

        // One entry with a very short TTL.
        backend
            .put("short", make_entry(b"gone", Duration::from_millis(10)))
            .await
            .unwrap();
        // One entry with a long TTL.
        backend
            .put("long", make_entry(b"stays", Duration::from_hours(1)))
            .await
            .unwrap();

        // Wait for the short entry to expire.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let evicted = backend.evict_expired().await;
        assert_eq!(evicted, 1);

        // The long-lived entry should still be present.
        assert!(backend.get("long").await.is_some());
        // The short-lived entry should be gone.
        assert!(backend.get("short").await.is_none());
    }

    #[tokio::test]
    async fn test_memory_backend_evict_to_size() {
        let backend = default_backend();

        // Insert several entries with staggered access times.
        for i in 0..5u8 {
            let data = vec![i; 100];
            backend
                .put(
                    &format!("k{i}"),
                    make_entry(&data, Duration::from_hours(1)),
                )
                .await
                .unwrap();
            // Small sleep to ensure different `last_accessed` timestamps.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(backend.current_size(), 500);

        // Evict down to 250 bytes -- should remove the two oldest entries.
        let freed = backend.evict_to_size(250).await;
        assert!(
            freed >= 200,
            "Expected at least 200 bytes freed, got {freed}"
        );
        assert!(
            backend.current_size() <= 250,
            "Expected size <= 250, got {}",
            backend.current_size()
        );
    }

    #[tokio::test]
    async fn test_memory_backend_evict_to_size_already_under() {
        let backend = default_backend();
        backend
            .put("k1", make_entry(b"small", Duration::from_mins(1)))
            .await
            .unwrap();

        let freed = backend.evict_to_size(1_000_000).await;
        assert_eq!(freed, 0);
        assert!(backend.get("k1").await.is_some());
    }

    #[tokio::test]
    async fn test_memory_backend_replace_updates_size() {
        let backend = default_backend();

        backend
            .put("k1", make_entry(b"short", Duration::from_mins(1)))
            .await
            .unwrap();
        assert_eq!(backend.current_size(), 5);

        // Replace with a larger value.
        backend
            .put(
                "k1",
                make_entry(b"a much longer value", Duration::from_mins(1)),
            )
            .await
            .unwrap();
        assert_eq!(backend.current_size(), 19);

        // Replace with a smaller value.
        backend
            .put("k1", make_entry(b"tiny", Duration::from_mins(1)))
            .await
            .unwrap();
        assert_eq!(backend.current_size(), 4);
    }

    #[tokio::test]
    async fn test_memory_backend_eviction_listener_removes_from_key_set() {
        // Create a backend with a tiny capacity so moka will evict entries
        // automatically when capacity is exceeded.
        let backend = MemoryBackend::new(
            150, // 150 bytes max
            Duration::from_hours(1),
        );

        // Insert entries totaling more than 150 bytes to trigger eviction.
        for i in 0..5u8 {
            let data = vec![i; 50]; // 50 bytes each, 250 total > 150
            backend
                .put(
                    &format!("k{i}"),
                    make_entry(&data, Duration::from_hours(1)),
                )
                .await
                .unwrap();
        }

        // Run pending tasks to force moka's eviction processing.
        backend.run_pending_tasks().await;

        // After eviction, moka's entry_count should be less than 5.
        let entry_count = backend.entry_count();
        assert!(
            entry_count <= 3,
            "Expected moka to evict some entries, but entry_count = {entry_count}"
        );

        // The eviction listener runs asynchronously, so the key_set may
        // briefly contain stale entries. Verify that keys which remain in
        // the key_set are actually backed by live cache entries -- i.e.,
        // any key still in the set should still be gettable from moka.
        let keys = backend.keys().await;
        let mut live_count = 0u64;
        for key in &keys {
            if backend.get(key).await.is_some() {
                live_count += 1;
            }
        }
        assert_eq!(
            live_count, entry_count,
            "Live keys ({live_count}) should match moka entry_count ({entry_count})"
        );
    }

    /// Regression test for H5: replacing a key must not double-subtract the old
    /// size.  Under the old code, `put()` manually subtracted the old size AND
    /// moka's eviction listener could also subtract it (via `Size` cause during
    /// capacity pressure), leading to underflow/wrap-around in total_bytes.
    #[tokio::test]
    async fn test_memory_backend_replace_size_tracking_no_double_subtract() {
        // Use a tight capacity so that moka is under pressure.
        let backend = MemoryBackend::new(
            500, // just enough for a few entries
            Duration::from_hours(1),
        );

        // Fill up close to capacity with other entries.
        for i in 0..4u8 {
            backend
                .put(
                    &format!("filler_{i}"),
                    make_entry(&[i; 100], Duration::from_hours(1)),
                )
                .await
                .unwrap();
        }
        backend.run_pending_tasks().await;

        // Insert the target key.
        backend
            .put("target", make_entry(&[0u8; 80], Duration::from_hours(1)))
            .await
            .unwrap();
        backend.run_pending_tasks().await;
        let size_before_replace = backend.current_size();

        // Replace the target key with a same-sized value.  Under the old code,
        // if moka evicts the old "target" entry (Size cause) between the
        // manual get() and insert(), total_bytes would be double-subtracted.
        backend
            .put("target", make_entry(&[1u8; 80], Duration::from_hours(1)))
            .await
            .unwrap();
        backend.run_pending_tasks().await;

        let size_after_replace = backend.current_size();

        // The size should be the same (replaced with identical-sized entry).
        // With the old buggy code, this could underflow or be too small.
        assert_eq!(
            size_before_replace, size_after_replace,
            "Size should be unchanged after same-size replace (before={size_before_replace}, after={size_after_replace})"
        );
    }

    /// Regression test: evict_to_size should not update moka's internal access
    /// times when collecting entries for LRU ordering.
    ///
    /// The bug: the old implementation called `cache.get()` for each key during
    /// evict_to_size, which updated moka's internal access time. This meant that
    /// the first key accessed would have its timestamp refreshed, potentially
    /// causing it to be incorrectly considered "recently used" and not evicted.
    ///
    /// The fix: use `cache.iter()` which does not update access times.
    #[tokio::test]
    async fn test_memory_backend_evict_to_size_lru_ordering_not_affected_by_get() {
        let backend = default_backend();

        // Insert entries with deliberate timing to establish LRU order.
        // k0 should be evicted first (oldest), k4 last (newest).
        for i in 0..5u8 {
            let data = vec![i; 100];
            backend
                .put(
                    &format!("k{i}"),
                    make_entry(&data, Duration::from_hours(1)),
                )
                .await
                .unwrap();
            // Sleep to ensure different last_accessed timestamps.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(backend.current_size(), 500);

        // Evict down to 400 bytes - should only remove k0 (oldest).
        let freed = backend.evict_to_size(400).await;
        assert_eq!(freed, 100, "Should have freed exactly 100 bytes (k0)");
        assert_eq!(backend.current_size(), 400);

        // k0 should be gone, k1-k4 should remain.
        assert!(
            backend.get("k0").await.is_none(),
            "k0 (oldest) should have been evicted"
        );
        assert!(backend.get("k1").await.is_some(), "k1 should still exist");
        assert!(
            backend.get("k4").await.is_some(),
            "k4 (newest) should still exist"
        );
    }

    /// Additional test: evict_to_size should respect actual LRU order even when
    /// entries have been accessed in a different order after insertion.
    #[tokio::test]
    async fn test_memory_backend_evict_to_size_respects_last_accessed_order() {
        let backend = default_backend();

        // Insert three entries with delays to establish initial order:
        // k0 (oldest), k1 (middle), k2 (newest)
        backend
            .put("k0", make_entry(&[0u8; 100], Duration::from_hours(1)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        backend
            .put("k1", make_entry(&[1u8; 100], Duration::from_hours(1)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        backend
            .put("k2", make_entry(&[2u8; 100], Duration::from_hours(1)))
            .await
            .unwrap();

        assert_eq!(backend.current_size(), 300);

        // Now access k0 to make it most recently used via get()
        // This updates moka's internal TTI tracker but NOT StoredEntry.last_accessed
        let _ = backend.get("k0").await;

        // Evict down to 200 bytes.
        // Since we use StoredEntry.last_accessed for LRU ordering (not moka's internal TTI),
        // k0 should still be evicted first because its StoredEntry.last_accessed is oldest.
        // (The get() updates moka's TTI but we don't update StoredEntry.last_accessed on get)
        let freed = backend.evict_to_size(200).await;
        assert_eq!(freed, 100, "Should have freed exactly 100 bytes");
        assert_eq!(backend.current_size(), 200);

        // k1 should be evicted (it has the middle timestamp in StoredEntry.last_accessed,
        // and we're evicting from oldest to newest based on that field).
        // Actually: k0 was inserted first, then k1, then k2.
        // After the get("k0"), moka's internal timer for k0 is updated, but
        // StoredEntry.last_accessed is NOT updated (get() returns a clone).
        // So the LRU order based on StoredEntry.last_accessed is: k0 (oldest), k1, k2 (newest).
        // Evicting to 200 bytes removes k0 (100 bytes).
        assert!(
            backend.get("k0").await.is_none(),
            "k0 should have been evicted (oldest StoredEntry.last_accessed)"
        );
        assert!(backend.get("k1").await.is_some(), "k1 should still exist");
        assert!(backend.get("k2").await.is_some(), "k2 should still exist");
    }

    /// Verify that after multiple put-replace-remove cycles, total_bytes
    /// returns to zero when all entries are removed.
    #[tokio::test]
    async fn test_memory_backend_size_returns_to_zero() {
        let backend = default_backend();

        // Insert, replace, and remove several keys.
        for round in 0..3u8 {
            let key = format!("k{round}");
            backend
                .put(&key, make_entry(&[round; 50], Duration::from_mins(1)))
                .await
                .unwrap();
            // Replace with different size.
            backend
                .put(&key, make_entry(&[round; 100], Duration::from_mins(1)))
                .await
                .unwrap();
        }
        backend.run_pending_tasks().await;
        assert_eq!(backend.current_size(), 300); // 3 keys * 100 bytes

        // Remove all.
        for round in 0..3u8 {
            backend.remove(&format!("k{round}")).await;
        }
        backend.run_pending_tasks().await;

        assert_eq!(
            backend.current_size(),
            0,
            "After removing all entries, total_bytes should be 0"
        );
    }
}
