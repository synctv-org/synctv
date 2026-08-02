// HLS proxy client for cross-node HLS streaming
// Non-publisher nodes use this client to fetch M3U8 playlists and TS segments
// from the publisher node via gRPC. TS segments are cached locally since they
// are immutable once created. Final playlists are cached for route grace so a
// viewer can finish after the publisher node has gone away.

use bytes::Bytes;
use moka::future::Cache;
use std::fmt::Write as _;
use std::time::Duration;
use tonic::codec::CompressionEncoding;
use tonic::Request;
use tracing::debug;

use synctv_xiu::hls::DEFAULT_HLS_GENERATION_RETENTION;

use crate::util::{
    validate_hls_segment_name, validate_hls_segment_url_base, validate_hls_segment_url_suffix,
    validate_stream_generation_id, validate_stream_ids,
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

#[derive(Debug, Clone, Copy)]
pub(crate) struct HlsRelayRoute<'a> {
    cluster_address: &'a str,
    room_id: &'a str,
    media_id: &'a str,
    generation_id: &'a str,
    lease_epoch: u64,
}

impl<'a> HlsRelayRoute<'a> {
    #[must_use]
    pub(crate) const fn new(
        cluster_address: &'a str,
        room_id: &'a str,
        media_id: &'a str,
        generation_id: &'a str,
        lease_epoch: u64,
    ) -> Self {
        Self {
            cluster_address,
            room_id,
            media_id,
            generation_id,
            lease_epoch,
        }
    }

    fn validate(self) -> anyhow::Result<()> {
        validate_stream_ids(self.room_id, self.media_id)?;
        validate_stream_generation_id(self.generation_id)
    }
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
/// Cache keys include the publisher lease_epoch. Every
/// string component is length-prefixed before concatenation so delimiters inside
/// a component cannot collide with stream boundaries:
///
/// - Segment key:
///   `|{len}:{room_id}|{len}:{media_id}|pub|{len}:{generation_id}|lease_epoch|{lease_epoch}|seg|{len}:{segment_name}`
///   StreamHub generations and registry epochs jointly isolate cache namespaces.
#[derive(Clone)]
pub(crate) struct HlsProxyClient {
    /// Local cache for TS segments (immutable once created)
    /// Key uses length-prefixed room/media/segment components.
    segment_cache: Cache<String, Bytes>,
    /// Cache only final playlists. Live playlists remain source-of-truth on the
    /// publisher node; an ended playlist can be served during route grace after
    /// that node becomes unreachable.
    final_playlist_cache: Cache<String, String>,
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

