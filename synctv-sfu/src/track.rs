//! Media track management for SFU
//!
//! This module handles complete WebRTC media track lifecycle including:
//! - Track creation and lifecycle management
//! - RTP packet reception and forwarding
//! - Simulcast quality layer handling
//! - Track statistics collection

use crate::types::{PeerId, TrackId};
use anyhow::Result;
use bytes::Bytes;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};
use webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver;
use webrtc::track::track_remote::TrackRemote;
use webrtc::util::marshal::MarshalSize;

/// Media track kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Audio,
    Video,
}

impl From<webrtc::rtp_transceiver::rtp_codec::RTPCodecType> for TrackKind {
    fn from(codec_type: webrtc::rtp_transceiver::rtp_codec::RTPCodecType) -> Self {
        match codec_type {
            webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio => Self::Audio,
            webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video => Self::Video,
            _ => {
                tracing::warn!(codec_type = ?codec_type, "Unknown codec type, defaulting to Video");
                Self::Video
            }
        }
    }
}

impl From<&str> for TrackKind {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "audio" => Self::Audio,
            _ => Self::Video,
        }
    }
}

/// Simulcast quality layer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QualityLayer {
    High,
    Medium,
    Low,
}

impl QualityLayer {
    /// Select quality layer based on available bandwidth
    /// bandwidth in kbps
    #[must_use] 
    pub const fn from_bandwidth(bandwidth_kbps: u32) -> Self {
        if bandwidth_kbps >= 2000 {
            Self::High // >= 2 Mbps
        } else if bandwidth_kbps >= 1000 {
            Self::Medium // >= 1 Mbps
        } else {
            Self::Low // < 1 Mbps
        }
    }

    /// Get the RID (restriction identifier) for this layer
    #[must_use]
    pub const fn rid(&self) -> &'static str {
        match self {
            Self::High => "h",
            Self::Medium => "m",
            Self::Low => "l",
        }
    }

    /// Parse a quality layer from a WebRTC RID (restriction identifier) string.
    ///
    /// Common RID conventions:
    /// - `"h"`, `"high"`, `"f"` (full) -> High
    /// - `"m"`, `"medium"`, `"mid"` -> Medium
    /// - `"l"`, `"low"`, `"q"` (quarter) -> Low
    ///
    /// Returns `None` for empty or unrecognized RIDs.
    #[must_use]
    pub fn from_rid(rid: &str) -> Option<Self> {
        match rid.to_lowercase().as_str() {
            "h" | "high" | "f" => Some(Self::High),
            "m" | "medium" | "mid" => Some(Self::Medium),
            "l" | "low" | "q" => Some(Self::Low),
            "" => None,
            _ => {
                tracing::debug!(rid = %rid, "Unknown simulcast RID, ignoring");
                None
            }
        }
    }

    /// Get expected bitrate for this layer (kbps)
    #[must_use] 
    pub const fn expected_bitrate(&self) -> u32 {
        match self {
            Self::High => 2500,    // 2.5 Mbps
            Self::Medium => 1200,  // 1.2 Mbps
            Self::Low => 500,      // 500 kbps
        }
    }

    /// Get spatial layer index (for SVC/Simulcast)
    #[must_use] 
    pub const fn spatial_layer(&self) -> u8 {
        match self {
            Self::High => 2,
            Self::Medium => 1,
            Self::Low => 0,
        }
    }
}

/// RTP packet with metadata for forwarding
#[derive(Debug, Clone)]
pub struct ForwardablePacket {
    /// RTP packet data
    pub data: Bytes,

    /// Source SSRC
    pub ssrc: u32,

    /// Sequence number
    pub sequence_number: u16,

    /// Timestamp
    pub timestamp: u32,

    /// Quality layer (for simulcast)
    pub quality_layer: Option<QualityLayer>,

    /// When packet was received
    pub received_at: Instant,

    /// Source track ID (identifies which published track this packet came from).
    /// Used by the subscriber output to route packets to the correct outbound track.
    pub source_track_id: Option<crate::types::TrackId>,

    /// Track kind (audio/video) for fast filtering without needing codec lookup
    pub kind: Option<TrackKind>,
}

