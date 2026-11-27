#![allow(clippy::missing_errors_doc, clippy::too_many_lines)]

//! Slice cache store: backend orchestration, per-key locking, and upstream
//! slice fetch/revalidation.
//!
//! Contains [`SliceCache`], the central cache struct backed by a
//! [`CacheBackend`] (memory or file). Small data types, maintenance helpers,
//! and key hashing live in sibling modules to keep this file focused on cache
//! behavior.

mod full_response;

use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use synctv_common::ExecutionControl;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::{
    ProviderHeaders, apply_provider_headers, run_with_proxy_cancellation,
    send_with_redirect_validation_with_control_and_timeout,
};

use super::backend::{CacheBackend, SliceCacheBackend};
use super::bounds::{aligned_request, response_extent};
use super::config::{CacheBackendConfig, SliceCacheConfig};
use super::etag::{CachedResourceMeta, StoredEntry};
use super::head::HeadFetchContext;
use super::keys::{resource_meta_key, slice_cache_key};
use super::maintenance::{MetaEvictionCandidate, UpdatingKeyGuard, ratio_u64};
use super::range::{format_request_range, parse_content_range};
use super::status::CacheStatus;
use super::types::{
    CachedSlice, FetchedSlice, HeadResourceResult, SliceCachePurgeResult, SliceCacheStats,
    SliceFetchRequest, SliceFetchResult,
};

pub(super) fn cleanup_stale_resource_meta(
    meta: &dashmap::DashMap<String, CachedResourceMeta>,
    max_meta_entries: usize,
) {
    if meta.len() <= max_meta_entries {
        return;
    }

    let retention_target = max_meta_entries / META_RETENTION_TARGET_DIVISOR;
    let target_removals = meta.len().saturating_sub(retention_target);
    if target_removals == 0 {
        return;
    }

    let mut oldest_candidates = BinaryHeap::with_capacity(target_removals);
    for entry in meta {
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
        meta.remove_if(&candidate.key, |_, entry| {
            entry.last_accessed == candidate.last_accessed
        });
    }
}

/// Number of lock cleanup cycles before triggering a stale-lock sweep.
const LOCK_CLEANUP_INTERVAL: u64 = 64;
pub(super) const MAX_META_ENTRIES: usize = 100_000;
const META_RETENTION_TARGET_DIVISOR: usize = 2;

/// Bound for a background revalidation when the request carries no upstream
/// header timeout.
const BACKGROUND_REVALIDATION_TIMEOUT: Duration = Duration::from_secs(60);

/// Whether `current` names a different resource than `previous`. Two present
/// validators of the same kind decide; one that appears or disappears does not.
fn validators_changed(
    previous: &(Option<String>, Option<String>),
    current: &(Option<String>, Option<String>),
) -> bool {
    match (&previous.0, &current.0) {
        (Some(previous), Some(current)) => previous != current,
        (None, None) => match (&previous.1, &current.1) {
            (Some(previous), Some(current)) => previous != current,
            _ => false,
        },
        _ => false,
    }
}

/// Per-key Mutex to prevent thundering herd on the same slice.
type SliceLock = Arc<Mutex<()>>;

struct SliceRangeRequest {
    request: reqwest::RequestBuilder,
    range_start: u64,
}

struct SliceResponseContext<'a> {
    meta_key: &'a str,
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
#[derive(Clone)]
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
    /// Keys currently being updated (STALE/UPDATING support).
    updating_keys: Arc<dashmap::DashSet<String>>,
    /// Counter for periodic stale lock cleanup.
    lock_ops: Arc<std::sync::atomic::AtomicU64>,
}

