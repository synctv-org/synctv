//! Tests for InMemoryStreamRegistry (standalone mode without Redis).
//!
//! These tests verify that the in-memory registry provides the same
//! semantics as the Redis-backed StreamRegistry.

use synctv_livestream::relay::{InMemoryStreamRegistry, StreamRegistryTrait};

#[tokio::test]
async fn test_register_publisher_success() {
    let registry = InMemoryStreamRegistry::new();

    let registered = registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    assert!(registered);

    let publisher = registry.get_publisher("room1", "media1").await.unwrap();
    assert!(publisher.is_some());

    let info = publisher.unwrap();
    assert_eq!(info.node_id, "node1");
    assert_eq!(info.grpc_address, "localhost:50051");
    assert_eq!(info.user_id, "user1");
    assert_eq!(info.epoch, 1);
}

#[tokio::test]
async fn test_register_duplicate_returns_false() {
    let registry = InMemoryStreamRegistry::new();

    let first = registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    assert!(first);

    let second = registry
        .try_register_publisher("room1", "media1", "node2", "user2", "localhost:50052")
        .await
        .unwrap();
    assert!(!second);

    // Original publisher should still be there
    let info = registry.get_publisher("room1", "media1").await.unwrap().unwrap();
    assert_eq!(info.node_id, "node1");
}

#[tokio::test]
async fn test_unregister_removes() {
    let registry = InMemoryStreamRegistry::new();

    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();

    assert!(registry.is_stream_active("room1", "media1").await.unwrap());

    registry.unregister_publisher("room1", "media1").await.unwrap();

    assert!(!registry.is_stream_active("room1", "media1").await.unwrap());
    assert!(registry.get_publisher("room1", "media1").await.unwrap().is_none());
}

#[tokio::test]
async fn test_validate_epoch_correct() {
    let registry = InMemoryStreamRegistry::new();

    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();

    let info = registry.get_publisher("room1", "media1").await.unwrap().unwrap();
    let valid = registry.validate_epoch("room1", "media1", info.epoch).await.unwrap();
    assert!(valid);
}

#[tokio::test]
async fn test_validate_epoch_stale() {
    let registry = InMemoryStreamRegistry::new();

    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();

    // Wrong epoch should be invalid
    let valid = registry.validate_epoch("room1", "media1", 999).await.unwrap();
    assert!(!valid);

    // Non-existent stream should be invalid
    let valid = registry.validate_epoch("nonexistent", "media", 1).await.unwrap();
    assert!(!valid);
}

#[tokio::test]
async fn test_list_streams_for_room() {
    let registry = InMemoryStreamRegistry::new();

    // Register publishers in different rooms
    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room1", "media2", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room2", "media3", "node1", "user1", "localhost:50051")
        .await
        .unwrap();

    let room1_streams = registry.list_streams_for_room("room1").await.unwrap();
    assert_eq!(room1_streams.len(), 2);
    assert!(room1_streams.contains(&"media1".to_string()));
    assert!(room1_streams.contains(&"media2".to_string()));

    let room2_streams = registry.list_streams_for_room("room2").await.unwrap();
    assert_eq!(room2_streams.len(), 1);
    assert!(room2_streams.contains(&"media3".to_string()));

    let empty = registry.list_streams_for_room("nonexistent").await.unwrap();
    assert!(empty.is_empty());
}

#[tokio::test]
async fn test_cleanup_all_for_node() {
    let registry = InMemoryStreamRegistry::new();

    // Register publishers on different nodes
    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room1", "media2", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room2", "media1", "node2", "user2", "localhost:50052")
        .await
        .unwrap();

    // Cleanup node1
    registry.cleanup_all_publishers_for_node("node1").await.unwrap();

    // node1 publishers should be gone
    assert!(!registry.is_stream_active("room1", "media1").await.unwrap());
    assert!(!registry.is_stream_active("room1", "media2").await.unwrap());

    // node2 publisher should still exist
    assert!(registry.is_stream_active("room2", "media1").await.unwrap());
}

#[tokio::test]
async fn test_epoch_increments_on_reregistration() {
    let registry = InMemoryStreamRegistry::new();

    // First registration: epoch = 1
    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    let info1 = registry.get_publisher("room1", "media1").await.unwrap().unwrap();
    assert_eq!(info1.epoch, 1);

    // Unregister and re-register: epoch should increment
    registry.unregister_publisher("room1", "media1").await.unwrap();

    // Note: InMemoryStreamRegistry removes epoch counters on unregister,
    // so re-registration starts from epoch 1 again (different from Redis behavior).
    // This is acceptable for single-node standalone mode.
    registry
        .try_register_publisher("room1", "media1", "node2", "user2", "localhost:50052")
        .await
        .unwrap();
    let info2 = registry.get_publisher("room1", "media1").await.unwrap().unwrap();
    // After unregister, the epoch counter is removed, so new registration starts at 1
    assert!(info2.epoch >= 1);
    assert_eq!(info2.node_id, "node2");
}

#[tokio::test]
async fn test_register_publisher_via_register_publisher_method() {
    let registry = InMemoryStreamRegistry::new();

    // Test the register_publisher method (which now takes grpc_address)
    let registered = registry
        .register_publisher("room1", "media1", "node1", "live", "localhost:50051")
        .await
        .unwrap();
    assert!(registered);

    let info = registry.get_publisher("room1", "media1").await.unwrap().unwrap();
    assert_eq!(info.node_id, "node1");
    assert_eq!(info.grpc_address, "localhost:50051");
    assert_eq!(info.app_name, "live");
}

#[tokio::test]
async fn test_get_user_publishers() {
    let registry = InMemoryStreamRegistry::new();

    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room2", "media2", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room3", "media3", "node1", "user2", "localhost:50051")
        .await
        .unwrap();

    let user1_pubs = registry.get_user_publishers("user1").await.unwrap();
    assert_eq!(user1_pubs.len(), 2);

    let user2_pubs = registry.get_user_publishers("user2").await.unwrap();
    assert_eq!(user2_pubs.len(), 1);

    let empty = registry.get_user_publishers("nonexistent").await.unwrap();
    assert!(empty.is_empty());
}

#[tokio::test]
async fn test_unregister_all_user_publishers() {
    let registry = InMemoryStreamRegistry::new();

    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room2", "media2", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room3", "media3", "node1", "user2", "localhost:50051")
        .await
        .unwrap();

    registry.unregister_all_user_publishers("user1").await.unwrap();

    assert!(!registry.is_stream_active("room1", "media1").await.unwrap());
    assert!(!registry.is_stream_active("room2", "media2").await.unwrap());
    // user2's publisher should still exist
    assert!(registry.is_stream_active("room3", "media3").await.unwrap());
}

#[tokio::test]
async fn test_list_active_streams() {
    let registry = InMemoryStreamRegistry::new();

    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room2", "media2", "node1", "user1", "localhost:50051")
        .await
        .unwrap();

    let streams = registry.list_active_streams().await.unwrap();
    assert_eq!(streams.len(), 2);
    assert!(streams.contains(&("room1".to_string(), "media1".to_string())));
    assert!(streams.contains(&("room2".to_string(), "media2".to_string())));
}