    /// Moka's capacity is measured in bytes for both HLS caches.
    fn cache_weight(key_len: usize, value_len: usize) -> u32 {
        let total = key_len.saturating_add(value_len);
        u32::try_from(total.min(usize::try_from(u32::MAX).unwrap_or(usize::MAX)))
            .unwrap_or(u32::MAX)
    }

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
                Self::cache_weight(key.len(), value.len())
            })
            .build();
        // A final playlist must remain usable for the whole retained HLS
        // generation window. The segment cache can use a shorter value because
        // a live publisher can still be queried for an uncached segment.
        let final_playlist_cache_ttl = segment_cache_ttl.max(DEFAULT_HLS_GENERATION_RETENTION);
        let final_playlist_cache = Cache::builder()
            .time_to_live(final_playlist_cache_ttl)
            .max_capacity(8 * 1024 * 1024)
            .weigher(|key: &String, value: &String| -> u32 {
                Self::cache_weight(key.len(), value.len())
            })
            .build();

        Self {
            segment_cache,
            final_playlist_cache,
            cluster_secret,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            connection_pool: GrpcConnectionPool::with_defaults(),
        }
    }

    /// Create with default settings (90s segment TTL, 150s final playlist TTL,
    /// 512 MB max segment bytes).
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
    /// * `route` - Publisher node and immutable publication generation to query
    pub(crate) async fn get_playlist(
        &self,
        route: HlsRelayRoute<'_>,
        segment_url_base: &str,
        segment_url_suffix: &str,
    ) -> anyhow::Result<Option<String>> {
        route.validate()?;
        validate_hls_segment_url_base(segment_url_base)?;
        validate_hls_segment_url_suffix(segment_url_suffix)?;

        let mut request = Request::new(GetHlsPlaylistRequest {
            room_id: route.room_id.to_string(),
            media_id: route.media_id.to_string(),
            generation_id: route.generation_id.to_string(),
            expected_lease_epoch: route.lease_epoch,
            segment_url_base: segment_url_base.to_string(),
            segment_url_suffix: segment_url_suffix.to_string(),
        });
        request.set_timeout(Self::GRPC_REQUEST_TIMEOUT);
        self.attach_auth(&mut request)?;

        let cache_key = Self::build_playlist_cache_key(
            route.room_id,
            route.media_id,
            route.generation_id,
            route.lease_epoch,
            segment_url_base,
            segment_url_suffix,
        );

        let mut client = match self.connect(route.cluster_address).await {
            Ok(client) => client,
            Err(error) => {
                if let Some(playlist) = self.final_playlist_cache.get(&cache_key).await {
                    return Ok(Some(playlist));
                }
                return Err(error);
            }
        };

        let response = match client.get_hls_playlist(request).await {
            Ok(response) => response.into_inner(),
            Err(error) => {
                if should_invalidate_connection(&error) {
                    self.connection_pool.invalidate(route.cluster_address).await;
                }
                if let Some(playlist) = self.final_playlist_cache.get(&cache_key).await {
                    return Ok(Some(playlist));
                }
                return Err(anyhow::anyhow!("gRPC GetHlsPlaylist failed: {error}"));
            }
        };

        Ok(if response.found {
            if response.playlist.contains("#EXT-X-ENDLIST") {
                self.final_playlist_cache
                    .insert(cache_key, response.playlist.clone())
                    .await;
            }
            Some(response.playlist)
        } else {
            None
        })
    }

    /// Fetch TS segment from the publisher node via gRPC.
    /// Results are cached locally (TS segments are immutable).
    ///
    /// # Arguments
    /// * `route` - Publisher node and immutable publication generation to query
    pub(crate) async fn get_segment(
        &self,
        route: HlsRelayRoute<'_>,
        segment_name: &str,
    ) -> anyhow::Result<Option<Bytes>> {
        route.validate()?;
        validate_hls_segment_name(segment_name)?;
        let cache_key = Self::build_segment_cache_key(
            route.room_id,
            route.media_id,
            route.generation_id,
            route.lease_epoch,
            segment_name,
        );

        // Check local cache first
        if let Some(cached) = self.segment_cache.get(&cache_key).await {
            debug!(
                room_id = route.room_id,
                segment_name = segment_name,
                "HLS segment cache hit"
            );
            return Ok(Some(cached));
        }

        let loaded = self
            .segment_cache
            .try_get_with(cache_key, async {
                let mut request = Request::new(GetHlsSegmentRequest {
                    room_id: route.room_id.to_string(),
                    media_id: route.media_id.to_string(),
                    generation_id: route.generation_id.to_string(),
                    segment_name: segment_name.to_string(),
                    expected_lease_epoch: route.lease_epoch,
                });
                request.set_timeout(Self::GRPC_REQUEST_TIMEOUT);
                self.attach_auth(&mut request)
                    .map_err(|error| SegmentFetchError::Failed(error.to_string()))?;

                let mut client = self
                    .connect(route.cluster_address)
                    .await
                    .map_err(|error| SegmentFetchError::Failed(error.to_string()))?;
                let response = match client.get_hls_segment(request).await {
                    Ok(response) => response.into_inner(),
                    Err(error) => {
                        if should_invalidate_connection(&error) {
                            self.connection_pool.invalidate(route.cluster_address).await;
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
        cluster_address: &str,
    ) -> anyhow::Result<StreamRelayServiceClient<tonic::transport::Channel>> {
        let channel = self
            .connection_pool
            .get_channel(cluster_address)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to connect to publisher cluster endpoint at {cluster_address}: {e}"
                )
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

    /// Build a segment cache key with lease_epoch isolation.
    #[inline]
    #[must_use]
    fn build_segment_cache_key(
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        lease_epoch: u64,
        segment_name: &str,
    ) -> String {
        let mut key = Self::build_cache_key_prefix(
            room_id,
            media_id,
            generation_id,
            lease_epoch,
            segment_name.len(),
        );
        let _ = write!(key, "seg|{}:{segment_name}", segment_name.len());
        key
    }

    #[inline]
    #[must_use]
    fn build_playlist_cache_key(
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        lease_epoch: u64,
        segment_url_base: &str,
        segment_url_suffix: &str,
    ) -> String {
        let mut key = Self::build_cache_key_prefix(
            room_id,
            media_id,
            generation_id,
            lease_epoch,
            segment_url_base
                .len()
                .saturating_add(segment_url_suffix.len())
                .saturating_add(32),
        );
        let _ = write!(
            key,
            "playlist|{}:{segment_url_base}|{}:{segment_url_suffix}",
            segment_url_base.len(),
            segment_url_suffix.len(),
        );
        key
    }

    #[inline]
    #[must_use]
    fn build_cache_key_prefix(
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        lease_epoch: u64,
        suffix_len: usize,
    ) -> String {
        let mut key = String::with_capacity(
            room_id
                .len()
                .saturating_add(media_id.len())
                .saturating_add(generation_id.len())
                .saturating_add(suffix_len)
                .saturating_add(64),
        );
        let _ = write!(
            key,
            "|{}:{room_id}|{}:{media_id}|pub|{}:{generation_id}|lease_epoch|{lease_epoch}|",
            room_id.len(),
            media_id.len(),
            generation_id.len(),
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
    use crate::{
        grpc::{StreamRelayServiceImpl, StreamRelayServiceServer},
        relay::{StreamRegistryTrait, TestStreamRegistry},
        util::TEST_GENERATION_ID,
    };
    use std::sync::Arc;
    use synctv_xiu::{
        hls::{CleanupConfig, HlsPlaylist, SegmentInfo, SegmentManager, StreamProcessorState},
        storage::{HlsStorage, MemoryStorage},
    };
    use tokio_util::sync::CancellationToken;
    use tonic::transport::{server::TcpIncoming, Server};

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    fn require_some<T>(value: Option<T>, message: &'static str) -> TestResult<T> {
        value.ok_or_else(|| test_error(message))
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
            .get_playlist(
                HlsRelayRoute::new("http://[invalid", "room", "media", TEST_GENERATION_ID, 1),
                "/segments",
                ".ts",
            )
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
            .get_segment(
                HlsRelayRoute::new("http://[invalid", "room", "media", TEST_GENERATION_ID, 1),
                "seg-1.ts",
            )
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
        let target_key =
            HlsProxyClient::build_segment_cache_key("1", "23", TEST_GENERATION_ID, 7, "seg.ts");
        let other_key =
            HlsProxyClient::build_segment_cache_key("12", "3", TEST_GENERATION_ID, 7, "seg.ts");
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
            .get_playlist(
                HlsRelayRoute::new("http://[invalid", "room:1", "media", TEST_GENERATION_ID, 1),
                "/segments/",
                ".ts",
            )
            .await
            .expect_err("invalid room id should fail before connect");
        assert!(playlist_error.to_string().contains("room_id"));

        let segment_error = client
            .get_segment(
                HlsRelayRoute::new("http://[invalid", "room", "media", TEST_GENERATION_ID, 1),
                "../secret",
            )
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
        let old_data = Bytes::from_static(b"old segment data from lease_epoch 0");
        let new_data = Bytes::from_static(b"new segment data from lease_epoch 1");

        let old_key = HlsProxyClient::build_segment_cache_key(
            room_id,
            media_id,
            TEST_GENERATION_ID,
            0,
            segment_name,
        );
        client.segment_cache.insert(old_key, old_data.clone()).await;

        let new_key = HlsProxyClient::build_segment_cache_key(
            room_id,
            media_id,
            TEST_GENERATION_ID,
            1,
            segment_name,
        );
        client.segment_cache.insert(new_key, new_data.clone()).await;

        let cached_old = client
            .segment_cache
            .get(&HlsProxyClient::build_segment_cache_key(
                room_id,
                media_id,
                TEST_GENERATION_ID,
                0,
                segment_name,
            ))
            .await;
        assert_eq!(
            require_some(cached_old, "lease_epoch 0 cache entry should exist")?,
            old_data
        );

        let cached_new = client
            .segment_cache
            .get(&HlsProxyClient::build_segment_cache_key(
                room_id,
                media_id,
                TEST_GENERATION_ID,
                1,
                segment_name,
            ))
            .await;
        assert_eq!(
            require_some(cached_new, "lease_epoch 1 cache entry should exist")?,
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

        let epoch0_key = HlsProxyClient::build_segment_cache_key(
            room_id,
            media_id,
            TEST_GENERATION_ID,
            0,
            segment_name,
        );
        client
            .segment_cache
            .insert(epoch0_key.clone(), old_data.clone())
            .await;

        let cached = client.segment_cache.get(&epoch0_key).await;
        assert_eq!(
            require_some(cached, "lease_epoch 0 cache entry should exist")?,
            old_data
        );

        let epoch1_key = HlsProxyClient::build_segment_cache_key(
            room_id,
            media_id,
            TEST_GENERATION_ID,
            1,
            segment_name,
        );

        client
            .segment_cache
            .insert(epoch1_key.clone(), new_data.clone())
            .await;

        let cached_new = client.segment_cache.get(&epoch1_key).await;
        assert_eq!(
            require_some(cached_new, "lease_epoch 1 cache entry should exist")?,
            new_data,
            "Epoch 1 should return new data, not stale lease_epoch 0 data"
        );

        let cached_old = client.segment_cache.get(&epoch0_key).await;
        assert_eq!(
            require_some(
                cached_old,
                "lease_epoch 0 cache entry should remain isolated"
            )?,
            old_data,
            "Epoch 0 data still exists but is isolated"
        );
        Ok(())
    }

    #[test]
    fn test_build_segment_cache_key_format() {
        // Test length-prefixed key format: room/media components are unambiguous.
        let key = HlsProxyClient::build_segment_cache_key(
            "room1",
            "stream1",
            TEST_GENERATION_ID,
            42,
            "segment001.ts",
        );
        assert_eq!(
            key,
            "|5:room1|7:stream1|pub|36:00000000-0000-4000-8000-000000000001|lease_epoch|42|seg|13:segment001.ts"
        );

        // Test with different lease_epoch
        let key2 = HlsProxyClient::build_segment_cache_key(
            "room1",
            "stream1",
            TEST_GENERATION_ID,
            0,
            "segment001.ts",
        );
        assert_eq!(
            key2,
            "|5:room1|7:stream1|pub|36:00000000-0000-4000-8000-000000000001|lease_epoch|0|seg|13:segment001.ts"
        );

        // Test with different segment
        let key3 = HlsProxyClient::build_segment_cache_key(
            "room1",
            "stream1",
            TEST_GENERATION_ID,
            42,
            "segment002.ts",
        );
        assert_eq!(
            key3,
            "|5:room1|7:stream1|pub|36:00000000-0000-4000-8000-000000000001|lease_epoch|42|seg|13:segment002.ts"
        );
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

    #[tokio::test]
    async fn real_tonic_relay_fetches_fresh_playlists_and_epoch_isolated_segments() -> TestResult {
        let generation_id = synctv_xiu::streamhub::utils::Uuid::new();
        let generation_id_string = generation_id.to_string();
        let replacement_generation_id = synctv_xiu::streamhub::utils::Uuid::new();
        let replacement_generation_id_string = replacement_generation_id.to_string();

        let registry_impl = Arc::new(TestStreamRegistry::new());
        let registry: Arc<dyn StreamRegistryTrait> = registry_impl.clone();
        assert!(
            registry
                .try_activate_generation(
                    "room",
                    "media",
                    "publisher-a",
                    "",
                    "127.0.0.1:1",
                    &generation_id_string,
                )
                .await?
        );

        let storage = Arc::new(MemoryStorage::unlimited());
        let segment_name = "29676270_segment";
        storage
            .write(
                "room",
                "media",
                segment_name,
                Bytes::from_static(b"lease_epoch-1"),
            )
            .await?;
        let segment_manager = Arc::new(SegmentManager::new(
            Arc::clone(&storage) as Arc<dyn HlsStorage>,
            CleanupConfig::default(),
        ));
        let hls_registry = Arc::new(dashmap::DashMap::new());
        let mut playlist = HlsPlaylist::new();
        playlist.push_segment(SegmentInfo {
            sequence: 0,
            duration_ms: 10_000,
            started_at_ms: 1_700_000_000_000,
            ts_name: segment_name.to_string(),
            discontinuity: false,
        });
        hls_registry.insert(
            synctv_xiu::hls::generation_registry_key("room", "media", &generation_id_string),
            Arc::new(parking_lot::RwLock::new(StreamProcessorState {
                app_name: "room".to_string(),
                stream_name: "media".to_string(),
                playlist,
                generation_id,
                marked_for_cleanup: false,
                cleanup_segment_names: Vec::new(),
            })),
        );

        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let relay_cancel = CancellationToken::new();
        let service = StreamRelayServiceImpl::new(
            Arc::clone(&registry),
            "publisher-a".to_string(),
            event_tx,
            relay_cancel.clone(),
        )
        .with_cluster_secret("cluster-secret")
        .with_segment_manager(segment_manager)
        .with_hls_stream_registry(Arc::clone(&hls_registry));
        let incoming = TcpIncoming::bind("127.0.0.1:0".parse()?)?;
        let relay_address = incoming.local_addr()?;
        let server_cancel = CancellationToken::new();
        let server_cancel_for_task = server_cancel.clone();
        let server_task = tokio::spawn(async move {
            Server::builder()
                .add_service(StreamRelayServiceServer::new(service))
                .serve_with_incoming_shutdown(incoming, server_cancel_for_task.cancelled_owned())
                .await
        });

        let client = HlsProxyClient::with_defaults(Some("cluster-secret".to_string()))
            .with_grpc_compression(false);
        let relay_url = format!("http://{relay_address}");
        let playlist = client
            .get_playlist(
                HlsRelayRoute::new(&relay_url, "room", "media", &generation_id_string, 1),
                "/viewer/segments/",
                ".ts?token=viewer",
            )
            .await?
            .ok_or_else(|| test_error("relay playlist should exist"))?;
        assert!(playlist.contains("/viewer/segments/29676270_segment.ts?token=viewer"));
        assert_eq!(
            client
                .get_segment(
                    HlsRelayRoute::new(&relay_url, "room", "media", &generation_id_string, 1),
                    segment_name,
                )
                .await?,
            Some(Bytes::from_static(b"lease_epoch-1"))
        );

        hls_registry
            .get(&synctv_xiu::hls::generation_registry_key(
                "room",
                "media",
                &generation_id_string,
            ))
            .ok_or_else(|| test_error("HLS state should exist"))?
            .write()
            .playlist
            .mark_ended();
        let ended = client
            .get_playlist(
                HlsRelayRoute::new(&relay_url, "room", "media", &generation_id_string, 1),
                "/viewer/segments/",
                ".ts",
            )
            .await?
            .ok_or_else(|| test_error("ended relay playlist should exist"))?;
        assert!(ended.contains("#EXT-X-ENDLIST"));

        registry
            .deactivate_generation_preserving_hls_if_lease_matches(
                "room",
                "media",
                &generation_id_string,
                1,
            )
            .await?;
        let retained = client
            .get_playlist(
                HlsRelayRoute::new(&relay_url, "room", "media", &generation_id_string, 1),
                "/viewer/segments/",
                ".ts",
            )
            .await?
            .ok_or_else(|| test_error("ended relay playlist route should be retained"))?;
        assert!(retained.contains("#EXT-X-ENDLIST"));
        assert_eq!(
            client
                .get_segment(
                    HlsRelayRoute::new(&relay_url, "room", "media", &generation_id_string, 1),
                    segment_name,
                )
                .await?,
            Some(Bytes::from_static(b"lease_epoch-1"))
        );

        assert!(
            registry
                .try_activate_generation(
                    "room",
                    "media",
                    "publisher-a",
                    "",
                    "127.0.0.1:1",
                    &replacement_generation_id_string,
                )
                .await?
        );
        storage
            .write(
                "room",
                "media",
                segment_name,
                Bytes::from_static(b"lease_epoch-2"),
            )
            .await?;
        assert_eq!(
            client
                .get_segment(
                    HlsRelayRoute::new(
                        &relay_url,
                        "room",
                        "media",
                        &replacement_generation_id_string,
                        2,
                    ),
                    segment_name,
                )
                .await?,
            Some(Bytes::from_static(b"lease_epoch-2"))
        );
        assert!(client
            .get_playlist(
                HlsRelayRoute::new(
                    &relay_url,
                    "room",
                    "media",
                    &replacement_generation_id_string,
                    1,
                ),
                "/segments/",
                ".ts",
            )
            .await
            .is_err());

        server_cancel.cancel();
        relay_cancel.cancel();
        server_task.await??;
        let cached_final = client
            .get_playlist(
                HlsRelayRoute::new(&relay_url, "room", "media", &generation_id_string, 1),
                "/viewer/segments/",
                ".ts",
            )
            .await?
            .ok_or_else(|| test_error("final playlist should be served from proxy cache"))?;
        assert!(cached_final.contains("#EXT-X-ENDLIST"));
        assert_eq!(
            client
                .get_segment(
                    HlsRelayRoute::new(
                        &relay_url,
                        "room",
                        "media",
                        &replacement_generation_id_string,
                        2,
                    ),
                    segment_name,
                )
                .await?,
            Some(Bytes::from_static(b"lease_epoch-2"))
        );
        assert!(client
            .get_segment(
                HlsRelayRoute::new(
                    &relay_url,
                    "room",
                    "media",
                    &replacement_generation_id_string,
                    2,
                ),
                "29676270_uncached",
            )
            .await
            .is_err());
        Ok(())
    }
}
