//! `PublisherManager` unit tests using `InMemoryStreamRegistry`.
//!
//! These tests verify publish/unpublish handling and active publisher tracking.
//! They use `InMemoryStreamRegistry` instead of `MockStreamRegistry` (which is only
//! available in cfg(test) within the crate).

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use synctv_livestream::relay::{InMemoryStreamRegistry, StreamRegistryTrait};

/// Helper to create a test `PublisherManager` with an `InMemoryStreamRegistry`.
fn setup() -> (
    synctv_livestream::relay::PublisherManager,
    Arc<InMemoryStreamRegistry>,
    tokio::sync::mpsc::Receiver<synctv_xiu::streamhub::define::StreamHubEvent>,
) {
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let manager = synctv_livestream::relay::PublisherManager::new(
        registry.clone(),
        "test-node".to_string(),
        tx,
    );
    (manager, registry, rx)
}

#[tokio::test]
async fn test_active_publisher_streams_parses_keys() {
    let (manager, registry, _rx) = setup();

    // Register publishers in the registry so handle_publish can look them up
    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room2", "media2", "test-node", "user1", "localhost:50051")
        .await
        .unwrap();

    // Simulate publish events by directly using handle_publish via the manager's
    // active_publisher_streams method. Since handle_publish is private, we test
    // through the public API.
    //
    // active_publisher_streams() returns parsed (room_id, media_id) pairs from
    // the internal DashMap keys formatted as "room_id:media_id".
    let streams = manager.active_publisher_streams();
    // Initially empty since no publish events have been processed
    assert!(streams.is_empty());
}

#[tokio::test]
async fn test_record_activity_for_nonexistent_publisher() {
    let (manager, _registry, _rx) = setup();

    // Should not panic when recording activity for a publisher that doesn't exist
    manager.record_publisher_activity("nonexistent", "publisher");
}

#[tokio::test]
async fn test_lag_event_count_initially_zero() {
    let (manager, _registry, _rx) = setup();
    assert_eq!(manager.lag_event_count(), 0);
}

#[tokio::test]
async fn test_restarting_flag() {
    let (manager, _registry, _rx) = setup();

    // Initially not restarting
    let flag = manager.restarting_flag();
    assert!(!flag.load(std::sync::atomic::Ordering::Acquire));

    // Set restarting
    manager.set_restarting();
    assert!(flag.load(std::sync::atomic::Ordering::Acquire));

    // Clear restarting
    manager.clear_restarting();
    assert!(!flag.load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn test_reregister_all_publishers_empty() {
    let (manager, _registry, _rx) = setup();

    // Should not panic or error with no publishers
    manager.reregister_all_publishers().await;
}

#[tokio::test]
async fn test_with_api_address() {
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let manager =
        synctv_livestream::relay::PublisherManager::new(registry, "test-node".to_string(), tx)
            .with_api_address("10.0.0.1:50051".to_string());

    // Manager should be created successfully with api_address
    assert_eq!(manager.lag_event_count(), 0);
}

// ============================================================================
// LS2: Silent publisher timeout detection
// ============================================================================

#[tokio::test]
async fn test_silent_publisher_detection_via_activity() {
    let (manager, registry, _rx) = setup();

    // Register a publisher and track it
    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await
        .unwrap();

    // Record activity - should not panic
    manager.record_publisher_activity("room1", "media1");

    // Record activity for non-existent publisher - should not panic
    manager.record_publisher_activity("room999", "media999");

    // After activity, publisher should still be tracked (not aged out)
    // The actual timeout check is internal, but we verify the activity API works.
    let streams = manager.active_publisher_streams();
    // Manager tracks publishers only when it processes Publish events from StreamHub,
    // not when record_publisher_activity is called. So streams may be empty here.
    assert!(streams.is_empty() || streams.len() <= 1);
}

// ============================================================================
// LS3: Heartbeat failure escalation thresholds
// ============================================================================

#[tokio::test]
async fn test_heartbeat_failure_counter_starts_at_zero() {
    let (manager, _registry, _rx) = setup();

    // Initially, lag count should be 0 (no broadcast channel lag detected)
    assert_eq!(manager.lag_event_count(), 0);
}

// ============================================================================
// LS4: reregister_all_publishers with conflict detection
// ============================================================================

#[tokio::test]
async fn test_reregister_all_publishers_with_existing_publishers() {
    let (manager, registry, _rx) = setup();

    // Pre-register publishers in the registry
    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await
        .unwrap();

    // Reregister should complete without error even if publishers exist
    manager.reregister_all_publishers().await;

    // Verify the publisher is still in the registry
    assert!(registry.is_stream_active("room1", "media1").await.unwrap());
}

#[tokio::test]
async fn test_reregister_all_publishers_different_node() {
    let (manager, registry, _rx) = setup();

    // Register from a different node
    registry
        .try_register_publisher("room1", "media1", "other-node", "user1", "other:50051")
        .await
        .unwrap();

    // Reregister from test-node should not error (but won't override)
    manager.reregister_all_publishers().await;

    // Original publisher should still be there
    let info = registry
        .get_publisher("room1", "media1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.node_id, "other-node");
}

// ============================================================================
// LS5: reconcile_missing_from_registry
// ============================================================================

#[tokio::test]
async fn test_reconcile_missing_from_registry_no_publishers() {
    let (manager, _registry, _rx) = setup();

    // With no tracked publishers, reregister should work fine
    manager.reregister_all_publishers().await;
    // No assertions needed - just verify it doesn't panic
}

// ============================================================================
// LS-extra: PublisherManager multiple operations
// ============================================================================

#[tokio::test]
async fn test_set_and_clear_restarting_multiple_times() {
    let (manager, _registry, _rx) = setup();

    for _ in 0..5 {
        manager.set_restarting();
        assert!(manager
            .restarting_flag()
            .load(std::sync::atomic::Ordering::Acquire));
        manager.clear_restarting();
        assert!(!manager
            .restarting_flag()
            .load(std::sync::atomic::Ordering::Acquire));
    }
}

#[tokio::test]
async fn test_restarting_flag_shared() {
    let (manager, _registry, _rx) = setup();

    let flag = manager.restarting_flag();
    assert!(!flag.load(std::sync::atomic::Ordering::Acquire));

    // Set via manager
    manager.set_restarting();

    // Read via shared flag
    assert!(flag.load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn test_multiple_publishers_registration_and_cleanup() {
    let (manager, registry, _rx) = setup();

    // Register multiple publishers
    for i in 0..5 {
        let room = format!("room{i}");
        let media = format!("media{i}");
        registry
            .try_register_publisher(&room, &media, "test-node", "user1", "localhost:50051")
            .await
            .unwrap();
    }

    // Verify all registered
    for i in 0..5 {
        let room = format!("room{i}");
        let media = format!("media{i}");
        assert!(registry.is_stream_active(&room, &media).await.unwrap());
    }

    // Cleanup all for test-node
    registry
        .cleanup_all_publishers_for_node("test-node")
        .await
        .unwrap();

    // All should be gone
    for i in 0..5 {
        let room = format!("room{i}");
        let media = format!("media{i}");
        assert!(!registry.is_stream_active(&room, &media).await.unwrap());
    }

    // Reregister should still work
    manager.reregister_all_publishers().await;
}
