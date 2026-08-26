use std::pin::Pin;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use synctv_xiu::hls::StreamRegistry as HlsStreamRegistry;
use synctv_xiu::streamhub::{
    define::{
        NotifyInfo, PacketData, StreamHubEvent, StreamHubEventSender, SubDataType, SubscribeType,
        SubscriberInfo,
    },
    errors::StreamHubErrorValue,
    send_event_with_backpressure_timeout, spawn_event_delivery_with_backpressure_timeout,
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use super::proto::{
    stream_relay_service_server, DeleteWebRtcSessionRequest, DeleteWebRtcSessionResponse,
    FrameType, GetHlsPlaylistRequest, GetHlsPlaylistResponse, GetHlsSegmentRequest,
    GetHlsSegmentResponse, PullRtmpStreamRequest, RtmpPacket, RtpPacket, RtpPacketType,
    WebRtcSessionKind as ProtoWebRtcSessionKind,
};
use crate::error::StreamError;
use crate::livestream::external_publish_manager::ExternalPublishManager;
use crate::livestream::webrtc_session_manager::WebRtcSessionManager;
use crate::livestream::SegmentManager;
use crate::relay::StreamRegistryTrait;
use crate::util::{
    validate_hls_segment_name, validate_hls_segment_url_base, validate_hls_segment_url_suffix,
    validate_stream_generation_id, validate_stream_ids,
};

type ResponseStream = Pin<Box<dyn Stream<Item = Result<RtmpPacket, Status>> + Send>>;
type RtpResponseStream = Pin<Box<dyn Stream<Item = Result<RtpPacket, Status>> + Send>>;

/// Metadata key for cluster authentication shared secret
const AUTH_SECRET_METADATA_KEY: &str = "x-cluster-secret";
const STREAM_HUB_SUBSCRIBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

struct RelaySubscriptionGuard {
    event_sender: StreamHubEventSender,
    subscriber_id: Uuid,
    room_id: String,
    media_id: String,
    sub_type: SubscribeType,
    sub_data_type: SubDataType,
    active: bool,
}

impl Drop for RelaySubscriptionGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        let sub_info = SubscriberInfo {
            id: self.subscriber_id,
            sub_type: self.sub_type.clone(),
            sub_data_type: self.sub_data_type,
            notify_info: NotifyInfo {
                request_url: String::new(),
                remote_addr: String::new(),
            },
        };
        let event = StreamHubEvent::UnSubscribe {
            identifier: StreamIdentifier::Rtmp {
                app_name: self.room_id.clone(),
                stream_name: self.media_id.clone(),
            },
            info: sub_info,
        };
        spawn_event_delivery_with_backpressure_timeout(self.event_sender.clone(), event);
    }
}

fn map_streamhub_enqueue_error(error: synctv_xiu::streamhub::errors::StreamHubError) -> Status {
    match error.value {
        StreamHubErrorValue::EventSendTimeout => {
            Status::resource_exhausted("StreamHub subscribe queue is saturated")
        }
        StreamHubErrorValue::EventChannelClosed | StreamHubErrorValue::SendError => {
            Status::unavailable("StreamHub subscribe queue is unavailable")
        }
        other => {
            tracing::error!("failed to enqueue StreamHub subscribe event: {other:?}");
            Status::internal("Failed to enqueue subscribe event")
        }
    }
}

fn map_webrtc_session_error(error: StreamError) -> Status {
    match error {
        StreamError::InvalidInput(message) => Status::invalid_argument(message),
        StreamError::PermissionDenied(message) => Status::permission_denied(message),
        StreamError::InvalidState(message) => Status::failed_precondition(message),
        StreamError::ResourceExhausted(message) => Status::resource_exhausted(message),
        StreamError::RegistryError(message) => Status::unavailable(message),
        other => Status::internal(other.to_string()),
    }
}

fn validate_relay_stream_ids(room_id: &str, media_id: &str) -> Result<(), Status> {
    validate_stream_ids(room_id, media_id)
        .map_err(|error| Status::invalid_argument(format!("invalid stream identifiers: {error}")))
}

fn validate_relay_hls_segment_name(segment_name: &str) -> Result<(), Status> {
    validate_hls_segment_name(segment_name)
        .map_err(|error| Status::invalid_argument(format!("invalid HLS segment name: {error}")))
}

fn validate_relay_generation_id(generation_id: &str) -> Result<(), Status> {
    validate_stream_generation_id(generation_id)
        .map_err(|error| Status::invalid_argument(format!("invalid generation_id: {error}")))
}

fn validate_relay_segment_url_base(segment_url_base: &str) -> Result<(), Status> {
    validate_hls_segment_url_base(segment_url_base)
        .map_err(|error| Status::invalid_argument(format!("invalid HLS segment URL base: {error}")))
}

fn validate_relay_segment_url_suffix(segment_url_suffix: &str) -> Result<(), Status> {
    validate_hls_segment_url_suffix(segment_url_suffix).map_err(|error| {
        Status::invalid_argument(format!("invalid HLS segment URL suffix: {error}"))
    })
}

fn require_expected_lease_epoch(expected_lease_epoch: u64) -> Result<(), Status> {
    if expected_lease_epoch == 0 {
        return Err(Status::invalid_argument(
            "expected_lease_epoch is required for stream relay fencing",
        ));
    }
    Ok(())
}

async fn forward_rtmp_packets(
    mut frame_receiver: synctv_xiu::streamhub::define::FrameDataReceiver,
    tx: mpsc::Sender<Result<RtmpPacket, Status>>,
    cancel_token: CancellationToken,
) {
    info!("Streaming live data to puller");

    loop {
        let frame_data = tokio::select! {
            () = cancel_token.cancelled() => {
                info!("Relay forwarding task cancelled (shutdown)");
                break;
            }
            result = frame_receiver.recv() => {
                match result {
                    Some(data) => data,
                    None => break,
                }
            }
            () = tx.closed() => {
                info!("Relay client closed the live-frame stream");
                break;
            }
        };

        let (data, timestamp, frame_type) = match frame_data {
            synctv_xiu::streamhub::define::FrameData::Video { data, timestamp } => {
                (data, timestamp, FrameType::Video as i32)
            }
            synctv_xiu::streamhub::define::FrameData::Audio { data, timestamp } => {
                (data, timestamp, FrameType::Audio as i32)
            }
            synctv_xiu::streamhub::define::FrameData::MetaData { data, timestamp } => {
                (data, timestamp, FrameType::Metadata as i32)
            }
            _ => continue,
        };

        let packet = RtmpPacket {
            data,
            timestamp,
            frame_type,
        };

        let send_result = tokio::select! {
            () = cancel_token.cancelled() => {
                info!("Relay forwarding task cancelled while waiting on client backpressure");
                break;
            }
            result = tx.send(Ok(packet)) => result,
        };

        if send_result.is_err() {
            warn!("Client disconnected during live streaming");
            break;
        }
    }
}

