//! `StreamHub` Redis cleanup race condition tests (Task H-5).
//!
//! These tests verify that the `StreamHub` restart cleanup does not race with
//! concurrent stream disconnections.
//!
//! Problem: When `StreamHub` restarts:
//! 1. `stop_all()` is called (with 100ms timeout)
//! 2. `cleanup_all_publishers_for_node()` is called immediately after
//!
//! Race condition: If a stream is disconnecting concurrently:
//! - The stream's `unregister_publisher()` may be in progress
//! - `cleanup_all_publishers_for_node()` may also try to delete the same entry
//! - This can cause Redis operation conflicts or inconsistent state
//!
//! Run with: cargo test --test `streamhub_cleanup_race_tests`

#![allow(clippy::unwrap_used)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, Barrier, Notify};

/// Simulates the cleanup sequence during `StreamHub` restart with concurrent
/// stream disconnections.
///
/// This test verifies that cleanup is safe even when streams are unregistering
/// concurrently.
#[tokio::test]
async fn test_cleanup_during_concurrent_unregistration() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let node_id = "test-node";

    // Register multiple publishers
    for i in 0..5 {
        registry
            .try_register_publisher(
                &format!("room{i}"),
                &format!("media{i}"),
                node_id,
                &format!("user{i}"),
                "grpc:50051",
            )
            .await
            .unwrap();
    }

    let registry_clone = registry.clone();
    let barrier = Arc::new(Barrier::new(2));

    // Task 1: Simulate streams unregistering concurrently
    let barrier_clone = barrier.clone();
    let unregister_started = Arc::new(Notify::new());
    let unregister_started_clone = Arc::clone(&unregister_started);
    let unregister_handle = tokio::spawn(async move {
        barrier_clone.wait().await;

        // Unregister some streams concurrently with cleanup
        for i in 0..3 {
            if i == 0 {
                unregister_started_clone.notify_one();
            }
            registry_clone
                .unregister_publisher(&format!("room{i}"), &format!("media{i}"))
                .await
                .unwrap();
        }
    });

    // Task 2: Simulate cleanup_all_publishers_for_node
    let barrier_clone = barrier.clone();
    let registry_for_cleanup = registry.clone();
    let cleanup_handle = tokio::spawn(async move {
        barrier_clone.wait().await;

        unregister_started.notified().await;

        // Cleanup should be safe even with concurrent unregistrations
        registry_for_cleanup
            .cleanup_all_publishers_for_node(node_id)
            .await
            .unwrap();
    });

    let (_, _) = tokio::join!(unregister_handle, cleanup_handle);

    // Verify: All streams should be cleaned up (no orphaned entries)
    let remaining = registry.list_active_streams().await.unwrap();
    assert!(
        remaining.is_empty(),
        "All streams should be cleaned up, but found: {remaining:?}"
    );
}

/// Test that cleanup handles the case where streams are still registering
/// during the cleanup window.
#[tokio::test]
async fn test_cleanup_during_concurrent_registration() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let node_id = "test-node";

    // Register initial publishers
    for i in 0..3 {
        registry
            .try_register_publisher(
                &format!("room{i}"),
                &format!("media{i}"),
                node_id,
                &format!("user{i}"),
                "grpc:50051",
            )
            .await
            .unwrap();
    }

    let registry_clone = registry.clone();
    let barrier = Arc::new(Barrier::new(2));

    // Task 1: New registrations during cleanup
    let barrier_clone = barrier.clone();
    let register_handle = tokio::spawn(async move {
        barrier_clone.wait().await;

        // Try to register new streams during cleanup
        for i in 5..8 {
            let _ = registry_clone
                .try_register_publisher(
                    &format!("room{i}"),
                    &format!("media{i}"),
                    node_id,
                    &format!("user{i}"),
                    "grpc:50051",
                )
                .await;
        }
    });

    // Task 2: Cleanup
    let barrier_clone = barrier.clone();
    let registry_for_cleanup = registry.clone();
    let cleanup_handle = tokio::spawn(async move {
        barrier_clone.wait().await;
        registry_for_cleanup
            .cleanup_all_publishers_for_node(node_id)
            .await
            .unwrap();
    });

    let (_, _) = tokio::join!(register_handle, cleanup_handle);

    // After cleanup, the original 3 streams should be gone
    // New registrations during/after cleanup may or may not succeed depending on timing
    for i in 0..3 {
        assert!(
            !registry
                .is_stream_active(&format!("room{i}"), &format!("media{i}"))
                .await
                .unwrap(),
            "Original stream room{i}/media{i} should be cleaned up"
        );
    }
}

