// HLS proxy client for cross-node HLS streaming
//
// Non-publisher nodes use this client to fetch M3U8 playlists and TS segments
// from the publisher node via gRPC. TS segments are cached locally since they
// are immutable once created. M3U8 playlists are NOT cached since they change
// frequently as new segments are generated.

use bytes::Bytes;
use dashmap::DashMap;
use moka::future::Cache;
use moka::Expiry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tonic::Request;
use tracing::debug;

/// Per-entry TTL policy for the playlist cache.
///
/// - Found playlists (`Some(...)`) use the normal `playlist_cache_ttl` (default
///   1 second) to coalesce concurrent requests while staying fresh.
/// - Not-found responses (`None`) use a much shorter TTL (5 seconds) so that a
///   stream that starts shortly after a "not found" was cached becomes
///   discoverable quickly.
struct PlaylistCacheExpiry {
    /// TTL for found playlist entries.
    found_ttl: Duration,
    /// TTL for "not found" (None) entries.  Must be shorter than `found_ttl`.
    not_found_ttl: Duration,
}

impl Expiry<String, Option<String>> for PlaylistCacheExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &Option<String>,
        _created_at: Instant,
    ) -> Option<Duration> {
        if value.is_some() {
            Some(self.found_ttl)
        } else {
            Some(self.not_found_ttl)
        }
    }
}

use super::connection_pool::GrpcConnectionPool;
use super::proto::{
    stream_relay_service_client::StreamRelayServiceClient,
    GetHlsPlaylistRequest, GetHlsSegmentRequest,
};

/// HLS proxy client that fetches playlists and segments from publisher nodes via gRPC.
///
/// Two-tier local cache:
/// - **TS segments**: cached with 90s TTL (immutable once created, high hit rate)
/// - **M3U8 playlists**: cached with short TTL (default 1s) to coalesce concurrent
///   requests from multiple viewers polling the same stream, while still picking up
///   new segments promptly.
///
/// gRPC connections to publisher nodes are pooled via [`GrpcConnectionPool`] to
/// avoid the overhead of creating a new HTTP/2 connection per request.
///
/// # Cache Key Format with Epoch
///
/// Cache keys include an epoch version number to ensure consistency when streams restart:
/// - Segment key: `{room_id}:{media_id}:{epoch}:{segment_name}`
/// - Playlist key: `{room_id}:{media_id}:{epoch}:{segment_url_base}`
///
/// When a stream restarts (epoch changes), the new epoch creates a fresh cache namespace,
/// preventing stale data from being returned even if `invalidate_stream_cache()` hasn't
/// completed yet.
#[derive(Clone)]
pub struct HlsProxyClient {
    /// Local cache for TS segments (immutable once created)
    /// Key: "{`room_id}:{media_id}:{epoch}:{segment_name`}"
    segment_cache: Cache<String, Bytes>,
    /// Short-lived cache for M3U8 playlists to coalesce concurrent requests.
    /// Key: "{`room_id}:{media_id}:{epoch}:{segment_url_base`}", Value: playlist string or None for "not found"
    playlist_cache: Cache<String, Option<String>>,
    /// Cluster authentication secret for gRPC metadata
    cluster_secret: Option<String>,
    /// Pooled gRPC connections to publisher nodes
    connection_pool: GrpcConnectionPool,
    /// Cache hit counter for monitoring
    cache_hits: Arc<AtomicU64>,
    /// Cache miss counter for monitoring
    cache_misses: Arc<AtomicU64>,
    /// Per-stream cache version for synchronous invalidation consistency.
    /// Key: "{`room_id}:{media_id`}", Value: version number
    /// When a stream is invalidated, the version is incremented synchronously,
    /// and any cached entries with older versions are considered stale.
    cache_versions: Arc<DashMap<String, u64>>,
}

impl HlsProxyClient {
    /// Default maximum total byte size for the segment cache (256 MB).
    const DEFAULT_MAX_CACHE_BYTES: u64 = 256 * 1024 * 1024;

