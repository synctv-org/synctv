#![allow(clippy::missing_errors_doc, clippy::too_many_lines)]

//! Slice cache store: backend orchestration, per-key locking, and upstream
//! slice fetch/revalidation.
//!
//! Contains [`SliceCache`], the central cache struct backed by a
//! [`CacheBackend`] (memory or file). Small data types, maintenance helpers,
//! and key hashing live in sibling modules to keep this file focused on cache
//! behavior.

use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use synctv_common::ExecutionControl;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::{
    apply_provider_headers, run_with_proxy_cancellation,
    send_with_redirect_validation_with_control_and_timeout, ProviderHeaders,
};

use super::backend::{CacheBackend, SliceCacheBackend};
use super::config::{CacheBackendConfig, SliceCacheConfig};
use super::etag::{CachedResourceMeta, StoredEntry};
use super::head::HeadFetchContext;
use super::keys::{resource_meta_key, slice_cache_key};
use super::maintenance::{ratio_u64, MetaEvictionCandidate, UpdatingKeyGuard};
use super::range::parse_content_range;
use super::status::CacheStatus;
use super::types::{
    CachedSlice, FetchedSlice, HeadResourceResult, SliceCachePurgeResult, SliceCacheStats,
    SliceFetchRequest, SliceFetchResult,
};

/// Number of lock cleanup cycles before triggering a stale-lock sweep.
const LOCK_CLEANUP_INTERVAL: u64 = 64;
const MAX_META_ENTRIES: usize = 100_000;
const META_RETENTION_TARGET_DIVISOR: usize = 2;

// SliceCache

/// Per-key Mutex to prevent thundering herd on the same slice.
type SliceLock = Arc<Mutex<()>>;

struct SliceRangeRequest {
    request: reqwest::RequestBuilder,
    range_start: u64,
}

struct SliceResponseContext<'a> {
    url: &'a str,
    provider_headers: &'a ProviderHeaders,
    key: &'a str,
    slice_index: u64,
    known_total_size: Option<u64>,
    range_start: u64,
    cache_status: CacheStatus,
    request_control: Option<&'a ExecutionControl>,
}

async fn read_exact_slice_body(
    mut resp: reqwest::Response,
    expected_len: usize,
    request_control: Option<&ExecutionControl>,
) -> Result<Bytes, anyhow::Error> {
    let mut data = Vec::with_capacity(expected_len);
    while let Some(chunk) =
        run_with_proxy_cancellation("slice cache body read", request_control, resp.chunk())
            .await?
            .map_err(|e| anyhow::anyhow!("Failed to read slice body: {e}"))?
    {
        let next_len = data
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow::anyhow!("Slice body length overflow"))?;
        if next_len > expected_len {
            return Err(anyhow::anyhow!(
                "Slice body length exceeded Content-Range: got at least {next_len}, expected {expected_len}"
            ));
        }
        data.extend_from_slice(&chunk);
    }

    if data.len() != expected_len {
        return Err(anyhow::anyhow!(
            "Slice body length mismatch: got {}, expected {} from Content-Range",
            data.len(),
            expected_len
        ));
    }

    Ok(Bytes::from(data))
}

