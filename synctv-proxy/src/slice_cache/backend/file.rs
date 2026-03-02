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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncReadExt;

use super::SliceCacheBackend;
use crate::slice_cache::etag::StoredEntry;

/// Magic bytes identifying a valid SyncTV cache file (version 1).
const CACHE_FILE_MAGIC: &[u8; 4] = b"STV\x01";

/// Minimum size of a valid cache file: 4 (magic) + 4 (header_len) = 8.
const MIN_FILE_SIZE: u64 = 8;

/// Safety limit: reject any cache file whose header claims to be larger than
/// 64 KiB.  A well-formed `FileEntryHeader` is typically under 1 KiB; anything
/// larger signals corruption or an attacker-crafted file.  64 KB is far more
/// than enough for a bincode-serialized header.
const MAX_HEADER_LEN: usize = 64 * 1024;

// ------------------------------------------------------------------
// File entry header (serialized into each cache file)
// ------------------------------------------------------------------

/// On-disk header written at the start of every cache file.
///
/// Intentionally stores timestamps as milliseconds-since-epoch so that
/// the format is portable and easy to inspect with external tools.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct FileEntryHeader {
    /// The original cache key (hex SHA256).
    key: String,
    /// Unix timestamp in milliseconds when the entry was first inserted.
    inserted_at_millis: u64,
    /// TTL in seconds.
    ttl_secs: u64,
    /// Unix timestamp in milliseconds of the last read access.
    last_accessed_millis: u64,
    /// Size of the cached data portion (bytes after the header).
    data_size: u64,
}

// ------------------------------------------------------------------
// In-memory index
// ------------------------------------------------------------------

/// A single entry in the in-memory file index.
///
/// The `last_accessed` field is updated lazily on reads (the file on
/// disk is NOT rewritten on every access -- matching nginx's approach
/// of updating the in-memory node and only persisting during cache
/// manager sweeps).
struct FileIndexEntry {
    /// Absolute path to the cache file on disk.
    path: PathBuf,
    /// Size of the data portion (excludes file header).
    data_size: u64,
    /// Insertion timestamp (millis since epoch).
    inserted_at_millis: u64,
    /// TTL in seconds.
    ttl_secs: u64,
    /// Last access timestamp (millis since epoch), updated atomically.
    last_accessed: AtomicU64,
}

/// The in-memory index that mirrors what is on disk.
///
/// Keyed by the cache key (hex SHA256).  The `total_size` atomic
/// tracks the aggregate data size across all entries for fast watermark
/// checks without iterating the map.
struct FileIndex {
    entries: DashMap<String, FileIndexEntry>,
    total_size: AtomicU64,
}

impl FileIndex {
    fn new() -> Self {
        Self {
            entries: DashMap::new(),
            total_size: AtomicU64::new(0),
        }
    }

    /// Insert or replace an entry, updating the total size.
    fn insert(&self, key: String, entry: FileIndexEntry) {
        let new_size = entry.data_size;
        if let Some(old) = self.entries.insert(key, entry) {
            // Replace: subtract old, add new.
            self.total_size.fetch_sub(old.data_size, Ordering::Relaxed);
        }
        self.total_size.fetch_add(new_size, Ordering::Relaxed);
    }

    /// Remove an entry from the index, updating the total size.
    /// Returns the removed entry if it existed.
    fn remove(&self, key: &str) -> Option<(String, FileIndexEntry)> {
        if let Some((k, v)) = self.entries.remove(key) {
            self.total_size.fetch_sub(v.data_size, Ordering::Relaxed);
            Some((k, v))
        } else {
            None
        }
    }

    fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    fn total_size(&self) -> u64 {
        self.total_size.load(Ordering::Relaxed)
    }
}

// ------------------------------------------------------------------
// FileBackend
// ------------------------------------------------------------------

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

/// Summary returned by [`FileBackend::load_index`].
#[derive(Debug, Default)]
pub struct LoadResult {
    /// Number of entries successfully loaded into the index.
    pub loaded: u64,
    /// Number of expired/stale entries deleted from disk.
    pub deleted: u64,
    /// Number of files that could not be read (corrupted, etc.).
    pub errors: u64,
    /// Total data bytes across all loaded entries.
    pub total_bytes: u64,
}

impl FileBackend {
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

    // ---------------------------------------------------------------
    // Path helpers
    // ---------------------------------------------------------------