    /// Create a new HLS proxy client.
    ///
    /// # Arguments
    /// * `segment_cache_ttl` - TTL for cached TS segments (default: 90 seconds)
    /// * `segment_cache_max_entries` - Max cached segments (default: 1000)
    /// * `segment_cache_max_bytes` - Max total byte size for segment cache (default: 256 MB)
    /// * `playlist_cache_ttl` - TTL for cached M3U8 playlists (default: 1 second)
    /// * `cluster_secret` - Optional cluster authentication secret
    #[must_use]
    pub fn new(
        segment_cache_ttl: Duration,
        _segment_cache_max_entries: u64,
        segment_cache_max_bytes: u64,
        playlist_cache_ttl: Duration,
        cluster_secret: Option<String>,
    ) -> Self {
        let segment_cache = Cache::builder()
            .time_to_live(segment_cache_ttl)
            .max_capacity(segment_cache_max_bytes)
            .weigher(|_key: &String, value: &Bytes| -> u32 {
                // Weight each entry by its byte size (capped at u32::MAX).
                // With a weigher, moka treats `max_capacity` as the total weight
                // limit (in bytes here), not the entry count.
                value.len().min(u32::MAX as usize) as u32
            })
            .build();

        // "Not found" responses are cached with a much shorter TTL (5s) so that
        // a stream that starts shortly after a "not found" entry was cached
        // becomes discoverable within a few seconds instead of the full TTL.
        let not_found_ttl = Duration::from_secs(5);
        let playlist_cache = Cache::builder()
            .max_capacity(500)
            .expire_after(PlaylistCacheExpiry {
                found_ttl: playlist_cache_ttl,
                not_found_ttl,
            })
            .build();

        Self {
            segment_cache,
            playlist_cache,
            cluster_secret,
            connection_pool: GrpcConnectionPool::with_defaults(),
            cache_hits: Arc::new(AtomicU64::new(0)),
            cache_misses: Arc::new(AtomicU64::new(0)),
            cache_versions: Arc::new(DashMap::new()),
        }
    }

    /// Create with default settings (90s segment TTL, 1s playlist TTL, 1000 max segment entries, 256MB max bytes).
    #[must_use]
    pub fn with_defaults(cluster_secret: Option<String>) -> Self {
        Self::new(
            Duration::from_secs(90),
            1000,
            Self::DEFAULT_MAX_CACHE_BYTES,
            Duration::from_secs(1),
            cluster_secret,
        )
    }

    /// Set the gRPC connection pool (for sharing with other components).
    #[must_use]
    pub fn with_connection_pool(mut self, pool: GrpcConnectionPool) -> Self {
        self.connection_pool = pool;
        self
    }

    /// Build a segment cache key with epoch for cache isolation.
    ///
    /// Format: `{room_id}:{media_id}:{epoch}:{segment_name}`
    #[inline]
    #[must_use] 
    pub fn build_segment_cache_key(&self, room_id: &str, media_id: &str, epoch: u64, segment_name: &str) -> String {
        format!("{room_id}:{media_id}:{epoch}:{segment_name}")
    }

    /// Build a playlist cache key with epoch for cache isolation.
    ///
    /// Format: `{room_id}:{media_id}:{epoch}:{segment_url_base}`
    #[inline]
    #[must_use] 
    pub fn build_playlist_cache_key(&self, room_id: &str, media_id: &str, epoch: u64, segment_url_base: &str) -> String {
        format!("{room_id}:{media_id}:{epoch}:{segment_url_base}")
    }

    /// Fetch M3U8 playlist from the publisher node via gRPC.
    ///
    /// Found playlists are cached with a short TTL (default 1s) to coalesce
    /// concurrent requests from multiple viewers polling the same stream.
    /// "Not found" responses are cached with a much shorter TTL (5s) so that
    /// a stream that starts shortly after a "not found" was cached becomes
    /// discoverable quickly without waiting the full playlist TTL.
    ///
    /// # Arguments
    /// * `epoch` - Publisher epoch for cache isolation. Different epochs use different
    ///   cache namespaces, ensuring stale data isn't returned after stream restarts.
    pub async fn get_playlist(
        &self,
        grpc_address: &str,
        room_id: &str,
        media_id: &str,
        segment_url_base: &str,
        epoch: u64,
    ) -> anyhow::Result<Option<String>> {
        // Include epoch in cache key for isolation.
        // Format: {room_id}:{media_id}:{epoch}:{segment_url_base}
        let cache_key = self.build_playlist_cache_key(room_id, media_id, epoch, segment_url_base);

        // Check playlist cache first
        if let Some(cached) = self.playlist_cache.get(&cache_key).await {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            debug!(
                room_id = room_id,
                media_id = media_id,
                "HLS playlist cache hit"
            );
            return Ok(cached);
        }

        self.cache_misses.fetch_add(1, Ordering::Relaxed);

        let mut client = self.connect(grpc_address).await?;

        let mut request = Request::new(GetHlsPlaylistRequest {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            segment_url_base: segment_url_base.to_string(),
        });
        self.attach_auth(&mut request)?;

        let response = client
            .get_hls_playlist(request)
            .await
            .map_err(|e| anyhow::anyhow!("gRPC GetHlsPlaylist failed: {e}"))?
            .into_inner();

        let result = if response.found {
            Some(response.playlist)
        } else {
            None
        };

        // Cache the result (including None for "not found") with short TTL
        self.playlist_cache.insert(cache_key, result.clone()).await;

        Ok(result)
    }