async fn forward_rtp_packets(
    mut packet_receiver: synctv_xiu::streamhub::define::PacketDataReceiver,
    tx: mpsc::Sender<Result<RtpPacket, Status>>,
    cancel_token: CancellationToken,
) {
    info!("Streaming RTP data to puller");

    loop {
        let packet_data = tokio::select! {
            () = cancel_token.cancelled() => break,
            () = tx.closed() => break,
            result = packet_receiver.recv() => {
                match result {
                    Some(data) => data,
                    None => break,
                }
            }
        };
        let packet = match packet_data {
            PacketData::Video { timestamp, data } => RtpPacket {
                data,
                timestamp,
                packet_type: RtpPacketType::Video as i32,
            },
            PacketData::Audio { timestamp, data } => RtpPacket {
                data,
                timestamp,
                packet_type: RtpPacketType::Audio as i32,
            },
        };
        let send_result = tokio::select! {
            () = cancel_token.cancelled() => break,
            result = tx.send(Ok(packet)) => result,
        };
        if send_result.is_err() {
            break;
        }
    }
}

/// `StreamRelayService` implementation
/// Publisher nodes use this to serve RTMP packets to Puller nodes via subscription
/// and HLS playlists/segments to non-publisher nodes via proxy.
///
/// GOP cache is handled by xiu's `StreamHub` internally — when a new subscriber
/// joins, `StreamHub` automatically sends cached GOP frames via `send_prior_data`.
pub struct StreamRelayServiceImpl {
    registry: Arc<dyn StreamRegistryTrait>,
    node_id: String,
    stream_hub_event_sender: StreamHubEventSender,
    /// Shared secret for cluster authentication (constant-time comparison)
    cluster_secret: Option<Vec<u8>>,
    /// Cancellation token for graceful shutdown of forwarding tasks
    cancel_token: CancellationToken,
    /// HLS segment manager for reading TS segments (optional, only on HLS-enabled nodes)
    segment_manager: Option<Arc<SegmentManager>>,
    /// HLS stream registry for M3U8 generation (optional, only on HLS-enabled nodes)
    hls_stream_registry: Option<HlsStreamRegistry>,
    external_publish_manager: Option<Arc<ExternalPublishManager>>,
    webrtc_session_manager: Option<Arc<WebRtcSessionManager>>,
}

