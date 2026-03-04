//! gRPC circuit breaker for cross-node streaming
//!
//! Prevents cascading failures when nodes are unhealthy or unreachable.
//! Uses the failsafe crate for state management (Closed -> Open -> Half-Open).

use failsafe::{backoff, failure_policy, Config as CbConfig, StateMachine};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Circuit breaker state for a single gRPC endpoint.
type EndpointCircuitBreaker =
    StateMachine<failure_policy::ConsecutiveFailures<backoff::Exponential>, ()>;

/// Wrapper around `EndpointCircuitBreaker` that adds a `probe_in_flight` flag
/// to prevent the half-open race condition where multiple concurrent callers
/// all see `is_call_permitted() = true` simultaneously in half-open state.
struct EndpointBreaker {
    breaker: EndpointCircuitBreaker,
    /// Set to `true` when the circuit opens (via `on_error` reaching the
    /// failure threshold). Cleared to `false` when `on_success` is called
    /// (probe succeeded, circuit closes).
    ///
    /// Used to distinguish calls in `Closed` state (where `was_open` is
    /// `false` and the probe guard should not apply) from calls in
    /// `HalfOpen` state (where `was_open` is `true` and only one concurrent
    /// probe should be allowed).
    was_open: AtomicBool,
    /// `true` while a half-open probe call is currently in flight.
    ///
    /// Lifecycle:
    /// - Starts `false`.
    /// - Set to `true` (via `compare_exchange`) by the single caller that
    ///   wins the half-open probe race.
    /// - Reset to `false` when the probe completes via `on_success` or
    ///   `on_error`, allowing the next cooldown window to try again.
    probe_in_flight: AtomicBool,
}

impl EndpointBreaker {
    const fn new(breaker: EndpointCircuitBreaker) -> Self {
        Self {
            breaker,
            was_open: AtomicBool::new(false),
            probe_in_flight: AtomicBool::new(false),
        }
    }

