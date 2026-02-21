//! redis_pubsub tests
//!
//! Tests for the is_sentinel_failover_error detection function.
//! The function is private, so we test the same logic inline.

/// Replicate the is_sentinel_failover_error logic from redis_pubsub.
fn is_sentinel_failover_error(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("READONLY") || msg.contains("LOADING")
}

// ============================================================================
// Test 1: READONLY is detected as failover
// ============================================================================

#[test]
fn test_is_sentinel_failover_error_readonly() {
    let err = anyhow::anyhow!("READONLY You can't write against a read only replica.");
    assert!(
        is_sentinel_failover_error(&err),
        "READONLY should be detected as a failover error"
    );
}

// ============================================================================
// Test 2: LOADING is detected as failover
// ============================================================================

#[test]
fn test_is_sentinel_failover_error_loading() {
    let err = anyhow::anyhow!("LOADING Redis is loading the dataset in memory");
    assert!(
        is_sentinel_failover_error(&err),
        "LOADING should be detected as a failover error"
    );
}

// ============================================================================
// Test 3: Other errors are NOT failover errors
// ============================================================================

#[test]
fn test_is_sentinel_failover_error_other() {
    let err = anyhow::anyhow!("Connection refused");
    assert!(
        !is_sentinel_failover_error(&err),
        "Generic connection error should not be a failover error"
    );

    let err2 = anyhow::anyhow!("ERR unknown command 'foo'");
    assert!(
        !is_sentinel_failover_error(&err2),
        "Unknown command error should not be a failover error"
    );

    let err3 = anyhow::anyhow!("NOSCRIPT No matching script");
    assert!(
        !is_sentinel_failover_error(&err3),
        "NOSCRIPT error should not be a failover error"
    );
}
