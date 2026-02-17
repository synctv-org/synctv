//! RTCP Feedback Handler
//!
//! This module processes RTCP statistics from WebRTC peer connections
//! to extract network quality metrics and feed them into the network monitor.
//!
//! ## Implementation
//!
//! Since the webrtc crate doesn't expose direct RTCP packet access, we use
//! RTCRtpReceiver.get_stats() to poll statistics periodically and derive:
//! - RTT (Round Trip Time) from RTCP reports
//! - Packet loss from cumulative counters
//! - Jitter from interarrival time variance
//! - Bandwidth from received bytes over time
//!
//! This integrates with the NetworkQualityMonitor to drive adaptive quality.

use crate::network_monitor::NetworkQualityMonitor;
use crate::peer::{PeerStats, SfuPeer};
use crate::types::PeerId;
use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver;
use webrtc::stats::StatsReportType;

/// Statistics polling interval
const STATS_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// RTCP feedback processor
pub struct RtcpHandler {
    /// Peer ID
    peer_id: PeerId,

    /// Network quality monitor for feeding RTT and packet loss metrics
    network_monitor: Arc<NetworkQualityMonitor>,

    /// SFU peer for stats updates
    peer: Arc<SfuPeer>,

    /// Cancellation token
    cancel_token: CancellationToken,

    /// Last statistics snapshot
    last_stats: Arc<parking_lot::Mutex<Option<StatsSnapshot>>>,
}

/// Statistics snapshot for delta calculation
#[derive(Debug, Clone)]
struct StatsSnapshot {
    timestamp: Instant,
    bytes_received: u64,
}

impl RtcpHandler {
    /// Create a new RTCP handler
    pub fn new(
        peer_id: PeerId,
        network_monitor: Arc<NetworkQualityMonitor>,
        peer: Arc<SfuPeer>,
    ) -> Self {
        Self {
            peer_id,
            network_monitor,
            peer,
            cancel_token: CancellationToken::new(),
            last_stats: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// Start RTCP processing loop with statistics polling
    pub async fn start(
        &self,
        pc: Arc<RTCPeerConnection>,
        receivers: Vec<Arc<RTCRtpReceiver>>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let peer_id = self.peer_id.clone();
        let network_monitor = Arc::clone(&self.network_monitor);
        let peer = Arc::clone(&self.peer);
        let cancel_token = self.cancel_token.clone();
        let last_stats = self.last_stats.clone();

        let handle = tokio::spawn(async move {
            info!(
                peer_id = %peer_id,
                receiver_count = receivers.len(),
                "Started RTCP statistics polling"
            );

            let mut interval = tokio::time::interval(STATS_POLL_INTERVAL);

            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        debug!(peer_id = %peer_id, "RTCP handler cancelled");
                        break;
                    }
                    _ = interval.tick() => {
                        // Poll statistics from peer connection
                        if let Err(e) = Self::poll_and_update_stats(
                            &peer_id,
                            &pc,
                            &receivers,
                            &network_monitor,
                            &peer,
                            &Arc::clone(&last_stats),
                        ).await {
                            warn!(
                                peer_id = %peer_id,
                                error = %e,
                                "Failed to poll RTCP statistics"
                            );
                        }
                    }
                }
            }

            info!(peer_id = %peer_id, "RTCP statistics polling stopped");
        });