    /// Map a cache key to its on-disk path using the 2-level directory
    /// hierarchy.
    ///
    /// For `dir_levels = (2, 2)` and key `"abcdef01234..."`:
    ///
    /// ```text
    /// cache_dir/ab/cd/abcdef01234...
    /// ```
    fn key_to_path(&self, key: &str) -> PathBuf {
        let (l1, l2) = self.dir_levels;
        let level1 = &key[..l1.min(key.len())];
        let level2 = &key[l1..((l1 + l2).min(key.len()))];
        self.cache_dir.join(level1).join(level2).join(key)
    }

    /// Return the path to the `.tmp` staging directory.
    fn tmp_dir(&self) -> PathBuf {
        self.cache_dir.join(".tmp")
    }

    /// Generate a unique temp file name using a monotonic counter and
    /// the process ID (avoids requiring the `rand` crate).
    fn next_temp_name(&self) -> String {
        let counter = self.temp_counter.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        format!("tmp_{pid}_{counter:012}")
    }

    // ---------------------------------------------------------------
    // Startup cache loader
    // ---------------------------------------------------------------

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
        let mut result = LoadResult::default();
        let now = millis_since_epoch();
        let stale_max_millis = stale_max_age.as_millis() as u64;

        self.walk_and_load(&self.cache_dir, now, stale_max_millis, &mut result)
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

    /// Recursive directory walker for [`load_index`](Self::load_index).
    fn walk_and_load<'a>(
        &'a self,
        dir: &'a PathBuf,
        now: u64,
        stale_max_millis: u64,
        result: &'a mut LoadResult,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut read_dir = match fs::read_dir(dir).await {
                Ok(rd) => rd,
                Err(e) => {
                    tracing::warn!(
                        dir = %dir.display(),
                        "Failed to read cache directory: {e}"
                    );
                    return Ok(());
                }
            };

            while let Some(entry) = read_dir.next_entry().await? {
                let path = entry.path();
                let metadata = match entry.metadata().await {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            "Failed to read metadata: {e}"
                        );
                        result.errors += 1;
                        continue;
                    }
                };

                if metadata.is_dir() {
                    // Skip the .tmp staging directory.
                    if path.file_name().is_some_and(|n| n == ".tmp") {
                        continue;
                    }
                    self.walk_and_load(&path, now, stale_max_millis, result)
                        .await?;
                    continue;
                }

                if !metadata.is_file() {
                    continue;
                }

                // Attempt to read and validate the cache file header.
                match read_cache_file_header(&path).await {
                    Ok(header) => {
                        let deadline_millis = header.inserted_at_millis + header.ttl_secs * 1000;
                        let stale_deadline = deadline_millis + stale_max_millis;

                        if now > stale_deadline {
                            // Expired beyond stale_max_age: delete.
                            if let Err(e) = fs::remove_file(&path).await {
                                tracing::warn!(
                                    path = %path.display(),
                                    "Failed to delete stale cache file: {e}"
                                );
                            }
                            result.deleted += 1;
                        } else {
                            // Valid or still within stale window: add to index.
                            self.index.insert(
                                header.key.clone(),
                                FileIndexEntry {
                                    path,
                                    data_size: header.data_size,
                                    inserted_at_millis: header.inserted_at_millis,
                                    ttl_secs: header.ttl_secs,
                                    last_accessed: AtomicU64::new(header.last_accessed_millis),
                                },
                            );
                            result.loaded += 1;
                            result.total_bytes += header.data_size;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            "Corrupted cache file during index load, deleting: {e}"
                        );
                        let _ = fs::remove_file(&path).await;
                        result.errors += 1;
                    }
                }
            }

            Ok(())
        })
    }

    // ---------------------------------------------------------------
    // Temp file cleanup
    // ---------------------------------------------------------------

    /// Remove orphaned temp files from the `.tmp` staging directory.
    ///
    /// Temp files older than 5 minutes are considered orphaned (the
    /// write that created them either completed or failed).  This
    /// matches nginx's `ngx_http_file_cache_manage_directory` pattern
    /// of skipping `/temp` during normal eviction but cleaning it
    /// separately.
    pub async fn cleanup_temp_files(&self) {
        let tmp_dir = self.tmp_dir();
        let mut read_dir = match fs::read_dir(&tmp_dir).await {
            Ok(rd) => rd,
            Err(_) => return, // .tmp may not exist yet; that's fine.
        };

        let cutoff = SystemTime::now() - Duration::from_secs(300);

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            match entry.metadata().await {
                Ok(meta) => {
                    let modified = meta.modified().unwrap_or(UNIX_EPOCH);
                    if modified < cutoff {
                        tracing::debug!(
                            path = %path.display(),
                            "Removing orphaned temp file"
                        );
                        let _ = fs::remove_file(&path).await;
                    }
                }
                Err(_) => {
                    // Cannot read metadata -- remove it.
                    let _ = fs::remove_file(&path).await;
                }
            }
        }
    }

    // ---------------------------------------------------------------
    // LRU access-time persistence (M3 fix)
    // ---------------------------------------------------------------

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
        for entry in self.index.entries.iter() {
            let current_accessed = entry.value().last_accessed.load(Ordering::Relaxed);
            let path = entry.value().path.clone();
            drop(entry); // Release the DashMap ref before doing I/O.

            if let Err(e) = update_file_last_accessed(&path, current_accessed).await {
                tracing::debug!(
                    path = %path.display(),
                    "Failed to persist access time: {e}"
                );
            }
        }
    }

    // ---------------------------------------------------------------
    // Internal: remove entry from both index and disk
    // ---------------------------------------------------------------

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