impl SliceCache {
    async fn lock_slice_key(
        &self,
        key: &str,
        wait_error: impl Into<String>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<OwnedMutexGuard<()>, anyhow::Error> {
        let lock = self.key_lock(key.to_string());

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
        validators: Option<&CachedResourceMeta>,
    ) -> Result<SliceRangeRequest, anyhow::Error> {
        let (range_start, range_end) = aligned_request(slice_index, self.config.slice_size as u64)
            .with_context(|| format!("Invalid slice range for index {slice_index}"))?;
        let range_header = format_request_range(range_start, range_end);

        let mut request = self.client.get(url);
        request = apply_provider_headers(request, url, provider_headers)?;
        request = request.header(reqwest::header::RANGE, &range_header);

        if let Some(meta) = validators {
            if let Some(etag) = &meta.etag {
                request = request.header(reqwest::header::IF_NONE_MATCH, etag.as_str());
            }
            if let Some(last_modified) = &meta.last_modified {
                request =
                    request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified.as_str());
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
    ) -> anyhow::Result<()> {
        if let Err(error) =
            run_with_proxy_cancellation("slice cache 304 drain", request_control, resp.bytes())
                .await?
        {
            tracing::debug!(%error, "failed to drain slice cache 304 response body");
        }
        Ok(())
    }

    /// Re-insert the still-cached slice data under a fresh segment TTL after a
    /// `304 Not Modified`, returning the data on success.
    async fn refresh_cached_slice_ttl(&self, key: &str, data: Bytes) -> Result<(), anyhow::Error> {
        let refreshed = StoredEntry::new(data, self.config.segment_ttl);
        self.backend
            .put(key, refreshed)
            .await
            .with_context(|| format!("failed to refresh cached slice TTL for {key}"))
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
                content_encoding: meta.and_then(|meta| meta.content_encoding.clone()),
                etag: meta.and_then(|meta| meta.etag.clone()),
                last_modified: meta.and_then(|meta| meta.last_modified.clone()),
                data,
            },
            status,
        }
    }

    fn spawn_slice_revalidation(
        &self,
        registration: UpdatingKeyGuard,
        fetch: &SliceFetchRequest<'_>,
        total_size: u64,
        meta_key: String,
    ) {
        let cache = self.clone();
        let url = fetch.url.to_owned();
        let provider_headers = fetch.provider_headers.clone();
        let slice_index = fetch.slice_index;
        let upstream_header_timeout = fetch.upstream_header_timeout;
        tokio::spawn(async move {
            let fetch = SliceFetchRequest {
                url: &url,
                provider_headers: &provider_headers,
                slice_index,
                known_total_size: Some(total_size),
                request_control: None,
                upstream_header_timeout,
                bypass_on_non_partial: false,
            };
            // Nobody waits for this fetch, so bound it as a whole. An upstream
            // that sends its headers and then stalls would otherwise hold the
            // slice lock, and with it every later reader of the slice.
            let deadline = fetch
                .upstream_header_timeout
                .map_or(BACKGROUND_REVALIDATION_TIMEOUT, |timeout| {
                    timeout.saturating_mul(2)
                });
            let fetched = tokio::time::timeout(
                deadline,
                cache.fetch_slice_after_lock(&fetch, registration.key(), &meta_key),
            )
            .await;
            match fetched {
                Ok(Err(error)) => {
                    tracing::debug!(slice_index, %error, "Background slice revalidation failed");
                }
                Err(_) => {
                    tracing::debug!(slice_index, "Background slice revalidation timed out");
                }
                Ok(Ok(_)) => {}
            }
        });
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
                fb.load_index(config.stale_retention()).await?;
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
        Self {
            config,
            ssrf_guard,
            client,
            backend: Arc::new(backend),
            locks: Arc::new(dashmap::DashMap::new()),
            meta: Arc::new(dashmap::DashMap::new()),
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
        // In-flight requests still own their locks and refresh registrations.
        self.cleanup_stale_locks();

        SliceCachePurgeResult {
            removed_entries,
            freed_bytes,
        }
    }

    /// Remove expired cached entries immediately, including entries in the stale window.
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
        slice_cache_key(&resource_meta_key(url, provider_headers), slice_index)
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
        match self.meta.get_mut(&mk) {
            Some(mut entry) => {
                entry.last_accessed = std::time::SystemTime::now();
                Some(entry.clone())
            }
            _ => None,
        }
    }

    pub(super) fn put_resource_meta(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
        meta: CachedResourceMeta,
    ) {
        let mk = Self::meta_key(url, provider_headers);
        self.put_resource_meta_by_key(mk, meta);
    }

    fn put_resource_meta_by_key(&self, meta_key: String, meta: CachedResourceMeta) {
        self.put_resource_meta_by_key_with_limit(meta_key, meta, MAX_META_ENTRIES);
    }

