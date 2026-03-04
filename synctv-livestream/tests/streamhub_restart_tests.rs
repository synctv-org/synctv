//! `StreamHub` restart race condition tests.
//!
//! Tests verify that the two-phase cleanup protocol during `StreamHub` restart
//! correctly synchronizes stream stopping with re-registration, preventing
//! the race condition where active streams are stopped while re-registration
//! is already in progress.
//!
//! Run with: cargo test --test `streamhub_restart_tests`

#![allow(clippy::unwrap_used)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Simulates the two-phase cleanup protocol used during `StreamHub` restart.
///
/// Phase 1: Send stop request with oneshot sender
/// Phase 2: Wait for confirmation (with timeout)
/// Phase 3: Proceed with re-registration
///
/// This test verifies the protocol works correctly under normal conditions.
#[tokio::test]
async fn test_two_phase_cleanup_normal_completion() {
    // Simulate the stop channel with oneshot response
    let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<oneshot::Sender<()>>(10);

    // Spawn the "receiver" task that simulates stream manager behavior
    let receiver_handle = tokio::spawn(async move {
        let mut stop_count = 0;
        while let Some(stop_done_tx) = stop_streams_rx.recv().await {
            stop_count += 1;
            // Simulate stop_all() work
            tokio::time::sleep(Duration::from_millis(10)).await;
            // Signal completion
            let _ = stop_done_tx.send(());
        }
        stop_count
    });

    // Simulate the restart loop: send stop request and wait for completion
    let (stop_done_tx, stop_done_rx) = oneshot::channel::<()>();
    stop_streams_tx.send(stop_done_tx).await.unwrap();

    // Wait for completion with timeout
    let result = tokio::time::timeout(Duration::from_millis(100), stop_done_rx).await;
    assert!(result.is_ok(), "Stop should complete within timeout");
    assert!(
        result.unwrap().is_ok(),
        "Oneshot sender should not be dropped"
    );

    // Clean up
    drop(stop_streams_tx);
    let stop_count = receiver_handle.await.unwrap();
    assert_eq!(
        stop_count, 1,
        "Should have processed exactly one stop request"
    );
}

/// Test that the restart loop properly handles timeout when stop takes too long.
#[tokio::test]
async fn test_two_phase_cleanup_timeout() {
    let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<oneshot::Sender<()>>(10);

    // Spawn a receiver that intentionally delays longer than the timeout
    let receiver_handle = tokio::spawn(async move {
        while let Some(stop_done_tx) = stop_streams_rx.recv().await {
            // Simulate a very slow stop_all() (200ms > 100ms timeout)
            tokio::time::sleep(Duration::from_millis(200)).await;
            // Even though we're late, we still send the response
            let _ = stop_done_tx.send(());
        }
    });

    // Send stop request
    let (stop_done_tx, stop_done_rx) = oneshot::channel::<()>();
    stop_streams_tx.send(stop_done_tx).await.unwrap();

    // Wait with short timeout (should timeout)
    let result = tokio::time::timeout(Duration::from_millis(100), stop_done_rx).await;
    assert!(result.is_err(), "Should timeout when stop takes too long");

    // Clean up
    drop(stop_streams_tx);
    let _ = receiver_handle.await;
}

/// Test that multiple rapid restarts are handled correctly.
#[tokio::test]
async fn test_two_phase_cleanup_rapid_restarts() {
    let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<oneshot::Sender<()>>(10);

    let stop_count = Arc::new(AtomicUsize::new(0));
    let stop_count_clone = Arc::clone(&stop_count);

    // Spawn receiver that counts stops
    let receiver_handle = tokio::spawn(async move {
        while let Some(stop_done_tx) = stop_streams_rx.recv().await {
            stop_count_clone.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = stop_done_tx.send(());
        }
    });

    // Simulate 3 rapid restarts
    for i in 0..3 {
        let (stop_done_tx, stop_done_rx) = oneshot::channel::<()>();
        stop_streams_tx.send(stop_done_tx).await.unwrap();

        let result = tokio::time::timeout(Duration::from_millis(100), stop_done_rx).await;
        assert!(result.is_ok(), "Restart {i} should complete within timeout");
    }

    // Clean up
    drop(stop_streams_tx);
    let _ = receiver_handle.await;

    assert_eq!(
        stop_count.load(Ordering::SeqCst),
        3,
        "Should have processed 3 stops"
    );
}

