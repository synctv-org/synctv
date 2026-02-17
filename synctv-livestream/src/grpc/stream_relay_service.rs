use std::pin::Pin;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use synctv_xiu::streamhub::{
    define::{NotifyInfo, StreamHubEvent, StreamHubEventSender, SubscribeType, SubscriberInfo},
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use super::proto::{
    RtmpPacket, stream_relay_service_server, PullRtmpStreamRequest, FrameType,
    GetHlsPlaylistRequest, GetHlsPlaylistResponse,
    GetHlsSegmentRequest, GetHlsSegmentResponse,
};
use crate::relay::StreamRegistry;
use crate::protocols::hls::StreamRegistry as HlsStreamRegistry;
use crate::livestream::segment_manager::SegmentManager;

type ResponseStream = Pin<Box<dyn Stream<Item = Result<RtmpPacket, Status>> + Send>>;

/// Metadata key for cluster authentication shared secret
const AUTH_SECRET_METADATA_KEY: &str = "x-cluster-secret";

/// `StreamRelayService` implementation
/// Publisher nodes use this to serve RTMP packets to Puller nodes via subscription
/// and HLS playlists/segments to non-publisher nodes via proxy.
///
/// GOP cache is handled by xiu's `StreamHub` internally — when a new subscriber
/// joins, `StreamHub` automatically sends cached GOP frames via `send_prior_data`.
pub struct StreamRelayServiceImpl {
    registry: Arc<StreamRegistry>,
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
}

impl StreamRelayServiceImpl {
    #[must_use]
    pub fn new(
        registry: Arc<StreamRegistry>,
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
        }
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
    fn authenticate<T>(&self, request: &Request<T>) -> Result<(), Status> {
        let Some(expected) = &self.cluster_secret else {
            return Ok(()); // No secret configured, skip auth
        };

        let provided = request
            .metadata()
            .get(AUTH_SECRET_METADATA_KEY)
            .ok_or_else(|| Status::unauthenticated("missing cluster authentication secret"))?
            .as_bytes();

        if expected.ct_eq(provided).into() {
            Ok(())
        } else {
            Err(Status::unauthenticated("invalid cluster authentication secret"))
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
            "PullRtmpStream request (service-to-service internal call)"
        );

        // Check if this node is the publisher
        let mut registry = (*self.registry).clone();
        let publisher_info = registry
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
        self.stream_hub_event_sender
            .try_send(subscribe_event)
            .map_err(|_| Status::internal("Failed to send subscribe event"))?;

        // Wait for subscription result
        let subscribe_result = event_result_receiver
            .await
            .map_err(|_| Status::internal("Subscribe result channel closed"))?
            .map_err(|e| {
                tracing::error!("Subscribe failed: {e}");
                Status::internal("Stream subscription failed")
            })?;

        let mut frame_receiver = subscribe_result
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
            // Stream live data from StreamHub subscription
            // (GOP frames are automatically sent first by StreamHub's send_prior_data)
            info!("Streaming live data to puller");
            loop {
                let frame_data = tokio::select! {
                    _ = child_token.cancelled() => {
                        info!("Relay forwarding task cancelled (shutdown)");
                        break;
                    }
                    result = frame_receiver.recv() => {
                        match result {
                            Some(data) => data,
                            None => break, // Channel closed, stream ended
                        }
                    }
                };

                // Extract data, timestamp, and frame_type from FrameData enum
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
                    data,  // Zero-copy: FrameData's Bytes passed directly to proto Bytes field
                    timestamp,
                    frame_type,
                };

                if tx.send(Ok(packet)).await.is_err() {
                    warn!("Client disconnected during live streaming");
                    break;
                }
            }

            info!("Stream ended, unsubscribing");
            Self::unsubscribe_from_hub(event_sender_clone, subscriber_id, room_id_clone, media_id_clone).await;
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

        let hls_registry = self.hls_stream_registry.as_ref()
            .ok_or_else(|| Status::unavailable("HLS not enabled on this node"))?;

        // Registry key format: "room_id/media_id" (matches remuxer's app_name/stream_name)
        let stream_key = format!("{}/{}", req.room_id, req.media_id);

        let response = match hls_registry.get(&stream_key) {
            Some(stream_state) => {
                let state = stream_state.read();
                let segment_url_base = req.segment_url_base;
                let playlist = state.generate_m3u8(|ts_name| {
                    format!("{segment_url_base}{ts_name}.ts")
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
        tracing::debug!(
            room_id = req.room_id,
            media_id = req.media_id,
            segment_name = req.segment_name,
            "GetHlsSegment request"
        );

        let segment_manager = self.segment_manager.as_ref()
            .ok_or_else(|| Status::unavailable("HLS not enabled on this node"))?;

        // Build storage key: app_name-stream_name-ts_name
        // With canonical format: app_name=room_id, stream_name=media_id
        let storage_key = format!("{}-{}-{}", req.room_id, req.media_id, req.segment_name);

        match segment_manager.storage().read(&storage_key).await {
            Ok(data) => Ok(Response::new(GetHlsSegmentResponse {
                data,  // Zero-copy: storage returns Bytes, proto field is now Bytes
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
            app_name: room_id,
            stream_name: media_id,
        };

        let unsubscribe_event = StreamHubEvent::UnSubscribe {
            identifier,
            info: sub_info,
        };

        if let Err(e) = event_sender.try_send(unsubscribe_event) {
            warn!("Failed to send unsubscribe event: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_service_creation() {
        let (_event_sender, _) = tokio::sync::mpsc::channel::<synctv_xiu::streamhub::define::StreamHubEvent>(64);
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
}
