//! Nginx-style range-request slice caching for media proxy.
//!
//! Splits large media files into fixed-size slices and caches each slice
//! independently.  Also supports full-body caching for non-range responses
//! (including M3U8/MPD manifests) with configurable TTLs.
//!
//! # Module structure
//!
//! Follows nginx's `ngx_http_slice_filter_module` separation:
//!
//! - **[`config`]**: `SliceCacheConfig`, `CacheBackendConfig`, `Default` impl,
//!   manifest content-type helper.
//! - **[`range`]**: Request Range parsing, response Content-Range parsing
//!   (modeled after `ngx_http_slice_parse_content_range`), and slice
//!   alignment helpers.
//! - **[`etag`]**: `CachedResourceMeta` for ETag consistency validation,
//!   `StoredEntry` for backend storage.
//! - **[`status`]**: `CacheStatus` enum (modeled after nginx cache status
//!   defines).
//! - **[`backend`]**: `SliceCacheBackend` trait and `CacheBackend` enum
//!   dispatch for memory and file backends.
//! - **[`store`]**: `SliceCache` struct with per-key locking, backend-agnostic
//!   storage, cache key computation, metadata management, and
//!   stale-while-revalidate support.
//! - **[`filter`]**: `proxy_with_cache`, `head_content_length`, and the
//!   full-body / stream-through paths (the "filter" entry points, analogous
//!   to nginx's header and body filters).
//!
//! # Key features
//!
//! - **Slice caching**: aligned 2 MB slices with per-key locking (thundering
//!   herd prevention).
//! - **Full body caching**: responses without Range support are cached as a
//!   single entry up to `max_cacheable_body`.
//! - **ETag consistency**: validates that the ETag is stable across slices
//!   belonging to the same resource; triggers invalidation on mismatch.
//! - **Content-Range validation**: upstream 206 responses are validated
//!   against the requested range, matching nginx's header filter logic.
//! - **TTL differentiation**: manifests get a shorter TTL than segments.
//! - **Refined cache status**: `HIT`, `MISS`, `BYPASS`, `EXPIRED`, `STALE`,
//!   `UPDATING`, `REVALIDATED`.

pub mod backend;
pub mod config;
pub mod etag;
pub mod filter;
pub mod lifecycle;
pub mod range;
pub mod status;
pub mod store;

// Re-export public items so that `synctv_proxy::slice_cache::Foo` still works.
pub use backend::{CacheBackend, SliceCacheBackend};
pub use config::SliceCacheConfig;
pub use etag::{CachedResourceMeta, StoredEntry};
pub use filter::{head_content_length, proxy_with_cache};
pub use lifecycle::CacheLifecycleManager;
pub use range::{
    aligned_range_for_slice, compute_needed_slices, parse_content_range, parse_range_header,
    ContentRange,
};
pub use status::CacheStatus;
pub use store::SliceCache;