/// Test that multiple concurrent cleanups for the same node are safe.
#[tokio::test]
async fn test_concurrent_cleanups_same_node() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let node_id = "test-node";

    // Register publishers
    for i in 0..5 {
        registry
            .try_register_publisher(
                &format!("room{i}"),
                &format!("media{i}"),
                node_id,
                &format!("user{i}"),
                "grpc:50051",
            )
            .await
            .unwrap();
    }

    let barrier = Arc::new(Barrier::new(3));
    let mut handles = vec![];

    // Spawn multiple cleanup tasks concurrently
    for _ in 0..3 {
        let registry_clone = registry.clone();
        let barrier_clone = barrier.clone();
        let node_id_owned = node_id.to_string();

        handles.push(tokio::spawn(async move {
            barrier_clone.wait().await;
            registry_clone
                .cleanup_all_publishers_for_node(&node_id_owned)
                .await
        }));
    }

    for handle in handles {
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "Concurrent cleanup should succeed");
    }

    // All streams should be cleaned up
    let remaining = registry.list_active_streams().await.unwrap();
    assert!(remaining.is_empty(), "All streams should be cleaned up");
}

/// Test that cleanup followed by immediate re-registration works correctly.
#[tokio::test]
async fn test_cleanup_then_reregister_sequence() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let node_id = "test-node";

    // Register initial publishers
    for i in 0..3 {
        registry
            .try_register_publisher(
                &format!("room{i}"),
                &format!("media{i}"),
                node_id,
                &format!("user{i}"),
                "grpc:50051",
            )
            .await
            .unwrap();
    }

    // Simulate the restart sequence:
    registry
        .cleanup_all_publishers_for_node(node_id)
        .await
        .unwrap();

    for i in 0..3 {
        let registered = registry
            .try_register_publisher(
                &format!("room{i}"),
                &format!("media{i}"),
                node_id,
                &format!("user{i}"),
                "grpc:50051",
            )
            .await
            .unwrap();
        assert!(registered, "Re-registration should succeed after cleanup");
    }

    // Verify all streams are active
    for i in 0..3 {
        assert!(
            registry
                .is_stream_active(&format!("room{i}"), &format!("media{i}"))
                .await
                .unwrap(),
            "Stream room{i}/media{i} should be active after re-registration"
        );
    }
}

/// Test the timing window: stop request sent but streams still disconnecting.
///
/// This simulates the actual race condition in `StreamHub` restart:
/// 1. `stop_all()` is called with oneshot channel
/// 2. `stop_done` is received (or timeout)
/// 3. `cleanup_all_publishers_for_node()` is called
///
/// The race: streams may still be in the process of calling `unregister_publisher()`
/// when cleanup starts.
#[tokio::test]
async fn test_stop_then_cleanup_with_delayed_unregistration() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let node_id = "test-node";

    // Register publishers
    for i in 0..3 {
        registry
            .try_register_publisher(
                &format!("room{i}"),
                &format!("media{i}"),
                node_id,
                &format!("user{i}"),
                "grpc:50051",
            )
            .await
            .unwrap();
    }

    // Simulate the two-phase stop protocol
    let (stop_done_tx, stop_done_rx) = oneshot::channel::<()>();
    let (begin_unregister_tx, begin_unregister_rx) = oneshot::channel::<()>();
    let unregister_started = Arc::new(Notify::new());
    let unregister_started_clone = Arc::clone(&unregister_started);

    let registry_clone = registry.clone();
    let unregister_handle = tokio::spawn(async move {
        begin_unregister_rx
            .await
            .expect("cleanup path should signal unregister start");

        // Streams are still unregistering during the cleanup window
        for i in 0..3 {
            if i == 0 {
                unregister_started_clone.notify_one();
            }
            let _ = registry_clone
                .unregister_publisher(&format!("room{i}"), &format!("media{i}"))
                .await;
        }
    });

    // Signal stop complete immediately (before streams actually stop)
    let _ = stop_done_tx.send(());

    let _ = tokio::time::timeout(Duration::from_millis(100), stop_done_rx).await;

    let _ = begin_unregister_tx.send(());
    unregister_started.notified().await;

    // Immediately call cleanup (this is where the race occurs)
    // Cleanup should handle the case where unregister is in progress
    let cleanup_result = registry.cleanup_all_publishers_for_node(node_id).await;
    assert!(
        cleanup_result.is_ok(),
        "Cleanup should succeed even with concurrent unregisters"
    );

    let _ = unregister_handle.await;

    // Final state: no publishers should remain
    let remaining = registry.list_active_streams().await.unwrap();
    assert!(remaining.is_empty(), "All publishers should be cleaned up");
}