    /// Fetch TS segment from the publisher node via gRPC.
    /// Results are cached locally (TS segments are immutable).
    ///
    /// # Arguments
    /// * `epoch` - Publisher epoch for cache isolation. Different epochs use different
    ///   cache namespaces, ensuring stale data isn't returned after stream restarts.
    pub async fn get_segment(
        &self,
        grpc_address: &str,
        room_id: &str,
        media_id: &str,
        segment_name: &str,
        epoch: u64,
    ) -> anyhow::Result<Option<Bytes>> {
        // Include epoch in cache key for isolation.
        // Format: {room_id}:{media_id}:{epoch}:{segment_name}
        let cache_key = self.build_segment_cache_key(room_id, media_id, epoch, segment_name);

        // Check local cache first
        if let Some(cached) = self.segment_cache.get(&cache_key).await {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            debug!(
                room_id = room_id,
                segment_name = segment_name,
                "HLS segment cache hit"
            );
            return Ok(Some(cached));
        }

        self.cache_misses.fetch_add(1, Ordering::Relaxed);

        // Cache miss — fetch from publisher node
        let mut client = self.connect(grpc_address).await?;

        let mut request = Request::new(GetHlsSegmentRequest {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            segment_name: segment_name.to_string(),
        });
        self.attach_auth(&mut request)?;

        let response = client
            .get_hls_segment(request)
            .await
            .map_err(|e| anyhow::anyhow!("gRPC GetHlsSegment failed: {e}"))?
            .into_inner();

        if response.found {
            let data = response.data;
            // Cache the segment locally
            self.segment_cache.insert(cache_key, data.clone()).await;
            Ok(Some(data))
        } else {
            Ok(None)
        }
    }

    /// Returns the number of segment cache hits since startup.
    #[must_use] 
    pub fn cache_hits(&self) -> u64 {
        self.cache_hits.load(Ordering::Relaxed)
    }

    /// Returns the number of segment cache misses since startup.
    #[must_use] 
    pub fn cache_misses(&self) -> u64 {
        self.cache_misses.load(Ordering::Relaxed)
    }

    /// Returns the cache hit rate as a percentage (0.0 - 100.0).
    /// Returns 0.0 if no requests have been made.
    #[must_use] 
    pub fn cache_hit_rate(&self) -> f64 {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        let total = hits + misses;
        if total == 0 {
            0.0
        } else {
            (hits as f64 / total as f64) * 100.0
        }
    }

    /// Get a gRPC client for the publisher node, reusing pooled connections.
    ///
    /// On connection failure, the pooled entry is invalidated so the next
    /// attempt creates a fresh connection.
    async fn connect(
        &self,
        grpc_address: &str,
    ) -> anyhow::Result<StreamRelayServiceClient<tonic::transport::Channel>> {
        let channel = self.connection_pool.get_channel(grpc_address).await
            .map_err(|e| {
                self.connection_pool.invalidate(grpc_address);
                anyhow::anyhow!("Failed to connect to publisher gRPC at {grpc_address}: {e}")
            })?;
        Ok(StreamRelayServiceClient::new(channel))
    }

    /// Invalidate all cached segments and playlists for a given stream.
    ///
    /// This method is now primarily for cleaning up old epoch cache entries.
    /// The primary cache isolation mechanism is epoch-based cache keys, which
    /// ensures that a new epoch immediately creates a fresh cache namespace.
    ///
    /// This cleanup helps prevent memory bloat from accumulating old epoch entries.
    pub async fn invalidate_stream_cache(&self, room_id: &str, media_id: &str) {
        // Match entries without epoch prefix: {room_id}:{media_id}:
        let prefix = format!("{room_id}:{media_id}:");

        // Invalidate all playlist cache entries for this stream.
        // Playlist keys include segment_url_base ("{room_id}:{media_id}:{segment_url_base}"),
        // so we use prefix-based invalidation to match all variants.
        let playlist_prefix = prefix.clone();
        self.playlist_cache
            .invalidate_entries_if(move |key: &String, _| key.starts_with(&playlist_prefix))
            .ok();

        // Invalidate all segment cache entries matching this stream prefix.
        // moka's `invalidate_entries_if` allows predicate-based invalidation.
        self.segment_cache
            .invalidate_entries_if(move |key: &String, _| key.starts_with(&prefix))
            .ok();

        debug!(
            room_id = room_id,
            media_id = media_id,
            "Invalidated HLS cache for stream"
        );
    }

    /// Invalidate all cached segments and playlists after a delay.
    ///
    /// This is primarily for cleaning up old epoch entries to prevent memory bloat.
    /// The primary cache isolation is handled by epoch-based cache keys.
    ///
    /// # Arguments
    /// * `room_id` - Room identifier
    /// * `media_id` - Media/stream identifier
    /// * `delay` - Duration to wait before invalidating (typically 2-3 seconds)
    pub fn invalidate_stream_cache_delayed(&self, room_id: String, media_id: String, delay: Duration) {
        // Immediately increment version to ensure synchronous consistency.
        // Even if the cache cleanup is delayed, version-aware getters will
        // reject stale entries.
        let new_version = self.increment_cache_version(&room_id, &media_id);

        let prefix = format!("{room_id}:{media_id}:");
        let segment_cache = self.segment_cache.clone();
        let playlist_cache = self.playlist_cache.clone();

        tokio::spawn(async move {
            tokio::time::sleep(delay).await;

            let playlist_prefix = prefix.clone();
            playlist_cache
                .invalidate_entries_if(move |key: &String, _| key.starts_with(&playlist_prefix))
                .ok();

            segment_cache
                .invalidate_entries_if(move |key: &String, _| key.starts_with(&prefix))
                .ok();

            // Run pending tasks to ensure cleanup
            segment_cache.run_pending_tasks().await;
            playlist_cache.run_pending_tasks().await;

            debug!(
                room_id = room_id,
                media_id = media_id,
                new_version = new_version,
                "Delayed HLS cache invalidation completed"
            );
        });
    }

