// HLS proxy client for cross-node HLS streaming
// Non-publisher nodes use this client to fetch M3U8 playlists and TS segments
// from the publisher node via gRPC. TS segments are cached locally since they
// are immutable once created. M3U8 playlists are NOT cached since they change
// frequently as new segments are generated.

use bytes::Bytes;
use moka::future::Cache;
use std::fmt::Write as _;
use std::time::Duration;
use tonic::codec::CompressionEncoding;
use tonic::Request;
use tracing::debug;

use crate::util::{
    validate_hls_segment_name, validate_hls_segment_url_base, validate_hls_segment_url_suffix,
    validate_stream_ids,
};

use super::connection_pool::GrpcConnectionPool;
use super::proto::{
    stream_relay_service_client::StreamRelayServiceClient, GetHlsPlaylistRequest,
    GetHlsSegmentRequest,
};

#[derive(Debug)]
enum SegmentFetchError {
    Missing,
    Failed(String),
}

/// HLS proxy client that fetches playlists and segments from publisher nodes via gRPC.
///
/// TS segments are cached locally with a short TTL because they are immutable
/// once created. M3U8 playlists are fetched fresh because they change frequently.
///
/// gRPC connections to publisher nodes are pooled via [`GrpcConnectionPool`] to
/// avoid the overhead of creating a new HTTP/2 connection per request.
///
/// # Cache Key Format
///
/// Cache keys include the publisher epoch. Every
/// string component is length-prefixed before concatenation so delimiters inside
/// a component cannot collide with stream boundaries:
///
/// - Segment key: `|{len}:{room_id}|{len}:{media_id}|seg|{epoch}|{len}:{segment_name}`
///   Stream restarts create fresh cache namespaces by changing the epoch component
///   in the key.
#[derive(Clone)]
pub(crate) struct HlsProxyClient {
    /// Local cache for TS segments (immutable once created)
    /// Key uses length-prefixed room/media/segment components.
    segment_cache: Cache<String, Bytes>,
    /// Cluster authentication secret for gRPC metadata
    cluster_secret: Option<String>,
    /// Maximum decoded gRPC message size for playlist and segment responses.
    grpc_max_message_size_bytes: usize,
    /// Whether cross-node HLS relay clients should negotiate gzip compression.
    grpc_compression_enabled: bool,
    /// Pooled gRPC connections to publisher nodes
    connection_pool: GrpcConnectionPool,
}

impl HlsProxyClient {
    const GRPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
    /// Default maximum total byte size for the segment cache (512 MB).
    const DEFAULT_MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;

    /// Create a new HLS proxy client.
    ///
    /// # Arguments
    /// * `segment_cache_ttl` - TTL for cached TS segments (default: 90 seconds)
    /// * `segment_cache_max_bytes` - Max total byte size for segment cache (default: 512 MB).
    ///   The cache uses a byte-based weigher so this is a hard memory bound, not an entry count.
    /// * `cluster_secret` - Optional cluster authentication secret
    #[must_use]
    fn new(
        segment_cache_ttl: Duration,
        segment_cache_max_bytes: u64,
        cluster_secret: Option<String>,
    ) -> Self {
        let segment_cache = Cache::builder()
            .time_to_live(segment_cache_ttl)
            .max_capacity(segment_cache_max_bytes)
            .weigher(|key: &String, value: &Bytes| -> u32 {
                // Weight each entry by key + value byte size (capped at u32::MAX).
                // With a weigher, moka treats `max_capacity` as the total weight
                // limit (in bytes here), not the entry count.
                // Include key size for more accurate memory accounting.
                let total = key.len().saturating_add(value.len());
                u32::try_from(total.min(usize::try_from(u32::MAX).unwrap_or(usize::MAX)))
                    .unwrap_or(u32::MAX)
            })
            .build();

        Self {
            segment_cache,
            cluster_secret,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            connection_pool: GrpcConnectionPool::with_defaults(),
        }
    }

    /// Create with default settings (90s segment TTL, 512 MB max segment bytes).
    #[must_use]
    pub(crate) fn with_defaults(cluster_secret: Option<String>) -> Self {
        Self::new(
            Duration::from_secs(90),
            Self::DEFAULT_MAX_CACHE_BYTES,
            cluster_secret,
        )
    }

