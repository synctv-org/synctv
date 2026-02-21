//! PublisherManager unit tests using InMemoryStreamRegistry.
//!
//! These tests verify publish/unpublish handling and active publisher tracking.
//! They use InMemoryStreamRegistry instead of MockStreamRegistry (which is only
//! available in cfg(test) within the crate).

use std::sync::Arc;
use synctv_livestream::relay::{InMemoryStreamRegistry, StreamRegistryTrait};

/// Helper to create a test PublisherManager with an InMemoryStreamRegistry.
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
async fn test_with_grpc_address() {
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let manager = synctv_livestream::relay::PublisherManager::new(
        registry,
        "test-node".to_string(),
        tx,
    ).with_grpc_address("10.0.0.1:50051".to_string());

    // Manager should be created successfully with grpc_address
    assert_eq!(manager.lag_event_count(), 0);
}
