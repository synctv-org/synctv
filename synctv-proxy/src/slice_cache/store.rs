//! Slice cache store: per-key locking, moka backend, metadata management.
//!
//! Contains [`SliceCache`], the central cache struct backed by
//! `moka::future::Cache` with per-key locking to prevent thundering herd.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{apply_provider_headers, PROXY_CLIENT};

use super::config::SliceCacheConfig;
use super::etag::CachedResourceMeta;
use super::range::{aligned_range_for_slice, parse_content_range};

/// Number of lock cleanup cycles before triggering a stale-lock sweep.
const LOCK_CLEANUP_INTERVAL: u64 = 64;

// ------------------------------------------------------------------
// Internal: cache entry wrapper with insertion timestamp for TTL
// ------------------------------------------------------------------

/// Wrapper around cached data that records when the entry was inserted
/// so that TTL-based expiry can report `EXPIRED` vs `MISS`.
#[derive(Clone)]
pub(super) struct CacheEntry {
    pub(super) data: Bytes,
    pub(super) inserted_at: Instant,
    pub(super) ttl: Duration,
}

impl CacheEntry {
    pub(super) fn is_expired(&self) -> bool {
        self.inserted_at.elapsed() > self.ttl
    }
}

// ------------------------------------------------------------------
// SliceCache
// ------------------------------------------------------------------

/// Per-key Mutex to prevent thundering herd on the same slice.
type SliceLock = Arc<Mutex<()>>;

/// A slice cache backed by `moka::future::Cache`.
///
/// Each cached entry is a [`CacheEntry`] containing the `Bytes` data plus
/// insertion metadata.  Per-key locking via a `DashMap<String, Mutex>`
/// ensures that concurrent requests for the same slice trigger at most a
/// single upstream fetch.
///
/// Resource metadata (ETag, Content-Type) is stored in a separate
/// `DashMap` keyed by `url_hash + "meta"` to enable cross-slice ETag
/// validation.
pub struct SliceCache {
    pub(super) config: SliceCacheConfig,
    /// The moka cache storing entries keyed by SHA256(url + sorted_headers + index).
    pub(super) inner: moka::future::Cache<String, CacheEntry>,
    /// Per-key locks to prevent thundering herd.
    locks: dashmap::DashMap<String, SliceLock>,
    /// Per-resource metadata for ETag consistency validation.
    pub(super) meta: dashmap::DashMap<String, CachedResourceMeta>,
    /// Tracks which cache keys have been inserted recently so we can
    /// distinguish `EXPIRED` (was cached, TTL elapsed) from `MISS`
    /// (never seen before). Backed by moka with TTL to prevent unbounded
    /// growth (previously a DashSet that was never pruned).
    pub(super) seen_keys: moka::future::Cache<String, ()>,
    /// Counter for periodic stale lock cleanup.
    lock_ops: std::sync::atomic::AtomicU64,
}

