//! File-based cache backend inspired by nginx's `ngx_http_file_cache`.
//!
//! # On-disk layout
//!
//! Follows nginx's 2-level directory hierarchy:
//!
//! ```text
//! cache_dir/
//!   ab/
//!     cd/
//!       abcdef0123456789...  (SHA256 hex as filename)
//!   .tmp/
//!     tmp_<unique_id>        (atomic-write staging area)
//! ```
//!
//! The first `dir_levels.0` hex chars of the key form the level-1 directory
//! name, and the next `dir_levels.1` chars form the level-2 name (mirroring
//! nginx's `levels=1:2` configuration directive).
//!
//! # File format
//!
//! Each cache file is prefixed with a binary header so that the on-disk index
//! can be rebuilt from files alone at startup (the "cache loader" pattern from
//! nginx's `ngx_http_file_cache_loader`):
//!
//! ```text
//! [4 bytes: magic "STV\x01"]
//! [4 bytes: header_len (little-endian u32)]
//! [header_len bytes: bincode-serialized FileEntryHeader]
//! [remaining bytes: cached data]
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use tokio::fs;

use super::file_format::{
    millis_since_epoch, read_cache_file, system_time_from_millis, update_file_last_accessed,
};
use super::file_index::{FileIndex, FileIndexEntry, LoadResult};
use super::file_loader;
use super::file_ops;
use super::SliceCacheBackend;
use crate::slice_cache::etag::StoredEntry;

// FileBackend

/// File-based cache backend.
///
/// Stores cached data as individual files under a 2-level directory
/// hierarchy.  An in-memory [`FileIndex`] mirrors the on-disk state
/// for fast lookups; it is rebuilt from disk on startup via
/// [`load_index`](Self::load_index).
pub struct FileBackend {
    /// Root directory for cache files.
    cache_dir: PathBuf,
    /// In-memory index of all cached entries.
    index: FileIndex,
    /// Number of hex chars for (level-1 dir, level-2 dir).
    dir_levels: (usize, usize),
    /// Monotonic counter for generating unique temp file names.
    temp_counter: AtomicU64,
}

impl FileBackend {
    const ACCESS_TIME_PERSIST_CONCURRENCY: usize = 8;

    /// Create a new file backend rooted at `cache_dir`.
    ///
    /// `dir_levels` controls the 2-level directory depth: the first
    /// element is the number of leading hex chars used for the level-1
    /// directory, and the second for level-2.  `(2, 2)` matches
    /// nginx's default `levels=1:2` (but using hex chars of our
    /// SHA256 key rather than MD5).
    ///
    /// The cache directory and `.tmp` staging directory are created if
    /// they do not already exist.
    pub async fn new(cache_dir: PathBuf, dir_levels: (usize, usize)) -> anyhow::Result<Self> {
        fs::create_dir_all(&cache_dir).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to create cache directory {}: {e}",
                cache_dir.display()
            )
        })?;
        fs::create_dir_all(cache_dir.join(".tmp"))
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create temp directory {}: {e}",
                    cache_dir.join(".tmp").display()
                )
            })?;

        Ok(Self {
            cache_dir,
            index: FileIndex::new(),
            dir_levels,
            temp_counter: AtomicU64::new(0),
        })
    }

    // Startup cache loader

    /// Load the on-disk cache index at startup.
    ///
    /// Walks `cache_dir` recursively, reads each file's header, and
    /// rebuilds the in-memory index.  Entries expired beyond
    /// `stale_max_age` past their TTL are deleted from disk (matching
    /// nginx's loader behavior of cleaning up truly stale files).
    ///
    /// Files that fail to parse (corrupted magic, bad bincode, etc.)
    /// are deleted and counted in `errors`.
    pub async fn load_index(&self, stale_max_age: Duration) -> anyhow::Result<LoadResult> {
        let result =
            file_loader::load_index(&self.cache_dir, self.dir_levels, &self.index, stale_max_age)
                .await?;

        tracing::debug!(
            loaded = result.loaded,
            deleted = result.deleted,
            errors = result.errors,
            total_bytes = result.total_bytes,
            "File cache index loaded"
        );

        Ok(result)
    }

    // Temp file cleanup

    /// Remove orphaned temp files from the `.tmp` staging directory.
    ///
    /// Temp files older than 5 minutes are considered orphaned (the
    /// write that created them either completed or failed).  This
    /// matches nginx's `ngx_http_file_cache_manage_directory` pattern
    /// of skipping `/temp` during normal eviction but cleaning it
    /// separately.
    pub async fn cleanup_temp_files(&self) {
        file_ops::cleanup_temp_files(&self.cache_dir).await;
    }

    // LRU access-time persistence (M3 fix)

    /// Persist the current in-memory `last_accessed` timestamps to disk so that
    /// LRU ordering survives restarts.
    ///
    /// Without this, `last_accessed` is only updated in the in-memory index on
    /// `get()` and the on-disk header retains the original insertion-time value.
    /// After a restart, `load_index()` reads the stale on-disk timestamp, which
    /// degrades LRU to approximate FIFO.
    ///
    /// This method should be called periodically (e.g., by the lifecycle
    /// manager) at a cadence that balances disk I/O against LRU accuracy.
    pub async fn persist_access_times(&self) {
        let dirty_entries = self
            .index
            .entries
            .iter()
            .filter_map(|entry| {
                let current_accessed = entry.last_accessed.load(Ordering::Relaxed);
                let persisted_accessed = entry.persisted_last_accessed.load(Ordering::Relaxed);
                (current_accessed != persisted_accessed)
                    .then(|| (entry.key().clone(), entry.path.clone(), current_accessed))
            })
            .collect::<Vec<_>>();

        futures::stream::iter(dirty_entries)
            .for_each_concurrent(
                Self::ACCESS_TIME_PERSIST_CONCURRENCY,
                |(key, path, current_accessed)| async move {
                    if let Err(e) = update_file_last_accessed(&path, current_accessed).await {
                        tracing::debug!(
                            path = %path.display(),
                            "Failed to persist access time: {e}"
                        );
                        return;
                    }

                    if let Some(entry) = self.index.entries.get(&key) {
                        if entry.path == path {
                            entry
                                .persisted_last_accessed
                                .fetch_max(current_accessed, Ordering::Relaxed);
                        }
                    }
                },
            )
            .await;
    }

    // Internal: remove entry from both index and disk

    /// Remove a cached entry by key (both index and disk).
    async fn remove_entry(&self, key: &str) {
        if let Some((_k, entry)) = self.index.remove(key) {
            if let Err(e) = fs::remove_file(&entry.path).await {
                // File might already be gone -- that's fine.
                tracing::debug!(
                    key,
                    path = %entry.path.display(),
                    "Failed to remove cache file (may already be deleted): {e}"
                );
            }
        }
    }
}

