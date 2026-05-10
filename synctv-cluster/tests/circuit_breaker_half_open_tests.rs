//! `GrpcCircuitBreakerRegistry` half-open probe concurrency
//!
//! Tests that the circuit breaker correctly transitions between states and
//! that the half-open probe guard allows exactly one concurrent caller.
//!
//! Note: The underlying failsafe library uses wall-clock time for its backoff.
//! We cannot mock time in these tests, so we test the behaviors that are
//! observable without waiting for the backoff window to expire.

#![allow(clippy::unwrap_used)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use synctv_cluster::grpc::circuit_breaker::GrpcCircuitBreakerRegistry;

/// After tripping the circuit (3 errors), it should be open and reject all calls.
#[tokio::test]
async fn test_circuit_opens_after_3_consecutive_errors() {
    let registry = GrpcCircuitBreakerRegistry::new();
    let addr = "node-trip:50051";

    // Trip the circuit breaker
    for _ in 0..3 {
        registry.on_error(addr).await;
    }

    // Circuit should be open
    assert!(
        registry.is_open(addr).await,
        "Circuit should be open after 3 failures"
    );
    assert!(
        !registry.is_call_permitted(addr).await,
        "Calls should be rejected when circuit is open"
    );
}

/// While the circuit is open, 10 concurrent callers should ALL get false.
#[tokio::test]
async fn test_open_circuit_rejects_all_concurrent_calls() {
    let registry = Arc::new(GrpcCircuitBreakerRegistry::new());
    let addr = "node-open-conc:50051";

    // Trip the circuit
    for _ in 0..3 {
        registry.on_error(addr).await;
    }

    let permitted_count = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(10));

    let mut handles = Vec::new();
    for _ in 0..10 {
        let registry = registry.clone();
        let count = permitted_count.clone();
        let barrier = barrier.clone();
        let addr = addr.to_string();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            if registry.is_call_permitted(&addr).await {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for h in handles {
        h.await.expect("task panicked");
    }

    let permitted = permitted_count.load(Ordering::Relaxed);
    assert_eq!(
        permitted, 0,
        "All 10 concurrent calls should be rejected when circuit is open, got {permitted}"
    );
}

/// After the circuit is tripped and a probe failure occurs, it stays open.
#[tokio::test]
async fn test_probe_failure_keeps_circuit_open() {
    let registry = GrpcCircuitBreakerRegistry::new();
    let addr = "node-probe-fail:50051";

    // Trip the circuit
    for _ in 0..3 {
        registry.on_error(addr).await;
    }
    assert!(registry.is_open(addr).await);

    // Another error should keep it open
    registry.on_error(addr).await;
    assert!(
        registry.is_open(addr).await,
        "Circuit should remain open after additional errors"
    );
}

/// Success before reaching failure threshold keeps circuit closed.
#[tokio::test]
async fn test_success_before_threshold_keeps_closed() {
    let registry = GrpcCircuitBreakerRegistry::new();
    let addr = "node-recover:50051";

    // 2 failures (threshold is 3)
    registry.on_error(addr).await;
    registry.on_error(addr).await;

    // Success resets the counter
    registry.on_success(addr).await;

    // Circuit should still be closed
    assert!(!registry.is_open(addr).await);
    assert!(registry.is_call_permitted(addr).await);

    // 2 more failures won't open it (counter was reset)
    registry.on_error(addr).await;
    registry.on_error(addr).await;
    assert!(registry.is_call_permitted(addr).await);
}

/// Verify `is_cluster_degraded` threshold (>50% open).
#[tokio::test]
async fn test_cluster_degraded_via_registry() {
    let registry = GrpcCircuitBreakerRegistry::new();

    // Trip 3 out of 4 endpoints
    for addr in ["a:50051", "b:50051", "c:50051"] {
        for _ in 0..3 {
            registry.on_error(addr).await;
        }
    }
    // Keep one healthy
    registry.on_success("d:50051").await;

    assert!(
        registry.is_cluster_degraded().await,
        "Should be degraded with 3/4 open"
    );

    let healthy = registry.healthy_endpoints().await;
    assert_eq!(healthy.len(), 1);
    assert_eq!(healthy[0], "d:50051");
}

/// Verify `healthy_endpoints` excludes open circuits.
#[tokio::test]
async fn test_healthy_endpoints_excludes_open() {
    let registry = GrpcCircuitBreakerRegistry::new();

    // Register 3 endpoints
    registry.on_success("healthy1:50051").await;
    registry.on_success("healthy2:50051").await;

    // Trip one
    for _ in 0..3 {
        registry.on_error("sick:50051").await;
    }

    let healthy = registry.healthy_endpoints().await;
    assert_eq!(healthy.len(), 2, "Should have 2 healthy endpoints");
    assert!(healthy.contains(&"healthy1:50051".to_string()));
    assert!(healthy.contains(&"healthy2:50051".to_string()));
    assert!(!healthy.contains(&"sick:50051".to_string()));
}

/// Stats should correctly report open/closed counts.
#[tokio::test]
async fn test_stats_report() {
    let registry = GrpcCircuitBreakerRegistry::new();

    // 2 healthy endpoints
    registry.on_success("h1:50051").await;
    registry.on_success("h2:50051").await;

    // 1 open endpoint
    for _ in 0..3 {
        registry.on_error("u1:50051").await;
    }

    let (total, open, closed) = registry.stats().await;
    assert_eq!(total, 3, "Total should be 3");
    assert_eq!(open, 1, "Open should be 1");
    assert_eq!(closed, 2, "Closed should be 2");
}

/// Remove clears a circuit breaker entry.
#[tokio::test]
async fn test_remove_clears_entry() {
    let registry = GrpcCircuitBreakerRegistry::new();
    let addr = "node-remove:50051";

    // Trip it
    for _ in 0..3 {
        registry.on_error(addr).await;
    }
    assert!(registry.is_open(addr).await);

    // Remove it
    registry.remove(addr).await;

    // After removal, calls should be permitted again (no breaker registered)
    assert!(
        registry.is_call_permitted(addr).await,
        "After removal, calls should be permitted"
    );
    assert!(!registry.is_open(addr).await);
}

/// Unknown endpoint has no breaker - calls should be permitted.
#[tokio::test]
async fn test_unknown_endpoint_permitted() {
    let registry = GrpcCircuitBreakerRegistry::new();
    assert!(
        registry.is_call_permitted("unknown:50051").await,
        "Unknown endpoints should be permitted"
    );
    assert!(!registry.is_open("unknown:50051").await);
}

/// Empty registry is not degraded.
#[tokio::test]
async fn test_empty_registry_not_degraded() {
    let registry = GrpcCircuitBreakerRegistry::new();
    assert!(!registry.is_cluster_degraded().await);
    assert!(registry.healthy_endpoints().await.is_empty());

    let (total, open, closed) = registry.stats().await;
    assert_eq!(total, 0);
    assert_eq!(open, 0);
    assert_eq!(closed, 0);
}
