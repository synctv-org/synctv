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
use std::io::Result;

/// HLS storage trait for pluggable backends
///
/// Pure key-value storage interface. The storage layer should NOT know about:
/// - Segment metadata/lifecycle (handled by `SegmentManager`)
/// - M3U8 generation (handled by HLS layer)
///
/// Storage layer does: write, read, delete, exists, cleanup, and optionally provides public URLs.
#[async_trait]
pub trait HlsStorage: Send + Sync {
    /// Write data to storage
    ///
    /// # Arguments
    /// * `key` - Storage key (e.g., "`live/room_123/segment_0.ts`")
    /// * `data` - Binary data to store
    async fn write(&self, key: &str, data: Bytes) -> Result<()>;

    /// Read data from storage
    ///
    /// # Arguments
    /// * `key` - Storage key
    ///
    /// # Returns
    /// Binary data or `NotFound` error
    async fn read(&self, key: &str) -> Result<Bytes>;

    /// Delete single key from storage
    ///
    /// # Arguments
    /// * `key` - Storage key
    async fn delete(&self, key: &str) -> Result<()>;

    /// Check if key exists
    ///
    /// # Arguments
    /// * `key` - Storage key
    async fn exists(&self, key: &str) -> Result<bool>;

    /// Cleanup expired data
    ///
    /// Storage backend scans and deletes all data older than the specified duration.
    /// Upper layer (`SegmentManager`) calls this periodically to cleanup old segments.
    ///
    /// # Arguments
    /// * `older_than` - Delete data older than this duration
    ///
    /// # Returns
    /// Number of keys successfully deleted
    ///
    /// # Default Implementation
    /// No-op by default (returns 0). Storage backends should implement this if possible.
    async fn cleanup(&self, _older_than: std::time::Duration) -> Result<usize> {
        Ok(0)
    }

    /// Delete all keys matching a given prefix.
    ///
    /// Used for immediate segment cleanup when a specific stream ends,
    /// rather than waiting for periodic time-based cleanup.
    ///
    /// # Arguments
    /// * `prefix` - Key prefix to match (e.g., "room123-media456-")
    ///
    /// # Returns
    /// Number of keys deleted
    ///
    /// # Default Implementation
    /// No-op by default (returns 0). Storage backends should implement this.
    async fn delete_by_prefix(&self, _prefix: &str) -> Result<usize> {
        Ok(0)
    }

    /// Get public URL for direct access (async)
    ///
    /// Use cases:
    /// - **OSS Storage with CDN**: Return CDN URL
    /// - **OSS Storage without CDN**: Generate temporary presigned URL with expiration
    /// - **File/Memory Storage**: Return None, let HTTP layer generate local URLs
    ///
    /// # Arguments
    /// * `key` - Storage key
    ///
    /// # Returns
    /// - `Ok(Some(url))` - Public URL (CDN or presigned) for direct access
    /// - `Ok(None)` - No public URL available (File/Memory storage)
    /// - `Err(e)` - Failed to generate presigned URL
    ///
    /// # Default Implementation
    /// Returns None by default (File/Memory storage don't need public URLs)
    async fn get_public_url(&self, _key: &str) -> Result<Option<String>> {
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
