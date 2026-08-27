use std::{io::Cursor, net::IpAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use sdp::description::session::{
    SessionDescription, ATTR_KEY_INACTIVE, ATTR_KEY_RECV_ONLY, ATTR_KEY_SEND_ONLY,
    ATTR_KEY_SEND_RECV,
};
use synctv_common::ssrf::SsrfGuard;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use webrtc::{
    api::{
        interceptor_registry::register_default_interceptors, media_engine::MediaEngine, APIBuilder,
    },
    ice_transport::ice_server::RTCIceServer,
    interceptor::registry::Registry,
    peer_connection::{
        configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription, RTCPeerConnection,
    },
    rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication,
    rtp::packet::Packet,
    rtp_transceiver::{
        rtp_codec::{
            RTCRtpCodecCapability, RTCRtpCodecParameters, RTCRtpHeaderExtensionCapability,
            RTPCodecType,
        },
        rtp_transceiver_direction::RTCRtpTransceiverDirection,
        RTCPFeedback, RTCRtpTransceiverInit,
    },
    sdp::extmap,
    track::{
        track_local::{track_local_static_rtp::TrackLocalStaticRTP, TrackLocal, TrackLocalWriter},
        track_remote::TrackRemote,
    },
    util::{Marshal, MarshalSize, Unmarshal},
};

use crate::{
    rtmp::auth::RtmpStreamMode,
    streamhub::define::{
        FrameData, FrameDataSender, MediaInfo, PacketData, PacketDataReceiver, PacketDataSender,
        VideoCodecType,
    },
};

use super::media::TrackFrameEncoder;

const MIME_TYPE_H264: &str = "video/H264";
const MIME_TYPE_OPUS: &str = "audio/opus";
const H264_BRIDGE_FMTP: &str =
    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";
const OPUS_BRIDGE_FMTP: &str = "minptime=10;useinbandfec=1";
const RTP_READ_BUFFER_SIZE: usize = 64 * 1024;
const PLI_INTERVAL: Duration = Duration::from_secs(3);
const FRAME_SEND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
pub struct WebRtcIceServer {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
}

impl std::fmt::Debug for WebRtcIceServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebRtcIceServer")
            .field("urls", &self.urls)
            .field("username", &self.username)
            .field("credential", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct WebRtcConfig {
    pub ice_servers: Vec<WebRtcIceServer>,
    pub ice_gathering_timeout: Duration,
    pub max_sdp_bytes: usize,
    pub ssrf_guard: SsrfGuard,
}

impl Default for WebRtcConfig {
    fn default() -> Self {
        Self {
            ice_servers: Vec::new(),
            ice_gathering_timeout: Duration::from_secs(10),
            max_sdp_bytes: 256 * 1024,
            ssrf_guard: SsrfGuard::strict_policy(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WebRtcError {
    #[error("SDP offer is empty")]
    EmptySdp,
    #[error("SDP offer exceeds the {max_bytes}-byte limit")]
    SdpTooLarge { max_bytes: usize },
    #[error("invalid SDP offer: {0}")]
    InvalidSdp(String),
    #[error("WHIP offer has no H.264 format compatible with the livestream bridge")]
    IncompatibleWhipVideoCodec,
    #[error(
        "WHIP offer has no sendable Opus or H.264 media compatible with the livestream bridge"
    )]
    NoCompatibleWhipMedia,
    #[error("WebRTC negotiation failed: {0}")]
    Negotiation(String),
    #[error("ICE gathering timed out after {0:?}")]
    IceGatheringTimeout(Duration),
    #[error("peer connection has no local description")]
    MissingLocalDescription,
}

pub struct PeerSession {
    pub answer_sdp: String,
    peer_connection: Arc<RTCPeerConnection>,
    cancel_token: CancellationToken,
}

pub struct WhepClientSession {
    pub offer_sdp: String,
    peer_connection: Arc<RTCPeerConnection>,
    cancel_token: CancellationToken,
    max_sdp_bytes: usize,
    ssrf_guard: SsrfGuard,
}

impl WhepClientSession {
    pub async fn apply_answer(&self, answer_sdp: &str) -> Result<(), WebRtcError> {
        let answer_sdp = sanitize_remote_sdp(answer_sdp, self.max_sdp_bytes, &self.ssrf_guard)?;
        let answer = RTCSessionDescription::answer(answer_sdp)
            .map_err(|error| WebRtcError::InvalidSdp(error.to_string()))?;
        self.peer_connection
            .set_remote_description(answer)
            .await
            .map_err(|error| WebRtcError::InvalidSdp(error.to_string()))
    }

    pub async fn wait_closed(&self) {
        self.cancel_token.cancelled().await;
    }

    pub async fn wait_connected(&self, timeout: Duration) -> Result<(), WebRtcError> {
        let wait = async {
            let mut interval = tokio::time::interval(Duration::from_millis(25));
            loop {
                match self.peer_connection.connection_state() {
                    RTCPeerConnectionState::Connected => return Ok(()),
                    RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                        return Err(WebRtcError::Negotiation(
                            "peer connection closed before media transport was established"
                                .to_string(),
                        ));
                    }
                    _ => {}
                }
                tokio::select! {
                    () = self.cancel_token.cancelled() => {
                        return Err(WebRtcError::Negotiation(
                            "peer connection closed before media transport was established"
                                .to_string(),
                        ));
                    }
                    _ = interval.tick() => {}
                }
            }
        };
        tokio::time::timeout(timeout, wait).await.map_err(|_| {
            WebRtcError::Negotiation(format!(
                "peer connection did not connect within {}s",
                timeout.as_secs()
            ))
        })?
    }

    pub async fn close(&self) -> Result<(), WebRtcError> {
        self.cancel_token.cancel();
        self.peer_connection
            .close()
            .await
            .map_err(|error| WebRtcError::Negotiation(error.to_string()))
    }
}

impl PeerSession {
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    pub async fn close(&self) -> Result<(), WebRtcError> {
        self.cancel_token.cancel();
        self.peer_connection
            .close()
            .await
            .map_err(|error| WebRtcError::Negotiation(error.to_string()))
    }
}

fn validate_sdp(sdp: &str, max_sdp_bytes: usize) -> Result<(), WebRtcError> {
    if sdp.trim().is_empty() {
        return Err(WebRtcError::EmptySdp);
    }
    if sdp.len() > max_sdp_bytes {
        return Err(WebRtcError::SdpTooLarge {
            max_bytes: max_sdp_bytes,
        });
    }
    Ok(())
}

fn candidate_target_allowed(candidate: &str, ssrf_guard: &SsrfGuard) -> Result<bool, WebRtcError> {
    let fields = candidate.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 6 {
        return Err(WebRtcError::InvalidSdp(
            "ICE candidate has too few fields".to_string(),
        ));
    }
    let address = fields[4];
    fields[5].parse::<u16>().map_err(|error| {
        WebRtcError::InvalidSdp(format!("ICE candidate has an invalid port: {error}"))
    })?;
    if let Ok(ip) = address.parse::<IpAddr>() {
        return Ok(!ssrf_guard.is_ip_blocked(&ip));
    }
    Ok(!ssrf_guard.is_host_blocked(&address) && ssrf_guard.allows_unresolved_host(&address))
}

fn filter_remote_candidates(
    attributes: &mut Vec<sdp::description::common::Attribute>,
    ssrf_guard: &SsrfGuard,
) -> Result<(), WebRtcError> {
    let mut filtered = Vec::with_capacity(attributes.len());
    for attribute in std::mem::take(attributes) {
        if !attribute.key.eq_ignore_ascii_case("candidate") {
            filtered.push(attribute);
            continue;
        }
        let candidate = attribute.value.as_deref().ok_or_else(|| {
            WebRtcError::InvalidSdp("ICE candidate attribute has no value".to_string())
        })?;
        if candidate_target_allowed(candidate, ssrf_guard)? {
            filtered.push(attribute);
        }
    }
    *attributes = filtered;
    Ok(())
}

fn sanitize_remote_sdp(
    sdp: &str,
    max_sdp_bytes: usize,
    ssrf_guard: &SsrfGuard,
) -> Result<String, WebRtcError> {
    validate_sdp(sdp, max_sdp_bytes)?;
    let mut reader = Cursor::new(sdp.as_bytes());
    let mut session = SessionDescription::unmarshal(&mut reader)
        .map_err(|error| WebRtcError::InvalidSdp(error.to_string()))?;
    filter_remote_candidates(&mut session.attributes, ssrf_guard)?;
    for media in &mut session.media_descriptions {
        filter_remote_candidates(&mut media.attributes, ssrf_guard)?;
    }
    Ok(session.to_string())
}

fn validate_offer(
    offer_sdp: &str,
    config: &WebRtcConfig,
) -> Result<SessionDescription, WebRtcError> {
    let sanitized = sanitize_remote_sdp(offer_sdp, config.max_sdp_bytes, &config.ssrf_guard)?;
    let mut reader = Cursor::new(sanitized.as_bytes());
    SessionDescription::unmarshal(&mut reader)
        .map_err(|error| WebRtcError::InvalidSdp(error.to_string()))
}

fn remote_sends_media(
    session: &SessionDescription,
    media: &sdp::description::media::MediaDescription,
) -> bool {
    for (key, sends) in [
        (ATTR_KEY_INACTIVE, false),
        (ATTR_KEY_RECV_ONLY, false),
        (ATTR_KEY_SEND_ONLY, true),
        (ATTR_KEY_SEND_RECV, true),
    ] {
        if media.has_attribute(key) {
            return sends;
        }
    }
    for (key, sends) in [
        (ATTR_KEY_INACTIVE, false),
        (ATTR_KEY_RECV_ONLY, false),
        (ATTR_KEY_SEND_ONLY, true),
        (ATTR_KEY_SEND_RECV, true),
    ] {
        if session.has_attribute(key) {
            return sends;
        }
    }
    true
}

fn fmtp_parameter<'a>(fmtp: &'a str, name: &str) -> Option<&'a str> {
    fmtp.split(';').find_map(|parameter| {
        let (key, value) = parameter.trim().split_once('=')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn h264_fmtp_is_bridge_compatible(fmtp: &str) -> bool {
    if fmtp_parameter(fmtp, "packetization-mode") != Some("1") {
        return false;
    }
    let Some(profile_level_id) = fmtp_parameter(fmtp, "profile-level-id") else {
        return false;
    };
    profile_level_id.len() == 6
        && profile_level_id
            .as_bytes()
            .iter()
            .all(u8::is_ascii_hexdigit)
        && profile_level_id[..4].eq_ignore_ascii_case("42e0")
}

fn validate_whip_offer(
    offer_sdp: &str,
    config: &WebRtcConfig,
    media_mode: RtmpStreamMode,
) -> Result<SessionDescription, WebRtcError> {
    let session = validate_offer(offer_sdp, config)?;
    let mut has_compatible_media = false;
    for media in session
        .media_descriptions
        .iter()
        .filter(|media| media.media_name.port.value != 0 && remote_sends_media(&session, media))
    {
        let kind = if media.media_name.media.eq_ignore_ascii_case("audio") {
            RTPCodecType::Audio
        } else if media.media_name.media.eq_ignore_ascii_case("video") {
            RTPCodecType::Video
        } else {
            continue;
        };
        if !whip_accepts_track(media_mode, kind) {
            continue;
        }
        let media_session = SessionDescription {
            media_descriptions: vec![media.clone()],
            ..Default::default()
        };
        let mut offered_h264 = false;
        let mut compatible_h264 = false;
        let mut compatible_opus = false;
        for payload in &media.media_name.formats {
            let payload_type = payload
                .parse::<u8>()
                .map_err(|error| WebRtcError::InvalidSdp(error.to_string()))?;
            let codec = media_session
                .get_codec_for_payload_type(payload_type)
                .map_err(|error| WebRtcError::InvalidSdp(error.to_string()))?;
            if codec.name.eq_ignore_ascii_case("H264") {
                offered_h264 = true;
                compatible_h264 |=
                    codec.clock_rate == 90_000 && h264_fmtp_is_bridge_compatible(&codec.fmtp);
            } else if codec.name.eq_ignore_ascii_case("opus") {
                let channels = codec.encoding_parameters.parse::<u16>().unwrap_or(0);
                compatible_opus |= codec.clock_rate == 48_000 && matches!(channels, 1 | 2);
            }
        }
        if kind == RTPCodecType::Video && offered_h264 && !compatible_h264 {
            return Err(WebRtcError::IncompatibleWhipVideoCodec);
        }
        has_compatible_media |= match kind {
            RTPCodecType::Audio => compatible_opus,
            RTPCodecType::Video => compatible_h264,
            RTPCodecType::Unspecified => false,
        };
    }
    if !has_compatible_media {
        return Err(WebRtcError::NoCompatibleWhipMedia);
    }
    Ok(session)
}

fn peer_configuration(config: &WebRtcConfig) -> RTCConfiguration {
    let ice_servers = config
        .ice_servers
        .iter()
        .map(|server| RTCIceServer {
            urls: server.urls.clone(),
            username: server.username.clone(),
            credential: server.credential.clone(),
        })
        .collect();
    RTCConfiguration {
        ice_servers,
        ..Default::default()
    }
}

async fn create_peer_connection(
    config: &WebRtcConfig,
) -> Result<Arc<RTCPeerConnection>, WebRtcError> {
    synctv_common::install_process_crypto_provider();
    let mut media_engine = MediaEngine::default();
    register_streaming_codecs(&mut media_engine)?;
    let registry = register_default_interceptors(Registry::new(), &mut media_engine)
        .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build();
    api.new_peer_connection(peer_configuration(config))
        .await
        .map(Arc::new)
        .map_err(|error| WebRtcError::Negotiation(error.to_string()))
}

fn register_streaming_codecs(media_engine: &mut MediaEngine) -> Result<(), WebRtcError> {
    media_engine
        .register_codec(
            RTCRtpCodecParameters {
                capability: streaming_codec_capability(RTPCodecType::Audio),
                payload_type: 111,
                ..Default::default()
            },
            RTPCodecType::Audio,
        )
        .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
    media_engine
        .register_codec(
            RTCRtpCodecParameters {
                capability: streaming_codec_capability(RTPCodecType::Video),
                payload_type: 102,
                ..Default::default()
            },
            RTPCodecType::Video,
        )
        .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
    for kind in [RTPCodecType::Audio, RTPCodecType::Video] {
        media_engine
            .register_header_extension(
                RTCRtpHeaderExtensionCapability {
                    uri: extmap::SDES_MID_URI.to_string(),
                },
                kind,
                None,
            )
            .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
    }
    for uri in [
        extmap::SDES_RTP_STREAM_ID_URI,
        extmap::SDES_REPAIR_RTP_STREAM_ID_URI,
    ] {
        media_engine
            .register_header_extension(
                RTCRtpHeaderExtensionCapability {
                    uri: uri.to_string(),
                },
                RTPCodecType::Video,
                None,
            )
            .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
    }
    Ok(())
}

fn streaming_codec_capability(kind: RTPCodecType) -> RTCRtpCodecCapability {
    match kind {
        RTPCodecType::Audio => RTCRtpCodecCapability {
            mime_type: MIME_TYPE_OPUS.to_string(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: OPUS_BRIDGE_FMTP.to_string(),
            rtcp_feedback: Vec::new(),
        },
        RTPCodecType::Video | RTPCodecType::Unspecified => RTCRtpCodecCapability {
            mime_type: MIME_TYPE_H264.to_string(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: H264_BRIDGE_FMTP.to_string(),
            rtcp_feedback: vec![
                RTCPFeedback {
                    typ: "nack".to_string(),
                    parameter: String::new(),
                },
                RTCPFeedback {
                    typ: "nack".to_string(),
                    parameter: "pli".to_string(),
                },
                RTCPFeedback {
                    typ: "ccm".to_string(),
                    parameter: "fir".to_string(),
                },
            ],
        },
    }
}

fn bind_connection_lifecycle(peer: &Arc<RTCPeerConnection>, cancel_token: CancellationToken) {
    peer.on_peer_connection_state_change(Box::new(move |state| {
        let cancel_token = cancel_token.clone();
        Box::pin(async move {
            match state {
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                    cancel_token.cancel();
                }
                RTCPeerConnectionState::Disconnected => {
                    debug!("WebRTC peer disconnected and may still recover");
                }
                _ => {}
            }
        })
    }));
}

async fn negotiate_answer(
    peer: &Arc<RTCPeerConnection>,
    offer_sdp: &str,
    config: &WebRtcConfig,
) -> Result<String, WebRtcError> {
    let offer = RTCSessionDescription::offer(offer_sdp.to_string())
        .map_err(|error| WebRtcError::InvalidSdp(error.to_string()))?;
    peer.set_remote_description(offer)
        .await
        .map_err(|error| WebRtcError::InvalidSdp(error.to_string()))?;
    let answer = peer
        .create_answer(None)
        .await
        .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
    gather_local_description(peer, answer, config).await
}

async fn gather_local_description(
    peer: &Arc<RTCPeerConnection>,
    description: RTCSessionDescription,
    config: &WebRtcConfig,
) -> Result<String, WebRtcError> {
    let mut gathering_complete = peer.gathering_complete_promise().await;
    peer.set_local_description(description)
        .await
        .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
    tokio::time::timeout(config.ice_gathering_timeout, gathering_complete.recv())
        .await
        .map_err(|_| WebRtcError::IceGatheringTimeout(config.ice_gathering_timeout))?;
    peer.local_description()
        .await
        .map(|description| description.sdp)
        .ok_or(WebRtcError::MissingLocalDescription)
}

pub async fn create_whep_client_session(
    frame_sender: FrameDataSender,
    packet_sender: PacketDataSender,
    config: &WebRtcConfig,
) -> Result<WhepClientSession, WebRtcError> {
    let peer = create_peer_connection(config).await?;
    let cancel_token = CancellationToken::new();
    bind_connection_lifecycle(&peer, cancel_token.clone());
    frame_sender
        .send(FrameData::MediaInfo {
            media_info: MediaInfo {
                audio_clock_rate: 48_000,
                video_clock_rate: 90_000,
                vcodec: VideoCodecType::H264,
            },
        })
        .await
        .map_err(|_| WebRtcError::Negotiation("StreamHub frame channel closed".to_string()))?;

    for kind in [RTPCodecType::Audio, RTPCodecType::Video] {
        peer.add_transceiver_from_kind(
            kind,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                send_encodings: Vec::new(),
            }),
        )
        .await
        .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
    }

    let weak_peer = Arc::downgrade(&peer);
    let track_cancel = cancel_token.clone();
    peer.on_track(Box::new(move |track, _, _| {
        let weak_peer = weak_peer.clone();
        let packet_sender = packet_sender.clone();
        let frame_sender = frame_sender.clone();
        let track_cancel = track_cancel.clone();
        Box::pin(async move {
            if track.kind() == RTPCodecType::Video {
                spawn_pli_loop(weak_peer, track.ssrc(), track_cancel.clone());
            }
            spawn_track_reader(track, packet_sender, frame_sender, track_cancel);
        })
    }));

    let offer = peer
        .create_offer(None)
        .await
        .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
    let offer_sdp = match gather_local_description(&peer, offer, config).await {
        Ok(offer_sdp) => offer_sdp,
        Err(error) => {
            cancel_token.cancel();
            let _ = peer.close().await;
            return Err(error);
        }
    };
    Ok(WhepClientSession {
        offer_sdp,
        peer_connection: peer,
        cancel_token,
        max_sdp_bytes: config.max_sdp_bytes,
        ssrf_guard: config.ssrf_guard.clone(),
    })
}

fn packet_kind(track: &TrackRemote) -> Option<RTPCodecType> {
    match track.kind() {
        RTPCodecType::Audio | RTPCodecType::Video => Some(track.kind()),
        RTPCodecType::Unspecified => None,
    }
}

fn spawn_pli_loop(
    peer: std::sync::Weak<RTCPeerConnection>,
    media_ssrc: u32,
    cancel_token: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PLI_INTERVAL);
        interval.tick().await;
        loop {
            tokio::select! {
                () = cancel_token.cancelled() => break,
                _ = interval.tick() => {
                    let Some(peer) = peer.upgrade() else {
                        break;
                    };
                    if let Err(error) = peer.write_rtcp(&[Box::new(PictureLossIndication {
                        sender_ssrc: 0,
                        media_ssrc,
                    })]).await {
                        debug!(%error, "stopped WebRTC PLI loop");
                        break;
                    }
                }
            }
        }
    });
}

