use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;
use synctv_common::ExecutionControl;

use super::status::CacheStatus;

/// Operational snapshot of the slice cache runtime.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SliceCacheStats {
    pub engine_enabled: bool,
    pub backend: String,
    pub file_cache_dir: Option<String>,
    pub slice_size: u64,
    pub max_cache_size: u64,
    pub segment_ttl_secs: u64,
    pub stale_max_age_secs: u64,
    pub stale_while_revalidate: bool,
    pub eviction_interval_secs: u64,
    pub watermark_ratio: f64,
    pub current_size_bytes: u64,
    pub entry_count: u64,
    pub metadata_entries: u64,
    pub updating_entries: u64,
    pub lock_count: u64,
    pub usage_ratio: f64,
}

#[derive(Clone)]
pub(super) struct CachedSlice {
    pub total_size: u64,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub data: Bytes,
}

#[derive(Clone)]
pub(super) struct FetchedSlice {
    pub slice: CachedSlice,
    pub status: CacheStatus,
}

pub(super) enum SliceFetchResult {
    Slice(FetchedSlice),
    Bypass(reqwest::Response),
}

pub(super) struct HeadResourceResult {
    pub status: reqwest::StatusCode,
    pub headers: reqwest::header::HeaderMap,
    pub cache_status: CacheStatus,
}

pub(super) struct SliceFetchRequest<'a> {
    pub url: &'a str,
    pub provider_headers: &'a HashMap<String, String>,
    pub slice_index: u64,
    pub known_total_size: Option<u64>,
    pub request_control: Option<&'a ExecutionControl>,
    pub upstream_header_timeout: Option<Duration>,
    pub bypass_on_non_partial: bool,
}

/// Result of purging all slice-cache entries.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct SliceCachePurgeResult {
    pub removed_entries: u64,
    pub freed_bytes: u64,
}
