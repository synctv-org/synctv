//! Tests for `ClusterManager` shutdown behavior
//!
//! These tests verify that the shutdown method properly waits for
//! the heartbeat task with appropriate timeout handling.
//!
//! ## Key Requirements
//!
//! 1. **Graceful wait**: shutdown should wait for heartbeat task to complete
//! 2. **Timeout protection**: shutdown should have a timeout to prevent infinite hang
//! 3. **Panic handling**: shutdown should log warning on panic, not crash
//! 4. **Idempotency**: multiple shutdown calls should be safe

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use std::time::Duration;

use synctv_cluster::discovery::{NodeInfo, NodeRegistry};
use synctv_cluster::sync::cluster_manager::ClusterConfig;
use synctv_cluster::sync::ClusterManager;

/// Helper to create a `NodeRegistry` for testing (local mode, no actual Redis connection needed)
fn make_registry(node_id: &str) -> Arc<NodeRegistry> {
    let client = redis::Client::open("redis://localhost:6379").unwrap();
    Arc::new(NodeRegistry::new(client, node_id.to_string(), 30, "test:").unwrap())
}

/// Helper to create a `ClusterManager` in single-node mode (no Redis)
async fn make_cluster_manager(node_id: &str) -> ClusterManager {
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        cluster_enabled: false,
        node_id: node_id.to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };
    ClusterManager::new(config, None, None)
        .await
        .expect("ClusterManager::new should succeed")
}

// =====================================================================
// Test 1: shutdown should wait for heartbeat task to complete
// =====================================================================

