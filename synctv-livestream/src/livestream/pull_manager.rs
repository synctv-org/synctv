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
    grpc::GrpcConnectionPool,
    livestream::pull_stream::PullStream,
    livestream::managed_stream::{ManagedStream, StreamPool},
};
use synctv_xiu::streamhub::define::{StreamHubEvent, StreamHubEventSender};
use synctv_xiu::streamhub::stream::StreamIdentifier;
use tracing::{debug, info, error, warn};
use std::sync::Arc;
use std::time::Duration;

/// Default gRPC port for inter-node streaming (fallback when grpc_address is empty)
const DEFAULT_GRPC_PORT: u16 = 50051;

/// Extract IP address from `node_id` and construct gRPC address.
/// `node_id` format is "{hostname}_{ip}-{suffix}", e.g., "server1_192.168.1.1-abc123"
/// Returns "ip:port" if IP is found, None otherwise.
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
    local_node_id: String,
    stream_hub_event_sender: StreamHubEventSender,
    /// gRPC port used when extracting address from node_id (fallback)
    grpc_port: u16,
    /// Shared gRPC connection pool for reusing HTTP/2 channels to publisher nodes.
    /// Shared across all `PullStream`/`GrpcStreamPuller` instances managed by this manager.
    connection_pool: GrpcConnectionPool,
}

impl PullStreamManager {
    pub fn new(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> Self {
        Self::with_timeouts(registry, local_node_id, stream_hub_event_sender, 60, 300)
    }

    /// Set the gRPC port used for fallback address extraction from node_id.
    /// If not called, defaults to 50051.
    #[must_use]
    pub fn with_grpc_port(mut self, port: u16) -> Self {
        self.grpc_port = port;
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
        local_node_id: String,
        stream_hub_event_sender: StreamHubEventSender,
        cleanup_check_interval_secs: u64,
        idle_timeout_secs: u64,
    ) -> Self {
        Self {
            pool: StreamPool::new(
                Duration::from_secs(cleanup_check_interval_secs),
                Duration::from_secs(idle_timeout_secs),
            ),
            registry,
            local_node_id,
            stream_hub_event_sender,
            grpc_port: DEFAULT_GRPC_PORT,
            connection_pool: GrpcConnectionPool::with_defaults(),
        }
    }

    /// Lazy-load: Get or create pull stream (only triggered by client FLV request)
    ///
    /// Uses double-checked locking to prevent duplicate pull streams for the same key
    /// when multiple viewers request the same stream concurrently.
    pub async fn get_or_create_pull_stream(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> StreamResult<Arc<PullStream>> {
        let stream_key = format!("{room_id}:{media_id}");

        // Fast path: Check if healthy pull stream already exists (no lock needed)
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

        // Re-check after acquiring lock
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

        // Use grpc_address if available, otherwise extract IP from node_id
        // node_id format is "{hostname}_{ip}-{suffix}", e.g., "server1_192.168.1.1-abc123"
        let publisher_address = if publisher_info.grpc_address.is_empty() {
            // Fallback: extract IP from node_id and use configured gRPC port
            warn!(
                node_id = %publisher_info.node_id,
                grpc_port = self.grpc_port,
                "Publisher has no grpc_address, falling back to IP extraction from node_id"
            );
            extract_address_from_node_id(&publisher_info.node_id, self.grpc_port)
                .unwrap_or_else(|| publisher_info.node_id.clone())
        } else {
            publisher_info.grpc_address.clone()
        };

        let pull_stream = Arc::new(
            PullStream::with_pool(
                room_id.to_string(),
                media_id.to_string(),
                publisher_address,
                self.local_node_id.clone(),
                Arc::clone(&self.registry),
                self.stream_hub_event_sender.clone(),
                epoch,
                self.connection_pool.clone(),
            )
        );

        // Start pull stream (connects via gRPC to publisher)
        pull_stream.start().await?;

        // Initial subscriber
        pull_stream.lifecycle().increment_subscriber_count();

        // Store in pool with idle cleanup - send UnPublish to StreamHub on cleanup
        let hub_sender = self.stream_hub_event_sender.clone();
        self.pool.insert_and_cleanup(
            stream_key,
            pull_stream.clone(),
            move |stream_key: &str| {
                let hub_sender = hub_sender.clone();
                let stream_key = stream_key.to_string();
                Box::pin(async move {
                    if let Some((room_id, media_id)) = stream_key.split_once(':') {
                        let identifier = StreamIdentifier::Rtmp {
                            app_name: "live".to_string(),
                            stream_name: format!("{room_id}/{media_id}"),
                        };
                        if let Err(e) = hub_sender.try_send(StreamHubEvent::UnPublish { identifier }) {
                            warn!("Failed to send UnPublish for idle-cleaned pull stream {}: {}", stream_key, e);
                        }
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
            "node-1".to_string(),
            stream_hub_event_sender,
        );

        assert_eq!(manager.local_node_id, "node-1");
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