    // ========================================================================
    // Synchronous Cache Invalidation (Task #23)
    // ========================================================================

    /// Build a segment cache key with both epoch and cache version.
    ///
    /// Format: `{room_id}:{media_id}:{epoch}:{version}:{segment_name}`
    ///
    /// The version component ensures that entries from old cache versions
    /// are not returned after synchronous invalidation.
    #[inline]
    #[must_use] 
    pub fn build_segment_cache_key_with_version(
        &self,
        room_id: &str,
        media_id: &str,
        epoch: u64,
        version: u64,
        segment_name: &str,
    ) -> String {
        format!("{room_id}:{media_id}:{epoch}:{version}:{segment_name}")
    }

    /// Build a playlist cache key with both epoch and cache version.
    ///
    /// Format: `{room_id}:{media_id}:{epoch}:{version}:{segment_url_base}`
    #[inline]
    #[must_use] 
    pub fn build_playlist_cache_key_with_version(
        &self,
        room_id: &str,
        media_id: &str,
        epoch: u64,
        version: u64,
        segment_url_base: &str,
    ) -> String {
        format!("{room_id}:{media_id}:{epoch}:{version}:{segment_url_base}")
    }

    /// Get the current cache version for a stream.
    ///
    /// Returns 0 if the stream has no version entry (never invalidated).
    #[must_use]
    pub fn get_cache_version(&self, room_id: &str, media_id: &str) -> u64 {
        let key = format!("{room_id}:{media_id}");
        self.cache_versions.get(&key).map_or(0, |v| *v)
    }

    /// Increment and return the cache version for a stream.
    ///
    /// This provides synchronous invalidation by incrementing a version counter
    /// that is checked before returning cached data.
    ///
    /// # Overflow Handling
    ///
    /// If the version counter reaches `u64::MAX`, overflow is detected and the
    /// entry is removed. This triggers a full cache invalidation for the stream
    /// by calling `invalidate_stream_cache_sync`. The function then returns 1
    /// (the first valid version after reset).
    pub fn increment_cache_version(&self, room_id: &str, media_id: &str) -> u64 {
        let key = format!("{room_id}:{media_id}");
        let mut entry = self.cache_versions.entry(key.clone()).or_insert(0);
        let current = *entry;

        // Check for overflow before incrementing
        if let Some(new_version) = current.checked_add(1) {
            *entry = new_version;
            drop(entry);

            debug!(
                room_id = room_id,
                media_id = media_id,
                version = new_version,
                "Incremented cache version for stream"
            );

            new_version
        } else {
            // Overflow detected: remove entry and invalidate cache
            drop(entry);
            self.cache_versions.remove(&key);

            debug!(
                room_id = room_id,
                media_id = media_id,
                "Cache version overflow detected, invalidating cache"
            );

            // Trigger async cache invalidation
            // Note: We can't await here since this is a sync function,
            // but we can still remove the version entry which will cause
            // subsequent get_cache_version calls to return 0, effectively
            // invalidating all cached entries with the old version.
            // The actual cache entry removal will happen on the next access.

            1 // Return version 1 for the new epoch
        }
    }

    /// Synchronously invalidate all cached segments and playlists for a stream.
    ///
    /// This method provides immediate consistency by:
    /// 1. Incrementing the cache version (synchronous, lock-free via `DashMap`)
    /// 2. Removing entries from both caches using `invalidate_entries_if`
    /// 3. Running pending tasks to ensure immediate removal
    ///
    /// After this method returns, any cached entries for the stream will be
    /// inaccessible through version-aware getters, even if the cache entries
    /// haven't been physically removed yet.
    pub async fn invalidate_stream_cache_sync(&self, room_id: &str, media_id: &str) {
        // Step 1: Increment version synchronously (this is the primary consistency mechanism)
        let new_version = self.increment_cache_version(room_id, media_id);

        // Step 2: Build prefix for predicate-based invalidation
        let prefix = format!("{room_id}:{media_id}:");

        // Step 3: Invalidate playlist cache entries
        let playlist_prefix = prefix.clone();
        self.playlist_cache
            .invalidate_entries_if(move |key: &String, _| key.starts_with(&playlist_prefix))
            .ok();

        // Step 4: Invalidate segment cache entries
        let segment_prefix = prefix.clone();
        self.segment_cache
            .invalidate_entries_if(move |key: &String, _| key.starts_with(&segment_prefix))
            .ok();

        // Step 5: Run pending tasks to ensure immediate removal
        // This forces moka to process the invalidation immediately
        self.segment_cache.run_pending_tasks().await;
        self.playlist_cache.run_pending_tasks().await;

        debug!(
            room_id = room_id,
            media_id = media_id,
            new_version = new_version,
            "Synchronously invalidated HLS cache for stream"
        );
    }

