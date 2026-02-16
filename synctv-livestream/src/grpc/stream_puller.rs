use bytes::Bytes;
use std::sync::Arc;
use std::time::Instant;
use synctv_xiu::rtmp::session::common::RtmpStreamHandler;
use synctv_xiu::streamhub::{
    define::{
        FrameData, FrameDataSender, NotifyInfo, PublishType, PublisherInfo, StreamHubEvent,
        StreamHubEventSender,
    },
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio::sync::{mpsc, oneshot};
use tonic::Request;
use tracing::{error, info, warn};

use super::connection_pool::GrpcConnectionPool;
use super::proto::{stream_relay_service_client::StreamRelayServiceClient, PullRtmpStreamRequest};
use crate::relay::StreamRegistryTrait;

/// gRPC Stream Puller
/// Pulls RTMP stream from remote Publisher node via gRPC and publishes to local `StreamHub`
pub struct GrpcStreamPuller {
    room_id: String,
    media_id: String,
    publisher_node_addr: String,
    stream_hub_event_sender: StreamHubEventSender,
    /// Cluster authentication secret (attached as x-cluster-secret metadata)
    cluster_secret: Option<String>,
    /// Shared connection pool for reusing gRPC channels to publisher nodes
    connection_pool: GrpcConnectionPool,
}

impl GrpcStreamPuller {
    pub fn new(
        room_id: String,
        media_id: String,
        publisher_node_addr: String,
        _node_id: String,
        stream_hub_event_sender: StreamHubEventSender,
        _registry: Arc<dyn StreamRegistryTrait>,
    ) -> Self {
        Self {
            room_id,
            media_id,
            publisher_node_addr,
            stream_hub_event_sender,
            cluster_secret: None,
            connection_pool: GrpcConnectionPool::with_defaults(),
        }
    }

    /// Create a new puller with a shared connection pool.
    ///
    /// Preferred over `new()` when a pool is available (e.g., from `PullStreamManager`),
    /// as it reuses HTTP/2 connections to publisher nodes across retry attempts
    /// and across different pull streams targeting the same node.
    pub fn with_pool(
        room_id: String,
        media_id: String,
        publisher_node_addr: String,
        stream_hub_event_sender: StreamHubEventSender,
        connection_pool: GrpcConnectionPool,
    ) -> Self {
        Self {
            room_id,
            media_id,
            publisher_node_addr,
            stream_hub_event_sender,
            cluster_secret: None,
            connection_pool,
        }
    }

    /// Set the cluster authentication secret for inter-node gRPC requests.
    #[must_use]
    pub fn with_cluster_secret(mut self, secret: Option<String>) -> Self {
        self.cluster_secret = secret;
        self
    }

    /// Run the puller with retry logic: connect to remote, pull stream, publish to local `StreamHub`.
    ///
    /// On transient failures (connection refused, stream interrupted), retries with exponential
    /// backoff (1s initial, 30s max, with jitter). Gives up after 10 consecutive failures.
    /// The `PullStreamManager` can recreate the puller on the next viewer request.
    pub async fn run(mut self) -> anyhow::Result<()> {
        const MAX_RETRIES: u32 = 10;
        const INITIAL_BACKOFF_MS: u64 = 1000;
        const MAX_BACKOFF_MS: u64 = 30_000;

        info!(
            room_id = %self.room_id,
            media_id = %self.media_id,
            publisher = %self.publisher_node_addr,
            "Starting gRPC stream puller"
        );

        let mut attempt: u32 = 0;
        /// Minimum connection duration to consider "successful" for retry reset
        const MIN_SUCCESSFUL_DURATION: std::time::Duration = std::time::Duration::from_secs(60);

        loop {
            attempt += 1;

            // 1. Publish to local StreamHub (re-publish on each retry to get a fresh sender)
            let data_sender = match self.publish_to_local_stream_hub().await {
                Ok(sender) => sender,
                Err(e) => {
                    error!(
                        room_id = %self.room_id,
                        attempt = attempt,
                        "Failed to publish to local StreamHub: {e}"
                    );
                    if attempt > MAX_RETRIES {
                        return Err(anyhow::anyhow!(
                            "Gave up after {MAX_RETRIES} retries (last error: publish to StreamHub: {e})"
                        ));
                    }
                    Self::backoff(attempt, INITIAL_BACKOFF_MS, MAX_BACKOFF_MS).await;
                    continue;
                }
            };

            let connect_start = Instant::now();
            let result = self.connect_and_stream(&data_sender).await;
            let stream_duration = connect_start.elapsed();

            // Always clean up local StreamHub before retry or exit
            if let Err(e) = self.unpublish_from_local_stream_hub().await {
                warn!("Failed to unpublish from local StreamHub: {e}");
            }

            match result {
                Ok(()) => {
                    info!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        "Stream ended normally"
                    );
                    return Ok(());
                }
                Err(e) => {
                    // Reset retry counter if the connection was up for a meaningful duration.
                    // This prevents long-running streams from exhausting retries over transient blips.
                    if stream_duration > MIN_SUCCESSFUL_DURATION {
                        info!(
                            room_id = %self.room_id,
                            duration_secs = stream_duration.as_secs(),
                            "Resetting retry counter after successful long connection"
                        );
                        attempt = 0;
                    }

                    if attempt >= MAX_RETRIES {
                        error!(
                            room_id = %self.room_id,
                            media_id = %self.media_id,
                            attempt = attempt,
                            "Gave up after {MAX_RETRIES} retries: {e}"
                        );
                        return Err(anyhow::anyhow!(
                            "Gave up after {MAX_RETRIES} retries (last error: {e})"
                        ));
                    }

                    warn!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        attempt = attempt,
                        max_retries = MAX_RETRIES,
                        "Stream pull failed, retrying: {e}"
                    );

                    Self::backoff(attempt, INITIAL_BACKOFF_MS, MAX_BACKOFF_MS).await;
                }
            }
        }
    }

    /// Connect to remote publisher and stream data to the local `StreamHub`.
    /// Returns `Ok(())` when the stream ends normally, `Err` on connection or protocol failure.
    ///
    /// Uses the shared [`GrpcConnectionPool`] to reuse HTTP/2 connections across
    /// retry attempts and across different pull streams targeting the same node.
    /// On connection failure, the pooled entry is invalidated so the next attempt
    /// creates a fresh connection.
    async fn connect_and_stream(&self, data_sender: &FrameDataSender) -> anyhow::Result<()> {
        let channel = self.connection_pool
            .get_channel(&self.publisher_node_addr)
            .await
            .map_err(|e| {
                self.connection_pool.invalidate(&self.publisher_node_addr);
                anyhow::anyhow!("Failed to connect to publisher: {e}")
            })?;

        let mut client = StreamRelayServiceClient::new(channel);

        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: self.room_id.clone(),
            media_id: self.media_id.clone(),
        });

        // Attach cluster authentication secret if configured
        if let Some(secret) = &self.cluster_secret {
            request.metadata_mut().insert(
                "x-cluster-secret",
                secret.parse().map_err(|_| anyhow::anyhow!("Invalid cluster secret format"))?,
            );
        }

        let mut stream = client
            .pull_rtmp_stream(request)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to pull stream: {e}"))?
            .into_inner();

        info!("Connected to remote publisher, receiving stream data");

        let mut dropped_frames: u64 = 0;
        const DROP_LOG_INTERVAL: u64 = 100;

        while let Some(packet_result) = stream.message().await? {
            let packet = packet_result;

            let frame_data = match packet.frame_type {
                1 => FrameData::Video {
                    timestamp: packet.timestamp,
                    data: Bytes::from(packet.data),
                },
                2 => FrameData::Audio {
                    timestamp: packet.timestamp,
                    data: Bytes::from(packet.data),
                },
                3 => FrameData::MetaData {
                    timestamp: packet.timestamp,
                    data: Bytes::from(packet.data),
                },
                _ => {
                    warn!("Unknown frame type: {}", packet.frame_type);
                    continue;
                }
            };

            // Use try_send for non-blocking behavior
            // If channel is full, drop the packet (backpressure)
            if let Err(mpsc::error::TrySendError::Full(_)) = data_sender.try_send(frame_data) {
                dropped_frames += 1;
                if dropped_frames % DROP_LOG_INTERVAL == 1 {
                    warn!(
                        room_id = %self.room_id,
                        media_id = %self.media_id,
                        total_dropped = dropped_frames,
                        "Frame dropped due to backpressure"
                    );
                }
            }
        }

        Ok(())
    }

    /// Exponential backoff with jitter (delegated to shared utility).
    async fn backoff(attempt: u32, initial_ms: u64, max_ms: u64) {
        crate::util::backoff(attempt, initial_ms, max_ms).await;
    }

    /// Publish to local `StreamHub` (similar to xiu `ClientSession::publish_to_stream_hub`)
    async fn publish_to_local_stream_hub(&mut self) -> anyhow::Result<FrameDataSender> {
        let publisher_id = Uuid::new();

        let publisher_info = PublisherInfo {
            id: publisher_id,
            pub_type: PublishType::RtmpRelay, // Using RtmpRelay for inter-node streaming
            pub_data_type: synctv_xiu::streamhub::define::PubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: format!(
                    "grpc://{}/{}/{}",
                    self.publisher_node_addr, self.room_id, self.media_id
                ),
                remote_addr: self.publisher_node_addr.clone(),
            },
        };

        // Stream name format: room_id/media_id
        let stream_name = format!("{}/{}", self.room_id, self.media_id);
        let identifier = StreamIdentifier::Rtmp {
            app_name: "live".to_string(),
            stream_name: stream_name.clone(),
        };

        let stream_handler = Arc::new(RtmpStreamHandler::new());

        let (event_result_sender, event_result_receiver) = oneshot::channel();
        let publish_event = StreamHubEvent::Publish {
            identifier,
            info: publisher_info,
            stream_handler,
            result_sender: event_result_sender,
        };

        self.stream_hub_event_sender
            .try_send(publish_event)
            .map_err(|_| anyhow::anyhow!("Failed to send publish event"))?;

        let result = event_result_receiver
            .await
            .map_err(|_| anyhow::anyhow!("Publish result channel closed"))?
            .map_err(|e| anyhow::anyhow!("Publish failed: {e}"))?;

        let data_sender = result
            .0
            .ok_or_else(|| anyhow::anyhow!("No data sender from publish result"))?;

        info!("Successfully published to local StreamHub");
        Ok(data_sender)
    }

    /// Unpublish from local `StreamHub`
    async fn unpublish_from_local_stream_hub(&mut self) -> anyhow::Result<()> {
        let stream_name = format!("{}/{}", self.room_id, self.media_id);
        let identifier = StreamIdentifier::Rtmp {
            app_name: "live".to_string(),
            stream_name,
        };

        let unpublish_event = StreamHubEvent::UnPublish { identifier };

        if let Err(e) = self.stream_hub_event_sender.try_send(unpublish_event) {
            warn!("Failed to send unpublish event: {}", e);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::MockStreamRegistry;

    #[tokio::test]
    async fn test_puller_creation() {
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);

        let puller = GrpcStreamPuller::new(
            "room123".to_string(),
            "media456".to_string(),
            "publisher-node:50051".to_string(),
            "node-1".to_string(),
            stream_hub_event_sender,
            std::sync::Arc::new(MockStreamRegistry::new()),
        );

        assert_eq!(puller.room_id, "room123");
        assert_eq!(puller.media_id, "media456");
        assert_eq!(puller.publisher_node_addr, "publisher-node:50051");
        // Default pool should be created
        assert!(puller.connection_pool.is_empty());
    }

    #[tokio::test]
    async fn test_puller_with_shared_pool() {
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);
        let pool = GrpcConnectionPool::with_defaults();

        let puller = GrpcStreamPuller::with_pool(
            "room123".to_string(),
            "media456".to_string(),
            "publisher-node:50051".to_string(),
            stream_hub_event_sender,
            pool.clone(),
        );

        assert_eq!(puller.room_id, "room123");
        assert_eq!(puller.media_id, "media456");
        assert_eq!(puller.publisher_node_addr, "publisher-node:50051");
        // Pool should be shared (same underlying Arc)
        assert!(puller.connection_pool.is_empty());
        assert_eq!(puller.connection_pool.len(), pool.len());
    }

    #[tokio::test]
    async fn test_puller_with_cluster_secret() {
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);

        let puller = GrpcStreamPuller::new(
            "room".to_string(),
            "media".to_string(),
            "node:50051".to_string(),
            "local".to_string(),
            stream_hub_event_sender,
            std::sync::Arc::new(MockStreamRegistry::new()),
        )
        .with_cluster_secret(Some("test-secret".to_string()));

        assert_eq!(puller.cluster_secret.as_deref(), Some("test-secret"));
    }
}
