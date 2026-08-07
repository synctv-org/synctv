//! Slice cache configuration.
//!
//! Contains [`SliceCacheConfig`], [`CacheBackendConfig`], and content-type
//! classification helpers.

use std::path::PathBuf;
use std::time::Duration;

/// Configuration for the slice cache.
#[derive(Clone, Debug)]
pub struct SliceCacheConfig {
    /// Whether the slice cache is enabled.
    pub enabled: bool,
    /// Size of each cache slice in bytes. Default: 2 MB.
    pub slice_size: usize,
    /// Maximum total cache size in bytes. Default: 512 MB.
    pub max_cache_size: u64,
    /// TTL for cached media slice entries.
    /// Default: 5 minutes.
    pub segment_ttl: Duration,
    /// Maximum time an expired entry can still be served as stale.
    /// Default: 60 seconds.
    pub stale_max_age: Duration,
    /// Whether to serve a stale entry while a background revalidation
    /// is in progress. Default: `true`.
    pub stale_while_revalidate: bool,
    /// Which storage backend to use. Default: `Memory`.
    pub backend: CacheBackendConfig,
    /// How often the background eviction task runs. Default: 60 seconds.
    pub eviction_interval: Duration,
    /// When eviction is triggered, free until usage drops below
    /// `max_cache_size * watermark_ratio`. Default: 0.875 (7/8).
    pub watermark_ratio: f64,
}

impl Default for SliceCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            slice_size: 2 * 1024 * 1024,       // 2 MB
            max_cache_size: 512 * 1024 * 1024, // 512 MB
            segment_ttl: Duration::from_mins(5),
            stale_max_age: Duration::from_mins(1),
            stale_while_revalidate: true,
            backend: CacheBackendConfig::default(),
            eviction_interval: Duration::from_mins(1),
            watermark_ratio: 0.875, // 7/8
        }
    }
}

/// Configuration for the cache storage backend.
#[derive(Clone, Debug, Default)]
pub enum CacheBackendConfig {
    /// In-memory backend (default). Fast but not persistent across restarts.
    #[default]
    Memory,
    /// File-based backend. Entries are stored on disk under `cache_dir`
    /// using a directory hierarchy inspired by nginx's `levels` directive.
    File {
        /// Root directory for cache files.
        cache_dir: PathBuf,
        /// Directory depth levels, e.g. `(1, 2)` corresponds to nginx's
        /// `levels=1:2` which creates paths like `cache_dir/a/bc/...`.
        dir_levels: (usize, usize),
    },
}