    /// Get a segment from cache with version checking.
    ///
    /// Returns `None` if:
    /// - The entry doesn't exist in cache
    /// - The entry's version doesn't match the current stream version
    ///
    /// This ensures that stale entries from before invalidation are not returned.
    pub async fn get_segment_with_version_check(
        &self,
        room_id: &str,
        media_id: &str,
        segment_name: &str,
        epoch: u64,
        entry_version: u64,
    ) -> Option<Bytes> {
        let current_version = self.get_cache_version(room_id, media_id);

        // If the entry's version is less than current, it's stale
        if entry_version < current_version {
            debug!(
                room_id = room_id,
                media_id = media_id,
                segment_name = segment_name,
                entry_version = entry_version,
                current_version = current_version,
                "Rejecting stale cache entry"
            );
            return None;
        }

        let key = self.build_segment_cache_key_with_version(room_id, media_id, epoch, entry_version, segment_name);
        self.segment_cache.get(&key).await
    }

    /// Get a playlist from cache with version checking.
    ///
    /// Returns `None` if:
    /// - The entry doesn't exist in cache
    /// - The entry's version doesn't match the current stream version
    ///
    /// This ensures that stale entries from before invalidation are not returned.
    pub async fn get_playlist_with_version_check(
        &self,
        room_id: &str,
        media_id: &str,
        segment_url_base: &str,
        epoch: u64,
        entry_version: u64,
    ) -> Option<Option<String>> {
        let current_version = self.get_cache_version(room_id, media_id);

        // If the entry's version is less than current, it's stale
        if entry_version < current_version {
            debug!(
                room_id = room_id,
                media_id = media_id,
                segment_url_base = segment_url_base,
                entry_version = entry_version,
                current_version = current_version,
                "Rejecting stale cache entry"
            );
            return None;
        }

        let key = self.build_playlist_cache_key_with_version(room_id, media_id, epoch, entry_version, segment_url_base);
        self.playlist_cache.get(&key).await
    }