/// A slice cache backed by a [`CacheBackend`] (memory or file).
///
/// Each cached entry is a [`StoredEntry`] containing the `Bytes` data plus
/// insertion metadata.  Per-key locking via a `DashMap<String, Mutex>`
/// ensures that concurrent requests for the same slice trigger at most a
/// single upstream fetch.
///
/// Resource metadata (`ETag`, Last-Modified, Content-Type) is stored in a
/// separate `DashMap` keyed by `url_hash + "meta"` to enable cross-slice
/// ETag/Last-Modified validation and conditional requests.
pub struct SliceCache {
    pub(super) config: SliceCacheConfig,
    /// SSRF policy used for cache fill and revalidation requests.
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
    /// Shared outbound HTTP client for cache fill and revalidation requests.
    client: reqwest::Client,
    /// The cache backend (memory or file).
    backend: Arc<CacheBackend>,
    /// Per-key locks to prevent thundering herd.
    locks: Arc<dashmap::DashMap<String, SliceLock>>,
    /// Per-resource metadata for `ETag` consistency validation.
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

impl Clone for SliceCache {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            ssrf_guard: self.ssrf_guard.clone(),
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

impl SliceCache {
    async fn lock_slice_key(
        &self,
        key: &str,
        wait_error: impl Into<String>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<OwnedMutexGuard<()>, anyhow::Error> {
        let lock = self
            .locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();

        if let Some(control) = request_control {
            let cancellation = control.cancellation_token();
            tokio::select! {
                () = cancellation.cancelled() => Err(anyhow::anyhow!(wait_error.into())),
                guard = lock.lock_owned() => Ok(guard),
            }
        } else {
            Ok(lock.lock_owned().await)
        }
    }

    fn build_slice_range_request(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
        slice_index: u64,
        include_conditionals: bool,
    ) -> Result<SliceRangeRequest, anyhow::Error> {
        let (range_start, range_end) =
            Self::aligned_slice_request_range(slice_index, self.config.slice_size)?;
        let range_header = format!("bytes={range_start}-{range_end}");

        let mut request = self.client.get(url);
        request = apply_provider_headers(request, url, provider_headers)?;
        request = request.header(reqwest::header::RANGE, &range_header);

        if include_conditionals {
            let meta_key = Self::meta_key(url, provider_headers);
            if let Some(meta_ref) = self.meta.get(&meta_key) {
                if let Some(ref etag) = meta_ref.etag {
                    request = request.header(reqwest::header::IF_NONE_MATCH, etag.as_str());
                }
                if let Some(ref last_modified) = meta_ref.last_modified {
                    request =
                        request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified.as_str());
                }
            }
        }

        Ok(SliceRangeRequest {
            request,
            range_start,
        })
    }

    /// Drain the (empty) body of a `304 Not Modified` response so the
    /// underlying connection can be reused, logging any drain failure.
    async fn drain_not_modified_body(
        resp: reqwest::Response,
        request_control: Option<&ExecutionControl>,
    ) {
        if let Err(error) =
            run_with_proxy_cancellation("slice cache 304 drain", request_control, resp.bytes())
                .await
        {
            tracing::debug!(%error, "failed to drain slice cache 304 response body");
        }
    }

