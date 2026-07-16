use std::sync::Arc;
use synctv_xiu::rtmp::session::common::RtmpStreamHandler;
use synctv_xiu::streamhub::{
    define::{
        FrameData, FrameDataSender, NotifyInfo, PublishType, PublisherInfo, StreamHubEvent,
        StreamHubEventSender,
    },
    send_event_with_backpressure_timeout_for,
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio::sync::oneshot;
use tonic::codec::CompressionEncoding;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::Request;
use tracing::{error, info, warn};

use super::connection_pool::GrpcConnectionPool;
use super::proto::{stream_relay_service_client::StreamRelayServiceClient, PullRtmpStreamRequest};

const STREAM_MESSAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const FRAME_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const STREAMHUB_EVENT_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn timeout_stream_message<T, E>(
    timeout: std::time::Duration,
    future: impl std::future::Future<Output = Result<Option<T>, E>>,
) -> anyhow::Result<Option<T>>
where
    E: std::fmt::Display,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "No gRPC relay frame received for {}s, stream appears dead",
                timeout.as_secs()
            )
        })?
        .map_err(|e| anyhow::anyhow!("Stream error: {e}"))
}

async fn next_packet_with_timeout(
    stream: &mut tonic::Streaming<super::proto::RtmpPacket>,
) -> anyhow::Result<Option<super::proto::RtmpPacket>> {
    timeout_stream_message(STREAM_MESSAGE_TIMEOUT, stream.message()).await
}

async fn send_frame_with_backpressure(
    data_sender: &FrameDataSender,
    frame_data: FrameData,
) -> anyhow::Result<()> {
    tokio::time::timeout(FRAME_SEND_TIMEOUT, data_sender.send(frame_data))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Timed out waiting {}s for local relay backpressure to clear",
                FRAME_SEND_TIMEOUT.as_secs()
            )
        })?
        .map_err(|_| anyhow::anyhow!("Local relay stream channel closed"))
}
/// gRPC Stream Puller
/// Pulls RTMP stream from remote Publisher node via gRPC and publishes to local `StreamHub`
pub(crate) struct GrpcStreamPuller {
    room_id: String,
    media_id: String,
    publisher_node_addr: String,
    expected_epoch: u64,
    stream_hub_event_sender: StreamHubEventSender,
    /// Cluster authentication secret (attached as x-cluster-secret metadata)
    cluster_secret: Option<String>,
    /// Shared connection pool for reusing gRPC channels to publisher nodes
    connection_pool: GrpcConnectionPool,
    /// Maximum gRPC message size for relay stream messages.
    grpc_max_message_size_bytes: usize,
    /// Whether relay stream clients should negotiate gzip compression.
    grpc_compression_enabled: bool,
}

impl GrpcStreamPuller {
    /// Create a new puller with a shared connection pool.
    ///
    /// A shared pool reuses HTTP/2 connections across pull streams targeting
    /// the same node.
    #[must_use]
    pub(crate) const fn new(
        room_id: String,
        media_id: String,
        publisher_node_addr: String,
        expected_epoch: u64,
        stream_hub_event_sender: StreamHubEventSender,
        connection_pool: GrpcConnectionPool,
    ) -> Self {
        Self {
            room_id,
            media_id,
            publisher_node_addr,
            expected_epoch,
            stream_hub_event_sender,
            cluster_secret: None,
            connection_pool,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
        }
    }

    /// Set the cluster authentication secret for inter-node gRPC requests.
    #[must_use]
    pub(crate) fn with_cluster_secret(mut self, secret: Option<String>) -> Self {
        self.cluster_secret = secret;
        self
    }

    /// Set the maximum gRPC message size for relay stream messages.
    #[must_use]
    pub(crate) const fn with_grpc_max_message_size(
        mut self,
        max_message_size_bytes: usize,
    ) -> Self {
        self.grpc_max_message_size_bytes = max_message_size_bytes;
        self
    }

    /// Enable or disable gzip compression negotiation for relay stream calls.
    #[must_use]
    pub(crate) const fn with_grpc_compression(mut self, enabled: bool) -> Self {
        self.grpc_compression_enabled = enabled;
        self
    }