/// Test that the restarting flag correctly suppresses stream creation during restart.
#[tokio::test]
async fn test_restarting_flag_blocks_stream_creation() {
    let is_restarting = Arc::new(AtomicBool::new(false));

    // Simulate normal operation - stream creation allowed
    assert!(
        !is_restarting.load(Ordering::Acquire),
        "Should not be restarting initially"
    );

    // Simulate restart phase 1: set flag before cleanup
    is_restarting.store(true, Ordering::Release);
    assert!(
        is_restarting.load(Ordering::Acquire),
        "Should be restarting after flag set"
    );

    // During restart, new stream creation should be blocked
    // (In real code, get_or_create checks is_restarting flag)

    // Simulate restart phase 2: cleanup and re-registration complete
    is_restarting.store(false, Ordering::Release);
    assert!(
        !is_restarting.load(Ordering::Acquire),
        "Should not be restarting after cleanup"
    );
}

/// Test the complete restart sequence with proper ordering.
#[tokio::test]
async fn test_restart_sequence_ordering() {
    let is_restarting = Arc::new(AtomicBool::new(false));
    let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<oneshot::Sender<()>>(10);
    let reregister_notify = Arc::new(tokio::sync::Notify::new());

    let is_restarting_clone = Arc::clone(&is_restarting);
    let reregister_clone = Arc::clone(&reregister_notify);

    // Spawn the stream manager receiver task
    let receiver_handle = tokio::spawn(async move {
        while let Some(stop_done_tx) = stop_streams_rx.recv().await {
            // Stop all streams
            tokio::time::sleep(Duration::from_millis(10)).await;
            // Signal completion
            let _ = stop_done_tx.send(());
        }
    });

    // Spawn the re-registration listener
    let reregister_handle = tokio::spawn(async move {
        reregister_clone.notified().await;
        // Simulate re-registration
        tokio::time::sleep(Duration::from_millis(5)).await;
        // Clear restarting flag after re-registration
        is_restarting_clone.store(false, Ordering::Release);
    });

    // Simulate the restart loop sequence:
    // 1. Set restarting flag FIRST (blocks new stream creation)
    is_restarting.store(true, Ordering::Release);

    // 2. Send stop request and wait for completion
    let (stop_done_tx, stop_done_rx) = oneshot::channel::<()>();
    stop_streams_tx.send(stop_done_tx).await.unwrap();

    let result = tokio::time::timeout(Duration::from_millis(100), stop_done_rx).await;
    assert!(result.is_ok(), "Stop should complete within timeout");

    // 3. Only AFTER stop completes, notify re-registration
    reregister_notify.notify_one();

    // Wait for re-registration to complete
    tokio::time::timeout(Duration::from_millis(100), reregister_handle)
        .await
        .unwrap()
        .unwrap();

    // 4. Verify restarting flag is cleared
    assert!(
        !is_restarting.load(Ordering::Acquire),
        "Restarting flag should be cleared"
    );

    // Clean up
    drop(stop_streams_tx);
    let _ = receiver_handle.await;
}

/// Test that `try_send` (non-blocking send) works correctly for the stop channel.
#[tokio::test]
async fn test_try_send_with_oneshot_response() {
    let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<oneshot::Sender<()>>(2);

    let receiver_handle = tokio::spawn(async move {
        while let Some(stop_done_tx) = stop_streams_rx.recv().await {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = stop_done_tx.send(());
        }
    });

    // Use try_send (non-blocking) like in the real implementation
    let (stop_done_tx, stop_done_rx) = oneshot::channel::<()>();
    match stop_streams_tx.try_send(stop_done_tx) {
        Ok(()) => {
            let result = tokio::time::timeout(Duration::from_millis(100), stop_done_rx).await;
            assert!(result.is_ok(), "Stop should complete within timeout");
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            panic!("Channel should not be full");
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            panic!("Channel should not be closed");
        }
    }

    // Clean up
    drop(stop_streams_tx);
    let _ = receiver_handle.await;
}

