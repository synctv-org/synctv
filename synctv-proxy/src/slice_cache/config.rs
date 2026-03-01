//! Slice cache configuration.
//!
//! Contains [`SliceCacheConfig`] and content-type classification helpers.

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
    /// Maximum body size that will be cached in full-body mode.
    /// Responses larger than this are streamed through without caching.
    /// Default: 10 MB.
    pub max_cacheable_body: usize,
    /// TTL for M3U8/MPD manifest entries. Default: 5 seconds.
    pub manifest_ttl: Duration,
    /// TTL for media segment entries (slices and non-manifest full bodies).
    /// Default: 5 minutes.
    pub segment_ttl: Duration,
}

impl Default for SliceCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            slice_size: 2 * 1024 * 1024,                // 2 MB
            max_cache_size: 512 * 1024 * 1024,           // 512 MB
            max_cacheable_body: 10 * 1024 * 1024,        // 10 MB
            manifest_ttl: Duration::from_secs(5),
            segment_ttl: Duration::from_mins(5),
        }
    }
}

/// Returns `true` if the content-type looks like an M3U8 or DASH manifest.
pub(crate) fn is_manifest_content_type(ct: &str) -> bool {
    let lower = ct.to_lowercase();
    lower.contains("mpegurl")
        || lower.contains("m3u8")
        || lower.contains("dash+xml")
        || lower.contains("mpd")
}