    /// Set the gRPC connection pool (for sharing with other components).
    #[must_use]
    pub(crate) fn with_connection_pool(mut self, pool: GrpcConnectionPool) -> Self {
        self.connection_pool = pool;
        self
    }

    /// Set the maximum decoded gRPC message size.
    #[must_use]
    pub(crate) const fn with_grpc_max_message_size(
        mut self,
        max_message_size_bytes: usize,
    ) -> Self {
        self.grpc_max_message_size_bytes = max_message_size_bytes;
        self
    }

    /// Enable or disable gzip compression negotiation for cross-node HLS relay calls.
    #[must_use]
    pub(crate) const fn with_grpc_compression(mut self, enabled: bool) -> Self {
        self.grpc_compression_enabled = enabled;
        self
    }

    /// Fetch M3U8 playlist from the publisher node via gRPC.
    ///
    /// # Arguments
    /// * `epoch` - Publisher epoch for cache isolation. Different epochs use different
    ///   cache namespaces, ensuring stale data isn't returned after stream restarts.
    pub(crate) async fn get_playlist(
        &self,
        api_address: &str,
        room_id: &str,
        media_id: &str,
        segment_url_base: &str,
        segment_url_suffix: &str,
        epoch: u64,
    ) -> anyhow::Result<Option<String>> {
        validate_stream_ids(room_id, media_id)?;
        validate_hls_segment_url_base(segment_url_base)?;
        validate_hls_segment_url_suffix(segment_url_suffix)?;

        let mut request = Request::new(GetHlsPlaylistRequest {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            segment_url_base: segment_url_base.to_string(),
            segment_url_suffix: segment_url_suffix.to_string(),
            expected_epoch: epoch,
        });
        request.set_timeout(Self::GRPC_REQUEST_TIMEOUT);
        self.attach_auth(&mut request)?;

        let mut client = self.connect(api_address).await?;

        let response = match client.get_hls_playlist(request).await {
            Ok(response) => response.into_inner(),
            Err(error) => {
                if should_invalidate_connection(&error) {
                    self.connection_pool.invalidate(api_address).await;
                }
                return Err(anyhow::anyhow!("gRPC GetHlsPlaylist failed: {error}"));
            }
        };

        Ok(if response.found {
            Some(response.playlist)
        } else {
            None
        })
    }

    /// Fetch TS segment from the publisher node via gRPC.
    /// Results are cached locally (TS segments are immutable).
    ///
    /// # Arguments
    /// * `epoch` - Publisher epoch for cache isolation. Different epochs use different
    ///   cache namespaces, ensuring stale data isn't returned after stream restarts.
    pub(crate) async fn get_segment(
        &self,
        api_address: &str,
        room_id: &str,
        media_id: &str,
        segment_name: &str,
        epoch: u64,
    ) -> anyhow::Result<Option<Bytes>> {
        validate_stream_ids(room_id, media_id)?;
        validate_hls_segment_name(segment_name)?;
        let cache_key = Self::build_segment_cache_key(room_id, media_id, epoch, segment_name);

        // Check local cache first
        if let Some(cached) = self.segment_cache.get(&cache_key).await {
            debug!(
                room_id = room_id,
                segment_name = segment_name,
                "HLS segment cache hit"
            );
            return Ok(Some(cached));
        }

        let loaded = self
            .segment_cache
            .try_get_with(cache_key, async {
                let mut request = Request::new(GetHlsSegmentRequest {
                    room_id: room_id.to_string(),
                    media_id: media_id.to_string(),
                    segment_name: segment_name.to_string(),
                    expected_epoch: epoch,
                });
                request.set_timeout(Self::GRPC_REQUEST_TIMEOUT);
                self.attach_auth(&mut request)
                    .map_err(|error| SegmentFetchError::Failed(error.to_string()))?;

                let mut client = self
                    .connect(api_address)
                    .await
                    .map_err(|error| SegmentFetchError::Failed(error.to_string()))?;
                let response = match client.get_hls_segment(request).await {
                    Ok(response) => response.into_inner(),
                    Err(error) => {
                        if should_invalidate_connection(&error) {
                            self.connection_pool.invalidate(api_address).await;
                        }
                        return Err(SegmentFetchError::Failed(format!(
                            "gRPC GetHlsSegment failed: {error}"
                        )));
                    }
                };

                if response.found {
                    Ok(response.data)
                } else {
                    Err(SegmentFetchError::Missing)
                }
            })
            .await;

        match loaded {
            Ok(data) => Ok(Some(data)),
            Err(error) => match error.as_ref() {
                SegmentFetchError::Missing => Ok(None),
                SegmentFetchError::Failed(message) => Err(anyhow::anyhow!(message.clone())),
            },
        }
    }