// ------------------------------------------------------------------
// SliceCacheBackend trait implementation
// ------------------------------------------------------------------

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
                let inserted_at = millis_to_system_time(inserted_at_millis);
                let ttl = Duration::from_secs(ttl_secs);
                let last_accessed = millis_to_system_time(now_millis);

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
        let path = self.key_to_path(key);

        // Ensure parent directories exist.
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let inserted_at_millis = system_time_to_millis(entry.inserted_at);
        let last_accessed_millis = system_time_to_millis(entry.last_accessed);

        let header = FileEntryHeader {
            key: key.to_string(),
            inserted_at_millis,
            ttl_secs: entry.ttl.as_secs(),
            last_accessed_millis,
            data_size: entry.data.len() as u64,
        };

        // Serialize header with bincode.
        let header_bytes =
            bincode::serialize(&header).map_err(|e| anyhow::anyhow!("bincode encode: {e}"))?;
        let header_len = header_bytes.len() as u32;

        // Build the complete file content.
        let mut file_content = Vec::with_capacity(4 + 4 + header_bytes.len() + entry.data.len());
        file_content.extend_from_slice(CACHE_FILE_MAGIC);
        file_content.extend_from_slice(&header_len.to_le_bytes());
        file_content.extend_from_slice(&header_bytes);
        file_content.extend_from_slice(&entry.data);

        // Atomic write: write to temp file, then rename.
        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir).await?;
        let tmp_name = self.next_temp_name();
        let tmp_path = tmp_dir.join(&tmp_name);

        fs::write(&tmp_path, &file_content).await.map_err(|e| {
            anyhow::anyhow!("Failed to write temp file {}: {e}", tmp_path.display())
        })?;

        if let Err(e) = fs::rename(&tmp_path, &path).await {
            // Clean up the temp file on rename failure.
            let _ = fs::remove_file(&tmp_path).await;
            return Err(anyhow::anyhow!(
                "Failed to rename {} -> {}: {e}",
                tmp_path.display(),
                path.display()
            ));
        }

        // Update in-memory index.
        self.index.insert(
            key.to_string(),
            FileIndexEntry {
                path,
                data_size: entry.data.len() as u64,
                inserted_at_millis,
                ttl_secs: entry.ttl.as_secs(),
                last_accessed: AtomicU64::new(last_accessed_millis),
            },
        );

        Ok(())
    }

    async fn remove(&self, key: &str) {
        self.remove_entry(key).await;
    }

    async fn contains(&self, key: &str) -> bool {
        self.index.contains(key)
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

        // Collect (key, last_accessed, data_size) for LRU sorting.
        let mut candidates: Vec<(String, u64, u64)> = self
            .index
            .entries
            .iter()
            .map(|r| {
                (
                    r.key().clone(),
                    r.last_accessed.load(Ordering::Relaxed),
                    r.data_size,
                )
            })
            .collect();

        // Sort by last_accessed ascending (oldest first = LRU).
        candidates.sort_by_key(|(_k, ts, _sz)| *ts);

        let mut freed = 0u64;
        for (key, _ts, size) in candidates {
            if self.current_size() <= target_bytes {
                break;
            }
            self.remove_entry(&key).await;
            freed += size;
        }
        freed
    }

    async fn evict_expired(&self) -> u64 {
        let now = millis_since_epoch();
        let mut expired_keys = Vec::new();

        for entry_ref in self.index.entries.iter() {
            let deadline_millis = entry_ref.inserted_at_millis + entry_ref.ttl_secs * 1000;
            if now > deadline_millis {
                expired_keys.push(entry_ref.key().clone());
            }
        }

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

// ------------------------------------------------------------------
// File I/O helpers
// ------------------------------------------------------------------

/// Read a cache file and return the deserialized header + data body.
async fn read_cache_file(path: &PathBuf) -> anyhow::Result<(FileEntryHeader, Bytes)> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to open cache file {}: {e}", path.display()))?;

    // Read magic bytes.
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read magic from {}: {e}", path.display()))?;
    if &magic != CACHE_FILE_MAGIC {
        return Err(anyhow::anyhow!(
            "Invalid magic in {}: expected {:?}, got {:?}",
            path.display(),
            CACHE_FILE_MAGIC,
            magic
        ));
    }

    // Read header length.
    let mut header_len_buf = [0u8; 4];
    file.read_exact(&mut header_len_buf).await?;
    let header_len = u32::from_le_bytes(header_len_buf) as usize;
    if header_len > MAX_HEADER_LEN {
        return Err(anyhow::anyhow!(
            "Header too large in {}: {} bytes (max {})",
            path.display(),
            header_len,
            MAX_HEADER_LEN
        ));
    }

    // Read and deserialize header.
    let mut header_buf = vec![0u8; header_len];
    file.read_exact(&mut header_buf).await?;
    let header: FileEntryHeader = bincode::deserialize(&header_buf).map_err(|e| {
        anyhow::anyhow!("Failed to deserialize header from {}: {e}", path.display())
    })?;

    // Read the remaining data body.
    let mut data_buf = Vec::new();
    file.read_to_end(&mut data_buf).await?;

    if data_buf.len() as u64 != header.data_size {
        return Err(anyhow::anyhow!(
            "Data size mismatch in {}: header says {} but file has {} bytes",
            path.display(),
            header.data_size,
            data_buf.len()
        ));
    }

    Ok((header, Bytes::from(data_buf)))
}

