// Pull Stream Manager for lazy-load FLV streaming
// Key feature: Create pull streams only when clients request FLV (not on publisher events)
// GOP cache is handled by xiu's StreamHub internally.
// NOTE: This manager handles **gRPC relay** pull streams only.
// External pull-to-publish streams are managed by `ExternalPublishManager`.

use crate::{
    error::StreamResult,
    grpc::GrpcConnectionPool,
    livestream::managed_stream::{ManagedStream, StreamPool},
    livestream::pull_stream::{PullStream, PullStreamRoute},
    relay::registry_trait::StreamRegistryTrait,
};
use std::sync::Arc;
use std::time::Duration;
use synctv_xiu::streamhub::define::StreamHubEventSender;
use tracing::{debug, error, info, warn};

pub(crate) struct PullStreamManager {
    pool: StreamPool<PullStream>,
    registry: Arc<dyn StreamRegistryTrait>,
    stream_hub_event_sender: StreamHubEventSender,
    /// Shared gRPC connection pool for reusing HTTP/2 channels to publisher nodes.
    /// Shared across all `PullStream`/`GrpcStreamPuller` instances managed by this manager.
    connection_pool: GrpcConnectionPool,
    /// Maximum gRPC message size for relay calls created by this manager.
    grpc_max_message_size_bytes: usize,
    /// Whether relay calls created by this manager negotiate gzip compression.
    grpc_compression_enabled: bool,
    /// Cluster authentication secret passed to `GrpcStreamPuller` for inter-node gRPC requests.
    cluster_secret: Option<String>,
}

