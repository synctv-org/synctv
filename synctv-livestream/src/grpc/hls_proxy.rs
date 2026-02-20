// HLS proxy client for cross-node HLS streaming
//
// Non-publisher nodes use this client to fetch M3U8 playlists and TS segments
// from the publisher node via gRPC. TS segments are cached locally since they
// are immutable once created. M3U8 playlists are NOT cached since they change
// frequently as new segments are generated.

use bytes::Bytes;
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
#[derive(Clone)]
pub struct HlsProxyClient {
    /// Local cache for TS segments (immutable once created)
    /// Key: "{`room_id}:{media_id}:{segment_name`}"
    segment_cache: Cache<String, Bytes>,
    /// Short-lived cache for M3U8 playlists to coalesce concurrent requests.
    /// Key: "{`room_id}:{media_id`}", Value: playlist string or empty for "not found"
    playlist_cache: Cache<String, Option<String>>,
    /// Cluster authentication secret for gRPC metadata
    cluster_secret: Option<String>,
    /// Pooled gRPC connections to publisher nodes
    connection_pool: GrpcConnectionPool,
    /// Cache hit counter for monitoring
    cache_hits: Arc<AtomicU64>,
    /// Cache miss counter for monitoring
    cache_misses: Arc<AtomicU64>,
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

    /// Fetch M3U8 playlist from the publisher node via gRPC.
    ///
    /// Found playlists are cached with a short TTL (default 1s) to coalesce
    /// concurrent requests from multiple viewers polling the same stream.
    /// "Not found" responses are cached with a much shorter TTL (5s) so that
    /// a stream that starts shortly after a "not found" was cached becomes
    /// discoverable quickly without waiting the full playlist TTL.
    pub async fn get_playlist(
        &self,
        grpc_address: &str,
        room_id: &str,
        media_id: &str,
        segment_url_base: &str,
    ) -> anyhow::Result<Option<String>> {
        // Include segment_url_base in cache key so different frontend domains
        // get correctly-generated playlists instead of serving a cached playlist
        // with the wrong base URL.
        let cache_key = format!("{room_id}:{media_id}:{segment_url_base}");

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
    pub async fn get_segment(
        &self,
        grpc_address: &str,
        room_id: &str,
        media_id: &str,
        segment_name: &str,
    ) -> anyhow::Result<Option<Bytes>> {
        let cache_key = format!("{room_id}:{media_id}:{segment_name}");

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
    /// Should be called when a publisher epoch changes (e.g., stream restarts)
    /// to prevent serving stale TS segments from a previous publish session.
    /// Without this, the 90s segment TTL could serve old data to viewers after
    /// the publisher reconnects with new content.
    pub async fn invalidate_stream_cache(&self, room_id: &str, media_id: &str) {
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
}