impl SliceCache {
    /// Create a new `SliceCache` with the given configuration.
    #[must_use]
    pub fn new(config: SliceCacheConfig) -> Self {
        let max_capacity = config.max_cache_size;
        let inner = moka::future::Cache::builder()
            .max_capacity(max_capacity)
            .weigher(|_key: &String, entry: &CacheEntry| -> u32 {
                u32::try_from(entry.data.len()).unwrap_or(u32::MAX)
            })
            // moka's time_to_idle is a hard upper bound; we use our own
            // soft TTL check for the EXPIRED distinction but let moka
            // eventually evict truly idle entries.
            .time_to_idle(Duration::from_hours(1))
            .build();

        // seen_keys uses a moka cache with a TTL slightly longer than
        // the main cache's time_to_idle so that "was ever seen" info
        // outlives the data entry but does not grow unbounded.
        let seen_keys = moka::future::Cache::builder()
            .max_capacity(1_000_000)
            .time_to_idle(Duration::from_hours(2))
            .build();

        Self {
            config,
            inner,
            locks: dashmap::DashMap::new(),
            meta: dashmap::DashMap::new(),
            seen_keys,
            lock_ops: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Access the configuration.
    #[must_use]
    pub const fn config(&self) -> &SliceCacheConfig {
        &self.config
    }

    // ---------------------------------------------------------------
    // Cache key helpers
    // ---------------------------------------------------------------

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
    pub(super) fn full_body_key(
        url: &str,
        provider_headers: &HashMap<String, String>,
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

        hasher.update(b"\0full");
        hex::encode(hasher.finalize())
    }

    /// Compute the key used to store per-resource metadata.
    pub(super) fn meta_key(
        url: &str,
        provider_headers: &HashMap<String, String>,
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

        hasher.update(b"\0meta");
        hex::encode(hasher.finalize())
    }

    // ---------------------------------------------------------------
    // Metadata access
    // ---------------------------------------------------------------

    /// Retrieve the stored metadata for a resource, if any.
    #[allow(clippy::unused_async)]
    pub async fn get_resource_meta(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
    ) -> Option<CachedResourceMeta> {
        let mk = Self::meta_key(url, provider_headers);
        self.meta.get(&mk).map(|r| r.clone())
    }

    // ---------------------------------------------------------------
    // Slice fetch
    // ---------------------------------------------------------------

    /// Get or fetch a single aligned slice.
    ///
    /// If the slice is already in cache (and not expired), returns it
    /// immediately.  Otherwise, acquires a per-key lock, double-checks the
    /// cache, and if still missing, fetches from upstream.
    ///
    /// The upstream response's Content-Range is validated against the
    /// requested range (like nginx's header filter at line 166:
    /// `if (cr.start != ctx->start || cr.end != end)`).
    ///
    /// ETag consistency is validated against the stored resource metadata.
    pub async fn get_or_fetch_slice(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        slice_index: u64,
        total_size: u64,
    ) -> Result<Bytes, anyhow::Error> {
        let key = Self::compute_cache_key(url, provider_headers, slice_index);

        // Fast path: check cache without locking.
        if let Some(entry) = self.inner.get(&key).await {
            if !entry.is_expired() {
                return Ok(entry.data);
            }
            // Expired -- fall through to re-fetch.
            self.inner.remove(&key).await;
        }

        // Acquire per-key lock to prevent thundering herd.
        let lock = self
            .locks
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        // Double-check after acquiring lock.
        if let Some(entry) = self.inner.get(&key).await {
            if !entry.is_expired() {
                return Ok(entry.data);
            }
            self.inner.remove(&key).await;
        }

        // Fetch from upstream.
        let (range_start, range_end) =
            aligned_range_for_slice(slice_index, self.config.slice_size, total_size);
        let range_header = format!("bytes={range_start}-{range_end}");

        let mut request = PROXY_CLIENT.get(url);
        request = apply_provider_headers(request, url, provider_headers);
        request = request.header("Range", &range_header);

        let resp = request
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Slice fetch failed: {e}"))?;

        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(anyhow::anyhow!(
                "Upstream returned {} for slice {}",
                resp.status(),
                slice_index
            ));
        }

        // Validate Content-Range response header (nginx header filter pattern).
        if let Some(cr_value) = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
        {
            if let Ok(cr) = parse_content_range(cr_value) {
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
            }
        }

        // Extract headers for metadata before consuming body.
        let resp_etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string);
        let resp_content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string);

        // Read body first so the connection is properly closed before any
        // error-path returns (dropping an unconsumed reqwest::Response can
        // block while the client drains the body for connection reuse).
        let data = resp
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read slice body: {e}"))?;

        // ETag consistency check.
        //
        // IMPORTANT: we must not hold a DashMap `Ref` across the call to
        // `invalidate_resource`, which needs a write lock on the same shard.
        // Clone the existing ETag out of the DashMap first to avoid deadlock.
        let mk = Self::meta_key(url, provider_headers);
        let existing_etag_cloned = self
            .meta
            .get(&mk)
            .and_then(|m| m.etag.clone());

        if let Some(existing_etag) = existing_etag_cloned {
            if let Some(new_etag) = &resp_etag {
                if &existing_etag != new_etag {
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
        } else if !self.meta.contains_key(&mk) {
            // First slice for this resource -- store metadata.
            self.meta.insert(
                mk,
                CachedResourceMeta {
                    etag: resp_etag,
                    total_size: Some(total_size),
                    content_type: resp_content_type,
                },
            );
        }

        // Insert into cache with TTL.
        let entry = CacheEntry {
            data: data.clone(),
            inserted_at: Instant::now(),
            ttl: self.config.segment_ttl,
        };
        self.inner.insert(key.clone(), entry).await;
        self.seen_keys.insert(key.clone(), ()).await;

        // Periodically clean up stale per-key locks to prevent unbounded growth.
        self.maybe_cleanup_locks();

        Ok(data)
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
            self.inner.remove(&key).await;
        }
        // Also remove metadata so next fetch establishes a new ETag.
        let mk = Self::meta_key(url, provider_headers);
        self.meta.remove(&mk);
    }