/// Read only the header from a cache file (used during index loading
/// to avoid reading potentially large data bodies into memory).
async fn read_cache_file_header(path: &PathBuf) -> anyhow::Result<FileEntryHeader> {
    let metadata = fs::metadata(path).await?;
    if metadata.len() < MIN_FILE_SIZE {
        return Err(anyhow::anyhow!(
            "File {} is too small ({} bytes) to be a valid cache file",
            path.display(),
            metadata.len()
        ));
    }

    let mut file = tokio::fs::File::open(path).await?;

    // Read magic bytes.
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).await?;
    if &magic != CACHE_FILE_MAGIC {
        return Err(anyhow::anyhow!(
            "Invalid magic in {}: expected {:?}, got {:?}",
            path.display(),
            CACHE_FILE_MAGIC,
            magic
        ));
    }

    // Read header length.
    let mut header_len_buf = [0u8; 4];
    file.read_exact(&mut header_len_buf).await?;
    let header_len = u32::from_le_bytes(header_len_buf) as usize;
    if header_len > MAX_HEADER_LEN {
        return Err(anyhow::anyhow!(
            "Header too large in {}: {} bytes (max {})",
            path.display(),
            header_len,
            MAX_HEADER_LEN
        ));
    }

    // Read and deserialize header.
    let mut header_buf = vec![0u8; header_len];
    file.read_exact(&mut header_buf).await?;
    let header: FileEntryHeader = bincode::deserialize(&header_buf)?;

    Ok(header)
}

/// Update the `last_accessed_millis` field in an existing cache file's header
/// without rewriting the data portion.
///
/// This is used by [`FileBackend::persist_access_times`] to write back the
/// in-memory LRU timestamps so they survive restarts.  The header is read,
/// modified, and written back in-place.  If the re-serialized header differs
/// in length from the original (should not happen with the same struct
/// definition), the update is silently skipped.
async fn update_file_last_accessed(
    path: &std::path::Path,
    last_accessed_millis: u64,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .await?;

    // Read magic.
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).await?;
    if &magic != CACHE_FILE_MAGIC {
        return Err(anyhow::anyhow!("Invalid magic"));
    }

    // Read header length.
    let mut header_len_buf = [0u8; 4];
    file.read_exact(&mut header_len_buf).await?;
    let header_len = u32::from_le_bytes(header_len_buf) as usize;
    if header_len > MAX_HEADER_LEN {
        return Err(anyhow::anyhow!("Header too large: {header_len}"));
    }

    // Read and deserialize existing header.
    let mut header_buf = vec![0u8; header_len];
    file.read_exact(&mut header_buf).await?;
    let mut header: FileEntryHeader = bincode::deserialize(&header_buf)?;

    // Update last_accessed.
    header.last_accessed_millis = last_accessed_millis;

    // Re-serialize.
    let new_header_buf =
        bincode::serialize(&header).map_err(|e| anyhow::anyhow!("bincode encode: {e}"))?;
    if new_header_buf.len() != header_len {
        // Header size changed -- skip update to avoid corrupting the file.
        return Ok(());
    }

    // Seek back to header position (offset 8 = 4 magic + 4 header_len).
    file.seek(std::io::SeekFrom::Start(8)).await?;
    file.write_all(&new_header_buf).await?;
    file.flush().await?;

    Ok(())
}

