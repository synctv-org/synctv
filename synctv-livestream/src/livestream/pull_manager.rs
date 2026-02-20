// Pull Stream Manager for lazy-load FLV streaming
//
// Key feature: Create pull streams only when clients request FLV (not on publisher events)
// GOP cache is handled by xiu's StreamHub internally.
//
// NOTE: This manager handles **gRPC relay** pull streams only.
// External pull-to-publish streams are managed by `ExternalPublishManager`.

use crate::{
    relay::registry_trait::StreamRegistryTrait,
    error::StreamResult,
    grpc::{GrpcConnectionPool, HlsProxyClient},
    livestream::pull_stream::PullStream,
    livestream::managed_stream::{ManagedStream, StreamPool},
};
use synctv_xiu::streamhub::define::StreamHubEventSender;
use tracing::{debug, info, error, warn};
use std::sync::Arc;
use std::time::Duration;

/// Default gRPC port for inter-node streaming (fallback when `grpc_address` is empty).
/// DEPRECATED: Only used by the legacy `extract_address_from_node_id` fallback.
const DEFAULT_GRPC_PORT: u16 = 50051;

/// Extract IP address from `node_id` and construct gRPC address.
/// `node_id` format is "{hostname}_{ip}-{suffix}", e.g., "server1_192.168.1.1-abc123"
/// Returns "ip:port" if IP is found, None otherwise.
///
/// **DEPRECATED**: This is a fragile fallback that parses the `node_id` format.
/// All publisher nodes should set `grpc_address` explicitly during registration.
/// This function is retained only for backwards compatibility with older nodes.
fn extract_address_from_node_id(node_id: &str, grpc_port: u16) -> Option<String> {
    // Split by '_' to get the part containing IP
    let after_underscore = node_id.split('_').nth(1)?;

    // Extract IP before the '-' suffix
    let ip_part = after_underscore.split('-').next()?;

    // Validate it looks like an IP address
    if ip_part.parse::<std::net::IpAddr>().is_ok() {
        Some(format!("{ip_part}:{grpc_port}"))
    } else {
        None
    }
}

pub struct PullStreamManager {
    pool: StreamPool<PullStream>,
    registry: Arc<dyn StreamRegistryTrait>,
    stream_hub_event_sender: StreamHubEventSender,
    /// gRPC port used when extracting address from `node_id` (fallback)
    grpc_port: u16,
    /// Shared gRPC connection pool for reusing HTTP/2 channels to publisher nodes.
    /// Shared across all `PullStream`/`GrpcStreamPuller` instances managed by this manager.
    connection_pool: GrpcConnectionPool,
    /// Handle for the background gRPC connection pool cleanup task.
    /// Kept alive for the lifetime of the manager; dropped (aborted) when the
    /// manager is dropped.
    _pool_cleanup_handle: tokio::task::JoinHandle<()>,
    /// Handle for the background creation lock cleanup task.
    /// Auto-started in `with_timeouts()` to prevent memory leaks from failed stream creation attempts.
    _cleanup_handle: tokio::task::JoinHandle<()>,
    /// Cluster authentication secret passed to `GrpcStreamPuller` for inter-node gRPC requests.
    cluster_secret: Option<String>,
    /// Optional HLS proxy client for cache invalidation on stale epoch detection.
    hls_proxy: Option<HlsProxyClient>,
}

