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
/// TS segments are cached locally with a configurable TTL (default 90s, matching
/// the Cache-Control header on segment HTTP responses). M3U8 playlists are never
/// cached because they change with every new segment.
///
/// gRPC connections to publisher nodes are pooled via [`GrpcConnectionPool`] to
/// avoid the overhead of creating a new HTTP/2 connection per request.
#[derive(Clone)]
pub struct HlsProxyClient {
    /// Local cache for TS segments (immutable once created)
    /// Key: "{room_id}:{media_id}:{segment_name}"
    segment_cache: Cache<String, Bytes>,
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
    /// * `cluster_secret` - Optional cluster authentication secret
    pub fn new(
        segment_cache_ttl: Duration,
        segment_cache_max_entries: u64,
        cluster_secret: Option<String>,
    ) -> Self {
        let segment_cache = Cache::builder()
            .time_to_live(segment_cache_ttl)
            .max_capacity(segment_cache_max_entries)
            .build();

        Self {
            segment_cache,
            cluster_secret,
            connection_pool: GrpcConnectionPool::with_defaults(),
            cache_hits: Arc::new(AtomicU64::new(0)),
            cache_misses: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Create with default settings (90s TTL, 1000 max entries).
    pub fn with_defaults(cluster_secret: Option<String>) -> Self {
        Self::new(
            Duration::from_secs(90),
            1000,
            cluster_secret,
        )
    }

    /// Fetch M3U8 playlist from the publisher node via gRPC.
    /// Playlists are NOT cached (they change frequently).
    pub async fn get_playlist(
        &self,
        grpc_address: &str,
        room_id: &str,
        media_id: &str,
        segment_url_base: &str,
    ) -> anyhow::Result<Option<String>> {
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

        if response.found {
            Ok(Some(response.playlist))
        } else {
            Ok(None)
        }
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
