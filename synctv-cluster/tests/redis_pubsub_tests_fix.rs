//! CL11: `redis_pubsub` tests fix
//!
//! Fixed tests that use the real `is_sentinel_failover_error` function from the
//! source module instead of duplicating the logic inline.

#![allow(clippy::unwrap_used)]
use synctv_cluster::sync::redis_pubsub::is_sentinel_failover_error;

// ============================================================================
// Test 1: READONLY is detected as failover
// ============================================================================

#[test]
fn test_real_is_sentinel_failover_error_readonly() {
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
fn test_real_is_sentinel_failover_error_loading() {
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
fn test_real_is_sentinel_failover_error_other() {
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

// ============================================================================
// Test 4: Nested anyhow context hides inner error from to_string()
// ============================================================================

#[test]
fn test_real_is_sentinel_failover_error_nested_context_hides() {
    // anyhow's Display (which to_string() calls) only shows the outermost context,
    // NOT the full chain. So is_sentinel_failover_error will NOT detect READONLY
    // when it is hidden behind a .context() wrapper.
    let inner = anyhow::anyhow!("READONLY You can't write against a read only replica.");
    let outer = inner.context("Failed to publish event");
    assert!(
        !is_sentinel_failover_error(&outer),
        "Context wrapping hides READONLY from to_string()-based detection"
    );

    // But if the context message itself contains READONLY, it IS detected
    let inner2 = anyhow::anyhow!("connection failed");
    let outer2 = inner2.context("READONLY: server switched to replica during failover");
    assert!(
        is_sentinel_failover_error(&outer2),
        "READONLY in context message should be detected"
    );
}

// ============================================================================
// Test 5: Case sensitivity check
// ============================================================================

#[test]
fn test_real_is_sentinel_failover_error_case_sensitive() {
    // The function checks for "READONLY" (uppercase) as Redis sends it
    let lowercase = anyhow::anyhow!("readonly you can't write");
    // Redis always uses uppercase READONLY, so this should also match
    // because the string repr contains "readonly" not "READONLY"
    // Actually "readonly" does NOT contain "READONLY", so it should NOT match
    assert!(
        !is_sentinel_failover_error(&lowercase),
        "Lowercase 'readonly' should not match (Redis sends uppercase)"
    );

    let mixed = anyhow::anyhow!("ReadOnly mode active");
    assert!(
        !is_sentinel_failover_error(&mixed),
        "Mixed case 'ReadOnly' should not match"
    );
}

// ============================================================================
// Test 6: Empty error message
// ============================================================================

#[test]
fn test_real_is_sentinel_failover_error_empty() {
    let err = anyhow::anyhow!("");
    assert!(
        !is_sentinel_failover_error(&err),
        "Empty error message should not be a failover error"
    );
}

// ============================================================================
// Test 7: Both patterns in same message
// ============================================================================

#[test]
fn test_real_is_sentinel_failover_error_both_patterns() {
    let err = anyhow::anyhow!("READONLY and LOADING simultaneously");
    assert!(
        is_sentinel_failover_error(&err),
        "Message containing both READONLY and LOADING should match"
    );
}
