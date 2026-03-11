// Pull Stream Manager for lazy-load FLV streaming
//
// Key feature: Create pull streams only when clients request FLV (not on publisher events)
// GOP cache is handled by xiu's StreamHub internally.
//
// NOTE: This manager handles **gRPC relay** pull streams only.
// External pull-to-publish streams are managed by `ExternalPublishManager`.

use crate::{
    error::StreamResult,
    grpc::{GrpcConnectionPool, HlsProxyClient},
    livestream::managed_stream::{ManagedStream, StreamPool},
    livestream::pull_stream::PullStream,
    relay::registry_trait::StreamRegistryTrait,
};
use std::sync::Arc;
use std::time::Duration;
use synctv_xiu::streamhub::define::StreamHubEventSender;
use tracing::{debug, error, info, warn};

pub struct PullStreamManager {
    pool: StreamPool<PullStream>,
    registry: Arc<dyn StreamRegistryTrait>,
    stream_hub_event_sender: StreamHubEventSender,
    /// Shared gRPC connection pool for reusing HTTP/2 channels to publisher nodes.
    /// Shared across all `PullStream`/`GrpcStreamPuller` instances managed by this manager.
    connection_pool: GrpcConnectionPool,
    /// Handle for the background gRPC connection pool cleanup task.
    /// Kept alive for the lifetime of the manager and rebuilt if the pool is replaced.
    pool_cleanup_handle: tokio::task::JoinHandle<()>,
    /// Cancellation token for the background gRPC connection pool cleanup task.
    pool_cleanup_cancel: tokio_util::sync::CancellationToken,
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

    /// Set the cluster authentication secret for inter-node gRPC requests.
    /// When set, all `GrpcStreamPuller` instances created by this manager
    /// will attach this secret as `x-cluster-secret` metadata.
    #[must_use]
    pub fn with_cluster_secret(mut self, secret: Option<String>) -> Self {
        self.cluster_secret = secret;
        self
    }

    /// Set a shared gRPC connection pool (for sharing with `HlsProxyClient` etc.).
    #[must_use]
    pub fn with_connection_pool(mut self, pool: GrpcConnectionPool) -> Self {
        self.pool_cleanup_cancel.cancel();
        self.pool_cleanup_handle.abort();
        self.connection_pool = pool;
        self.pool_cleanup_cancel = tokio_util::sync::CancellationToken::new();
        let cleanup_interval = self.connection_pool.max_idle();
        self.pool_cleanup_handle = self
            .connection_pool
            .spawn_cleanup_task(cleanup_interval, self.pool_cleanup_cancel.clone());
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
        let cleanup_token = tokio_util::sync::CancellationToken::new();
        let pool_cleanup_handle =
            connection_pool.spawn_cleanup_task(Duration::from_mins(5), cleanup_token.clone());
        let pool = StreamPool::new(
            Duration::from_secs(cleanup_check_interval_secs),
            Duration::from_secs(idle_timeout_secs),
        );
        Self {
            pool,
            registry,
            stream_hub_event_sender,
            connection_pool,
            pool_cleanup_handle,
            pool_cleanup_cancel: cleanup_token,
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
                room_id, media_id,
            );
            return Ok(stream);
        }

        // Lazy-load: Create new pull stream on first FLV request
        info!(
            "Lazy-load: Creating pull stream for room {} / media {} from publisher",
            room_id, media_id
        );

