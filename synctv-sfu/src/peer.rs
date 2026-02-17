//! SFU Peer management
//!
//! This module handles:
//! - Peer connection state and lifecycle
//! - Bandwidth estimation with exponential smoothing
//! - Adaptive quality layer selection
//! - Track subscription management
//! - Peer statistics tracking

use crate::track::{ForwardablePacket, QualityLayer};
use crate::types::PeerId;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::debug;

/// Maximum number of samples to retain in the bandwidth estimator.
/// This prevents unbounded memory growth under high packet rates.
const MAX_BANDWIDTH_SAMPLES: usize = 1000;

/// Bandwidth estimator using exponential smoothing
struct BandwidthEstimator {
    /// Recent data samples: (timestamp, bytes)
    recent_bytes: VecDeque<(Instant, usize)>,

    /// Current estimated bandwidth in kbps
    current_bandwidth_kbps: u32,

    /// Last update time
    last_update: Instant,

    /// Smoothing factor (0.8 = 80% weight on previous value)
    smoothing_factor: f64,

    /// Time window for bandwidth calculation (1 second)
    window_duration_secs: u64,
}

impl BandwidthEstimator {
    fn new() -> Self {
        Self {
            recent_bytes: VecDeque::new(),
            current_bandwidth_kbps: 1000, // Start with 1 Mbps assumption
            last_update: Instant::now(),
            // Smoothing factor of 0.5 gives equal weight to the previous
            // estimate and the latest measurement. This adapts more quickly
            // to bandwidth changes than the prior value of 0.8, which was
            // too sluggish for real-time video and caused delayed quality
            // layer switching.
            smoothing_factor: 0.5,
            window_duration_secs: 1,
        }
    }

    /// Record received bytes
    fn record_bytes(&mut self, bytes: usize) {
        let now = Instant::now();

        // Enforce capacity limit to prevent unbounded memory growth
        if self.recent_bytes.len() >= MAX_BANDWIDTH_SAMPLES {
            self.recent_bytes.pop_front();
        }

        self.recent_bytes.push_back((now, bytes));

        // Remove samples outside the window
        let window = std::time::Duration::from_secs(self.window_duration_secs);
        let cutoff = now.checked_sub(window).unwrap_or(now);
        while let Some(&(timestamp, _)) = self.recent_bytes.front() {
            if timestamp < cutoff {
                self.recent_bytes.pop_front();
            } else {
                break;
            }
        }
    }

    /// Estimate current bandwidth using exponential smoothing
    fn estimate(&mut self) -> u32 {
        let now = Instant::now();

        // Calculate total bytes in the window
        let total_bytes: usize = self.recent_bytes.iter().map(|(_, bytes)| bytes).sum();

        // Calculate instantaneous bandwidth
        let duration_secs = self.window_duration_secs as f64;
        let new_bandwidth_kbps = ((total_bytes * 8) as f64 / duration_secs / 1000.0) as u32;

        // Apply exponential smoothing: new_estimate = α * old + (1-α) * new
        self.current_bandwidth_kbps = self.smoothing_factor.mul_add(f64::from(self.current_bandwidth_kbps), (1.0 - self.smoothing_factor) * f64::from(new_bandwidth_kbps)) as u32;

        self.last_update = now;
        self.current_bandwidth_kbps
    }

    /// Get current bandwidth estimate without updating
    const fn get_current(&self) -> u32 {
        self.current_bandwidth_kbps
    }
}

/// Capacity of per-peer packet forwarding channel.
/// Bounded to prevent OOM from slow subscribers.
const PEER_PACKET_CHANNEL_CAPACITY: usize = 256;

/// SFU Peer - represents a connected peer in SFU mode
pub struct SfuPeer {
    /// Peer ID
    pub id: PeerId,

    /// Bandwidth estimator
    bandwidth_estimator: Arc<RwLock<BandwidthEstimator>>,

    /// Current preferred quality layer for this peer
    preferred_quality: Arc<RwLock<QualityLayer>>,

    /// Peer statistics
    stats: Arc<RwLock<PeerStats>>,

    /// Sender for forwarding RTP packets to this peer.
    /// The forwarding loop writes packets here; the WebRTC output path reads them.
    packet_tx: mpsc::Sender<ForwardablePacket>,

    /// Receiver for forwarded RTP packets (taken once by the output task).
    packet_rx: parking_lot::Mutex<Option<mpsc::Receiver<ForwardablePacket>>>,
}

impl SfuPeer {
    /// Create a new SFU peer
    #[must_use]
    pub fn new(id: PeerId) -> Self {
        let (packet_tx, packet_rx) = mpsc::channel(PEER_PACKET_CHANNEL_CAPACITY);
        Self {
            id,
            bandwidth_estimator: Arc::new(RwLock::new(BandwidthEstimator::new())),
            preferred_quality: Arc::new(RwLock::new(QualityLayer::Medium)),
            stats: Arc::new(RwLock::new(PeerStats::default())),
            packet_tx,
            packet_rx: parking_lot::Mutex::new(Some(packet_rx)),
        }
    }