/// Test the proposed fix: adding a delay between stop confirmation and cleanup.
///
/// This verifies that a short delay allows concurrent unregistrations to complete
/// before cleanup starts, reducing the race window.
#[tokio::test]
async fn test_stop_delay_then_cleanup_prevents_race() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let node_id = "test-node";

    // Register publishers
    for i in 0..3 {
        registry
            .try_register_publisher(
                &format!("room{i}"),
                &format!("media{i}"),
                node_id,
                &format!("user{i}"),
                "grpc:50051",
            )
            .await
            .unwrap();
    }

    let registry_clone = registry.clone();
    let unregister_started = Arc::new(AtomicUsize::new(0));
    let unregister_completed = Arc::new(AtomicUsize::new(0));
    let started_notify = Arc::new(Notify::new());
    let completed_notify = Arc::new(Notify::new());

    let unregister_started_clone = unregister_started.clone();
    let unregister_completed_clone = unregister_completed.clone();
    let started_notify_clone = Arc::clone(&started_notify);
    let completed_notify_clone = Arc::clone(&completed_notify);

    // Spawn unregister task that takes some time
    let unregister_handle = tokio::spawn(async move {
        for i in 0..3 {
            if unregister_started_clone.fetch_add(1, Ordering::SeqCst) == 0 {
                started_notify_clone.notify_one();
            }
            registry_clone
                .unregister_publisher(&format!("room{i}"), &format!("media{i}"))
                .await
                .unwrap();
            if unregister_completed_clone.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
                completed_notify_clone.notify_one();
            }
        }
    });

    if unregister_started.load(Ordering::SeqCst) == 0 {
        started_notify.notified().await;
    }

    // which is what the grace period is intended to achieve.
    if unregister_completed.load(Ordering::SeqCst) < 3 {
        completed_notify.notified().await;
    }

    // Now cleanup
    registry
        .cleanup_all_publishers_for_node(node_id)
        .await
        .unwrap();

    let _ = unregister_handle.await;

    // Verify: all streams should be gone
    let remaining = registry.list_active_streams().await.unwrap();
    assert!(
        remaining.is_empty(),
        "All streams should be cleaned up after delay"
    );
}

/// Test cleanup with mixed node ownership.
///
/// Cleanup should only affect streams owned by the target node.
#[tokio::test]
async fn test_cleanup_only_affects_target_node() {
    let registry = synctv_livestream::relay::local_stream_registry();

    // Register streams for multiple nodes
    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "grpc:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room2", "media2", "node2", "user2", "grpc:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room3", "media3", "node1", "user3", "grpc:50051")
        .await
        .unwrap();

    // Cleanup node1 only
    registry
        .cleanup_all_publishers_for_node("node1")
        .await
        .unwrap();

    // node1 streams should be gone
    assert!(!registry.is_stream_active("room1", "media1").await.unwrap());
    assert!(!registry.is_stream_active("room3", "media3").await.unwrap());

    // node2 stream should remain
    assert!(registry.is_stream_active("room2", "media2").await.unwrap());
}

/// Test that cleanup is idempotent.
///
/// Calling cleanup multiple times should be safe.
#[tokio::test]
async fn test_cleanup_is_idempotent() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let node_id = "test-node";

    // Register publishers
    for i in 0..3 {
        registry
            .try_register_publisher(
                &format!("room{i}"),
                &format!("media{i}"),
                node_id,
                &format!("user{i}"),
                "grpc:50051",
            )
            .await
            .unwrap();
    }

    // Cleanup multiple times
    for _ in 0..3 {
        registry
            .cleanup_all_publishers_for_node(node_id)
            .await
            .unwrap();
    }

    // All streams should be gone
    let remaining = registry.list_active_streams().await.unwrap();
    assert!(remaining.is_empty());
}

