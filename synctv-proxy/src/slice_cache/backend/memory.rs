//! Memory-based cache backend using moka.
//!
//! Extracted from the original moka-backed `SliceCache` to serve as the
//! default in-memory backend behind the [`SliceCacheBackend`] trait.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use moka::ops::compute::{CompResult, Op};
use rayon::prelude::*;

use super::SliceCacheBackend;
use crate::slice_cache::etag::StoredEntry;

struct MemoryEntry {
    value: StoredEntry,
    last_accessed: AtomicU64,
}

/// In-memory slice storage. Each value carries its own LRU timestamp, and Moka
/// accounts for the byte weight of admitted entries during maintenance.
pub struct MemoryBackend {
    cache: moka::future::Cache<String, Arc<MemoryEntry>>,
    started: Instant,
}

impl MemoryBackend {
    pub(super) async fn evict_expired_with_grace(&self, grace: Duration) -> u64 {
        let candidates: Vec<_> = self
            .cache
            .iter()
            .filter(|(_, entry)| entry.value.is_expired_after(grace))
            .map(|(key, _)| key)
            .collect();
        let mut removed = 0;
        for key in candidates {
            if self
                .remove_if(&key, |entry| entry.value.is_expired_after(grace))
                .await
                .is_some()
            {
                removed += 1;
            }
        }
        removed
    }

    pub(super) fn keys_with_prefix(&self, prefix: &str) -> Vec<String> {
        self.cache
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, _)| key.as_ref().clone())
            .collect()
    }

    /// Create a new memory backend.
    ///
    /// - `max_capacity`: maximum cache size in bytes (moka's weighted capacity).
    /// - `time_to_idle`: hard upper bound TTL for moka's internal eviction.
    #[must_use]
    pub fn new(max_capacity: u64, time_to_idle: Duration) -> Self {
        let cache = moka::future::Cache::builder()
            .max_capacity(max_capacity)
            .weigher(|_: &String, entry: &Arc<MemoryEntry>| {
                u32::try_from(entry.value.data_size()).unwrap_or(u32::MAX)
            })
            .time_to_idle(time_to_idle)
            .build();

        Self {
            cache,
            started: Instant::now(),
        }
    }

    fn access_time(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    async fn remove_if(
        &self,
        key: &str,
        should_remove: impl FnOnce(&Arc<MemoryEntry>) -> bool + Send,
    ) -> Option<u64> {
        let result = self
            .cache
            .entry_by_ref(key)
            .and_compute_with(|entry| async move {
                if entry
                    .as_ref()
                    .is_some_and(|entry| should_remove(entry.value()))
                {
                    Op::Remove
                } else {
                    Op::Nop
                }
            })
            .await;
        if let CompResult::Removed(entry) = result {
            self.cache.run_pending_tasks().await;
            Some(entry.value().value.data_size())
        } else {
            None
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
        let entry = self.cache.get(key).await?;
        entry
            .last_accessed
            .fetch_max(self.access_time(), Ordering::Relaxed);
        Some(entry.value.clone())
    }

    async fn put(&self, key: &str, value: StoredEntry) -> anyhow::Result<()> {
        // Moka's weight is a u32. A larger single body can still be served,
        // but admitting it would undercount memory and defeat the byte limit.
        if value.data_size() > u64::from(u32::MAX) {
            self.remove(key).await;
            return Ok(());
        }
        let entry = Arc::new(MemoryEntry {
            value,
            last_accessed: AtomicU64::new(self.access_time()),
        });
        self.cache
            .entry_by_ref(key)
            .and_compute_with(|_| async move { Op::Put(entry) })
            .await;
        self.cache.run_pending_tasks().await;
        Ok(())
    }

    async fn remove(&self, key: &str) {
        self.remove_if(key, |_| true).await;
    }

    fn current_size(&self) -> u64 {
        self.cache.weighted_size()
    }

    fn entry_count(&self) -> u64 {
        self.cache.entry_count()
    }

    async fn evict_to_size(&self, target_bytes: u64) -> u64 {
        if self.current_size() <= target_bytes {
            return 0;
        }

        let mut candidates: Vec<_> = self
            .cache
            .iter()
            .map(|(key, entry)| {
                (
                    key,
                    entry.last_accessed.load(Ordering::Relaxed),
                    Arc::downgrade(&entry),
                )
            })
            .collect();
        candidates.par_sort_unstable_by_key(|(_, accessed, _)| *accessed);

        let mut freed = 0;
        for (key, accessed, candidate) in candidates {
            if self.current_size() <= target_bytes {
                break;
            }
            freed += self
                .remove_if(&key, |entry| {
                    std::ptr::eq(candidate.as_ptr(), Arc::as_ptr(entry))
                        && entry.last_accessed.load(Ordering::Relaxed) == accessed
                })
                .await
                .unwrap_or(0);
        }
        freed
    }

    async fn evict_expired(&self) -> u64 {
        self.evict_expired_with_grace(Duration::ZERO).await
    }

    async fn keys(&self) -> Vec<String> {
        self.keys_with_prefix("")
    }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