fn spawn_track_reader(
    track: Arc<TrackRemote>,
    packet_sender: PacketDataSender,
    frame_sender: FrameDataSender,
    cancel_token: CancellationToken,
) {
    tokio::spawn(async move {
        let Some(kind) = packet_kind(&track) else {
            warn!("ignoring WebRTC track with unspecified media kind");
            return;
        };
        let codec = track.codec().capability;
        let mut frame_encoder = match TrackFrameEncoder::new(kind, &codec.mime_type, codec.channels)
        {
            Ok(encoder) => encoder,
            Err(error) => {
                warn!(%error, "ignoring unsupported WebRTC media track");
                cancel_token.cancel();
                return;
            }
        };
        let mut buffer = vec![0_u8; RTP_READ_BUFFER_SIZE];
        loop {
            let read_result = tokio::select! {
                () = cancel_token.cancelled() => break,
                result = track.read(&mut buffer) => result,
            };
            let rtp_packet = match read_result {
                Ok((packet, _)) => packet,
                Err(error) => {
                    warn!(%error, "failed to read incoming WebRTC RTP track");
                    cancel_token.cancel();
                    break;
                }
            };
            let mut marshaled = vec![0_u8; rtp_packet.marshal_size()];
            let marshaled_len = match rtp_packet.marshal_to(&mut marshaled) {
                Ok(length) => length,
                Err(error) => {
                    warn!(%error, "failed to marshal incoming WebRTC RTP packet");
                    continue;
                }
            };
            marshaled.truncate(marshaled_len);
            let packet = match kind {
                RTPCodecType::Video => PacketData::Video {
                    timestamp: rtp_packet.header.timestamp,
                    data: Bytes::from(marshaled),
                },
                RTPCodecType::Audio => PacketData::Audio {
                    timestamp: rtp_packet.header.timestamp,
                    data: Bytes::from(marshaled),
                },
                RTPCodecType::Unspecified => continue,
            };
            match packet_sender.try_send(packet) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    debug!(
                        "dropping incoming WebRTC RTP packet because StreamHub is backpressured"
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
            }
            let frames = match frame_encoder.push(&rtp_packet) {
                Ok(frames) => frames,
                Err(error) => {
                    warn!(%error, "failed to convert WebRTC RTP for FLV/HLS");
                    continue;
                }
            };
            for frame in frames {
                match tokio::time::timeout(FRAME_SEND_TIMEOUT, frame_sender.send(frame)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        cancel_token.cancel();
                        return;
                    }
                    Err(_) => {
                        warn!("WebRTC FLV/HLS conversion stalled on StreamHub backpressure");
                        cancel_token.cancel();
                        return;
                    }
                }
            }
        }
    });
}