impl PullStreamManager {
    pub fn new(
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> Self {
        Self::with_timeouts(registry, stream_hub_event_sender, 60, 300)
    }

    /// Set the gRPC port used for fallback address extraction from `node_id`.
    /// If not called, defaults to 50051.
    #[must_use]
    pub const fn with_grpc_port(mut self, port: u16) -> Self {
        self.grpc_port = port;
        self
    }

    /// Set the cluster authentication secret for inter-node gRPC requests.
    /// When set, all `GrpcStreamPuller` instances created by this manager
    /// will attach this secret as `x-cluster-secret` metadata.
    #[must_use]
    pub fn with_cluster_secret(mut self, secret: Option<String>) -> Self {
        self.cluster_secret = secret;
        self
    }

    /// Set a shared gRPC connection pool (for sharing with HlsProxyClient etc.).
    #[must_use]
    pub fn with_connection_pool(mut self, pool: GrpcConnectionPool) -> Self {
        self.connection_pool = pool;
        self
    }

    /// Set the HLS proxy client for cache invalidation on stale epoch detection.
    #[must_use]
    pub fn with_hls_proxy(mut self, hls_proxy: HlsProxyClient) -> Self {
        self.hls_proxy = Some(hls_proxy);
        self
    }

    /// Start the background cleanup task for stale creation locks.
    ///
    /// Should be called once after creating the manager to prevent memory leaks
    /// from failed stream creation attempts.
    #[must_use] 
    pub fn start_cleanup_task(&self) -> tokio::task::JoinHandle<()> {
        self.pool.start_creation_lock_cleanup()
    }

    pub fn with_timeouts(
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        cleanup_check_interval_secs: u64,
        idle_timeout_secs: u64,
    ) -> Self {
        let connection_pool = GrpcConnectionPool::with_defaults();
        // Evict stale gRPC connections every 5 minutes in the background
        let pool_cleanup_handle = connection_pool.spawn_cleanup_task(Duration::from_mins(5));
        let pool = StreamPool::new(
            Duration::from_secs(cleanup_check_interval_secs),
            Duration::from_secs(idle_timeout_secs),
        );
        // Auto-start creation lock cleanup to prevent memory leaks
        let cleanup_handle = pool.start_creation_lock_cleanup();
        Self {
            pool,
            registry,
            stream_hub_event_sender,
            grpc_port: DEFAULT_GRPC_PORT,
            connection_pool,
            _pool_cleanup_handle: pool_cleanup_handle,
            _cleanup_handle: cleanup_handle,
            cluster_secret: None,
            hls_proxy: None,
        }
    }

    /// Stop all managed pull streams, aborting their tasks and clearing the pool.
    ///
    /// Called during `StreamHub` restart to ensure zombie streams (still connected
    /// to the old hub instance) are cleaned up before the new hub starts.
    pub async fn stop_all(&self) {
        self.pool.stop_all().await;
    }

    /// Lazy-load: Get or create pull stream (only triggered by client FLV request)
    ///
    /// Uses double-checked locking to prevent duplicate pull streams for the same key
    /// when multiple viewers request the same stream concurrently.
    ///
    /// ## Subscriber count contract
    ///
    /// Each call to this method increments the subscriber count exactly once,
    /// regardless of which path is taken (fast-path reuse, post-lock reuse, or
    /// creation). The caller is responsible for calling `decrement_subscriber_count()`
    /// exactly once when the viewer disconnects (typically via `StreamSubscriberGuard`).
    ///
    /// - **Fast path** (existing healthy stream): `pool.get_existing()` increments.
    /// - **Post-lock reuse** (concurrent creation won): `pool.get_existing()` increments.
    /// - **Creation path** (new stream): explicit `increment_subscriber_count()`.
    pub async fn get_or_create_pull_stream(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> StreamResult<Arc<PullStream>> {
        let stream_key = format!("{room_id}:{media_id}");

        // Fast path: reuse healthy stream. get_existing() increments subscriber count.
        if let Some(stream) = self.pool.get_existing(&stream_key).await {
            debug!(
                "Reusing existing pull stream for {}/{}, subscribers: {}",
                room_id,
                media_id,
                stream.lifecycle().subscriber_count()
            );
            return Ok(stream);
        }

        // Acquire per-key creation lock
        let _guard = self.pool.acquire_creation_lock(&stream_key).await;

        // Re-check after acquiring lock. get_existing() increments subscriber count.
        if let Some(stream) = self.pool.get_existing(&stream_key).await {
            debug!(
                "Reusing pull stream created by concurrent request for {}/{}",
                room_id,
                media_id,
            );
            return Ok(stream);
        }

        // Lazy-load: Create new pull stream on first FLV request
        info!(
            "Lazy-load: Creating pull stream for room {} / media {} from publisher",
            room_id,
            media_id
        );

        // Get publisher node address from registry
        let publisher_info = self.registry.get_publisher(room_id, media_id).await
            .map_err(|e| {
                error!("Failed to get publisher for {} / {}: {}", room_id, media_id, e);
                crate::error::StreamError::RegistryError(format!("Failed to get publisher: {e}"))
            })?
            .ok_or_else(|| {
                warn!("No publisher found for {} / {}", room_id, media_id);
                crate::error::StreamError::NoPublisher(format!("{room_id} / {media_id}"))
            })?;

        // Create pull stream with gRPC puller
        // Store the epoch from publisher info for split-brain detection
        let epoch = publisher_info.epoch;

        // Use grpc_address from publisher info. All publisher nodes MUST set this
        // during registration for reliable cross-node proxying.
        let publisher_address = if let Ok(addr) = publisher_info.validate_grpc_address() { addr.to_string() } else {
            // DEPRECATED fallback: extract IP from node_id format.
            // This path exists only for backwards compatibility with older nodes
            // that were registered without grpc_address. New deployments should
            // always set `advertise_grpc_address` in config.
            warn!(
                node_id = %publisher_info.node_id,
                grpc_port = self.grpc_port,
                "Publisher has no grpc_address (misconfiguration). \
                 Falling back to deprecated IP extraction from node_id. \
                 Fix: set advertise_grpc_address in the publisher node's config."
            );
            extract_address_from_node_id(&publisher_info.node_id, self.grpc_port)
                .ok_or_else(|| {
                    error!(
                        node_id = %publisher_info.node_id,
                        "Cannot extract gRPC address from node_id and grpc_address is empty. \
                         Set advertise_grpc_address on the publisher node."
                    );
                    crate::error::StreamError::InvalidAddress(format!(
                        "Publisher node '{}' has no grpc_address and node_id format is unrecognized. \
                         Configure advertise_grpc_address on the publisher node.",
                        publisher_info.node_id
                    ))
                })?
        };

        let pull_stream = Arc::new(
            PullStream::with_pool(
                room_id.to_string(),
                media_id.to_string(),
                publisher_address,
                Arc::clone(&self.registry),
                self.stream_hub_event_sender.clone(),
                epoch,
                self.connection_pool.clone(),
            )
            .with_cluster_secret(self.cluster_secret.clone())
            .with_hls_proxy(self.hls_proxy.clone())
        );

        // Start pull stream (connects via gRPC to publisher)
        pull_stream.start().await?;

        // Creation path: increment subscriber count exactly once for the viewer
        // that triggered creation. (Reuse paths increment inside get_existing().)
        pull_stream.lifecycle().increment_subscriber_count();

        // Store in pool with idle cleanup.
        // Call stream.stop() which sets the `stopped` flag and sends UnPublish exactly once,
        // preventing the Drop impl from sending a duplicate UnPublish.
        let cleanup_stream = pull_stream.clone();
        self.pool.insert_and_cleanup(
            stream_key,
            pull_stream.clone(),
            move |_stream_key: &str| {
                let stream = cleanup_stream.clone();
                Box::pin(async move {
                    if let Err(e) = stream.stop().await {
                        warn!("Failed to stop pull stream during idle cleanup: {}", e);
                    }
                })
            },
        );

        Ok(pull_stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::MockStreamRegistry;
    use crate::livestream::managed_stream::ManagedStream;

    #[tokio::test]
    async fn test_pull_stream_manager_creation() {
        let registry = Arc::new(MockStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);

        let manager = PullStreamManager::new(
            registry,
            stream_hub_event_sender,
        );

        assert_eq!(manager.pool.streams.len(), 0);
    }

    #[tokio::test]
    async fn test_pull_stream_creation() {
        let registry = Arc::new(MockStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);

        let pull_stream = PullStream::new(
            "room-123".to_string(),
            "media-456".to_string(),
            "publisher-node".to_string(),
            "puller-node".to_string(),
            registry,
            stream_hub_event_sender,
            1, // epoch
        );

        assert_eq!(pull_stream.room_id, "room-123");
        assert_eq!(pull_stream.media_id, "media-456");
        assert_eq!(pull_stream.publisher_node, "publisher-node");
    }

    #[tokio::test]
    async fn test_subscriber_count() {
        let registry = Arc::new(MockStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);

        let pull_stream = PullStream::new(
            "room-123".to_string(),
            "media-456".to_string(),
            "publisher-node".to_string(),
            "puller-node".to_string(),
            registry,
            stream_hub_event_sender,
            1, // epoch
        );

        assert_eq!(pull_stream.subscriber_count(), 0);

        pull_stream.increment_subscriber_count();
        assert_eq!(pull_stream.subscriber_count(), 1);

        pull_stream.increment_subscriber_count();
        assert_eq!(pull_stream.subscriber_count(), 2);

        pull_stream.decrement_subscriber_count();
        assert_eq!(pull_stream.subscriber_count(), 1);
    }

    #[tokio::test]
    async fn test_stream_key() {
        let registry = Arc::new(MockStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);

        let pull_stream = PullStream::new(
            "room-123".to_string(),
            "media-456".to_string(),
            "publisher-node".to_string(),
            "puller-node".to_string(),
            registry,
            stream_hub_event_sender,
            1, // epoch
        );

        assert_eq!(pull_stream.stream_key(), "room-123:media-456");
    }
}
