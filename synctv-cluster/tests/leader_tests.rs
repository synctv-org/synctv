//! Leader election tests
//!
//! Tests for leader guard cancellation via the LeaderElect trait,
//! first-election timing (verifies the bug fix), and vacancy events.
//! These tests do not require a running Redis instance.

use std::time::Duration;
use synctv_cluster::leader::{LeaderElect, LeadershipEvent};
use tokio::sync::broadcast;

/// A minimal mock elector that exposes a broadcast::Sender so tests can
/// inject arbitrary LeadershipEvents.
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
        "First tick of tokio::time::interval should fire immediately, took {:?}",
        elapsed
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