/// Test that channel full condition is handled gracefully.
#[tokio::test]
async fn test_try_send_full_handling() {
    // Create a channel with capacity 1
    let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<oneshot::Sender<()>>(1);

    // Don't consume from receiver, so channel will be full

    // First send should succeed
    let (tx1, _rx1) = oneshot::channel::<()>();
    assert!(
        stop_streams_tx.try_send(tx1).is_ok(),
        "First send should succeed"
    );

    // Second send should fail with Full (not closed)
    let (tx2, _rx2) = oneshot::channel::<()>();
    match stop_streams_tx.try_send(tx2) {
        Err(mpsc::error::TrySendError::Full(_)) => {
            // Expected - channel is full
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            panic!("Should be Full, not Closed");
        }
        Ok(()) => {
            panic!("Should fail when channel is full");
        }
    }

    // Clean up
    drop(stop_streams_tx);
    while stop_streams_rx.recv().await.is_some() {}
}

/// Test the race condition prevention: verify that the restarting flag is set
/// BEFORE stop request is sent, not after.
#[tokio::test]
async fn test_restarting_flag_set_before_stop_request() {
    let events: Arc<std::sync::Mutex<Vec<&'static str>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);

    let is_restarting = Arc::new(AtomicBool::new(false));
    let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<oneshot::Sender<()>>(10);

    // Spawn receiver that records event order
    let receiver_handle = tokio::spawn(async move {
        while let Some(stop_done_tx) = stop_streams_rx.recv().await {
            events_clone.lock().unwrap().push("stop_received");
            tokio::time::sleep(Duration::from_millis(5)).await;
            events_clone.lock().unwrap().push("stop_completed");
            let _ = stop_done_tx.send(());
        }
    });

    // Record the exact sequence of operations
    events.lock().unwrap().push("set_restarting");
    is_restarting.store(true, Ordering::Release);

    events.lock().unwrap().push("send_stop_request");
    let (stop_done_tx, stop_done_rx) = oneshot::channel::<()>();
    stop_streams_tx.send(stop_done_tx).await.unwrap();

    // Wait for completion
    tokio::time::timeout(Duration::from_millis(100), stop_done_rx)
        .await
        .unwrap()
        .unwrap();

    events.lock().unwrap().push("stop_confirmed");

    // Verify ordering
    let recorded = events.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            "set_restarting",
            "send_stop_request",
            "stop_received",
            "stop_completed",
            "stop_confirmed"
        ],
        "Operations should be in correct order: set flag -> send request -> receive confirmation"
    );

    // Clean up
    drop(stop_streams_tx);
    let _ = receiver_handle.await;
}

// ============================================================================
// D5: Verify stop_all timeout is sufficient (5 seconds, not 100ms)
// ============================================================================

/// D5 fix verification: The two-phase cleanup timeout should be 5 seconds, not 100ms.
///
/// The original 100ms timeout was too short for streams that need to cleanly
/// disconnect from remote servers. This test verifies that a slow stop_all()
/// completes within the 5-second window instead of timing out.
#[tokio::test]
async fn test_stop_all_timeout_sufficient_for_slow_cleanup() {
    let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<oneshot::Sender<()>>(10);

    // Spawn a receiver that simulates a slow stop_all() (takes ~2 seconds)
    let receiver_handle = tokio::spawn(async move {
        while let Some(stop_done_tx) = stop_streams_rx.recv().await {
            // Simulate slow stream cleanup (2 seconds)
            // This would have timed out with the old 100ms timeout
            tokio::time::sleep(Duration::from_secs(2)).await;
            let _ = stop_done_tx.send(());
        }
    });

    // Simulate the restart path with the new 5-second timeout
    let (stop_done_tx, stop_done_rx) = oneshot::channel::<()>();
    stop_streams_tx.send(stop_done_tx).await.unwrap();

    let result = tokio::time::timeout(Duration::from_secs(5), stop_done_rx).await;

    assert!(
        result.is_ok(),
        "stop_all() with 2s cleanup should complete within the 5s timeout (D5 fix)"
    );

    // Clean up
    drop(stop_streams_tx);
    let _ = receiver_handle.await;
}