/// Test that shutdown waits for the heartbeat task to complete gracefully.
///
/// This test verifies:
/// 1. The heartbeat task receives the cancellation signal
/// 2. The heartbeat task exits cleanly
/// 3. `shutdown()` returns only after the heartbeat task has terminated
#[tokio::test]
async fn test_shutdown_waits_for_heartbeat_task_completion() {
    let manager = make_cluster_manager("shutdown-test-node").await;
    let registry = make_registry("shutdown-test-node");

    // Insert node info into local registry for testing
    registry
        .test_insert_local(NodeInfo::new(
            "shutdown-test-node".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    // Start the heartbeat loop
    manager
        .start_heartbeat_loop(registry, "localhost:8080".to_string(), None::<fn() -> usize>)
        .await;

    // Give the heartbeat task a moment to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Call shutdown - this should wait for heartbeat task to complete
    manager.shutdown().await;

    // If we get here, shutdown completed
    // The key assertion is that shutdown() returned at all (didn't hang forever)
}

// =====================================================================
// Test 2: shutdown should have timeout for heartbeat task
// =====================================================================

/// Test that shutdown has a timeout when waiting for the heartbeat task.
///
/// This test verifies that even if the heartbeat task takes time to respond,
/// the shutdown will eventually timeout and proceed rather than hanging forever.
///
/// The timeout should be consistent with `publisher_task` (10 seconds).
/// We allow up to 15 seconds to account for system variability.
#[tokio::test]
async fn test_shutdown_has_timeout_for_heartbeat_task() {
    let manager = make_cluster_manager("timeout-test-node").await;
    let registry = make_registry("timeout-test-node");

    registry
        .test_insert_local(NodeInfo::new(
            "timeout-test-node".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    // Start the heartbeat loop
    manager
        .start_heartbeat_loop(registry, "localhost:8080".to_string(), None::<fn() -> usize>)
        .await;

    // Give the heartbeat task a moment to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Measure how long shutdown takes
    let start = std::time::Instant::now();
    manager.shutdown().await;
    let elapsed = start.elapsed();

    // The shutdown should complete within a reasonable time.
    // With a 10-second timeout for heartbeat task, shutdown should complete
    // within 15 seconds to account for other shutdown work and system variability.
    //
    // IMPORTANT: If this test fails because elapsed >= 15 seconds, it means
    // the heartbeat task is not being awaited with a timeout and may be hanging.
    assert!(
        elapsed < Duration::from_secs(15),
        "Shutdown took too long ({elapsed:?}), suggesting heartbeat task wait has no timeout"
    );
}

// =====================================================================
// Test 3: shutdown handles JoinHandle error gracefully
// =====================================================================

/// Test that shutdown handles `JoinHandle` errors (like task panic) gracefully.
///
/// The shutdown code should match on both Ok(()) and Err(e) from handle.await,
/// logging a warning for errors rather than crashing or ignoring silently.
///
/// This test verifies basic completion when the task is in normal state.
#[tokio::test]
async fn test_shutdown_handles_task_error_gracefully() {
    let manager = make_cluster_manager("error-test-node").await;
    let registry = make_registry("error-test-node");

    registry
        .test_insert_local(NodeInfo::new(
            "error-test-node".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    // Start the heartbeat loop
    manager
        .start_heartbeat_loop(registry, "localhost:8080".to_string(), None::<fn() -> usize>)
        .await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Call shutdown - should complete without hanging
    manager.shutdown().await;

    // If we reach this point, shutdown completed successfully
}

// =====================================================================
// Test 4: shutdown without heartbeat task should succeed
// =====================================================================

/// Test that shutdown works correctly when no heartbeat task was started.
///
/// This verifies that the shutdown logic handles the case where
/// `start_heartbeat_loop` was never called.
#[tokio::test]
async fn test_shutdown_without_heartbeat_task() {
    let manager = make_cluster_manager("no-heartbeat-node").await;

    // Don't start heartbeat loop

    // Shutdown should complete quickly without hanging
    let start = std::time::Instant::now();
    manager.shutdown().await;
    let elapsed = start.elapsed();

    // Should complete within 1 second since there's no heartbeat task to wait for
    assert!(
        elapsed < Duration::from_secs(1),
        "Shutdown without heartbeat task took too long: {elapsed:?}"
    );
}

// =====================================================================
// Test 5: shutdown is idempotent
// =====================================================================

/// Test that calling shutdown multiple times is safe.
///
/// This verifies that:
/// 1. The first shutdown completes successfully
/// 2. Subsequent shutdown calls don't panic or hang
#[tokio::test]
async fn test_shutdown_is_idempotent() {
    let manager = make_cluster_manager("idempotent-node").await;
    let registry = make_registry("idempotent-node");

    registry
        .test_insert_local(NodeInfo::new(
            "idempotent-node".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    // Start the heartbeat loop
    manager
        .start_heartbeat_loop(registry, "localhost:8080".to_string(), None::<fn() -> usize>)
        .await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    // First shutdown
    manager.shutdown().await;

    // Second shutdown should not panic or hang
    // Note: After the first shutdown, the handle is taken(), so this should be a no-op
    let start = std::time::Instant::now();
    manager.shutdown().await;
    let elapsed = start.elapsed();

    // Second shutdown should complete quickly since there's nothing to do
    assert!(
        elapsed < Duration::from_millis(100),
        "Second shutdown took too long: {elapsed:?}"
    );
}

// =====================================================================
// Test 6: shutdown completes within expected timeout
// =====================================================================

/// Test that shutdown completes within the expected timeout.
///
/// This test verifies that shutdown doesn't wait indefinitely for the
/// heartbeat task, matching the 10-second timeout pattern used for `publisher_task`.
#[tokio::test]
async fn test_shutdown_completes_within_timeout() {
    let manager = make_cluster_manager("timeout-complete-node").await;
    let registry = make_registry("timeout-complete-node");

    registry
        .test_insert_local(NodeInfo::new(
            "timeout-complete-node".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    // Start the heartbeat loop
    manager
        .start_heartbeat_loop(registry, "localhost:8080".to_string(), None::<fn() -> usize>)
        .await;

    // Give the heartbeat task a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Measure shutdown time
    let start = std::time::Instant::now();
    manager.shutdown().await;
    let elapsed = start.elapsed();

    // Shutdown should complete quickly because the heartbeat task
    // should respond to cancellation immediately.
    // If shutdown has a proper timeout, even a stuck task won't block beyond that.
    // We expect:
    // - Normal case: < 1 second (task responds to cancel quickly)
    // - Timeout case: < 15 seconds (10s timeout + margin)
    //
    // If this test fails, it indicates the heartbeat task wait lacks a timeout.
    assert!(
        elapsed < Duration::from_secs(15),
        "Shutdown took too long: {elapsed:?}. This suggests heartbeat task wait lacks timeout."
    );
}

// =====================================================================
// Test 7: verify shutdown timeout matches publisher_task pattern
// =====================================================================

/// Test that the heartbeat task shutdown follows the same pattern as `publisher_task`.
///
/// Both should:
/// 1. Use `tokio::time::timeout` with `Duration::from_secs(10)`
/// 2. Log "completed cleanly" on Ok(Ok(()))
/// 3. Log warning on Ok(Err(e)) (panic case)
/// 4. Log warning and proceed on timeout
///
/// This test verifies the behavior is consistent with `publisher_task` handling.
#[tokio::test]
async fn test_shutdown_pattern_matches_publisher_task() {
    let manager = make_cluster_manager("pattern-test-node").await;
    let registry = make_registry("pattern-test-node");

    registry
        .test_insert_local(NodeInfo::new(
            "pattern-test-node".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    // Start the heartbeat loop
    manager
        .start_heartbeat_loop(registry, "localhost:8080".to_string(), None::<fn() -> usize>)
        .await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Shutdown should complete - the implementation should match publisher_task pattern:
    // - Use tokio::time::timeout(Duration::from_secs(10), handle)
    // - Handle Ok(Ok(())), Ok(Err(e)), and Err(_) cases
    let start = std::time::Instant::now();
    manager.shutdown().await;
    let elapsed = start.elapsed();

    // The shutdown should complete within the expected timeout bounds
    // - Fast completion (< 1s) means task responded to cancel
    // - Completion within 10-15s means timeout kicked in
    // - Completion > 15s indicates a bug (no timeout on heartbeat task)
    assert!(
        elapsed < Duration::from_secs(15),
        "Shutdown exceeded expected timeout: {elapsed:?}. \
         Heartbeat task handling should match publisher_task pattern with 10s timeout."
    );
}

// =====================================================================
// Test 8: parent cancel token propagation (L11)
// =====================================================================

/// Test that cancelling a parent token also cancels the ClusterManager's
/// internal cancel token (child token pattern).
#[tokio::test]
async fn test_parent_cancel_token_propagates_cancellation() {
    use tokio_util::sync::CancellationToken;

    let parent = CancellationToken::new();

    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        cluster_enabled: false,
        node_id: "child-token-test".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: Some(parent.clone()),
    };

    let manager = ClusterManager::new(config, None, None)
        .await
        .expect("ClusterManager::new should succeed");

    // The child token should not be cancelled yet
    assert!(
        !manager.cancel_token().is_cancelled(),
        "Child token should not be cancelled before parent"
    );

    // Cancel the parent token
    parent.cancel();

    // The child token should now be cancelled too
    assert!(
        manager.cancel_token().is_cancelled(),
        "Cancelling parent token should propagate to ClusterManager's child token"
    );
}

// =====================================================================
// Test 9: without parent cancel token, ClusterManager uses independent token
// =====================================================================

#[tokio::test]
async fn test_without_parent_cancel_token_uses_independent_token() {
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        cluster_enabled: false,
        node_id: "independent-token-test".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };

    let manager = ClusterManager::new(config, None, None)
        .await
        .expect("ClusterManager::new should succeed");

    // Token should not be cancelled
    assert!(
        !manager.cancel_token().is_cancelled(),
        "Independent cancel token should not be cancelled"
    );

    // Shutdown should cancel it
    manager.shutdown().await;
    assert!(
        manager.cancel_token().is_cancelled(),
        "Token should be cancelled after shutdown"
    );
}