// SliceCacheBackend trait implementation

#[async_trait]
impl SliceCacheBackend for FileBackend {
    async fn get(&self, key: &str) -> Option<StoredEntry> {
        let entry_ref = self.index.entries.get(key)?;

        let path = entry_ref.path.clone();
        let inserted_at_millis = entry_ref.inserted_at_millis;
        let ttl_secs = entry_ref.ttl_secs;

        // Update last_accessed lazily (in-memory only, like nginx).
        let now_millis = millis_since_epoch();
        entry_ref.last_accessed.store(now_millis, Ordering::Relaxed);
        drop(entry_ref);

        match read_cache_file(&path).await {
            Ok((_header, data)) => {
                let Some(inserted_at) = system_time_from_millis(inserted_at_millis) else {
                    tracing::warn!(
                        key,
                        path = %path.display(),
                        inserted_at_millis,
                        "Invalid cache timestamp, removing from index"
                    );
                    self.remove_entry(key).await;
                    return None;
                };
                let ttl = Duration::from_secs(ttl_secs);
                let Some(last_accessed) = system_time_from_millis(now_millis) else {
                    tracing::warn!(
                        key,
                        path = %path.display(),
                        now_millis,
                        "Invalid current cache timestamp, removing from index"
                    );
                    self.remove_entry(key).await;
                    return None;
                };

                Some(StoredEntry {
                    data,
                    inserted_at,
                    ttl,
                    last_accessed,
                })
            }
            Err(e) => {
                tracing::warn!(
                    key,
                    path = %path.display(),
                    "Corrupted cache file, removing from index: {e}"
                );
                // Remove corrupted entry from both index and disk.
                self.remove_entry(key).await;
                None
            }
        }
    }

    async fn put(&self, key: &str, entry: StoredEntry) -> anyhow::Result<()> {
        let written = file_ops::write_entry(
            &self.cache_dir,
            self.dir_levels,
            &self.temp_counter,
            key,
            &entry,
        )
        .await?;

        self.index.insert(
            key.to_string(),
            FileIndexEntry {
                path: written.path,
                data_size: written.data_size,
                inserted_at_millis: written.inserted_at_millis,
                ttl_secs: written.ttl_secs,
                last_accessed: AtomicU64::new(written.last_accessed_millis),
                persisted_last_accessed: AtomicU64::new(written.last_accessed_millis),
            },
        );

        Ok(())
    }

    async fn remove(&self, key: &str) {
        self.remove_entry(key).await;
    }

    fn current_size(&self) -> u64 {
        self.index.total_size()
    }

    fn entry_count(&self) -> u64 {
        self.index.entries.len() as u64
    }

    async fn evict_to_size(&self, target_bytes: u64) -> u64 {
        if self.current_size() <= target_bytes {
            return 0;
        }

        let candidates = file_ops::lru_candidates(&self.index);
        let mut freed = 0u64;
        for candidate in candidates {
            if self.current_size() <= target_bytes {
                break;
            }
            self.remove_entry(&candidate.key).await;
            freed += candidate.data_size;
        }
        freed
    }

    async fn evict_expired(&self) -> u64 {
        let expired_keys = file_ops::expired_keys(&self.index);
        let count = expired_keys.len() as u64;
        for key in expired_keys {
            self.remove_entry(&key).await;
        }
        count
    }

    async fn keys(&self) -> Vec<String> {
        self.index.entries.iter().map(|r| r.key().clone()).collect()
    }
}

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