/// Media track in the SFU
pub struct MediaTrack {
    /// Track ID
    pub id: TrackId,

    /// Owner peer ID
    pub peer_id: PeerId,

    /// Track kind (audio/video)
    pub kind: TrackKind,

    /// Remote track from WebRTC
    pub remote_track: Arc<TrackRemote>,

    /// RTP receiver
    pub receiver: Arc<RTCRtpReceiver>,

    /// Current active quality layer (for simulcast video)
    pub active_quality_layer: Arc<RwLock<Option<QualityLayer>>>,

    /// Cancellation token for signalling shutdown to the RTP reader task
    cancel_token: CancellationToken,

    /// Handle to the spawned RTP reader task (interior mutability for shared access)
    reader_handle: parking_lot::Mutex<Option<JoinHandle<()>>>,

    /// Track statistics
    stats: Arc<TrackStatsInner>,

    /// Packet forwarding channel (interior mutability for shared access)
    packet_tx: parking_lot::Mutex<Option<mpsc::Sender<ForwardablePacket>>>,
}

/// Internal track statistics with atomic counters
struct TrackStatsInner {
    packets_received: AtomicU64,
    bytes_received: AtomicU64,
    packets_sent: AtomicU64,
    bytes_sent: AtomicU64,
    packets_lost: AtomicU64,
    last_packet_time: RwLock<Option<Instant>>,
    /// Sliding window of (timestamp, bytes) for accurate bitrate calculation
    bitrate_window: parking_lot::Mutex<std::collections::VecDeque<(Instant, u64)>>,
}

impl MediaTrack {
    /// Create a new media track
    pub fn new(
        id: TrackId,
        peer_id: PeerId,
        remote_track: Arc<TrackRemote>,
        receiver: Arc<RTCRtpReceiver>,
    ) -> Self {
        let kind = TrackKind::from(remote_track.kind());

        info!(
            track_id = %id,
            peer_id = %peer_id,
            kind = ?kind,
            codec = %remote_track.codec().capability.mime_type,
            "Creating media track"
        );

        Self {
            id,
            peer_id,
            kind,
            remote_track,
            receiver,
            active_quality_layer: Arc::new(RwLock::new(None)),
            cancel_token: CancellationToken::new(),
            reader_handle: parking_lot::Mutex::new(None),
            stats: Arc::new(TrackStatsInner {
                packets_received: AtomicU64::new(0),
                bytes_received: AtomicU64::new(0),
                packets_sent: AtomicU64::new(0),
                bytes_sent: AtomicU64::new(0),
                packets_lost: AtomicU64::new(0),
                last_packet_time: RwLock::new(None),
                bitrate_window: parking_lot::Mutex::new(std::collections::VecDeque::new()),
            }),
            packet_tx: parking_lot::Mutex::new(None),
        }
    }

    /// RTP channel buffer size — limits memory for slow subscribers
    const RTP_CHANNEL_CAPACITY: usize = 256;

    /// Start reading RTP packets from the track
    ///
    /// Uses interior mutability so this can be called through `Arc<MediaTrack>`.
    /// The channel is bounded to prevent OOM from slow subscribers.
    pub async fn start_reading(
        &self,
    ) -> Result<mpsc::Receiver<ForwardablePacket>> {
        let (packet_tx, packet_rx) = mpsc::channel(Self::RTP_CHANNEL_CAPACITY);
        *self.packet_tx.lock() = Some(packet_tx.clone());

        let track = Arc::clone(&self.remote_track);
        let stats = Arc::clone(&self.stats);
        let track_id = self.id.clone();
        let track_kind = self.kind;
        let quality_layer = Arc::clone(&self.active_quality_layer);
        let cancel_token = self.cancel_token.clone();

        // Spawn RTP packet reading task and store the handle
        let handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 1500]; // MTU size