    /// Returns `true` if this call is permitted.
    ///
    /// - `Closed` state (`was_open = false`): all concurrent calls pass
    ///   through normally — no probe guard is applied.
    /// - `Open` state (cooldown not yet elapsed): all calls are rejected.
    /// - `Half-open` state (`was_open = true`, cooldown elapsed): only the
    ///   **first** concurrent caller gets `true`; all others get `false`
    ///   until the probe completes via `on_success` or `on_error`.
    fn is_call_permitted(&self) -> bool {
        // Ask the underlying failsafe state machine.  This also drives the
        // Open -> HalfOpen transition when the cooldown expires.
        if !self.breaker.is_call_permitted() {
            return false;
        }

        // The underlying breaker returned `true`.  This happens in both
        // `Closed` and `HalfOpen` states.
        //
        // Only apply the single-probe guard when the circuit was previously
        // open (`was_open = true`).  In `Closed` state `was_open` is always
        // `false` so all concurrent calls proceed without restriction.
        if !self.was_open.load(Ordering::Acquire) {
            return true;
        }

        // Circuit is (or was) open and has now entered the half-open window.
        // Use compare_exchange so only the first concurrent caller is allowed
        // through as the probe; all others are rejected.
        self.probe_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn on_success(&self) {
        // Probe succeeded: circuit closes.  Clear both flags so subsequent
        // closed-state callers are never blocked by the half-open guard.
        self.probe_in_flight.store(false, Ordering::Release);
        self.was_open.store(false, Ordering::Release);
        self.breaker.on_success();
    }

    fn on_error(&self) {
        // Probe failed (or normal closed-state failure).  Reset the probe
        // slot so the next cooldown window can attempt a fresh probe.
        // Keep `was_open = true` if the underlying breaker just re-opened.
        self.probe_in_flight.store(false, Ordering::Release);
        self.breaker.on_error();
        // Reflect the open state: after on_error the failsafe breaker may
        // have transitioned to Open.  Check via is_call_permitted — if it
        // now returns false the circuit is open; set was_open accordingly.
        // We intentionally do NOT consume a probe slot here; this is purely
        // a state observation.
        if !self.breaker.is_call_permitted() {
            self.was_open.store(true, Ordering::Release);
        }
    }
}

/// Create a new circuit breaker for a gRPC endpoint.
///
/// Opens after 3 consecutive failures. Uses exponential backoff starting at
/// 5 seconds up to 30 seconds before allowing probe requests in half-open state.
fn create_endpoint_breaker() -> EndpointBreaker {
    let backoff = backoff::exponential(Duration::from_secs(5), Duration::from_secs(30));
    let policy = failure_policy::consecutive_failures(3, backoff);
    let breaker = CbConfig::new().failure_policy(policy).build();
    EndpointBreaker::new(breaker)
}

/// Circuit breaker registry for gRPC endpoints.
///
/// Tracks circuit breaker state per endpoint address to prevent hammering
/// unhealthy nodes during cross-node fan-out queries.
pub struct GrpcCircuitBreakerRegistry {
    /// Map of endpoint address -> circuit breaker
    breakers: Arc<RwLock<HashMap<String, EndpointBreaker>>>,
}

impl GrpcCircuitBreakerRegistry {
    /// Create a new circuit breaker registry
    pub fn new() -> Self {
        Self {
            breakers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Check if a call to the given endpoint is permitted by its circuit breaker.
    ///
    /// Returns `true` if the circuit is closed or half-open (allowing the single
    /// probe call). Returns `false` if the circuit is open or another probe is
    /// already in flight (half-open race protection).
    pub async fn is_call_permitted(&self, address: &str) -> bool {
        let breakers = self.breakers.read().await;
        if let Some(breaker) = breakers.get(address) {
            breaker.is_call_permitted()
        } else {
            // No breaker for this endpoint yet, allow the call
            true
        }
    }

    /// Record a successful call to the given endpoint.
    ///
    /// Resets the probe_in_flight flag and failure count; transitions from
    /// Half-Open -> Closed if applicable.
    pub async fn on_success(&self, address: &str) {
        let mut breakers = self.breakers.write().await;
        let breaker = breakers
            .entry(address.to_string())
            .or_insert_with(create_endpoint_breaker);
        breaker.on_success();
        debug!(address = %address, "gRPC circuit breaker: success recorded");
    }

    /// Record a failed call to the given endpoint.
    ///
    /// Resets the probe_in_flight flag and increments failure count; may
    /// transition from Closed -> Open or Half-Open -> Open if the failure
    /// threshold is reached.
    pub async fn on_error(&self, address: &str) {
        let mut breakers = self.breakers.write().await;
        let breaker = breakers
            .entry(address.to_string())
            .or_insert_with(create_endpoint_breaker);
        breaker.on_error();
        // `was_open` is set inside `on_error()` if the circuit just opened.
        let is_open = breaker.was_open.load(Ordering::Acquire);
        warn!(
            address = %address,
            is_open = is_open,
            "gRPC circuit breaker: failure recorded"
        );
    }

    /// Get the current state of the circuit breaker for an endpoint.
    ///
    /// Returns `true` if the circuit is open (unhealthy), `false` if closed/half-open.
    /// Note: this queries the underlying failsafe state without consuming a probe slot.
    pub async fn is_open(&self, address: &str) -> bool {
        let breakers = self.breakers.read().await;
        if let Some(breaker) = breakers.get(address) {
            !breaker.breaker.is_call_permitted()
        } else {
            false
        }
    }

    /// Remove a circuit breaker for a given endpoint.
    ///
    /// Useful when a node is deregistered from the cluster.
    pub async fn remove(&self, address: &str) {
        let mut breakers = self.breakers.write().await;
        breakers.remove(address);
        debug!(address = %address, "gRPC circuit breaker removed");
    }

    /// Get statistics about circuit breaker states.
    ///
    /// Returns (total_endpoints, open_circuits, closed_circuits).
    /// Queries the underlying failsafe state without consuming probe slots.
    pub async fn stats(&self) -> (usize, usize, usize) {
        let breakers = self.breakers.read().await;
        let total = breakers.len();
        let open = breakers
            .values()
            .filter(|b| !b.breaker.is_call_permitted())
            .count();
        let closed = total - open;
        (total, open, closed)
    }

    /// Check if the cluster is in a degraded state (mass failure).
    ///
    /// Returns `true` if more than 50% of known endpoints have open circuit
    /// breakers. When degraded, callers should skip fan-out to unhealthy nodes
    /// and return partial results immediately to avoid cascading timeouts.
    /// Queries the underlying failsafe state without consuming probe slots.
    pub async fn is_cluster_degraded(&self) -> bool {
        let breakers = self.breakers.read().await;
        let total = breakers.len();
        if total == 0 {
            return false;
        }
        let open = breakers
            .values()
            .filter(|b| !b.breaker.is_call_permitted())
            .count();
        open * 2 > total // more than 50% open
    }

    /// Get only the endpoints with closed (healthy) circuit breakers.
    ///
    /// Used during degraded mode to limit fan-out to nodes that are likely
    /// reachable, returning partial results rather than waiting for timeouts
    /// from unhealthy nodes.
    /// Queries the underlying failsafe state without consuming probe slots.
    pub async fn healthy_endpoints(&self) -> Vec<String> {
        let breakers = self.breakers.read().await;
        breakers
            .iter()
            .filter(|(_, b)| b.breaker.is_call_permitted())
            .map(|(addr, _)| addr.clone())
            .collect()
    }
}

impl Default for GrpcCircuitBreakerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_circuit_breaker_allows_initial_call() {
        let registry = GrpcCircuitBreakerRegistry::new();
        assert!(registry.is_call_permitted("node1:50051").await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_opens_after_failures() {
        let registry = GrpcCircuitBreakerRegistry::new();
        let addr = "node1:50051";

        // Record 3 consecutive failures
        registry.on_error(addr).await;
        registry.on_error(addr).await;
        registry.on_error(addr).await;

        // Circuit should be open now
        assert!(!registry.is_call_permitted(addr).await);
        assert!(registry.is_open(addr).await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_resets_on_success() {
        let registry = GrpcCircuitBreakerRegistry::new();
        let addr = "node1:50051";

        // Record 2 failures
        registry.on_error(addr).await;
        registry.on_error(addr).await;

        // Record a success (resets failure count)
        registry.on_success(addr).await;

        // Circuit should still be closed
        assert!(registry.is_call_permitted(addr).await);

        // Another 2 failures won't open it (threshold is 3)
        registry.on_error(addr).await;
        registry.on_error(addr).await;
        assert!(registry.is_call_permitted(addr).await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_remove() {
        let registry = GrpcCircuitBreakerRegistry::new();
        let addr = "node1:50051";

        registry.on_error(addr).await;
        registry.remove(addr).await;

        // After removal, should allow calls (no breaker registered)
        assert!(registry.is_call_permitted(addr).await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_stats() {
        let registry = GrpcCircuitBreakerRegistry::new();

        // Add two endpoints, open one
        registry.on_error("node1:50051").await;
        registry.on_error("node1:50051").await;
        registry.on_error("node1:50051").await; // Opens

        registry.on_error("node2:50051").await; // Stays closed (only 1 failure)

        let (total, open, closed) = registry.stats().await;
        assert_eq!(total, 2);
        assert_eq!(open, 1);
        assert_eq!(closed, 1);
    }
}