    /// Get a gRPC client for the publisher node, reusing pooled connections.
    ///
    /// On connection failure, the pooled entry is invalidated so the next
    /// attempt creates a fresh connection.
    async fn connect(
        &self,
        api_address: &str,
    ) -> anyhow::Result<StreamRelayServiceClient<tonic::transport::Channel>> {
        let channel = self
            .connection_pool
            .get_channel(api_address)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to connect to publisher API at {api_address}: {e}")
            })?;
        let client = StreamRelayServiceClient::new(channel)
            .max_decoding_message_size(self.grpc_max_message_size_bytes)
            .max_encoding_message_size(self.grpc_max_message_size_bytes);
        let client = if self.grpc_compression_enabled {
            client
                .accept_compressed(CompressionEncoding::Gzip)
                .send_compressed(CompressionEncoding::Gzip)
        } else {
            client
        };
        Ok(client)
    }

    /// Build a segment cache key with epoch isolation.
    #[inline]
    #[must_use]
    fn build_segment_cache_key(
        room_id: &str,
        media_id: &str,
        epoch: u64,
        segment_name: &str,
    ) -> String {
        let mut key = String::with_capacity(
            room_id
                .len()
                .saturating_add(media_id.len())
                .saturating_add(segment_name.len())
                .saturating_add(64),
        );
        let _ = write!(
            key,
            "|{}:{room_id}|{}:{media_id}|seg|{epoch}|{}:{segment_name}",
            room_id.len(),
            media_id.len(),
            segment_name.len()
        );
        key
    }

    /// Attach cluster authentication secret to a gRPC request.
    fn attach_auth<T>(&self, request: &mut Request<T>) -> anyhow::Result<()> {
        let secret = self
            .cluster_secret
            .as_deref()
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| anyhow::anyhow!("cluster secret is required for remote HLS RPC"))?;
        request.metadata_mut().insert(
            "x-cluster-secret",
            secret
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid cluster secret format"))?,
        );
        Ok(())
    }
}