// ------------------------------------------------------------------
// Timestamp helpers
// ------------------------------------------------------------------

/// Return the current time as milliseconds since the Unix epoch.
fn millis_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Convert a [`SystemTime`] to milliseconds since the Unix epoch.
fn system_time_to_millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// Convert milliseconds since the Unix epoch to a [`SystemTime`].
fn millis_to_system_time(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

// ------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: create a `FileBackend` in a fresh temp directory.
    async fn make_backend() -> (FileBackend, TempDir) {
        let tmp = TempDir::new().expect("create temp dir");
        let backend = FileBackend::new(tmp.path().to_path_buf(), (2, 2))
            .await
            .expect("create backend");
        (backend, tmp)
    }

    /// Helper: store an entry with sensible defaults.
    async fn put_entry(backend: &FileBackend, key: &str, data: &[u8]) {
        let entry = StoredEntry::new(Bytes::from(data.to_vec()), Duration::from_secs(300));
        backend.put(key, entry).await.expect("put entry");
    }

    // --- Basic put/get ---

    #[tokio::test]
    async fn test_file_backend_put_get() {
        let (backend, _tmp) = make_backend().await;
        let data = b"hello cache world";
        let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

        put_entry(&backend, key, data).await;

        let result = backend.get(key).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().data, Bytes::from(data.to_vec()));
    }

    #[tokio::test]
    async fn test_file_backend_get_missing() {
        let (backend, _tmp) = make_backend().await;
        let result = backend
            .get("nonexistent_key_00000000000000000000000000000000000000000000")
            .await;
        assert!(result.is_none());
    }

    // --- Remove ---

    #[tokio::test]
    async fn test_file_backend_remove() {
        let (backend, _tmp) = make_backend().await;
        let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        put_entry(&backend, key, b"data").await;
        assert!(backend.contains(key).await);

        backend.remove(key).await;
        assert!(!backend.contains(key).await);

        let result = backend.get(key).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_file_backend_remove_nonexistent() {
        let (backend, _tmp) = make_backend().await;
        // Should not panic.
        backend
            .remove("does_not_exist_0000000000000000000000000000000000000000000000")
            .await;
    }

    // --- Contains ---

    #[tokio::test]
    async fn test_file_backend_contains() {
        let (backend, _tmp) = make_backend().await;
        let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        assert!(!backend.contains(key).await);

        put_entry(&backend, key, b"data").await;
        assert!(backend.contains(key).await);
    }

    // --- Directory structure ---

    #[tokio::test]
    async fn test_file_backend_directory_structure() {
        let (backend, tmp) = make_backend().await;
        let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

        put_entry(&backend, key, b"payload").await;

        // With dir_levels (2, 2), expect: cache_dir/ab/cd/<key>
        let expected_path = tmp.path().join("ab").join("cd").join(key);
        assert!(
            expected_path.exists(),
            "Expected cache file at {}",
            expected_path.display()
        );
    }

    // --- Atomic write ---

    #[tokio::test]
    async fn test_file_backend_atomic_write() {
        let (backend, tmp) = make_backend().await;
        let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

        put_entry(&backend, key, b"first").await;
        put_entry(&backend, key, b"second").await;

        // The final file should have "second".
        let result = backend.get(key).await.expect("should exist");
        assert_eq!(result.data, Bytes::from("second"));

        // No leftover temp files.
        let mut tmp_entries = fs::read_dir(tmp.path().join(".tmp"))
            .await
            .expect("read .tmp");
        let mut count = 0u64;
        while tmp_entries.next_entry().await.expect("entry").is_some() {
            count += 1;
        }
        assert_eq!(count, 0, "No orphaned temp files should remain");
    }

    // --- Current size ---

    #[tokio::test]
    async fn test_file_backend_current_size() {
        let (backend, _tmp) = make_backend().await;
        assert_eq!(backend.current_size(), 0);

        put_entry(
            &backend,
            "aaaa0000000000000000000000000000000000000000000000000000000000aa",
            &[0u8; 100],
        )
        .await;
        assert_eq!(backend.current_size(), 100);

        put_entry(
            &backend,
            "bbbb0000000000000000000000000000000000000000000000000000000000bb",
            &[0u8; 200],
        )
        .await;
        assert_eq!(backend.current_size(), 300);

        backend
            .remove("aaaa0000000000000000000000000000000000000000000000000000000000aa")
            .await;
        assert_eq!(backend.current_size(), 200);
    }

    // --- Entry count ---

    #[tokio::test]
    async fn test_file_backend_entry_count() {
        let (backend, _tmp) = make_backend().await;
        assert_eq!(backend.entry_count(), 0);

        put_entry(
            &backend,
            "aaaa0000000000000000000000000000000000000000000000000000000000aa",
            b"a",
        )
        .await;
        put_entry(
            &backend,
            "bbbb0000000000000000000000000000000000000000000000000000000000bb",
            b"b",
        )
        .await;
        assert_eq!(backend.entry_count(), 2);
    }

    // --- Evict expired ---

    #[tokio::test]
    async fn test_file_backend_evict_expired() {
        let (backend, _tmp) = make_backend().await;

        // Insert an entry that is already expired (inserted 10 seconds ago, TTL = 1 second).
        let past = SystemTime::now() - Duration::from_secs(10);
        let expired_entry = StoredEntry {
            data: Bytes::from("old_data"),
            inserted_at: past,
            ttl: Duration::from_secs(1),
            last_accessed: past,
        };
        backend
            .put(
                "expired_key_000000000000000000000000000000000000000000000000000000",
                expired_entry,
            )
            .await
            .expect("put expired");

        // Insert a fresh entry.
        put_entry(
            &backend,
            "fresh_key_0000000000000000000000000000000000000000000000000000000000",
            b"fresh_data",
        )
        .await;

        assert_eq!(backend.entry_count(), 2);

        let evicted = backend.evict_expired().await;
        assert_eq!(evicted, 1);
        assert_eq!(backend.entry_count(), 1);
        assert!(
            backend
                .contains("fresh_key_0000000000000000000000000000000000000000000000000000000000")
                .await
        );
    }

    // --- Evict to size ---

    #[tokio::test]
    async fn test_file_backend_evict_to_size() {
        let (backend, _tmp) = make_backend().await;

        // Insert three entries with staggered access times.
        let t1 = SystemTime::now() - Duration::from_secs(30);
        let t2 = SystemTime::now() - Duration::from_secs(20);
        let t3 = SystemTime::now() - Duration::from_secs(10);

        backend
            .put(
                "oldest_key_0000000000000000000000000000000000000000000000000000000",
                StoredEntry {
                    data: Bytes::from(vec![0u8; 100]),
                    inserted_at: t1,
                    ttl: Duration::from_secs(3600),
                    last_accessed: t1,
                },
            )
            .await
            .expect("put oldest");

        backend
            .put(
                "middle_key_0000000000000000000000000000000000000000000000000000000",
                StoredEntry {
                    data: Bytes::from(vec![0u8; 100]),
                    inserted_at: t2,
                    ttl: Duration::from_secs(3600),
                    last_accessed: t2,
                },
            )
            .await
            .expect("put middle");

        backend
            .put(
                "newest_key_0000000000000000000000000000000000000000000000000000000",
                StoredEntry {
                    data: Bytes::from(vec![0u8; 100]),
                    inserted_at: t3,
                    ttl: Duration::from_secs(3600),
                    last_accessed: t3,
                },
            )
            .await
            .expect("put newest");

        assert_eq!(backend.current_size(), 300);

        // Evict down to 150 bytes -- should remove the 2 oldest.
        let freed = backend.evict_to_size(150).await;
        assert!(
            freed >= 200,
            "Expected at least 200 bytes freed, got {freed}"
        );
        assert!(
            backend.current_size() <= 150,
            "Expected size <= 150, got {}",
            backend.current_size()
        );

        // The newest entry should survive.
        assert!(
            backend
                .contains("newest_key_0000000000000000000000000000000000000000000000000000000")
                .await
        );
    }

    #[tokio::test]
    async fn test_file_backend_evict_to_size_no_op() {
        let (backend, _tmp) = make_backend().await;
        put_entry(
            &backend,
            "aaaa0000000000000000000000000000000000000000000000000000000000aa",
            &[0u8; 50],
        )
        .await;

        // Target is above current size -- nothing to do.
        let freed = backend.evict_to_size(1000).await;
        assert_eq!(freed, 0);
        assert_eq!(backend.entry_count(), 1);
    }

    // --- Load index ---

    #[tokio::test]
    async fn test_file_backend_load_index() {
        let tmp = TempDir::new().expect("create temp dir");
        let cache_dir = tmp.path().to_path_buf();

        // Phase 1: populate cache files using one backend instance.
        {
            let backend = FileBackend::new(cache_dir.clone(), (2, 2))
                .await
                .expect("create backend");

            put_entry(
                &backend,
                "aaaa0000000000000000000000000000000000000000000000000000000000aa",
                b"hello",
            )
            .await;
            put_entry(
                &backend,
                "bbbb0000000000000000000000000000000000000000000000000000000000bb",
                b"world",
            )
            .await;
        }

        // Phase 2: create a fresh backend and rebuild the index from disk.
        let backend2 = FileBackend::new(cache_dir, (2, 2))
            .await
            .expect("create backend2");
        assert_eq!(backend2.entry_count(), 0, "Fresh backend has empty index");

        let result = backend2
            .load_index(Duration::from_secs(3600))
            .await
            .expect("load index");

        assert_eq!(result.loaded, 2);
        assert_eq!(result.errors, 0);
        assert_eq!(result.deleted, 0);
        assert_eq!(result.total_bytes, 10); // "hello" + "world"

        // Verify that we can read entries via the rebuilt index.
        let entry = backend2
            .get("aaaa0000000000000000000000000000000000000000000000000000000000aa")
            .await
            .expect("should exist");
        assert_eq!(entry.data, Bytes::from("hello"));
    }

    #[tokio::test]
    async fn test_file_backend_load_index_deletes_stale() {
        let tmp = TempDir::new().expect("create temp dir");
        let cache_dir = tmp.path().to_path_buf();

        // Insert an entry that expired 100 seconds ago with TTL of 1 second.
        {
            let backend = FileBackend::new(cache_dir.clone(), (2, 2))
                .await
                .expect("create backend");
            let past = SystemTime::now() - Duration::from_secs(100);
            let stale_entry = StoredEntry {
                data: Bytes::from("stale_data"),
                inserted_at: past,
                ttl: Duration::from_secs(1),
                last_accessed: past,
            };
            backend
                .put(
                    "stale_key_0000000000000000000000000000000000000000000000000000000",
                    stale_entry,
                )
                .await
                .expect("put stale");
        }

        // Load with a stale_max_age of 10 seconds (entry is 100s past
        // expiry, so it should be deleted).
        let backend2 = FileBackend::new(cache_dir, (2, 2))
            .await
            .expect("create backend2");
        let result = backend2
            .load_index(Duration::from_secs(10))
            .await
            .expect("load index");

        assert_eq!(result.loaded, 0);
        assert_eq!(result.deleted, 1);
    }

    // --- Corrupted file handling ---

    #[tokio::test]
    async fn test_file_backend_corrupted_file_handled() {
        let tmp = TempDir::new().expect("create temp dir");
        let cache_dir = tmp.path().to_path_buf();

        // Write a garbage file that looks like it belongs in the cache hierarchy.
        let garbage_dir = cache_dir.join("ga").join("rb");
        fs::create_dir_all(&garbage_dir).await.expect("create dirs");
        let garbage_path =
            garbage_dir.join("garb0000000000000000000000000000000000000000000000000000000000");
        fs::write(&garbage_path, b"this is not a valid cache file")
            .await
            .expect("write garbage");

        // Also need the .tmp dir to exist.
        fs::create_dir_all(cache_dir.join(".tmp"))
            .await
            .expect("create .tmp");

        let backend = FileBackend::new(cache_dir, (2, 2))
            .await
            .expect("create backend");
        let result = backend
            .load_index(Duration::from_secs(3600))
            .await
            .expect("load index");

        assert_eq!(result.errors, 1);
        assert_eq!(result.loaded, 0);

        // The garbage file should have been deleted.
        assert!(
            !garbage_path.exists(),
            "Corrupted file should have been deleted"
        );
    }

    // --- Keys ---

    #[tokio::test]
    async fn test_file_backend_keys() {
        let (backend, _tmp) = make_backend().await;

        put_entry(
            &backend,
            "aaaa0000000000000000000000000000000000000000000000000000000000aa",
            b"data_a",
        )
        .await;
        put_entry(
            &backend,
            "bbbb0000000000000000000000000000000000000000000000000000000000bb",
            b"data_b",
        )
        .await;

        let mut keys = backend.keys().await;
        keys.sort();

        assert_eq!(keys.len(), 2);
        assert_eq!(
            keys[0],
            "aaaa0000000000000000000000000000000000000000000000000000000000aa"
        );
        assert_eq!(
            keys[1],
            "bbbb0000000000000000000000000000000000000000000000000000000000bb"
        );
    }

    // --- Temp file cleanup ---

    #[tokio::test]
    async fn test_file_backend_cleanup_temp_files() {
        let (backend, tmp) = make_backend().await;

        // Create a fake orphaned temp file.
        let tmp_dir = tmp.path().join(".tmp");
        let orphan = tmp_dir.join("tmp_orphaned_file");
        fs::write(&orphan, b"orphan data")
            .await
            .expect("write orphan");

        // The cleanup function runs without panic.  Since the file was
        // just created (< 5 min old), it should not be removed.
        backend.cleanup_temp_files().await;

        // File should still exist because it's brand new.
        assert!(orphan.exists(), "Fresh temp file should not be cleaned up");
    }

    // --- Edge cases ---

    #[tokio::test]
    async fn test_file_backend_overwrite_updates_size() {
        let (backend, _tmp) = make_backend().await;
        let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

        put_entry(&backend, key, &[0u8; 100]).await;
        assert_eq!(backend.current_size(), 100);

        // Overwrite with a different size.
        put_entry(&backend, key, &[0u8; 50]).await;
        assert_eq!(backend.current_size(), 50);
    }

    #[tokio::test]
    async fn test_file_backend_empty_data() {
        let (backend, _tmp) = make_backend().await;
        let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

        put_entry(&backend, key, b"").await;
        let result = backend.get(key).await.expect("should exist");
        assert_eq!(result.data, Bytes::new());
        assert_eq!(backend.current_size(), 0);
    }

    #[tokio::test]
    async fn test_file_backend_get_returns_stored_entry_fields() {
        let (backend, _tmp) = make_backend().await;
        let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let ttl = Duration::from_secs(600);

        let entry = StoredEntry::new(Bytes::from("test data"), ttl);
        backend.put(key, entry).await.expect("put");

        let got = backend.get(key).await.expect("should exist");
        assert_eq!(got.data, Bytes::from("test data"));
        assert_eq!(got.ttl.as_secs(), 600);
        // inserted_at should be very recent.
        assert!(
            got.inserted_at.elapsed().unwrap_or_default() < Duration::from_secs(5),
            "inserted_at should be recent"
        );
    }

    // --- LRU access time persistence (M3) ---

    /// Verify that `persist_access_times` writes updated `last_accessed` to
    /// disk, and a fresh backend loading the same cache directory picks up the
    /// updated timestamps.
    #[tokio::test]
    async fn test_file_backend_persist_access_times() {
        let tmp = TempDir::new().expect("create temp dir");
        let cache_dir = tmp.path().to_path_buf();

        let key_a = "aaaa0000000000000000000000000000000000000000000000000000000000aa";
        let key_b = "bbbb0000000000000000000000000000000000000000000000000000000000bb";

        // Phase 1: create entries, then read key_b to update its last_accessed.
        {
            let backend = FileBackend::new(cache_dir.clone(), (2, 2))
                .await
                .expect("create backend");
            put_entry(&backend, key_a, b"data_a").await;
            put_entry(&backend, key_b, b"data_b").await;

            // Access key_b to update its in-memory last_accessed.
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = backend.get(key_b).await.expect("get key_b");

            // Persist the in-memory access times to disk.
            backend.persist_access_times().await;
        }

        // Phase 2: reload from disk and verify that key_b has a more recent
        // last_accessed than key_a.
        {
            let backend2 = FileBackend::new(cache_dir, (2, 2))
                .await
                .expect("create backend2");
            backend2
                .load_index(Duration::from_secs(3600))
                .await
                .expect("load index");

            let entry_a = backend2.index.entries.get(key_a).expect("key_a in index");
            let entry_b = backend2.index.entries.get(key_b).expect("key_b in index");

            let accessed_a = entry_a.last_accessed.load(Ordering::Relaxed);
            let accessed_b = entry_b.last_accessed.load(Ordering::Relaxed);

            assert!(
                accessed_b > accessed_a,
                "key_b should have a more recent last_accessed than key_a \
                 (key_a={accessed_a}, key_b={accessed_b})"
            );
        }
    }

    /// Verify that `persist_access_times` does not corrupt cache files:
    /// after persisting, entries are still readable.
    #[tokio::test]
    async fn test_file_backend_persist_access_times_no_corruption() {
        let (backend, _tmp) = make_backend().await;
        let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

        put_entry(&backend, key, b"important data").await;

        // Read to update last_accessed.
        let _ = backend.get(key).await;

        // Persist.
        backend.persist_access_times().await;

        // Verify the entry is still readable and has correct data.
        let got = backend.get(key).await.expect("should still exist");
        assert_eq!(got.data, Bytes::from("important data"));
    }
}