            loop {
                tokio::select! {
                    // Check for cancellation
                    () = cancel_token.cancelled() => {
                        debug!(track_id = %track_id, "Track cancelled, stopping RTP reader");
                        break;
                    }
                    // Read RTP packet
                    result = track.read(&mut buf) => {
                        match result {
                            Ok((rtp_packet, _attributes)) => {
                                // Update statistics
                                let packet_size = rtp_packet.header.marshal_size() + rtp_packet.payload.len();
                                stats.packets_received.fetch_add(1, Ordering::Relaxed);
                                stats.bytes_received.fetch_add(packet_size as u64, Ordering::Relaxed);
                                let now = Instant::now();
                                *stats.last_packet_time.write() = Some(now);

                                // Record in sliding window for bitrate calculation
                                {
                                    let mut window = stats.bitrate_window.lock();
                                    window.push_back((now, packet_size as u64));
                                    // Prune entries older than 2 seconds
                                    if let Some(cutoff) = now.checked_sub(Duration::from_secs(2)) {
                                        while let Some(&(t, _)) = window.front() {
                                            if t < cutoff { window.pop_front(); } else { break; }
                                        }
                                    }
                                }

                                // Create forwardable packet
                                let forwardable = ForwardablePacket {
                                    data: Bytes::copy_from_slice(&buf[..packet_size]),
                                    ssrc: rtp_packet.header.ssrc,
                                    sequence_number: rtp_packet.header.sequence_number,
                                    timestamp: rtp_packet.header.timestamp,
                                    quality_layer: *quality_layer.read(),
                                    received_at: Instant::now(),
                                    source_track_id: Some(track_id.clone()),
                                    kind: Some(track_kind),
                                };

                                // Forward packet to subscribers (drop on overflow to prevent OOM)
                                match packet_tx.try_send(forwardable) {
                                    Ok(()) => {}
                                    Err(mpsc::error::TrySendError::Full(_)) => {
                                        // Slow subscriber — drop packet to prevent OOM
                                        debug!(
                                            track_id = %track_id,
                                            "RTP channel full, dropping packet"
                                        );
                                    }
                                    Err(mpsc::error::TrySendError::Closed(_)) => {
                                        error!(
                                            track_id = %track_id,
                                            "RTP channel closed, stopping reader"
                                        );
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                error!(
                                    track_id = %track_id,
                                    error = %e,
                                    "Failed to read RTP packet"
                                );
                                break;
                            }
                        }
                    }
                }
            }

            info!(track_id = %track_id, "RTP reader stopped");
        });

        *self.reader_handle.lock() = Some(handle);

        Ok(packet_rx)
    }

    /// Get track SSRC (Synchronization Source)
    #[must_use] 
    pub fn ssrc(&self) -> u32 {
        self.remote_track.ssrc()
    }

    /// Get track codec
    #[must_use] 
    pub fn codec(&self) -> String {
        self.remote_track.codec().capability.mime_type
    }

    /// Set active quality layer for simulcast
    pub fn set_quality_layer(&self, layer: QualityLayer) {
        let mut current = self.active_quality_layer.write();
        if *current != Some(layer) {
            debug!(
                track_id = %self.id,
                old_layer = ?*current,
                new_layer = ?layer,
                "Switching quality layer"
            );
            *current = Some(layer);
        }
    }

    /// Get active quality layer
    #[must_use] 
    pub fn quality_layer(&self) -> Option<QualityLayer> {
        *self.active_quality_layer.read()
    }

    /// Check if track is video
    #[must_use] 
    pub fn is_video(&self) -> bool {
        self.kind == TrackKind::Video
    }

    /// Check if track is audio
    #[must_use] 
    pub fn is_audio(&self) -> bool {
        self.kind == TrackKind::Audio
    }

    /// Check if track is active (not cancelled)
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.cancel_token.is_cancelled()
    }

    /// Deactivate track: cancel the token and abort the reader task
    pub fn deactivate(&self) {
        self.cancel_token.cancel();
        if let Some(handle) = self.reader_handle.lock().take() {
            handle.abort();
        }
    }

    /// Get track statistics
    #[must_use] 
    pub fn get_stats(&self) -> TrackStats {
        let packets_received = self.stats.packets_received.load(Ordering::Relaxed);
        let bytes_received = self.stats.bytes_received.load(Ordering::Relaxed);
        let packets_sent = self.stats.packets_sent.load(Ordering::Relaxed);
        let bytes_sent = self.stats.bytes_sent.load(Ordering::Relaxed);
        let packets_lost = self.stats.packets_lost.load(Ordering::Relaxed);

        // Calculate bitrate from sliding window (bytes in last 2 seconds)
        let bitrate_kbps = {
            let now = Instant::now();
            let mut window = self.stats.bitrate_window.lock();
            // Prune old entries
            if let Some(cutoff) = now.checked_sub(Duration::from_secs(2)) {
                while let Some(&(t, _)) = window.front() {
                    if t < cutoff { window.pop_front(); } else { break; }
                }
            }
            let total_bytes: u64 = window.iter().map(|(_, b)| b).sum();
            // Bits per second over 2-second window, converted to kbps
            (total_bytes * 8 / 2 / 1000) as u32
        };

        TrackStats {
            track_id: self.id.as_str().to_string(),
            kind: self.kind,
            packets_received,
            bytes_received,
            packets_sent,
            bytes_sent,
            packets_lost,
            bitrate_kbps,
            quality_layer: self.quality_layer(),
        }
    }

    /// Update sent packet statistics
    pub fn record_sent_packet(&self, packet_size: usize) {
        self.stats.packets_sent.fetch_add(1, Ordering::Relaxed);
        self.stats.bytes_sent.fetch_add(packet_size as u64, Ordering::Relaxed);
    }

    /// Record packet loss
    pub fn record_packet_loss(&self, count: u64) {
        self.stats.packets_lost.fetch_add(count, Ordering::Relaxed);
    }
}

