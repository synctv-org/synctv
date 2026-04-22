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
use super::range::{aligned_range_for_slice, parse_content_range};
use super::status::CacheStatus;

/// Number of lock cleanup cycles before triggering a stale-lock sweep.
const LOCK_CLEANUP_INTERVAL: u64 = 64;
const MAX_META_ENTRIES: usize = 100_000;
const META_RETENTION_TARGET_DIVISOR: usize = 2;

// SliceCache

/// Per-key Mutex to prevent thundering herd on the same slice.
type SliceLock = Arc<Mutex<()>>;

pub(super) struct FullBodyWrite<'a> {
    pub(super) url: &'a str,
    pub(super) provider_headers: &'a HashMap<String, String>,
    pub(super) data: Bytes,
    pub(super) etag: Option<&'a str>,
    pub(super) last_modified: Option<&'a str>,
    pub(super) content_type: Option<&'a str>,
    pub(super) ttl: Duration,
}

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

impl SliceCache {
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

        if let Some(entry) = self.backend.get(&key).await {
            if !entry.is_expired() {
                return Ok(());
            }
        }

        let (range_start, _range_end) =
            aligned_range_for_slice(slice_index, self.config.slice_size, total_size)?;
        let range_header = format!(
            "bytes={range_start}-{}",
            std::cmp::min(range_start + self.config.slice_size as u64, total_size) - 1
        );

        let mut request = self.client.get(url);
        request = apply_provider_headers(request, url, provider_headers)?;
        request = request.header("Range", &range_header);