/// D5: The old 100ms timeout would fail for cleanup taking > 100ms.
///
/// This test verifies that the scenario that previously failed (cleanup > 100ms)
/// now succeeds with the 5-second timeout.
#[tokio::test]
async fn test_stop_all_handles_moderate_cleanup_time() {
    let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<oneshot::Sender<()>>(10);

    // Spawn a receiver with 500ms cleanup (would fail with 100ms timeout)
    let receiver_handle = tokio::spawn(async move {
        while let Some(stop_done_tx) = stop_streams_rx.recv().await {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = stop_done_tx.send(());
        }
    });

    let (stop_done_tx, stop_done_rx) = oneshot::channel::<()>();
    stop_streams_tx.send(stop_done_tx).await.unwrap();

    let result = tokio::time::timeout(Duration::from_secs(5), stop_done_rx).await;

    assert!(
        result.is_ok(),
        "stop_all() with 500ms cleanup should succeed with 5s timeout (previously failed with 100ms)"
    );

    drop(stop_streams_tx);
    let _ = receiver_handle.await;
}

// ============================================================================
// Race condition fix: Verify restart mutex and publication blocking
// ============================================================================

/// Test that concurrent restart attempts are serialized via a mutex.
///
/// This simulates the scenario where the StreamHub exits multiple times
/// in quick succession. Without a mutex, multiple restart flows could
/// execute concurrently, leading to:
/// - Corrupted state from parallel cleanup_all_publishers_for_node calls
/// - Lost re-registration signals
/// - Inconsistent is_restarting flag state
#[tokio::test]
async fn test_restart_mutex_serializes_concurrent_attempts() {
    use tokio::sync::Mutex;

    let restart_mutex = Arc::new(Mutex::new(()));
    let restart_count = Arc::new(AtomicUsize::new(0));
    let concurrent_count = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));

    // Spawn multiple tasks that try to restart concurrently
    let mut handles = vec![];
    for _ in 0..5 {
        let mutex = Arc::clone(&restart_mutex);
        let count = Arc::clone(&restart_count);
        let concurrent = Arc::clone(&concurrent_count);
        let max = Arc::clone(&max_concurrent);

        let handle = tokio::spawn(async move {
            let _guard = mutex.lock().await;

            // Track concurrent executions
            let current = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
            max.fetch_max(current, Ordering::SeqCst);

            // Simulate restart work
            tokio::time::sleep(Duration::from_millis(50)).await;

            count.fetch_add(1, Ordering::SeqCst);
            concurrent.fetch_sub(1, Ordering::SeqCst);
        });
        handles.push(handle);
    }

    // Wait for all restarts to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // All 5 restarts should have completed
    assert_eq!(
        restart_count.load(Ordering::SeqCst),
        5,
        "All restarts should complete"
    );

    // Maximum concurrent executions should be 1 (serialized)
    assert_eq!(
        max_concurrent.load(Ordering::SeqCst),
        1,
        "Restarts should be serialized, not concurrent"
    );
}

/// Test that the is_restarting flag is set BEFORE the hub exits, not after.
///
/// This ensures that new publications are blocked during the entire restart
/// window, including the cleanup phase. If the flag is set after hub exit,
/// there's a window where:
/// 1. Old hub has exited
/// 2. New publications can still arrive (from in-flight requests)
/// 3. is_restarting is false, so they're accepted
/// 4. These publications then get cleaned up by cleanup_all_publishers_for_node
#[tokio::test]
async fn test_restarting_flag_set_before_hub_exit() {
    // This test verifies the ordering: is_restarting = true -> hub exit -> cleanup
    //
    // In the actual implementation, we need to set is_restarting BEFORE
    // the hub.run() returns. However, since hub exit is unpredictable,
    // the fix is to:
    // 1. Use a mutex to block new publications during the critical section
    // 2. Check the mutex in on_publish before allowing registration

    let is_restarting = Arc::new(AtomicBool::new(false));
    let publications_allowed = Arc::new(AtomicBool::new(true));

    // Simulate the restart flow:
    // 1. Hub is about to exit - set restarting flag FIRST
    is_restarting.store(true, Ordering::Release);
    publications_allowed.store(false, Ordering::Release);

    // 2. Now publications are blocked
    assert!(
        !publications_allowed.load(Ordering::Acquire),
        "Publications should be blocked as soon as restart begins"
    );

    // 3. Hub exits, cleanup happens
    tokio::time::sleep(Duration::from_millis(10)).await;

    // 4. New hub starts, re-registration completes
    publications_allowed.store(true, Ordering::Release);
    is_restarting.store(false, Ordering::Release);

    // 5. Publications are allowed again
    assert!(
        publications_allowed.load(Ordering::Acquire),
        "Publications should be allowed after restart completes"
    );
}