impl Drop for MediaTrack {
    fn drop(&mut self) {
        // Ensure the reader task is cancelled and aborted on drop
        self.cancel_token.cancel();
        if let Some(handle) = self.reader_handle.lock().take() {
            handle.abort();
        }
    }
}

/// Track statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackStats {
    pub track_id: String,
    pub kind: TrackKind,
    pub packets_received: u64,
    pub bytes_received: u64,
    pub packets_sent: u64,
    pub bytes_sent: u64,
    pub packets_lost: u64,
    pub bitrate_kbps: u32,
    pub quality_layer: Option<QualityLayer>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quality_layer_from_rid() {
        assert_eq!(QualityLayer::from_rid("h"), Some(QualityLayer::High));
        assert_eq!(QualityLayer::from_rid("m"), Some(QualityLayer::Medium));
        assert_eq!(QualityLayer::from_rid("l"), Some(QualityLayer::Low));
        assert_eq!(QualityLayer::from_rid("high"), Some(QualityLayer::High));
        assert_eq!(QualityLayer::from_rid("medium"), Some(QualityLayer::Medium));
        assert_eq!(QualityLayer::from_rid("low"), Some(QualityLayer::Low));
        assert_eq!(QualityLayer::from_rid("f"), Some(QualityLayer::High));
        assert_eq!(QualityLayer::from_rid("q"), Some(QualityLayer::Low));
        assert_eq!(QualityLayer::from_rid(""), None);
        assert_eq!(QualityLayer::from_rid("unknown"), None);
    }

    #[test]
    fn test_quality_layer_from_bandwidth() {
        assert_eq!(QualityLayer::from_bandwidth(3000), QualityLayer::High);
        assert_eq!(QualityLayer::from_bandwidth(2000), QualityLayer::High);
        assert_eq!(QualityLayer::from_bandwidth(1500), QualityLayer::Medium);
        assert_eq!(QualityLayer::from_bandwidth(1000), QualityLayer::Medium);
        assert_eq!(QualityLayer::from_bandwidth(500), QualityLayer::Low);
        assert_eq!(QualityLayer::from_bandwidth(0), QualityLayer::Low);
    }

    #[test]
    fn test_quality_layer_rid_roundtrip() {
        for layer in [QualityLayer::High, QualityLayer::Medium, QualityLayer::Low] {
            let rid = layer.rid();
            assert_eq!(QualityLayer::from_rid(rid), Some(layer));
        }
    }
}
