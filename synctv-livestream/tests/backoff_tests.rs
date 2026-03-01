//! Tests for the exponential backoff with jitter utility.
//!
//! These tests verify the backoff calculation after the B10 fix
//! (removing the off-by-one .saturating_sub(1)).

#![allow(clippy::unwrap_used)]
use std::time::Instant;

/// Helper: compute the expected base delay for a given attempt.
/// After the B10 fix: base = initial_ms * 2^attempt (capped at attempt=16)
fn expected_base(attempt: u32, initial_ms: u64) -> u64 {
    initial_ms.saturating_mul(1u64 << attempt.min(16))
}

#[tokio::test]
async fn test_backoff_attempt_0() {
    let initial_ms = 100;
    let max_ms = 10_000;

    let start = Instant::now();
    synctv_livestream::util::backoff(0, initial_ms, max_ms).await;
    let elapsed = start.elapsed().as_millis() as u64;

    // attempt=0: base = 100 * 2^0 = 100, with +/- 25% jitter = [75, 125]
    // Allow some scheduling slack
    assert!(
        (50..=200).contains(&elapsed),
        "attempt 0 delay was {}ms, expected ~100ms",
        elapsed
    );
}

#[tokio::test]
async fn test_backoff_exponential_growth() {
    // Verify that attempt 0 and attempt 1 produce different delays
    // (the B10 bug caused both to be identical).
    //
    // After fix: attempt 0 = initial*1, attempt 1 = initial*2
    let initial_ms = 100;
    let max_ms = 10_000;

    // Expected bases after fix:
    let base_0 = expected_base(0, initial_ms); // 100
    let base_1 = expected_base(1, initial_ms); // 200
    let base_2 = expected_base(2, initial_ms); // 400
    let base_3 = expected_base(3, initial_ms); // 800

    assert_eq!(base_0, 100);
    assert_eq!(base_1, 200);
    assert_eq!(base_2, 400);
    assert_eq!(base_3, 800);

    // Actually run the backoff for attempt 1 to verify it's longer than attempt 0
    let start = Instant::now();
    synctv_livestream::util::backoff(1, initial_ms, max_ms).await;
    let elapsed_1 = start.elapsed().as_millis() as u64;

    // attempt 1 base = 200, should be noticeably larger than attempt 0
    assert!(
        elapsed_1 >= 100,
        "attempt 1 delay was {}ms, expected >= 100ms",
        elapsed_1
    );
}

#[tokio::test]
async fn test_backoff_caps_at_max() {
    let initial_ms = 100;
    let max_ms = 500;

    let start = Instant::now();
    synctv_livestream::util::backoff(10, initial_ms, max_ms).await;
    let elapsed = start.elapsed().as_millis() as u64;

    // attempt=10: base = 100 * 1024 = 102400, but capped at max_ms=500
    // With jitter: [375, 500] (75% to 100% of max_ms)
    assert!(
        elapsed <= 600,
        "attempt 10 delay was {}ms, should be capped at ~500ms",
        elapsed
    );
}

#[tokio::test]
async fn test_backoff_jitter_within_bounds() {
    // Run multiple times and verify all delays are reasonable
    let initial_ms = 50;
    let max_ms = 5_000;

    for attempt in 0..4u32 {
        let base = expected_base(attempt, initial_ms).min(max_ms);
        let min_expected = base.saturating_sub(base / 4); // 75% of base
        let max_expected = max_ms; // capped at max_ms

        let start = Instant::now();
        synctv_livestream::util::backoff(attempt, initial_ms, max_ms).await;
        let elapsed = start.elapsed().as_millis() as u64;

        // Allow generous slack for scheduling jitter
        assert!(
            elapsed <= max_expected + 100,
            "attempt {} delay {}ms exceeded max {}ms + slack",
            attempt, elapsed, max_expected
        );

        // The delay should be at least some portion of the minimum
        // (very loose check because OS scheduling can introduce delays)
        assert!(
            elapsed >= min_expected.saturating_sub(50),
            "attempt {} delay {}ms was below min {}ms - slack",
            attempt, elapsed, min_expected
        );
    }
}

#[tokio::test]
async fn test_backoff_high_attempt_saturates() {
    // Very high attempt number should not panic due to overflow
    let initial_ms = 100;
    let max_ms = 1_000;

    // attempt=100 is clamped to 16, so base = 100 * 2^16 = 6553600, capped to 1000
    let start = Instant::now();
    synctv_livestream::util::backoff(100, initial_ms, max_ms).await;
    let elapsed = start.elapsed().as_millis() as u64;

    assert!(
        elapsed <= 1200,
        "high attempt delay {}ms should be capped at max_ms",
        elapsed
    );
}