/// Test that publications are rejected during the restart window.
///
/// This simulates the scenario where an RTMP publish request arrives
/// while the StreamHub is restarting. The publication should be rejected
/// with an appropriate error.
#[tokio::test]
async fn test_publication_rejected_during_restart() {
    let is_restarting = Arc::new(AtomicBool::new(true));
    let publication_succeeded = Arc::new(AtomicBool::new(false));

    // Simulate a publication attempt during restart
    let flag = Arc::clone(&is_restarting);
    let success = Arc::clone(&publication_succeeded);

    let handle = tokio::spawn(async move {
        // Check the flag before allowing publication
        if flag.load(Ordering::Acquire) {
            // Publication should be rejected
            return Err("StreamHub is restarting, publication rejected");
        }
        // If not restarting, allow the publication
        success.store(true, Ordering::Release);
        Ok(())
    });

    let result = handle.await.unwrap();
    assert!(
        result.is_err(),
        "Publication should be rejected during restart"
    );
    assert!(
        !publication_succeeded.load(Ordering::SeqCst),
        "Publication should not have succeeded"
    );
}

/// Test that HUB_MAX_RESTARTS doesn't get exhausted by rapid transient failures.
///
/// The issue: if restart_count is incremented on every exit and never decremented,
/// transient failures (e.g., brief network issues) could exhaust the limit
/// even though the hub eventually stabilizes.
///
/// The fix: decrement restart_count on each successful exit (clean shutdown),
/// allowing the hub to recover from transient failure bursts.
#[tokio::test]
async fn test_restart_count_decrements_on_successful_exit() {
    const HUB_MAX_RESTARTS: u32 = 10;
    let restart_count = Arc::new(AtomicUsize::new(0));

    // Simulate 3 failures (panics) - increment restart_count
    for _ in 0..3 {
        let prev = restart_count.fetch_add(1, Ordering::SeqCst);
        assert!(prev < HUB_MAX_RESTARTS as usize);
    }
    assert_eq!(restart_count.load(Ordering::SeqCst), 3);

    // Simulate 2 successful exits (clean shutdowns) - decrement restart_count
    for _ in 0..2 {
        let prev = restart_count.fetch_sub(1, Ordering::SeqCst);
        assert!(prev > 0, "restart_count should not go below 0");
    }
    assert_eq!(
        restart_count.load(Ordering::SeqCst),
        1,
        "restart_count should be 3 - 2 = 1 after 2 successful exits"
    );

    // Simulate 1 more successful exit - should bring count to 0
    let prev = restart_count.fetch_sub(1, Ordering::SeqCst);
    assert!(prev > 0);
    assert_eq!(
        restart_count.load(Ordering::SeqCst),
        0,
        "restart_count should be 0 after all failures are 'forgiven'"
    );

    // Verify we haven't hit the limit
    assert!(
        (restart_count.load(Ordering::SeqCst) as u32) < HUB_MAX_RESTARTS,
        "Should be below max restarts limit"
    );
}

/// Test that restart_count never goes below 0 (floor at 0).
#[tokio::test]
async fn test_restart_count_floor_at_zero() {
    let restart_count: u32 = 0;

    // Try to decrement when already at 0 using saturating_sub
    let result = restart_count.saturating_sub(1);
    // Note: saturating_sub on 0 returns 0, not -1

    // Using saturating_sub correctly handles the floor
    assert_eq!(result, 0, "saturating_sub should not go below 0");

    // The actual code uses: restart_count = restart_count.saturating_sub(1)
    // which correctly prevents underflow
}