    fn put_resource_meta_by_key_with_limit(
        &self,
        meta_key: String,
        meta: CachedResourceMeta,
        max_meta_entries: usize,
    ) {
        self.meta.insert(meta_key, meta);
        self.cleanup_stale_meta_with_limit(max_meta_entries);
    }

    fn resource_meta_lock(&self, meta_key: &str) -> SliceLock {
        self.key_lock(format!("meta:{meta_key}"))
    }

    fn key_lock(&self, key: String) -> SliceLock {
        let lock = self
            .locks
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let count = self
            .lock_ops
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count.is_multiple_of(LOCK_CLEANUP_INTERVAL) {
            self.cleanup_stale_locks();
        }
        lock
    }

    pub(super) async fn get_or_fetch_head_resource_with_control(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
        range_header: Option<&str>,
        request_control: Option<&ExecutionControl>,
        upstream_header_timeout: Option<Duration>,
    ) -> Result<HeadResourceResult, anyhow::Error> {
        let meta_key = Self::meta_key(url, provider_headers);
        let previous_validators = self.resource_validators(&meta_key);
        let result = super::head::get_or_fetch_head_resource_with_control(
            HeadFetchContext {
                client: &self.client,
                ssrf_guard: &self.ssrf_guard,
                segment_ttl: self.config.segment_ttl,
                meta: &self.meta,
                meta_lock: self.resource_meta_lock(&meta_key),
                meta_key: meta_key.clone(),
            },
            url,
            provider_headers,
            range_header,
            request_control,
            upstream_header_timeout,
        )
        .await?;
        // Slices are revalidated against the resource metadata. When a HEAD
        // learns other validators, they are stale, and a later 304 would keep
        // serving the old bytes as fresh. Drop them and keep the metadata.
        if result.cache_status == CacheStatus::Miss
            && let Some(previous) = previous_validators
            && let Some(current) = self.resource_validators(&meta_key)
            && validators_changed(&previous, &current)
        {
            self.drop_resource_slices(&meta_key).await;
        }
        Ok(result)
    }

    /// The `ETag` and `Last-Modified` a conditional request for the resource
    /// would send.
    fn resource_validators(&self, meta_key: &str) -> Option<(Option<String>, Option<String>)> {
        self.meta
            .get(meta_key)
            .map(|meta| (meta.etag.clone(), meta.last_modified.clone()))
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
        let meta_key = Self::meta_key(fetch.url, fetch.provider_headers);
        let key = slice_cache_key(&meta_key, fetch.slice_index);
        let (cached_meta, effective_total_size) =
            self.cached_meta_and_total_size(&meta_key, fetch.known_total_size);

        // Fast path: check cache without locking.
        if let Some(entry) = self.backend.get(&key).await {
            if !entry.is_expired()
                && let Some(total_size) = effective_total_size
            {
                return Ok(SliceFetchResult::Slice(Self::fetched_slice_from_meta(
                    total_size,
                    cached_meta.as_ref(),
                    entry.data,
                    CacheStatus::Hit,
                )));
            }
            // Check stale window for stale-while-revalidate.
            if self.config.stale_while_revalidate
                && entry.is_stale(self.config.stale_max_age)
                && let Some(total_size) = effective_total_size
            {
                let status = if let Some(registration) =
                    UpdatingKeyGuard::try_register(&self.updating_keys, key)
                {
                    self.spawn_slice_revalidation(registration, &fetch, total_size, meta_key);
                    CacheStatus::Stale
                } else {
                    CacheStatus::Updating
                };
                return Ok(SliceFetchResult::Slice(Self::fetched_slice_from_meta(
                    total_size,
                    cached_meta.as_ref(),
                    entry.data,
                    status,
                )));
            }
            // Expired beyond stale window -- fall through to re-fetch
            // under the lock. Do NOT remove the entry here; it may be
            // needed for conditional request (304 Not Modified) handling.
        }

        self.fetch_slice_after_lock(&fetch, &key, &meta_key).await
    }

