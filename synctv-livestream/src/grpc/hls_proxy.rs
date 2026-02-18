// HLS proxy client for cross-node HLS streaming
//
// Non-publisher nodes use this client to fetch M3U8 playlists and TS segments
// from the publisher node via gRPC. TS segments are cached locally since they
// are immutable once created. M3U8 playlists are NOT cached since they change
// frequently as new segments are generated.

use bytes::Bytes;
use moka::future::Cache;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tonic::Request;
use tracing::debug;

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
    /// Key: "{room_id}:{media_id}:{segment_name}"
    segment_cache: Cache<String, Bytes>,
    /// Short-lived cache for M3U8 playlists to coalesce concurrent requests.
    /// Key: "{room_id}:{media_id}", Value: playlist string or empty for "not found"
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
    /// Create a new HLS proxy client.
    ///
    /// # Arguments
    /// * `segment_cache_ttl` - TTL for cached TS segments (default: 90 seconds)
    /// * `segment_cache_max_entries` - Max cached segments (default: 1000)
    /// * `playlist_cache_ttl` - TTL for cached M3U8 playlists (default: 1 second)
    /// * `cluster_secret` - Optional cluster authentication secret
    pub fn new(
        segment_cache_ttl: Duration,
        segment_cache_max_entries: u64,
        playlist_cache_ttl: Duration,
        cluster_secret: Option<String>,
    ) -> Self {
        let segment_cache = Cache::builder()
            .time_to_live(segment_cache_ttl)
            .max_capacity(segment_cache_max_entries)
            .build();

        let playlist_cache = Cache::builder()
            .time_to_live(playlist_cache_ttl)
            .max_capacity(500)
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

    /// Create with default settings (90s segment TTL, 1s playlist TTL, 1000 max segment entries).
    pub fn with_defaults(cluster_secret: Option<String>) -> Self {
        Self::new(
            Duration::from_secs(90),
            1000,
            Duration::from_secs(1),
            cluster_secret,
        )
    }

    /// Fetch M3U8 playlist from the publisher node via gRPC.
    ///
    /// Playlists are cached with a short TTL (default 1s) to coalesce concurrent
    /// requests from multiple viewers polling the same stream. This significantly
    /// reduces gRPC calls under load while still picking up new segments promptly.
    pub async fn get_playlist(
        &self,
        grpc_address: &str,
        room_id: &str,
        media_id: &str,
        segment_url_base: &str,
    ) -> anyhow::Result<Option<String>> {
        let cache_key = format!("{room_id}:{media_id}");

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
            let data = Bytes::from(response.data);
            // Cache the segment locally
            self.segment_cache.insert(cache_key, data.clone()).await;
            Ok(Some(data))
        } else {
            Ok(None)
        }
    }

    /// Returns the number of segment cache hits since startup.
    pub fn cache_hits(&self) -> u64 {
        self.cache_hits.load(Ordering::Relaxed)
    }

    /// Returns the number of segment cache misses since startup.
    pub fn cache_misses(&self) -> u64 {
        self.cache_misses.load(Ordering::Relaxed)
    }

    /// Returns the cache hit rate as a percentage (0.0 - 100.0).
    /// Returns 0.0 if no requests have been made.
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
        let playlist_key = format!("{room_id}:{media_id}");

        // Invalidate playlist cache entry
        self.playlist_cache.invalidate(&playlist_key).await;

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
