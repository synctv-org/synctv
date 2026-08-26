use synctv_xiu::streamhub::define::{FrameData, FrameDataSender, PacketData, PacketDataSender};
use tokio::time::Instant;
use tonic::codec::CompressionEncoding;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::Request;
use tracing::{info, warn};

use super::connection_pool::GrpcConnectionPool;
use super::proto::{
    stream_relay_service_client::StreamRelayServiceClient, PullRtmpStreamRequest, RtpPacket,
    RtpPacketType,
};

const STREAM_MESSAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const FRAME_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug)]
struct RelayIdleDeadlines {
    timeout: std::time::Duration,
    frame: Instant,
    rtp: Option<Instant>,
}

impl RelayIdleDeadlines {
    fn new(now: Instant, timeout: std::time::Duration, expects_rtp: bool) -> Self {
        let deadline = now + timeout;
        Self {
            timeout,
            frame: deadline,
            rtp: expects_rtp.then_some(deadline),
        }
    }

    fn observe_frame(&mut self, now: Instant) {
        self.frame = now + self.timeout;
    }

    fn observe_rtp(&mut self, now: Instant) {
        self.rtp = Some(now + self.timeout);
    }
}

fn map_stream_message<T>(result: Result<Option<T>, tonic::Status>) -> anyhow::Result<Option<T>> {
    result.map_err(|error| anyhow::anyhow!("Stream error: {error}"))
}

fn stream_idle_timeout(stream_kind: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "No gRPC {stream_kind} relay message received for {}s, stream appears dead",
        STREAM_MESSAGE_TIMEOUT.as_secs()
    )
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

fn decode_frame_packet(packet: super::proto::RtmpPacket) -> Option<FrameData> {
    match packet.frame_type {
        1 => Some(FrameData::Video {
            timestamp: packet.timestamp,
            data: packet.data,
        }),
        2 => Some(FrameData::Audio {
            timestamp: packet.timestamp,
            data: packet.data,
        }),
        3 => Some(FrameData::MetaData {
            timestamp: packet.timestamp,
            data: packet.data,
        }),
        _ => None,
    }
}

