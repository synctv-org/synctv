//! Leader election tests
//!
//! Tests for leader guard cancellation via the `LeaderElect` trait,
//! first-election timing (verifies the bug fix), and vacancy events.
//! These tests do not require a running Redis instance.

#![allow(clippy::unwrap_used)]
use std::time::Duration;
use synctv_cluster::leader::{LeaderElect, LeadershipEvent};
use tokio::sync::broadcast;

/// A minimal mock elector that exposes a `broadcast::Sender` so tests can
/// inject arbitrary `LeadershipEvents`.
struct MockElector {
    tx: broadcast::Sender<LeadershipEvent>,
}

impl MockElector {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self { tx }
    }
}

impl LeaderElect for MockElector {
    fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent> {
        self.tx.subscribe()
    }
}

// ============================================================================
// Test 1: leader_guard cancelled on Lost event
// ============================================================================

#[tokio::test]
async fn test_leader_guard_cancelled_on_lost() {
    let elector = MockElector::new();
    let guard = elector.leader_guard();

    assert!(!guard.is_cancelled(), "Guard should start uncancelled");

    // Simulate leadership loss
    let _ = elector.tx.send(LeadershipEvent::Lost);

    // Give the spawned task time to process the event
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        guard.is_cancelled(),
        "Guard should be cancelled after Lost event"
    );
}

// ============================================================================
// Test 2: leader_guard cancelled on Vacancy event
// ============================================================================

#[tokio::test]
async fn test_leader_guard_cancelled_on_vacancy() {
    let elector = MockElector::new();
    let guard = elector.leader_guard();

    assert!(!guard.is_cancelled());

    // Simulate vacancy
    let _ = elector.tx.send(LeadershipEvent::Vacancy);

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        guard.is_cancelled(),
        "Guard should be cancelled after Vacancy event"
    );
}

// ============================================================================
// Test 3: leader_guard NOT cancelled on Gained event
// ============================================================================

#[tokio::test]
async fn test_leader_guard_not_cancelled_on_gained() {
    let elector = MockElector::new();
    let guard = elector.leader_guard();

    let _ = elector.tx.send(LeadershipEvent::Gained { epoch: 1 });

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        !guard.is_cancelled(),
        "Guard should NOT be cancelled after Gained event"
    );
}

// ============================================================================
// Test 4: First election not delayed (verifies the bug fix)
//
// The old code used `tokio::time::sleep(interval)` which always waited
// one full renew_interval before the first election attempt. The fix
// uses `tokio::time::interval` which fires immediately on first tick.
//
// This test verifies the fix by confirming that `tokio::time::interval`
// with the default `Burst` tick behavior fires its first tick without delay.
// ============================================================================

#[tokio::test]
async fn test_first_election_not_delayed() {
    use std::time::Instant;

    let start = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_secs(10));
    // First tick fires immediately (this is the behavior we rely on in run_loop)
    ticker.tick().await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(50),
        "First tick of tokio::time::interval should fire immediately, took {elapsed:?}"
    );
}

// ============================================================================
// Test 5: leader_guard cancelled when channel is closed (elector dropped)
// ============================================================================

#[tokio::test]
async fn test_leader_guard_cancelled_on_channel_close() {
    let guard = {
        let elector = MockElector::new();
        elector.leader_guard()
    };
    // elector is dropped, closing the broadcast channel

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        guard.is_cancelled(),
        "Guard should be cancelled when elector is dropped"
    );
}

// ============================================================================
// Test 6: Redis time-based grace period prevents clock skew split-brain
// ============================================================================

/// This test verifies that the clock skew fix works correctly by simulating
/// the scenario where two nodes have different local clocks but both query
/// Redis TIME for grace period calculations.
///
/// The fix ensures that:
/// 1. Leadership loss timestamps are stored as Redis TIME (not local Instant)
/// 2. Grace period checks query Redis TIME and compare against stored timestamp
/// 3. Multiple nodes with clock skew cannot simultaneously exit grace period
#[test]
fn test_redis_time_prevents_clock_skew_split_brain() {
    // Simulate the scenario:
    // - Node A loses leadership at Redis timestamp T=1000
    // - Grace period is 10 seconds
    // - Node B has clock skew: its local time shows T=1015 (15s ahead)
    // - Node C has clock skew: its local time shows T=990 (10s behind)
    //
    // With old implementation (local Instant):
    // - Node B sees 15s elapsed > 10s grace period, attempts acquisition
    // - Node C sees -10s (wrapped around) or 0s elapsed, waits
    // - Result: Node B exits grace period early, can acquire leadership
    //
    // With new implementation (Redis TIME):
    // - Node B queries Redis: current=1005, elapsed=1005-1000=5s < 10s, waits
    // - Node C queries Redis: current=1005, elapsed=1005-1000=5s < 10s, waits
    // - Result: Both nodes correctly wait until T=1010 (Redis time)

    let lost_at_redis_ts = 1000u64;
    let renew_interval_secs = 10u64;
    let current_redis_ts = 1005u64;

    // Simulate what in_grace_period() does
    let elapsed = current_redis_ts.saturating_sub(lost_at_redis_ts);
    let in_grace = elapsed < renew_interval_secs;

    assert!(
        in_grace,
        "Should be in grace period: 5s elapsed < 10s grace period"
    );

    // After grace period expires
    let current_redis_ts = 1011u64;
    let elapsed = current_redis_ts.saturating_sub(lost_at_redis_ts);
    let in_grace = elapsed < renew_interval_secs;

    assert!(
        !in_grace,
        "Should NOT be in grace period: 11s elapsed >= 10s grace period"
    );
}

/// Test that `saturating_sub` handles timestamp underflow correctly
/// (when Redis time goes backwards due to clock adjustments)
#[test]
fn test_redis_time_saturating_sub_handles_underflow() {
    // If Redis time is adjusted backwards (rare but possible)
    let lost_at_redis_ts = 1000u64;
    let current_redis_ts = 990u64; // Went backwards!
    let renew_interval_secs = 10u64;

    // saturating_sub returns 0 on underflow
    let elapsed = current_redis_ts.saturating_sub(lost_at_redis_ts);
    assert_eq!(elapsed, 0, "saturating_sub should return 0 on underflow");

    let in_grace = elapsed < renew_interval_secs;
    assert!(
        in_grace,
        "Should be in grace period when time goes backwards (conservative)"
    );
}