pub async fn create_whip_session(
    offer_sdp: &str,
    frame_sender: FrameDataSender,
    packet_sender: PacketDataSender,
    media_mode: RtmpStreamMode,
    config: &WebRtcConfig,
) -> Result<PeerSession, WebRtcError> {
    let offer_sdp = validate_whip_offer(offer_sdp, config, media_mode)?.to_string();
    let peer = create_peer_connection(config).await?;
    let cancel_token = CancellationToken::new();
    bind_connection_lifecycle(&peer, cancel_token.clone());
    frame_sender
        .send(FrameData::MediaInfo {
            media_info: MediaInfo {
                audio_clock_rate: 48_000,
                video_clock_rate: 90_000,
                vcodec: VideoCodecType::H264,
            },
        })
        .await
        .map_err(|_| WebRtcError::Negotiation("StreamHub frame channel closed".to_string()))?;

    for kind in [RTPCodecType::Audio, RTPCodecType::Video] {
        peer.add_transceiver_from_kind(
            kind,
            Some(RTCRtpTransceiverInit {
                direction: whip_transceiver_direction(media_mode, kind),
                send_encodings: Vec::new(),
            }),
        )
        .await
        .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
    }

    let weak_peer = Arc::downgrade(&peer);
    let track_cancel = cancel_token.clone();
    peer.on_track(Box::new(move |track, _, _| {
        let weak_peer = weak_peer.clone();
        let packet_sender = packet_sender.clone();
        let frame_sender = frame_sender.clone();
        let track_cancel = track_cancel.clone();
        Box::pin(async move {
            if !whip_accepts_track(media_mode, track.kind()) {
                return;
            }
            if track.kind() == RTPCodecType::Video {
                spawn_pli_loop(weak_peer, track.ssrc(), track_cancel.clone());
            }
            spawn_track_reader(track, packet_sender, frame_sender, track_cancel);
        })
    }));

    let answer_sdp = match negotiate_answer(&peer, &offer_sdp, config).await {
        Ok(answer) => answer,
        Err(error) => {
            cancel_token.cancel();
            let _ = peer.close().await;
            return Err(error);
        }
    };
    Ok(PeerSession {
        answer_sdp,
        peer_connection: peer,
        cancel_token,
    })
}

