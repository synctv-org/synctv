use std::pin::Pin;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use synctv_xiu::streamhub::{
    define::{NotifyInfo, StreamHubEvent, StreamHubEventSender, SubscribeType, SubscriberInfo},
    errors::StreamHubErrorValue,
    send_event_with_backpressure_timeout,
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use super::proto::{
    stream_relay_service_server, FrameType, GetHlsPlaylistRequest, GetHlsPlaylistResponse,
    GetHlsSegmentRequest, GetHlsSegmentResponse, PullRtmpStreamRequest, RtmpPacket,
};
use crate::livestream::segment_manager::SegmentManager;
use crate::protocols::hls::StreamRegistry as HlsStreamRegistry;
use crate::relay::StreamRegistryTrait;

type ResponseStream = Pin<Box<dyn Stream<Item = Result<RtmpPacket, Status>> + Send>>;

/// Metadata key for cluster authentication shared secret
const AUTH_SECRET_METADATA_KEY: &str = "x-cluster-secret";
const STREAM_HUB_SUBSCRIBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn map_streamhub_enqueue_error(error: synctv_xiu::streamhub::errors::StreamHubError) -> Status {
    match error.value {
        StreamHubErrorValue::SendError => {
            Status::internal("StreamHub subscribe queue is unavailable")
        }
        other => {
            tracing::error!("failed to enqueue StreamHub subscribe event: {other:?}");
            Status::internal("Failed to enqueue subscribe event")
        }
    }
}

/// Callback invoked when the relay service forwards frames from a local publisher.
///
/// Used to record publisher data activity so that silent publisher detection does not
/// incorrectly time out publishers that are actively sending data via gRPC relay.
pub type RelayActivityCallback = Arc<dyn Fn(&str, &str) + Send + Sync>;

async fn forward_rtmp_packets(
    mut frame_receiver: tokio::sync::mpsc::Receiver<synctv_xiu::streamhub::define::FrameData>,
    tx: mpsc::Sender<Result<RtmpPacket, Status>>,
    cancel_token: CancellationToken,
    room_id: &str,
    media_id: &str,
    activity_callback: Option<RelayActivityCallback>,
) {
    info!("Streaming live data to puller");
    let mut frame_count: u64 = 0;

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

        frame_count += 1;
        if frame_count % 100 == 1 {
            if let Some(ref callback) = activity_callback {
                callback(room_id, media_id);
            }
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
    /// Optional callback to record publisher activity when forwarding frames.
    /// This extends silent publisher detection (LS-5) to the gRPC relay path,
    /// preventing false timeouts when a publisher has remote viewers via gRPC
    /// but no local HLS consumers.
    activity_callback: Option<RelayActivityCallback>,
}

impl StreamRelayServiceImpl {
    #[must_use]
    pub fn new(
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
            activity_callback: None,
        }
    }

    /// Set a callback to record publisher activity when forwarding frames.
    /// This extends LS-5 silent publisher detection to the FLV/gRPC relay path.
    #[must_use]
    pub fn with_activity_callback(mut self, callback: RelayActivityCallback) -> Self {
        self.activity_callback = Some(callback);
        self
    }

    /// Set the cluster authentication secret.
    /// When set, all incoming requests must include this secret in metadata.
    #[must_use]
    pub fn with_cluster_secret(mut self, secret: impl Into<Vec<u8>>) -> Self {
        self.cluster_secret = Some(secret.into());
        self
    }

    /// Set the HLS segment manager for serving TS segments via gRPC proxy.
    #[must_use]
    pub fn with_segment_manager(mut self, segment_manager: Arc<SegmentManager>) -> Self {
        self.segment_manager = Some(segment_manager);
        self
    }

    /// Set the HLS stream registry for generating M3U8 playlists via gRPC proxy.
    #[must_use]
    pub fn with_hls_stream_registry(mut self, hls_stream_registry: HlsStreamRegistry) -> Self {
        self.hls_stream_registry = Some(hls_stream_registry);
        self
    }

    /// Authenticate a gRPC request using the cluster shared secret.
    /// Uses constant-time comparison to prevent timing attacks.
    #[allow(clippy::result_large_err)]
    pub fn authenticate<T>(&self, request: &Request<T>) -> Result<(), Status> {
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
        // HIGH-7: Authenticate the request using cluster shared secret
        self.authenticate(&request)?;

        let req = request.into_inner();
        info!(
            room_id = req.room_id,
            media_id = req.media_id,
            is_reconnect = req.is_reconnect,
            "PullRtmpStream request (service-to-service internal call)"
        );

        // Check if this node is the publisher
        let publisher_info = self
            .registry
            .get_publisher(&req.room_id, &req.media_id)
            .await
            .map_err(|e| {
                tracing::error!("Failed to get publisher: {e}");
                Status::internal("Failed to get publisher info")
            })?
            .ok_or_else(|| Status::not_found("No active publisher for this media"))?;

        if publisher_info.node_id != self.node_id {
            return Err(Status::failed_precondition(format!(
                "This node ({}) is not the publisher (publisher is {})",
                self.node_id, publisher_info.node_id
            )));
        }

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
        let subscribe_event = StreamHubEvent::Subscribe {
            identifier,
            info: sub_info,
            result_sender: event_result_sender,
        };

        // Send subscribe event (mpsc::Sender is Clone + Send + Sync, no Mutex needed)
        send_event_with_backpressure_timeout(&self.stream_hub_event_sender, subscribe_event)
            .await
            .map_err(|error| {
                if matches!(error.value, StreamHubErrorValue::SendError)
                    && !self.stream_hub_event_sender.is_closed()
                {
                    Status::resource_exhausted("StreamHub subscribe queue is saturated")
                } else {
                    map_streamhub_enqueue_error(error)
                }
            })?;

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
        let activity_cb = self.activity_callback.clone();
        tokio::spawn(async move {
            forward_rtmp_packets(
                frame_receiver,
                tx,
                child_token,
                &room_id_clone,
                &media_id_clone,
                activity_cb,
            )
            .await;

            info!("Stream ended, unsubscribing");
            Self::unsubscribe_from_hub(
                event_sender_clone,
                subscriber_id,
                room_id_clone,
                media_id_clone,
            )
            .await;
        });

        let output_stream = ReceiverStream::new(rx);
        Ok(Response::new(
            Box::pin(output_stream) as Self::PullRtmpStreamStream
        ))
    }

    /// Get HLS M3U8 playlist from this (publisher) node.
    /// Non-publisher nodes proxy HLS playlist requests here via gRPC.
    async fn get_hls_playlist(
        &self,
        request: Request<GetHlsPlaylistRequest>,
    ) -> Result<Response<GetHlsPlaylistResponse>, Status> {
        self.authenticate(&request)?;

        let req = request.into_inner();
        tracing::debug!(
            room_id = req.room_id,
            media_id = req.media_id,
            "GetHlsPlaylist request"
        );

        let hls_registry = self
            .hls_stream_registry
            .as_ref()
            .ok_or_else(|| Status::unavailable("HLS not enabled on this node"))?;

        // Registry key format: "room_id/media_id" (matches remuxer's app_name/stream_name)
        let stream_key = format!("{}/{}", req.room_id, req.media_id);

        let response = match hls_registry.get(&stream_key) {
            Some(stream_state) => {
                let state = stream_state.read();
                let segment_url_base = req.segment_url_base;
                let playlist =
                    state.generate_m3u8(|ts_name| format!("{segment_url_base}{ts_name}.ts"));
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
        tracing::debug!(
            room_id = req.room_id,
            media_id = req.media_id,
            segment_name = req.segment_name,
            "GetHlsSegment request"
        );

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
            Err(_) => Ok(Response::new(GetHlsSegmentResponse {
                data: bytes::Bytes::new(),
                found: false,
            })),
        }
    }
}

impl StreamRelayServiceImpl {
    /// Unsubscribe from `StreamHub`
    async fn unsubscribe_from_hub(
        event_sender: StreamHubEventSender,
        subscriber_id: Uuid,
        room_id: String,
        media_id: String,
    ) {
        let sub_info = SubscriberInfo {
            id: subscriber_id,
            sub_type: SubscribeType::RtmpPull,
            sub_data_type: synctv_xiu::streamhub::define::SubDataType::Frame,
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

        if let Err(e) = event_sender.send(unsubscribe_event).await {
            warn!(
                room_id = %room_id,
                media_id = %media_id,
                "Failed to send unsubscribe event to StreamHub (channel closed): {}",
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::proto::stream_relay_service_server::StreamRelayService;
    use bytes::Bytes;
    use synctv_xiu::streamhub::define::{DataReceiver, StreamHubEvent};
    use synctv_xiu::streamhub::stream::StreamIdentifier;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn test_service_creation() {
        let (_event_sender, _) =
            tokio::sync::mpsc::channel::<synctv_xiu::streamhub::define::StreamHubEvent>(64);
        let node_id = "test_node".to_string();

        // Verify the node_id is correct
        assert_eq!(node_id, "test_node");

        // Note: Full service creation requires StreamRegistry which needs Redis
    }

    #[test]
    fn test_response_stream_type() {
        // Just verify the ResponseStream type alias compiles
        let (_tx, rx) = tokio::sync::mpsc::channel(128);
        let _: ResponseStream = Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
    }

    #[tokio::test]
    async fn test_forward_rtmp_packets_cancels_while_backpressured() {
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(1);
        let (packet_tx, mut packet_rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();

        packet_tx
            .send(Ok(RtmpPacket {
                data: Bytes::from_static(b"prefill"),
                timestamp: 0,
                frame_type: FrameType::Video as i32,
            }))
            .await
            .expect("prefill output channel");
        frame_tx
            .send(synctv_xiu::streamhub::define::FrameData::Video {
                timestamp: 1,
                data: Bytes::from_static(b"video"),
            })
            .await
            .expect("send frame");
        drop(frame_tx);

        let handle = tokio::spawn(forward_rtmp_packets(
            frame_rx,
            packet_tx,
            cancel.clone(),
            "room1",
            "media1",
            None,
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        timeout(Duration::from_secs(1), handle)
            .await
            .expect("forwarder should exit after cancellation")
            .expect("forwarder task should not panic");

        let retained = packet_rx
            .recv()
            .await
            .expect("prefilled packet still present");
        assert_eq!(retained.unwrap().data, Bytes::from_static(b"prefill"));
        assert!(
            packet_rx.try_recv().is_err(),
            "cancelled forwarder must not enqueue extra packets after backpressure cancellation"
        );
    }

    #[tokio::test]
    async fn test_unsubscribe_sent_after_backpressure_cancellation() {
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
            .await
            .expect("prefill output channel");
        frame_tx
            .send(synctv_xiu::streamhub::define::FrameData::Video {
                timestamp: 1,
                data: Bytes::from_static(b"video"),
            })
            .await
            .expect("send frame");
        drop(frame_tx);

        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            forward_rtmp_packets(
                frame_rx,
                packet_tx,
                cancel_for_task,
                "room1",
                "media1",
                None,
            )
            .await;
            StreamRelayServiceImpl::unsubscribe_from_hub(
                event_tx,
                subscriber_id,
                "room1".to_string(),
                "media1".to_string(),
            )
            .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        timeout(Duration::from_secs(1), handle)
            .await
            .expect("wrapper should exit after cancellation")
            .expect("wrapper task should not panic");

        let event = timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("unsubscribe should be emitted")
            .expect("event channel should receive unsubscribe");

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
            other => panic!("expected unsubscribe event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_pull_rtmp_stream_times_out_when_streamhub_subscription_never_completes() {
        let registry = Arc::new(crate::relay::InMemoryStreamRegistry::new());
        registry
            .register_publisher("room1", "media1", "test-node", "room1", "127.0.0.1:50051")
            .await
            .expect("register publisher");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");

        let service_task = tokio::spawn(async move {
            let mut request = Request::new(PullRtmpStreamRequest {
                room_id: "room1".to_string(),
                media_id: "media1".to_string(),
                is_reconnect: false,
            });
            request
                .metadata_mut()
                .insert(AUTH_SECRET_METADATA_KEY, "cluster-secret".parse().unwrap());
            service.pull_rtmp_stream(request).await
        });

        let event = timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("subscribe event should be emitted")
            .expect("event channel should receive subscribe request");
        let StreamHubEvent::Subscribe { .. } = event else {
            panic!("expected subscribe event");
        };

        let result = timeout(
            STREAM_HUB_SUBSCRIBE_TIMEOUT + Duration::from_secs(1),
            service_task,
        )
        .await
        .expect("service call should time out instead of hanging forever")
        .expect("service task should complete");
        let Err(status) = result else {
            panic!("streamhub subscription stall must fail");
        };

        assert_eq!(status.code(), tonic::Code::DeadlineExceeded);
        assert!(status.message().contains("Timed out"));
    }

    #[tokio::test]
    async fn test_pull_rtmp_stream_waits_for_temporarily_full_streamhub_queue() {
        let registry = Arc::new(crate::relay::InMemoryStreamRegistry::new());
        registry
            .register_publisher("room1", "media1", "test-node", "room1", "127.0.0.1:50051")
            .await
            .expect("register publisher");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .try_send(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "prefill".to_string(),
                    stream_name: "prefill".to_string(),
                },
            })
            .expect("prefill event channel");

        let service = StreamRelayServiceImpl::new(
            registry,
            "test-node".to_string(),
            event_tx,
            CancellationToken::new(),
        )
        .with_cluster_secret("cluster-secret");

        let service_task = tokio::spawn(async move {
            let mut request = Request::new(PullRtmpStreamRequest {
                room_id: "room1".to_string(),
                media_id: "media1".to_string(),
                is_reconnect: false,
            });
            request
                .metadata_mut()
                .insert(AUTH_SECRET_METADATA_KEY, "cluster-secret".parse().unwrap());
            service.pull_rtmp_stream(request).await
        });

        let blocked = event_rx
            .recv()
            .await
            .expect("prefill event should be present");
        assert!(matches!(blocked, StreamHubEvent::UnPublish { .. }));

        let event = timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("subscribe event should be emitted once queue capacity is freed")
            .expect("event channel should receive subscribe request");
        let StreamHubEvent::Subscribe { result_sender, .. } = event else {
            panic!("expected subscribe event");
        };

        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(8);
        drop(frame_tx);
        result_sender
            .send(Ok((
                DataReceiver {
                    frame_receiver: Some(frame_rx),
                    packet_receiver: None,
                },
                None,
            )))
            .expect("subscription response should be delivered");

        let response = timeout(Duration::from_secs(1), service_task)
            .await
            .expect("service call should complete once StreamHub queue drains")
            .expect("service task should complete")
            .expect("pull_rtmp_stream should succeed");
        drop(response);
    }

    #[tokio::test]
    async fn test_pull_rtmp_stream_maps_full_streamhub_queue_to_resource_exhausted() {
        let registry = Arc::new(crate::relay::InMemoryStreamRegistry::new());
        registry
            .register_publisher("room1", "media1", "test-node", "room1", "127.0.0.1:50051")
            .await
            .expect("register publisher");

        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .try_send(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "prefill".to_string(),
                    stream_name: "prefill".to_string(),
                },
            })
            .expect("prefill event channel");

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
            is_reconnect: false,
        });
        request
            .metadata_mut()
            .insert(AUTH_SECRET_METADATA_KEY, "cluster-secret".parse().unwrap());

        let Err(status) = service.pull_rtmp_stream(request).await else {
            panic!("persistently full queue must fail");
        };
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    }

    #[tokio::test]
    async fn test_pull_rtmp_stream_maps_closed_streamhub_queue_to_internal() {
        let registry = Arc::new(crate::relay::InMemoryStreamRegistry::new());
        registry
            .register_publisher("room1", "media1", "test-node", "room1", "127.0.0.1:50051")
            .await
            .expect("register publisher");

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
            is_reconnect: false,
        });
        request
            .metadata_mut()
            .insert(AUTH_SECRET_METADATA_KEY, "cluster-secret".parse().unwrap());

        let Err(status) = service.pull_rtmp_stream(request).await else {
            panic!("closed streamhub queue must fail");
        };
        assert_eq!(status.code(), tonic::Code::Internal);
        assert!(
            status.message().contains("unavailable"),
            "closed streamhub queue should report backend unavailability, got: {}",
            status.message()
        );
    }
}
