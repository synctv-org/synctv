//! Packet Pacing and Congestion Control
//!
//! This module implements packet pacing to prevent bursty traffic that can
//! cause network congestion and packet loss. It uses a token bucket algorithm
//! to smooth out RTP packet transmission.
//!
//! ## Features
//!
//! - Token bucket rate limiting
//! - Dynamic rate adjustment based on network conditions
//! - Per-peer pacing to prevent overwhelming slow subscribers
//! - Congestion window management (inspired by TCP BBR)

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::debug;

/// Packet pacer using token bucket algorithm
pub struct PacketPacer {
    /// Target bitrate in kbps
    target_bitrate_kbps: Arc<Mutex<u32>>,

    /// Token bucket state
    tokens: Arc<Mutex<f64>>,

    /// Maximum burst size (tokens)
    max_burst_tokens: f64,

    /// Last refill timestamp
    last_refill: Arc<Mutex<Instant>>,

    /// Minimum pacing interval (prevents CPU spinning)
    min_pacing_interval: Duration,
}

impl PacketPacer {
    /// Create a new packet pacer
    ///
    /// # Arguments
    /// * `target_bitrate_kbps` - Target bitrate in kilobits per second
    /// * `max_burst_ms` - Maximum burst duration in milliseconds
    pub fn new(target_bitrate_kbps: u32, max_burst_ms: u32) -> Self {
        // Calculate max burst in bytes
        let max_burst_bytes = (u64::from(target_bitrate_kbps) * u64::from(max_burst_ms) / 8) as f64;

        Self {
            target_bitrate_kbps: Arc::new(Mutex::new(target_bitrate_kbps)),
            tokens: Arc::new(Mutex::new(max_burst_bytes)),
            max_burst_tokens: max_burst_bytes,
            last_refill: Arc::new(Mutex::new(Instant::now())),
            min_pacing_interval: Duration::from_micros(100), // 100 microseconds
        }
    }

    /// Wait until enough tokens are available for sending a packet
    ///
    /// # Arguments
    /// * `packet_size` - Size of the packet in bytes
    ///
    /// # Returns
    /// Returns `true` if the packet can be sent, `false` if dropped due to overload
    pub async fn pace_packet(&self, packet_size: usize) -> bool {
        let packet_size_f64 = packet_size as f64;

        loop {
            // Refill tokens based on elapsed time
            self.refill_tokens().await;

            // Try to consume tokens
            let mut tokens = self.tokens.lock();
            if *tokens >= packet_size_f64 {
                *tokens -= packet_size_f64;
                drop(tokens);
                return true;
            }

            // Not enough tokens, calculate wait time
            let bitrate_kbps = *self.target_bitrate_kbps.lock();
            let bytes_per_sec = f64::from(bitrate_kbps) * 1000.0 / 8.0;

            let tokens_needed = packet_size_f64 - *tokens;
            let wait_time_secs = tokens_needed / bytes_per_sec;
            let wait_time = Duration::from_secs_f64(wait_time_secs.max(0.001)); // Min 1ms

            drop(tokens);

            // Check if wait time is reasonable
            if wait_time > Duration::from_secs(1) {
                // If we need to wait more than 1 second, drop the packet
                debug!(
                    packet_size = packet_size,
                    bitrate_kbps = bitrate_kbps,
                    "Dropping packet due to excessive pacing delay"
                );
                return false;
            }

            // Wait for tokens to become available
            sleep(wait_time.max(self.min_pacing_interval)).await;
        }
    }

    /// Refill token bucket based on elapsed time
    async fn refill_tokens(&self) {
        let now = Instant::now();
        let mut last_refill = self.last_refill.lock();
        let elapsed = now.duration_since(*last_refill);

        if elapsed >= self.min_pacing_interval {
            let bitrate_kbps = *self.target_bitrate_kbps.lock();
            let bytes_per_sec = f64::from(bitrate_kbps) * 1000.0 / 8.0;
            let tokens_to_add = bytes_per_sec * elapsed.as_secs_f64();

            let mut tokens = self.tokens.lock();
            *tokens = (*tokens + tokens_to_add).min(self.max_burst_tokens);
            *last_refill = now;
        }
    }

    /// Update target bitrate (for adaptive bitrate)
    pub fn set_target_bitrate(&self, bitrate_kbps: u32) {
        let mut target = self.target_bitrate_kbps.lock();
        if *target != bitrate_kbps {
            debug!(
                old_bitrate = *target,
                new_bitrate = bitrate_kbps,
                "Updated packet pacer target bitrate"
            );
            *target = bitrate_kbps;
        }
    }

    /// Get current target bitrate
    pub fn get_target_bitrate(&self) -> u32 {
        *self.target_bitrate_kbps.lock()
    }

    /// Get current token count (for debugging)
    pub fn get_token_count(&self) -> f64 {
        *self.tokens.lock()
    }
}

/// Congestion controller using a simplified BBR-like algorithm
pub struct CongestionController {
    /// Current estimated bandwidth in kbps
    estimated_bandwidth_kbps: Arc<Mutex<u32>>,

