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

use crate::packet_pacer::PacketPacer;
use crate::peer::SfuPeer;
use crate::track::TrackKind;
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
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType};
use webrtc::rtp_transceiver::RTCPFeedback;
use webrtc::rtp_transceiver::rtp_sender::RTCRtpSender;
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

    /// Packet pacer shared with the RTCP handler so bandwidth estimation
    /// updates can dynamically adjust the pacing rate.
    packet_pacer: Arc<PacketPacer>,

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

        // RTCP feedback parameters for video codecs: enable PLI, NACK, REMB,
        // and transport-cc so that browsers/clients can report packet loss
        // and bandwidth estimates back to the SFU.
        let video_rtcp_feedback = vec![
            RTCPFeedback { typ: "nack".to_string(), parameter: "".to_string() },
            RTCPFeedback { typ: "nack".to_string(), parameter: "pli".to_string() },
            RTCPFeedback { typ: "goog-remb".to_string(), parameter: "".to_string() },
            RTCPFeedback { typ: "transport-cc".to_string(), parameter: "".to_string() },
        ];

        // Register VP8 video codec
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/VP8".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "".to_string(),
                    rtcp_feedback: video_rtcp_feedback.clone(),
                },
                payload_type: 96,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;

        // Register H264 video codecs.
        //
        // Supported profiles (profile-level-id format: PPCCll):
        //   42e01f = Constrained Baseline Profile, Level 3.1
        //            Widest compatibility (mobile, low-end devices, all browsers)
        //   42001f = Baseline Profile, Level 3.1
        //            Same as CB but without constraint flags
        //   4d001f = Main Profile, Level 3.1
        //            Better compression than Baseline (B-frames), most desktop browsers
        //   640c1f = High Profile, Level 3.1
        //            Best compression, hardware acceleration on modern devices
        //
        // Each profile is registered as a separate codec entry so that SDP
        // negotiation can match the profile advertised by the remote peer.
        // Payload types follow Chrome/Firefox conventions.

        // H264 Constrained Baseline Profile, Level 3.1 (widest compatibility)
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/H264".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_string(),
                    rtcp_feedback: video_rtcp_feedback.clone(),
                },
                payload_type: 102,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;

        // H264 Constrained Baseline Profile, Level 3.1 (packetization-mode=0, single NAL)
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/H264".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42e01f".to_string(),
                    rtcp_feedback: video_rtcp_feedback.clone(),
                },
                payload_type: 127,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;

        // H264 Main Profile, Level 3.1 (better compression with B-frames)
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/H264".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=4d001f".to_string(),
                    rtcp_feedback: video_rtcp_feedback.clone(),
                },
                payload_type: 125,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;

        // H264 High Profile, Level 3.1 (best compression, hardware accelerated)
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/H264".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=640c1f".to_string(),
                    rtcp_feedback: video_rtcp_feedback.clone(),
                },
                payload_type: 108,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;

        // Register VP9 video codec (Profile 0)
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/VP9".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "profile-id=0".to_string(),
                    rtcp_feedback: video_rtcp_feedback.clone(),
                },
                payload_type: 98,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;

        // Register VP9 video codec (Profile 2 - 10-bit HDR)
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/VP9".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "profile-id=2".to_string(),
                    rtcp_feedback: video_rtcp_feedback.clone(),
                },
                payload_type: 100,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;

        // Register AV1 video codec
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/AV1".to_string(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: "".to_string(),
                    rtcp_feedback: video_rtcp_feedback,
                },
                payload_type: 35,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;

        // Register Opus audio codec (transport-cc for audio bandwidth estimation)
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "audio/opus".to_string(),
                    clock_rate: 48000,
                    channels: 2,
                    sdp_fmtp_line: "minptime=10;useinbandfec=1".to_string(),
                    rtcp_feedback: vec![
                        RTCPFeedback { typ: "transport-cc".to_string(), parameter: "".to_string() },
                    ],
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
            ice_servers: ice_servers.clone(),
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
            ice_servers,
            outbound_tracks: Arc::new(RwLock::new(HashMap::new())),
            output_task_handle: parking_lot::Mutex::new(None),
            packet_pacer: Arc::new(PacketPacer::new(1000, 50)),
            cancel_token: CancellationToken::new(),
        })
    }

    /// Set up connection state monitoring callbacks (ICE state, peer connection state).
    ///
    /// This method only registers connection lifecycle callbacks. Track and ICE
    /// candidate callbacks should be set by the caller (e.g., `SfuSessionManager`)
    /// which has the room context needed to route tracks and candidates.
    ///
    /// Includes ICE failure fallback: when ICE enters `Disconnected` or `Failed`,
    /// an ICE restart is attempted to recover the connection without requiring a
    /// full renegotiation.
    ///
    /// ## Parameters
    ///
    /// * `_network_monitor` - Network quality monitor (reserved for future use)
    /// * `ice_restart_tx` - Optional signaling channel for sending ICE restart offers
    ///   back to the client. If `None`, ICE restart offers are created but cannot be
    ///   delivered (the pre-SFU-23 behavior).
    pub async fn setup_callbacks(
        &self,
        _network_monitor: Arc<crate::network_monitor::NetworkQualityMonitor>,
        ice_restart_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::session_manager::SfuSignalingEvent>>,
    ) -> Result<()> {
        let peer_id = self.peer_id.clone();
        let room_id = self.room_id.clone();

        // ICE connection state change handler with restart fallback
        let peer_id_clone = peer_id.clone();
        let room_id_clone = room_id.clone();
        let pc_for_ice = Arc::clone(&self.pc);
        self.pc
            .on_ice_connection_state_change(Box::new(move |state: RTCIceConnectionState| {
                let peer_id = peer_id_clone.clone();
                let room_id = room_id_clone.clone();
                let pc = Arc::clone(&pc_for_ice);
                let restart_tx = ice_restart_tx.clone();
                Box::pin(async move {
                    info!(
                        peer_id = %peer_id,
                        room_id = %room_id,
                        state = ?state,
                        "ICE connection state changed"
                    );

                    // Attempt ICE restart on disconnection or failure
                    match state {
                        RTCIceConnectionState::Disconnected => {
                            warn!(
                                peer_id = %peer_id,
                                room_id = %room_id,
                                "ICE disconnected, will attempt restart if it progresses to Failed"
                            );
                        }
                        RTCIceConnectionState::Failed => {
                            warn!(
                                peer_id = %peer_id,
                                room_id = %room_id,
                                "ICE failed, attempting ICE restart"
                            );
                            // Create a new offer with ICE restart to re-gather candidates
                            // without tearing down the entire PeerConnection.
                            let mut offer_options = webrtc::peer_connection::offer_answer_options::RTCOfferOptions::default();
                            offer_options.ice_restart = true;

                            match pc.create_offer(Some(offer_options)).await {
                                Ok(offer) => {
                                    if let Err(e) = pc.set_local_description(offer.clone()).await {
                                        warn!(
                                            peer_id = %peer_id,
                                            error = %e,
                                            "ICE restart: failed to set local description"
                                        );
                                    } else {
                                        // SFU-23 Fix 2: Send the restart offer to the client
                                        // through the signaling channel so the client can
                                        // respond with an answer to complete ICE restart.
                                        if let Some(ref tx) = restart_tx {
                                            match serde_json::to_string(&offer) {
                                                Ok(offer_json) => {
                                                    if tx.send(crate::session_manager::SfuSignalingEvent::IceRestartOffer {
                                                        peer_id: peer_id.as_str().to_string(),
                                                        sdp: offer_json,
                                                    }).is_err() {
                                                        warn!(
                                                            peer_id = %peer_id,
                                                            "ICE restart: signaling channel closed, cannot deliver offer"
                                                        );
                                                    } else {
                                                        info!(
                                                            peer_id = %peer_id,
                                                            "ICE restart: offer sent to client via signaling channel"
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!(
                                                        peer_id = %peer_id,
                                                        error = %e,
                                                        "ICE restart: failed to serialize offer"
                                                    );
                                                }
                                            }
                                        } else {
                                            warn!(
                                                peer_id = %peer_id,
                                                "ICE restart: no signaling channel available to deliver offer"
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        peer_id = %peer_id,
                                        error = %e,
                                        "ICE restart: failed to create offer"
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
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
        // Create local track with RTCP feedback matching the codec registration
        let rtcp_feedback = if kind == TrackKind::Audio {
            vec![
                RTCPFeedback { typ: "transport-cc".to_string(), parameter: "".to_string() },
            ]
        } else {
            vec![
                RTCPFeedback { typ: "nack".to_string(), parameter: "".to_string() },
                RTCPFeedback { typ: "nack".to_string(), parameter: "pli".to_string() },
                RTCPFeedback { typ: "goog-remb".to_string(), parameter: "".to_string() },
                RTCPFeedback { typ: "transport-cc".to_string(), parameter: "".to_string() },
            ]
        };
        let local_track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: codec.to_string(),
                clock_rate: if kind == TrackKind::Audio { 48000 } else { 90000 },
                channels: if kind == TrackKind::Audio { 2 } else { 0 },
                sdp_fmtp_line: String::new(),
                rtcp_feedback,
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

    /// Get a reference to the shared packet pacer.
    ///
    /// The RTCP handler uses this to call `set_target_bitrate()` whenever
    /// bandwidth estimation produces a new value.
    pub fn pacer(&self) -> &Arc<PacketPacer> {
        &self.packet_pacer
    }

    /// Start subscriber output task (reads from peer's packet channel and writes to WebRTC)
    ///
    /// Routes each forwarded packet to the correct outbound track by matching
    /// `source_track_id`. This ensures audio packets go to audio tracks and
    /// video packets go to video tracks.
    ///
    /// Uses `PacketPacer` to smooth outgoing traffic and prevent bursty
    /// transmission that can cause congestion and packet loss.
    pub async fn start_subscriber_output(&self, peer: Arc<SfuPeer>) -> Result<()> {
        // Take packet receiver from peer (can only be called once)
        let mut packet_rx = peer
            .take_packet_receiver()
            .ok_or_else(|| anyhow!("Packet receiver already taken"))?;

        let peer_id = self.peer_id.clone();
        let room_id = self.room_id.clone();
        let outbound_tracks = Arc::clone(&self.outbound_tracks);
        let cancel_token = self.cancel_token.clone();

        // Use the shared packet pacer so the RTCP handler can update its
        // target bitrate dynamically based on bandwidth estimation.
        let pacer = Arc::clone(&self.packet_pacer);

        // Spawn output task
        let handle = tokio::spawn(async move {
            info!(
                peer_id = %peer_id,
                room_id = %room_id,
                "Started subscriber output task with packet pacing"
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
                                // Pace packet to prevent bursty transmission
                                let packet_size = forwarded_packet.data.len();
                                if !pacer.pace_packet(packet_size).await {
                                    debug!(
                                        peer_id = %peer_id,
                                        packet_size = packet_size,
                                        "Packet dropped by pacer (congestion)"
                                    );
                                    continue;
                                }

                                let tracks = outbound_tracks.read().await;

                                // Route packet to the matching outbound track by source_track_id.
                                // If the packet has a source_track_id, write only to that track.
                                // This prevents audio packets from being written to video tracks
                                // and vice versa.
                                if let Some(ref source_id) = forwarded_packet.source_track_id {
                                    if let Some((_sender, local_track)) = tracks.get(source_id) {
                                        if let Err(e) = local_track.write(&forwarded_packet.data).await {
                                            warn!(
                                                peer_id = %peer_id,
                                                track_id = %source_id,
                                                error = %e,
                                                "Failed to write RTP packet to matched track"
                                            );
                                        }
                                    } else {
                                        // No matching outbound track for this source.
                                        // This can happen transiently during track setup/teardown.
                                        debug!(
                                            peer_id = %peer_id,
                                            source_track_id = %source_id,
                                            "No outbound track for source, dropping packet"
                                        );
                                    }
                                } else {
                                    // Legacy fallback: no source_track_id on packet.
                                    // Write to all tracks (preserves old behavior for any
                                    // code paths that haven't been updated yet).
                                    for (track_id, (_sender, local_track)) in tracks.iter() {
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
