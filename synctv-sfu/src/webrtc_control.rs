//! WebRTC Control Plane
//!
//! This module implements the WebRTC peer connection management, SDP signaling,
//! ICE candidate handling, and subscriber output path.
//!
//! ## Architecture
//!
//! - `PeerConnection`: Wraps `RTCPeerConnection` with lifecycle management
//! - `SignalingChannel`: Handles SDP offer/answer exchange
//! - `IceManager`: Manages ICE candidates and STUN/TURN configuration
//! - `SubscriberOutput`: Sends forwarded RTP packets to outbound WebRTC tracks
//!
//! ## Integration
//!
//! - Creates `RTCPeerConnection` for each peer
//! - Handles incoming tracks (publisher) via `MediaTrack`
//! - Handles outgoing tracks (subscriber) via `SubscriberOutput`
//! - Provides RTCP feedback for network monitoring

use crate::peer::SfuPeer;
use crate::track::{MediaTrack, TrackKind};
use crate::types::{PeerId, RoomId, TrackId};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::ice::network_type::NetworkType;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType};
use webrtc::rtp_transceiver::rtp_sender::RTCRtpSender;
use webrtc::rtp_transceiver::RTCRtpTransceiver;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{TrackLocal, TrackLocalWriter};

/// WebRTC peer connection wrapper with lifecycle management
pub struct PeerConnection {
    /// Peer ID
    pub peer_id: PeerId,

    /// Room ID
    pub room_id: RoomId,

    /// Underlying WebRTC peer connection
    pub pc: Arc<RTCPeerConnection>,

    /// ICE servers configuration
    ice_servers: Vec<RTCIceServer>,

    /// Outbound tracks (subscriber): track_id -> (sender, local_track)
    outbound_tracks: Arc<RwLock<HashMap<TrackId, (Arc<RTCRtpSender>, Arc<TrackLocalStaticRTP>)>>>,

    /// Subscriber output task handle
    output_task_handle: parking_lot::Mutex<Option<JoinHandle<()>>>,

    /// Cancellation token for cleanup
    cancel_token: CancellationToken,
}

impl PeerConnection {
    /// Create a new peer connection
    pub async fn new(
        peer_id: PeerId,
        room_id: RoomId,
        ice_servers: Vec<RTCIceServer>,
    ) -> Result<Self> {
        // Create media engine with standard codecs
        let mut media_engine = MediaEngine::default();

        // Register VP8 video codec
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/VP8".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "".to_string(),
                    rtcp_feedback: vec![],
                },
                payload_type: 96,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;