fn whip_accepts_track(media_mode: RtmpStreamMode, kind: RTPCodecType) -> bool {
    matches!(
        (media_mode, kind),
        (
            RtmpStreamMode::Default,
            RTPCodecType::Audio | RTPCodecType::Video
        ) | (RtmpStreamMode::VideoOnly, RTPCodecType::Video)
            | (RtmpStreamMode::AudioOnly, RTPCodecType::Audio)
    )
}

fn whip_transceiver_direction(
    media_mode: RtmpStreamMode,
    kind: RTPCodecType,
) -> RTCRtpTransceiverDirection {
    if whip_accepts_track(media_mode, kind) {
        RTCRtpTransceiverDirection::Recvonly
    } else {
        RTCRtpTransceiverDirection::Inactive
    }
}

fn outgoing_track(kind: RTPCodecType) -> Arc<TrackLocalStaticRTP> {
    let id = match kind {
        RTPCodecType::Audio => "audio",
        RTPCodecType::Video | RTPCodecType::Unspecified => "video",
    };
    Arc::new(TrackLocalStaticRTP::new(
        streaming_codec_capability(kind),
        id.to_string(),
        "synctv".to_string(),
    ))
}

fn spawn_packet_writer(
    mut receiver: PacketDataReceiver,
    audio_track: Arc<TrackLocalStaticRTP>,
    video_track: Arc<TrackLocalStaticRTP>,
    cancel_token: CancellationToken,
) {
    tokio::spawn(async move {
        loop {
            let packet = tokio::select! {
                () = cancel_token.cancelled() => break,
                packet = receiver.recv() => packet,
            };
            let Some(packet) = packet else {
                break;
            };
            let (raw, track) = match packet {
                PacketData::Audio { data, .. } => (data, &audio_track),
                PacketData::Video { data, .. } => (data, &video_track),
            };
            let mut raw = raw;
            let packet = match Packet::unmarshal(&mut raw) {
                Ok(packet) => packet,
                Err(error) => {
                    warn!(%error, "dropping malformed StreamHub RTP packet");
                    continue;
                }
            };
            if let Err(error) = track.write_rtp(&packet).await {
                debug!(%error, "stopped WebRTC RTP writer");
                break;
            }
        }
        cancel_token.cancel();
    });
}

