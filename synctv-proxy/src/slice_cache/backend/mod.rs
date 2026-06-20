//! Cache backend trait and enum dispatch.
//!
//! Inspired by nginx's `ngx_http_file_cache` shared memory zone operations
//! (`init`, `create`, `update`, `free`, `expire`).

pub mod file;
mod file_format;
mod file_index;
mod file_loader;
mod file_ops;
pub mod memory;

use async_trait::async_trait;

use super::etag::StoredEntry;

/// Core storage operations for the slice cache.
///
/// Inspired by nginx's `ngx_http_file_cache` shared memory zone operations
/// (init, create, update, free, expire).
#[async_trait]
pub trait SliceCacheBackend: Send + Sync {
    /// Retrieve an entry by key. Returns `None` if not found.
    async fn get(&self, key: &str) -> Option<StoredEntry>;

    /// Store an entry under the given key.
    async fn put(&self, key: &str, entry: StoredEntry) -> anyhow::Result<()>;

    /// Remove an entry by key.
    async fn remove(&self, key: &str);

    /// Total bytes currently stored across all entries.
    fn current_size(&self) -> u64;

    /// Number of entries currently stored.
    fn entry_count(&self) -> u64;

    /// Evict entries (LRU order) until the total stored size is at or below
    /// `target_bytes`. Returns the number of bytes freed.
    async fn evict_to_size(&self, target_bytes: u64) -> u64;

    /// Remove all expired entries. Returns the count of entries removed.
    async fn evict_expired(&self) -> u64;

    /// Return all keys currently stored.
    async fn keys(&self) -> Vec<String>;
}

/// Enum dispatch for cache backends -- zero-cost runtime selection without
/// dynamic dispatch overhead.
pub enum CacheBackend {
    /// In-memory backend.
    Memory(memory::MemoryBackend),
    /// File-based backend.
    File(file::FileBackend),
}

#[async_trait]
impl SliceCacheBackend for CacheBackend {
    async fn get(&self, key: &str) -> Option<StoredEntry> {
        match self {
            Self::Memory(b) => b.get(key).await,
            Self::File(b) => b.get(key).await,
        }
    }

    async fn put(&self, key: &str, entry: StoredEntry) -> anyhow::Result<()> {
        match self {
            Self::Memory(b) => b.put(key, entry).await,
            Self::File(b) => b.put(key, entry).await,
        }
    }

    async fn remove(&self, key: &str) {
        match self {
            Self::Memory(b) => b.remove(key).await,
            Self::File(b) => b.remove(key).await,
        }
    }

    fn current_size(&self) -> u64 {
        match self {
            Self::Memory(b) => b.current_size(),
            Self::File(b) => b.current_size(),
        }
    }

    fn entry_count(&self) -> u64 {
        match self {
            Self::Memory(b) => b.entry_count(),
            Self::File(b) => b.entry_count(),
        }
    }

    async fn evict_to_size(&self, target_bytes: u64) -> u64 {
        match self {
            Self::Memory(b) => b.evict_to_size(target_bytes).await,
            Self::File(b) => b.evict_to_size(target_bytes).await,
        }
    }

    async fn evict_expired(&self) -> u64 {
        match self {
            Self::Memory(b) => b.evict_expired().await,
            Self::File(b) => b.evict_expired().await,
        }
    }

    async fn keys(&self) -> Vec<String> {
        match self {
            Self::Memory(b) => b.keys().await,
            Self::File(b) => b.keys().await,
        }
    }
}
