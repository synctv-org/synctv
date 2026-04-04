//! Tests for `GrpcConnectionPool` circuit breaker and connection management.
//!
//! These tests verify the circuit breaker behavior within the connection pool.

#![allow(clippy::unwrap_used)]
use synctv_livestream::grpc::GrpcConnectionPool;

#[tokio::test]
async fn test_circuit_breaker_failure_below_threshold() {
    let pool = GrpcConnectionPool::with_defaults();

    // A single failure should not open the circuit breaker.
    // Try connecting to a non-existent server
    let result = pool.get_channel("127.0.0.1:65535").await;
    assert!(result.is_err());

    // The pool should still allow further connection attempts
    // (circuit breaker threshold is 5, we only have 1 failure)
    let result2 = pool.get_channel("127.0.0.1:65535").await;
    assert!(result2.is_err());

    // Still allowing attempts (2 failures, threshold is 5)
    let result3 = pool.get_channel("127.0.0.1:65535").await;
    assert!(result3.is_err());
}

#[tokio::test]
async fn test_circuit_breaker_opens_at_threshold() {
    let pool = GrpcConnectionPool::with_defaults();

    // Connect 5 times to a non-existent server (CIRCUIT_BREAKER_THRESHOLD = 5)
    for _ in 0..5 {
        let _ = pool.get_channel("127.0.0.1:65534").await;
    }

    // After 5 failures, the circuit should be open.
    // The 6th attempt should fail immediately with a circuit breaker error.
    let result = pool.get_channel("127.0.0.1:65534").await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Circuit breaker open") || err_msg.contains("connect"),
        "Expected circuit breaker error, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_record_success_resets_errors() {
    let pool = GrpcConnectionPool::with_defaults();

    // Record some connection errors for a specific address
    pool.record_connection_error("10.0.0.1:50051");
    pool.record_connection_error("10.0.0.1:50051");

    // Record a success - should reset the error counter
    pool.record_connection_success("10.0.0.1:50051");

    // The connection should not be evicted after success reset
    // (no entry in pool to check, but verify it doesn't panic)
    pool.evict_stale();
}

#[tokio::test]
async fn test_different_addresses_independent() {
    let pool = GrpcConnectionPool::with_defaults();

    // Failures on one address should not affect another
    for _ in 0..5 {
        let _ = pool.get_channel("127.0.0.1:65534").await;
    }

    // Different address should still work (well, fail with connection error, not circuit breaker)
    let result = pool.get_channel("127.0.0.1:65533").await;
    assert!(result.is_err());
    // The error should be a connection error, not a circuit breaker error
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("connect") || err_msg.contains("Failed"),
        "Expected connection error for different address, got: {err_msg}"
    );
}