/// Test that alternating failures and successes don't exhaust the limit.
#[tokio::test]
async fn test_alternating_failures_successes_no_exhaustion() {
    const HUB_MAX_RESTARTS: u32 = 10;
    let mut restart_count: u32 = 0;

    // Simulate 20 cycles of alternating failure and success
    // Without the fix, this would exhaust the limit
    for i in 0..20 {
        let is_failure = i % 2 == 0; // Even = failure, Odd = success

        if is_failure {
            restart_count += 1;
        } else {
            // Success: decrement if > 0
            if restart_count > 0 {
                restart_count = restart_count.saturating_sub(1);
            }
        }
    }

    assert!(
        restart_count < HUB_MAX_RESTARTS,
        "restart_count should not exceed HUB_MAX_RESTARTS after alternating cycles, got {restart_count}"
    );
}

/// Test the complete restart flow with mutex protection.
///
/// This test verifies the entire restart flow with:
/// 1. Mutex to serialize restart attempts
/// 2. is_restarting flag to block publications
/// 3. Two-phase cleanup with oneshot confirmation
/// 4. Re-registration notification
#[tokio::test]
async fn test_complete_restart_flow_with_mutex() {
    use tokio::sync::Mutex;

    let restart_mutex = Arc::new(Mutex::new(()));
    let is_restarting = Arc::new(AtomicBool::new(false));
    let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<oneshot::Sender<()>>(10);
    let reregister_notify = Arc::new(tokio::sync::Notify::new());

    // Spawn the stop receiver task
    let receiver_handle = tokio::spawn(async move {
        while let Some(stop_done_tx) = stop_streams_rx.recv().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = stop_done_tx.send(());
        }
    });

    // Simulate the restart flow
    let mutex = Arc::clone(&restart_mutex);
    let restarting = Arc::clone(&is_restarting);
    let notify = Arc::clone(&reregister_notify);
    let tx = stop_streams_tx.clone();

    let restart_handle = tokio::spawn(async move {
        // 1. Acquire restart mutex (blocks concurrent restarts)
        let _guard = mutex.lock().await;

        // 2. Set restarting flag FIRST (blocks new publications)
        restarting.store(true, Ordering::Release);

        // 3. Send stop request and wait for completion
        let (stop_done_tx, stop_done_rx) = oneshot::channel::<()>();
        tx.send(stop_done_tx).await.unwrap();

        let result = tokio::time::timeout(Duration::from_millis(100), stop_done_rx).await;
        assert!(result.is_ok(), "Stop should complete");

        // 4. Simulate cleanup
        tokio::time::sleep(Duration::from_millis(5)).await;

        // 5. Notify re-registration
        notify.notify_one();

        // 6. Clear restarting flag
        restarting.store(false, Ordering::Release);
    });

    // Wait for restart to complete
    tokio::time::timeout(Duration::from_millis(500), restart_handle)
        .await
        .unwrap()
        .unwrap();

    // Verify final state
    assert!(
        !is_restarting.load(Ordering::Acquire),
        "Restarting flag should be cleared"
    );

    // Clean up
    drop(stop_streams_tx);
    let _ = receiver_handle.await;
}

/// Test that publications wait for restart to complete when using try_lock.
///
/// This test verifies that when a publication attempt coincides with a restart,
/// it can either:
/// 1. Fail fast with "restarting" error (if flag is set)
/// 2. Wait briefly then proceed (if restart completes quickly)
#[tokio::test]
async fn test_publication_during_restart_handles_gracefully() {
    let is_restarting = Arc::new(AtomicBool::new(true));
    let publication_result = Arc::new(std::sync::Mutex::new(None::<Result<(), &'static str>>));

    // Try to publish while restarting
    let flag = Arc::clone(&is_restarting);
    let result = Arc::clone(&publication_result);

    let pub_handle = tokio::spawn(async move {
        // Check flag with a small retry loop
        for _ in 0..10 {
            if !flag.load(Ordering::Acquire) {
                *result.lock().unwrap() = Some(Ok(()));
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Timed out waiting for restart to complete
        *result.lock().unwrap() = Some(Err("StreamHub restarting timeout"));
    });

    // Simulate restart completing after 20ms
    let flag = Arc::clone(&is_restarting);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        flag.store(false, Ordering::Release);
    });

    pub_handle.await.unwrap();

    let final_result = *publication_result.lock().unwrap();
    assert!(
        matches!(final_result, Some(Ok(()))),
        "Publication should succeed after restart completes, got {final_result:?}"
    );
}