    // ---------------------------------------------------------------
    // Cache status helpers
    // ---------------------------------------------------------------

    /// Check whether a key is currently in cache and not expired.
    pub(super) async fn is_cached_and_valid(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        slice_index: u64,
    ) -> bool {
        let key = Self::compute_cache_key(url, provider_headers, slice_index);
        self.inner
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
    ) -> &'static str {
        let mut all_valid = true;
        let mut any_was_seen = false;

        for &idx in needed {
            let key = Self::compute_cache_key(url, provider_headers, idx);
            if self.is_cached_and_valid(url, provider_headers, idx).await {
                any_was_seen = true;
            } else {
                all_valid = false;
                if self.was_ever_seen(&key).await {
                    any_was_seen = true;
                }
            }
        }

        if all_valid {
            "HIT"
        } else if any_was_seen {
            "EXPIRED"
        } else {
            "MISS"
        }
    }

    // ---------------------------------------------------------------
    // Full-body cache
    // ---------------------------------------------------------------

    /// Try to get a full-body entry from cache.
    ///
    /// Returns `Some((data, content_type, status))` where status is
    /// `"HIT"` or `None` if not cached / expired.  When expired, we
    /// return `None` but record it for EXPIRED status reporting.
    pub(super) async fn get_full_body(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
    ) -> Option<(Bytes, Option<String>, &'static str)> {
        let key = Self::full_body_key(url, provider_headers);
        if let Some(entry) = self.inner.get(&key).await {
            if !entry.is_expired() {
                let ct = self
                    .meta
                    .get(&Self::meta_key(url, provider_headers))
                    .and_then(|m| m.content_type.clone());
                return Some((entry.data.clone(), ct, "HIT"));
            }
            // Expired -- remove so the re-fetch can insert anew.
            self.inner.remove(&key).await;
        }
        None
    }

    /// Determine the full-body cache status *before* fetching.
    pub(super) async fn full_body_pre_status(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
    ) -> &'static str {
        let key = Self::full_body_key(url, provider_headers);
        if self.was_ever_seen(&key).await {
            "EXPIRED"
        } else {
            "MISS"
        }
    }

    /// Insert a full-body entry into cache.
    pub(super) async fn put_full_body(
        &self,
        url: &str,
        provider_headers: &HashMap<String, String>,
        data: Bytes,
        content_type: Option<&str>,
        ttl: Duration,
    ) {
        let key = Self::full_body_key(url, provider_headers);
        let entry = CacheEntry {
            data,
            inserted_at: Instant::now(),
            ttl,
        };
        self.inner.insert(key.clone(), entry).await;
        self.seen_keys.insert(key, ()).await;

        // Store metadata.
        let mk = Self::meta_key(url, provider_headers);
        self.meta.insert(
            mk,
            CachedResourceMeta {
                etag: None,
                total_size: None,
                content_type: content_type.map(|s| s.to_string()),
            },
        );
    }

    // ---------------------------------------------------------------
    // Lock cleanup (L3 fix)
    // ---------------------------------------------------------------

    /// Periodically remove stale per-key locks that are not currently held
    /// by any task.  A lock is considered stale when the only remaining
    /// `Arc` reference is the one stored in the `DashMap` itself
    /// (`strong_count == 1`).
    fn maybe_cleanup_locks(&self) {
        let count = self
            .lock_ops
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count % LOCK_CLEANUP_INTERVAL != 0 {
            return;
        }
        self.cleanup_stale_locks();
    }

    /// Remove all per-key locks not currently held by any task.
    pub fn cleanup_stale_locks(&self) {
        self.locks
            .retain(|_key, lock| Arc::strong_count(lock) > 1);
    }

    /// Return the current number of per-key locks (for diagnostics/testing).
    #[must_use]
    pub fn lock_count(&self) -> usize {
        self.locks.len()
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
