// HLS Storage abstraction layer
// Supports multiple storage backends:
// - FileStorage: Local filesystem (default)
// - MemoryStorage: In-memory (for testing/caching)
// - S3Storage: S3-compatible object storage
// Based on xiu's HLS implementation but with pluggable storage

pub mod file;
pub mod memory;
#[cfg(feature = "s3")]
pub mod s3;

use async_trait::async_trait;
use bytes::Bytes;
use std::io::{Error, ErrorKind, Result};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const SECONDS_PER_MINUTE: u64 = 60;
pub(crate) const HLS_SEGMENTS_ROOT: &str = "segments";
const MIN_EPOCH_MINUTE_BUCKET_DIGITS: usize = 8;

pub(crate) fn segment_minute_bucket(name: &str) -> Option<&str> {
    let (bucket, _) = name.split_once('_')?;
    if is_minute_bucket(bucket) {
        Some(bucket)
    } else {
        None
    }
}

pub(crate) fn is_minute_bucket(value: &str) -> bool {
    value.len() >= MIN_EPOCH_MINUTE_BUCKET_DIGITS
        && value.bytes().all(|b| b.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}

pub(crate) fn minute_bucket_is_expired(bucket: &str, older_than: Duration) -> bool {
    if !is_minute_bucket(bucket) {
        return false;
    }

    let Ok(bucket_minute) = bucket.parse::<u64>() else {
        return false;
    };

    let Some(bucket_end_secs) = bucket_minute
        .checked_add(1)
        .and_then(|minute| minute.checked_mul(SECONDS_PER_MINUTE))
    else {
        return false;
    };

    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return false;
    };
    let Some(cutoff) = now.checked_sub(older_than) else {
        return false;
    };
    let cutoff = cutoff.as_secs();

    bucket_end_secs <= cutoff
}

#[cfg(feature = "s3")]
pub(crate) fn path_leaf(path: &str) -> Option<&str> {
    path.trim_end_matches('/').rsplit('/').next()
}

/// Validate a single storage key component (app, stream, or name).
///
/// Rejects path traversal sequences, directory separators, null bytes, and empty strings.
pub fn validate_component(s: &str, label: &str) -> Result<()> {
    if s.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Storage {label} must not be empty"),
        ));
    }
    if s == "." || s == ".." {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Storage {label} must not be '.' or '..'"),
        ));
    }
    if s.contains('/') || s.contains('\\') || s.contains('\0') {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Storage {label} contains invalid characters"),
        ));
    }
    Ok(())
}

/// Validate all three storage key components.
pub fn validate_storage_key(app: &str, stream: &str, name: &str) -> Result<()> {
    validate_component(app, "app")?;
    validate_component(stream, "stream")?;
    validate_component(name, "name")?;
    Ok(())
}

/// HLS storage trait for pluggable backends
///
/// Structured key-value storage interface using `(app, stream, name)` components.
/// The storage layer should NOT know about:
/// - Segment metadata/lifecycle (handled by `SegmentManager`)
/// - M3U8 generation (handled by HLS layer)
///
/// Storage layer does: write, read, delete, exists, cleanup, and optionally provides public URLs.
#[async_trait]
pub trait HlsStorage: Send + Sync {
    /// Write data to storage
    ///
    /// # Arguments
    /// * `app` - Application name (e.g., `room_id`)
    /// * `stream` - Stream name (e.g., `media_id`)
    /// * `name` - Segment name (e.g., "29676270_a1b2c3d4e5f6")
    /// * `data` - Binary data to store
    async fn write(&self, app: &str, stream: &str, name: &str, data: Bytes) -> Result<()>;

    /// Read data from storage
    ///
    /// # Returns
    /// Binary data or `NotFound` error
    async fn read(&self, app: &str, stream: &str, name: &str) -> Result<Bytes>;

    /// Delete single item from storage
    async fn delete(&self, app: &str, stream: &str, name: &str) -> Result<()>;

    /// Check if item exists
    async fn exists(&self, app: &str, stream: &str, name: &str) -> Result<bool>;

    /// Delete all items under app/stream/
    ///
    /// Used for immediate segment cleanup when a specific stream ends,
    /// rather than waiting for periodic time-based cleanup.
    ///
    /// # Returns
    /// Number of items deleted
    async fn delete_app_stream(&self, app: &str, stream: &str) -> Result<usize>;

    /// Delete all items under app/
    ///
    /// # Returns
    /// Number of items deleted
    async fn delete_app(&self, app: &str) -> Result<usize>;

    /// List all distinct (app, stream) pairs currently stored.
    ///
    /// Used by `SegmentManager` to enumerate streams for per-stream segment count enforcement.
    ///
    /// # Returns
    /// List of (app, stream) tuples
    async fn list_streams(&self) -> Result<Vec<(String, String)>>;

    /// Count segments for a specific stream.
    ///
    /// Used by `SegmentManager` to enforce per-stream segment count limits.
    ///
    /// # Returns
    /// Number of segments stored for this app/stream
    async fn count_stream_segments(&self, app: &str, stream: &str) -> Result<usize>;

    /// Delete the oldest segments for a stream until count is at or below `max_count`.
    ///
    /// Used by `SegmentManager` to enforce per-stream segment count bounds.
    ///
    /// # Returns
    /// Number of segments deleted
    async fn delete_oldest_stream_segments(
        &self,
        app: &str,
        stream: &str,
        max_count: usize,
    ) -> Result<usize>;

    /// Cleanup expired data
    ///
    /// Storage backend scans and deletes all data older than the specified duration.
    /// Upper layer (`SegmentManager`) calls this periodically to cleanup old segments.
    ///
    /// # Returns
    /// Number of keys successfully deleted
    async fn cleanup(&self, older_than: std::time::Duration) -> Result<usize>;

    /// Get public URL for direct access (async)
    ///
    /// Use cases:
    /// - **S3 Storage with CDN**: Return CDN URL
    /// - **S3 Storage without CDN**: Generate temporary presigned URL with expiration
    /// - **File/Memory Storage**: Return None, let HTTP layer generate local URLs
    ///
    /// # Returns
    /// - `Ok(Some(url))` - Public URL (CDN or presigned) for direct access
    /// - `Ok(None)` - No public URL available (File/Memory storage)
    /// - `Err(e)` - Failed to generate presigned URL
    async fn get_public_url(&self, app: &str, stream: &str, name: &str) -> Result<Option<String>>;
}

/// Storage backend type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    /// Local filesystem storage
    File,
    /// In-memory storage (for testing/caching)
    Memory,
    /// S3-compatible object storage
    S3,
}

pub use file::FileStorage;
pub use memory::MemoryStorage;
#[cfg(feature = "s3")]
pub use s3::{S3Config, S3Storage};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minute_bucket_rejects_short_numeric_ids() {
        assert!(!is_minute_bucket("123"));
        assert_eq!(segment_minute_bucket("123_segment"), None);
        assert!(!minute_bucket_is_expired("123", Duration::from_mins(3)));
    }

    #[test]
    fn minute_bucket_accepts_epoch_minute_prefix() {
        assert!(is_minute_bucket("29676270"));
        assert_eq!(segment_minute_bucket("29676270_segment"), Some("29676270"));
    }
}