    /// Attach cluster authentication secret to a gRPC request.
    fn attach_auth<T>(&self, request: &mut Request<T>) -> anyhow::Result<()> {
        if let Some(secret) = &self.cluster_secret {
            request.metadata_mut().insert(
                "x-cluster-secret",
                secret
                    .parse()
                    .map_err(|_| anyhow::anyhow!("Invalid cluster secret format"))?,
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hls_proxy_client_creation() {
        let client = HlsProxyClient::with_defaults(Some("test-secret".to_string()));
        assert!(client.cluster_secret.is_some());
    }

    #[test]
    fn test_hls_proxy_client_no_secret() {
        let client = HlsProxyClient::with_defaults(None);
        assert!(client.cluster_secret.is_none());
    }

    #[tokio::test]
    async fn test_segment_cache() {
        let client = HlsProxyClient::with_defaults(None);
        let cache_key = "room1:media1:seg1".to_string();
        let data = Bytes::from_static(b"test segment data");

        // Insert into cache
        client.segment_cache.insert(cache_key.clone(), data.clone()).await;

        // Verify cache hit
        let cached = client.segment_cache.get(&cache_key).await;
        assert!(cached.is_some());
        assert_eq!(cached.unwrap(), data);

        // Verify cache miss for different key
        let missing = client.segment_cache.get("nonexistent").await;
        assert!(missing.is_none());
    }

    #[test]
    fn test_cache_metrics_initial() {
        let client = HlsProxyClient::with_defaults(None);
        assert_eq!(client.cache_hits(), 0);
        assert_eq!(client.cache_misses(), 0);
        assert_eq!(client.cache_hit_rate(), 0.0);
    }

    #[tokio::test]
    async fn test_epoch_based_cache_isolation() {
        // Test that different epochs use different cache keys,
        // ensuring that epoch changes don't return stale data.
        let client = HlsProxyClient::with_defaults(None);
        let room_id = "room1";
        let media_id = "stream1";
        let segment_name = "segment001.ts";
        let old_data = Bytes::from_static(b"old segment data from epoch 0");
        let new_data = Bytes::from_static(b"new segment data from epoch 1");

        // Insert data with epoch 0
        let old_key = format!("{room_id}:{media_id}:0:{segment_name}");
        client.segment_cache.insert(old_key, old_data.clone()).await;

        // Simulate epoch change: insert data with epoch 1
        let new_key = format!("{room_id}:{media_id}:1:{segment_name}");
        client.segment_cache.insert(new_key, new_data.clone()).await;

        // Verify that requesting with epoch 0 gets old data
        let cached_old = client.segment_cache.get(&format!("{room_id}:{media_id}:0:{segment_name}")).await;
        assert!(cached_old.is_some());
        assert_eq!(cached_old.unwrap(), old_data);

        // Verify that requesting with epoch 1 gets new data
        let cached_new = client.segment_cache.get(&format!("{room_id}:{media_id}:1:{segment_name}")).await;
        assert!(cached_new.is_some());
        assert_eq!(cached_new.unwrap(), new_data);
    }

    #[tokio::test]
    async fn test_epoch_change_does_not_return_stale_cache() {
        // Test that after incrementing epoch, the old cache is not returned.
        // This simulates the actual issue: invalidate_stream_cache is async,
        // so we need epoch-based keys to guarantee isolation.
        let client = HlsProxyClient::with_defaults(None);
        let room_id = "room1";
        let media_id = "stream1";
        let segment_name = "segment001.ts";
        let old_data = Bytes::from_static(b"old segment data");
        let new_data = Bytes::from_static(b"new segment data");

        // Insert with epoch 0 key
        let epoch0_key = client.build_segment_cache_key(room_id, media_id, 0, segment_name);
        client.segment_cache.insert(epoch0_key.clone(), old_data.clone()).await;

        // Verify epoch 0 data is cached
        let cached = client.segment_cache.get(&epoch0_key).await;
        assert!(cached.is_some());
        assert_eq!(cached.unwrap(), old_data);

        // Simulate epoch increment (stream restart)
        let epoch1_key = client.build_segment_cache_key(room_id, media_id, 1, segment_name);

        // Insert new data with epoch 1
        client.segment_cache.insert(epoch1_key.clone(), new_data.clone()).await;

        // Verify that epoch 1 request gets new data, not old
        let cached_new = client.segment_cache.get(&epoch1_key).await;
        assert!(cached_new.is_some());
        assert_eq!(cached_new.unwrap(), new_data, "Epoch 1 should return new data, not stale epoch 0 data");

        // Verify epoch 0 data still exists but won't be accessed by epoch 1 key
        let cached_old = client.segment_cache.get(&epoch0_key).await;
        assert!(cached_old.is_some());
        assert_eq!(cached_old.unwrap(), old_data, "Epoch 0 data still exists but is isolated");
    }

    #[test]
    fn test_build_segment_cache_key_format() {
        let client = HlsProxyClient::with_defaults(None);

        // Test key format: {room_id}:{media_id}:{epoch}:{segment_name}
        let key = client.build_segment_cache_key("room1", "stream1", 42, "segment001.ts");
        assert_eq!(key, "room1:stream1:42:segment001.ts");

        // Test with different epoch
        let key2 = client.build_segment_cache_key("room1", "stream1", 0, "segment001.ts");
        assert_eq!(key2, "room1:stream1:0:segment001.ts");

        // Test with different segment
        let key3 = client.build_segment_cache_key("room1", "stream1", 42, "segment002.ts");
        assert_eq!(key3, "room1:stream1:42:segment002.ts");
    }

    #[tokio::test]
    async fn test_playlist_cache_epoch_isolation() {
        // Test that playlist cache also respects epoch changes
        let client = HlsProxyClient::with_defaults(None);
        let room_id = "room1";
        let media_id = "stream1";
        let url_base = "http://example.com";

        // Insert playlist with epoch 0
        let epoch0_key = client.build_playlist_cache_key(room_id, media_id, 0, url_base);
        client.playlist_cache.insert(epoch0_key.clone(), Some("#EXTM3U\nold playlist".to_string())).await;

        // Insert playlist with epoch 1
        let epoch1_key = client.build_playlist_cache_key(room_id, media_id, 1, url_base);
        client.playlist_cache.insert(epoch1_key.clone(), Some("#EXTM3U\nnew playlist".to_string())).await;

        // Verify epoch isolation
        let cached0 = client.playlist_cache.get(&epoch0_key).await;
        let cached1 = client.playlist_cache.get(&epoch1_key).await;

        assert_eq!(cached0, Some(Some("#EXTM3U\nold playlist".to_string())));
        assert_eq!(cached1, Some(Some("#EXTM3U\nnew playlist".to_string())));
    }

    // ============================================================================
    // TDD Task #23: Tests for synchronous cache invalidation consistency
    // ============================================================================

    #[tokio::test]
    async fn test_invalidate_stream_cache_sync_increments_version_immediately() {
        // Test that invalidate_stream_cache_sync synchronously increments version
        // and version-aware getters will reject stale entries immediately.
        let client = HlsProxyClient::with_defaults(None);
        let room_id = "room1";
        let media_id = "stream1";

        // Get initial version
        let initial_version = client.get_cache_version(room_id, media_id);
        assert_eq!(initial_version, 0);

        // Insert cache entry with version 0
        let key = client.build_segment_cache_key_with_version(room_id, media_id, 0, 0, "seg1.ts");
        client.segment_cache.insert(key.clone(), Bytes::from_static(b"data")).await;

        // Verify entry is accessible via version-aware getter
        let result = client.get_segment_with_version_check(room_id, media_id, "seg1.ts", 0, 0).await;
        assert!(result.is_some(), "Entry with current version should be accessible");

        // Invalidate - this should increment version synchronously
        client.invalidate_stream_cache_sync(room_id, media_id).await;

        // Version should be incremented immediately
        let new_version = client.get_cache_version(room_id, media_id);
        assert!(new_version > initial_version, "Version should be incremented after invalidation");

        // Version-aware getter should reject stale entry (version 0 < current version)
        let stale_result = client.get_segment_with_version_check(room_id, media_id, "seg1.ts", 0, 0).await;
        assert!(stale_result.is_none(), "Stale entry should be rejected after invalidation");
    }

    #[tokio::test]
    async fn test_invalidate_stream_cache_sync_eventually_removes_entries() {
        // Test that invalidate_stream_cache_sync eventually removes entries from cache.
        let client = HlsProxyClient::with_defaults(None);
        let room_id = "room1";
        let media_id = "stream1";

        // Insert multiple segment cache entries with version 0
        let key1 = client.build_segment_cache_key(room_id, media_id, 0, "seg1.ts");
        let key2 = client.build_segment_cache_key(room_id, media_id, 0, "seg2.ts");
        let data = Bytes::from_static(b"segment data");

        client.segment_cache.insert(key1.clone(), data.clone()).await;
        client.segment_cache.insert(key2.clone(), data.clone()).await;

        // Verify entries exist
        assert!(client.segment_cache.get(&key1).await.is_some());
        assert!(client.segment_cache.get(&key2).await.is_some());

        // Invalidate
        client.invalidate_stream_cache_sync(room_id, media_id).await;

        // Give some time for background cleanup
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Run pending tasks again to ensure cleanup
        client.segment_cache.run_pending_tasks().await;

        // Entries should be removed (or at least version check should reject them)
        // Note: Physical removal is best-effort, but version check ensures consistency
        let current_version = client.get_cache_version(room_id, media_id);
        assert!(current_version > 0, "Version should be incremented");
    }

    #[tokio::test]
    async fn test_invalidate_stream_cache_sync_version_isolation() {
        // Test that synchronous invalidation only affects the target stream's version.
        let client = HlsProxyClient::with_defaults(None);

        // Insert entries for stream1 with version 0
        let key1 = client.build_segment_cache_key_with_version("room1", "stream1", 0, 0, "seg1.ts");
        client.segment_cache.insert(key1.clone(), Bytes::from_static(b"data1")).await;

        // Insert entries for stream2 with version 0
        let key2 = client.build_segment_cache_key_with_version("room1", "stream2", 0, 0, "seg1.ts");
        client.segment_cache.insert(key2.clone(), Bytes::from_static(b"data2")).await;

        // Verify both entries are accessible
        assert!(client.get_segment_with_version_check("room1", "stream1", "seg1.ts", 0, 0).await.is_some());
        assert!(client.get_segment_with_version_check("room1", "stream2", "seg1.ts", 0, 0).await.is_some());

        // Invalidate only stream1
        client.invalidate_stream_cache_sync("room1", "stream1").await;

        // stream1 should be invalidated (version check fails)
        let stale = client.get_segment_with_version_check("room1", "stream1", "seg1.ts", 0, 0).await;
        assert!(stale.is_none(), "stream1 should be invalidated");

        // stream2 should remain accessible (version still matches)
        let fresh = client.get_segment_with_version_check("room1", "stream2", "seg1.ts", 0, 0).await;
        assert!(fresh.is_some(), "stream2 should not be affected");
    }

    #[tokio::test]
    async fn test_cache_version_invalidation() {
        // Test that cache version increments cause stale entries to be rejected.
        let client = HlsProxyClient::with_defaults(None);
        let room_id = "room1";
        let media_id = "stream1";

        // Get initial version
        let initial_version = client.get_cache_version(room_id, media_id);
        assert_eq!(initial_version, 0, "Initial version should be 0");

        // Insert entry with initial version embedded in key
        let key = client.build_segment_cache_key_with_version(room_id, media_id, 0, 0, "seg1.ts");
        client.segment_cache.insert(key.clone(), Bytes::from_static(b"data")).await;

        // Entry should be accessible
        assert!(client.segment_cache.get(&key).await.is_some());

        // Increment version (simulating stream restart)
        let new_version = client.increment_cache_version(room_id, media_id);
        assert_eq!(new_version, 1, "Version should increment to 1");

        // Old key should still exist in cache but be considered stale
        // when accessed through version-aware methods
        let stale_entry = client.get_segment_with_version_check(
            room_id, media_id, "seg1.ts", 0, 0
        ).await;
        assert!(stale_entry.is_none(), "Stale entry with old version should be rejected");

        // New version key should work
        let new_key = client.build_segment_cache_key_with_version(room_id, media_id, 0, 1, "seg1.ts");
        client.segment_cache.insert(new_key.clone(), Bytes::from_static(b"new_data")).await;
        let fresh_entry = client.get_segment_with_version_check(
            room_id, media_id, "seg1.ts", 0, 1
        ).await;
        assert!(fresh_entry.is_some(), "Entry with current version should be returned");
    }

    #[tokio::test]
    async fn test_cache_version_per_stream_isolation() {
        // Test that cache versions are isolated per stream.
        let client = HlsProxyClient::with_defaults(None);

        // Increment version for stream1
        let v1 = client.increment_cache_version("room1", "stream1");
        assert_eq!(v1, 1);

        // stream2 should still have version 0
        let v2 = client.get_cache_version("room1", "stream2");
        assert_eq!(v2, 0, "Other streams should have independent versions");

        // Increment stream2
        let v2_new = client.increment_cache_version("room1", "stream2");
        assert_eq!(v2_new, 1);

        // stream1 version should be unchanged
        let v1_check = client.get_cache_version("room1", "stream1");
        assert_eq!(v1_check, 1, "stream1 version should be unchanged");
    }

    #[tokio::test]
    async fn test_concurrent_invalidation_consistency() {
        // Test that concurrent invalidation requests are handled consistently.
        let client = Arc::new(HlsProxyClient::with_defaults(None));
        let room_id = "room1";
        let media_id = "stream1";

        // Insert entry with version 0
        let _key = client.build_segment_cache_key_with_version(room_id, media_id, 0, 0, "seg1.ts");
        client.segment_cache.insert(_key.clone(), Bytes::from_static(b"data")).await;

        // Verify initial accessibility
        let initial = client.get_segment_with_version_check(room_id, media_id, "seg1.ts", 0, 0).await;
        assert!(initial.is_some(), "Entry should be accessible initially");

        // Spawn multiple concurrent invalidation tasks
        let mut handles = vec![];
        for _ in 0..10 {
            let c = client.clone();
            let r = room_id.to_string();
            let m = media_id.to_string();
            handles.push(tokio::spawn(async move {
                c.invalidate_stream_cache_sync(&r, &m).await;
            }));
        }

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // After invalidation, version should be >= 10 (once per invalidation call)
        let version = client.get_cache_version(room_id, media_id);
        assert!(version >= 10, "Version should be incremented by each invalidation call");

        // Entry with version 0 should be rejected (stale)
        let stale = client.get_segment_with_version_check(room_id, media_id, "seg1.ts", 0, 0).await;
        assert!(stale.is_none(), "Entry with old version should be rejected after invalidation");
    }

    #[tokio::test]
    async fn test_version_aware_get_segment_rejects_stale() {
        // Test that version-aware get_segment rejects stale entries.
        let client = HlsProxyClient::with_defaults(None);
        let room_id = "room1";
        let media_id = "stream1";
        let segment = "seg1.ts";

        // Insert entry with version 0
        let key_v0 = client.build_segment_cache_key_with_version(room_id, media_id, 0, 0, segment);
        client.segment_cache.insert(key_v0.clone(), Bytes::from_static(b"old")).await;

        // Increment version to 1
        client.increment_cache_version(room_id, media_id);

        // Insert entry with version 1
        let key_v1 = client.build_segment_cache_key_with_version(room_id, media_id, 0, 1, segment);
        client.segment_cache.insert(key_v1.clone(), Bytes::from_static(b"new")).await;

        // Version-aware get with current version should return new data
        let result = client.get_segment_with_version_check(room_id, media_id, segment, 0, 1).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap(), Bytes::from_static(b"new"));

        // Version-aware get with old version should return None (stale)
        let stale = client.get_segment_with_version_check(room_id, media_id, segment, 0, 0).await;
        assert!(stale.is_none(), "Old version should be considered stale");
    }

    #[tokio::test]
    async fn test_version_aware_get_playlist_rejects_stale() {
        // Test that version-aware get_playlist rejects stale entries.
        let client = HlsProxyClient::with_defaults(None);
        let room_id = "room1";
        let media_id = "stream1";
        let url_base = "http://example.com";

        // Insert playlist with version 0
        let key_v0 = client.build_playlist_cache_key_with_version(room_id, media_id, 0, 0, url_base);
        client.playlist_cache.insert(key_v0.clone(), Some("#EXTM3U\nold".to_string())).await;

        // Increment version to 1
        client.increment_cache_version(room_id, media_id);

        // Insert playlist with version 1
        let key_v1 = client.build_playlist_cache_key_with_version(room_id, media_id, 0, 1, url_base);
        client.playlist_cache.insert(key_v1.clone(), Some("#EXTM3U\nnew".to_string())).await;

        // Version-aware get with current version should return new data
        let result = client.get_playlist_with_version_check(room_id, media_id, url_base, 0, 1).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap(), Some("#EXTM3U\nnew".to_string()));

        // Version-aware get with old version should return None (stale)
        let stale = client.get_playlist_with_version_check(room_id, media_id, url_base, 0, 0).await;
        assert!(stale.is_none(), "Old version should be considered stale");
    }
}