    async fn fetch_slice_after_lock(
        &self,
        fetch: &SliceFetchRequest<'_>,
        key: &str,
        meta_key: &str,
    ) -> anyhow::Result<SliceFetchResult> {
        let SliceFetchRequest {
            url,
            provider_headers,
            slice_index,
            known_total_size,
            request_control,
            upstream_header_timeout,
            bypass_on_non_partial,
        } = *fetch;
        let _guard = self
            .lock_slice_key(
                key,
                format!("Request cancelled while waiting for slice cache lock {slice_index}"),
                request_control,
            )
            .await?;
        let (cached_meta, effective_total_size) =
            self.cached_meta_and_total_size(meta_key, known_total_size);
        let existing = self.backend.get(key).await;
        if let Some(entry) = &existing
            && !entry.is_expired()
            && let Some(total_size) = effective_total_size
        {
            return Ok(SliceFetchResult::Slice(Self::fetched_slice_from_meta(
                total_size,
                cached_meta.as_ref(),
                entry.data.clone(),
                CacheStatus::Hit,
            )));
        }
        let has_entry = existing.is_some();
        drop(existing);

        let validators = cached_meta
            .as_ref()
            .filter(|meta| has_entry && (meta.etag.is_some() || meta.last_modified.is_some()));
        let request =
            self.build_slice_range_request(url, provider_headers, slice_index, validators)?;
        let range_start = request.range_start;
        let mut resp = send_with_redirect_validation_with_control_and_timeout(
            &self.client,
            request.request,
            &self.ssrf_guard,
            request_control,
            upstream_header_timeout,
        )
        .await
        .context("Slice fetch failed")?
        .response;

        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            Self::drain_not_modified_body(resp, request_control).await?;
            if validators.is_some()
                && let Some(existing) = self.backend.get(key).await
                && let Some(total_size) = effective_total_size
            {
                self.refresh_cached_slice_ttl(key, existing.data.clone())
                    .await?;
                return Ok(SliceFetchResult::Slice(Self::fetched_slice_from_meta(
                    total_size,
                    cached_meta.as_ref(),
                    existing.data,
                    CacheStatus::Revalidated,
                )));
            }
            // An unsolicited 304 or an eviction during revalidation requires fresh bytes.
            let retry = self.build_slice_range_request(url, provider_headers, slice_index, None)?;
            resp = send_with_redirect_validation_with_control_and_timeout(
                &self.client,
                retry.request,
                &self.ssrf_guard,
                request_control,
                upstream_header_timeout,
            )
            .await
            .context("Slice re-fetch failed after 304")?
            .response;
        }