        // Register H264 video codec
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/H264".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_string(),
                    rtcp_feedback: vec![],
                },
                payload_type: 102,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;

        // Register Opus audio codec
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "audio/opus".to_string(),
                    clock_rate: 48000,
                    channels: 2,
                    sdp_fmtp_line: "minptime=10;useinbandfec=1".to_string(),
                    rtcp_feedback: vec![],
                },
                payload_type: 111,
                ..Default::default()
            },
            RTPCodecType::Audio,
        )?;

        // Create setting engine
        let mut setting_engine = SettingEngine::default();
        setting_engine.set_network_types(vec![NetworkType::Udp4, NetworkType::Udp6]);

        // Create interceptor registry with default interceptors
        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)?;

        // Build WebRTC API
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_setting_engine(setting_engine)
            .with_interceptor_registry(registry)
            .build();

        // Create peer connection configuration
        let config = RTCConfiguration {
            ice_servers,
            ..Default::default()
        };

        // Create peer connection
        let pc = Arc::new(api.new_peer_connection(config).await?);

        info!(
            peer_id = %peer_id,
            room_id = %room_id,
            "Created WebRTC peer connection"
        );

        Ok(Self {
            peer_id,
            room_id,
            pc,
            ice_servers: vec![],
            outbound_tracks: Arc::new(RwLock::new(HashMap::new())),
            output_task_handle: parking_lot::Mutex::new(None),
            cancel_token: CancellationToken::new(),
        })
    }

    /// Set up connection state callbacks
    pub async fn setup_callbacks(
        &self,
        _network_monitor: Arc<crate::network_monitor::NetworkQualityMonitor>,
    ) -> Result<()> {
        let peer_id = self.peer_id.clone();
        let room_id = self.room_id.clone();

        // ICE connection state change handler
        let peer_id_clone = peer_id.clone();
        let room_id_clone = room_id.clone();
        self.pc
            .on_ice_connection_state_change(Box::new(move |state: RTCIceConnectionState| {
                let peer_id = peer_id_clone.clone();
                let room_id = room_id_clone.clone();
                Box::pin(async move {
                    info!(
                        peer_id = %peer_id,
                        room_id = %room_id,
                        state = ?state,
                        "ICE connection state changed"
                    );
                })
            }));

        // Peer connection state change handler
        let peer_id_clone = peer_id.clone();
        let room_id_clone = room_id.clone();
        self.pc
            .on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
                let peer_id = peer_id_clone.clone();
                let room_id = room_id_clone.clone();
                Box::pin(async move {
                    info!(
                        peer_id = %peer_id,
                        room_id = %room_id,
                        state = ?state,
                        "Peer connection state changed"
                    );
                })
            }));

        // ICE candidate handler
        let peer_id_clone = peer_id.clone();
        self.pc
            .on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
                let peer_id = peer_id_clone.clone();
                Box::pin(async move {
                    if let Some(candidate) = candidate {
                        debug!(
                            peer_id = %peer_id,
                            candidate = %candidate,
                            "ICE candidate generated"
                        );
                        // TODO: Send candidate to signaling channel
                    }
                })
            }));

        // Track handler (for incoming tracks from publisher)
        let peer_id_clone = peer_id.clone();
        let room_id_clone = room_id.clone();
        self.pc
            .on_track(Box::new(
                move |remote_track: Arc<webrtc::track::track_remote::TrackRemote>,
                      rtp_receiver: Arc<webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver>,
                      _transceiver: Arc<RTCRtpTransceiver>| {
                    let peer_id = peer_id_clone.clone();
                    let room_id = room_id_clone.clone();
                    Box::pin(async move {
                        let track_id = TrackId::from(remote_track.id());
                        let kind = TrackKind::from(remote_track.kind());

                        info!(
                            peer_id = %peer_id,
                            room_id = %room_id,
                            track_id = %track_id,
                            kind = ?kind,
                            "Received new track"
                        );

                        // Create MediaTrack wrapper
                        let _media_track = Arc::new(MediaTrack::new(
                            track_id.clone(),
                            peer_id.clone(),
                            remote_track,
                            rtp_receiver,
                        ));

                        // TODO: Register track with room
                        // room.add_published_track(&peer_id, track_id, media_track).await?;
                    })
                },
            ));

        Ok(())
    }

    /// Handle SDP offer (from remote peer) and generate answer
    pub async fn handle_offer(&self, offer: RTCSessionDescription) -> Result<RTCSessionDescription> {
        // Set remote description
        self.pc
            .set_remote_description(offer)
            .await
            .context("Failed to set remote description")?;

        // Create answer
        let answer = self
            .pc
            .create_answer(None)
            .await
            .context("Failed to create answer")?;

        // Set local description
        self.pc
            .set_local_description(answer.clone())
            .await
            .context("Failed to set local description")?;

        info!(
            peer_id = %self.peer_id,
            room_id = %self.room_id,
            "Generated SDP answer"
        );

        Ok(answer)
    }

    /// Create SDP offer (for outbound connection)
    pub async fn create_offer(&self) -> Result<RTCSessionDescription> {
        // Create offer
        let offer = self
            .pc
            .create_offer(None)
            .await
            .context("Failed to create offer")?;

        // Set local description
        self.pc
            .set_local_description(offer.clone())
            .await
            .context("Failed to set local description")?;

        info!(
            peer_id = %self.peer_id,
            room_id = %self.room_id,
            "Created SDP offer"
        );

        Ok(offer)
    }

    /// Handle SDP answer (from remote peer after we sent offer)
    pub async fn handle_answer(&self, answer: RTCSessionDescription) -> Result<()> {
        self.pc
            .set_remote_description(answer)
            .await
            .context("Failed to set remote description")?;

        info!(
            peer_id = %self.peer_id,
            room_id = %self.room_id,
            "Set SDP answer"
        );

        Ok(())
    }

    /// Add ICE candidate
    pub async fn add_ice_candidate(&self, candidate: RTCIceCandidateInit) -> Result<()> {
        self.pc
            .add_ice_candidate(candidate)
            .await
            .context("Failed to add ICE candidate")?;

        debug!(
            peer_id = %self.peer_id,
            "Added ICE candidate"
        );

        Ok(())
    }

    /// Add outbound track (for subscriber)
    pub async fn add_outbound_track(
        &self,
        track_id: TrackId,
        kind: TrackKind,
        codec: &str,
    ) -> Result<Arc<TrackLocalStaticRTP>> {
        // Create local track
        let local_track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: codec.to_string(),
                clock_rate: if kind == TrackKind::Audio { 48000 } else { 90000 },
                channels: if kind == TrackKind::Audio { 2 } else { 0 },
                sdp_fmtp_line: String::new(),
                rtcp_feedback: vec![],
            },
            track_id.as_str().to_string(),
            format!("stream-{}", self.peer_id.as_str()),
        ));

        // Add track to peer connection
        let sender = self
            .pc
            .add_track(Arc::clone(&local_track) as Arc<dyn TrackLocal + Send + Sync>)
            .await
            .context("Failed to add track to peer connection")?;

        // Store sender and track
        self.outbound_tracks
            .write()
            .await
            .insert(track_id.clone(), (sender, Arc::clone(&local_track)));

        info!(
            peer_id = %self.peer_id,
            track_id = %track_id,
            kind = ?kind,
            "Added outbound track"
        );

        Ok(local_track)
    }

    /// Start subscriber output task (reads from peer's packet channel and writes to WebRTC)
    pub async fn start_subscriber_output(&self, peer: Arc<SfuPeer>) -> Result<()> {
        // Take packet receiver from peer (can only be called once)
        let mut packet_rx = peer
            .take_packet_receiver()
            .ok_or_else(|| anyhow!("Packet receiver already taken"))?;

        let peer_id = self.peer_id.clone();
        let room_id = self.room_id.clone();
        let outbound_tracks = Arc::clone(&self.outbound_tracks);
        let cancel_token = self.cancel_token.clone();

        // Spawn output task
        let handle = tokio::spawn(async move {
            info!(
                peer_id = %peer_id,
                room_id = %room_id,
                "Started subscriber output task"
            );

            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        debug!(peer_id = %peer_id, "Subscriber output cancelled");
                        break;
                    }
                    packet = packet_rx.recv() => {
                        match packet {
                            Some(forwarded_packet) => {
                                // Write raw packet bytes to appropriate outbound track
                                // The outbound track is selected based on the subscription
                                let tracks = outbound_tracks.read().await;

                                // For simplicity, write to all outbound tracks
                                // In production, this should respect subscriptions
                                for (track_id, (_sender, local_track)) in tracks.iter() {
                                    // Use write() instead of write_rtp() since we have raw bytes
                                    if let Err(e) = local_track.write(&forwarded_packet.data).await {
                                        warn!(
                                            peer_id = %peer_id,
                                            track_id = %track_id,
                                            error = %e,
                                            "Failed to write RTP packet"
                                        );
                                    }
                                }
                            }
                            None => {
                                info!(peer_id = %peer_id, "Packet channel closed");
                                break;
                            }
                        }
                    }
                }
            }

            info!(peer_id = %peer_id, "Subscriber output task stopped");
        });

        *self.output_task_handle.lock() = Some(handle);

        Ok(())
    }

    /// Close the peer connection
    pub async fn close(&self) -> Result<()> {
        // Cancel output task
        self.cancel_token.cancel();

        // Close peer connection
        self.pc.close().await?;

        info!(
            peer_id = %self.peer_id,
            room_id = %self.room_id,
            "Closed peer connection"
        );

        Ok(())
    }
}

