//! gRPC circuit breaker for cross-node streaming
//!
//! Prevents cascading failures when nodes are unhealthy or unreachable.
//! Uses the failsafe crate for state management (Closed -> Open -> Half-Open).

use failsafe::{backoff, failure_policy, Config as CbConfig, StateMachine};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Circuit breaker state for a single gRPC endpoint.
type EndpointCircuitBreaker = StateMachine<failure_policy::ConsecutiveFailures<backoff::Exponential>, ()>;

/// Create a new circuit breaker for a gRPC endpoint.
///
/// Opens after 3 consecutive failures. Uses exponential backoff starting at
/// 5 seconds up to 30 seconds before allowing probe requests in half-open state.
fn create_endpoint_breaker() -> EndpointCircuitBreaker {
    let backoff = backoff::exponential(Duration::from_secs(5), Duration::from_secs(30));
    let policy = failure_policy::consecutive_failures(3, backoff);
    CbConfig::new().failure_policy(policy).build()
}

/// Circuit breaker registry for gRPC endpoints.
///
/// Tracks circuit breaker state per endpoint address to prevent hammering
/// unhealthy nodes during cross-node fan-out queries.
pub struct GrpcCircuitBreakerRegistry {
    /// Map of endpoint address -> circuit breaker
    breakers: Arc<RwLock<HashMap<String, EndpointCircuitBreaker>>>,
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
    /// Returns `true` if the circuit is closed or half-open (allowing a probe).
    /// Returns `false` if the circuit is open (endpoint is unhealthy).
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
    /// Resets failure count and transitions from Half-Open -> Closed if applicable.
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
    /// Increments failure count and may transition from Closed -> Open or
    /// Half-Open -> Open if the failure threshold is reached.
    pub async fn on_error(&self, address: &str) {
        let mut breakers = self.breakers.write().await;
        let breaker = breakers
            .entry(address.to_string())
            .or_insert_with(create_endpoint_breaker);
        breaker.on_error();
        warn!(
            address = %address,
            is_open = !breaker.is_call_permitted(),
            "gRPC circuit breaker: failure recorded"
        );
    }

    /// Get the current state of the circuit breaker for an endpoint.
    ///
    /// Returns `true` if the circuit is open (unhealthy), `false` if closed/half-open.
    pub async fn is_open(&self, address: &str) -> bool {
        let breakers = self.breakers.read().await;
        if let Some(breaker) = breakers.get(address) {
            !breaker.is_call_permitted()
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
    pub async fn stats(&self) -> (usize, usize, usize) {
        let breakers = self.breakers.read().await;
        let total = breakers.len();
        let open = breakers.values().filter(|b| !b.is_call_permitted()).count();
        let closed = total - open;
        (total, open, closed)
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
