//! Slice cache store: per-key locking, backend-agnostic storage, metadata
//! management.
//!
//! Contains [`SliceCache`], the central cache struct backed by a
//! [`CacheBackend`] (memory or file) with per-key locking to prevent
//! thundering herd.

use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use sha2::{Digest, Sha256};
use synctv_common::ExecutionControl;
use tokio::sync::Mutex;

use crate::{
    apply_provider_headers, run_with_proxy_cancellation, send_with_redirect_validation,
    send_with_redirect_validation_with_control,
};

use super::backend::{CacheBackend, SliceCacheBackend};
use super::config::{CacheBackendConfig, SliceCacheConfig};
use super::etag::{CachedResourceMeta, StoredEntry};
use super::range::{parse_content_range, parse_range_header};
use super::status::CacheStatus;

/// Number of lock cleanup cycles before triggering a stale-lock sweep.
const LOCK_CLEANUP_INTERVAL: u64 = 64;
const MAX_META_ENTRIES: usize = 100_000;
const META_RETENTION_TARGET_DIVISOR: usize = 2;

// SliceCache

/// Per-key Mutex to prevent thundering herd on the same slice.
type SliceLock = Arc<Mutex<()>>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MetaEvictionCandidate {
    last_accessed: std::time::SystemTime,
    key: String,
}

/// A slice cache backed by a [`CacheBackend`] (memory or file).
///
/// Each cached entry is a [`StoredEntry`] containing the `Bytes` data plus
/// insertion metadata.  Per-key locking via a `DashMap<String, Mutex>`
/// ensures that concurrent requests for the same slice trigger at most a
/// single upstream fetch.
///
/// Resource metadata (ETag, Last-Modified, Content-Type) is stored in a
/// separate `DashMap` keyed by `url_hash + "meta"` to enable cross-slice
/// ETag/Last-Modified validation and conditional requests.
pub struct SliceCache {
    pub(super) config: SliceCacheConfig,
    /// Shared outbound HTTP client for cache fill and revalidation requests.
    client: reqwest::Client,
    /// The cache backend (memory or file).
    backend: Arc<CacheBackend>,
    /// Per-key locks to prevent thundering herd.
    locks: Arc<dashmap::DashMap<String, SliceLock>>,
    /// Per-resource metadata for ETag consistency validation.
    pub(super) meta: Arc<dashmap::DashMap<String, CachedResourceMeta>>,
    /// Tracks which cache keys have been inserted recently so we can
    /// distinguish `EXPIRED` (was cached, TTL elapsed) from `MISS`
    /// (never seen before). Backed by moka with TTL to prevent unbounded
    /// growth.
    pub(super) seen_keys: moka::future::Cache<String, ()>,
    /// Keys currently being updated (STALE/UPDATING support).
    updating_keys: Arc<dashmap::DashSet<String>>,
    /// Counter for periodic stale lock cleanup.
    lock_ops: Arc<std::sync::atomic::AtomicU64>,
}

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

/// Result of purging all slice-cache entries.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct SliceCachePurgeResult {
    pub removed_entries: u64,
    pub freed_bytes: u64,
}

impl Clone for SliceCache {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            client: self.client.clone(),
            backend: Arc::clone(&self.backend),
            locks: Arc::clone(&self.locks),
            meta: Arc::clone(&self.meta),
            seen_keys: self.seen_keys.clone(),
            updating_keys: Arc::clone(&self.updating_keys),
            lock_ops: Arc::clone(&self.lock_ops),
        }
    }
}

struct UpdatingKeyGuard {
    updating_keys: Arc<dashmap::DashSet<String>>,
    key: String,
}

impl UpdatingKeyGuard {
    const fn new(updating_keys: Arc<dashmap::DashSet<String>>, key: String) -> Self {
        Self { updating_keys, key }
    }
}

impl Drop for UpdatingKeyGuard {
    fn drop(&mut self) {
        self.updating_keys.remove(&self.key);
    }
}

