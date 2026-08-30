// Pull stream instance — single gRPC relay stream with lifecycle management
// Pulls RTMP data from a publisher node via gRPC and publishes it into
// the local StreamHub. GOP cache is handled by StreamHub internally.

use crate::{
    error::StreamResult,
    grpc::stream_puller::GrpcStreamPuller,
    grpc::GrpcConnectionPool,
    livestream::managed_stream::{ManagedStream, StreamLifecycle},
    relay::registry_trait::StreamRegistryTrait,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use synctv_xiu::rtmp::session::common::RtmpStreamHandler;
use synctv_xiu::streamhub::stream::StreamIdentifier;
use synctv_xiu::streamhub::{
    define::{
        FrameDataSender, NotifyInfo, PacketDataSender, PubDataType, PublishType, PublisherInfo,
        StreamHubEvent, StreamHubEventSender,
    },
    send_event_with_backpressure_timeout_for, spawn_event_delivery_with_backpressure_timeout_for,
    utils::Uuid,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const STREAMHUB_EVENT_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

struct LocalRelayPublication {
    frame_sender: FrameDataSender,
    packet_sender: Option<PacketDataSender>,
}

#[derive(Clone, Copy)]
struct RelayRetryPolicy {
    max_rebuilds: u32,
    rebuild_delay: std::time::Duration,
    epoch_revalidation_interval: std::time::Duration,
    max_consecutive_epoch_failures: u32,
}

impl Default for RelayRetryPolicy {
    fn default() -> Self {
        Self {
            max_rebuilds: 3,
            rebuild_delay: std::time::Duration::from_secs(5),
            epoch_revalidation_interval: std::time::Duration::from_secs(30),
            max_consecutive_epoch_failures: 3,
        }
    }
}

pub(crate) struct PullStreamRoute {
    room_id: String,
    media_id: String,
    publisher_address: String,
    generation_id: String,
    lease_epoch: u64,
}

impl PullStreamRoute {
    pub(crate) fn new(
        room_id: String,
        media_id: String,
        publisher_address: String,
        generation_id: String,
        lease_epoch: u64,
    ) -> Self {
        Self {
            room_id,
            media_id,
            publisher_address,
            generation_id,
            lease_epoch,
        }
    }
}

/// Pull stream instance (pulls RTMP from publisher via gRPC, serves FLV to local clients)
///
/// GOP cache is handled by xiu's `StreamHub` — when the gRPC puller publishes
/// frames to the local `StreamHub`, and a new subscriber joins, `StreamHub`
/// automatically sends cached GOP frames via `send_prior_data`.
pub(crate) struct PullStream {
    pub(crate) room_id: String,
    pub(crate) media_id: String,
    pub(crate) publisher_address: String,
    source_generation_id: String,
    registry: Arc<dyn StreamRegistryTrait>,
    stream_hub_event_sender: StreamHubEventSender,
    lifecycle: StreamLifecycle,
    /// Fencing token (lease_epoch) from when the stream was created.
    /// Used to detect split-brain when publisher changes during network partition.
    lease_epoch: u64,
    /// Cancellation token for graceful shutdown propagation.
    cancel_token: CancellationToken,
    /// Shared ownership flag that guarantees exactly one local `UnPublish` across
    /// natural completion, explicit stop, and drop cleanup.
    local_publication_active: Arc<AtomicBool>,
    local_generation_id: Uuid,
    /// Shared gRPC connection pool for reusing HTTP/2 channels to publisher nodes.
    connection_pool: GrpcConnectionPool,
    /// Maximum gRPC message size for relay calls.
    grpc_max_message_size_bytes: usize,
    /// Whether relay calls negotiate gzip compression.
    grpc_compression_enabled: bool,
    /// Cluster authentication secret passed to `GrpcStreamPuller` for inter-node gRPC requests.
    cluster_secret: Option<String>,
    retry_policy: RelayRetryPolicy,
}

#[async_trait::async_trait]
impl ManagedStream for PullStream {
    fn lifecycle(&self) -> &StreamLifecycle {
        &self.lifecycle
    }

    async fn stop_managed(&self) {
        if let Err(error) = self.stop().await {
            warn!(
                room_id = %self.room_id,
                media_id = %self.media_id,
                %error,
                "Failed to stop pull stream during managed cleanup"
            );
        }
    }
}

impl PullStream {
    /// Create a new `PullStream` with a shared gRPC connection pool.
    pub(crate) fn with_pool(
        route: PullStreamRoute,
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        connection_pool: GrpcConnectionPool,
    ) -> Self {
        Self {
            room_id: route.room_id,
            media_id: route.media_id,
            publisher_address: route.publisher_address,
            source_generation_id: route.generation_id,
            registry,
            stream_hub_event_sender,
            lifecycle: StreamLifecycle::new(),
            lease_epoch: route.lease_epoch,
            cancel_token: CancellationToken::new(),
            local_publication_active: Arc::new(AtomicBool::new(false)),
            local_generation_id: Uuid::new(),
            connection_pool,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            cluster_secret: None,
            retry_policy: RelayRetryPolicy::default(),
        }
    }

    /// Set the maximum gRPC message size for relay calls.
    #[must_use]
    pub(crate) const fn with_grpc_max_message_size(
        mut self,
        max_message_size_bytes: usize,
    ) -> Self {
        self.grpc_max_message_size_bytes = max_message_size_bytes;
        self
    }

    /// Enable or disable gzip compression negotiation for relay calls.
    #[must_use]
    pub(crate) const fn with_grpc_compression(mut self, enabled: bool) -> Self {
        self.grpc_compression_enabled = enabled;
        self
    }

    /// Set the cluster authentication secret for inter-node gRPC requests.
    #[must_use]
    pub(crate) fn with_cluster_secret(mut self, secret: Option<String>) -> Self {
        self.cluster_secret = secret;
        self
    }

    #[cfg(test)]
    #[must_use]
    const fn with_retry_policy(mut self, retry_policy: RelayRetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    async fn publish_to_local_stream_hub(
        &self,
        supports_rtp: bool,
    ) -> StreamResult<LocalRelayPublication> {
        let publisher_info = PublisherInfo {
            id: self.local_generation_id,
            pub_type: PublishType::RtmpRelay,
            pub_data_type: if supports_rtp {
                PubDataType::Both
            } else {
                PubDataType::Frame
            },
            notify_info: NotifyInfo {
                request_url: format!(
                    "grpc://{}/{}/{}",
                    self.publisher_address, self.room_id, self.media_id
                ),
                remote_addr: self.publisher_address.clone(),
            },
        };
        let identifier = StreamIdentifier::Rtmp {
            app_name: self.room_id.clone(),
            stream_name: self.media_id.clone(),
        };
        let (result_sender, result_receiver) = oneshot::channel();

        send_event_with_backpressure_timeout_for(
            &self.stream_hub_event_sender,
            StreamHubEvent::Publish {
                identifier,
                info: publisher_info,
                stream_handler: Arc::new(RtmpStreamHandler::new()),
                result_sender,
            },
            STREAMHUB_EVENT_SEND_TIMEOUT,
        )
        .await
        .map_err(|error| {
            crate::error::StreamError::StreamHubError(format!(
                "Failed to send local relay publish event: {error}"
            ))
        })?;

        let result = result_receiver
            .await
            .map_err(|_| {
                crate::error::StreamError::StreamHubError(
                    "Local relay publish result channel closed".to_string(),
                )
            })?
            .map_err(|error| {
                crate::error::StreamError::StreamHubError(format!(
                    "Local relay publication failed: {error}"
                ))
            })?;
        let data_sender = result.0.ok_or_else(|| {
            crate::error::StreamError::StreamHubError(
                "Local relay publication returned no frame sender".to_string(),
            )
        })?;
        let packet_sender = result.1;
        if supports_rtp && packet_sender.is_none() {
            return Err(crate::error::StreamError::StreamHubError(
                "Local RTP relay publication returned no packet sender".to_string(),
            ));
        }

        self.local_publication_active.store(true, Ordering::Release);
        info!(
            room_id = %self.room_id,
            media_id = %self.media_id,
            "Published gRPC relay stream to local StreamHub"
        );
        Ok(LocalRelayPublication {
            frame_sender: data_sender,
            packet_sender,
        })
    }

    async fn unpublish_local_stream_hub(
        event_sender: &StreamHubEventSender,
        room_id: &str,
        media_id: &str,
        generation_id: Uuid,
        publication_active: &AtomicBool,
    ) {
        if !publication_active.swap(false, Ordering::AcqRel) {
            return;
        }

        let identifier = StreamIdentifier::Rtmp {
            app_name: room_id.to_string(),
            stream_name: media_id.to_string(),
        };
        if let Err(error) = send_event_with_backpressure_timeout_for(
            event_sender,
            StreamHubEvent::UnPublish {
                identifier,
                generation_id,
            },
            STREAMHUB_EVENT_SEND_TIMEOUT,
        )
        .await
        {
            warn!(
                room_id,
                media_id, "Failed to unpublish local relay stream: {error}"
            );
        }
    }

    /// Start the pull stream - connects to publisher via gRPC
    pub async fn start(&self) -> StreamResult<()> {
        // Validate lease_epoch before starting to detect split-brain
        match self
            .registry
            .validate_lease(
                &self.room_id,
                &self.media_id,
                &self.source_generation_id,
                self.lease_epoch,
            )
            .await
        {
            Ok(true) => {
                debug!(
                    "Epoch {} validated for pull stream {}/{}",
                    self.lease_epoch, self.room_id, self.media_id
                );
            }
            Ok(false) => {
                warn!(
                    "Epoch {} is stale for pull stream {}/{}, publisher may have changed. Stopping.",
                    self.lease_epoch,
                    self.room_id,
                    self.media_id
                );
                return Err(crate::error::StreamError::StaleEpoch(format!(
                    "{} / {}",
                    self.room_id, self.media_id
                )));
            }
            Err(e) => {
                // Fail-CLOSED on Redis error to prevent split-brain during
                // network partitions. If we cannot validate the lease_epoch, we cannot
                // confirm that our publisher record is still valid. Optimistic
                // continuation ("fail-open") risks streaming stale data from the wrong
                // publisher node during a network partition scenario.
                // The caller (ExternalPublishManager / PullStreamManager) treats this
                // as a failed start and will retry on the next viewer request.
                error!(
                    "Failed to validate lease_epoch for pull stream {}/{}: {}. \
                     Failing closed to prevent potential split-brain. \
                     Stream will retry when Redis is available.",
                    self.room_id, self.media_id, e
                );
                return Err(crate::error::StreamError::RegistryError(format!(
                    "Epoch validation failed for {}/{}: {e}",
                    self.room_id, self.media_id
                )));
            }
        }

        let capability_probe = GrpcStreamPuller::new(
            self.room_id.clone(),
            self.media_id.clone(),
            self.publisher_address.clone(),
            self.source_generation_id.clone(),
            self.lease_epoch,
            self.connection_pool.clone(),
            false,
        )
        .with_cluster_secret(self.cluster_secret.clone())
        .with_grpc_max_message_size(self.grpc_max_message_size_bytes)
        .with_grpc_compression(self.grpc_compression_enabled);
        let supports_rtp = capability_probe.supports_rtp().await.map_err(|error| {
            crate::error::StreamError::GrpcError(format!(
                "Failed to determine remote RTP capability: {error}"
            ))
        })?;
        let publication = self.publish_to_local_stream_hub(supports_rtp).await?;

        self.lifecycle.set_running();
        self.lifecycle.update_last_active_time();

        let room_id = self.room_id.clone();
        let media_id = self.media_id.clone();
        let publisher_address = self.publisher_address.clone();
        let source_generation_id = self.source_generation_id.clone();
        let hub_sender = self.stream_hub_event_sender.clone();
        let pool = self.connection_pool.clone();
        let cluster_secret = self.cluster_secret.clone();
        let grpc_max_message_size_bytes = self.grpc_max_message_size_bytes;
        let grpc_compression_enabled = self.grpc_compression_enabled;
        let retry_policy = self.retry_policy;
        let lease_epoch = self.lease_epoch;
        let registry = Arc::clone(&self.registry);
        let publication_active = Arc::clone(&self.local_publication_active);
        let local_generation_id = self.local_generation_id;
        // Clone the is_running flag to mark failure in the spawned task
        let is_running_flag = self.lifecycle.is_running_clone();

        let child_token = self.cancel_token.child_token();
        let handle = tokio::spawn(async move {
            info!("gRPC puller task started for {} / {}", room_id, media_id);
            let mut rebuild_count: u32 = 0;
            let mut consecutive_epoch_failures: u32 = 0;
            let result = loop {
                let grpc_puller = GrpcStreamPuller::new(
                    room_id.clone(),
                    media_id.clone(),
                    publisher_address.clone(),
                    source_generation_id.clone(),
                    lease_epoch,
                    pool.clone(),
                    rebuild_count > 0,
                )
                .with_cluster_secret(cluster_secret.clone())
                .with_grpc_max_message_size(grpc_max_message_size_bytes)
                .with_grpc_compression(grpc_compression_enabled);

                // Race the puller against cancellation and periodic lease_epoch re-validation
                let mut epoch_interval =
                    tokio::time::interval(retry_policy.epoch_revalidation_interval);
                // Skip the first immediate tick
                epoch_interval.tick().await;

                let run_result = {
                    let _relay_metrics = synctv_core::metrics::stream::track_relay(
                        synctv_core::metrics::stream::RelayProtocol::Rtmp,
                    );

                    tokio::select! {
                        r = grpc_puller.run(
                            &publication.frame_sender,
                            publication.packet_sender.as_ref(),
                        ) => r,
                        () = child_token.cancelled() => {
                            info!("gRPC puller task cancelled for {} / {}", room_id, media_id);
                            break Ok(());
                        }
                        () = async {
                            loop {
                                epoch_interval.tick().await;
                                match registry
                                    .validate_lease(
                                        &room_id,
                                        &media_id,
                                        &source_generation_id,
                                        lease_epoch,
                                    )
                                    .await
                                {
                                    Ok(true) => {
                                        // Reset failure counter on success.
                                        consecutive_epoch_failures = 0;
                                        debug!(
                                            "Periodic lease_epoch {} still valid for {}/{}",
                                            lease_epoch, room_id, media_id
                                        );
                                    }
                                    Ok(false) => {
                                        warn!(
                                            "Periodic lease_epoch re-validation: lease_epoch {} is stale for {}/{}, publisher changed",
                                            lease_epoch, room_id, media_id
                                        );
                                        return;
                                    }
                                    Err(e) => {
                                        // Track consecutive failures instead of unconditional fail-open.
                                        consecutive_epoch_failures += 1;
                                        if consecutive_epoch_failures >= retry_policy.max_consecutive_epoch_failures {
                                            error!(
                                                "Epoch validation failed {} consecutive times for {}/{}: {}. \
                                                 Terminating pull stream (publisher may be stale). \
                                                 Stream will reconnect when Redis is available.",
                                                consecutive_epoch_failures, room_id, media_id, e
                                            );
                                            return;
                                        }
                                        warn!(
                                            "Periodic lease_epoch re-validation failed for {}/{}: {} ({}/{} consecutive failures). Continuing.",
                                            room_id, media_id, e, consecutive_epoch_failures, retry_policy.max_consecutive_epoch_failures
                                        );
                                    }
                                }
                            }
                        } => {
                            warn!(
                                "Stale lease_epoch detected during streaming for {}/{}; stopping pull stream",
                                room_id, media_id
                            );
                            break Err(anyhow::anyhow!(
                                "Stale lease_epoch detected during streaming: publisher changed for {room_id} / {media_id}"
                            ));
                        }
                    }
                };

                match run_result {
                    Ok(()) => break Ok(()),
                    Err(e) => {
                        let err_str = e.to_string();
                        synctv_core::metrics::stream::record_error(
                            synctv_core::metrics::stream::RelayProtocol::Rtmp,
                            &err_str,
                        );

                        rebuild_count += 1;
                        if rebuild_count > retry_policy.max_rebuilds {
                            error!(
                                "gRPC puller exhausted all retries and {} rebuilds for {} / {}: {}",
                                retry_policy.max_rebuilds, room_id, media_id, e
                            );
                            break Err(e);
                        }

                        warn!(
                            "gRPC puller exited for {} / {}, rebuilding ({}/{}): {}",
                            room_id, media_id, rebuild_count, retry_policy.max_rebuilds, e
                        );

                        // Wait before rebuilding, but respect cancellation
                        tokio::select! {
                            () = tokio::time::sleep(retry_policy.rebuild_delay) => {}
                            () = child_token.cancelled() => {
                                info!("gRPC puller rebuild cancelled for {} / {}", room_id, media_id);
                                break Ok(());
                            }
                        }

                        // Re-validate lease_epoch before reconnecting to detect split-brain
                        // scenarios where the publisher changed during the disruption.
                        match registry
                            .validate_lease(&room_id, &media_id, &source_generation_id, lease_epoch)
                            .await
                        {
                            Ok(true) => {
                                // Reset failure counter on success.
                                consecutive_epoch_failures = 0;
                                debug!(
                                    "Epoch {} still valid on reconnect for {}/{}",
                                    lease_epoch, room_id, media_id
                                );
                            }
                            Ok(false) => {
                                warn!(
                                    "Epoch {} is stale on reconnect for {}/{}, publisher changed. Stopping pull stream.",
                                    lease_epoch, room_id, media_id
                                );
                                break Err(anyhow::anyhow!(
                                    "Stale lease_epoch on reconnect: publisher changed for {room_id} / {media_id}"
                                ));
                            }
                            Err(e) => {
                                // Track consecutive failures instead of unconditional fail-open.
                                consecutive_epoch_failures += 1;
                                if consecutive_epoch_failures
                                    >= retry_policy.max_consecutive_epoch_failures
                                {
                                    error!(
                                        "Epoch validation on reconnect failed {} consecutive times for {}/{}: {}. \
                                         Terminating pull stream (publisher may be stale). \
                                         Stream will reconnect when Redis is available.",
                                        consecutive_epoch_failures, room_id, media_id, e
                                    );
                                    break Err(anyhow::anyhow!(
                                        "Epoch validation unreachable after {consecutive_epoch_failures} consecutive failures for {room_id} / {media_id}"
                                    ));
                                }
                                warn!(
                                    "Failed to validate lease_epoch on reconnect for {}/{}: {} ({}/{} consecutive failures). Continuing.",
                                    room_id, media_id, e, consecutive_epoch_failures, retry_policy.max_consecutive_epoch_failures
                                );
                            }
                        }
                    }
                }
            };

            is_running_flag.store(false, Ordering::Release);
            Self::unpublish_local_stream_hub(
                &hub_sender,
                &room_id,
                &media_id,
                local_generation_id,
                &publication_active,
            )
            .await;
            result
        });

        self.lifecycle.set_task_handle(handle).await;

        info!(
            "Pull stream started for room {} / media {}",
            self.room_id, self.media_id
        );
        Ok(())
    }

    /// Stop the pull stream
    ///
    /// Sends `UnPublish` to the local `StreamHub` BEFORE aborting the puller task,
    /// because the puller's own cleanup path won't run on abort.
    pub async fn stop(&self) -> StreamResult<()> {
        self.lifecycle.mark_stopping();

        // Cancel the puller task gracefully first
        self.cancel_token.cancel();

        Self::unpublish_local_stream_hub(
            &self.stream_hub_event_sender,
            &self.room_id,
            &self.media_id,
            self.local_generation_id,
            &self.local_publication_active,
        )
        .await;

        self.lifecycle.abort_task().await;
        info!(
            "Pull stream stopped for room {} / media {}",
            self.room_id, self.media_id
        );
        Ok(())
    }
}

impl Drop for PullStream {
    fn drop(&mut self) {
        // Cancel the puller task gracefully via token
        self.cancel_token.cancel();

        if !self.local_publication_active.swap(false, Ordering::AcqRel) {
            return;
        }

        // Send UnPublish to StreamHub so the local stream entry is removed.
        let identifier = StreamIdentifier::Rtmp {
            app_name: self.room_id.clone(),
            stream_name: self.media_id.clone(),
        };
        let room_id = self.room_id.clone();
        let media_id = self.media_id.clone();
        debug!("PullStream drop: scheduling UnPublish for {room_id}/{media_id}");
        spawn_event_delivery_with_backpressure_timeout_for(
            self.stream_hub_event_sender.clone(),
            StreamHubEvent::UnPublish {
                identifier,
                generation_id: self.local_generation_id,
            },
            STREAMHUB_EVENT_SEND_TIMEOUT,
        );
        // StreamLifecycle's Drop will abort the task handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::proto::{
        stream_relay_service_server::{StreamRelayService, StreamRelayServiceServer},
        DeleteWebRtcSessionRequest, DeleteWebRtcSessionResponse, FrameType, GetHlsPlaylistRequest,
        GetHlsPlaylistResponse, GetHlsSegmentRequest, GetHlsSegmentResponse, PullRtmpStreamRequest,
        RtmpPacket, RtpPacket,
    };
    use crate::grpc::StreamRelayServiceImpl;
    use crate::relay::TestStreamRegistry;
    use crate::util::TEST_GENERATION_ID;
    use bytes::{Bytes, BytesMut};
    use futures::{stream, Stream, StreamExt as _};
    use std::pin::Pin;
    use synctv_xiu::httpflv::HttpFlvSession;
    use synctv_xiu::streamhub::{
        define::{BroadcastEvent, STREAM_HUB_EVENT_CHANNEL_CAPACITY},
        StreamsHub,
    };
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration, Instant};
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::{Request, Response, Status};

    type TestResult<T = ()> = anyhow::Result<T>;
    type RelayResponseStream =
        Pin<Box<dyn Stream<Item = Result<RtmpPacket, Status>> + Send + 'static>>;
    type RtpRelayResponseStream =
        Pin<Box<dyn Stream<Item = Result<RtpPacket, Status>> + Send + 'static>>;

    #[derive(Clone)]
    struct TestRelayService {
        requests: mpsc::UnboundedSender<bool>,
        packets: Arc<Vec<RtmpPacket>>,
        disconnect: CancellationToken,
    }

    #[tonic::async_trait]
    impl StreamRelayService for TestRelayService {
        type PullRtmpStreamStream = RelayResponseStream;

        async fn pull_rtmp_stream(
            &self,
            request: Request<PullRtmpStreamRequest>,
        ) -> Result<Response<Self::PullRtmpStreamStream>, Status> {
            self.requests
                .send(request.into_inner().is_reconnect)
                .map_err(|_| Status::internal("request observer closed"))?;
            let packets = self.packets.as_ref().clone();
            let disconnect = self.disconnect.clone();
            let response_stream =
                stream::iter(packets.into_iter().map(Ok)).chain(stream::once(async move {
                    disconnect.cancelled().await;
                    Err(Status::unavailable("test relay connection interrupted"))
                }));
            Ok(Response::new(Box::pin(response_stream)))
        }

        type PullRtpStreamStream = RtpRelayResponseStream;

        async fn pull_rtp_stream(
            &self,
            _request: Request<PullRtmpStreamRequest>,
        ) -> Result<Response<Self::PullRtpStreamStream>, Status> {
            Err(Status::failed_precondition(
                "test stream does not provide RTP",
            ))
        }

        async fn get_hls_playlist(
            &self,
            _request: Request<GetHlsPlaylistRequest>,
        ) -> Result<Response<GetHlsPlaylistResponse>, Status> {
            Err(Status::unimplemented("HLS is outside this relay test"))
        }

        async fn get_hls_segment(
            &self,
            _request: Request<GetHlsSegmentRequest>,
        ) -> Result<Response<GetHlsSegmentResponse>, Status> {
            Err(Status::unimplemented("HLS is outside this relay test"))
        }

        async fn delete_web_rtc_session(
            &self,
            _request: Request<DeleteWebRtcSessionRequest>,
        ) -> Result<Response<DeleteWebRtcSessionResponse>, Status> {
            Err(Status::unimplemented("WebRTC is outside this relay test"))
        }
    }

    fn avc_sequence_header() -> Bytes {
        let sps = [
            0x67, 0x42, 0x00, 0x1f, 0x95, 0xa8, 0x14, 0x01, 0x6e, 0x40, 0x00,
        ];
        let pps = [0x68, 0xce, 0x06, 0xe2];
        let mut data = BytesMut::from(&[0x17, 0x00, 0x00, 0x00, 0x00][..]);
        data.extend_from_slice(&[1, sps[1], sps[2], sps[3], 0xff, 0xe1]);
        data.extend_from_slice(
            &u16::try_from(sps.len())
                .expect("test SPS fits in u16")
                .to_be_bytes(),
        );
        data.extend_from_slice(&sps);
        data.extend_from_slice(&[1]);
        data.extend_from_slice(
            &u16::try_from(pps.len())
                .expect("test PPS fits in u16")
                .to_be_bytes(),
        );
        data.extend_from_slice(&pps);
        data.freeze()
    }

    fn video_frame(timestamp: u32) -> Bytes {
        if timestamp == 0 {
            return avc_sequence_header();
        }
        let nal = [0x65, 0x88, 0x84, 0x21];
        let mut data = BytesMut::from(&[0x17, 0x01, 0, 0, 0][..]);
        data.extend_from_slice(
            &u32::try_from(nal.len())
                .expect("test NAL fits in u32")
                .to_be_bytes(),
        );
        data.extend_from_slice(&nal);
        data.freeze()
    }

    fn audio_frame(timestamp: u32) -> Bytes {
        if timestamp == 0 {
            Bytes::from_static(&[0xaf, 0x00, 0x12, 0x10])
        } else {
            Bytes::from_static(&[0xaf, 0x01, 0x21, 0x10])
        }
    }

    fn relay_packets(timestamp: u32) -> Vec<RtmpPacket> {
        let mut packets = vec![
            RtmpPacket {
                data: video_frame(timestamp),
                timestamp,
                frame_type: FrameType::Video as i32,
            },
            RtmpPacket {
                data: audio_frame(timestamp),
                timestamp,
                frame_type: FrameType::Audio as i32,
            },
        ];
        if timestamp == 0 {
            packets.extend([
                RtmpPacket {
                    data: video_frame(100),
                    timestamp: 100,
                    frame_type: FrameType::Video as i32,
                },
                RtmpPacket {
                    data: audio_frame(100),
                    timestamp: 100,
                    frame_type: FrameType::Audio as i32,
                },
            ]);
        }
        packets
    }

    fn spawn_relay_server(
        listener: TcpListener,
        requests: mpsc::UnboundedSender<bool>,
        packets: Vec<RtmpPacket>,
        disconnect: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(StreamRelayServiceServer::new(TestRelayService {
                    requests,
                    packets: Arc::new(packets),
                    disconnect,
                }))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .expect("test relay server should run");
        })
    }

    fn spawn_production_relay_server(
        listener: TcpListener,
        registry: Arc<TestStreamRegistry>,
        event_sender: StreamHubEventSender,
        cancel_token: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let service = StreamRelayServiceImpl::new(
                registry,
                "publisher-node".to_string(),
                event_sender,
                cancel_token.clone(),
            )
            .with_cluster_secret("cluster-secret");
            tonic::transport::Server::builder()
                .add_service(StreamRelayServiceServer::new(service))
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(listener),
                    cancel_token.cancelled_owned(),
                )
                .await
                .expect("production relay test server should run");
        })
    }

    async fn publish_source_stream(
        event_sender: &StreamHubEventSender,
    ) -> TestResult<(Uuid, FrameDataSender)> {
        let generation_id = Uuid::new();
        let (result_sender, result_receiver) = oneshot::channel();
        event_sender
            .send(StreamHubEvent::Publish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "room1".to_string(),
                    stream_name: "media1".to_string(),
                },
                info: PublisherInfo {
                    id: generation_id,
                    pub_type: PublishType::RtmpPush,
                    pub_data_type: synctv_xiu::streamhub::define::PubDataType::Frame,
                    notify_info: NotifyInfo {
                        request_url: "rtmp://publisher/room1/media1".to_string(),
                        remote_addr: "127.0.0.1".to_string(),
                    },
                },
                stream_handler: Arc::new(RtmpStreamHandler::new()),
                result_sender,
            })
            .await?;
        let frame_sender = result_receiver
            .await??
            .0
            .ok_or_else(|| anyhow::anyhow!("source publication returned no frame sender"))?;
        Ok((generation_id, frame_sender))
    }

    async fn send_source_frame(
        sender: &FrameDataSender,
        frame: synctv_xiu::streamhub::define::FrameData,
    ) -> TestResult {
        sender
            .send(frame)
            .await
            .map_err(|error| anyhow::anyhow!("source frame delivery failed: {error:?}"))
    }

    async fn bind_same_address(address: std::net::SocketAddr) -> TestResult<TcpListener> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match TcpListener::bind(address).await {
                Ok(listener) => return Ok(listener),
                Err(error)
                    if error.kind() == std::io::ErrorKind::AddrInUse
                        && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn spawn_flv_session(
        event_sender: StreamHubEventSender,
    ) -> (
        mpsc::Receiver<Result<Bytes, std::io::Error>>,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    ) {
        let (response_tx, response_rx) = mpsc::channel(32);
        let mut session = HttpFlvSession::new(
            "room1".to_string(),
            "media1".to_string(),
            event_sender,
            response_tx,
        );
        let handle = tokio::spawn(async move { session.run().await });
        (response_rx, handle)
    }

    fn flv_tag_timestamp(chunk: &[u8]) -> Option<(u8, u32)> {
        if chunk.len() < 8 || !matches!(chunk[0], 8 | 9) {
            return None;
        }
        let timestamp = u32::from(chunk[4]) << 16
            | u32::from(chunk[5]) << 8
            | u32::from(chunk[6])
            | u32::from(chunk[7]) << 24;
        Some((chunk[0], timestamp))
    }

    async fn expect_flv_header_and_av(
        receiver: &mut mpsc::Receiver<Result<Bytes, std::io::Error>>,
    ) -> TestResult {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut header_seen = false;
        let mut audio_seen = false;
        let mut video_seen = false;
        while Instant::now() < deadline && !(header_seen && audio_seen && video_seen) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let chunk = timeout(remaining, receiver.recv())
                .await
                .map_err(|_| anyhow::anyhow!("timed out waiting for initial HTTP-FLV data"))?
                .ok_or_else(|| anyhow::anyhow!("HTTP-FLV response closed"))??;
            if chunk.starts_with(b"FLV") {
                header_seen = true;
                assert_eq!(chunk.get(4).copied().unwrap_or_default() & 0x05, 0x05);
            } else if let Some((tag_type, _)) = flv_tag_timestamp(&chunk) {
                audio_seen |= tag_type == 8;
                video_seen |= tag_type == 9;
            }
        }
        anyhow::ensure!(header_seen, "HTTP-FLV header missing");
        anyhow::ensure!(audio_seen, "HTTP-FLV audio tag missing");
        anyhow::ensure!(video_seen, "HTTP-FLV video tag missing");
        Ok(())
    }

    async fn expect_reconnected_av(
        receiver: &mut mpsc::Receiver<Result<Bytes, std::io::Error>>,
        minimum_timestamp: u32,
    ) -> TestResult {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut audio_seen = false;
        let mut video_seen = false;
        while Instant::now() < deadline && !(audio_seen && video_seen) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let chunk = timeout(remaining, receiver.recv())
                .await
                .map_err(|_| anyhow::anyhow!("timed out waiting for reconnected HTTP-FLV data"))?
                .ok_or_else(|| anyhow::anyhow!("HTTP-FLV response closed during reconnect"))??;
            if let Some((tag_type, timestamp)) = flv_tag_timestamp(&chunk) {
                if timestamp >= minimum_timestamp {
                    audio_seen |= tag_type == 8;
                    video_seen |= tag_type == 9;
                }
            }
        }
        anyhow::ensure!(audio_seen, "reconnected HTTP-FLV audio tag missing");
        anyhow::ensure!(video_seen, "reconnected HTTP-FLV video tag missing");
        Ok(())
    }

    async fn next_broadcast(
        receiver: &mut tokio::sync::broadcast::Receiver<BroadcastEvent>,
    ) -> TestResult<BroadcastEvent> {
        Ok(timeout(Duration::from_secs(2), receiver.recv())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for StreamHub broadcast"))??)
    }

    #[tokio::test]
    async fn tonic_reconnect_keeps_one_local_publication_and_two_flv_subscribers() -> TestResult {
        let first_listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = first_listener.local_addr()?;
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let first_disconnect = CancellationToken::new();
        let first_server = spawn_relay_server(
            first_listener,
            request_tx.clone(),
            relay_packets(0),
            first_disconnect.clone(),
        );

        let registry = Arc::new(TestStreamRegistry::new());
        anyhow::ensure!(
            registry
                .try_activate_generation(
                    "room1",
                    "media1",
                    "publisher-node",
                    "user1",
                    &address.to_string(),
                    TEST_GENERATION_ID,
                )
                .await?,
            "test publisher registration failed"
        );

        let (event_tx, event_rx) = mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY);
        let mut hub = StreamsHub::new(event_tx.clone(), event_rx);
        let mut broadcast_rx = hub.get_client_event_consumer();
        let hub_task = tokio::spawn(async move {
            let _ = hub.run().await;
        });

        let pull_stream = PullStream::with_pool(
            PullStreamRoute::new(
                "room1".to_string(),
                "media1".to_string(),
                address.to_string(),
                TEST_GENERATION_ID.to_string(),
                1,
            ),
            registry,
            event_tx.clone(),
            GrpcConnectionPool::with_defaults(),
        )
        .with_cluster_secret(Some("cluster-secret".to_string()))
        .with_grpc_compression(false)
        .with_retry_policy(RelayRetryPolicy {
            max_rebuilds: 3,
            rebuild_delay: Duration::from_millis(50),
            epoch_revalidation_interval: Duration::from_secs(30),
            max_consecutive_epoch_failures: 3,
        });
        pull_stream.start().await?;

        let publish_event = next_broadcast(&mut broadcast_rx).await?;
        assert!(matches!(publish_event, BroadcastEvent::Publish { .. }));
        assert!(!timeout(Duration::from_secs(2), request_rx.recv())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for initial relay request"))?
            .ok_or_else(|| anyhow::anyhow!("initial relay request missing"))?);

        let (mut first_flv, first_flv_task) = spawn_flv_session(event_tx.clone());
        let (mut second_flv, second_flv_task) = spawn_flv_session(event_tx.clone());
        expect_flv_header_and_av(&mut first_flv).await?;
        expect_flv_header_and_av(&mut second_flv).await?;

        first_server.abort();
        let _ = first_server.await;
        let second_listener = bind_same_address(address).await?;
        let second_server = spawn_relay_server(
            second_listener,
            request_tx,
            relay_packets(2_000),
            CancellationToken::new(),
        );
        first_disconnect.cancel();

        assert!(
            timeout(Duration::from_secs(2), request_rx.recv())
                .await
                .map_err(|_| anyhow::anyhow!("timed out waiting for reconnect relay request"))?
                .ok_or_else(|| anyhow::anyhow!("reconnect relay request missing"))?,
            "second relay request must be marked as reconnect"
        );
        expect_reconnected_av(&mut first_flv, 2_000).await?;
        expect_reconnected_av(&mut second_flv, 2_000).await?;
        assert!(
            broadcast_rx.try_recv().is_err(),
            "remote reconnect must keep the existing local publication"
        );

        pull_stream.stop().await?;
        let unpublish_event = next_broadcast(&mut broadcast_rx).await?;
        assert!(matches!(unpublish_event, BroadcastEvent::UnPublish { .. }));

        timeout(Duration::from_secs(2), first_flv_task).await???;
        timeout(Duration::from_secs(2), second_flv_task).await???;
        second_server.abort();
        let _ = second_server.await;
        hub_task.abort();
        let _ = hub_task.await;
        Ok(())
    }

    #[tokio::test]
    async fn production_relay_bridges_source_streamhub_to_two_destination_flv_clients() -> TestResult
    {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let registry = Arc::new(TestStreamRegistry::new());

        let (source_event_tx, source_event_rx) = mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY);
        let mut source_hub = StreamsHub::new(source_event_tx.clone(), source_event_rx);
        let source_hub_task = tokio::spawn(async move {
            let _ = source_hub.run().await;
        });
        let (source_generation_id, source_frames) = publish_source_stream(&source_event_tx).await?;
        anyhow::ensure!(
            registry
                .try_activate_generation(
                    "room1",
                    "media1",
                    "publisher-node",
                    "user1",
                    &address.to_string(),
                    &source_generation_id.to_string(),
                )
                .await?,
            "test publisher registration failed"
        );

        let relay_cancel = CancellationToken::new();
        let relay_task = spawn_production_relay_server(
            listener,
            Arc::clone(&registry),
            source_event_tx.clone(),
            relay_cancel.clone(),
        );

        let (destination_event_tx, destination_event_rx) =
            mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY);
        let mut destination_hub =
            StreamsHub::new(destination_event_tx.clone(), destination_event_rx);
        let mut destination_broadcast = destination_hub.get_client_event_consumer();
        let destination_hub_task = tokio::spawn(async move {
            let _ = destination_hub.run().await;
        });

        let pull_stream = PullStream::with_pool(
            PullStreamRoute::new(
                "room1".to_string(),
                "media1".to_string(),
                address.to_string(),
                source_generation_id.to_string(),
                1,
            ),
            registry,
            destination_event_tx.clone(),
            GrpcConnectionPool::with_defaults(),
        )
        .with_cluster_secret(Some("cluster-secret".to_string()))
        .with_grpc_compression(false)
        .with_retry_policy(RelayRetryPolicy {
            max_rebuilds: 1,
            rebuild_delay: Duration::from_millis(50),
            epoch_revalidation_interval: Duration::from_secs(30),
            max_consecutive_epoch_failures: 1,
        });
        pull_stream.start().await?;
        assert!(matches!(
            next_broadcast(&mut destination_broadcast).await?,
            BroadcastEvent::Publish { .. }
        ));

        send_source_frame(
            &source_frames,
            synctv_xiu::streamhub::define::FrameData::Video {
                timestamp: 0,
                data: avc_sequence_header(),
            },
        )
        .await?;
        send_source_frame(
            &source_frames,
            synctv_xiu::streamhub::define::FrameData::Audio {
                timestamp: 0,
                data: audio_frame(0),
            },
        )
        .await?;
        send_source_frame(
            &source_frames,
            synctv_xiu::streamhub::define::FrameData::Video {
                timestamp: 100,
                data: video_frame(100),
            },
        )
        .await?;
        send_source_frame(
            &source_frames,
            synctv_xiu::streamhub::define::FrameData::Audio {
                timestamp: 100,
                data: audio_frame(100),
            },
        )
        .await?;

        let (mut first_flv, first_flv_task) = spawn_flv_session(destination_event_tx.clone());
        let (mut second_flv, second_flv_task) = spawn_flv_session(destination_event_tx.clone());
        expect_flv_header_and_av(&mut first_flv).await?;
        expect_flv_header_and_av(&mut second_flv).await?;

        pull_stream.stop().await?;
        assert!(matches!(
            next_broadcast(&mut destination_broadcast).await?,
            BroadcastEvent::UnPublish { .. }
        ));
        timeout(Duration::from_secs(2), first_flv_task).await???;
        timeout(Duration::from_secs(2), second_flv_task).await???;

        source_event_tx
            .send(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "room1".to_string(),
                    stream_name: "media1".to_string(),
                },
                generation_id: source_generation_id,
            })
            .await?;
        relay_cancel.cancel();
        timeout(Duration::from_secs(2), relay_task).await??;
        source_hub_task.abort();
        destination_hub_task.abort();
        let _ = source_hub_task.await;
        let _ = destination_hub_task.await;
        Ok(())
    }

    #[tokio::test]
    async fn publisher_lease_change_fences_running_pull_and_unpublishes_once() -> TestResult {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (request_tx, _request_rx) = mpsc::unbounded_channel();
        let relay_server = spawn_relay_server(
            listener,
            request_tx,
            relay_packets(0),
            CancellationToken::new(),
        );
        let registry = Arc::new(TestStreamRegistry::new());
        anyhow::ensure!(
            registry
                .try_activate_generation(
                    "room1",
                    "media1",
                    "publisher-node",
                    "user1",
                    &address.to_string(),
                    TEST_GENERATION_ID,
                )
                .await?,
            "initial publisher registration failed"
        );

        let (event_tx, event_rx) = mpsc::channel(STREAM_HUB_EVENT_CHANNEL_CAPACITY);
        let mut hub = StreamsHub::new(event_tx.clone(), event_rx);
        let mut broadcast_rx = hub.get_client_event_consumer();
        let hub_task = tokio::spawn(async move {
            let _ = hub.run().await;
        });
        let pull_stream = PullStream::with_pool(
            PullStreamRoute::new(
                "room1".to_string(),
                "media1".to_string(),
                address.to_string(),
                TEST_GENERATION_ID.to_string(),
                1,
            ),
            Arc::clone(&registry) as Arc<dyn StreamRegistryTrait>,
            event_tx,
            GrpcConnectionPool::with_defaults(),
        )
        .with_cluster_secret(Some("cluster-secret".to_string()))
        .with_grpc_compression(false)
        .with_retry_policy(RelayRetryPolicy {
            max_rebuilds: 1,
            rebuild_delay: Duration::from_millis(20),
            epoch_revalidation_interval: Duration::from_millis(50),
            max_consecutive_epoch_failures: 1,
        });
        pull_stream.start().await?;
        assert!(matches!(
            next_broadcast(&mut broadcast_rx).await?,
            BroadcastEvent::Publish { .. }
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let unexpected = broadcast_rx.try_recv();
        assert!(
            unexpected.is_err(),
            "pull stream must remain published while its lease_epoch is current; got {unexpected:?}"
        );

        registry
            .deactivate_current_generation("room1", "media1")
            .await?;
        anyhow::ensure!(
            registry
                .try_activate_generation(
                    "room1",
                    "media1",
                    "replacement-node",
                    "user2",
                    "127.0.0.1:50052",
                    "00000000-0000-4000-8000-000000000002",
                )
                .await?,
            "replacement publisher registration failed"
        );
        assert!(matches!(
            next_broadcast(&mut broadcast_rx).await?,
            BroadcastEvent::UnPublish { .. }
        ));

        pull_stream.stop().await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            broadcast_rx.try_recv().is_err(),
            "explicit stop after lease_epoch fencing must not emit a second unpublish"
        );

        relay_server.abort();
        let _ = relay_server.await;
        hub_task.abort();
        let _ = hub_task.await;
        Ok(())
    }

    #[tokio::test]
    async fn local_publish_waits_for_temporarily_full_streamhub_queue() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        registry
            .try_activate_generation(
                "room1",
                "media1",
                "publisher-node",
                "user1",
                "127.0.0.1:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        let (event_tx, mut event_rx) = mpsc::channel(1);
        event_tx
            .send(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "occupied".to_string(),
                    stream_name: "occupied".to_string(),
                },
                generation_id: Uuid::new(),
            })
            .await?;

        let responder = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            event_rx.recv().await;
            let Some(StreamHubEvent::Publish { result_sender, .. }) = event_rx.recv().await else {
                anyhow::bail!("expected local relay publish event");
            };
            let (frame_tx, _frame_rx) = mpsc::channel(4);
            result_sender
                .send(Ok((Some(FrameDataSender::bounded(frame_tx)), None, None)))
                .map_err(|_| anyhow::anyhow!("publish result receiver closed"))?;
            Ok::<(), anyhow::Error>(())
        });

        let pull_stream = PullStream::with_pool(
            PullStreamRoute::new(
                "room1".to_string(),
                "media1".to_string(),
                "127.0.0.1:50051".to_string(),
                TEST_GENERATION_ID.to_string(),
                1,
            ),
            registry,
            event_tx,
            GrpcConnectionPool::with_defaults(),
        );
        timeout(
            Duration::from_secs(2),
            pull_stream.publish_to_local_stream_hub(false),
        )
        .await??;
        responder.await??;
        Ok(())
    }
}
