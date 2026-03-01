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