        let mk = Self::meta_key(url, provider_headers);
        if let Some(meta_ref) = self.meta.get(&mk) {
            if let Some(ref etag) = meta_ref.etag {
                request = request.header("If-None-Match", etag.as_str());
            }
            if let Some(ref lm) = meta_ref.last_modified {
                request = request.header("If-Modified-Since", lm.as_str());
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
                total_size,
                range_start,
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
            total_size,
            range_start,
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

    /// Compute the cache key used for full-body entries.
    pub(super) fn full_body_key(url: &str, provider_headers: &HashMap<String, String>) -> String {
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

        hasher.update(b"\0full");
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
        let key = Self::compute_cache_key(url, provider_headers, slice_index);

        // Fast path: check cache without locking.
        if let Some(entry) = self.backend.get(&key).await {
            if !entry.is_expired() {
                return Ok((entry.data, CacheStatus::Hit));
            }
            // Check stale window for stale-while-revalidate.
            if self.config.stale_while_revalidate && entry.is_stale(self.config.stale_max_age) {
                // Mark as updating so subsequent requests know a refresh
                // is expected.  `DashSet::insert` returns true if the key
                // was newly inserted (i.e., we are the first stale request).
                if self.updating_keys.insert(key.clone()) {
                    // Newly inserted -- we are the first stale request.
                    self.spawn_slice_revalidation(url, provider_headers, slice_index, total_size);
                    return Ok((entry.data, CacheStatus::Stale));
                }
                // Already present -- another request is updating this key.
                return Ok((entry.data, CacheStatus::Updating));
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

        // Double-check after acquiring lock.
        if let Some(entry) = self.backend.get(&key).await {
            if !entry.is_expired() {
                // Another task may have completed a re-fetch while we were
                // waiting for the lock.
                self.updating_keys.remove(&key);
                return Ok((entry.data, CacheStatus::Hit));
            }
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
            aligned_range_for_slice(slice_index, self.config.slice_size, total_size)?;
        let range_header = format!("bytes={range_start}-{range_end}");

        let mut request = self.client.get(url);
        request = apply_provider_headers(request, url, provider_headers)?;
        request = request.header("Range", &range_header);

        // Conditional request headers: If-None-Match / If-Modified-Since.
        let mk = Self::meta_key(url, provider_headers);
        if let Some(meta_ref) = self.meta.get(&mk) {
            if let Some(ref etag) = meta_ref.etag {
                request = request.header("If-None-Match", etag.as_str());
            }
            if let Some(ref lm) = meta_ref.last_modified {
                request = request.header("If-Modified-Since", lm.as_str());
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
                let refreshed = StoredEntry::new(existing.data.clone(), self.config.segment_ttl);
                let _ = self.backend.put(&key, refreshed).await;
                self.updating_keys.remove(&key);
                return Ok((existing.data, CacheStatus::Revalidated));
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
            let result = self
                .process_slice_response(
                    resp2,
                    url,
                    provider_headers,
                    &key,
                    slice_index,
                    total_size,
                    range_start,
                    request_control,
                )
                .await;
            // Always clean up updating_keys (idempotent remove).
            self.updating_keys.remove(&key);
            return result;
        }

        let result = self
            .process_slice_response(
                resp,
                url,
                provider_headers,
                &key,
                slice_index,
                total_size,
                range_start,
                request_control,
            )
            .await;
        // Always clean up updating_keys (idempotent). Prevents the key from
        // being stuck in "updating" state if process_slice_response errs.
        self.updating_keys.remove(&key);
        result
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
        total_size: u64,
        range_start: u64,
        request_control: Option<&ExecutionControl>,
    ) -> Result<(Bytes, CacheStatus), anyhow::Error> {
        // For slice requests, only 206 Partial Content is valid.
        // A 200 OK means upstream doesn't support Range requests and
        // returned the full body, which would corrupt the slice cache.
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
            let expected_end =
                std::cmp::min(range_start + self.config.slice_size as u64, total_size);
            if cr.start != range_start || cr.end != expected_end {
                return Err(anyhow::anyhow!(
                    "Content-Range mismatch: got {}-{}, expected {}-{} \
                     (nginx slice header filter validation)",
                    cr.start,
                    cr.end,
                    range_start,
                    expected_end,
                ));
            }
            match cr.complete_length {
                Some(complete_length) if complete_length == total_size => {}
                Some(complete_length) => {
                    return Err(anyhow::anyhow!(
                        "Content-Range total mismatch: got {complete_length}, expected {total_size}"
                    ));
                }
                None => {}
            }
            Some(cr)
        } else {
            tracing::warn!(
                slice_index = slice_index,
                range_start = range_start,
                "Upstream returned 206 Partial Content without Content-Range header; \
                 cannot validate slice boundaries"
            );
            None
        };

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

        if let Some(cr) = parsed_content_range {
            let expected_len = usize::try_from(cr.end.saturating_sub(cr.start))
                .map_err(|_| anyhow::anyhow!("Slice length overflow for slice {slice_index}"))?;
            if data.len() != expected_len {
                return Err(anyhow::anyhow!(
                    "Slice body length mismatch: got {}, expected {} from Content-Range",
                    data.len(),
                    expected_len
                ));
            }
        }

        if existing_etag_cloned.is_none() && !self.meta.contains_key(&mk) {
            // First slice for this resource -- store metadata.
            self.meta.insert(
                mk,
                CachedResourceMeta {
                    etag: resp_etag,
                    last_modified: resp_last_modified,
                    total_size: Some(total_size),
                    content_type: resp_content_type,
                    last_accessed: std::time::SystemTime::now(),
                },
            );
        }

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

        Ok((data, CacheStatus::Miss))
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

    // Cache status helpers

    /// Check whether a key is currently in cache and not expired.
    pub(super) async fn is_cached_and_valid(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        slice_index: u64,
    ) -> bool {
        let key = Self::compute_cache_key(url, provider_headers, slice_index);
        self.backend
            .get(&key)
            .await
            .is_some_and(|entry| !entry.is_expired())
    }

    /// Check whether a key was recently inserted (used to distinguish
    /// `EXPIRED` from `MISS`). Backed by a bounded moka cache with TTL,
    /// so very old entries will naturally expire.
    pub(super) async fn was_ever_seen(&self, key: &str) -> bool {
        self.seen_keys.get(key).await.is_some()
    }

    /// Determine the slice-level cache status for a set of needed slices.
    pub(super) async fn determine_slice_cache_status(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        needed: &[u64],
    ) -> CacheStatus {
        let mut all_valid = true;
        let mut any_was_seen = false;
        let mut any_stale = false;

        for &idx in needed {
            let key = Self::compute_cache_key(url, provider_headers, idx);
            if self.is_cached_and_valid(url, provider_headers, idx).await {
                any_was_seen = true;
            } else {
                all_valid = false;
                // Check if the entry is in the stale window.
                if let Some(entry) = self.backend.get(&key).await {
                    if self.config.stale_while_revalidate
                        && entry.is_stale(self.config.stale_max_age)
                    {
                        any_stale = true;
                        any_was_seen = true;
                    } else if entry.is_expired() {
                        any_was_seen = true;
                    }
                } else if self.was_ever_seen(&key).await {
                    any_was_seen = true;
                }
            }
        }

        if all_valid {
            CacheStatus::Hit
        } else if any_stale {
            CacheStatus::Stale
        } else if any_was_seen {
            CacheStatus::Expired
        } else {
            CacheStatus::Miss
        }
    }

    // Full-body cache

    /// Try to get a full-body entry from cache.
    ///
    /// Returns `Some((data, content_type, status))` where status is
    /// [`CacheStatus::Hit`], [`CacheStatus::Stale`], or
    /// [`CacheStatus::Updating`].  Returns `None` if not cached or
    /// expired beyond the stale window.
    pub(super) async fn get_full_body(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
    ) -> Option<(Bytes, Option<String>, CacheStatus)> {
        let key = Self::full_body_key(url, provider_headers);
        if let Some(entry) = self.backend.get(&key).await {
            if !entry.is_expired() {
                let ct = self
                    .meta
                    .get(&Self::meta_key(url, provider_headers))
                    .and_then(|m| m.content_type.clone());
                return Some((entry.data, ct, CacheStatus::Hit));
            }
            // Check stale window.
            if self.config.stale_while_revalidate && entry.is_stale(self.config.stale_max_age) {
                let ct = self
                    .meta
                    .get(&Self::meta_key(url, provider_headers))
                    .and_then(|m| m.content_type.clone());
                let status = if self.updating_keys.contains(&key) {
                    CacheStatus::Updating
                } else {
                    let _ = self.updating_keys.insert(key);
                    CacheStatus::Stale
                };
                return Some((entry.data, ct, status));
            }
            // Expired beyond stale window -- do NOT remove here. The
            // re-fetch will overwrite the entry, and removing eagerly can
            // race with a concurrent re-fetch that expects the entry to
            // still exist for conditional request (304) support.
        }
        None
    }

    /// Retrieve the cached full-body entry regardless of freshness state.
    ///
    /// Used by conditional 304 revalidation paths, which need access to the
    /// expired bytes in order to refresh TTL without forcing a full re-download.
    pub(super) async fn get_full_body_cached_entry(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
    ) -> Option<(Bytes, Option<String>)> {
        let key = Self::full_body_key(url, provider_headers);
        self.backend.get(&key).await.map(|entry| {
            let ct = self
                .meta
                .get(&Self::meta_key(url, provider_headers))
                .and_then(|m| m.content_type.clone());
            (entry.data, ct)
        })
    }

    /// Determine the full-body cache status *before* fetching.
    pub(super) async fn full_body_pre_status(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
    ) -> CacheStatus {
        let key = Self::full_body_key(url, provider_headers);
        if self.was_ever_seen(&key).await {
            CacheStatus::Expired
        } else {
            CacheStatus::Miss
        }
    }

    /// Insert a full-body entry into cache.
    pub(super) async fn put_full_body(&self, write: FullBodyWrite<'_>) {
        let FullBodyWrite {
            url,
            provider_headers,
            data,
            etag,
            last_modified,
            content_type,
            ttl,
        } = write;
        let key = Self::full_body_key(url, provider_headers);
        let entry = StoredEntry::new(data, ttl);
        // Best-effort insert; log the error if it occurs.
        if let Err(e) = self.backend.put(&key, entry).await {
            tracing::warn!("Failed to store full-body entry: {e}");
            return;
        }
        self.seen_keys.insert(key.clone(), ()).await;

        // Clear updating flag.
        self.updating_keys.remove(&key);

        // Store metadata.
        let mk = Self::meta_key(url, provider_headers);
        self.meta.insert(
            mk,
            CachedResourceMeta {
                etag: etag.map(std::string::ToString::to_string),
                last_modified: last_modified.map(std::string::ToString::to_string),
                total_size: None,
                content_type: content_type.map(std::string::ToString::to_string),
                last_accessed: std::time::SystemTime::now(),
            },
        );

        // Periodically clean up stale per-key locks.
        self.maybe_cleanup_locks();
    }

    /// Clear the stale-while-revalidate marker for a full-body cache key.
    ///
    /// Background revalidation uses this on early-return and error paths where
    /// `put_full_body()` is not reached, preventing the key from being stuck in
    /// `UPDATING` forever after a failed refresh.
    pub(super) fn finish_full_body_update(&self, key: &str) {
        self.updating_keys.remove(key);
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
                    content_type: None,
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
                    content_type: None,
                    last_accessed: now,
                },
            );
        }

        cache.cleanup_stale_meta_with_limit(4);

        assert_eq!(cache.meta_count(), 4);
    }
}