    /// Minimum observed RTT in milliseconds
    min_rtt_ms: Arc<Mutex<u32>>,

    /// Pacing gain for bandwidth probing
    pacing_gain: f64,

    /// Current congestion state
    state: Arc<Mutex<CongestionState>>,
}

/// Congestion control state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CongestionState {
    /// Startup: aggressively probe for bandwidth
    Startup,
    /// Drain: reduce sending rate after startup
    Drain,
    /// ProbeBW: maintain bandwidth and occasionally probe for more
    ProbeBW,
    /// ProbeRTT: reduce sending rate to measure minimum RTT
    ProbeRTT,
}

impl CongestionController {
    /// Create a new congestion controller
    pub fn new(initial_bandwidth_kbps: u32) -> Self {
        Self {
            estimated_bandwidth_kbps: Arc::new(Mutex::new(initial_bandwidth_kbps)),
            min_rtt_ms: Arc::new(Mutex::new(u32::MAX)),
            pacing_gain: 2.0, // Start with 2x gain for startup
            state: Arc::new(Mutex::new(CongestionState::Startup)),
        }
    }

    /// Update with new RTT measurement
    pub fn update_rtt(&self, rtt_ms: u32) {
        let mut min_rtt = self.min_rtt_ms.lock();
        if rtt_ms < *min_rtt {
            *min_rtt = rtt_ms;
            debug!(min_rtt_ms = rtt_ms, "Updated minimum RTT");
        }
    }

    /// Update with bandwidth measurement
    pub fn update_bandwidth(&self, bandwidth_kbps: u32) {
        let mut estimated = self.estimated_bandwidth_kbps.lock();
        let old_estimate = *estimated;

        // Use exponential moving average
        *estimated = ((old_estimate as f64 * 0.8) + (bandwidth_kbps as f64 * 0.2)) as u32;

        debug!(
            old_bandwidth = old_estimate,
            new_measurement = bandwidth_kbps,
            estimated_bandwidth = *estimated,
            "Updated bandwidth estimate"
        );
    }

    /// Update with packet loss indication
    pub fn update_loss(&self, loss_rate: f32) {
        if loss_rate > 0.05 {
            // Significant loss detected, reduce estimate
            let mut estimated = self.estimated_bandwidth_kbps.lock();
            let reduction = (*estimated as f32 * loss_rate.min(0.5)) as u32;
            *estimated = estimated.saturating_sub(reduction).max(100); // Min 100 kbps

            debug!(
                loss_rate = loss_rate,
                new_bandwidth = *estimated,
                "Reduced bandwidth estimate due to packet loss"
            );

            // Transition to drain state
            *self.state.lock() = CongestionState::Drain;
        }
    }

    /// Get pacing rate for packet pacer
    pub fn get_pacing_rate(&self) -> u32 {
        let estimated = *self.estimated_bandwidth_kbps.lock();
        let state = *self.state.lock();

        let pacing_gain = match state {
            CongestionState::Startup => 2.0,
            CongestionState::Drain => 0.75,
            CongestionState::ProbeBW => 1.25,
            CongestionState::ProbeRTT => 0.5,
        };

        ((estimated as f64) * pacing_gain) as u32
    }

    /// Get current estimated bandwidth
    pub fn get_estimated_bandwidth(&self) -> u32 {
        *self.estimated_bandwidth_kbps.lock()
    }

    /// Get minimum RTT
    pub fn get_min_rtt(&self) -> u32 {
        *self.min_rtt_ms.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_pacer_creation() {
        let pacer = PacketPacer::new(1000, 100); // 1 Mbps, 100ms burst
        assert_eq!(pacer.get_target_bitrate(), 1000);
    }

    #[test]
    fn test_packet_pacer_bitrate_update() {
        let pacer = PacketPacer::new(1000, 100);
        pacer.set_target_bitrate(2000);
        assert_eq!(pacer.get_target_bitrate(), 2000);
    }

    #[tokio::test]
    async fn test_packet_pacing() {
        let pacer = PacketPacer::new(1000, 100); // 1 Mbps
        let packet_size = 1000; // 1 KB

        // First packet should be allowed immediately (burst)
        let allowed = pacer.pace_packet(packet_size).await;
        assert!(allowed);
    }

    #[test]
    fn test_congestion_controller_creation() {
        let controller = CongestionController::new(1000);
        assert_eq!(controller.get_estimated_bandwidth(), 1000);
    }

    #[test]
    fn test_congestion_controller_rtt_update() {
        let controller = CongestionController::new(1000);
        controller.update_rtt(50);
        assert_eq!(controller.get_min_rtt(), 50);
    }

    #[test]
    fn test_congestion_controller_loss_reduction() {
        let controller = CongestionController::new(1000);
        controller.update_loss(0.1); // 10% loss
        let new_bandwidth = controller.get_estimated_bandwidth();
        assert!(new_bandwidth < 1000);
    }
}
