//! gRPC circuit breaker for cross-node streaming
//!
//! Prevents cascading failures when nodes are unhealthy or unreachable.
//! Uses the failsafe crate for state management (Closed -> Open -> Half-Open).

use failsafe::{backoff, failure_policy, Config as CbConfig, StateMachine};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Circuit breaker state for a single gRPC endpoint.
type EndpointCircuitBreaker =
    StateMachine<failure_policy::ConsecutiveFailures<backoff::Exponential>, ()>;

/// Maximum time (in seconds) a probe can remain in-flight before being
/// automatically reset. This prevents a permanently wedged circuit breaker
/// when the caller panics or drops without calling `on_success`/`on_error`.
const PROBE_STUCK_TIMEOUT_SECS: u64 = 60;

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
    /// - Automatically reset if stuck for longer than
    ///   `PROBE_STUCK_TIMEOUT_SECS` (panic/drop safety).
    probe_in_flight: AtomicBool,
    /// Unix timestamp (seconds) when `probe_in_flight` was last set to `true`.
    /// Used to detect and recover from stuck probes.
    probe_started_at: AtomicU64,
}

/// Get current Unix timestamp in seconds.
fn current_timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl EndpointBreaker {
    const fn new(breaker: EndpointCircuitBreaker) -> Self {
        Self {
            breaker,
            was_open: AtomicBool::new(false),
            probe_in_flight: AtomicBool::new(false),
            probe_started_at: AtomicU64::new(0),
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

        // Safety: reset a stuck probe if the caller panicked or dropped
        // without calling on_success/on_error, preventing permanent wedge.
        if self.probe_in_flight.load(Ordering::Acquire) {
            let started = self.probe_started_at.load(Ordering::Acquire);
            let now = current_timestamp_secs();
            if started > 0 && now.saturating_sub(started) > PROBE_STUCK_TIMEOUT_SECS {
                warn!(
                    elapsed_secs = now - started,
                    "Circuit breaker probe stuck for longer than {}s; resetting. \
                     Caller may have panicked without calling on_success/on_error.",
                    PROBE_STUCK_TIMEOUT_SECS
                );
                self.probe_in_flight.store(false, Ordering::Release);
            }
        }

        // Circuit is (or was) open and has now entered the half-open window.
        // Use compare_exchange so only the first concurrent caller is allowed
        // through as the probe; all others are rejected.
        let claimed = self
            .probe_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if claimed {
            self.probe_started_at
                .store(current_timestamp_secs(), Ordering::Release);
        }
        claimed
    }

    fn on_success(&self) {
        // Probe succeeded: circuit closes.  Clear all flags so subsequent
        // closed-state callers are never blocked by the half-open guard.
        self.probe_in_flight.store(false, Ordering::Release);
        self.probe_started_at.store(0, Ordering::Release);
        self.was_open.store(false, Ordering::Release);
        self.breaker.on_success();
    }

    fn on_error(&self) {
        // Probe failed (or normal closed-state failure).  Reset the probe
        // slot so the next cooldown window can attempt a fresh probe.
        // Keep `was_open = true` if the underlying breaker just re-opened.
        self.probe_in_flight.store(false, Ordering::Release);
        self.probe_started_at.store(0, Ordering::Release);
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
    ///
    /// This is a **read-only** check that inspects the `was_open` flag without
    /// calling the underlying `is_call_permitted()`, which would mutate state
    /// (transitioning Open -> HalfOpen when the cooldown expires). Use this
    /// method for monitoring, logging, and conditional logic where you do not
    /// want to trigger state transitions or consume a probe slot.
    pub async fn is_open(&self, address: &str) -> bool {
        let breakers = self.breakers.read().await;
        if let Some(breaker) = breakers.get(address) {
            // Read-only: was_open is set by on_error() when the circuit opens,
            // and cleared by on_success() when the circuit closes. This avoids
            // the side-effect of is_call_permitted() which drives Open -> HalfOpen.
            breaker.was_open.load(Ordering::Acquire)
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
    ///
    /// **Read-only**: uses the `was_open` flag instead of `is_call_permitted()`
    /// to avoid side effects (state transitions and probe slot consumption).
    pub async fn stats(&self) -> (usize, usize, usize) {
        let breakers = self.breakers.read().await;
        let total = breakers.len();
        let open = breakers
            .values()
            .filter(|b| b.was_open.load(Ordering::Acquire))
            .count();
        let closed = total - open;
        (total, open, closed)
    }

    /// Check if the cluster is in a degraded state (mass failure).
    ///
    /// Returns `true` if more than 50% of known endpoints have open circuit
    /// breakers. When degraded, callers should skip fan-out to unhealthy nodes
    /// and return partial results immediately to avoid cascading timeouts.
    ///
    /// **Read-only**: uses the `was_open` flag instead of `is_call_permitted()`
    /// to avoid side effects (state transitions and probe slot consumption).
    pub async fn is_cluster_degraded(&self) -> bool {
        let breakers = self.breakers.read().await;
        let total = breakers.len();
        if total == 0 {
            return false;
        }
        let open = breakers
            .values()
            .filter(|b| b.was_open.load(Ordering::Acquire))
            .count();
        open * 2 > total // more than 50% open
    }

    /// Get only the endpoints with closed (healthy) circuit breakers.
    ///
    /// Used during degraded mode to limit fan-out to nodes that are likely
    /// reachable, returning partial results rather than waiting for timeouts
    /// from unhealthy nodes.
    ///
    /// **Read-only**: uses the `was_open` flag instead of `is_call_permitted()`
    /// to avoid side effects (state transitions and probe slot consumption).
    pub async fn healthy_endpoints(&self) -> Vec<String> {
        let breakers = self.breakers.read().await;
        breakers
            .iter()
            .filter(|(_, b)| !b.was_open.load(Ordering::Acquire))
            .map(|(addr, _)| addr.clone())
            .collect()
    }

    /// Returns `true` only when the endpoint has a known circuit breaker entry
    /// and that breaker is currently open.
    ///
    /// Unknown endpoints return `false` so degraded-mode callers can still probe
    /// newly discovered nodes that have not accumulated breaker history yet.
    pub async fn is_endpoint_open_known(&self, address: &str) -> bool {
        let breakers = self.breakers.read().await;
        breakers
            .get(address)
            .is_some_and(|breaker| breaker.was_open.load(Ordering::Acquire))
    }

    /// Remove circuit breakers for endpoints that are no longer present in
    /// the active address set.
    ///
    /// Call this periodically (e.g., during fan-out after a node registry
    /// refresh) to prevent unbounded growth of the internal `HashMap` from
    /// departed nodes.
    pub async fn retain_only(&self, active_addresses: &std::collections::HashSet<String>) {
        let mut breakers = self.breakers.write().await;
        let before = breakers.len();
        breakers.retain(|addr, _| active_addresses.contains(addr));
        let pruned = before - breakers.len();
        if pruned > 0 {
            debug!(
                pruned = pruned,
                remaining = breakers.len(),
                "Pruned circuit breakers for departed nodes"
            );
        }
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

    #[tokio::test]
    async fn test_is_open_is_read_only() {
        let registry = GrpcCircuitBreakerRegistry::new();
        let addr = "node1:50051";

        // Open the circuit
        registry.on_error(addr).await;
        registry.on_error(addr).await;
        registry.on_error(addr).await;

        assert!(registry.is_open(addr).await);

        // Calling is_open multiple times should not change state (read-only)
        for _ in 0..10 {
            assert!(registry.is_open(addr).await);
        }

        // After success, circuit should close and is_open should return false
        registry.on_success(addr).await;
        assert!(!registry.is_open(addr).await);
    }

    #[tokio::test]
    async fn test_stats_is_read_only() {
        let registry = GrpcCircuitBreakerRegistry::new();

        // Open one circuit
        registry.on_error("node1:50051").await;
        registry.on_error("node1:50051").await;
        registry.on_error("node1:50051").await;

        // Calling stats many times should give consistent results (no side effects)
        let first = registry.stats().await;
        let second = registry.stats().await;
        assert_eq!(first, second, "stats() should be idempotent (read-only)");
    }

    #[tokio::test]
    async fn test_is_cluster_degraded_is_read_only() {
        let registry = GrpcCircuitBreakerRegistry::new();

        // Create two endpoints, open both
        for addr in &["node1:50051", "node2:50051"] {
            for _ in 0..3 {
                registry.on_error(addr).await;
            }
        }

        // Should be degraded (100% open)
        assert!(registry.is_cluster_degraded().await);
        // Calling again should give same result (read-only, no state transition)
        assert!(registry.is_cluster_degraded().await);
    }

    #[tokio::test]
    async fn test_healthy_endpoints_is_read_only() {
        let registry = GrpcCircuitBreakerRegistry::new();

        // node1 open, node2 closed
        for _ in 0..3 {
            registry.on_error("node1:50051").await;
        }
        registry.on_success("node2:50051").await;

        let first = registry.healthy_endpoints().await;
        let second = registry.healthy_endpoints().await;
        assert_eq!(first, second, "healthy_endpoints() should be idempotent");
        assert!(first.contains(&"node2:50051".to_string()));
        assert!(!first.contains(&"node1:50051".to_string()));
    }

    #[tokio::test]
    async fn test_is_endpoint_open_known_treats_unknown_endpoint_as_queryable() {
        let registry = GrpcCircuitBreakerRegistry::new();

        for _ in 0..3 {
            registry.on_error("node-open:50051").await;
        }

        assert!(
            registry.is_endpoint_open_known("node-open:50051").await,
            "known open breaker should be reported as open"
        );
        assert!(
            !registry.is_endpoint_open_known("node-unknown:50051").await,
            "unknown endpoints must remain queryable during degraded mode"
        );
    }

    #[tokio::test]
    async fn test_probe_in_flight_resets_on_success_and_error() {
        let breaker = create_endpoint_breaker();

        // Simulate half-open: set was_open and claim a probe
        breaker.was_open.store(true, Ordering::Release);
        assert!(breaker
            .probe_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok());

        // on_success should clear probe_in_flight and probe_started_at
        breaker.on_success();
        assert!(!breaker.probe_in_flight.load(Ordering::Acquire));
        assert_eq!(breaker.probe_started_at.load(Ordering::Acquire), 0);
        assert!(!breaker.was_open.load(Ordering::Acquire));

        // Simulate re-open: claim another probe
        breaker.was_open.store(true, Ordering::Release);
        assert!(breaker
            .probe_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok());

        // on_error should also clear flags
        breaker.on_error();
        assert!(!breaker.probe_in_flight.load(Ordering::Acquire));
        assert_eq!(breaker.probe_started_at.load(Ordering::Acquire), 0);
    }
}