pub async fn create_whep_session(
    offer_sdp: &str,
    packet_receiver: PacketDataReceiver,
    config: &WebRtcConfig,
) -> Result<PeerSession, WebRtcError> {
    let offer_sdp = validate_offer(offer_sdp, config)?.to_string();
    let peer = create_peer_connection(config).await?;
    let cancel_token = CancellationToken::new();
    bind_connection_lifecycle(&peer, cancel_token.clone());

    let audio_track = outgoing_track(RTPCodecType::Audio);
    let video_track = outgoing_track(RTPCodecType::Video);
    for track in [&audio_track, &video_track] {
        let sender = peer
            .add_track(Arc::clone(track) as Arc<dyn TrackLocal + Send + Sync>)
            .await
            .map_err(|error| WebRtcError::Negotiation(error.to_string()))?;
        let rtcp_cancel = cancel_token.clone();
        tokio::spawn(async move {
            let mut buffer = vec![0_u8; 1500];
            loop {
                tokio::select! {
                    () = rtcp_cancel.cancelled() => break,
                    result = sender.read(&mut buffer) => {
                        if result.is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }

    let answer_sdp = match negotiate_answer(&peer, &offer_sdp, config).await {
        Ok(answer) => answer,
        Err(error) => {
            cancel_token.cancel();
            let _ = peer.close().await;
            return Err(error);
        }
    };
    spawn_packet_writer(
        packet_receiver,
        audio_track,
        video_track,
        cancel_token.clone(),
    );
    Ok(PeerSession {
        answer_sdp,
        peer_connection: peer,
        cancel_token,
    })
}

#[cfg(test)]
mod tests {
    use anyhow::{anyhow, Result};
    use tokio::sync::mpsc;
    use webrtc::{
        media::Sample, track::track_local::track_local_static_sample::TrackLocalStaticSample,
    };

    use crate::flv::define::avc_packet_type;

    use super::*;

    async fn wait_for_connected(peer: &RTCPeerConnection, timeout: Duration) -> Result<()> {
        tokio::time::timeout(timeout, async {
            let mut interval = tokio::time::interval(Duration::from_millis(20));
            loop {
                match peer.connection_state() {
                    RTCPeerConnectionState::Connected => return Ok(()),
                    RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                        return Err(anyhow!("peer connection closed before connecting"));
                    }
                    _ => {
                        interval.tick().await;
                    }
                }
            }
        })
        .await
        .map_err(|_| anyhow!("peer connection timed out"))?
    }

    async fn wait_for_video_frame(
        receiver: &mut tokio::sync::mpsc::Receiver<FrameData>,
        packet_type: u8,
    ) -> Result<Bytes> {
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(frame) = receiver.recv().await {
                if let FrameData::Video { data, .. } = frame {
                    if data.get(1).copied() == Some(packet_type) {
                        return Ok(data);
                    }
                }
            }
            Err(anyhow!(
                "frame channel closed before the expected video frame"
            ))
        })
        .await
        .map_err(|_| anyhow!("timed out waiting for the expected video frame"))?
    }

    #[test]
    fn rejects_empty_and_oversized_sdp() {
        let config = WebRtcConfig {
            max_sdp_bytes: 4,
            ..WebRtcConfig::default()
        };
        assert!(matches!(
            validate_offer(" ", &config),
            Err(WebRtcError::EmptySdp)
        ));
        assert!(matches!(
            validate_offer("12345", &config),
            Err(WebRtcError::SdpTooLarge { max_bytes: 4 })
        ));
    }

    #[test]
    fn remote_sdp_filters_candidates_blocked_by_ssrf_policy() {
        const OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=candidate:1 1 UDP 2122260223 127.0.0.1 50000 typ host\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=sendonly\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=candidate:2 1 UDP 2122260222 192.168.1.20 50001 typ host\r\n\
a=candidate:3 1 UDP 2122260221 peer.local 50002 typ host\r\n\
a=candidate:4 1 UDP 2122260220 8.8.8.8 50003 typ srflx\r\n";

        let sanitized = sanitize_remote_sdp(
            OFFER,
            WebRtcConfig::default().max_sdp_bytes,
            &SsrfGuard::strict_policy(),
        )
        .expect("valid SDP should be sanitized");

        assert!(!sanitized.contains("127.0.0.1 50000"));
        assert!(!sanitized.contains("192.168.1.20 50001"));
        assert!(!sanitized.contains("peer.local 50002"));
        assert!(sanitized.contains("8.8.8.8 50003"));
    }

    #[test]
    fn remote_sdp_preserves_private_candidates_when_policy_allows_them() {
        const OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=sendonly\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=candidate:1 1 UDP 2122260223 192.168.1.20 50001 typ host\r\n";
        let guard = SsrfGuard::builder()
            .allow_private_network_targets(true)
            .build();

        let sanitized = sanitize_remote_sdp(OFFER, 256 * 1024, &guard)
            .expect("explicitly allowed private candidate should be retained");
        assert!(sanitized.contains("192.168.1.20 50001"));
    }

    #[test]
    fn remote_sdp_rejects_malformed_candidates() {
        const OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=candidate:too-short\r\n";

        assert!(matches!(
            sanitize_remote_sdp(OFFER, 256 * 1024, &SsrfGuard::strict_policy()),
            Err(WebRtcError::InvalidSdp(_))
        ));
    }

    #[test]
    fn whip_offer_rejects_h264_that_cannot_be_relayed_as_the_bridge_profile() {
        const HIGH_PROFILE_OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=sendonly\r\n\
a=rtpmap:96 H264/90000\r\n\
a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=640c1f\r\n";

        assert!(matches!(
            validate_whip_offer(
                HIGH_PROFILE_OFFER,
                &WebRtcConfig::default(),
                RtmpStreamMode::Default,
            ),
            Err(WebRtcError::IncompatibleWhipVideoCodec)
        ));
        assert!(matches!(
            validate_whip_offer(
                HIGH_PROFILE_OFFER,
                &WebRtcConfig::default(),
                RtmpStreamMode::AudioOnly,
            ),
            Err(WebRtcError::NoCompatibleWhipMedia)
        ));
    }

    #[test]
    fn bridge_h264_compatibility_requires_an_exact_profile_and_packetization_match() {
        assert!(h264_fmtp_is_bridge_compatible(
            "packetization-mode=1;profile-level-id=42e029"
        ));
        assert!(!h264_fmtp_is_bridge_compatible(
            "packetization-mode=1;profile-level-id=640c1f"
        ));
        assert!(!h264_fmtp_is_bridge_compatible(
            "packetization-mode=0;profile-level-id=42e01f"
        ));
        assert!(!h264_fmtp_is_bridge_compatible("packetization-mode=1"));
    }

    #[test]
    fn whip_offer_accepts_a_bridge_compatible_h264_alternative() {
        const MULTI_PROFILE_OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96 98\r\n\
a=sendonly\r\n\
a=rtpmap:96 H264/90000\r\n\
a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=640c1f\r\n\
a=rtpmap:98 H264/90000\r\n\
a=fmtp:98 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e029\r\n";

        assert!(validate_whip_offer(
            MULTI_PROFILE_OFFER,
            &WebRtcConfig::default(),
            RtmpStreamMode::Default,
        )
        .is_ok());
    }

    #[test]
    fn whip_offer_requires_a_sendable_bridge_codec() {
        const VP8_ONLY_OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=sendonly\r\n\
a=rtpmap:96 VP8/90000\r\n";
        const INACTIVE_H264_OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=inactive\r\n\
a=rtpmap:96 H264/90000\r\n\
a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n";

        for offer in [VP8_ONLY_OFFER, INACTIVE_H264_OFFER] {
            assert!(matches!(
                validate_whip_offer(offer, &WebRtcConfig::default(), RtmpStreamMode::Default,),
                Err(WebRtcError::NoCompatibleWhipMedia)
            ));
        }
    }

    #[test]
    fn whip_offer_accepts_sendable_opus_for_audio_mode() {
        const OPUS_OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=sendonly\r\n\
a=rtpmap:111 opus/48000/2\r\n";

        assert!(validate_whip_offer(
            OPUS_OFFER,
            &WebRtcConfig::default(),
            RtmpStreamMode::AudioOnly,
        )
        .is_ok());
    }

    #[test]
    fn builds_configured_ice_server() {
        let config = WebRtcConfig {
            ice_servers: vec![WebRtcIceServer {
                urls: vec!["turn:turn.example.com:3478?transport=udp".to_string()],
                username: "stream-user".to_string(),
                credential: "stream-password".to_string(),
            }],
            ..WebRtcConfig::default()
        };
        let rtc = peer_configuration(&config);
        assert_eq!(rtc.ice_servers.len(), 1);
        assert_eq!(rtc.ice_servers[0].urls, config.ice_servers[0].urls);
        assert_eq!(rtc.ice_servers[0].username, "stream-user");
        assert_eq!(rtc.ice_servers[0].credential, "stream-password");
    }

    #[test]
    fn whip_publish_mode_controls_accepted_tracks() {
        for (mode, audio_direction, video_direction) in [
            (
                RtmpStreamMode::Default,
                RTCRtpTransceiverDirection::Recvonly,
                RTCRtpTransceiverDirection::Recvonly,
            ),
            (
                RtmpStreamMode::VideoOnly,
                RTCRtpTransceiverDirection::Inactive,
                RTCRtpTransceiverDirection::Recvonly,
            ),
            (
                RtmpStreamMode::AudioOnly,
                RTCRtpTransceiverDirection::Recvonly,
                RTCRtpTransceiverDirection::Inactive,
            ),
        ] {
            assert_eq!(
                whip_transceiver_direction(mode, RTPCodecType::Audio),
                audio_direction
            );
            assert_eq!(
                whip_transceiver_direction(mode, RTPCodecType::Video),
                video_direction
            );
        }
    }

    #[test]
    fn outgoing_tracks_use_the_bridge_codec_capabilities() {
        assert_eq!(
            outgoing_track(RTPCodecType::Video).codec().sdp_fmtp_line,
            H264_BRIDGE_FMTP
        );
        assert_eq!(
            outgoing_track(RTPCodecType::Audio).codec().sdp_fmtp_line,
            OPUS_BRIDGE_FMTP
        );
    }

    #[tokio::test]
    async fn peer_factory_delivers_h264_track() -> Result<()> {
        let config = WebRtcConfig::default();
        let sender = create_peer_connection(&config).await?;
        let receiver = create_peer_connection(&config).await?;
        let track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_string(),
                clock_rate: 90_000,
                ..Default::default()
            },
            "video".to_string(),
            "synctv".to_string(),
        ));
        sender
            .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        let (packet_sender, mut packet_receiver) = mpsc::channel(1);
        receiver.on_track(Box::new(move |track, _, _| {
            let packet_sender = packet_sender.clone();
            Box::pin(async move {
                if let Ok((packet, _)) = track.read_rtp().await {
                    let _ = packet_sender.send(packet).await;
                }
            })
        }));

        let offer = sender.create_offer(None).await?;
        let offer = gather_local_description(&sender, offer, &config).await?;
        receiver
            .set_remote_description(RTCSessionDescription::offer(offer)?)
            .await?;
        let answer = receiver.create_answer(None).await?;
        let answer = gather_local_description(&receiver, answer, &config).await?;
        sender
            .set_remote_description(RTCSessionDescription::answer(answer)?)
            .await?;
        wait_for_connected(&sender, Duration::from_secs(5)).await?;
        wait_for_connected(&receiver, Duration::from_secs(5)).await?;

        let packet = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                track
                    .write_sample(&Sample {
                        data: Bytes::from_static(&[0x65, 0x88, 0x84, 0x21]),
                        duration: Duration::from_millis(33),
                        ..Default::default()
                    })
                    .await?;
                if let Ok(Some(packet)) =
                    tokio::time::timeout(Duration::from_millis(50), packet_receiver.recv()).await
                {
                    return Ok::<Packet, anyhow::Error>(packet);
                }
            }
        })
        .await
        .map_err(|_| anyhow!("timed out waiting for peer factory RTP"))??;
        assert!(!packet.payload.is_empty());
        sender.close().await?;
        receiver.close().await?;
        Ok(())
    }

    #[tokio::test]
    async fn whip_to_whep_forwards_rtp_and_emits_avc_frames() -> Result<()> {
        let config = WebRtcConfig {
            ice_gathering_timeout: Duration::from_secs(5),
            ssrf_guard: SsrfGuard::disabled(),
            ..WebRtcConfig::default()
        };
        let (frame_sender, mut frame_receiver) = mpsc::channel(256);
        let (whip_packet_sender, mut whip_packet_receiver) = mpsc::channel(16);
        let (whep_packet_sender, packet_receiver) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Some(packet) = whip_packet_receiver.recv().await {
                if whep_packet_sender.send(packet).await.is_err() {
                    break;
                }
            }
        });

        let publisher = create_peer_connection(&config).await?;
        let publisher_track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_string(),
                clock_rate: 90_000,
                ..Default::default()
            },
            "video".to_string(),
            "synctv".to_string(),
        ));
        let publisher_audio_track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_string(),
                clock_rate: 48_000,
                channels: 2,
                ..Default::default()
            },
            "audio".to_string(),
            "synctv".to_string(),
        ));
        publisher
            .add_transceiver_from_track(
                publisher_audio_track as Arc<dyn TrackLocal + Send + Sync>,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Sendonly,
                    send_encodings: Vec::new(),
                }),
            )
            .await?;
        let publisher_transceiver = publisher
            .add_transceiver_from_track(
                Arc::clone(&publisher_track) as Arc<dyn TrackLocal + Send + Sync>,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Sendonly,
                    send_encodings: Vec::new(),
                }),
            )
            .await?;
        let publisher_rtcp = publisher_transceiver.sender().await;
        tokio::spawn(async move {
            let mut buffer = vec![0_u8; 1_500];
            while publisher_rtcp.read(&mut buffer).await.is_ok() {}
        });
        let publisher_offer = publisher.create_offer(None).await?;
        let publisher_offer =
            gather_local_description(&publisher, publisher_offer, &config).await?;
        let whip = create_whip_session(
            &publisher_offer,
            FrameDataSender::bounded(frame_sender),
            whip_packet_sender,
            RtmpStreamMode::Default,
            &config,
        )
        .await?;
        publisher
            .set_remote_description(RTCSessionDescription::answer(whip.answer_sdp.clone())?)
            .await?;

        let viewer = create_peer_connection(&config).await?;
        for kind in [RTPCodecType::Audio, RTPCodecType::Video] {
            viewer
                .add_transceiver_from_kind(
                    kind,
                    Some(RTCRtpTransceiverInit {
                        direction: RTCRtpTransceiverDirection::Recvonly,
                        send_encodings: Vec::new(),
                    }),
                )
                .await?;
        }
        let (viewer_packet_sender, mut viewer_packet_receiver) = mpsc::channel(16);
        viewer.on_track(Box::new(move |track, _, _| {
            let viewer_packet_sender = viewer_packet_sender.clone();
            Box::pin(async move {
                if track.kind() != RTPCodecType::Video {
                    return;
                }
                tokio::spawn(async move {
                    let mut buffer = vec![0_u8; RTP_READ_BUFFER_SIZE];
                    while let Ok((packet, _)) = track.read(&mut buffer).await {
                        if viewer_packet_sender.send(packet).await.is_err() {
                            break;
                        }
                    }
                });
            })
        }));
        let viewer_offer = viewer.create_offer(None).await?;
        let viewer_offer = gather_local_description(&viewer, viewer_offer, &config).await?;
        let whep = create_whep_session(&viewer_offer, packet_receiver, &config).await?;
        viewer
            .set_remote_description(RTCSessionDescription::answer(whep.answer_sdp.clone())?)
            .await?;

        wait_for_connected(&publisher, Duration::from_secs(5)).await?;
        wait_for_connected(&viewer, Duration::from_secs(5)).await?;

        let viewer_idr = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                for payload in [
                    Bytes::from_static(&[0x67, 0x42, 0x00, 0x1f, 0xe5]),
                    Bytes::from_static(&[0x68, 0xce, 0x06, 0xe2]),
                    Bytes::from_static(&[0x65, 0x88, 0x84, 0x21]),
                ] {
                    publisher_track
                        .write_sample(&Sample {
                            data: payload,
                            duration: Duration::from_millis(33),
                            ..Default::default()
                        })
                        .await?;
                }

                if let Ok(Some(packet)) =
                    tokio::time::timeout(Duration::from_millis(100), viewer_packet_receiver.recv())
                        .await
                {
                    if packet
                        .payload
                        .first()
                        .is_some_and(|value| value & 0x1f == 5)
                    {
                        return Ok::<Packet, anyhow::Error>(packet);
                    }
                }
            }
        })
        .await
        .map_err(|_| anyhow!("timed out waiting for WHEP video RTP"))??;
        assert_eq!(
            viewer_idr.payload,
            Bytes::from_static(&[0x65, 0x88, 0x84, 0x21])
        );

        let sequence_header =
            wait_for_video_frame(&mut frame_receiver, avc_packet_type::AVC_SEQHDR).await?;
        assert_eq!(&sequence_header[..5], &[0x17, 0, 0, 0, 0]);
        let keyframe = wait_for_video_frame(&mut frame_receiver, avc_packet_type::AVC_NALU).await?;
        assert_eq!(&keyframe[..5], &[0x17, 1, 0, 0, 0]);

        whep.close().await?;
        whip.close().await?;
        viewer.close().await?;
        publisher.close().await?;
        Ok(())
    }
}
