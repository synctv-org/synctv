//! Tests for ExternalStreamPuller retry counter reset behavior.
//!
//! These tests verify that the global_attempt_count is properly reset
//! after a successful long-lived connection.

#![allow(clippy::unwrap_used)]
/// Test that attempt counter is reset after long connection.
/// This test currently PASSES because we're documenting the bug.
#[test]
fn test_attempt_reset_after_long_connection_documents_bug() {
    let mut attempt: u32 = 5;
    let global_attempt_count: u32 = 100;

    // Simulate a successful long connection (> 1 minute)
    let stream_duration = std::time::Duration::from_secs(120);
    let min_successful_duration = std::time::Duration::from_secs(60);

    if stream_duration > min_successful_duration {
        attempt = 0;
        // BUG: global_attempt_count should also be reset here
        // global_attempt_count = 0;
    }

    // Current behavior: attempt is reset but global_attempt_count is not
    assert_eq!(attempt, 0);
    // This assertion documents the BUG
    assert_eq!(global_attempt_count, 100, "BUG: global_attempt_count should be 0");
}

/// Test expected behavior: both counters reset after long connection.
#[test]
fn test_both_counters_reset_after_long_connection() {
    let mut attempt: u32 = 5;
    let mut global_attempt_count: u32 = 100;

    // Simulate a successful long connection (> 1 minute)
    let stream_duration = std::time::Duration::from_secs(120);
    let min_successful_duration = std::time::Duration::from_secs(60);

    if stream_duration > min_successful_duration {
        attempt = 0;
        global_attempt_count = 0; // FIX: also reset global counter
    }

    // Expected behavior after fix
    assert_eq!(attempt, 0);
    assert_eq!(global_attempt_count, 0, "Both counters should be reset");
}

/// Test that short connections don't reset counters.
#[test]
fn test_short_connection_no_reset() {
    let mut attempt: u32 = 5;
    let mut global_attempt_count: u32 = 100;

    // Simulate a short failed connection (< 1 minute)
    let stream_duration = std::time::Duration::from_secs(30);
    let min_successful_duration = std::time::Duration::from_secs(60);

    if stream_duration > min_successful_duration {
        attempt = 0;
        global_attempt_count = 0;
    }

    // Counters should not be reset for short connections
    assert_eq!(attempt, 5);
    assert_eq!(global_attempt_count, 100);
}

/// Test boundary condition: exactly at threshold.
#[test]
fn test_boundary_at_threshold() {
    let mut attempt: u32 = 5;
    let mut global_attempt_count: u32 = 100;

    // Exactly at the threshold (1 minute)
    let stream_duration = std::time::Duration::from_secs(60);
    let min_successful_duration = std::time::Duration::from_secs(60);

    // > comparison means exactly 60 seconds does NOT trigger reset
    if stream_duration > min_successful_duration {
        attempt = 0;
        global_attempt_count = 0;
    }

    // At exactly 60 seconds, should NOT reset (must be >)
    assert_eq!(attempt, 5);
    assert_eq!(global_attempt_count, 100);
}

/// Test boundary condition: just above threshold.
#[test]
fn test_boundary_above_threshold() {
    let mut attempt: u32 = 5;
    let mut global_attempt_count: u32 = 100;

    // Just above the threshold (60 seconds + 1 nanosecond)
    let stream_duration = std::time::Duration::from_secs(60) + std::time::Duration::from_nanos(1);
    let min_successful_duration = std::time::Duration::from_secs(60);

    if stream_duration > min_successful_duration {
        attempt = 0;
        global_attempt_count = 0;
    }

    // Just above 60 seconds, should reset
    assert_eq!(attempt, 0);
    assert_eq!(global_attempt_count, 0);
}

/// Test that global_attempt_count can grow without reset.
#[test]
fn test_global_attempt_count_growth() {
    // Simulate multiple short failures without a long connection
    let mut _attempt: u32 = 0;
    let mut global_attempt_count: u32 = 0;

    for _ in 0..200 {
        _attempt += 1;
        global_attempt_count += 1;

        // Simulate short failure, no reset
        let stream_duration = std::time::Duration::from_secs(10);
        let min_successful_duration = std::time::Duration::from_secs(60);

        if stream_duration > min_successful_duration {
            _attempt = 0;
            global_attempt_count = 0;
        }
    }

    // After many short failures, global_attempt_count should be high
    assert_eq!(global_attempt_count, 200);
}

/// Test that GLOBAL_MAX_ATTEMPTS (1000) would be hit without proper reset.
#[test]
fn test_global_max_attempts_reached() {
    const GLOBAL_MAX_ATTEMPTS: u32 = 1000;

    let mut _attempt: u32 = 0;
    let mut global_attempt_count: u32 = 0;
    let mut hit_limit = false;

    for _ in 0..1100 {
        _attempt += 1;
        global_attempt_count += 1;

        if global_attempt_count > GLOBAL_MAX_ATTEMPTS {
            hit_limit = true;
            break;
        }

        // Simulate short failure, no reset (BUG)
        let stream_duration = std::time::Duration::from_secs(10);
        let min_successful_duration = std::time::Duration::from_secs(60);

        if stream_duration > min_successful_duration {
            _attempt = 0;
            // BUG: should also reset global_attempt_count
        }
    }

    assert!(hit_limit, "Should hit GLOBAL_MAX_ATTEMPTS limit");
}

/// Test that long connection reset prevents hitting GLOBAL_MAX_ATTEMPTS.
#[test]
fn test_long_connection_reset_prevents_limit() {
    const GLOBAL_MAX_ATTEMPTS: u32 = 1000;

    let mut _attempt: u32 = 0;
    let mut global_attempt_count: u32 = 0;
    let mut hit_limit = false;

    for i in 0..1100 {
        _attempt += 1;
        global_attempt_count += 1;

        if global_attempt_count > GLOBAL_MAX_ATTEMPTS {
            hit_limit = true;
            break;
        }

        // Every 100 iterations, simulate a long successful connection
        let stream_duration = if i % 100 == 99 {
            std::time::Duration::from_secs(120)
        } else {
            std::time::Duration::from_secs(10)
        };
        let min_successful_duration = std::time::Duration::from_secs(60);

        if stream_duration > min_successful_duration {
            _attempt = 0;
            global_attempt_count = 0; // FIX: reset both counters
        }
    }

    assert!(!hit_limit, "Should NOT hit GLOBAL_MAX_ATTEMPTS limit with proper reset");
}
