// In-memory storage backend for HLS
//
// Useful for:
// - Testing without filesystem I/O
// - Temporary caching before OSS upload
// - Short-lived streams that don't need persistence
//
// Note: Data is lost on server restart
//
// Memory Safety:
// - Configurable max memory and max key limits prevent OOM
// - When limits are reached, oldest entries are evicted (LRU-like by write time)
//
// Concurrency:
// - Uses `parking_lot::RwLock` so multiple readers proceed in parallel while
//   writers (write/delete/eviction) get exclusive access.  This eliminates the
//   previous `tokio::sync::Mutex` bottleneck where every read blocked behind
//   writes.
//
// Eviction uses a BTreeMap index keyed by sequence number for O(log N) oldest
// lookup instead of scanning all entries.

use super::HlsStorage;
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::BTreeMap;
use std::io::{Error, ErrorKind, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Default max memory: 512 MB
const DEFAULT_MAX_MEMORY_BYTES: usize = 512 * 1024 * 1024;
/// Default max keys: 10,000
const DEFAULT_MAX_KEYS: usize = 10_000;

/// Monotonic sequence number used as a total ordering for entries.
/// This avoids ties that would occur with `Instant` on fast inserts.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn next_seq() -> u64 {
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

struct Entry {
    data: Bytes,
    seq: u64,
    write_time: std::time::Instant,
}

/// In-memory storage backend with configurable memory limits.
///
/// Uses `parking_lot::RwLock` for concurrent reads (lock-free relative to each
/// other) while writers get exclusive access for consistent eviction.
#[derive(Clone)]
pub struct MemoryStorage {
    inner: std::sync::Arc<parking_lot::RwLock<MemoryStorageInner>>,
    max_memory_bytes: usize,
    max_keys: usize,
}

struct MemoryStorageInner {
    /// Primary map: key -> entry
    data: std::collections::HashMap<String, Entry>,
    /// Time-ordered index: (seq, key) for O(log N) eviction of oldest entry
    time_index: BTreeMap<u64, String>,
    /// Running total of data bytes for O(1) memory usage queries
    total_bytes: usize,
}

impl MemoryStorageInner {
    fn new() -> Self {
        Self {
            data: std::collections::HashMap::new(),
            time_index: BTreeMap::new(),
            total_bytes: 0,
        }
    }

    /// Remove a key, updating both the data map and time index.
    fn remove(&mut self, key: &str) -> bool {
        if let Some(entry) = self.data.remove(key) {
            self.total_bytes -= entry.data.len();
            self.time_index.remove(&entry.seq);
            true
        } else {
            false
        }
    }

    /// Evict the oldest entry by sequence number. Returns true if evicted.
    fn evict_oldest(&mut self) -> bool {
        // BTreeMap iteration starts at the smallest key (oldest seq)
        let oldest_seq = if let Some((&seq, _)) = self.time_index.iter().next() {
            seq
        } else {
            return false;
        };
        if let Some(key) = self.time_index.remove(&oldest_seq) {
            if let Some(entry) = self.data.remove(&key) {
                self.total_bytes -= entry.data.len();
            }
            true
        } else {
            false
        }
    }

    /// Evict entries until we're under limits for the incoming data.
    fn evict_if_needed(&mut self, incoming_bytes: usize, max_keys: usize, max_memory_bytes: usize) -> usize {
        let mut evicted = 0;

        if max_keys > 0 {
            while self.data.len() >= max_keys {
                if self.evict_oldest() {
                    evicted += 1;
                } else {
                    break;
                }
            }
        }

        if max_memory_bytes > 0 {
            while self.total_bytes + incoming_bytes > max_memory_bytes {
                if self.evict_oldest() {
                    evicted += 1;
                } else {
                    break;
                }
            }
        }

        if evicted > 0 {
            tracing::debug!(
                evicted = evicted,
                keys = self.data.len(),
                memory_bytes = self.total_bytes,
                "Evicted old entries from memory storage"
            );
        }

        evicted
    }
}

impl MemoryStorage {
    /// Create new memory storage with default limits (512 MB, 10,000 keys)
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(parking_lot::RwLock::new(MemoryStorageInner::new())),
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            max_keys: DEFAULT_MAX_KEYS,
        }
    }

    /// Create new memory storage with custom limits
    ///
    /// # Arguments
    /// * `max_memory_bytes` - Maximum memory in bytes (0 = unlimited)
    /// * `max_keys` - Maximum number of keys (0 = unlimited)
    #[must_use]
    pub fn with_limits(max_memory_bytes: usize, max_keys: usize) -> Self {
        Self {
            inner: std::sync::Arc::new(parking_lot::RwLock::new(MemoryStorageInner::new())),
            max_memory_bytes,
            max_keys,
        }
    }

    /// Create new memory storage with no limits (use with caution)
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            inner: std::sync::Arc::new(parking_lot::RwLock::new(MemoryStorageInner::new())),
            max_memory_bytes: 0,
            max_keys: 0,
        }
    }

    /// Get current memory usage in bytes
    pub async fn memory_usage(&self) -> usize {
        self.inner.read().total_bytes
    }

    /// Get number of stored keys
    pub async fn key_count(&self) -> usize {
        self.inner.read().data.len()
    }

    /// Clear all data (for testing/cleanup)
    pub async fn clear(&self) {
        let mut inner = self.inner.write();
        inner.data.clear();
        inner.time_index.clear();
        inner.total_bytes = 0;
        tracing::info!("Cleared memory storage");
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HlsStorage for MemoryStorage {
    async fn write(&self, key: &str, data: Bytes) -> Result<()> {
        let size = data.len();

        if self.max_memory_bytes > 0 && size > self.max_memory_bytes {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Data size ({size} bytes) exceeds max memory limit ({} bytes)",
                    self.max_memory_bytes
                ),
            ));
        }

        let mut inner = self.inner.write();

        // If key already exists, remove the old entry first
        inner.remove(key);

        // Evict old entries if needed
        inner.evict_if_needed(size, self.max_keys, self.max_memory_bytes);

        let seq = next_seq();
        let write_time = std::time::Instant::now();
        inner.total_bytes += size;
        inner.time_index.insert(seq, key.to_string());
        inner.data.insert(key.to_string(), Entry { data, seq, write_time });

        tracing::trace!("Wrote to memory: {} ({} bytes)", key, size);

        Ok(())
    }

    async fn read(&self, key: &str) -> Result<Bytes> {
        let inner = self.inner.read();
        if let Some(entry) = inner.data.get(key) {
            tracing::trace!("Read from memory: {} ({} bytes)", key, entry.data.len());
            Ok(entry.data.clone())
        } else {
            tracing::warn!("Key not found in memory: {}", key);
            Err(Error::new(
                ErrorKind::NotFound,
                format!("Key not found: {key}"),
            ))
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let mut inner = self.inner.write();
        if inner.remove(key) {
            tracing::trace!("Deleted from memory: {}", key);
        }
        Ok(())
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        let inner = self.inner.read();
        Ok(inner.data.contains_key(key))
    }

    async fn cleanup(&self, older_than: Duration) -> Result<usize> {
        let mut inner = self.inner.write();
        let cutoff = std::time::Instant::now()
            .checked_sub(older_than)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "older_than duration is too large"))?;

        // Collect expired keys (O(N) scan, but cleanup is infrequent)
        let expired_keys: Vec<String> = inner.data
            .iter()
            .filter(|(_, entry)| entry.write_time < cutoff)
            .map(|(key, _)| key.clone())
            .collect();

        let mut deleted = 0;
        for key in expired_keys {
            if inner.remove(&key) {
                deleted += 1;
                tracing::trace!("Deleted expired key from memory: {}", key);
            }
        }

        tracing::info!(
            "Cleanup expired: deleted {} keys older than {:?}",
            deleted,
            older_than
        );

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_memory_storage_write_read() {
        let storage = MemoryStorage::new();

        let data = Bytes::from_static(b"test segment data");
        let result = storage
            .write("live-room_123-segment_0", data.clone())
            .await;
        assert!(result.is_ok());

        let read_data = storage
            .read("live-room_123-segment_0")
            .await
            .unwrap();
        assert_eq!(data, read_data);

        let exists = storage
            .exists("live-room_123-segment_0")
            .await
            .unwrap();
        assert!(exists);

        assert_eq!(storage.memory_usage().await, data.len());
        assert_eq!(storage.key_count().await, 1);

        let result = storage.delete("live-room_123-segment_0").await;
        assert!(result.is_ok());

        let exists = storage
            .exists("live-room_123-segment_0")
            .await
            .unwrap();
        assert!(!exists);

        assert_eq!(storage.memory_usage().await, 0);
        assert_eq!(storage.key_count().await, 0);
    }

    #[tokio::test]
    async fn test_memory_storage_clear() {
        let storage = MemoryStorage::new();

        storage
            .write("live-room_123-segment_0", Bytes::from_static(b"data1"))
            .await
            .unwrap();
        storage
            .write("live-room_456-segment_0", Bytes::from_static(b"data2"))
            .await
            .unwrap();

        assert_eq!(storage.key_count().await, 2);

        storage.clear().await;

        assert_eq!(storage.key_count().await, 0);
        assert_eq!(storage.memory_usage().await, 0);
    }

    #[tokio::test]
    async fn test_memory_storage_not_found() {
        let storage = MemoryStorage::new();

        let result = storage.read("live-room_123-segment_0").await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn test_memory_storage_public_url() {
        let storage = MemoryStorage::new();

        let url = storage.get_public_url("live-room_123-segment_0").await.unwrap();
        assert_eq!(url, None);
    }

    #[tokio::test]
    async fn test_memory_storage_key_limit_eviction() {
        let storage = MemoryStorage::with_limits(0, 3);

        storage.write("key1", Bytes::from_static(b"data1")).await.unwrap();
        storage.write("key2", Bytes::from_static(b"data2")).await.unwrap();
        storage.write("key3", Bytes::from_static(b"data3")).await.unwrap();
        assert_eq!(storage.key_count().await, 3);

        // Writing a 4th key should evict the oldest (key1)
        storage.write("key4", Bytes::from_static(b"data4")).await.unwrap();
        assert_eq!(storage.key_count().await, 3);
        assert!(!storage.exists("key1").await.unwrap());
        assert!(storage.exists("key4").await.unwrap());
    }

    #[tokio::test]
    async fn test_memory_storage_memory_limit_eviction() {
        let storage = MemoryStorage::with_limits(15, 0);

        storage.write("key1", Bytes::from_static(b"12345")).await.unwrap(); // 5 bytes
        storage.write("key2", Bytes::from_static(b"12345")).await.unwrap(); // 5 bytes, total 10
        assert_eq!(storage.key_count().await, 2);
        assert_eq!(storage.memory_usage().await, 10);

        // Writing 10 more bytes would exceed 15 byte limit, oldest (key1) should be evicted
        storage.write("key3", Bytes::from_static(b"1234567890")).await.unwrap(); // 10 bytes
        assert!(storage.memory_usage().await <= 15);
        assert!(!storage.exists("key1").await.unwrap());
        assert!(storage.exists("key3").await.unwrap());
    }

    #[tokio::test]
    async fn test_memory_storage_reject_oversized() {
        let storage = MemoryStorage::with_limits(10, 0);

        let result = storage.write("big", Bytes::from(vec![0u8; 20])).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn test_memory_storage_unlimited() {
        let storage = MemoryStorage::unlimited();

        for i in 0..100 {
            storage
                .write(&format!("key{i}"), Bytes::from(vec![0u8; 1024]))
                .await
                .unwrap();
        }
        assert_eq!(storage.key_count().await, 100);
    }

    #[tokio::test]
    async fn test_memory_storage_overwrite_key() {
        let storage = MemoryStorage::with_limits(100, 0);

        storage.write("key1", Bytes::from_static(b"hello")).await.unwrap();
        assert_eq!(storage.memory_usage().await, 5);

        // Overwriting same key should update data and not double-count memory
        storage.write("key1", Bytes::from_static(b"world!")).await.unwrap();
        assert_eq!(storage.memory_usage().await, 6);
        assert_eq!(storage.key_count().await, 1);

        let data = storage.read("key1").await.unwrap();
        assert_eq!(data, Bytes::from_static(b"world!"));
    }
}