fn should_invalidate_connection(error: &tonic::Status) -> bool {
    matches!(
        error.code(),
        tonic::Code::DeadlineExceeded
            | tonic::Code::Unavailable
            | tonic::Code::Unknown
            | tonic::Code::Cancelled
            | tonic::Code::Internal
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    fn require_some<T>(value: Option<T>, message: &'static str) -> TestResult<T> {
        value.ok_or_else(|| test_error(message))
    }

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

    #[test]
    fn attach_auth_rejects_missing_cluster_secret() {
        let client = HlsProxyClient::with_defaults(None);
        let mut request = Request::new(());

        let error = client
            .attach_auth(&mut request)
            .expect_err("remote HLS RPC must fail fast without a cluster secret");

        assert!(
            error.to_string().contains("cluster secret is required"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn attach_auth_rejects_empty_cluster_secret() {
        let client = HlsProxyClient::with_defaults(Some(String::new()));
        let mut request = Request::new(());

        let error = client
            .attach_auth(&mut request)
            .expect_err("remote HLS RPC must fail fast with an empty cluster secret");

        assert!(
            error.to_string().contains("cluster secret is required"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn attach_auth_sets_cluster_secret_metadata() -> TestResult {
        let client = HlsProxyClient::with_defaults(Some("cluster-secret".to_string()));
        let mut request = Request::new(());

        client.attach_auth(&mut request)?;

        assert_eq!(
            request
                .metadata()
                .get("x-cluster-secret")
                .and_then(|value| value.to_str().ok()),
            Some("cluster-secret")
        );
        Ok(())
    }

    #[tokio::test]
    async fn get_playlist_rejects_missing_cluster_secret_before_connect() {
        let client = HlsProxyClient::with_defaults(None);

        let error = client
            .get_playlist("http://[invalid", "room", "media", "/segments", ".ts", 1)
            .await
            .expect_err("remote HLS RPC must validate auth before connecting");

        assert!(
            error.to_string().contains("cluster secret is required"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn get_segment_rejects_missing_cluster_secret_before_connect() {
        let client = HlsProxyClient::with_defaults(None);

        let error = client
            .get_segment("http://[invalid", "room", "media", "seg-1.ts", 1)
            .await
            .expect_err("remote HLS RPC must validate auth before connecting");

        assert!(
            error.to_string().contains("cluster secret is required"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn length_prefixed_cache_keys_prevent_stream_prefix_collisions() {
        let client = HlsProxyClient::with_defaults(None);
        let target_key = HlsProxyClient::build_segment_cache_key("1", "23", 7, "seg.ts");
        let other_key = HlsProxyClient::build_segment_cache_key("12", "3", 7, "seg.ts");
        let target_data = Bytes::from_static(b"target");
        let other_data = Bytes::from_static(b"other");

        assert_ne!(
            target_key, other_key,
            "length-prefixed keys must distinguish ambiguous stream components"
        );

        client
            .segment_cache
            .insert(target_key.clone(), target_data.clone())
            .await;
        client
            .segment_cache
            .insert(other_key.clone(), other_data.clone())
            .await;

        assert_eq!(
            client.segment_cache.get(&other_key).await,
            Some(other_data),
            "similarly-prefixed but different stream must remain cached"
        );
    }

    #[tokio::test]
    async fn hls_proxy_rejects_invalid_internal_identifiers_before_connect() {
        let client = HlsProxyClient::with_defaults(Some("cluster-secret".to_string()));

        let playlist_error = client
            .get_playlist("http://[invalid", "room:1", "media", "/segments/", ".ts", 1)
            .await
            .expect_err("invalid room id should fail before connect");
        assert!(playlist_error.to_string().contains("room_id"));

        let segment_error = client
            .get_segment("http://[invalid", "room", "media", "../secret", 1)
            .await
            .expect_err("invalid segment name should fail before connect");
        assert!(segment_error.to_string().contains("segment_name"));
    }

    #[tokio::test]
    async fn test_segment_cache() -> TestResult {
        let client = HlsProxyClient::with_defaults(None);
        let cache_key = "room1:media1:seg1".to_string();
        let data = Bytes::from_static(b"test segment data");

        client
            .segment_cache
            .insert(cache_key.clone(), data.clone())
            .await;

        let cached = client.segment_cache.get(&cache_key).await;
        assert_eq!(
            require_some(cached, "segment cache entry should exist")?,
            data
        );

        let missing = client.segment_cache.get("nonexistent").await;
        assert!(missing.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_epoch_based_cache_isolation() -> TestResult {
        let client = HlsProxyClient::with_defaults(None);
        let room_id = "room1";
        let media_id = "stream1";
        let segment_name = "segment001.ts";
        let old_data = Bytes::from_static(b"old segment data from epoch 0");
        let new_data = Bytes::from_static(b"new segment data from epoch 1");

        let old_key = HlsProxyClient::build_segment_cache_key(room_id, media_id, 0, segment_name);
        client.segment_cache.insert(old_key, old_data.clone()).await;

        let new_key = HlsProxyClient::build_segment_cache_key(room_id, media_id, 1, segment_name);
        client.segment_cache.insert(new_key, new_data.clone()).await;

        let cached_old = client
            .segment_cache
            .get(&HlsProxyClient::build_segment_cache_key(
                room_id,
                media_id,
                0,
                segment_name,
            ))
            .await;
        assert_eq!(
            require_some(cached_old, "epoch 0 cache entry should exist")?,
            old_data
        );

        let cached_new = client
            .segment_cache
            .get(&HlsProxyClient::build_segment_cache_key(
                room_id,
                media_id,
                1,
                segment_name,
            ))
            .await;
        assert_eq!(
            require_some(cached_new, "epoch 1 cache entry should exist")?,
            new_data
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_epoch_change_does_not_return_stale_cache() -> TestResult {
        let client = HlsProxyClient::with_defaults(None);
        let room_id = "room1";
        let media_id = "stream1";
        let segment_name = "segment001.ts";
        let old_data = Bytes::from_static(b"old segment data");
        let new_data = Bytes::from_static(b"new segment data");

        let epoch0_key =
            HlsProxyClient::build_segment_cache_key(room_id, media_id, 0, segment_name);
        client
            .segment_cache
            .insert(epoch0_key.clone(), old_data.clone())
            .await;

        let cached = client.segment_cache.get(&epoch0_key).await;
        assert_eq!(
            require_some(cached, "epoch 0 cache entry should exist")?,
            old_data
        );

        let epoch1_key =
            HlsProxyClient::build_segment_cache_key(room_id, media_id, 1, segment_name);

        client
            .segment_cache
            .insert(epoch1_key.clone(), new_data.clone())
            .await;

        let cached_new = client.segment_cache.get(&epoch1_key).await;
        assert_eq!(
            require_some(cached_new, "epoch 1 cache entry should exist")?,
            new_data,
            "Epoch 1 should return new data, not stale epoch 0 data"
        );

        let cached_old = client.segment_cache.get(&epoch0_key).await;
        assert_eq!(
            require_some(cached_old, "epoch 0 cache entry should remain isolated")?,
            old_data,
            "Epoch 0 data still exists but is isolated"
        );
        Ok(())
    }

    #[test]
    fn test_build_segment_cache_key_format() {
        // Test length-prefixed key format: room/media components are unambiguous.
        let key = HlsProxyClient::build_segment_cache_key("room1", "stream1", 42, "segment001.ts");
        assert_eq!(key, "|5:room1|7:stream1|seg|42|13:segment001.ts");

        // Test with different epoch
        let key2 = HlsProxyClient::build_segment_cache_key("room1", "stream1", 0, "segment001.ts");
        assert_eq!(key2, "|5:room1|7:stream1|seg|0|13:segment001.ts");

        // Test with different segment
        let key3 = HlsProxyClient::build_segment_cache_key("room1", "stream1", 42, "segment002.ts");
        assert_eq!(key3, "|5:room1|7:stream1|seg|42|13:segment002.ts");
    }

    #[test]
    fn test_should_invalidate_connection_for_transport_level_statuses() {
        assert!(should_invalidate_connection(
            &tonic::Status::deadline_exceeded("timeout")
        ));
        assert!(should_invalidate_connection(&tonic::Status::unavailable(
            "down"
        )));
        assert!(should_invalidate_connection(&tonic::Status::cancelled(
            "cancelled"
        )));
        assert!(should_invalidate_connection(&tonic::Status::internal(
            "internal"
        )));
        assert!(should_invalidate_connection(&tonic::Status::unknown(
            "unknown"
        )));
        assert!(!should_invalidate_connection(&tonic::Status::not_found(
            "segment missing"
        )));
        assert!(!should_invalidate_connection(
            &tonic::Status::permission_denied("forbidden")
        ));
        assert!(!should_invalidate_connection(
            &tonic::Status::invalid_argument("bad request")
        ));
    }

    #[tokio::test]
    async fn test_segment_cache_respects_byte_limit() {
        // Create a client with a very small byte limit (100 bytes).
        // Inserting segments that exceed this should cause evictions.
        let client = HlsProxyClient::new(
            Duration::from_secs(90),
            100, // 100 bytes max
            None,
        );

        // Insert entries that together exceed 100 bytes
        let data_50 = Bytes::from(vec![0u8; 50]);
        let data_60 = Bytes::from(vec![0u8; 60]);

        client
            .segment_cache
            .insert("seg1".to_string(), data_50.clone())
            .await;
        client
            .segment_cache
            .insert("seg2".to_string(), data_60.clone())
            .await;

        // Run pending tasks to force eviction processing
        client.segment_cache.run_pending_tasks().await;

        // The total exceeds 100 bytes (50 + 60 = 110), so at least one entry
        // should be evicted. The remaining entries' total weight should be <= 100.
        let has_seg1 = client.segment_cache.get("seg1").await.is_some();
        let has_seg2 = client.segment_cache.get("seg2").await.is_some();

        // At least one must still be present, but both cannot both be present
        // if the cache respects the byte limit. (Moka may keep both briefly
        // until pending tasks run, but after run_pending_tasks they should be
        // evicted.)
        assert!(
            !(has_seg1 && has_seg2),
            "Cache should evict entries when byte limit is exceeded"
        );
    }
}