    /// Try to send a forwarded RTP packet to this peer.
    /// Returns false if the channel is full (slow subscriber) or closed.
    /// Increments packet_loss_count in stats when a packet is dropped due
    /// to a full channel, providing visibility into slow-subscriber backpressure.
    pub fn try_forward_packet(&self, packet: &ForwardablePacket) -> bool {
        match self.packet_tx.try_send(packet.clone()) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.stats.write().packet_loss_count += 1;
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.stats.write().packet_loss_count += 1;
                false
            }
        }
    }

    /// Take the packet receiver (can only be called once).
    /// Used by the WebRTC output task to read forwarded packets.
    pub fn take_packet_receiver(&self) -> Option<mpsc::Receiver<ForwardablePacket>> {
        self.packet_rx.lock().take()
    }

    /// Record received bytes and update bandwidth estimate
    pub fn record_received_bytes(&self, bytes: usize) {
        let mut estimator = self.bandwidth_estimator.write();
        estimator.record_bytes(bytes);

        // Update stats
        let mut stats = self.stats.write();
        stats.packets_received += 1;
        stats.bytes_received += bytes as u64;
    }

    /// Record sent bytes
    pub fn record_sent_bytes(&self, bytes: usize) {
        let mut stats = self.stats.write();
        stats.packets_sent += 1;
        stats.bytes_sent += bytes as u64;
    }

    /// Update bandwidth estimation and potentially adjust quality
    #[must_use] 
    pub fn update_bandwidth_estimation(&self) -> (u32, Option<QualityLayer>) {
        let estimated_bandwidth = self.bandwidth_estimator.write().estimate();
        let old_quality = *self.preferred_quality.read();
        let new_quality = QualityLayer::from_bandwidth(estimated_bandwidth);

        // Only change quality if there's a significant bandwidth change
        // or if the quality layer should change
        if new_quality != old_quality {
            *self.preferred_quality.write() = new_quality;
            return (estimated_bandwidth, Some(new_quality));
        }

        (estimated_bandwidth, None)
    }

    /// Get current bandwidth estimate
    #[must_use] 
    pub fn get_bandwidth(&self) -> u32 {
        self.bandwidth_estimator.read().get_current()
    }

    /// Get preferred quality layer
    #[must_use] 
    pub fn get_preferred_quality(&self) -> QualityLayer {
        *self.preferred_quality.read()
    }

    /// Set preferred quality layer manually
    pub fn set_preferred_quality(&self, quality: QualityLayer) {
        *self.preferred_quality.write() = quality;
    }

    /// Get peer statistics
    #[must_use] 
    pub fn get_stats(&self) -> PeerStats {
        self.stats.read().clone()
    }

    /// Reset statistics
    pub fn reset_stats(&self) {
        *self.stats.write() = PeerStats::default();
    }
}

impl Drop for SfuPeer {
    fn drop(&mut self) {
        debug!(
            peer_id = %self.id,
            bandwidth_kbps = self.bandwidth_estimator.read().get_current(),
            "SfuPeer dropped"
        );
        // The mpsc::Sender (packet_tx) is dropped here, which closes the channel
        // and signals the output task to stop reading. No explicit cleanup needed.
    }
}

/// Peer statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerStats {
    /// Number of packets received from this peer
    pub packets_received: u64,

    /// Number of bytes received from this peer
    pub bytes_received: u64,

    /// Number of packets sent to this peer
    pub packets_sent: u64,

    /// Number of bytes sent to this peer
    pub bytes_sent: u64,

    /// Number of packet loss events
    pub packet_loss_count: u64,

    /// Current estimated bandwidth in kbps
    pub bandwidth_kbps: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_creation() {
        let peer = SfuPeer::new(PeerId::from("test-peer"));
        assert_eq!(peer.get_bandwidth(), 1000); // Default 1 Mbps
        assert_eq!(peer.get_preferred_quality(), QualityLayer::Medium);
    }

    #[test]
    fn test_bandwidth_estimation() {
        let peer = SfuPeer::new(PeerId::from("test-peer"));

        // Record some bytes
        peer.record_received_bytes(10000);
        peer.record_received_bytes(10000);
        peer.record_received_bytes(10000);

        // Update estimation
        let (bandwidth, _) = peer.update_bandwidth_estimation();

        // Should have some bandwidth estimate
        assert!(bandwidth > 0);
    }

    #[test]
    fn test_quality_adaptation() {
        let peer = SfuPeer::new(PeerId::from("test-peer"));

        // Set quality manually
        peer.set_preferred_quality(QualityLayer::High);
        assert_eq!(peer.get_preferred_quality(), QualityLayer::High);

        peer.set_preferred_quality(QualityLayer::Low);
        assert_eq!(peer.get_preferred_quality(), QualityLayer::Low);
    }

    #[test]
    fn test_stats() {
        let peer = SfuPeer::new(PeerId::from("test-peer"));

        peer.record_received_bytes(1000);
        peer.record_sent_bytes(500);

        let stats = peer.get_stats();
        assert_eq!(stats.packets_received, 1);
        assert_eq!(stats.bytes_received, 1000);
        assert_eq!(stats.packets_sent, 1);
        assert_eq!(stats.bytes_sent, 500);

        // Reset stats
        peer.reset_stats();
        let stats = peer.get_stats();
        assert_eq!(stats.packets_received, 0);
        assert_eq!(stats.bytes_received, 0);
    }
}