    fn cluster_secret_metadata(&self) -> anyhow::Result<MetadataValue<Ascii>> {
        let secret = self
            .cluster_secret
            .as_deref()
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| anyhow::anyhow!("cluster secret is required for remote stream relay"))?;
        secret
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid cluster secret format"))
    }

    /// Run one pull session: publish locally, connect to the remote publisher,
    /// forward frames, then unpublish from the local `StreamHub`.
    pub(crate) async fn run(mut self) -> anyhow::Result<()> {
        info!(
            room_id = %self.room_id,
            media_id = %self.media_id,
            publisher = %self.publisher_node_addr,
            expected_epoch = self.expected_epoch,
            "Starting gRPC stream puller"
        );

        self.cluster_secret_metadata()?;

        // Publish to local StreamHub to get a frame data sender for this pull session.
        let data_sender = match self.publish_to_local_stream_hub().await {
            Ok(sender) => sender,
            Err(e) => {
                error!(
                    room_id = %self.room_id,
                    "Failed to publish to local StreamHub: {e}"
                );
                return Err(anyhow::anyhow!("Failed to publish to local StreamHub: {e}"));
            }
        };

        let result = self.connect_and_stream(&data_sender).await;

        // Always clean up local StreamHub publication before returning
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
                Ok(())
            }
            Err(e) => {
                warn!(
                    room_id = %self.room_id,
                    media_id = %self.media_id,
                    "Stream pull ended with error: {e}"
                );
                Err(e)
            }
        }
    }

    /// Connect to remote publisher and stream data to the local `StreamHub`.
    /// Returns `Ok(())` when the stream ends normally, `Err` on connection or protocol failure.
    ///
    /// Uses the shared [`GrpcConnectionPool`] to reuse HTTP/2 connections across
    /// pull streams targeting the same node.
    /// On connection failure, the pooled entry is invalidated so the next attempt
    /// creates a fresh connection.
    async fn connect_and_stream(&self, data_sender: &FrameDataSender) -> anyhow::Result<()> {
        let cluster_secret = self.cluster_secret_metadata()?;

        let channel = self
            .connection_pool
            .get_channel(&self.publisher_node_addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to publisher: {e}"))?;

        let client = StreamRelayServiceClient::new(channel)
            .max_decoding_message_size(self.grpc_max_message_size_bytes)
            .max_encoding_message_size(self.grpc_max_message_size_bytes);
        let mut client = if self.grpc_compression_enabled {
            client
                .accept_compressed(CompressionEncoding::Gzip)
                .send_compressed(CompressionEncoding::Gzip)
        } else {
            client
        };

        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: self.room_id.clone(),
            media_id: self.media_id.clone(),
            is_reconnect: false,
            expected_epoch: self.expected_epoch,
        });

        request
            .metadata_mut()
            .insert("x-cluster-secret", cluster_secret);

        let stream_result = client.pull_rtmp_stream(request).await;
        let mut stream = match stream_result {
            Ok(response) => response.into_inner(),
            Err(e) => {
                self.connection_pool
                    .invalidate(&self.publisher_node_addr)
                    .await;
                return Err(anyhow::anyhow!("Failed to pull stream: {e}"));
            }
        };

        info!("Connected to remote publisher, receiving stream data");

        let mut dropped_frames: u64 = 0;
        let drop_log_interval: u64 = 100;

        loop {
            match next_packet_with_timeout(&mut stream).await {
                Ok(Some(packet)) => {
                    let frame_data = match packet.frame_type {
                        1 => FrameData::Video {
                            timestamp: packet.timestamp,
                            data: packet.data,
                        },
                        2 => FrameData::Audio {
                            timestamp: packet.timestamp,
                            data: packet.data,
                        },
                        3 => FrameData::MetaData {
                            timestamp: packet.timestamp,
                            data: packet.data,
                        },
                        _ => {
                            warn!("Unknown frame type: {}", packet.frame_type);
                            continue;
                        }
                    };

                    if let Err(e) = send_frame_with_backpressure(data_sender, frame_data).await {
                        dropped_frames += 1;
                        synctv_core::metrics::livestream::LIVESTREAM_RELAY_FRAME_DROPS.inc();
                        warn!(
                            room_id = %self.room_id,
                            media_id = %self.media_id,
                            total_blocked = dropped_frames,
                            "Relay frame delivery failed under backpressure: {e}"
                        );
                        return Err(e);
                    }
                }
                Ok(None) => break, // Stream ended normally
                Err(e) => {
                    self.connection_pool
                        .invalidate(&self.publisher_node_addr)
                        .await;
                    if dropped_frames >= drop_log_interval {
                        warn!(
                            room_id = %self.room_id,
                            media_id = %self.media_id,
                            blocked_frames_before_failure = dropped_frames,
                            "Relay stream terminated after sustained backpressure failures"
                        );
                    }
                    return Err(e);
                }
            }
        }

        Ok(())
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

        // Use canonical (room_id, media_id) format matching RTMP publish identifier
        let identifier = StreamIdentifier::Rtmp {
            app_name: self.room_id.clone(),
            stream_name: self.media_id.clone(),
        };

        let stream_handler = Arc::new(RtmpStreamHandler::new());

        let (event_result_sender, event_result_receiver) = oneshot::channel();
        let publish_event = StreamHubEvent::Publish {
            identifier,
            info: publisher_info,
            stream_handler,
            result_sender: event_result_sender,
        };

        send_event_with_backpressure_timeout_for(
            &self.stream_hub_event_sender,
            publish_event,
            STREAMHUB_EVENT_SEND_TIMEOUT,
        )
        .await
        .map_err(|error| anyhow::anyhow!("Failed to send publish event: {error}"))?;

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
        let identifier = StreamIdentifier::Rtmp {
            app_name: self.room_id.clone(),
            stream_name: self.media_id.clone(),
        };

        let unpublish_event = StreamHubEvent::UnPublish { identifier };

        if let Err(e) = send_event_with_backpressure_timeout_for(
            &self.stream_hub_event_sender,
            unpublish_event,
            STREAMHUB_EVENT_SEND_TIMEOUT,
        )
        .await
        {
            warn!(
                room_id = %self.room_id,
                media_id = %self.media_id,
                "Failed to send unpublish event to StreamHub: {}",
                e
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    const TEST_STREAM_MESSAGE_TIMEOUT: Duration = Duration::from_millis(50);
    #[tokio::test]
    async fn test_puller_creation() {
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);
        let pool = GrpcConnectionPool::with_defaults();

        let puller = GrpcStreamPuller::new(
            "room123".to_string(),
            "media456".to_string(),
            "publisher-node:50051".to_string(),
            7,
            stream_hub_event_sender,
            pool.clone(),
        );

        assert_eq!(puller.room_id, "room123");
        assert_eq!(puller.media_id, "media456");
        assert_eq!(puller.publisher_node_addr, "publisher-node:50051");
        // Pool should be shared
        assert!(puller.connection_pool.is_empty());
        assert_eq!(puller.connection_pool.len(), pool.len());
    }

    #[tokio::test]
    async fn test_puller_with_cluster_secret() {
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);
        let pool = GrpcConnectionPool::with_defaults();

        let puller = GrpcStreamPuller::new(
            "room".to_string(),
            "media".to_string(),
            "node:50051".to_string(),
            7,
            stream_hub_event_sender,
            pool,
        )
        .with_cluster_secret(Some("test-secret".to_string()));

        assert_eq!(puller.cluster_secret.as_deref(), Some("test-secret"));
    }

    #[tokio::test]
    async fn test_puller_rejects_missing_cluster_secret() {
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);
        let pool = GrpcConnectionPool::with_defaults();

        let puller = GrpcStreamPuller::new(
            "room".to_string(),
            "media".to_string(),
            "node:50051".to_string(),
            7,
            stream_hub_event_sender,
            pool,
        );

        let error = puller
            .cluster_secret_metadata()
            .expect_err("remote stream relay must fail fast without a cluster secret");

        assert!(
            error.to_string().contains("cluster secret is required"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_puller_rejects_empty_cluster_secret() {
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);
        let pool = GrpcConnectionPool::with_defaults();

        let puller = GrpcStreamPuller::new(
            "room".to_string(),
            "media".to_string(),
            "node:50051".to_string(),
            7,
            stream_hub_event_sender,
            pool,
        )
        .with_cluster_secret(Some(String::new()));

        let error = puller
            .cluster_secret_metadata()
            .expect_err("remote stream relay must fail fast with an empty cluster secret");

        assert!(
            error.to_string().contains("cluster secret is required"),
            "unexpected error: {error}"
        );
    }

    /// Test that `GrpcStreamPuller` uses the connection pool correctly.
    ///
    /// This test verifies that:
    /// 1. The puller is created with the correct connection pool
    /// 2. The pool is shared (not cloned) across multiple pullers
    #[tokio::test]
    async fn test_puller_uses_shared_connection_pool() {
        let (stream_hub_event_sender, _) = tokio::sync::mpsc::channel(64);
        let pool = GrpcConnectionPool::with_defaults();

        // Create two pullers with the same pool
        let puller1 = GrpcStreamPuller::new(
            "room1".to_string(),
            "media1".to_string(),
            "node1:50051".to_string(),
            7,
            stream_hub_event_sender.clone(),
            pool.clone(),
        );

        let puller2 = GrpcStreamPuller::new(
            "room2".to_string(),
            "media2".to_string(),
            "node2:50051".to_string(),
            8,
            stream_hub_event_sender,
            pool.clone(),
        );

        // Both pullers should have empty pools initially
        assert!(puller1.connection_pool.is_empty());
        assert!(puller2.connection_pool.is_empty());

        // Both should share the same underlying pool
        assert_eq!(puller1.connection_pool.len(), pool.len());
        assert_eq!(puller2.connection_pool.len(), pool.len());
    }

    #[tokio::test]
    async fn test_send_frame_with_backpressure_times_out_instead_of_dropping_silently() -> TestResult
    {
        let (data_sender, mut data_receiver) = mpsc::channel(1);
        let data_sender = FrameDataSender::bounded(data_sender);
        data_sender
            .send(FrameData::MetaData {
                timestamp: 1,
                data: bytes::Bytes::from_static(b"first"),
            })
            .await
            .map_err(|_| test_error("initial send should fill channel"))?;

        let send_future = send_frame_with_backpressure(
            &data_sender,
            FrameData::Video {
                timestamp: 2,
                data: bytes::Bytes::from_static(b"second"),
            },
        );

        let err = tokio::time::timeout(FRAME_SEND_TIMEOUT + Duration::from_secs(1), send_future)
            .await?
            .expect_err("second send must fail once backpressure exceeds timeout");
        assert!(
            err.to_string().contains("backpressure"),
            "unexpected error: {err}"
        );

        let first = data_receiver
            .recv()
            .await
            .ok_or_else(|| test_error("first frame remains queued"))?;
        assert!(matches!(first, FrameData::MetaData { .. }));
        assert!(
            data_receiver.try_recv().is_err(),
            "timed out send must not enqueue an extra frame"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_publish_to_local_stream_hub_waits_for_backpressure_instead_of_failing_immediately(
    ) -> TestResult {
        let (stream_hub_event_sender, mut stream_hub_event_receiver) = mpsc::channel(1);
        stream_hub_event_sender
            .send(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "occupied".to_string(),
                    stream_name: "occupied".to_string(),
                },
            })
            .await?;

        let release_handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            stream_hub_event_receiver
                .recv()
                .await
                .ok_or_else(|| test_error("occupied event should be drained"))?;
            if let Some(StreamHubEvent::Publish { result_sender, .. }) =
                stream_hub_event_receiver.recv().await
            {
                let (data_sender, _) = mpsc::channel(4);
                result_sender
                    .send(Ok((
                        Some(FrameDataSender::bounded(data_sender)),
                        None,
                        None,
                    )))
                    .map_err(|_| test_error("publish result receiver should be alive"))?;
                Ok(())
            } else {
                Err(test_error(
                    "expected publish event after backpressure cleared",
                ))
            }
        });

        let mut puller = GrpcStreamPuller::new(
            "room".to_string(),
            "media".to_string(),
            "publisher:50051".to_string(),
            7,
            stream_hub_event_sender,
            GrpcConnectionPool::with_defaults(),
        );

        let result =
            tokio::time::timeout(Duration::from_secs(2), puller.publish_to_local_stream_hub())
                .await?;

        assert!(
            result.is_ok(),
            "publish should succeed after brief backpressure"
        );
        release_handle.await??;
        Ok(())
    }

    #[tokio::test]
    async fn test_next_packet_with_timeout_fails_when_stream_stalls() -> TestResult {
        let err = tokio::time::timeout(
            TEST_STREAM_MESSAGE_TIMEOUT + Duration::from_secs(1),
            timeout_stream_message(TEST_STREAM_MESSAGE_TIMEOUT, async {
                tokio::time::sleep(TEST_STREAM_MESSAGE_TIMEOUT + Duration::from_secs(1)).await;
                Ok::<Option<super::super::proto::RtmpPacket>, tonic::Status>(None)
            }),
        )
        .await?
        .expect_err("stalled stream must be reported as dead");
        assert!(
            err.to_string().contains("stream appears dead"),
            "unexpected error: {err}"
        );
        Ok(())
    }
}