        Ok(handle)
    }

    /// Poll statistics from RTCRtpReceivers and update monitors.
    ///
    /// Parses the `StatsReport` returned by `RTCPeerConnection::get_stats()` to
    /// extract real network measurements:
    /// - `RemoteInboundRTP` reports provide RTT and packet loss from RTCP receiver reports
    /// - `InboundRTP` reports provide packets received and bytes received
    /// - `CandidatePair` reports provide ICE-level RTT and bandwidth estimates
    async fn poll_and_update_stats(
        peer_id: &PeerId,
        pc: &RTCPeerConnection,
        _receivers: &[Arc<RTCRtpReceiver>],
        network_monitor: &NetworkQualityMonitor,
        peer: &SfuPeer,
        last_stats: &Arc<parking_lot::Mutex<Option<StatsSnapshot>>>,
    ) -> Result<()> {
        let stats_report = pc.get_stats().await;

        let mut total_packets_received: u64 = 0;
        let mut total_packets_lost: u64 = 0;
        let mut total_bytes_received: u64 = 0;
        let mut total_rtt_sum: f64 = 0.0;
        let mut rtt_count: u32 = 0;

        // Parse each report by matching on the StatsReportType enum variants
        for (_report_id, report) in &stats_report.reports {
            match report {
                // RemoteInboundRTP contains RTT and packet loss from RTCP receiver reports
                StatsReportType::RemoteInboundRTP(remote_inbound) => {
                    if let Some(rtt) = remote_inbound.round_trip_time {
                        // RTT is in seconds, convert to milliseconds
                        total_rtt_sum += rtt * 1000.0;
                        rtt_count += 1;
                    }
                    // Accumulate packet loss (may be negative in webrtc-rs, clamp to 0)
                    if remote_inbound.packets_lost > 0 {
                        total_packets_lost += remote_inbound.packets_lost as u64;
                    }
                    debug!(
                        peer_id = %peer_id,
                        rtt = ?remote_inbound.round_trip_time,
                        packets_lost = remote_inbound.packets_lost,
                        fraction_lost = remote_inbound.fraction_lost,
                        "Parsed RemoteInboundRTP stats"
                    );
                }
                // InboundRTP provides local receive counters
                StatsReportType::InboundRTP(inbound) => {
                    total_packets_received += inbound.packets_received;
                    total_bytes_received += inbound.bytes_received;
                    debug!(
                        peer_id = %peer_id,
                        packets_received = inbound.packets_received,
                        bytes_received = inbound.bytes_received,
                        nack_count = inbound.nack_count,
                        pli_count = ?inbound.pli_count,
                        fir_count = ?inbound.fir_count,
                        "Parsed InboundRTP stats"
                    );
                }
                // CandidatePair provides ICE-level RTT as a fallback and bandwidth
                StatsReportType::CandidatePair(pair) => {
                    // Use ICE candidate pair RTT as fallback when no RTCP RTT is available
                    if rtt_count == 0 && pair.current_round_trip_time > 0.0 {
                        total_rtt_sum += pair.current_round_trip_time * 1000.0;
                        rtt_count += 1;
                    }
                    debug!(
                        peer_id = %peer_id,
                        ice_rtt = pair.current_round_trip_time,
                        available_outgoing_bitrate = pair.available_outgoing_bitrate,
                        "Parsed CandidatePair stats"
                    );
                }
                _ => {}
            }
        }

        // Update RTT in network monitor from actual RTCP measurements
        if rtt_count > 0 {
            let avg_rtt_ms = (total_rtt_sum / f64::from(rtt_count)) as u32;
            network_monitor.update_rtt(peer_id, avg_rtt_ms);
            debug!(peer_id = %peer_id, rtt_ms = avg_rtt_ms, "Updated RTT from RTCP stats");
        }

        // Calculate bandwidth from byte count deltas
        let now = Instant::now();
        let current_snapshot = StatsSnapshot {
            timestamp: now,
            bytes_received: total_bytes_received,
        };

        let bandwidth_kbps = if let Some(prev) = last_stats.lock().as_ref() {
            let elapsed = now.duration_since(prev.timestamp).as_secs_f64();
            if elapsed > 0.0 {
                let bytes_delta = current_snapshot
                    .bytes_received
                    .saturating_sub(prev.bytes_received);
                ((bytes_delta as f64 * 8.0) / elapsed / 1000.0) as u32
            } else {
                peer.get_bandwidth()
            }
        } else {
            peer.get_bandwidth()
        };

        // Build peer stats from actual parsed values
        let existing_stats = peer.get_stats();
        let peer_stats = PeerStats {
            packets_received: total_packets_received,
            bytes_received: total_bytes_received,
            packets_sent: existing_stats.packets_sent,
            bytes_sent: existing_stats.bytes_sent,
            packet_loss_count: total_packets_lost,
            bandwidth_kbps,
        };

        // Feed real measurements to network quality monitor
        network_monitor.update_peer_stats(peer_id, &peer_stats, bandwidth_kbps);

        // Feed measured bytes into the peer's bandwidth estimator so it has
        // real data for exponential smoothing. Without this, the estimator
        // only has its initial 1 Mbps guess and never adapts.
        if let Some(prev) = last_stats.lock().as_ref() {
            let bytes_delta = current_snapshot
                .bytes_received
                .saturating_sub(prev.bytes_received);
            if bytes_delta > 0 {
                peer.record_received_bytes(bytes_delta as usize);
            }
        }

        // Trigger bandwidth estimation and quality layer switching.
        // This calls the peer's BandwidthEstimator which may change the
        // preferred QualityLayer (High/Medium/Low) based on measured bandwidth.
        let (estimated_bw, quality_change) = peer.update_bandwidth_estimation();
        if let Some(new_quality) = quality_change {
            info!(
                peer_id = %peer_id,
                estimated_bandwidth_kbps = estimated_bw,
                new_quality = ?new_quality,
                "RTCP: Quality layer switched based on bandwidth estimation"
            );
        }

        // Store current snapshot for next delta calculation
        *last_stats.lock() = Some(current_snapshot);

        Ok(())
    }

    /// Cancel RTCP processing
    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }
}

impl Drop for RtcpHandler {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtcp_handler_creation() {
        let peer_id = PeerId::from("test-peer");
        let network_monitor = Arc::new(NetworkQualityMonitor::new());
        let peer = Arc::new(SfuPeer::new(peer_id.clone()));
        let handler = RtcpHandler::new(peer_id.clone(), network_monitor, peer);
        assert_eq!(handler.peer_id.as_str(), "test-peer");
    }
}