#[allow(clippy::cast_precision_loss)]
fn ratio_u64(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

impl SliceCache {
    fn aligned_slice_request_range(
        slice_index: u64,
        slice_size: usize,
    ) -> Result<(u64, u64), anyhow::Error> {
        let slice_size = slice_size as u64;
        let range_start = slice_index
            .checked_mul(slice_size)
            .ok_or_else(|| anyhow::anyhow!("Slice start overflow for index {slice_index}"))?;
        let range_end = range_start
            .checked_add(slice_size.saturating_sub(1))
            .ok_or_else(|| anyhow::anyhow!("Slice end overflow for index {slice_index}"))?;
        Ok((range_start, range_end))
    }

    fn fetched_slice_from_meta(
        total_size: u64,
        meta: Option<&CachedResourceMeta>,
        data: Bytes,
        status: CacheStatus,
    ) -> FetchedSlice {
        FetchedSlice {
            slice: CachedSlice {
                total_size,
                content_type: meta.and_then(|meta| meta.content_type.clone()),
                etag: meta.and_then(|meta| meta.etag.clone()),
                last_modified: meta.and_then(|meta| meta.last_modified.clone()),
                data,
            },
            status,
        }
    }

    fn spawn_slice_revalidation(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        slice_index: u64,
        total_size: u64,
    ) {
        let cache = self.clone();
        let url = url.to_string();
        let provider_headers = provider_headers.clone();

        tokio::spawn(async move {
            if let Err(error) = cache
                .refresh_stale_slice(&url, &provider_headers, slice_index, total_size)
                .await
            {
                tracing::debug!(
                    url = %url,
                    slice_index,
                    error = %error,
                    "Background slice revalidation failed"
                );
            }
        });
    }

    async fn refresh_stale_slice(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        slice_index: u64,
        total_size: u64,
    ) -> Result<(), anyhow::Error> {
        let key = Self::compute_cache_key(url, provider_headers, slice_index);
        let _updating_guard = UpdatingKeyGuard::new(self.updating_keys.clone(), key.clone());

        let lock = self
            .locks
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        let mut should_send_conditional = false;

        if let Some(entry) = self.backend.get(&key).await {
            if !entry.is_expired() {
                return Ok(());
            }
            should_send_conditional = true;
        }

        let (range_start, range_end) =
            Self::aligned_slice_request_range(slice_index, self.config.slice_size)?;
        let range_header = format!("bytes={range_start}-{range_end}");

        let mut request = self.client.get(url);
        request = apply_provider_headers(request, url, provider_headers)?;
        request = request.header("Range", &range_header);

        let mk = Self::meta_key(url, provider_headers);
        if should_send_conditional {
            if let Some(meta_ref) = self.meta.get(&mk) {
                if let Some(ref etag) = meta_ref.etag {
                    request = request.header("If-None-Match", etag.as_str());
                }
                if let Some(ref lm) = meta_ref.last_modified {
                    request = request.header("If-Modified-Since", lm.as_str());
                }
            }
        }

        let resp = match send_with_redirect_validation(&self.client, request).await {
            Ok(proxy_response) => proxy_response.response,
            Err(e) => return Err(anyhow::anyhow!("Slice fetch failed: {e}")),
        };

        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            let _ = resp.bytes().await;

            if let Some(existing) = self.backend.get(&key).await {
                let refreshed = StoredEntry::new(existing.data.clone(), self.config.segment_ttl);
                let _ = self.backend.put(&key, refreshed).await;
                return Ok(());
            }

            let mut retry_request = self.client.get(url);
            retry_request = apply_provider_headers(retry_request, url, provider_headers)?;
            retry_request = retry_request.header("Range", &range_header);
            let retry_resp = match send_with_redirect_validation(&self.client, retry_request).await
            {
                Ok(proxy_response) => proxy_response.response,
                Err(e) => return Err(anyhow::anyhow!("Slice re-fetch failed after 304: {e}")),
            };
            self.process_slice_response(
                retry_resp,
                url,
                provider_headers,
                &key,
                slice_index,
                Some(total_size),
                range_start,
                CacheStatus::Miss,
                None,
            )
            .await
            .map(|_| ())?;
            return Ok(());
        }

        self.process_slice_response(
            resp,
            url,
            provider_headers,
            &key,
            slice_index,
            Some(total_size),
            range_start,
            CacheStatus::Miss,
            None,
        )
        .await
        .map(|_| ())?;
        Ok(())
    }

    /// Create a new `SliceCache` with the given configuration.
    ///
    /// This constructor works synchronously for the default in-memory
    /// backend.  For the file backend, use [`try_new`](Self::try_new)
    /// which creates the cache directory asynchronously.
    ///
    /// # Panics
    ///
    /// Panics if `config.backend` is [`CacheBackendConfig::File`] -- use
    /// [`try_new`](Self::try_new) instead.
    #[must_use]
    pub fn new(config: SliceCacheConfig) -> Self {
        let client =
            crate::build_proxy_http_client().expect("proxy HTTP client must build for SliceCache");
        Self::new_with_client(config, client)
    }

    /// Create a new in-memory `SliceCache` with an explicit outbound HTTP client.
    ///
    /// This is the preferred constructor for runtime code so the proxy/cache stack
    /// shares one injected client instance.
    #[must_use]
    pub fn new_with_client(config: SliceCacheConfig, client: reqwest::Client) -> Self {
        assert!(
            matches!(config.backend, CacheBackendConfig::Memory),
            "SliceCache::new() only supports the Memory backend; \
             use SliceCache::try_new() for File backend"
        );
        let backend = CacheBackend::Memory(super::backend::memory::MemoryBackend::new(
            config.max_cache_size,
            Duration::from_hours(1),
        ));
        Self::with_backend(config, client, backend)
    }

    /// Create a new `SliceCache`, initializing the backend from the
    /// configuration.  This is the async variant that supports both
    /// memory and file backends.
    pub async fn try_new(config: SliceCacheConfig) -> anyhow::Result<Self> {
        let client = crate::build_proxy_http_client()?;
        Self::try_new_with_client(config, client).await
    }

    /// Create a new `SliceCache` with an explicit outbound HTTP client.
    pub async fn try_new_with_client(
        config: SliceCacheConfig,
        client: reqwest::Client,
    ) -> anyhow::Result<Self> {
        let backend = match &config.backend {
            CacheBackendConfig::Memory => {
                CacheBackend::Memory(super::backend::memory::MemoryBackend::new(
                    config.max_cache_size,
                    Duration::from_hours(1),
                ))
            }
            CacheBackendConfig::File {
                cache_dir,
                dir_levels,
            } => {
                let fb =
                    super::backend::file::FileBackend::new(cache_dir.clone(), *dir_levels).await?;
                CacheBackend::File(fb)
            }
        };
        Ok(Self::with_backend(config, client, backend))
    }

    /// Internal helper: assemble a `SliceCache` from an already-created
    /// backend.
    fn with_backend(
        config: SliceCacheConfig,
        client: reqwest::Client,
        backend: CacheBackend,
    ) -> Self {
        // seen_keys uses a moka cache with a TTL slightly longer than
        // the main cache's time_to_idle so that "was ever seen" info
        // outlives the data entry but does not grow unbounded.
        let seen_keys = moka::future::Cache::builder()
            .max_capacity(1_000_000)
            .time_to_idle(Duration::from_hours(2))
            .build();

        Self {
            config,
            client,
            backend: Arc::new(backend),
            locks: Arc::new(dashmap::DashMap::new()),
            meta: Arc::new(dashmap::DashMap::new()),
            seen_keys,
            updating_keys: Arc::new(dashmap::DashSet::new()),
            lock_ops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Access the configuration.
    #[must_use]
    pub const fn config(&self) -> &SliceCacheConfig {
        &self.config
    }

    /// Get a shared reference to the backend (for lifecycle manager).
    #[must_use]
    pub const fn backend(&self) -> &Arc<CacheBackend> {
        &self.backend
    }

    /// Access the injected outbound HTTP client.
    #[must_use]
    pub const fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Return an operational snapshot of the cache backend and runtime state.
    #[must_use]
    pub fn stats(&self) -> SliceCacheStats {
        let (backend, file_cache_dir) = match &self.config.backend {
            CacheBackendConfig::Memory => ("memory".to_string(), None),
            CacheBackendConfig::File { cache_dir, .. } => {
                ("file".to_string(), Some(cache_dir.display().to_string()))
            }
        };
        let current_size_bytes = self.backend.current_size();
        let usage_ratio = ratio_u64(current_size_bytes, self.config.max_cache_size);

        SliceCacheStats {
            engine_enabled: self.config.enabled,
            backend,
            file_cache_dir,
            slice_size: u64::try_from(self.config.slice_size).unwrap_or(u64::MAX),
            max_cache_size: self.config.max_cache_size,
            segment_ttl_secs: self.config.segment_ttl.as_secs(),
            stale_max_age_secs: self.config.stale_max_age.as_secs(),
            stale_while_revalidate: self.config.stale_while_revalidate,
            eviction_interval_secs: self.config.eviction_interval.as_secs(),
            watermark_ratio: self.config.watermark_ratio,
            current_size_bytes,
            entry_count: self.backend.entry_count(),
            metadata_entries: u64::try_from(self.meta.len()).unwrap_or(u64::MAX),
            updating_entries: u64::try_from(self.updating_keys.len()).unwrap_or(u64::MAX),
            lock_count: u64::try_from(self.locks.len()).unwrap_or(u64::MAX),
            usage_ratio,
        }
    }

    /// Remove every cached body/slice entry and clear in-memory metadata.
    pub async fn purge_all(&self) -> SliceCachePurgeResult {
        let freed_bytes = self.backend.current_size();
        let keys = self.backend.keys().await;
        let removed_entries = u64::try_from(keys.len()).unwrap_or(u64::MAX);
        for key in keys {
            self.backend.remove(&key).await;
        }
        self.meta.clear();
        self.updating_keys.clear();
        self.locks.clear();
        self.seen_keys.invalidate_all();

        SliceCachePurgeResult {
            removed_entries,
            freed_bytes,
        }
    }

    /// Remove expired cached entries using the same backend primitive as the lifecycle manager.
    pub async fn evict_expired_entries(&self) -> u64 {
        self.backend.evict_expired().await
    }

    // Cache key helpers

    /// Compute a deterministic cache key from URL, sorted provider headers,
    /// and slice index.
    #[must_use]
    pub fn compute_cache_key(
        url: &str,
        provider_headers: &HashMap<String, String>,
        slice_index: u64,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(url.as_bytes());
        hasher.update(b"\0");

        let mut sorted: Vec<(&String, &String)> = provider_headers.iter().collect();
        sorted.sort_by_key(|(k, _)| *k);
        for (k, v) in sorted {
            hasher.update(k.as_bytes());
            hasher.update(b"=");
            hasher.update(v.as_bytes());
            hasher.update(b"\n");
        }

        hasher.update(b"\0");
        hasher.update(slice_index.to_le_bytes());

        hex::encode(hasher.finalize())
    }

    /// Compute the key used to store per-resource metadata.
    pub(super) fn meta_key(url: &str, provider_headers: &HashMap<String, String>) -> String {
        let mut hasher = Sha256::new();
        hasher.update(url.as_bytes());
        hasher.update(b"\0");

        let mut sorted: Vec<(&String, &String)> = provider_headers.iter().collect();
        sorted.sort_by_key(|(k, _)| *k);
        for (k, v) in sorted {
            hasher.update(k.as_bytes());
            hasher.update(b"=");
            hasher.update(v.as_bytes());
            hasher.update(b"\n");
        }

        hasher.update(b"\0meta");
        hex::encode(hasher.finalize())
    }

    fn cached_meta_and_total_size(
        &self,
        meta_key: &str,
        known_total_size: Option<u64>,
    ) -> (Option<CachedResourceMeta>, Option<u64>) {
        let cached_meta = self.meta.get(meta_key).map(|m| m.clone());
        let effective_total_size = known_total_size.or_else(|| {
            cached_meta
                .as_ref()
                .and_then(|meta| meta.total_size)
                .filter(|total_size| *total_size > 0)
        });
        (cached_meta, effective_total_size)
    }

    // Metadata access

    /// Retrieve the stored metadata for a resource, if any.
    /// Updates `last_accessed` on read so that LRU eviction in
    /// `cleanup_stale_meta` works correctly.
    #[allow(clippy::unused_async)]
    pub async fn get_resource_meta(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
    ) -> Option<CachedResourceMeta> {
        let mk = Self::meta_key(url, provider_headers);
        if let Some(mut entry) = self.meta.get_mut(&mk) {
            entry.last_accessed = std::time::SystemTime::now();
            Some(entry.clone())
        } else {
            None
        }
    }

    pub(super) fn put_resource_meta(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        meta: CachedResourceMeta,
    ) {
        let mk = Self::meta_key(url, provider_headers);
        self.meta.insert(mk, meta);
    }

    pub(super) fn resource_meta_lock(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
    ) -> SliceLock {
        let key = format!("meta:{}", Self::meta_key(url, provider_headers));
        self.locks
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn cached_head_headers(
        meta: &CachedResourceMeta,
        range_header: Option<&str>,
        metadata_ttl: Duration,
    ) -> Option<(reqwest::StatusCode, reqwest::header::HeaderMap)> {
        if std::time::SystemTime::now()
            .duration_since(meta.validated_at)
            .unwrap_or(Duration::ZERO)
            > metadata_ttl
        {
            return None;
        }

        let total_size = meta.total_size?;
        let mut headers = reqwest::header::HeaderMap::new();
        if meta.supports_ranges {
            headers.insert(
                reqwest::header::ACCEPT_RANGES,
                reqwest::header::HeaderValue::from_static("bytes"),
            );
        }
        if let Some(content_type) = meta.content_type.as_ref() {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(content_type) {
                headers.insert(reqwest::header::CONTENT_TYPE, value);
            }
        }
        if let Some(etag) = meta.etag.as_ref() {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(etag) {
                headers.insert(reqwest::header::ETAG, value);
            }
        }
        if let Some(last_modified) = meta.last_modified.as_ref() {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(last_modified) {
                headers.insert(reqwest::header::LAST_MODIFIED, value);
            }
        }

        if let Some(range) = range_header {
            if !meta.supports_ranges {
                return None;
            }
            let Ok((start, end)) = parse_range_header(range, total_size) else {
                return None;
            };
            if let Ok(value) =
                reqwest::header::HeaderValue::from_str(&format!("bytes {start}-{end}/{total_size}"))
            {
                headers.insert(reqwest::header::CONTENT_RANGE, value);
            }
            if let Ok(value) =
                reqwest::header::HeaderValue::from_str(&(end - start + 1).to_string())
            {
                headers.insert(reqwest::header::CONTENT_LENGTH, value);
            }
            Some((reqwest::StatusCode::PARTIAL_CONTENT, headers))
        } else {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(&total_size.to_string()) {
                headers.insert(reqwest::header::CONTENT_LENGTH, value);
            }
            Some((reqwest::StatusCode::OK, headers))
        }
    }

    pub(super) async fn get_or_fetch_head_resource_with_control(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        range_header: Option<&str>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<HeadResourceResult, anyhow::Error> {
        if let Some(meta) = self.get_resource_meta(url, provider_headers).await {
            if let Some((status, headers)) =
                Self::cached_head_headers(&meta, range_header, self.config.segment_ttl)
            {
                return Ok(HeadResourceResult {
                    status,
                    headers,
                    cache_status: CacheStatus::Hit,
                });
            }
        }

        let lock = self.resource_meta_lock(url, provider_headers);
        let _guard = if let Some(control) = request_control {
            let cancellation = control.cancellation_token();
            tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(anyhow::anyhow!(
                        "Request cancelled while waiting for HEAD metadata cache lock",
                    ));
                }
                guard = lock.lock() => guard,
            }
        } else {
            lock.lock().await
        };

        if let Some(meta) = self.get_resource_meta(url, provider_headers).await {
            if let Some((status, headers)) =
                Self::cached_head_headers(&meta, range_header, self.config.segment_ttl)
            {
                return Ok(HeadResourceResult {
                    status,
                    headers,
                    cache_status: CacheStatus::Hit,
                });
            }
        }

        let mut request = self.client.head(url);
        request = apply_provider_headers(request, url, provider_headers)?;
        if let Some(range) = range_header {
            request = request.header(reqwest::header::RANGE, range);
        }

        let resp = crate::send_head_with_redirect_validation_with_control(
            &self.client,
            request,
            request_control,
        )
        .await
        .map_err(|e| anyhow::anyhow!("HEAD metadata request failed: {e}"))?
        .response;
        let status = resp.status();
        let headers = resp.headers().clone();

        if status.is_success() {
            let content_range_total_size = headers
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| parse_content_range(value).ok())
                .and_then(|parsed| parsed.complete_length);
            let accepts_ranges = headers
                .get(reqwest::header::ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.eq_ignore_ascii_case("bytes"));
            let supports_ranges = content_range_total_size.is_some() || accepts_ranges;
            let total_size = content_range_total_size.or_else(|| {
                headers
                    .get(reqwest::header::CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
            });
            self.put_resource_meta(
                url,
                provider_headers,
                CachedResourceMeta {
                    etag: headers
                        .get(reqwest::header::ETAG)
                        .and_then(|v| v.to_str().ok())
                        .map(ToString::to_string),
                    last_modified: headers
                        .get(reqwest::header::LAST_MODIFIED)
                        .and_then(|v| v.to_str().ok())
                        .map(ToString::to_string),
                    total_size,
                    supports_ranges,
                    content_type: headers
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(ToString::to_string),
                    validated_at: std::time::SystemTime::now(),
                    last_accessed: std::time::SystemTime::now(),
                },
            );
        }

        Ok(HeadResourceResult {
            status,
            headers,
            cache_status: CacheStatus::Miss,
        })
    }

    // Slice fetch

    /// Get or fetch a single aligned slice.
    ///
    /// If the slice is already in cache (and not expired), returns it
    /// immediately with [`CacheStatus::Hit`].  If the entry is expired
    /// but within the stale window (`stale_while_revalidate`), returns
    /// the stale data with [`CacheStatus::Stale`] or
    /// [`CacheStatus::Updating`].  Otherwise, acquires a per-key lock,
    /// double-checks the cache, and if still missing, fetches from
    /// upstream.
    ///
    /// The upstream response's Content-Range is validated against the
    /// requested range (like nginx's header filter at line 166:
    /// `if (cr.start != ctx->start || cr.end != end)`).
    ///
    /// ETag consistency is validated against the stored resource metadata.
    /// Conditional requests (If-None-Match, If-Modified-Since) are sent
    /// when stored metadata is available.
    pub async fn get_or_fetch_slice(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        slice_index: u64,
        total_size: u64,
    ) -> Result<(Bytes, CacheStatus), anyhow::Error> {
        self.get_or_fetch_slice_with_control(url, provider_headers, slice_index, total_size, None)
            .await
    }

    /// Get or fetch a single aligned slice with cooperative execution control.
    pub async fn get_or_fetch_slice_with_control(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        slice_index: u64,
        total_size: u64,
        request_control: Option<&ExecutionControl>,
    ) -> Result<(Bytes, CacheStatus), anyhow::Error> {
        match self
            .get_or_fetch_slice_result_with_control(
                url,
                provider_headers,
                slice_index,
                Some(total_size),
                request_control,
                false,
            )
            .await?
        {
            SliceFetchResult::Slice(fetched) => Ok((fetched.slice.data, fetched.status)),
            SliceFetchResult::Bypass(resp) => Err(anyhow::anyhow!(
                "Upstream returned {} for slice {} (expected 206 Partial Content)",
                resp.status(),
                slice_index
            )),
        }
    }

    pub(super) async fn get_or_fetch_slice_or_bypass_with_control(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        slice_index: u64,
        known_total_size: Option<u64>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<SliceFetchResult, anyhow::Error> {
        self.get_or_fetch_slice_result_with_control(
            url,
            provider_headers,
            slice_index,
            known_total_size,
            request_control,
            true,
        )
        .await
    }

    async fn get_or_fetch_slice_result_with_control(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        slice_index: u64,
        known_total_size: Option<u64>,
        request_control: Option<&ExecutionControl>,
        bypass_on_non_partial: bool,
    ) -> Result<SliceFetchResult, anyhow::Error> {
        let key = Self::compute_cache_key(url, provider_headers, slice_index);
        let meta_key = Self::meta_key(url, provider_headers);
        let (mut cached_meta, mut effective_total_size) =
            self.cached_meta_and_total_size(&meta_key, known_total_size);

        // Fast path: check cache without locking.
        if let Some(entry) = self.backend.get(&key).await {
            if !entry.is_expired() {
                if let Some(total_size) = effective_total_size {
                    return Ok(SliceFetchResult::Slice(Self::fetched_slice_from_meta(
                        total_size,
                        cached_meta.as_ref(),
                        entry.data,
                        CacheStatus::Hit,
                    )));
                }
            }
            // Check stale window for stale-while-revalidate.
            if self.config.stale_while_revalidate
                && entry.is_stale(self.config.stale_max_age)
                && effective_total_size.is_some()
            {
                let Some(total_size) = effective_total_size else {
                    unreachable!("effective_total_size.is_some() was checked above");
                };
                // Mark as updating so subsequent requests know a refresh
                // is expected.  `DashSet::insert` returns true if the key
                // was newly inserted (i.e., we are the first stale request).
                if self.updating_keys.insert(key.clone()) {
                    // Newly inserted -- we are the first stale request.
                    self.spawn_slice_revalidation(url, provider_headers, slice_index, total_size);
                    return Ok(SliceFetchResult::Slice(Self::fetched_slice_from_meta(
                        total_size,
                        cached_meta.as_ref(),
                        entry.data,
                        CacheStatus::Stale,
                    )));
                }
                // Already present -- another request is updating this key.
                return Ok(SliceFetchResult::Slice(Self::fetched_slice_from_meta(
                    total_size,
                    cached_meta.as_ref(),
                    entry.data,
                    CacheStatus::Updating,
                )));
            }
            // Expired beyond stale window -- fall through to re-fetch
            // under the lock. Do NOT remove the entry here; it may be
            // needed for conditional request (304 Not Modified) handling.
        }

        // Acquire the per-key lock cooperatively.
        // Proxy requests should only stop for caller cancellation, not for a
        // proxy-local timeout budget.
        let lock = self
            .locks
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = if let Some(control) = request_control {
            let cancellation = control.cancellation_token();
            tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(anyhow::anyhow!(
                        "Request cancelled while waiting for slice cache lock {slice_index}",
                    ));
                }
                guard = lock.lock() => guard,
            }
        } else {
            lock.lock().await
        };

        let mut should_send_conditional = false;
        let mut fetch_status = CacheStatus::Miss;

        (cached_meta, effective_total_size) =
            self.cached_meta_and_total_size(&meta_key, known_total_size);

        // Double-check after acquiring lock.
        if let Some(entry) = self.backend.get(&key).await {
            if !entry.is_expired() {
                // Another task may have completed a re-fetch while we were
                // waiting for the lock.
                if let Some(total_size) = effective_total_size {
                    self.updating_keys.remove(&key);
                    return Ok(SliceFetchResult::Slice(Self::fetched_slice_from_meta(
                        total_size,
                        cached_meta.as_ref(),
                        entry.data,
                        CacheStatus::Hit,
                    )));
                }
            }
            should_send_conditional = true;
            fetch_status = CacheStatus::Expired;
            // Still expired -- check stale once more for concurrent stale
            // serving case, then proceed with re-fetch. Keep the entry
            // in the backend for now (conditional request may use it).
            if self.config.stale_while_revalidate && entry.is_stale(self.config.stale_max_age) {
                // We hold the lock and will do the re-fetch below, so
                // let stale requests continue being served.
            }
            // Do NOT remove here; the entry is still needed for
            // conditional request (304 Not Modified) support. It will
            // be overwritten by a successful re-fetch or naturally
            // evicted by the lifecycle manager.
        }

        // Build the upstream request.
        let (range_start, range_end) =
            Self::aligned_slice_request_range(slice_index, self.config.slice_size)?;
        let range_header = format!("bytes={range_start}-{range_end}");

        let mut request = self.client.get(url);
        request = apply_provider_headers(request, url, provider_headers)?;
        request = request.header("Range", &range_header);

        // Conditional request headers are only valid when revalidating an
        // existing slice entry. Sending validators on a cold miss can trigger
        // some origins to ignore the Range header and return a full-body 200.
        let mk = Self::meta_key(url, provider_headers);
        if should_send_conditional {
            if let Some(meta_ref) = self.meta.get(&mk) {
                if let Some(ref etag) = meta_ref.etag {
                    request = request.header("If-None-Match", etag.as_str());
                }
                if let Some(ref lm) = meta_ref.last_modified {
                    request = request.header("If-Modified-Since", lm.as_str());
                }
            }
        }

        let resp = match send_with_redirect_validation_with_control(
            &self.client,
            request,
            request_control,
        )
        .await
        {
            Ok(proxy_response) => proxy_response.response,
            Err(e) => {
                // Clean up updating_keys on send failure so the key is not
                // permanently stuck in "updating" state.
                self.updating_keys.remove(&key);
                return Err(anyhow::anyhow!("Slice fetch failed: {e}"));
            }
        };

        // Handle 304 Not Modified: refresh the TTL and return Revalidated.
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            // Consume the (empty) body to release the connection.
            let _ =
                run_with_proxy_cancellation("slice cache 304 drain", request_control, resp.bytes())
                    .await;

            // Refresh the entry's TTL by re-inserting it.
            if let Some(existing) = self.backend.get(&key).await {
                if let Some(total_size) = effective_total_size {
                    let refreshed =
                        StoredEntry::new(existing.data.clone(), self.config.segment_ttl);
                    let _ = self.backend.put(&key, refreshed).await;
                    self.updating_keys.remove(&key);
                    return Ok(SliceFetchResult::Slice(Self::fetched_slice_from_meta(
                        total_size,
                        cached_meta.as_ref(),
                        existing.data,
                        CacheStatus::Revalidated,
                    )));
                }
            }
            // Entry was evicted between the conditional request and now --
            // fall through to a full re-fetch.  This is an unlikely edge
            // case; we rebuild the request without conditional headers.
            let mut request2 = self.client.get(url);
            request2 = apply_provider_headers(request2, url, provider_headers)?;
            request2 = request2.header("Range", &range_header);
            let resp2 = match send_with_redirect_validation_with_control(
                &self.client,
                request2,
                request_control,
            )
            .await
            {
                Ok(proxy_response) => proxy_response.response,
                Err(e) => {
                    self.updating_keys.remove(&key);
                    return Err(anyhow::anyhow!("Slice re-fetch failed after 304: {e}"));
                }
            };
            if bypass_on_non_partial && resp2.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                self.updating_keys.remove(&key);
                return Ok(SliceFetchResult::Bypass(resp2));
            }
            let result = self
                .process_slice_response(
                    resp2,
                    url,
                    provider_headers,
                    &key,
                    slice_index,
                    effective_total_size,
                    range_start,
                    fetch_status,
                    request_control,
                )
                .await;
            // Always clean up updating_keys (idempotent remove).
            self.updating_keys.remove(&key);
            return result.map(SliceFetchResult::Slice);
        }

        if bypass_on_non_partial && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            self.updating_keys.remove(&key);
            return Ok(SliceFetchResult::Bypass(resp));
        }

        let result = self
            .process_slice_response(
                resp,
                url,
                provider_headers,
                &key,
                slice_index,
                effective_total_size,
                range_start,
                fetch_status,
                request_control,
            )
            .await;
        // Always clean up updating_keys (idempotent). Prevents the key from
        // being stuck in "updating" state if process_slice_response errs.
        self.updating_keys.remove(&key);
        result.map(SliceFetchResult::Slice)
    }

    /// Process a successful (non-304) slice response: validate, store, and
    /// return the data.
    #[allow(clippy::too_many_arguments)]
    async fn process_slice_response(
        &self,
        resp: reqwest::Response,
        url: &str,
        provider_headers: &HashMap<String, String>,
        key: &str,
        slice_index: u64,
        known_total_size: Option<u64>,
        range_start: u64,
        cache_status: CacheStatus,
        request_control: Option<&ExecutionControl>,
    ) -> Result<FetchedSlice, anyhow::Error> {
        let requested_range_end_exclusive = range_start
            .checked_add(self.config.slice_size as u64)
            .ok_or_else(|| anyhow::anyhow!("Slice request end overflow for slice {slice_index}"))?;

        if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(anyhow::anyhow!(
                "Upstream returned {} for slice {} (expected 206 Partial Content)",
                resp.status(),
                slice_index
            ));
        }

        // Validate Content-Range response header (nginx header filter pattern).
        let parsed_content_range = if let Some(cr_value) = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
        {
            let cr = parse_content_range(cr_value)?;
            if cr.start != range_start
                || cr.end > requested_range_end_exclusive
                || cr.end <= cr.start
            {
                return Err(anyhow::anyhow!(
                    "Content-Range mismatch: got {}-{}, expected {}-{} \
                     (nginx slice header filter validation)",
                    cr.start,
                    cr.end,
                    range_start,
                    requested_range_end_exclusive,
                ));
            }
            if cr.end < requested_range_end_exclusive {
                let complete_length = cr.complete_length.or(known_total_size);
                if complete_length != Some(cr.end) {
                    return Err(anyhow::anyhow!(
                        "Short slice response is only valid at resource end: got {}-{}, requested {}-{}",
                        cr.start,
                        cr.end,
                        range_start,
                        requested_range_end_exclusive,
                    ));
                }
            }
            match (known_total_size, cr.complete_length) {
                (Some(expected), Some(complete_length)) if complete_length != expected => {
                    return Err(anyhow::anyhow!(
                        "Content-Range total mismatch: got {complete_length}, expected {expected}"
                    ));
                }
                _ => {}
            }
            cr
        } else {
            return Err(anyhow::anyhow!(
                "Upstream returned 206 Partial Content without Content-Range header; \
                 cannot validate slice boundaries"
            ));
        };
        let total_size = parsed_content_range
            .complete_length
            .or(known_total_size)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Slice response did not include a complete resource length and no known total size was available"
                )
            })?;

        // Extract headers for metadata before consuming body.
        let resp_etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string);
        let resp_last_modified = resp
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string);
        let resp_content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string);

        // Early ETag consistency check BEFORE reading the body (nginx header
        // filter pattern).  This avoids reading a full 2 MiB slice body only
        // to discard it on mismatch.
        // IMPORTANT: we must not hold a DashMap `Ref` across the call to
        // `invalidate_resource`, which needs a write lock on the same shard.
        // Clone the existing ETag out of the DashMap first to avoid deadlock.
        let mk = Self::meta_key(url, provider_headers);
        let existing_etag_cloned = self.meta.get(&mk).and_then(|m| m.etag.clone());

        if let Some(existing_etag) = &existing_etag_cloned {
            if let Some(new_etag) = &resp_etag {
                if existing_etag != new_etag {
                    // Drain the body to release the connection cleanly for
                    // reqwest's connection pool (dropping an unconsumed
                    // Response can block while the client drains it anyway).
                    let _ = run_with_proxy_cancellation(
                        "slice cache etag mismatch drain",
                        request_control,
                        resp.bytes(),
                    )
                    .await;
                    // ETag mismatch: resource was modified between slices.
                    // Invalidate all cached slices for this resource.
                    self.invalidate_resource(url, provider_headers, total_size)
                        .await;
                    return Err(anyhow::anyhow!(
                        "ETag mismatch: resource modified between slice fetches \
                         (expected {existing_etag}, got {new_etag})"
                    ));
                }
            }
        }

        // Read body now that ETag is validated.  Reading eagerly (rather than
        // on-demand) ensures the connection is properly closed before any
        // error-path returns.
        let data =
            run_with_proxy_cancellation("slice cache body read", request_control, resp.bytes())
                .await?
                .map_err(|e| anyhow::anyhow!("Failed to read slice body: {e}"))?;

        let expected_len = usize::try_from(
            parsed_content_range
                .end
                .saturating_sub(parsed_content_range.start),
        )
        .map_err(|_| anyhow::anyhow!("Slice length overflow for slice {slice_index}"))?;
        if data.len() != expected_len {
            return Err(anyhow::anyhow!(
                "Slice body length mismatch: got {}, expected {} from Content-Range",
                data.len(),
                expected_len
            ));
        }

        self.meta.insert(
            mk,
            CachedResourceMeta {
                etag: resp_etag.clone(),
                last_modified: resp_last_modified.clone(),
                total_size: Some(total_size),
                supports_ranges: true,
                content_type: resp_content_type.clone(),
                validated_at: std::time::SystemTime::now(),
                last_accessed: std::time::SystemTime::now(),
            },
        );

        // Insert into cache with TTL.
        let entry = StoredEntry::new(data.clone(), self.config.segment_ttl);
        self.backend
            .put(key, entry)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to store slice in cache: {e}"))?;
        self.seen_keys.insert(key.to_string(), ()).await;

        // Clear updating flag now that fresh data is stored.
        self.updating_keys.remove(key);

        // Periodically clean up stale per-key locks to prevent unbounded growth.
        self.maybe_cleanup_locks();

        Ok(FetchedSlice {
            slice: CachedSlice {
                total_size,
                content_type: resp_content_type,
                etag: resp_etag,
                last_modified: resp_last_modified,
                data,
            },
            status: cache_status,
        })
    }

    /// Invalidate all cached slices for a given resource.
    pub(super) async fn invalidate_resource(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        total_size: u64,
    ) {
        let ss = self.config.slice_size as u64;
        let num_slices = total_size.div_ceil(ss);
        for i in 0..num_slices {
            let key = Self::compute_cache_key(url, provider_headers, i);
            self.backend.remove(&key).await;
        }
        // Also remove metadata so next fetch establishes a new ETag.
        let mk = Self::meta_key(url, provider_headers);
        self.meta.remove(&mk);
    }

    // Lock cleanup (L3 fix)

    /// Periodically remove stale per-key locks that are not currently held
    /// by any task.  A lock is considered stale when the only remaining
    /// `Arc` reference is the one stored in the `DashMap` itself
    /// (`strong_count == 1`).
    fn maybe_cleanup_locks(&self) {
        let count = self
            .lock_ops
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !count.is_multiple_of(LOCK_CLEANUP_INTERVAL) {
            return;
        }
        self.cleanup_stale_locks();
        self.cleanup_stale_meta();
    }

    /// Remove all per-key locks not currently held by any task.
    pub fn cleanup_stale_locks(&self) {
        self.locks.retain(|_key, lock| Arc::strong_count(lock) > 1);
    }

    fn cleanup_stale_meta_with_limit(&self, max_meta_entries: usize) {
        if self.meta.len() <= max_meta_entries {
            return;
        }

        let retention_target = max_meta_entries / META_RETENTION_TARGET_DIVISOR;
        let target_removals = self.meta.len().saturating_sub(retention_target);
        if target_removals == 0 {
            return;
        }

        let mut oldest_candidates = BinaryHeap::with_capacity(target_removals);
        for entry in self.meta.iter() {
            let candidate = MetaEvictionCandidate {
                key: entry.key().clone(),
                last_accessed: entry.value().last_accessed,
            };

            if oldest_candidates.len() < target_removals {
                oldest_candidates.push(candidate);
                continue;
            }

            let Some(newest_eviction_candidate) = oldest_candidates.peek() else {
                continue;
            };
            if candidate < *newest_eviction_candidate {
                oldest_candidates.pop();
                oldest_candidates.push(candidate);
            }
        }

        for candidate in oldest_candidates {
            self.meta.remove(&candidate.key);
        }
    }

    /// Remove stale metadata entries to prevent unbounded growth of the
    /// `meta` DashMap. Uses a simple size cap: when the map exceeds
    /// `MAX_META_ENTRIES`, evicts the least recently accessed entries until
    /// the map is at half capacity.
    pub fn cleanup_stale_meta(&self) {
        self.cleanup_stale_meta_with_limit(MAX_META_ENTRIES);
    }

    /// Return the current number of per-key locks (for diagnostics/testing).
    #[must_use]
    pub fn lock_count(&self) -> usize {
        self.locks.len()
    }

    /// Return the current number of metadata entries (for diagnostics/testing).
    #[must_use]
    pub fn meta_count(&self) -> usize {
        self.meta.len()
    }

    /// Return the current number of entries in `seen_keys` (for diagnostics/testing).
    ///
    /// Note: moka's `entry_count()` is eventually consistent. Call
    /// `sync_seen_keys()` first for an accurate count after recent inserts.
    #[must_use]
    pub fn seen_keys_count(&self) -> u64 {
        self.seen_keys.entry_count()
    }

    /// Run pending maintenance tasks on the seen_keys cache so that
    /// `entry_count()` reflects recent inserts.
    pub async fn sync_seen_keys(&self) {
        self.seen_keys.run_pending_tasks().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache() -> SliceCache {
        SliceCache::new(SliceCacheConfig::default())
    }

    #[test]
    fn cleanup_stale_meta_evicts_oldest_entries() {
        let cache = test_cache();
        let now = std::time::SystemTime::now();

        for index in 0_u64..6 {
            cache.meta.insert(
                format!("meta-{index}"),
                CachedResourceMeta {
                    etag: None,
                    last_modified: None,
                    total_size: None,
                    supports_ranges: false,
                    content_type: None,
                    validated_at: now,
                    last_accessed: now + Duration::from_secs(index),
                },
            );
        }

        cache.cleanup_stale_meta_with_limit(4);

        assert_eq!(cache.meta_count(), 2);
        assert!(!cache.meta.contains_key("meta-0"));
        assert!(!cache.meta.contains_key("meta-1"));
        assert!(!cache.meta.contains_key("meta-2"));
        assert!(!cache.meta.contains_key("meta-3"));
        assert!(cache.meta.contains_key("meta-4"));
        assert!(cache.meta.contains_key("meta-5"));
    }

    #[test]
    fn cleanup_stale_meta_skips_when_within_limit() {
        let cache = test_cache();
        let now = std::time::SystemTime::now();

        for index in 0..4 {
            cache.meta.insert(
                format!("meta-{index}"),
                CachedResourceMeta {
                    etag: None,
                    last_modified: None,
                    total_size: None,
                    supports_ranges: false,
                    content_type: None,
                    validated_at: now,
                    last_accessed: now,
                },
            );
        }

        cache.cleanup_stale_meta_with_limit(4);

        assert_eq!(cache.meta_count(), 4);
    }

    #[tokio::test]
    async fn stats_reports_backend_and_runtime_counters() {
        let cache = test_cache();
        cache
            .backend
            .put(
                "slice-1",
                StoredEntry::new(Bytes::from_static(b"cached"), Duration::from_mins(1)),
            )
            .await
            .expect("entry should be cached");
        cache.meta.insert(
            "meta-1".to_string(),
            CachedResourceMeta {
                etag: Some("etag".to_string()),
                last_modified: None,
                total_size: Some(6),
                supports_ranges: true,
                content_type: Some("video/mp2t".to_string()),
                validated_at: std::time::SystemTime::now(),
                last_accessed: std::time::SystemTime::now(),
            },
        );
        cache.updating_keys.insert("slice-1".to_string());
        cache
            .locks
            .insert("slice-1".to_string(), Arc::new(Mutex::new(())));

        let stats = cache.stats();

        assert!(stats.engine_enabled);
        assert_eq!(stats.backend, "memory");
        assert_eq!(stats.file_cache_dir, None);
        assert_eq!(stats.current_size_bytes, 6);
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.metadata_entries, 1);
        assert_eq!(stats.updating_entries, 1);
        assert_eq!(stats.lock_count, 1);
        assert!(stats.usage_ratio > 0.0);
    }

    #[tokio::test]
    async fn purge_all_removes_entries_and_runtime_metadata() {
        let cache = test_cache();
        cache
            .backend
            .put(
                "slice-1",
                StoredEntry::new(Bytes::from_static(b"cached"), Duration::from_mins(1)),
            )
            .await
            .expect("entry should be cached");
        cache.meta.insert(
            "meta-1".to_string(),
            CachedResourceMeta {
                etag: None,
                last_modified: None,
                total_size: None,
                supports_ranges: false,
                content_type: None,
                validated_at: std::time::SystemTime::now(),
                last_accessed: std::time::SystemTime::now(),
            },
        );
        cache.updating_keys.insert("slice-1".to_string());
        cache
            .locks
            .insert("slice-1".to_string(), Arc::new(Mutex::new(())));

        let result = cache.purge_all().await;

        assert_eq!(result.removed_entries, 1);
        assert_eq!(result.freed_bytes, 6);
        assert_eq!(cache.backend.entry_count(), 0);
        assert_eq!(cache.meta_count(), 0);
        assert!(cache.updating_keys.is_empty());
        assert!(cache.locks.is_empty());
    }

    #[tokio::test]
    async fn evict_expired_entries_removes_expired_backend_entries() {
        let cache = test_cache();
        cache
            .backend
            .put(
                "expired-slice",
                StoredEntry::new(Bytes::from_static(b"old"), Duration::ZERO),
            )
            .await
            .expect("entry should be cached");

        let removed = cache.evict_expired_entries().await;

        assert_eq!(removed, 1);
        assert_eq!(cache.backend.entry_count(), 0);
    }
}