impl StreamRelayServiceImpl {
    #[must_use]
    pub(crate) fn new(
        registry: Arc<dyn StreamRegistryTrait>,
        node_id: String,
        stream_hub_event_sender: StreamHubEventSender,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            registry,
            node_id,
            stream_hub_event_sender,
            cluster_secret: None,
            cancel_token,
            segment_manager: None,
            hls_stream_registry: None,
            external_publish_manager: None,
            webrtc_session_manager: None,
        }
    }

    /// Set the cluster authentication secret.
    /// When set, all incoming requests must include this secret in metadata.
    #[must_use]
    pub(crate) fn with_cluster_secret(mut self, secret: impl Into<Vec<u8>>) -> Self {
        self.cluster_secret = Some(secret.into());
        self
    }

    /// Set the HLS segment manager for serving TS segments via gRPC proxy.
    #[must_use]
    pub(crate) fn with_segment_manager(mut self, segment_manager: Arc<SegmentManager>) -> Self {
        self.segment_manager = Some(segment_manager);
        self
    }

    /// Set the HLS stream registry for generating M3U8 playlists via gRPC proxy.
    #[must_use]
    pub(crate) fn with_hls_stream_registry(
        mut self,
        hls_stream_registry: HlsStreamRegistry,
    ) -> Self {
        self.hls_stream_registry = Some(hls_stream_registry);
        self
    }

    #[must_use]
    pub(crate) fn with_external_publish_manager(
        mut self,
        external_publish_manager: Arc<ExternalPublishManager>,
    ) -> Self {
        self.external_publish_manager = Some(external_publish_manager);
        self
    }

    #[must_use]
    pub(crate) fn with_webrtc_session_manager(
        mut self,
        manager: Arc<WebRtcSessionManager>,
    ) -> Self {
        self.webrtc_session_manager = Some(manager);
        self
    }

    /// Authenticate a gRPC request using the cluster shared secret.
    /// Uses constant-time comparison to prevent timing attacks.
    #[allow(clippy::result_large_err)]
    fn authenticate<T>(&self, request: &Request<T>) -> Result<(), Status> {
        let Some(expected) = &self.cluster_secret else {
            return Err(Status::unauthenticated(
                "cluster authentication secret is not configured",
            ));
        };

        let provided = request
            .metadata()
            .get(AUTH_SECRET_METADATA_KEY)
            .ok_or_else(|| Status::unauthenticated("missing cluster authentication secret"))?
            .as_bytes();

        if expected.ct_eq(provided).into() {
            Ok(())
        } else {
            Err(Status::unauthenticated(
                "invalid cluster authentication secret",
            ))
        }
    }

    async fn verify_local_active_generation(
        &self,
        room_id: &str,
        media_id: &str,
        expected_generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<(), Status> {
        require_expected_lease_epoch(expected_lease_epoch)?;

        let publisher_info = self
            .registry
            .get_active_generation(room_id, media_id)
            .await
            .map_err(|error| {
                tracing::error!(%error, room_id, media_id, "Failed to get publisher");
                Status::internal("Failed to get publisher info")
            })?
            .ok_or_else(|| Status::not_found("No active publisher for this media"))?;

        self.verify_local_route(
            room_id,
            media_id,
            expected_generation_id,
            expected_lease_epoch,
            &publisher_info,
        )
    }

    async fn verify_local_hls_generation(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<crate::relay::StreamGeneration, Status> {
        require_expected_lease_epoch(expected_lease_epoch)?;

        let publisher_info = self
            .registry
            .get_generation(room_id, media_id, generation_id)
            .await
            .map_err(|error| {
                tracing::error!(%error, room_id, media_id, "Failed to get HLS route");
                Status::internal("Failed to get HLS route")
            })?
            .ok_or_else(|| Status::not_found("No active or ended HLS route for this media"))?;

        self.verify_local_route(
            room_id,
            media_id,
            generation_id,
            expected_lease_epoch,
            &publisher_info,
        )?;
        Ok(publisher_info)
    }

    fn verify_local_route(
        &self,
        room_id: &str,
        media_id: &str,
        expected_generation_id: &str,
        expected_lease_epoch: u64,
        publisher_info: &crate::relay::StreamGeneration,
    ) -> Result<(), Status> {
        if publisher_info.node_id != self.node_id {
            return Err(Status::failed_precondition(format!(
                "This node ({}) is not the publisher (publisher is {})",
                self.node_id, publisher_info.node_id
            )));
        }

        if publisher_info.generation_id != expected_generation_id {
            return Err(Status::failed_precondition(format!(
                "Publisher generation mismatch for {room_id}/{media_id}: expected {expected_generation_id}, current {}",
                publisher_info.generation_id
            )));
        }

        if publisher_info.lease_epoch != expected_lease_epoch {
            return Err(Status::failed_precondition(format!(
                "Publisher lease_epoch mismatch for {room_id}/{media_id}: expected {}, current {}",
                expected_lease_epoch, publisher_info.lease_epoch
            )));
        }

        Ok(())
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)] // tonic::Status is inherently large; required by gRPC trait
impl stream_relay_service_server::StreamRelayService for StreamRelayServiceImpl {
    /// Pull RTMP stream from publisher node (server streaming)
    /// Subscribe to `StreamHub` and forward data — GOP is sent automatically by `StreamHub`.
    type PullRtmpStreamStream = ResponseStream;

    async fn pull_rtmp_stream(
        &self,
        request: Request<PullRtmpStreamRequest>,
    ) -> Result<Response<Self::PullRtmpStreamStream>, Status> {
        // Authenticate the request using the cluster shared secret.
        self.authenticate(&request)?;

        let req = request.into_inner();
        validate_relay_stream_ids(&req.room_id, &req.media_id)?;
        validate_relay_generation_id(&req.generation_id)?;
        self.verify_local_active_generation(
            &req.room_id,
            &req.media_id,
            &req.generation_id,
            req.expected_lease_epoch,
        )
        .await?;
        info!(
            room_id = req.room_id,
            media_id = req.media_id,
            generation_id = req.generation_id,
            is_reconnect = req.is_reconnect,
            expected_lease_epoch = req.expected_lease_epoch,
            "PullRtmpStream request (service-to-service internal call)"
        );

        // Subscribe to StreamHub for live data (GOP is sent automatically by StreamHub)
        let subscriber_id = Uuid::new();
        let sub_info = SubscriberInfo {
            id: subscriber_id,
            sub_type: SubscribeType::RtmpPull,
            sub_data_type: synctv_xiu::streamhub::define::SubDataType::Frame,
            notify_info: NotifyInfo {
                request_url: String::new(),
                remote_addr: String::new(),
            },
        };

        // Use canonical (room_id, media_id) format matching RTMP publish identifier
        let identifier = StreamIdentifier::Rtmp {
            app_name: req.room_id.clone(),
            stream_name: req.media_id.clone(),
        };

        let (event_result_sender, event_result_receiver) = tokio::sync::oneshot::channel();
        let subscribe_event = StreamHubEvent::SubscribeWithGeneration {
            identifier,
            info: sub_info,
            expected_generation_id: Uuid::parse_str(&req.generation_id)
                .map_err(|_| Status::invalid_argument("generation_id must be a UUID"))?,
            result_sender: event_result_sender,
        };

        // Send subscribe event (mpsc::Sender is Clone + Send + Sync, no Mutex needed)
        send_event_with_backpressure_timeout(&self.stream_hub_event_sender, subscribe_event)
            .await
            .map_err(map_streamhub_enqueue_error)?;

        // Wait for subscription result
        let subscribe_result =
            tokio::time::timeout(STREAM_HUB_SUBSCRIBE_TIMEOUT, event_result_receiver)
                .await
                .map_err(|_| {
                    Status::deadline_exceeded(format!(
                        "Timed out waiting {}s for StreamHub subscription",
                        STREAM_HUB_SUBSCRIBE_TIMEOUT.as_secs()
                    ))
                })?
                .map_err(|_| Status::internal("Subscribe result channel closed"))?
                .map_err(|e| {
                    tracing::error!("Subscribe failed: {e}");
                    Status::internal("Stream subscription failed")
                })?;

        // The RPC may be cancelled or fail any of the remaining setup checks after
        // StreamHub has accepted the subscription. Keep an async drop guard armed
        // until the forwarding task owns cleanup.
        let mut subscription_guard = RelaySubscriptionGuard {
            event_sender: self.stream_hub_event_sender.clone(),
            subscriber_id,
            room_id: req.room_id.clone(),
            media_id: req.media_id.clone(),
            sub_type: SubscribeType::RtmpPull,
            sub_data_type: SubDataType::Frame,
            active: true,
        };

        // The registry can change while StreamHub processes the event. A
        // second lease check fences a subscriber that was accepted just
        // before unpublish/republish completed on the publisher node.
        if let Err(status) = self
            .verify_local_active_generation(
                &req.room_id,
                &req.media_id,
                &req.generation_id,
                req.expected_lease_epoch,
            )
            .await
        {
            Self::unsubscribe_from_hub(
                self.stream_hub_event_sender.clone(),
                subscriber_id,
                req.room_id.clone(),
                req.media_id.clone(),
                SubscribeType::RtmpPull,
                SubDataType::Frame,
            )
            .await;
            return Err(status);
        }

        let frame_receiver = subscribe_result
            .0
            .frame_receiver
            .ok_or_else(|| Status::internal("No frame receiver from subscription"))?;

        // Create a channel for streaming packets
        let (tx, rx) = mpsc::channel(128);

        // Spawn task to forward frames with cancellation support
        let room_id_clone = req.room_id.clone();
        let media_id_clone = req.media_id.clone();
        let event_sender_clone = self.stream_hub_event_sender.clone();
        let child_token = self.cancel_token.child_token();
        tokio::spawn(async move {
            forward_rtmp_packets(frame_receiver, tx, child_token).await;

            info!("Stream ended, unsubscribing");
            Self::unsubscribe_from_hub(
                event_sender_clone,
                subscriber_id,
                room_id_clone,
                media_id_clone,
                SubscribeType::RtmpPull,
                SubDataType::Frame,
            )
            .await;
        });

        subscription_guard.active = false;

        let output_stream = ReceiverStream::new(rx);
        Ok(Response::new(
            Box::pin(output_stream) as Self::PullRtmpStreamStream
        ))
    }

    type PullRtpStreamStream = RtpResponseStream;

    async fn pull_rtp_stream(
        &self,
        request: Request<PullRtmpStreamRequest>,
    ) -> Result<Response<Self::PullRtpStreamStream>, Status> {
        self.authenticate(&request)?;

        let req = request.into_inner();
        validate_relay_stream_ids(&req.room_id, &req.media_id)?;
        validate_relay_generation_id(&req.generation_id)?;
        self.verify_local_active_generation(
            &req.room_id,
            &req.media_id,
            &req.generation_id,
            req.expected_lease_epoch,
        )
        .await?;

        let subscriber_id = Uuid::new();
        let sub_info = SubscriberInfo {
            id: subscriber_id,
            sub_type: SubscribeType::WhepPull,
            sub_data_type: SubDataType::Packet,
            notify_info: NotifyInfo {
                request_url: String::new(),
                remote_addr: String::new(),
            },
        };
        let identifier = StreamIdentifier::Rtmp {
            app_name: req.room_id.clone(),
            stream_name: req.media_id.clone(),
        };
        let (event_result_sender, event_result_receiver) = tokio::sync::oneshot::channel();
        send_event_with_backpressure_timeout(
            &self.stream_hub_event_sender,
            StreamHubEvent::SubscribeWithGeneration {
                identifier,
                info: sub_info,
                expected_generation_id: Uuid::parse_str(&req.generation_id)
                    .map_err(|_| Status::invalid_argument("generation_id must be a UUID"))?,
                result_sender: event_result_sender,
            },
        )
        .await
        .map_err(map_streamhub_enqueue_error)?;

        let subscribe_result =
            tokio::time::timeout(STREAM_HUB_SUBSCRIBE_TIMEOUT, event_result_receiver)
                .await
                .map_err(|_| Status::deadline_exceeded("Timed out waiting for RTP subscription"))?
                .map_err(|_| Status::internal("RTP subscribe result channel closed"))?
                .map_err(|error| match error.value {
                    StreamHubErrorValue::NotCorrectDataSenderType => {
                        Status::failed_precondition("Active stream does not provide RTP packets")
                    }
                    other => {
                        tracing::error!("RTP subscribe failed: {other:?}");
                        Status::internal("RTP stream subscription failed")
                    }
                })?;

        let mut subscription_guard = RelaySubscriptionGuard {
            event_sender: self.stream_hub_event_sender.clone(),
            subscriber_id,
            room_id: req.room_id.clone(),
            media_id: req.media_id.clone(),
            sub_type: SubscribeType::WhepPull,
            sub_data_type: SubDataType::Packet,
            active: true,
        };

        if let Err(status) = self
            .verify_local_active_generation(
                &req.room_id,
                &req.media_id,
                &req.generation_id,
                req.expected_lease_epoch,
            )
            .await
        {
            Self::unsubscribe_from_hub(
                self.stream_hub_event_sender.clone(),
                subscriber_id,
                req.room_id.clone(),
                req.media_id.clone(),
                SubscribeType::WhepPull,
                SubDataType::Packet,
            )
            .await;
            subscription_guard.active = false;
            return Err(status);
        }

        let packet_receiver = subscribe_result
            .0
            .packet_receiver
            .ok_or_else(|| Status::internal("RTP subscription returned no packet receiver"))?;
        let (tx, rx) = mpsc::channel(256);
        let room_id = req.room_id;
        let media_id = req.media_id;
        let event_sender = self.stream_hub_event_sender.clone();
        let child_token = self.cancel_token.child_token();
        tokio::spawn(async move {
            forward_rtp_packets(packet_receiver, tx, child_token).await;
            Self::unsubscribe_from_hub(
                event_sender,
                subscriber_id,
                room_id,
                media_id,
                SubscribeType::WhepPull,
                SubDataType::Packet,
            )
            .await;
        });
        subscription_guard.active = false;

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    /// Get HLS M3U8 playlist from this (publisher) node.
    /// Non-publisher nodes proxy HLS playlist requests here via gRPC.
    async fn get_hls_playlist(
        &self,
        request: Request<GetHlsPlaylistRequest>,
    ) -> Result<Response<GetHlsPlaylistResponse>, Status> {
        self.authenticate(&request)?;

        let req = request.into_inner();
        validate_relay_stream_ids(&req.room_id, &req.media_id)?;
        validate_relay_generation_id(&req.generation_id)?;
        validate_relay_segment_url_base(&req.segment_url_base)?;
        validate_relay_segment_url_suffix(&req.segment_url_suffix)?;
        let generation = self
            .verify_local_hls_generation(
                &req.room_id,
                &req.media_id,
                &req.generation_id,
                req.expected_lease_epoch,
            )
            .await?;
        tracing::debug!(
            room_id = req.room_id,
            media_id = req.media_id,
            expected_lease_epoch = req.expected_lease_epoch,
            "GetHlsPlaylist request"
        );
        let _activity_guard = if let Some(manager) = &self.external_publish_manager {
            manager
                .subscribe_active_generation(&req.room_id, &req.media_id, &req.generation_id)
                .await
        } else {
            None
        };

        let hls_registry = self
            .hls_stream_registry
            .as_ref()
            .ok_or_else(|| Status::unavailable("HLS not enabled on this node"))?;

        let stream_state = crate::api::livestream::find_hls_generation_state(
            hls_registry,
            &req.room_id,
            &req.media_id,
            &req.generation_id,
            generation.ended_at.is_none(),
        )
        .await;

        let response = match stream_state {
            Some(stream_state) => {
                let state = stream_state.read();
                let segment_url_base = req.segment_url_base;
                let segment_url_suffix = req.segment_url_suffix;
                let playlist = state.generate_m3u8(|ts_name| {
                    format!("{segment_url_base}{ts_name}{segment_url_suffix}")
                });
                GetHlsPlaylistResponse {
                    playlist,
                    found: true,
                }
            }
            None => GetHlsPlaylistResponse {
                playlist: String::new(),
                found: false,
            },
        };

        Ok(Response::new(response))
    }

    /// Get HLS TS segment from this (publisher) node.
    /// Non-publisher nodes proxy HLS segment requests here via gRPC.
    async fn get_hls_segment(
        &self,
        request: Request<GetHlsSegmentRequest>,
    ) -> Result<Response<GetHlsSegmentResponse>, Status> {
        self.authenticate(&request)?;

        let req = request.into_inner();
        validate_relay_stream_ids(&req.room_id, &req.media_id)?;
        validate_relay_generation_id(&req.generation_id)?;
        validate_relay_hls_segment_name(&req.segment_name)?;
        self.verify_local_hls_generation(
            &req.room_id,
            &req.media_id,
            &req.generation_id,
            req.expected_lease_epoch,
        )
        .await?;
        tracing::debug!(
            room_id = req.room_id,
            media_id = req.media_id,
            segment_name = req.segment_name,
            expected_lease_epoch = req.expected_lease_epoch,
            "GetHlsSegment request"
        );
        let _activity_guard = if let Some(manager) = &self.external_publish_manager {
            manager
                .subscribe_active_generation(&req.room_id, &req.media_id, &req.generation_id)
                .await
        } else {
            None
        };

        let segment_manager = self
            .segment_manager
            .as_ref()
            .ok_or_else(|| Status::unavailable("HLS not enabled on this node"))?;

        match segment_manager
            .storage()
            .read(&req.room_id, &req.media_id, &req.segment_name)
            .await
        {
            Ok(data) => Ok(Response::new(GetHlsSegmentResponse {
                data, // Zero-copy: storage returns Bytes, proto field is now Bytes
                found: true,
            })),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(Response::new(GetHlsSegmentResponse {
                    data: bytes::Bytes::new(),
                    found: false,
                }))
            }
            Err(err) => {
                tracing::error!(
                    room_id = req.room_id,
                    media_id = req.media_id,
                    segment_name = req.segment_name,
                    error = %err,
                    "failed to read HLS segment for relay"
                );
                Err(Status::internal("Failed to read HLS segment"))
            }
        }
    }

    async fn delete_web_rtc_session(
        &self,
        request: Request<DeleteWebRtcSessionRequest>,
    ) -> Result<Response<DeleteWebRtcSessionResponse>, Status> {
        self.authenticate(&request)?;
        let req = request.into_inner();
        validate_relay_stream_ids(&req.room_id, &req.media_id)?;
        validate_relay_generation_id(&req.session_id)?;
        let kind = ProtoWebRtcSessionKind::try_from(req.kind)
            .map_err(|_| Status::invalid_argument("invalid WebRTC session kind"))?;
        let manager = self
            .webrtc_session_manager
            .as_ref()
            .ok_or_else(|| Status::unavailable("WebRTC session manager is unavailable"))?;
        let deleted = match kind {
            ProtoWebRtcSessionKind::Whip => {
                if req.publish_token.is_empty() {
                    return Err(Status::invalid_argument(
                        "WHIP session deletion requires a publish token",
                    ));
                }
                manager
                    .delete_whip_session(
                        &req.session_id,
                        &req.room_id,
                        &req.media_id,
                        &req.publish_token,
                    )
                    .await
            }
            ProtoWebRtcSessionKind::Whep => {
                manager
                    .delete_whep_session(&req.session_id, &req.room_id, &req.media_id)
                    .await
            }
            ProtoWebRtcSessionKind::Unspecified => {
                return Err(Status::invalid_argument("WebRTC session kind is required"));
            }
        }
        .map_err(map_webrtc_session_error)?;
        Ok(Response::new(DeleteWebRtcSessionResponse { deleted }))
    }
}

impl StreamRelayServiceImpl {
    /// Unsubscribe from `StreamHub`
    async fn unsubscribe_from_hub(
        event_sender: StreamHubEventSender,
        subscriber_id: Uuid,
        room_id: String,
        media_id: String,
        sub_type: SubscribeType,
        sub_data_type: SubDataType,
    ) {
        let sub_info = SubscriberInfo {
            id: subscriber_id,
            sub_type,
            sub_data_type,
            notify_info: NotifyInfo {
                request_url: String::new(),
                remote_addr: String::new(),
            },
        };

        let identifier = StreamIdentifier::Rtmp {
            app_name: room_id.clone(),
            stream_name: media_id.clone(),
        };

        let unsubscribe_event = StreamHubEvent::UnSubscribe {
            identifier,
            info: sub_info,
        };

        if let Err(e) = send_event_with_backpressure_timeout(&event_sender, unsubscribe_event).await
        {
            warn!(
                room_id = %room_id,
                media_id = %media_id,
                "Failed to send unsubscribe event to StreamHub: {}",
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::proto::stream_relay_service_server::StreamRelayService;
    use crate::livestream::managed_stream::ManagedStream as _;
    use crate::util::TEST_GENERATION_ID;
    use bytes::Bytes;
    use futures::StreamExt as _;
    use synctv_xiu::streamhub::define::{DataReceiver, StreamHubEvent};
    use synctv_xiu::streamhub::errors::{StreamHubError, StreamHubErrorValue};
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    use tokio::time::{timeout, Duration};

    type TestResult = anyhow::Result<()>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn attach_test_auth<T>(request: &mut Request<T>) -> anyhow::Result<()> {
        request
            .metadata_mut()
            .insert(AUTH_SECRET_METADATA_KEY, "cluster-secret".parse()?);
        Ok(())
    }

    async fn register_test_publisher(
        registry: &Arc<dyn crate::relay::StreamRegistryTrait>,
    ) -> anyhow::Result<synctv_xiu::streamhub::utils::Uuid> {
        let generation_id = synctv_xiu::streamhub::utils::Uuid::new();
        registry
            .try_activate_generation(
                "room1",
                "media1",
                "test-node",
                "",
                "127.0.0.1:50051",
                &generation_id.to_string(),
            )
            .await?;
        Ok(generation_id)
    }

    async fn recv_event(
        event_rx: &mut tokio::sync::mpsc::Receiver<StreamHubEvent>,
        message: &'static str,
    ) -> anyhow::Result<StreamHubEvent> {
        timeout(Duration::from_secs(1), event_rx.recv())
            .await?
            .ok_or_else(|| test_error(message))
    }

    fn expect_status<T>(
        result: Result<T, tonic::Status>,
        message: &'static str,
    ) -> anyhow::Result<tonic::Status> {
        match result {
            Ok(_) => Err(test_error(message)),
            Err(status) => Ok(status),
        }
    }

    #[test]
    fn test_response_stream_type() {
        // Just verify the ResponseStream type alias compiles
        let (_tx, rx) = tokio::sync::mpsc::channel(128);
        let _: ResponseStream = Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
    }

    #[tokio::test]
    async fn pull_rtp_stream_forwards_packets_and_unsubscribes() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let generation_id = register_test_publisher(&registry).await?;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");
        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: generation_id.to_string(),
            is_reconnect: false,
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;
        let service_task = tokio::spawn(async move { service.pull_rtp_stream(request).await });

        let event = recv_event(&mut event_rx, "RTP subscribe event missing").await?;
        let StreamHubEvent::SubscribeWithGeneration {
            info,
            result_sender,
            ..
        } = event
        else {
            return Err(test_error("expected RTP subscribe event"));
        };
        assert_eq!(info.sub_data_type, SubDataType::Packet);
        let subscriber_id = info.id;
        let (packet_tx, packet_rx) = mpsc::channel(4);
        result_sender
            .send(Ok((
                DataReceiver {
                    frame_receiver: None,
                    packet_receiver: Some(packet_rx),
                },
                None,
            )))
            .map_err(|_| test_error("RTP subscribe result receiver dropped"))?;

        let response = timeout(Duration::from_secs(1), service_task).await???;
        let mut stream = response.into_inner();
        packet_tx
            .send(PacketData::Video {
                timestamp: 90_000,
                data: Bytes::from_static(b"rtp-packet"),
            })
            .await?;
        let forwarded = timeout(Duration::from_secs(1), stream.next())
            .await?
            .ok_or_else(|| test_error("RTP response stream closed"))??;
        assert_eq!(forwarded.packet_type, RtpPacketType::Video as i32);
        assert_eq!(forwarded.timestamp, 90_000);
        assert_eq!(forwarded.data, Bytes::from_static(b"rtp-packet"));

        drop(stream);
        let cleanup = recv_event(&mut event_rx, "RTP unsubscribe event missing").await?;
        assert!(matches!(
            cleanup,
            StreamHubEvent::UnSubscribe { info, .. }
                if info.id == subscriber_id && info.sub_data_type == SubDataType::Packet
        ));
        Ok(())
    }

    #[tokio::test]
    async fn pull_rtp_stream_reports_frame_only_source() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let generation_id = register_test_publisher(&registry).await?;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");
        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: generation_id.to_string(),
            is_reconnect: false,
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;
        let service_task = tokio::spawn(async move { service.pull_rtp_stream(request).await });

        let event = recv_event(&mut event_rx, "RTP subscribe event missing").await?;
        let StreamHubEvent::SubscribeWithGeneration { result_sender, .. } = event else {
            return Err(test_error("expected RTP subscribe event"));
        };
        result_sender
            .send(Err(StreamHubError {
                value: StreamHubErrorValue::NotCorrectDataSenderType,
            }))
            .map_err(|_| test_error("RTP subscribe result receiver dropped"))?;

        let result = timeout(Duration::from_secs(1), service_task).await??;
        let status = expect_status(result, "frame-only source must reject RTP pull")?;
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert!(status.message().contains("does not provide RTP"));
        Ok(())
    }

    #[tokio::test]
    async fn test_forward_rtmp_packets_cancels_while_backpressured() -> TestResult {
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(1);
        let (packet_tx, mut packet_rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();

        packet_tx
            .send(Ok(RtmpPacket {
                data: Bytes::from_static(b"prefill"),
                timestamp: 0,
                frame_type: FrameType::Video as i32,
            }))
            .await?;
        frame_tx
            .send(synctv_xiu::streamhub::define::FrameData::Video {
                timestamp: 1,
                data: Bytes::from_static(b"video"),
            })
            .await?;
        drop(frame_tx);

        let handle = tokio::spawn(forward_rtmp_packets(
            synctv_xiu::streamhub::define::FrameDataReceiver::bounded(frame_rx),
            packet_tx,
            cancel.clone(),
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        timeout(Duration::from_secs(1), handle).await??;

        let retained = packet_rx
            .recv()
            .await
            .ok_or_else(|| test_error("prefilled packet still present"))??;
        assert_eq!(retained.data, Bytes::from_static(b"prefill"));
        assert!(
            packet_rx.try_recv().is_err(),
            "cancelled forwarder must not enqueue extra packets after backpressure cancellation"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_unsubscribe_sent_after_backpressure_cancellation() -> TestResult {
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(1);
        let (packet_tx, _packet_rx) = mpsc::channel(1);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        let cancel = CancellationToken::new();
        let subscriber_id = Uuid::new();

        packet_tx
            .send(Ok(RtmpPacket {
                data: Bytes::from_static(b"prefill"),
                timestamp: 0,
                frame_type: FrameType::Video as i32,
            }))
            .await?;
        frame_tx
            .send(synctv_xiu::streamhub::define::FrameData::Video {
                timestamp: 1,
                data: Bytes::from_static(b"video"),
            })
            .await?;
        drop(frame_tx);

        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            forward_rtmp_packets(
                synctv_xiu::streamhub::define::FrameDataReceiver::bounded(frame_rx),
                packet_tx,
                cancel_for_task,
            )
            .await;
            StreamRelayServiceImpl::unsubscribe_from_hub(
                event_tx,
                subscriber_id,
                "room1".to_string(),
                "media1".to_string(),
                SubscribeType::RtmpPull,
                SubDataType::Frame,
            )
            .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        timeout(Duration::from_secs(1), handle).await??;

        let event = recv_event(&mut event_rx, "event channel should receive unsubscribe").await?;

        match event {
            StreamHubEvent::UnSubscribe { identifier, .. } => {
                assert_eq!(
                    identifier,
                    StreamIdentifier::Rtmp {
                        app_name: "room1".to_string(),
                        stream_name: "media1".to_string(),
                    }
                );
            }
            other => {
                return Err(test_error(format!(
                    "expected unsubscribe event, got {other:?}"
                )))
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn relay_subscription_guard_unsubscribes_when_setup_is_cancelled() -> TestResult {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        let subscriber_id = Uuid::new();
        let guard = RelaySubscriptionGuard {
            event_sender: event_tx,
            subscriber_id,
            room_id: "room-cancel".to_string(),
            media_id: "media-cancel".to_string(),
            sub_type: SubscribeType::RtmpPull,
            sub_data_type: SubDataType::Frame,
            active: true,
        };
        drop(guard);

        let event = timeout(Duration::from_secs(1), event_rx.recv())
            .await?
            .ok_or_else(|| test_error("cancelled relay setup must enqueue unsubscribe"))?;
        let StreamHubEvent::UnSubscribe { identifier, info } = event else {
            return Err(test_error("expected relay setup rollback unsubscribe"));
        };
        assert_eq!(info.id, subscriber_id);
        assert_eq!(
            identifier,
            StreamIdentifier::Rtmp {
                app_name: "room-cancel".to_string(),
                stream_name: "media-cancel".to_string(),
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_pull_rtmp_stream_times_out_when_streamhub_subscription_never_completes(
    ) -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let generation_id = register_test_publisher(&registry).await?;

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");

        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: generation_id.to_string(),
            is_reconnect: false,
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;
        let service_task = tokio::spawn(async move { service.pull_rtmp_stream(request).await });

        let event = recv_event(
            &mut event_rx,
            "event channel should receive subscribe request",
        )
        .await?;
        let StreamHubEvent::SubscribeWithGeneration { .. } = event else {
            return Err(test_error("expected subscribe event"));
        };

        let result = timeout(
            STREAM_HUB_SUBSCRIBE_TIMEOUT + Duration::from_secs(1),
            service_task,
        )
        .await??;
        let status = expect_status(result, "streamhub subscription stall must fail")?;

        assert_eq!(status.code(), tonic::Code::DeadlineExceeded);
        assert!(status.message().contains("Timed out"));
        Ok(())
    }

    #[tokio::test]
    async fn test_relay_rejects_invalid_stream_ids_before_backend_work() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");

        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: "room:1".to_string(),
            media_id: "media1".to_string(),
            generation_id: TEST_GENERATION_ID.to_string(),
            is_reconnect: false,
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;

        let status = expect_status(
            service.pull_rtmp_stream(request).await,
            "invalid stream identifiers must be rejected",
        )?;
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        Ok(())
    }

    #[tokio::test]
    async fn test_relay_rejects_invalid_hls_playlist_base() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret")
        .with_hls_stream_registry(Arc::new(dashmap::DashMap::new()));

        let mut request = Request::new(GetHlsPlaylistRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: TEST_GENERATION_ID.to_string(),
            segment_url_base: "/segments/\n#EXT-X-ENDLIST".to_string(),
            segment_url_suffix: ".ts".to_string(),
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;

        let status = expect_status(
            service.get_hls_playlist(request).await,
            "invalid HLS segment URL base must be rejected",
        )?;
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        Ok(())
    }

    #[tokio::test]
    async fn test_relay_rejects_invalid_hls_playlist_suffix() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret")
        .with_hls_stream_registry(Arc::new(dashmap::DashMap::new()));

        let mut request = Request::new(GetHlsPlaylistRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: TEST_GENERATION_ID.to_string(),
            segment_url_base: "/segments/".to_string(),
            segment_url_suffix: ".ts\n#EXT-X-ENDLIST".to_string(),
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;

        let status = expect_status(
            service.get_hls_playlist(request).await,
            "invalid HLS segment URL suffix must be rejected",
        )?;
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        Ok(())
    }

    #[tokio::test]
    async fn test_relay_preserves_hls_playlist_segment_url_suffix() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let generation_id = register_test_publisher(&registry).await?;
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        let hls_registry = Arc::new(dashmap::DashMap::new());
        let mut playlist = synctv_xiu::hls::HlsPlaylist::new();
        playlist.push_segment(synctv_xiu::hls::SegmentInfo {
            sequence: 0,
            duration_ms: 1_000,
            started_at_ms: 0,
            ts_name: "seg001".to_string(),
            discontinuity: false,
        });
        hls_registry.insert(
            synctv_xiu::hls::generation_registry_key("room1", "media1", &generation_id.to_string()),
            Arc::new(parking_lot::RwLock::new(
                synctv_xiu::hls::StreamProcessorState {
                    app_name: "room1".to_string(),
                    stream_name: "media1".to_string(),
                    playlist,
                    generation_id,
                    marked_for_cleanup: false,
                    cleanup_segment_names: Vec::new(),
                },
            )),
        );
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret")
        .with_hls_stream_registry(hls_registry);

        let mut request = Request::new(GetHlsPlaylistRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: generation_id.to_string(),
            segment_url_base: "/api/live/segment/".to_string(),
            segment_url_suffix: ".png?sig=abc&rid=room1".to_string(),
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;

        let response = service.get_hls_playlist(request).await?.into_inner();

        assert!(response.found);
        assert!(
            response
                .playlist
                .contains("/api/live/segment/seg001.png?sig=abc&rid=room1"),
            "playlist must preserve signed query and disguised extension: {}",
            response.playlist
        );
        Ok(())
    }

    #[tokio::test]
    async fn remote_hls_playlist_poll_refreshes_external_stream_activity() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let manager = Arc::new(ExternalPublishManager::with_timeouts(
            registry.clone(),
            "test-node".to_string(),
            "127.0.0.1:50051".to_string(),
            event_tx.clone(),
            synctv_common::ssrf::SsrfGuard::disabled(),
            60,
            300,
        )?);
        let (stream, generation_id) = manager.install_running_test_stream(
            "room1",
            "media1",
            synctv_core::models::ExternalLiveSourceConfig::HttpFlv {
                url: "http://127.0.0.1/live.flv".to_string(),
            },
        );
        let generation_id_string = generation_id.to_string();
        assert!(
            registry
                .try_activate_generation(
                    "room1",
                    "media1",
                    "test-node",
                    "",
                    "127.0.0.1:50051",
                    &generation_id_string,
                )
                .await?
        );

        let mut playlist = synctv_xiu::hls::HlsPlaylist::new();
        playlist.push_segment(synctv_xiu::hls::SegmentInfo {
            sequence: 0,
            duration_ms: 1_000,
            started_at_ms: 0,
            ts_name: "seg001".to_string(),
            discontinuity: false,
        });
        let hls_registry = Arc::new(dashmap::DashMap::new());
        hls_registry.insert(
            synctv_xiu::hls::generation_registry_key("room1", "media1", &generation_id_string),
            Arc::new(parking_lot::RwLock::new(
                synctv_xiu::hls::StreamProcessorState {
                    app_name: "room1".to_string(),
                    stream_name: "media1".to_string(),
                    playlist,
                    generation_id,
                    marked_for_cleanup: false,
                    cleanup_segment_names: Vec::new(),
                },
            )),
        );
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret")
        .with_external_publish_manager(manager)
        .with_hls_stream_registry(hls_registry);

        tokio::time::timeout(Duration::from_secs(2), async {
            while stream.lifecycle().last_active_elapsed_secs() == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        let mut request = Request::new(GetHlsPlaylistRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: generation_id_string,
            segment_url_base: "/segments/".to_string(),
            segment_url_suffix: ".ts".to_string(),
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;

        let response = service.get_hls_playlist(request).await?.into_inner();
        assert!(response.found);
        assert_eq!(stream.lifecycle().last_active_elapsed_secs(), 0);
        assert_eq!(stream.lifecycle().subscriber_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_relay_rejects_invalid_hls_segment_name() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        let segment_manager = Arc::new(SegmentManager::new(
            Arc::new(synctv_xiu::storage::MemoryStorage::new()),
            Default::default(),
        ));
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret")
        .with_segment_manager(segment_manager);

        let mut request = Request::new(GetHlsSegmentRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: TEST_GENERATION_ID.to_string(),
            segment_name: "../secret".to_string(),
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;

        let status = expect_status(
            service.get_hls_segment(request).await,
            "invalid HLS segment name must be rejected",
        )?;
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        Ok(())
    }

    #[tokio::test]
    async fn test_relay_rejects_stale_expected_lease_epoch_before_streamhub_work() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let generation_id = register_test_publisher(&registry).await?;

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");

        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: generation_id.to_string(),
            is_reconnect: false,
            expected_lease_epoch: 999,
        });
        attach_test_auth(&mut request)?;

        let status = expect_status(
            service.pull_rtmp_stream(request).await,
            "stale lease_epoch must fail closed",
        )?;
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert!(
            event_rx.try_recv().is_err(),
            "stale lease_epoch must be rejected before StreamHub subscription"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_relay_rejects_stale_generation_before_streamhub_work() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let active_generation_id = register_test_publisher(&registry).await?;
        let stale_generation_id = synctv_xiu::streamhub::utils::Uuid::new();
        assert_ne!(stale_generation_id, active_generation_id);

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");

        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: stale_generation_id.to_string(),
            expected_lease_epoch: 1,
            is_reconnect: true,
        });
        attach_test_auth(&mut request)?;

        let status = expect_status(
            service.pull_rtmp_stream(request).await,
            "stale generation must fail closed",
        )?;
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert!(
            event_rx.try_recv().is_err(),
            "stale generation must be rejected before StreamHub subscription"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_hls_playlist_rejects_stale_expected_lease_epoch_before_registry_read(
    ) -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let generation_id = register_test_publisher(&registry).await?;

        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        let hls_registry = Arc::new(dashmap::DashMap::new());
        hls_registry.insert(
            synctv_xiu::hls::generation_registry_key("room1", "media1", &generation_id.to_string()),
            Arc::new(parking_lot::RwLock::new(
                synctv_xiu::hls::StreamProcessorState {
                    app_name: "room1".to_string(),
                    stream_name: "media1".to_string(),
                    playlist: synctv_xiu::hls::HlsPlaylist::new(),
                    generation_id,
                    marked_for_cleanup: false,
                    cleanup_segment_names: Vec::new(),
                },
            )),
        );
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret")
        .with_hls_stream_registry(hls_registry);

        let mut request = Request::new(GetHlsPlaylistRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: generation_id.to_string(),
            segment_url_base: "/segments/".to_string(),
            segment_url_suffix: ".ts".to_string(),
            expected_lease_epoch: 999,
        });
        attach_test_auth(&mut request)?;

        let status = expect_status(
            service.get_hls_playlist(request).await,
            "stale lease_epoch must fail closed",
        )?;
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        Ok(())
    }

    #[tokio::test]
    async fn test_pull_rtmp_stream_waits_for_temporarily_full_streamhub_queue() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let generation_id = register_test_publisher(&registry).await?;

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .try_send(StreamHubEvent::ForceUnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "prefill".to_string(),
                    stream_name: "prefill".to_string(),
                },
            })
            .map_err(|_| test_error("prefill event channel"))?;

        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");

        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: generation_id.to_string(),
            is_reconnect: false,
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;
        let service_task = tokio::spawn(async move { service.pull_rtmp_stream(request).await });

        let blocked = event_rx
            .recv()
            .await
            .ok_or_else(|| test_error("prefill event should be present"))?;
        assert!(matches!(blocked, StreamHubEvent::ForceUnPublish { .. }));

        let event = recv_event(
            &mut event_rx,
            "event channel should receive subscribe request",
        )
        .await?;
        let StreamHubEvent::SubscribeWithGeneration { result_sender, .. } = event else {
            return Err(test_error("expected subscribe event"));
        };

        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(8);
        drop(frame_tx);
        result_sender
            .send(Ok((
                DataReceiver {
                    frame_receiver: Some(
                        synctv_xiu::streamhub::define::FrameDataReceiver::bounded(frame_rx),
                    ),
                    packet_receiver: None,
                },
                None,
            )))
            .map_err(|_| test_error("subscription response should be delivered"))?;

        let response = timeout(Duration::from_secs(1), service_task).await???;
        drop(response);
        Ok(())
    }

    #[tokio::test]
    async fn test_pull_rtmp_stream_maps_full_streamhub_queue_to_resource_exhausted() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let generation_id = register_test_publisher(&registry).await?;

        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .try_send(StreamHubEvent::ForceUnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "prefill".to_string(),
                    stream_name: "prefill".to_string(),
                },
            })
            .map_err(|_| test_error("prefill event channel"))?;

        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");

        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: generation_id.to_string(),
            is_reconnect: false,
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;

        let status = expect_status(
            service.pull_rtmp_stream(request).await,
            "persistently full queue must fail",
        )?;
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        Ok(())
    }

    #[tokio::test]
    async fn test_pull_rtmp_stream_maps_closed_streamhub_queue_to_unavailable() -> TestResult {
        let registry = crate::relay::local_stream_registry();
        let generation_id = register_test_publisher(&registry).await?;

        let (event_tx, event_rx) = tokio::sync::mpsc::channel(1);
        drop(event_rx);

        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");

        let mut request = Request::new(PullRtmpStreamRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            generation_id: generation_id.to_string(),
            is_reconnect: false,
            expected_lease_epoch: 1,
        });
        attach_test_auth(&mut request)?;

        let status = expect_status(
            service.pull_rtmp_stream(request).await,
            "closed streamhub queue must fail",
        )?;
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert!(
            status.message().contains("unavailable"),
            "closed streamhub queue should report backend unavailability, got: {}",
            status.message()
        );
        Ok(())
    }
}