impl PullStreamManager {
    pub(crate) fn new(
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> Self {
        Self::with_timeouts(registry, stream_hub_event_sender, 60, 300)
    }

    /// Set the cluster authentication secret for inter-node gRPC requests.
    /// When set, all `GrpcStreamPuller` instances created by this manager
    /// will attach this secret as `x-cluster-secret` metadata.
    #[must_use]
    pub(crate) fn with_cluster_secret(mut self, secret: Option<String>) -> Self {
        self.cluster_secret = secret;
        self
    }

    /// Set a shared gRPC connection pool.
    #[must_use]
    pub(crate) fn with_connection_pool(mut self, pool: GrpcConnectionPool) -> Self {
        self.connection_pool = pool;
        self
    }

    /// Set the maximum gRPC message size for relay calls created by this manager.
    #[must_use]
    pub(crate) const fn with_grpc_max_message_size(
        mut self,
        max_message_size_bytes: usize,
    ) -> Self {
        self.grpc_max_message_size_bytes = max_message_size_bytes;
        self
    }

    /// Enable or disable gzip compression negotiation for relay calls created by this manager.
    #[must_use]
    pub(crate) const fn with_grpc_compression(mut self, enabled: bool) -> Self {
        self.grpc_compression_enabled = enabled;
        self
    }

    pub(crate) fn with_timeouts(
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        cleanup_check_interval_secs: u64,
        idle_timeout_secs: u64,
    ) -> Self {
        let connection_pool = GrpcConnectionPool::with_defaults();
        let pool = StreamPool::new(
            Duration::from_secs(cleanup_check_interval_secs),
            Duration::from_secs(idle_timeout_secs),
        );
        Self {
            pool,
            registry,
            stream_hub_event_sender,
            connection_pool,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            cluster_secret: None,
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
        let registry_timeout = std::time::Duration::from_secs(5);
        let publisher_info = tokio::time::timeout(
            registry_timeout,
            self.registry.get_active_generation(room_id, media_id),
        )
        .await
        .map_err(|_| {
            error!(
                "Timed out querying registry for publisher {} / {}",
                room_id, media_id
            );
            crate::error::StreamError::RegistryError(format!(
                "Registry query timed out after {}s for {room_id} / {media_id}",
                registry_timeout.as_secs()
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
        // Store the lease_epoch from publisher info for split-brain detection
        let lease_epoch = publisher_info.lease_epoch;

        // Use the cluster listener address registered by the publisher node.
        let publisher_address = publisher_info
            .validate_cluster_address()
            .map(std::string::ToString::to_string)
            .map_err(|_| {
                error!(
                    node_id = %publisher_info.node_id,
                    "Publisher has no valid cluster_address. \
                     Set advertise_cluster_address on the publisher node."
                );
                crate::error::StreamError::InvalidAddress(format!(
                    "Publisher node '{}' has no cluster_address. \
                     Configure advertise_cluster_address on the publisher node.",
                    publisher_info.node_id
                ))
            })?;

        let pull_stream = Arc::new(
            PullStream::with_pool(
                PullStreamRoute::new(
                    room_id.to_string(),
                    media_id.to_string(),
                    publisher_address,
                    publisher_info.generation_id.clone(),
                    lease_epoch,
                ),
                Arc::clone(&self.registry),
                self.stream_hub_event_sender.clone(),
                self.connection_pool.clone(),
            )
            .with_grpc_max_message_size(self.grpc_max_message_size_bytes)
            .with_grpc_compression(self.grpc_compression_enabled)
            .with_cluster_secret(self.cluster_secret.clone()),
        );

        // Start pull stream (connects via gRPC to publisher)
        if let Err(e) = pull_stream.start().await {
            // Only remove the Redis registry entry on permanent/non-retryable failures.
            // Permanent failures (publisher definitively gone or misconfigured):
            //   - StaleEpoch: publisher changed (split-brain), our record is obsolete
            //   - NoPublisher: no registry entry exists
            //   - InvalidAddress: publisher node has an unresolvable address
            //   - InvalidStreamKey: stream key is malformed/invalid
            // Transient failures (keep the registry entry, let TTL manage it):
            //   - RegistryError: Redis unavailable or slow; the publisher may still be live
            //   - GrpcError: network hiccup; the publisher node may still be running
            //   - ConnectionFailed: transient connectivity issue
            //   - IoError: OS-level I/O error, typically transient
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
                if let Err(unreg_err) = self
                    .registry
                    .deactivate_generation_if_lease_matches(
                        room_id,
                        media_id,
                        &publisher_info.generation_id,
                        lease_epoch,
                    )
                    .await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::livestream::managed_stream::ManagedStream;
    use crate::relay::TestStreamRegistry;

    fn make_pull_stream(
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> PullStream {
        PullStream::with_pool(
            PullStreamRoute::new(
                "room-123".to_string(),
                "media-456".to_string(),
                "publisher-node".to_string(),
                crate::util::TEST_GENERATION_ID.to_string(),
                1,
            ),
            registry,
            stream_hub_event_sender,
            GrpcConnectionPool::with_defaults(),
        )
    }

    #[tokio::test]
    async fn test_subscriber_count() {
        let registry = Arc::new(TestStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);

        let pull_stream = make_pull_stream(registry, stream_hub_event_sender);

        assert_eq!(pull_stream.lifecycle().subscriber_count(), 0);

        pull_stream.lifecycle().increment_subscriber_count();
        assert_eq!(pull_stream.lifecycle().subscriber_count(), 1);

        pull_stream.lifecycle().increment_subscriber_count();
        assert_eq!(pull_stream.lifecycle().subscriber_count(), 2);

        pull_stream.lifecycle().decrement_subscriber_count();
        assert_eq!(pull_stream.lifecycle().subscriber_count(), 1);
    }

    /// Verify that get_existing() properly rolls back subscriber count
    /// when a stream becomes unhealthy between the two health checks.
    #[tokio::test]
    async fn test_subscriber_count_rollback_on_unhealthy_stream() {
        let pool: StreamPool<PullStream> =
            StreamPool::new(Duration::from_mins(1), Duration::from_mins(5));
        let registry = Arc::new(TestStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);

        let pull_stream = Arc::new(make_pull_stream(registry, stream_hub_event_sender));
        pull_stream.lifecycle().set_running();
        pool.streams
            .insert("room-123:media-456".to_string(), pull_stream.clone());

        // First get_existing succeeds, subscriber count = 1
        let result = pool.get_existing("room-123:media-456").await;
        assert!(result.is_some());
        assert_eq!(pull_stream.lifecycle().subscriber_count(), 1);

        // Mark unhealthy (simulating stream failure)
        pull_stream.lifecycle().mark_stopping();

        // get_existing should fail and NOT increment subscriber count
        let result = pool.get_existing("room-123:media-456").await;
        assert!(result.is_none());
        // Subscriber count should still be 1 (not 2)
        assert_eq!(pull_stream.lifecycle().subscriber_count(), 1);
    }

    #[tokio::test]
    async fn test_with_connection_pool_replaces_default_pool() {
        let registry = Arc::new(TestStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);
        let shared_pool = GrpcConnectionPool::new(Duration::from_millis(5), 8);

        shared_pool
            .insert_test_channel("publisher-node:50051")
            .await;

        let manager = PullStreamManager::with_timeouts(registry, stream_hub_event_sender, 1, 300)
            .with_connection_pool(shared_pool.clone());

        assert_eq!(manager.connection_pool.len(), shared_pool.len());
    }
}