fn decode_rtp_packet(packet: RtpPacket) -> Option<PacketData> {
    match RtpPacketType::try_from(packet.packet_type).ok()? {
        RtpPacketType::Video => Some(PacketData::Video {
            timestamp: packet.timestamp,
            data: packet.data,
        }),
        RtpPacketType::Audio => Some(PacketData::Audio {
            timestamp: packet.timestamp,
            data: packet.data,
        }),
        RtpPacketType::Unspecified => None,
    }
}
/// gRPC Stream Puller
/// Pulls one RTMP relay session from a remote publisher and forwards frames to
/// an existing local `StreamHub` publication.
pub(crate) struct GrpcStreamPuller {
    room_id: String,
    media_id: String,
    publisher_node_addr: String,
    generation_id: String,
    expected_lease_epoch: u64,
    is_reconnect: bool,
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
        generation_id: String,
        expected_lease_epoch: u64,
        connection_pool: GrpcConnectionPool,
        is_reconnect: bool,
    ) -> Self {
        Self {
            room_id,
            media_id,
            publisher_node_addr,
            generation_id,
            expected_lease_epoch,
            is_reconnect,
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

    /// Run one remote pull session using a local publication owned by `PullStream`.
    pub(crate) async fn run(
        self,
        data_sender: &FrameDataSender,
        packet_sender: Option<&PacketDataSender>,
    ) -> anyhow::Result<()> {
        info!(
            room_id = %self.room_id,
            media_id = %self.media_id,
            publisher = %self.publisher_node_addr,
            generation_id = %self.generation_id,
            expected_lease_epoch = self.expected_lease_epoch,
            "Starting gRPC stream puller"
        );

        self.cluster_secret_metadata()?;

        let result = self.connect_and_stream(data_sender, packet_sender).await;

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
    pub(crate) async fn supports_rtp(&self) -> anyhow::Result<bool> {
        let cluster_secret = self.cluster_secret_metadata()?;
        let channel = self
            .connection_pool
            .get_channel(&self.publisher_node_addr)
            .await
            .map_err(|error| anyhow::anyhow!("Failed to connect to publisher: {error}"))?;
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
            generation_id: self.generation_id.clone(),
            expected_lease_epoch: self.expected_lease_epoch,
            is_reconnect: false,
        });
        request
            .metadata_mut()
            .insert("x-cluster-secret", cluster_secret);

        match client.pull_rtp_stream(request).await {
            Ok(response) => {
                drop(response);
                Ok(true)
            }
            Err(status)
                if matches!(
                    status.code(),
                    tonic::Code::FailedPrecondition | tonic::Code::Unimplemented
                ) =>
            {
                Ok(false)
            }
            Err(status) => {
                self.connection_pool
                    .invalidate(&self.publisher_node_addr)
                    .await;
                Err(anyhow::anyhow!(
                    "Failed to probe RTP relay support: {status}"
                ))
            }
        }
    }

    async fn connect_and_stream(
        &self,
        data_sender: &FrameDataSender,
        packet_sender: Option<&PacketDataSender>,
    ) -> anyhow::Result<()> {
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
            generation_id: self.generation_id.clone(),
            expected_lease_epoch: self.expected_lease_epoch,
            is_reconnect: self.is_reconnect,
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

        let mut rtp_stream = if packet_sender.is_some() {
            let mut request = Request::new(PullRtmpStreamRequest {
                room_id: self.room_id.clone(),
                media_id: self.media_id.clone(),
                generation_id: self.generation_id.clone(),
                expected_lease_epoch: self.expected_lease_epoch,
                is_reconnect: self.is_reconnect,
            });
            request
                .metadata_mut()
                .insert("x-cluster-secret", self.cluster_secret_metadata()?);
            match client.pull_rtp_stream(request).await {
                Ok(response) => Some(response.into_inner()),
                Err(error) => {
                    self.connection_pool
                        .invalidate(&self.publisher_node_addr)
                        .await;
                    return Err(anyhow::anyhow!("Failed to pull RTP stream: {error}"));
                }
            }
        } else {
            None
        };

        info!("Connected to remote publisher, receiving stream data");

        let mut dropped_frames: u64 = 0;
        let mut dropped_packets: u64 = 0;
        let drop_log_interval: u64 = 100;
        let mut idle_deadlines =
            RelayIdleDeadlines::new(Instant::now(), STREAM_MESSAGE_TIMEOUT, rtp_stream.is_some());

        loop {
            enum RelayMessage {
                Frame(anyhow::Result<Option<super::proto::RtmpPacket>>),
                Rtp(anyhow::Result<Option<RtpPacket>>),
            }
            let frame_deadline = idle_deadlines.frame;
            let rtp_deadline = idle_deadlines.rtp;
            let message = tokio::select! {
                frame = stream.message() => RelayMessage::Frame(map_stream_message(frame)),
                rtp = async {
                    match rtp_stream.as_mut() {
                        Some(stream) => map_stream_message(stream.message().await),
                        None => std::future::pending().await,
                    }
                } => RelayMessage::Rtp(rtp),
                () = tokio::time::sleep_until(frame_deadline) => {
                    RelayMessage::Frame(Err(stream_idle_timeout("frame")))
                }
                () = async {
                    match rtp_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => RelayMessage::Rtp(Err(stream_idle_timeout("RTP"))),
            };
            match message {
                RelayMessage::Frame(Ok(Some(packet))) => {
                    idle_deadlines.observe_frame(Instant::now());
                    let Some(frame_data) = decode_frame_packet(packet) else {
                        warn!("Received unknown relay frame type");
                        continue;
                    };
                    if let Err(error) = send_frame_with_backpressure(data_sender, frame_data).await
                    {
                        dropped_frames += 1;
                        synctv_core::metrics::livestream::LIVESTREAM_RELAY_FRAME_DROPS.inc();
                        warn!(
                            room_id = %self.room_id,
                            media_id = %self.media_id,
                            total_blocked = dropped_frames,
                            "Relay frame delivery failed under backpressure: {error}"
                        );
                        return Err(error);
                    }
                }
                RelayMessage::Rtp(Ok(Some(packet))) => {
                    idle_deadlines.observe_rtp(Instant::now());
                    let Some(packet_data) = decode_rtp_packet(packet) else {
                        warn!("Received unknown relay RTP packet type");
                        continue;
                    };
                    let Some(packet_sender) = packet_sender else {
                        return Err(anyhow::anyhow!(
                            "RTP relay produced data without a local packet channel"
                        ));
                    };
                    match packet_sender.try_send(packet_data) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            dropped_packets += 1;
                            if dropped_packets.is_multiple_of(drop_log_interval) {
                                warn!(
                                    room_id = %self.room_id,
                                    media_id = %self.media_id,
                                    dropped_packets,
                                    "Dropping relayed RTP packets under local backpressure"
                                );
                            }
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            return Err(anyhow::anyhow!("Local RTP relay channel closed"));
                        }
                    }
                }
                RelayMessage::Frame(Ok(None)) if rtp_stream.is_none() => break,
                RelayMessage::Frame(Ok(None)) | RelayMessage::Rtp(Ok(None)) => {
                    return Err(anyhow::anyhow!("Remote relay stream ended"));
                }
                RelayMessage::Frame(Err(error)) | RelayMessage::Rtp(Err(error)) => {
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
                    return Err(error);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::TEST_GENERATION_ID;
    use std::time::Duration;
    use tokio::sync::mpsc;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    const TEST_STREAM_MESSAGE_TIMEOUT: Duration = Duration::from_millis(50);
    #[tokio::test]
    async fn test_puller_with_cluster_secret() {
        let pool = GrpcConnectionPool::with_defaults();

        let puller = GrpcStreamPuller::new(
            "room".to_string(),
            "media".to_string(),
            "node:50051".to_string(),
            TEST_GENERATION_ID.to_string(),
            7,
            pool,
            false,
        )
        .with_cluster_secret(Some("test-secret".to_string()));

        assert_eq!(puller.cluster_secret.as_deref(), Some("test-secret"));
    }

    #[tokio::test]
    async fn test_puller_rejects_missing_cluster_secret() {
        let pool = GrpcConnectionPool::with_defaults();

        let puller = GrpcStreamPuller::new(
            "room".to_string(),
            "media".to_string(),
            "node:50051".to_string(),
            TEST_GENERATION_ID.to_string(),
            7,
            pool,
            false,
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
        let pool = GrpcConnectionPool::with_defaults();

        let puller = GrpcStreamPuller::new(
            "room".to_string(),
            "media".to_string(),
            "node:50051".to_string(),
            TEST_GENERATION_ID.to_string(),
            7,
            pool,
            false,
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
        let pool = GrpcConnectionPool::with_defaults();

        // Create two pullers with the same pool
        let puller1 = GrpcStreamPuller::new(
            "room1".to_string(),
            "media1".to_string(),
            "node1:50051".to_string(),
            TEST_GENERATION_ID.to_string(),
            7,
            pool.clone(),
            false,
        );

        let puller2 = GrpcStreamPuller::new(
            "room2".to_string(),
            "media2".to_string(),
            "node2:50051".to_string(),
            TEST_GENERATION_ID.to_string(),
            8,
            pool.clone(),
            true,
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

    #[tokio::test(start_paused = true)]
    async fn frame_activity_does_not_refresh_rtp_idle_deadline() {
        let started_at = Instant::now();
        let mut deadlines = RelayIdleDeadlines::new(started_at, TEST_STREAM_MESSAGE_TIMEOUT, true);
        let original_rtp_deadline = deadlines.rtp.expect("RTP deadline should exist");

        tokio::time::advance(TEST_STREAM_MESSAGE_TIMEOUT / 2).await;
        deadlines.observe_frame(Instant::now());
        tokio::time::advance(TEST_STREAM_MESSAGE_TIMEOUT / 2).await;

        assert_eq!(deadlines.rtp, Some(original_rtp_deadline));
        assert!(Instant::now() >= original_rtp_deadline);
        assert!(Instant::now() < deadlines.frame);
    }
}