        // Get publisher node address from registry (with timeout to prevent
        // indefinite blocking on slow/partitioned Redis)
        const REGISTRY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        let publisher_info = tokio::time::timeout(
            REGISTRY_TIMEOUT,
            self.registry.get_publisher(room_id, media_id),
        )
        .await
        .map_err(|_| {
            error!(
                "Timed out querying registry for publisher {} / {}",
                room_id, media_id
            );
            crate::error::StreamError::RegistryError(format!(
                "Registry query timed out after {}s for {room_id} / {media_id}",
                REGISTRY_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| {
            error!(
                "Failed to get publisher for {} / {}: {}",
                room_id, media_id, e
            );
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
        let publisher_address = publisher_info
            .validate_grpc_address()
            .map(|addr| addr.to_string())
            .map_err(|_| {
                error!(
                    node_id = %publisher_info.node_id,
                    "Publisher has no valid grpc_address. \
                     Set advertise_grpc_address on the publisher node."
                );
                crate::error::StreamError::InvalidAddress(format!(
                    "Publisher node '{}' has no grpc_address. \
                     Configure advertise_grpc_address on the publisher node.",
                    publisher_info.node_id
                ))
            })?;

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
            .with_hls_proxy(self.hls_proxy.clone()),
        );

        // Start pull stream (connects via gRPC to publisher)
        if let Err(e) = pull_stream.start().await {
            // Only remove the Redis registry entry on permanent/non-retryable failures.
            //
            // Permanent failures (publisher definitively gone or misconfigured):
            //   - StaleEpoch: publisher changed (split-brain), our record is obsolete
            //   - NoPublisher: no registry entry exists
            //   - InvalidAddress: publisher node has an unresolvable address
            //   - InvalidStreamKey: stream key is malformed/invalid
            //
            // Transient failures (keep the registry entry, let TTL manage it):
            //   - RegistryError: Redis unavailable or slow; the publisher may still be live
            //   - GrpcError: network hiccup; the publisher node may still be running
            //   - ConnectionFailed: transient connectivity issue
            //   - IoError: OS-level I/O error, typically transient
            //
            // Deleting the entry on transient errors causes a ~60-second routing outage
            // because all other nodes route to this stream's Redis entry; once deleted,
            // no node knows where the stream is until it re-registers (up to TTL expiry).
            let is_permanent = matches!(
                e,
                crate::error::StreamError::StaleEpoch(_)
                    | crate::error::StreamError::NoPublisher(_)
                    | crate::error::StreamError::InvalidAddress(_)
                    | crate::error::StreamError::InvalidStreamKey(_)
            );

            if is_permanent {
                warn!(
                    room_id = %room_id,
                    media_id = %media_id,
                    error = %e,
                    "Pull stream start failed with permanent error; removing stale publisher registry entry"
                );
                if let Err(unreg_err) = self.registry.unregister_publisher(room_id, media_id).await
                {
                    error!(
                        room_id = %room_id,
                        media_id = %media_id,
                        error = %unreg_err,
                        "Failed to unregister stale publisher after permanent pull stream start failure"
                    );
                }
            } else {
                warn!(
                    room_id = %room_id,
                    media_id = %media_id,
                    error = %e,
                    "Pull stream start failed with transient error; keeping publisher registry entry to avoid routing outage"
                );
            }

            return Err(crate::error::StreamError::ConnectionFailed(format!(
                "Stream temporarily unavailable for {room_id}/{media_id}: publisher unreachable. {e}"
            )));
        }

        // Creation path: increment subscriber count exactly once for the viewer
        // that triggered creation. (Reuse paths increment inside get_existing().)
        pull_stream.lifecycle().increment_subscriber_count();

        // Store in pool with idle cleanup.
        // Call stream.stop() which sets the `stopped` flag and sends UnPublish exactly once,
        // preventing the Drop impl from sending a duplicate UnPublish.
        let cleanup_stream = pull_stream.clone();
        self.pool
            .insert_and_cleanup(stream_key, pull_stream.clone(), move |_stream_key: &str| {
                let stream = cleanup_stream.clone();
                Box::pin(async move {
                    if let Err(e) = stream.stop().await {
                        warn!("Failed to stop pull stream during idle cleanup: {}", e);
                    }
                })
            });

        Ok(pull_stream)
    }
}

impl Drop for PullStreamManager {
    fn drop(&mut self) {
        self.pool_cleanup_cancel.cancel();
        self.pool_cleanup_handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::livestream::managed_stream::ManagedStream;
    use crate::relay::MockStreamRegistry;
    use tonic::transport::Channel;

    #[tokio::test]
    async fn test_pull_stream_manager_creation() {
        let registry = Arc::new(MockStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);

        let manager = PullStreamManager::new(registry, stream_hub_event_sender);

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

    /// M4: Verify that get_existing() properly rolls back subscriber count
    /// when a stream becomes unhealthy between the two health checks.
    #[tokio::test]
    async fn test_subscriber_count_rollback_on_unhealthy_stream() {
        let pool: StreamPool<PullStream> =
            StreamPool::new(Duration::from_mins(1), Duration::from_mins(5));
        let registry = Arc::new(MockStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);

        let pull_stream = Arc::new(PullStream::new(
            "room-123".to_string(),
            "media-456".to_string(),
            "publisher-node".to_string(),
            "puller-node".to_string(),
            registry,
            stream_hub_event_sender,
            1,
        ));
        pull_stream.lifecycle().set_running();
        pool.streams
            .insert("room-123:media-456".to_string(), pull_stream.clone());

        // First get_existing succeeds, subscriber count = 1
        let result = pool.get_existing("room-123:media-456").await;
        assert!(result.is_some());
        assert_eq!(pull_stream.subscriber_count(), 1);

        // Mark unhealthy (simulating stream failure)
        pull_stream.lifecycle().mark_stopping();

        // get_existing should fail and NOT increment subscriber count
        let result = pool.get_existing("room-123:media-456").await;
        assert!(result.is_none());
        // Subscriber count should still be 1 (not 2)
        assert_eq!(pull_stream.subscriber_count(), 1);
    }

    #[tokio::test]
    async fn test_with_connection_pool_rebuilds_cleanup_for_replaced_pool() {
        let registry = Arc::new(MockStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);
        let shared_pool = GrpcConnectionPool::new(Duration::from_millis(5), 8);
        let channel = Channel::from_static("http://[::1]:50051").connect_lazy();

        shared_pool.insert_test_channel_with_age(
            "publisher-node:50051",
            channel,
            Duration::from_secs(1),
        );

        let manager = PullStreamManager::with_timeouts(registry, stream_hub_event_sender, 1, 300)
            .with_connection_pool(shared_pool.clone());

        tokio::time::sleep(Duration::from_millis(20)).await;

        assert!(
            shared_pool.is_empty(),
            "the active shared pool should inherit its own idle cleanup cadence after replacement"
        );

        drop(manager);
    }
}
