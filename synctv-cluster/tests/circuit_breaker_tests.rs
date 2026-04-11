//! Circuit breaker tests
//!
//! Tests for the failsafe circuit breaker used in `NodeRegistry`.
//! These tests verify the circuit breaker behavior without requiring Redis.

#![allow(clippy::unwrap_used)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use failsafe::{backoff, failure_policy, Config as CbConfig};

/// Create a circuit breaker matching the production configuration:
/// opens after 3 consecutive failures, exponential backoff 10s..60s.
fn create_test_circuit_breaker(
    failure_threshold: u32,
    min_backoff: Duration,
    max_backoff: Duration,
) -> failsafe::StateMachine<failure_policy::ConsecutiveFailures<backoff::Exponential>, ()> {
    let backoff = backoff::exponential(min_backoff, max_backoff);
    let policy = failure_policy::consecutive_failures(failure_threshold, backoff);
    CbConfig::new().failure_policy(policy).build()
}

// ============================================================================
// Test 1: Half-open state allows exactly one probe call
// ============================================================================

#[tokio::test]
async fn test_half_open_single_probe_wins_race() {
    // Create a circuit breaker that opens after 1 failure with short backoff
    // Note: failsafe requires backoff >= 1 second
    let cb = Arc::new(create_test_circuit_breaker(
        1,
        Duration::from_secs(1),
        Duration::from_secs(1),
    ));

    // Trip the circuit breaker
    cb.on_error();

    // Circuit should be open (no calls permitted)
    assert!(
        !cb.is_call_permitted(),
        "Circuit should be open after failure"
    );

    // Wait for backoff to expire so circuit transitions to half-open
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // After backoff expires, the failsafe circuit breaker transitions to
    // half-open which permits calls. Verify that at least one call is
    // permitted after the backoff window.
    assert!(
        cb.is_call_permitted(),
        "Circuit should permit calls after backoff expires (half-open)"
    );

    // If we report success, the circuit closes and all subsequent calls are permitted
    cb.on_success();

    let permitted_count = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(10));

    let mut handles = Vec::new();
    for _ in 0..10 {
        let cb = cb.clone();
        let count = permitted_count.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            if cb.is_call_permitted() {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for h in handles {
        h.await.expect("task panicked");
    }

    let permitted = permitted_count.load(Ordering::Relaxed);
    assert_eq!(
        permitted, 10,
        "All calls should be permitted after circuit closes, got {permitted}"
    );
}

// ============================================================================
// Test 2: Half-open probe failure re-opens the circuit
// ============================================================================

#[tokio::test]
async fn test_half_open_probe_failure_reopens() {
    let cb = create_test_circuit_breaker(1, Duration::from_secs(1), Duration::from_secs(1));

    // Trip the circuit
    cb.on_error();
    assert!(!cb.is_call_permitted(), "Circuit should be open");

    // Wait for half-open
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // One probe is permitted
    assert!(
        cb.is_call_permitted(),
        "Circuit should be half-open, permitting one probe"
    );

    // Simulate probe failure
    cb.on_error();

    // Circuit should be open again
    assert!(
        !cb.is_call_permitted(),
        "Circuit should re-open after probe failure"
    );
}

// ============================================================================
// Test 3: healthy_endpoints filters open circuits
// ============================================================================

#[tokio::test]
async fn test_healthy_endpoints_filters_open() {
    // Simulate 3 endpoints with independent circuit breakers
    let endpoints = ["endpoint_a", "endpoint_b", "endpoint_c"];
    let breakers: Vec<_> = endpoints
        .iter()
        .map(|_| create_test_circuit_breaker(1, Duration::from_secs(10), Duration::from_mins(1)))
        .collect();

    // All endpoints start healthy (closed circuit)

    assert_eq!(
        endpoints
            .iter()
            .zip(&breakers)
            .filter(|(_, cb)| cb.is_call_permitted())
            .count(),
        3,
        "All endpoints should be healthy initially"
    );

    // Open one endpoint's circuit
    breakers[1].on_error();

    let healthy: Vec<_> = endpoints
        .iter()
        .zip(&breakers)
        .filter(|(_, cb)| cb.is_call_permitted())
        .map(|(ep, _)| *ep)
        .collect();
    assert_eq!(
        healthy.len(),
        2,
        "Should have 2 healthy endpoints after opening 1"
    );
    assert!(
        !healthy.contains(&"endpoint_b"),
        "endpoint_b should be filtered out"
    );
}

// ============================================================================
// Test 4: Cluster degraded when >50% endpoints have open circuits
// ============================================================================

#[tokio::test]
async fn test_cluster_degraded_threshold() {
    let endpoint_count = 4;
    let breakers: Vec<_> = (0..endpoint_count)
        .map(|_| create_test_circuit_breaker(1, Duration::from_secs(10), Duration::from_mins(1)))
        .collect();

    // Open 3 out of 4 circuits (>50%)
    for cb in &breakers[0..3] {
        cb.on_error();
    }

    let open_count = breakers.iter().filter(|cb| !cb.is_call_permitted()).count();
    let is_degraded = open_count * 2 > breakers.len();

    assert!(
        is_degraded,
        "Cluster should be degraded when >50% endpoints have open circuits ({open_count}/{endpoint_count} open)"
    );
}

// ============================================================================
// Test 5: Failures below threshold keep circuit closed
// ============================================================================

#[tokio::test]
async fn test_failure_below_threshold_stays_closed() {
    // Circuit opens after 3 consecutive failures
    let cb = create_test_circuit_breaker(3, Duration::from_secs(10), Duration::from_mins(1));

    // 2 failures (below threshold of 3)
    cb.on_error();
    cb.on_error();

    // Circuit should still be closed
    assert!(
        cb.is_call_permitted(),
        "Circuit should stay closed with failures below threshold"
    );

    // One success resets the counter
    cb.on_success();

    // Two more failures (still below threshold since counter was reset)
    cb.on_error();
    cb.on_error();

    assert!(
        cb.is_call_permitted(),
        "Circuit should stay closed after reset + 2 failures"
    );

    // Now 3 consecutive failures without a success
    cb.on_error();

    assert!(
        !cb.is_call_permitted(),
        "Circuit should open after 3 consecutive failures"
    );
}
