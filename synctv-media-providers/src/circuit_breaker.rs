use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;

use tracing::{error, warn};

/// Number of consecutive failures before the circuit opens.
pub const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;

/// Seconds the circuit stays open before transitioning to half-open.
pub const CIRCUIT_BREAKER_TIMEOUT_SECS: i64 = 30;

/// Circuit breaker state per provider service.
#[derive(Default)]
pub struct CircuitBreaker {
    /// Number of consecutive failures (reset to 0 on success)
    consecutive_failures: AtomicU32,
    /// Unix timestamp (seconds) when the circuit was opened. -1 = never opened.
    opened_at: AtomicI64,
    // Whether a half-open probe request is currently in flight.
    half_open_probe_in_flight: AtomicBool,
}

fn unix_timestamp_secs() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(error) => {
            warn!(%error, "system clock is before Unix epoch");
            0
        }
    }
}

impl CircuitBreaker {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            consecutive_failures: AtomicU32::new(0),
            opened_at: AtomicI64::new(-1),
            half_open_probe_in_flight: AtomicBool::new(false),
        })
    }

    /// Check whether a request should be allowed through.
    pub fn allow_request(&self) -> bool {
        let failures = self.consecutive_failures.load(Ordering::SeqCst);
        if failures < CIRCUIT_BREAKER_THRESHOLD {
            return true;
        }

        let opened_at = self.opened_at.load(Ordering::SeqCst);
        if opened_at < 0 {
            return true;
        }

        let now = unix_timestamp_secs();
        if now.saturating_sub(opened_at) < CIRCUIT_BREAKER_TIMEOUT_SECS {
            return false;
        }

        self.half_open_probe_in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Record a successful request: reset failure counter.
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.opened_at.store(-1, Ordering::SeqCst);
        self.half_open_probe_in_flight
            .store(false, Ordering::SeqCst);
    }

    /// Record a failure: increment counter and open circuit if threshold reached.
    pub fn record_failure(&self, service: &str) {
        let prev = self.consecutive_failures.fetch_add(1, Ordering::SeqCst);
        let new_failures = prev + 1;
        if new_failures >= CIRCUIT_BREAKER_THRESHOLD {
            let now = unix_timestamp_secs();
            self.opened_at.store(now, Ordering::SeqCst);
            self.half_open_probe_in_flight
                .store(false, Ordering::SeqCst);
            if prev < CIRCUIT_BREAKER_THRESHOLD {
                error!(
                    service = %service,
                    threshold = CIRCUIT_BREAKER_THRESHOLD,
                    "Circuit breaker opened after {} consecutive failures",
                    CIRCUIT_BREAKER_THRESHOLD
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "circuit_breaker_tests.rs"]
mod tests;
