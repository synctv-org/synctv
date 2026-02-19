// HLS Storage abstraction layer
//
// Supports multiple storage backends:
// - FileStorage: Local filesystem (default)
// - MemoryStorage: In-memory (for testing/caching)
// - OssStorage: Object storage (S3/Aliyun OSS/etc)
//
// Based on xiu's HLS implementation but with pluggable storage

pub mod file;
pub mod memory;
pub mod oss;

use async_trait::async_trait;
use bytes::Bytes;
use std::io::{Error, ErrorKind, Result};

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
    /// * `app` - Application name (e.g., room_id)
    /// * `stream` - Stream name (e.g., media_id)
    /// * `name` - Segment name (e.g., "a1b2c3d4e5f6")
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
    async fn delete_app_stream(&self, _app: &str, _stream: &str) -> Result<usize> {
        Ok(0)
    }

    /// Delete all items under app/
    ///
    /// # Returns
    /// Number of items deleted
    async fn delete_app(&self, _app: &str) -> Result<usize> {
        Ok(0)
    }

    /// Cleanup expired data
    ///
    /// Storage backend scans and deletes all data older than the specified duration.
    /// Upper layer (`SegmentManager`) calls this periodically to cleanup old segments.
    ///
    /// # Returns
    /// Number of keys successfully deleted
    async fn cleanup(&self, _older_than: std::time::Duration) -> Result<usize> {
        Ok(0)
    }

    /// Get public URL for direct access (async)
    ///
    /// Use cases:
    /// - **OSS Storage with CDN**: Return CDN URL
    /// - **OSS Storage without CDN**: Generate temporary presigned URL with expiration
    /// - **File/Memory Storage**: Return None, let HTTP layer generate local URLs
    ///
    /// # Returns
    /// - `Ok(Some(url))` - Public URL (CDN or presigned) for direct access
    /// - `Ok(None)` - No public URL available (File/Memory storage)
    /// - `Err(e)` - Failed to generate presigned URL
    async fn get_public_url(&self, _app: &str, _stream: &str, _name: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

/// Storage backend type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    /// Local filesystem storage
    File,
    /// In-memory storage (for testing/caching)
    Memory,
    /// Object storage (S3/OSS/etc)
    Oss,
}

pub use file::FileStorage;
pub use memory::MemoryStorage;
pub use oss::{OssStorage, OssConfig};