        if bypass_on_non_partial && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            warn_upstream_slice_rejection(&resp, slice_index, url);
            return Ok(SliceFetchResult::Bypass(resp));
        }
        self.process_slice_response(
            resp,
            SliceResponseContext {
                meta_key,
                key,
                slice_index,
                known_total_size: effective_total_size,
                range_start,
                cache_status: if has_entry {
                    CacheStatus::Expired
                } else {
                    CacheStatus::Miss
                },
                request_control,
            },
        )
        .await
        .map(SliceFetchResult::Slice)
    }

    /// Process a successful (non-304) slice response: validate, store, and
    /// return the data.
    async fn process_slice_response(
        &self,
        resp: reqwest::Response,
        context: SliceResponseContext<'_>,
    ) -> Result<FetchedSlice, anyhow::Error> {
        let SliceResponseContext {
            meta_key,
            key,
            slice_index,
            known_total_size,
            range_start,
            cache_status,
            request_control,
        } = context;

        if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(anyhow::anyhow!(
                "Upstream returned {} for slice {} (expected 206 Partial Content)",
                resp.status(),
                slice_index
            ));
        }

        let content_range = resp
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            .context(
                "Upstream returned 206 Partial Content without a valid Content-Range header",
            )?;
        let cr = parse_content_range(content_range)?;
        let (total_size, expected_len) = response_extent(
            range_start,
            self.config.slice_size,
            cr.start,
            cr.end,
            cr.complete_length,
            known_total_size,
        ).with_context(|| format!(
            "Content-Range mismatch: got {content_range}, requested slice at {range_start} with width {} and known total {known_total_size:?}",
            self.config.slice_size
        ))?;

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
        let resp_content_encoding = resp
            .headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string);

        // Early validator consistency check BEFORE reading the body (nginx header
        // filter pattern).  This avoids reading a full 2 MiB slice body only
        // to discard it on mismatch.
        // IMPORTANT: we must not hold a DashMap `Ref` across the call to
        // `invalidate_resource`, which needs a write lock on the same shard.
        // Clone existing validators out of the DashMap first to avoid deadlock.
        let existing_validators = self
            .meta
            .get(meta_key)
            .map(|m| (m.etag.clone(), m.last_modified.clone()));

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
                    self.invalidate_resource(meta_key).await;
                    return Err(anyhow::anyhow!(
                        "ETag mismatch: resource modified between slice fetches \
                         (expected {existing_etag}, got {new_etag})"
                    ));
                }
                None => {
                    self.invalidate_resource(meta_key).await;
                    return Err(anyhow::anyhow!(
                        "ETag disappeared while fetching slices for resource"
                    ));
                }
            }
        } else if let Some((None, Some(existing_last_modified))) = &existing_validators
            && resp_last_modified.as_ref() != Some(existing_last_modified)
        {
            self.invalidate_resource(meta_key).await;
            return Err(anyhow::anyhow!(
                "Last-Modified mismatch: resource modified between slice fetches"
            ));
        }

        let data = read_exact_slice_body(resp, expected_len, request_control).await?;

        self.put_resource_meta_by_key(
            meta_key.to_owned(),
            CachedResourceMeta {
                status: Some(reqwest::StatusCode::PARTIAL_CONTENT.as_u16()),
                etag: resp_etag.clone(),
                last_modified: resp_last_modified.clone(),
                total_size: Some(total_size),
                supports_ranges: true,
                content_type: resp_content_type.clone(),
                content_encoding: resp_content_encoding.clone(),
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

        Ok(FetchedSlice {
            slice: CachedSlice {
                total_size,
                content_type: resp_content_type,
                content_encoding: resp_content_encoding,
                etag: resp_etag,
                last_modified: resp_last_modified,
                data,
            },
            status: cache_status,
        })
    }

    /// Removes the cached slices of a resource and keeps its metadata.
    async fn drop_resource_slices(&self, resource: &str) {
        let prefix = format!("{resource}-");
        for key in self.backend.keys_with_prefix(&prefix) {
            self.backend.remove(&key).await;
        }
    }

    async fn invalidate_resource(&self, resource: &str) {
        self.drop_resource_slices(resource).await;
        self.meta.remove(resource);
    }

    /// Remove all per-key locks not currently held by any task.
    pub fn cleanup_stale_locks(&self) {
        self.locks.retain(|_key, lock| Arc::strong_count(lock) > 1);
    }

    pub(super) fn cleanup_stale_meta_with_limit(&self, max_meta_entries: usize) {
        cleanup_stale_resource_meta(&self.meta, max_meta_entries);
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
}

/// Log an upstream client-error rejection of a slice range request.
///
/// These rejections are relayed to the client (bypass), so without this log
/// the reason — for example a signed-URL authorization or risk-control
/// rejection on a media CDN — would be invisible server-side. The URL is
/// reduced to its host plus a few non-secret diagnostic query parameters;
/// full signed URLs stay out of the logs.
fn warn_upstream_slice_rejection(resp: &reqwest::Response, slice_index: u64, url: &str) {
    if !resp.status().is_client_error() {
        return;
    }
    let (host, expire, upstream_ip, itag) = match url::Url::parse(url) {
        Ok(parsed) => {
            let mut expire = None;
            let mut upstream_ip = None;
            let mut itag = None;
            for (key, value) in parsed.query_pairs() {
                match &*key {
                    "expire" => expire = Some(value.to_string()),
                    "ip" => upstream_ip = Some(value.to_string()),
                    "itag" => itag = Some(value.to_string()),
                    _ => {}
                }
            }
            (
                parsed.host_str().unwrap_or_default().to_string(),
                expire,
                upstream_ip,
                itag,
            )
        }
        Err(_) => (String::new(), None, None, None),
    };
    tracing::warn!(
        status = %resp.status(),
        slice_index,
        host = %host,
        url_expire = expire,
        upstream_client_ip = upstream_ip,
        itag,
        "Upstream rejected a slice range request; relaying the response to the client"
    );
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