    /// Re-insert the still-cached slice data under a fresh segment TTL after a
    /// `304 Not Modified`, returning the data on success.
    async fn refresh_cached_slice_ttl(
        &self,
        key: &str,
        data: Bytes,
    ) -> Result<(), anyhow::Error> {
        let refreshed = StoredEntry::new(data, self.config.segment_ttl);
        self.backend
            .put(key, refreshed)
            .await
            .with_context(|| format!("failed to refresh cached slice TTL for {key}"))
    }

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
        provider_headers: &ProviderHeaders,
        slice_index: u64,
        total_size: u64,
        upstream_header_timeout: Option<Duration>,
    ) {
        let cache = self.clone();
        let url = url.to_string();
        let provider_headers = provider_headers.clone();

        tokio::spawn(async move {
            if let Err(error) = cache
                .refresh_stale_slice(
                    &url,
                    &provider_headers,
                    slice_index,
                    total_size,
                    upstream_header_timeout,
                )
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
        provider_headers: &ProviderHeaders,
        slice_index: u64,
        total_size: u64,
        upstream_header_timeout: Option<Duration>,
    ) -> Result<(), anyhow::Error> {
        let key = Self::compute_cache_key(url, provider_headers, slice_index);
        let _updating_guard = UpdatingKeyGuard::new(self.updating_keys.clone(), key.clone());

        let _guard = self.lock_slice_key(&key, "", None).await?;

        let mut should_send_conditional = false;

        if let Some(entry) = self.backend.get(&key).await {
            if !entry.is_expired() {
                return Ok(());
            }
            should_send_conditional = true;
        }

        let range_request = self.build_slice_range_request(
            url,
            provider_headers,
            slice_index,
            should_send_conditional,
        )?;

        let resp = match send_with_redirect_validation_with_control_and_timeout(
            &self.client,
            range_request.request,
            &self.ssrf_guard,
            None,
            upstream_header_timeout,
        )
        .await
        {
            Ok(proxy_response) => proxy_response.response,
            Err(e) => return Err(anyhow::anyhow!("Slice fetch failed: {e}")),
        };

        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            Self::drain_not_modified_body(resp, None).await;

            if let Some(existing) = self.backend.get(&key).await {
                self.refresh_cached_slice_ttl(&key, existing.data.clone())
                    .await?;
                return Ok(());
            }

            let retry_request =
                self.build_slice_range_request(url, provider_headers, slice_index, false)?;
            let retry_resp = match send_with_redirect_validation_with_control_and_timeout(
                &self.client,
                retry_request.request,
                &self.ssrf_guard,
                None,
                upstream_header_timeout,
            )
            .await
            {
                Ok(proxy_response) => proxy_response.response,
                Err(e) => return Err(anyhow::anyhow!("Slice re-fetch failed after 304: {e}")),
            };
            self.process_slice_response(
                retry_resp,
                SliceResponseContext {
                    url,
                    provider_headers,
                    key: &key,
                    slice_index,
                    known_total_size: Some(total_size),
                    range_start: range_request.range_start,
                    cache_status: CacheStatus::Miss,
                    request_control: None,
                },
            )
            .await
            .map(|_| ())?;
            return Ok(());
        }

        self.process_slice_response(
            resp,
            SliceResponseContext {
                url,
                provider_headers,
                key: &key,
                slice_index,
                known_total_size: Some(total_size),
                range_start: range_request.range_start,
                cache_status: CacheStatus::Miss,
                request_control: None,
            },
        )
        .await
        .map(|_| ())?;
        Ok(())
    }

    /// Create a new `SliceCache` with the given configuration.
    ///
    /// This constructor works synchronously for the default in-memory backend.
    /// For the file backend, use [`try_new`](Self::try_new), which creates the
    /// cache directory asynchronously.
    pub fn new(config: SliceCacheConfig) -> anyhow::Result<Self> {
        Self::new_with_ssrf_guard(config, synctv_common::ssrf::SsrfGuard::strict_policy())
    }

    pub fn new_with_ssrf_guard(
        config: SliceCacheConfig,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> anyhow::Result<Self> {
        let client = crate::build_proxy_http_client(ssrf_guard.clone())?;
        Self::new_with_client_and_ssrf_guard(config, client, ssrf_guard)
    }

    /// Create a new in-memory `SliceCache` with an explicit outbound HTTP client.
    ///
    /// This is the preferred constructor for runtime code so the proxy/cache stack
    /// shares one injected client instance.
    pub fn new_with_client(
        config: SliceCacheConfig,
        client: reqwest::Client,
    ) -> anyhow::Result<Self> {
        Self::new_with_client_and_ssrf_guard(
            config,
            client,
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    pub fn new_with_client_and_ssrf_guard(
        config: SliceCacheConfig,
        client: reqwest::Client,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            matches!(config.backend, CacheBackendConfig::Memory),
            "SliceCache::new() supports the Memory backend; use SliceCache::try_new() for File backend"
        );
        let backend = CacheBackend::Memory(super::backend::memory::MemoryBackend::new(
            config.max_cache_size,
            Duration::from_hours(1),
        ));
        Ok(Self::with_backend(config, client, ssrf_guard, backend))
    }

    /// Create a new `SliceCache`, initializing the backend from the
    /// configuration.  This is the async variant that supports both
    /// memory and file backends.
    pub async fn try_new(config: SliceCacheConfig) -> anyhow::Result<Self> {
        Self::try_new_with_ssrf_guard(config, synctv_common::ssrf::SsrfGuard::strict_policy()).await
    }

    pub async fn try_new_with_ssrf_guard(
        config: SliceCacheConfig,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> anyhow::Result<Self> {
        let client = crate::build_proxy_http_client(ssrf_guard.clone())?;
        Self::try_new_with_client_and_ssrf_guard(config, client, ssrf_guard).await
    }

    /// Create a new `SliceCache` with an explicit outbound HTTP client.
    pub async fn try_new_with_client(
        config: SliceCacheConfig,
        client: reqwest::Client,
    ) -> anyhow::Result<Self> {
        Self::try_new_with_client_and_ssrf_guard(
            config,
            client,
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .await
    }

    pub async fn try_new_with_client_and_ssrf_guard(
        config: SliceCacheConfig,
        client: reqwest::Client,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
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
        Ok(Self::with_backend(config, client, ssrf_guard, backend))
    }

    /// Internal helper: assemble a `SliceCache` from an already-created
    /// backend.
    fn with_backend(
        config: SliceCacheConfig,
        client: reqwest::Client,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
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
            ssrf_guard,
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

    #[must_use]
    pub const fn ssrf_guard(&self) -> &synctv_common::ssrf::SsrfGuard {
        &self.ssrf_guard
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
        provider_headers: &ProviderHeaders,
        slice_index: u64,
    ) -> String {
        slice_cache_key(url, provider_headers, slice_index)
    }

    /// Compute the key used to store per-resource metadata.
    pub(super) fn meta_key(url: &str, provider_headers: &ProviderHeaders) -> String {
        resource_meta_key(url, provider_headers)
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
    #[must_use]
    pub fn get_resource_meta(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
    ) -> Option<CachedResourceMeta> {
        self.get_resource_meta_sync(url, provider_headers)
    }

    pub(super) fn get_resource_meta_sync(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
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
        provider_headers: &ProviderHeaders,
        meta: CachedResourceMeta,
    ) {
        let mk = Self::meta_key(url, provider_headers);
        self.meta.insert(mk, meta);
    }

    pub(super) fn resource_meta_lock(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
    ) -> SliceLock {
        let key = format!("meta:{}", Self::meta_key(url, provider_headers));
        self.locks
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub(super) async fn get_or_fetch_head_resource_with_control(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
        range_header: Option<&str>,
        request_control: Option<&ExecutionControl>,
        upstream_header_timeout: Option<Duration>,
    ) -> Result<HeadResourceResult, anyhow::Error> {
        super::head::get_or_fetch_head_resource_with_control(
            HeadFetchContext {
                client: &self.client,
                ssrf_guard: &self.ssrf_guard,
                segment_ttl: self.config.segment_ttl,
                meta: &self.meta,
                meta_key: Self::meta_key(url, provider_headers),
                meta_lock: self.resource_meta_lock(url, provider_headers),
            },
            url,
            provider_headers,
            range_header,
            request_control,
            upstream_header_timeout,
        )
        .await
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
    /// `ETag` consistency is validated against the stored resource metadata.
    /// Conditional requests (If-None-Match, If-Modified-Since) are sent
    /// when stored metadata is available.
    pub async fn get_or_fetch_slice(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
        slice_index: u64,
        total_size: u64,
    ) -> Result<(Bytes, CacheStatus), anyhow::Error> {
        self.get_or_fetch_slice_with_control(
            url,
            provider_headers,
            slice_index,
            total_size,
            None,
            None,
        )
        .await
    }

    /// Get or fetch a single aligned slice with cooperative execution control.
    pub async fn get_or_fetch_slice_with_control(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
        slice_index: u64,
        total_size: u64,
        request_control: Option<&ExecutionControl>,
        upstream_header_timeout: Option<Duration>,
    ) -> Result<(Bytes, CacheStatus), anyhow::Error> {
        match self
            .get_or_fetch_slice_result_with_control(SliceFetchRequest {
                url,
                provider_headers,
                slice_index,
                known_total_size: Some(total_size),
                request_control,
                upstream_header_timeout,
                bypass_on_non_partial: false,
            })
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
        provider_headers: &ProviderHeaders,
        slice_index: u64,
        known_total_size: Option<u64>,
        request_control: Option<&ExecutionControl>,
        upstream_header_timeout: Option<Duration>,
    ) -> Result<SliceFetchResult, anyhow::Error> {
        self.get_or_fetch_slice_result_with_control(SliceFetchRequest {
            url,
            provider_headers,
            slice_index,
            known_total_size,
            request_control,
            upstream_header_timeout,
            bypass_on_non_partial: true,
        })
        .await
    }

    async fn get_or_fetch_slice_result_with_control(
        &self,
        fetch: SliceFetchRequest<'_>,
    ) -> Result<SliceFetchResult, anyhow::Error> {
        let SliceFetchRequest {
            url,
            provider_headers,
            slice_index,
            known_total_size,
            request_control,
            upstream_header_timeout,
            bypass_on_non_partial,
        } = fetch;
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
            if self.config.stale_while_revalidate && entry.is_stale(self.config.stale_max_age) {
                if let Some(total_size) = effective_total_size {
                    // Mark as updating so subsequent requests know a refresh
                    // is expected.  `DashSet::insert` returns true if the key
                    // was newly inserted (i.e., we are the first stale request).
                    if self.updating_keys.insert(key.clone()) {
                        // Newly inserted -- we are the first stale request.
                        self.spawn_slice_revalidation(
                            url,
                            provider_headers,
                            slice_index,
                            total_size,
                            upstream_header_timeout,
                        );
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
            }
            // Expired beyond stale window -- fall through to re-fetch
            // under the lock. Do NOT remove the entry here; it may be
            // needed for conditional request (304 Not Modified) handling.
        }

        // Acquire the per-key lock cooperatively.
        // Proxy requests should only stop for caller cancellation, not for a
        // proxy-local timeout budget.
        let _guard = self
            .lock_slice_key(
                &key,
                format!("Request cancelled while waiting for slice cache lock {slice_index}"),
                request_control,
            )
            .await?;

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
        let range_request = self.build_slice_range_request(
            url,
            provider_headers,
            slice_index,
            should_send_conditional,
        )?;

        let resp = match send_with_redirect_validation_with_control_and_timeout(
            &self.client,
            range_request.request,
            &self.ssrf_guard,
            request_control,
            upstream_header_timeout,
        )
        .await
        {
            Ok(proxy_response) => proxy_response.response,
            Err(e) => {
                // Clean up updating_keys on send failure so the key is not
                // permanently stuck in "updating" state.
                self.updating_keys.remove(&key);
                return Err(e).context("Slice fetch failed");
            }
        };

        // Handle 304 Not Modified: refresh the TTL and return Revalidated.
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            // Consume the (empty) body to release the connection.
            Self::drain_not_modified_body(resp, request_control).await;

            // Refresh the entry's TTL by re-inserting it.
            if let Some(existing) = self.backend.get(&key).await {
                if let Some(total_size) = effective_total_size {
                    self.refresh_cached_slice_ttl(&key, existing.data.clone())
                        .await?;
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
            let request2 =
                self.build_slice_range_request(url, provider_headers, slice_index, false)?;
            let resp2 = match send_with_redirect_validation_with_control_and_timeout(
                &self.client,
                request2.request,
                &self.ssrf_guard,
                request_control,
                upstream_header_timeout,
            )
            .await
            {
                Ok(proxy_response) => proxy_response.response,
                Err(e) => {
                    self.updating_keys.remove(&key);
                    return Err(e).context("Slice re-fetch failed after 304");
                }
            };
            if bypass_on_non_partial && resp2.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                self.updating_keys.remove(&key);
                return Ok(SliceFetchResult::Bypass(resp2));
            }
            let result = self
                .process_slice_response(
                    resp2,
                    SliceResponseContext {
                        url,
                        provider_headers,
                        key: &key,
                        slice_index,
                        known_total_size: effective_total_size,
                        range_start: range_request.range_start,
                        cache_status: fetch_status,
                        request_control,
                    },
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
                SliceResponseContext {
                    url,
                    provider_headers,
                    key: &key,
                    slice_index,
                    known_total_size: effective_total_size,
                    range_start: range_request.range_start,
                    cache_status: fetch_status,
                    request_control,
                },
            )
            .await;
        // Always clean up updating_keys (idempotent). Prevents the key from
        // being stuck in "updating" state if process_slice_response errs.
        self.updating_keys.remove(&key);
        result.map(SliceFetchResult::Slice)
    }

    /// Process a successful (non-304) slice response: validate, store, and
    /// return the data.
    async fn process_slice_response(
        &self,
        resp: reqwest::Response,
        context: SliceResponseContext<'_>,
    ) -> Result<FetchedSlice, anyhow::Error> {
        let SliceResponseContext {
            url,
            provider_headers,
            key,
            slice_index,
            known_total_size,
            range_start,
            cache_status,
            request_control,
        } = context;

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
            if cr.start != range_start || cr.end > requested_range_end_exclusive {
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

        // Early validator consistency check BEFORE reading the body (nginx header
        // filter pattern).  This avoids reading a full 2 MiB slice body only
        // to discard it on mismatch.
        // IMPORTANT: we must not hold a DashMap `Ref` across the call to
        // `invalidate_resource`, which needs a write lock on the same shard.
        // Clone existing validators out of the DashMap first to avoid deadlock.
        let mk = Self::meta_key(url, provider_headers);
        let existing_validators = self
            .meta
            .get(&mk)
            .map(|m| (m.etag.clone(), m.last_modified.clone()));

        let expected_len = usize::try_from(
            parsed_content_range
                .end
                .saturating_sub(parsed_content_range.start),
        )
        .map_err(|_| anyhow::anyhow!("Slice length overflow for slice {slice_index}"))?;

        if let Some(content_length) = resp.content_length() {
            let content_length = usize::try_from(content_length)
                .map_err(|_| anyhow::anyhow!("Slice Content-Length overflow"))?;
            if content_length != expected_len {
                return Err(anyhow::anyhow!(
                    "Slice body length mismatch: Content-Length got {content_length}, expected {expected_len}"
                ));
            }
        }

        if let Some((Some(existing_etag), _)) = &existing_validators {
            match &resp_etag {
                Some(new_etag) if new_etag == existing_etag => {}
                Some(new_etag) => {
                    self.invalidate_resource(url, provider_headers, total_size)
                        .await;
                    return Err(anyhow::anyhow!(
                        "ETag mismatch: resource modified between slice fetches \
                         (expected {existing_etag}, got {new_etag})"
                    ));
                }
                None => {
                    self.invalidate_resource(url, provider_headers, total_size)
                        .await;
                    return Err(anyhow::anyhow!(
                        "ETag disappeared while fetching slices for resource"
                    ));
                }
            }
        } else if let Some((None, Some(existing_last_modified))) = &existing_validators {
            if resp_last_modified.as_ref() != Some(existing_last_modified) {
                self.invalidate_resource(url, provider_headers, total_size)
                    .await;
                return Err(anyhow::anyhow!(
                    "Last-Modified mismatch: resource modified between slice fetches"
                ));
            }
        }

        let data = read_exact_slice_body(resp, expected_len, request_control).await?;

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
        provider_headers: &ProviderHeaders,
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
    /// `meta` `DashMap`. Uses a simple size cap: when the map exceeds
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

    /// Run pending maintenance tasks on the `seen_keys` cache so that
    /// `entry_count()` reflects recent inserts.
    pub async fn sync_seen_keys(&self) {
        self.seen_keys.run_pending_tasks().await;
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