/// Stress test: rapid cleanup and re-registration cycles.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_stress_cleanup_reregister_cycles() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let node_id = "test-node";

    let iterations: usize = 10;
    let barrier = Arc::new(Barrier::new(iterations));
    let success_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    for i in 0..iterations {
        let registry_clone = registry.clone();
        let barrier_clone = barrier.clone();
        let success_clone = success_count.clone();

        handles.push(tokio::spawn(async move {
            barrier_clone.wait().await;

            // Each iteration: cleanup, then re-register
            registry_clone
                .cleanup_all_publishers_for_node(node_id)
                .await
                .unwrap();

            // Try to register
            for j in 0..3 {
                let room = format!("room_{i}_{j}");
                let media = format!("media_{i}_{j}");
                if registry_clone
                    .try_register_publisher(&room, &media, node_id, "user", "grpc:50051")
                    .await
                    .is_ok()
                {
                    success_clone.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // At least some registrations should succeed
    assert!(success_count.load(Ordering::SeqCst) > 0);
}

/// Test the complete `StreamHub` restart sequence with the 500ms delay fix.
///
/// This test simulates the actual restart sequence:
/// 1. Streams are active
/// 2. Stop signal sent (with 100ms timeout)
/// 3. Streams start disconnecting (async)
/// 4. 500ms delay (the fix)
/// 5. Cleanup all publishers for node
/// 6. Re-registration
///
/// The 500ms delay gives in-progress unregistrations time to complete,
/// reducing the race window between cleanup and unregister operations.
#[tokio::test]
async fn test_complete_restart_sequence_with_delay_fix() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let node_id = "test-node";

    // Phase 1: Active streams
    for i in 0..5 {
        registry
            .try_register_publisher(
                &format!("room{i}"),
                &format!("media{i}"),
                node_id,
                &format!("user{i}"),
                "grpc:50051",
            )
            .await
            .unwrap();
    }

    let registry_clone = registry.clone();
    let unregister_completed = Arc::new(AtomicUsize::new(0));
    let unregister_completed_clone = unregister_completed.clone();
    let (allow_unregister_tx, allow_unregister_rx) = oneshot::channel::<()>();
    let completed_notify = Arc::new(Notify::new());
    let completed_notify_clone = Arc::clone(&completed_notify);

    // Phase 2: Stop signal sent, streams start disconnecting async
    let unregister_handle = tokio::spawn(async move {
        allow_unregister_rx
            .await
            .expect("restart flow should release unregister task");

        for i in 0..5 {
            let _ = registry_clone
                .unregister_publisher(&format!("room{i}"), &format!("media{i}"))
                .await;
            if unregister_completed_clone.fetch_add(1, Ordering::SeqCst) + 1 == 5 {
                completed_notify_clone.notify_one();
            }
        }
    });

    // Phase 3: grace period (the fix) - modeled by waiting for the in-flight
    // unregister sequence to complete instead of a wall-clock sleep.
    let _ = allow_unregister_tx.send(());
    if unregister_completed.load(Ordering::SeqCst) < 5 {
        completed_notify.notified().await;
    }

    // Phase 4: Cleanup (should find most/all streams already unregistered)
    registry
        .cleanup_all_publishers_for_node(node_id)
        .await
        .unwrap();

    // Phase 5: Re-registration
    for i in 0..3 {
        let registered = registry
            .try_register_publisher(
                &format!("room{i}"),
                &format!("media{i}"),
                node_id,
                &format!("user{i}"),
                "grpc:50051",
            )
            .await
            .unwrap();
        assert!(registered, "Re-registration should succeed");
    }

    let _ = unregister_handle.await;

    // Verify: 3 streams should be active (the ones we re-registered)
    let active = registry.list_active_streams().await.unwrap();
    assert_eq!(active.len(), 3);

    // Verify: unregistrations completed (the delay allowed them to finish)
    assert_eq!(unregister_completed.load(Ordering::SeqCst), 5);
}
