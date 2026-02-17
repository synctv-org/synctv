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
            self.refill_tokens();

            // Try to consume tokens or calculate wait time.
            // All mutex access is scoped in this block so no MutexGuard
            // is held across the subsequent await point.
            let wait_result = {
                let mut tokens = self.tokens.lock();
                if *tokens >= packet_size_f64 {
                    *tokens -= packet_size_f64;
                    None // No wait needed, packet is allowed
                } else {
                    let bitrate_kbps = *self.target_bitrate_kbps.lock();
                    let bytes_per_sec = f64::from(bitrate_kbps) * 1000.0 / 8.0;

                    let tokens_needed = packet_size_f64 - *tokens;
                    let wait_time_secs = tokens_needed / bytes_per_sec;
                    let wait_time = Duration::from_secs_f64(wait_time_secs.max(0.001)); // Min 1ms
                    Some((wait_time, bitrate_kbps))
                }
            };

            match wait_result {
                None => return true,
                Some((wait_time, bitrate_kbps)) => {
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
        }
    }

    /// Refill token bucket based on elapsed time
    fn refill_tokens(&self) {
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

// NOTE: CongestionController was removed (SFU-23 Issue 1).
//
// The previous implementation had an incomplete BBR-like state machine:
// - Entering `Drain` on packet loss was a one-way trip (no recovery to ProbeBW/ProbeRTT)
// - The `pacing_gain` field was never used (shadowed by per-state constants)
// - No timer-driven state transitions existed
//
// The PacketPacer above handles pacing correctly via its token-bucket algorithm.
// Bandwidth estimation is handled by the RTCP handler and NetworkQualityMonitor.
// A proper congestion controller should be implemented when needed, with full
// BBR state transitions (Startup -> Drain -> ProbeBW <-> ProbeRTT).

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

    // CongestionController tests removed along with the dead code (SFU-23 Issue 1)
}