impl Drop for PeerConnection {
    fn drop(&mut self) {
        self.cancel_token.cancel();

        if let Some(handle) = self.output_task_handle.lock().take() {
            handle.abort();
        }

        debug!(
            peer_id = %self.peer_id,
            room_id = %self.room_id,
            "PeerConnection dropped"
        );
    }
}

/// ICE server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceServerConfig {
    /// STUN/TURN URLs
    pub urls: Vec<String>,

    /// Username (for TURN)
    pub username: Option<String>,

    /// Credential (for TURN)
    pub credential: Option<String>,
}

impl From<IceServerConfig> for RTCIceServer {
    fn from(config: IceServerConfig) -> Self {
        Self {
            urls: config.urls,
            username: config.username.unwrap_or_default(),
            credential: config.credential.unwrap_or_default(),
            ..Default::default()
        }
    }
}

/// ICE manager for STUN/TURN server configuration
pub struct IceManager {
    /// Configured ICE servers
    ice_servers: Arc<RwLock<Vec<RTCIceServer>>>,
}

impl IceManager {
    /// Create a new ICE manager with default configuration
    pub fn new() -> Self {
        Self {
            ice_servers: Arc::new(RwLock::new(vec![
                // Default Google STUN servers
                RTCIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".to_string()],
                    ..Default::default()
                },
            ])),
        }
    }

    /// Create ICE manager with custom configuration
    pub fn with_servers(servers: Vec<IceServerConfig>) -> Self {
        let ice_servers = servers.into_iter().map(Into::into).collect();
        Self {
            ice_servers: Arc::new(RwLock::new(ice_servers)),
        }
    }

    /// Get current ICE servers
    pub async fn get_servers(&self) -> Vec<RTCIceServer> {
        self.ice_servers.read().await.clone()
    }

    /// Update ICE servers (for dynamic configuration)
    pub async fn update_servers(&self, servers: Vec<IceServerConfig>) {
        let ice_servers = servers.into_iter().map(Into::into).collect();
        *self.ice_servers.write().await = ice_servers;
        info!("Updated ICE servers configuration");
    }

    /// Add built-in STUN server (if available)
    pub async fn add_builtin_stun(&self, host: &str, port: u16) {
        let url = format!("stun:{}:{}", host, port);
        self.ice_servers.write().await.push(RTCIceServer {
            urls: vec![url.clone()],
            ..Default::default()
        });
        info!(url = %url, "Added built-in STUN server");
    }
}

impl Default for IceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ice_manager_creation() {
        let ice_manager = IceManager::new();
        let servers = ice_manager.get_servers().await;
        assert!(!servers.is_empty());
    }

    #[tokio::test]
    async fn test_ice_manager_update() {
        let ice_manager = IceManager::new();

        let custom_servers = vec![IceServerConfig {
            urls: vec!["stun:custom.server.com:3478".to_string()],
            username: None,
            credential: None,
        }];

        ice_manager.update_servers(custom_servers).await;
        let servers = ice_manager.get_servers().await;
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls[0], "stun:custom.server.com:3478");
    }

    #[tokio::test]
    async fn test_peer_connection_creation() {
        let peer_id = PeerId::from("test-peer");
        let room_id = RoomId::from("test-room");
        let ice_servers = vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_string()],
            ..Default::default()
        }];

        let result = PeerConnection::new(peer_id, room_id, ice_servers).await;
        assert!(result.is_ok());
    }
}
