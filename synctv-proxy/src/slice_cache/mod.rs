//! Nginx-style range-request slice caching for media proxy.
//!
//! Splits large media files into fixed-size slices and caches each slice
//! independently. Non-range client requests are only cached when the origin
//! supports byte ranges; otherwise they are streamed through without caching.
//!
//! # Module structure
//!
//! Follows nginx's `ngx_http_slice_filter_module` separation:
//!
//! - **[`config`]**: `SliceCacheConfig`, `CacheBackendConfig`, `Default` impl,
//!   and backend selection.
//! - **[`range`]**: Request Range parsing, response Content-Range parsing
//!   (modeled after `ngx_http_slice_parse_content_range`), and slice
//!   alignment helpers.
//! - **[`etag`]**: `CachedResourceMeta` for ETag consistency validation,
//!   `StoredEntry` for backend storage.
//! - **[`status`]**: `CacheStatus` enum (modeled after nginx cache status
//!   defines).
//! - **`keys`**: deterministic cache-key and metadata-key hashing.
//! - **`types`**: shared slice-cache DTOs and public stats/result payloads.
//! - **`maintenance`**: small lock/metadata cleanup helpers.
//! - **[`backend`]**: `SliceCacheBackend` trait and `CacheBackend` enum
//!   dispatch for memory and file backends.
//! - **[`store`]**: `SliceCache` struct with per-key locking, backend-agnostic
//!   storage, metadata management, and stale-while-revalidate support.
//! - **[`filter`]**: `proxy_with_cache`, `head_content_length`, and the
//!   range-probe / stream-through paths (the "filter" entry points, analogous
//!   to nginx's header and body filters).
//!
//! # Key features
//!
//! - **Slice caching**: aligned 2 MB slices with per-key locking (thundering
//!   herd prevention).
//! - **No full-body caching**: non-range client requests are served from cached
//!   slices only when the origin supports range requests.
//! - **ETag consistency**: validates that the ETag is stable across slices
//!   belonging to the same resource; triggers invalidation on mismatch.
//! - **Content-Range validation**: upstream 206 responses are validated
//!   against the requested range, matching nginx's header filter logic.
//! - **Refined cache status**: `HIT`, `MISS`, `BYPASS`, `EXPIRED`, `STALE`,
//!   `UPDATING`, `REVALIDATED`.

pub mod backend;
pub mod config;
pub mod etag;
pub mod filter;
mod head;
mod keys;
pub mod lifecycle;
mod maintenance;
mod passthrough;
pub mod range;
pub mod status;
pub mod store;
mod types;

// Public slice-cache API.
pub use backend::{CacheBackend, SliceCacheBackend};
pub use config::{CacheBackendConfig, SliceCacheConfig};
pub use etag::{CachedResourceMeta, StoredEntry};
pub use filter::{
    proxy_head_with_cache_enabled_with_control,
    proxy_head_with_cache_enabled_with_control_and_timeout, proxy_with_cache,
    proxy_with_cache_enabled, proxy_with_cache_enabled_with_control,
    proxy_with_cache_enabled_with_control_and_timeout, proxy_with_cache_with_control,
    proxy_with_cache_with_control_and_timeout,
};
pub use lifecycle::CacheLifecycleManager;
pub use range::{
    aligned_range_for_slice, compute_needed_slices, parse_content_range, parse_range_header,
    ContentRange,
};
pub use status::CacheStatus;
pub use store::SliceCache;
pub use types::{SliceCachePurgeResult, SliceCacheStats};
