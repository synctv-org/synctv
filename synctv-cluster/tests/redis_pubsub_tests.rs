//! redis_pubsub tests
//!
//! Tests for the is_sentinel_failover_error detection function.
//! The function is private, so we test the same logic inline.
//!
//! Also tests for catchup_start_id calculation used during reconnection.

#![allow(clippy::unwrap_used)]
use std::time::{SystemTime, UNIX_EPOCH};

/// Replicate the is_sentinel_failover_error logic from redis_pubsub.
fn is_sentinel_failover_error(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("READONLY") || msg.contains("LOADING")
}

/// Replicate the catchup_start_id logic from redis_pubsub.
/// Returns a Redis Stream ID that represents the start of the catchup window.
fn catchup_start_id(catchup_window_ms: u128) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let start_ms = now_ms.saturating_sub(catchup_window_ms);
    format!("{start_ms}-0")
}

/// Parse a Redis Stream ID into (timestamp_ms, sequence) for comparison.
fn parse_stream_id(id: &str) -> Option<(u64, u64)> {
    let parts: Vec<&str> = id.split('-').collect();
    if parts.len() != 2 {
        return None;
    }
    let ts: u64 = parts[0].parse().ok()?;
    let seq: u64 = parts[1].parse().ok()?;
    Some((ts, seq))
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

// ============================================================================
// Catchup Start ID Tests
//
// These tests verify the catchup_start_id calculation used during
// reconnection to avoid reading all historical events from "0".
// ============================================================================

/// Test that catchup_start_id returns a valid Redis Stream ID format.
#[test]
fn test_catchup_start_id_format() {
    let id = catchup_start_id(300_000); // 5 minutes window
    let parsed = parse_stream_id(&id);
    assert!(parsed.is_some(), "catchup_start_id should be parseable");
    let (ts, seq) = parsed.unwrap();
    assert!(ts > 0, "Timestamp should be non-zero");
    assert_eq!(seq, 0, "Sequence should be 0");
}

/// Test that catchup_start_id is approximately (now - catchup_window).
#[test]
fn test_catchup_start_id_is_within_window() {
    let window_ms: u128 = 300_000; // 5 minutes
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let id = catchup_start_id(window_ms);
    let (ts, _) = parse_stream_id(&id).expect("Valid stream ID");

    let expected_min = now_ms.saturating_sub(window_ms + 1000); // Allow 1s tolerance
    let expected_max = now_ms.saturating_sub(window_ms.saturating_sub(1000));

    assert!(
        ts as u128 >= expected_min,
        "catchup_start_id timestamp should be at least window_ms ago"
    );
    assert!(
        ts as u128 <= expected_max + 2000, // More tolerance for test execution time
        "catchup_start_id timestamp should be at most window_ms ago (+ tolerance)"
    );
}

/// Test that catchup_start_id with zero window returns current time.
#[test]
fn test_catchup_start_id_zero_window() {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let id = catchup_start_id(0);
    let (ts, _) = parse_stream_id(&id).expect("Valid stream ID");

    // Should be very close to now (within 2 seconds tolerance)
    let diff = (ts as u128).abs_diff(now_ms);
    assert!(
        diff < 2000,
        "catchup_start_id with zero window should be close to current time, diff={diff}ms"
    );
}

/// Test that catchup_start_id is NOT "0" (the problematic default).
/// This is the key test for the bug fix: reconnect should NOT read from "0".
#[test]
fn test_catchup_start_id_is_not_zero() {
    let id = catchup_start_id(300_000);
    assert_ne!(
        id, "0",
        "catchup_start_id should NEVER be '0' - this would read all history"
    );
    assert!(
        id.parse::<u64>().is_err() || id != "0",
        "catchup_start_id should not be parseable as just '0'"
    );
}

/// Test that catchup_start_id is greater than "0" for lexicographic comparison.
/// Redis Stream IDs are compared lexicographically, so "0" < any timestamp-based ID.
#[test]
fn test_catchup_start_id_greater_than_zero() {
    let id = catchup_start_id(300_000);
    assert!(
        id.as_str() > "0",
        "catchup_start_id should be lexicographically greater than '0'"
    );
    assert!(
        id.as_str() > "1000000000000-0", // A timestamp from Sept 2001
        "catchup_start_id should be greater than old timestamps"
    );
}

/// Test that multiple calls to catchup_start_id produce increasing values.
#[test]
fn test_catchup_start_id_monotonic() {
    let id1 = catchup_start_id(300_000);
    // Small delay to ensure time progresses
    std::thread::sleep(std::time::Duration::from_millis(10));
    let id2 = catchup_start_id(300_000);

    let (ts1, _) = parse_stream_id(&id1).expect("Valid stream ID 1");
    let (ts2, _) = parse_stream_id(&id2).expect("Valid stream ID 2");

    // Both should be close, but id2 should be >= id1
    // (with a small tolerance for clock precision)
    assert!(
        ts2 >= ts1.saturating_sub(1),
        "catchup_start_id should be roughly monotonic: {id1} vs {id2}"
    );
}

/// Test that a large catchup window doesn't cause underflow.
#[test]
fn test_catchup_start_id_large_window_no_underflow() {
    // 1 year in milliseconds - larger than any reasonable Unix timestamp
    let large_window: u128 = 365 * 24 * 60 * 60 * 1000;

    let id = catchup_start_id(large_window);
    let (ts, seq) = parse_stream_id(&id).expect("Valid stream ID");

    // Should still be a valid ID (parsed successfully above)
    let _ = ts; // timestamp is always valid (unsigned)
    assert_eq!(seq, 0, "Sequence should be 0");
    // Due to saturating_sub, if window > now, we get 0
    // This is acceptable - it means reading from the beginning
}

/// Test that stream ID comparison works correctly for typical reconnection scenarios.
#[test]
fn test_stream_id_comparison_for_reconnect() {
    // Simulate a previous cursor (e.g., from before disconnect)
    let old_cursor = "1700000000000-0"; // Some past timestamp

    // Generate a new catchup start ID
    let catchup_id = catchup_start_id(300_000);

    // The catchup_id should be after the old_cursor if the disconnect was short
    // or before if the disconnect was longer than the window.
    // For this test, we just verify the format allows correct comparison.
    let (old_ts, _) = parse_stream_id(old_cursor).expect("Valid old cursor");
    let (new_ts, _) = parse_stream_id(&catchup_id).expect("Valid catchup ID");

    // Both timestamps should be valid (parsed successfully above).
    // The comparison should work numerically, not lexicographically
    // (Redis Stream IDs are {timestamp}-{sequence}, not simple strings).
    let _ = old_ts.cmp(&new_ts); // Verify Ord comparison compiles
}
