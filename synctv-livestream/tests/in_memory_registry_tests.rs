//! Tests for `InMemoryStreamRegistry` (standalone mode without Redis).
//!
//! These tests verify that the in-memory registry provides the same
//! semantics as the Redis-backed `StreamRegistry`.

#![allow(clippy::unwrap_used)]
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
    let info = registry
        .get_publisher("room1", "media1")
        .await
        .unwrap()
        .unwrap();
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

    registry
        .unregister_publisher("room1", "media1")
        .await
        .unwrap();

    assert!(!registry.is_stream_active("room1", "media1").await.unwrap());
    assert!(registry
        .get_publisher("room1", "media1")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_validate_epoch_correct() {
    let registry = InMemoryStreamRegistry::new();

    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();

    let info = registry
        .get_publisher("room1", "media1")
        .await
        .unwrap()
        .unwrap();
    let valid = registry
        .validate_epoch("room1", "media1", info.epoch)
        .await
        .unwrap();
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
    let valid = registry
        .validate_epoch("room1", "media1", 999)
        .await
        .unwrap();
    assert!(!valid);

    // Non-existent stream should be invalid
    let valid = registry
        .validate_epoch("nonexistent", "media", 1)
        .await
        .unwrap();
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
    registry
        .cleanup_all_publishers_for_node("node1")
        .await
        .unwrap();

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
    let info1 = registry
        .get_publisher("room1", "media1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info1.epoch, 1);

    // Unregister and re-register: epoch should increment
    registry
        .unregister_publisher("room1", "media1")
        .await
        .unwrap();

    registry
        .try_register_publisher("room1", "media1", "node2", "user2", "localhost:50052")
        .await
        .unwrap();
    let info2 = registry
        .get_publisher("room1", "media1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        info2.epoch,
        info1.epoch + 1,
        "epochs must remain monotonic across unregister for fencing safety"
    );
    assert_eq!(info2.node_id, "node2");
}

#[tokio::test]
async fn test_epoch_does_not_reset_after_many_other_streams_churn() {
    let registry = InMemoryStreamRegistry::new();

    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();
    let first = registry
        .get_publisher("room1", "media1")
        .await
        .unwrap()
        .unwrap();
    registry
        .unregister_publisher("room1", "media1")
        .await
        .unwrap();

    for idx in 0..5000 {
        let room_id = format!("room-churn-{idx}");
        let media_id = format!("media-churn-{idx}");
        registry
            .try_register_publisher(&room_id, &media_id, "node-x", "user-x", "localhost:50051")
            .await
            .unwrap();
        registry
            .unregister_publisher(&room_id, &media_id)
            .await
            .unwrap();
    }

    registry
        .try_register_publisher("room1", "media1", "node2", "user2", "localhost:50052")
        .await
        .unwrap();
    let second = registry
        .get_publisher("room1", "media1")
        .await
        .unwrap()
        .unwrap();

    assert!(
        second.epoch > first.epoch,
        "epochs must remain monotonic even after heavy churn on unrelated stream ids"
    );
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

    let info = registry
        .get_publisher("room1", "media1")
        .await
        .unwrap()
        .unwrap();
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

    registry
        .unregister_all_user_publishers("user1")
        .await
        .unwrap();

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

// ============================================================================
// LS8: Concurrent InMemoryStreamRegistry tests
// ============================================================================

#[tokio::test]
async fn test_concurrent_register_same_stream() {
    let registry = std::sync::Arc::new(InMemoryStreamRegistry::new());

    let mut handles = Vec::new();
    for i in 0..10 {
        let reg = registry.clone();
        let node = format!("node{i}");
        let user = format!("user{i}");
        handles.push(tokio::spawn(async move {
            reg.try_register_publisher("room1", "media1", &node, &user, "localhost:50051")
                .await
                .unwrap()
        }));
    }

    let results: Vec<bool> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // Exactly one should succeed (first-come-first-served)
    let success_count = results.iter().filter(|&&r| r).count();
    assert_eq!(
        success_count, 1,
        "Exactly one concurrent registration should succeed, got {success_count}"
    );

    // The publisher should exist
    let info = registry
        .get_publisher("room1", "media1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.epoch, 1);
}

#[tokio::test]
async fn test_concurrent_register_different_streams() {
    let registry = std::sync::Arc::new(InMemoryStreamRegistry::new());

    let mut handles = Vec::new();
    for i in 0..10 {
        let reg = registry.clone();
        let room = format!("room{i}");
        let media = format!("media{i}");
        handles.push(tokio::spawn(async move {
            reg.try_register_publisher(&room, &media, "node1", "user1", "localhost:50051")
                .await
                .unwrap()
        }));
    }

    let results: Vec<bool> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // All should succeed (different streams)
    assert!(
        results.iter().all(|&r| r),
        "All registrations for different streams should succeed"
    );

    let streams = registry.list_active_streams().await.unwrap();
    assert_eq!(streams.len(), 10);
}

#[tokio::test]
async fn test_concurrent_register_and_unregister() {
    let registry = std::sync::Arc::new(InMemoryStreamRegistry::new());

    // First register some publishers
    for i in 0..5 {
        let room = format!("room{i}");
        let media = format!("media{i}");
        registry
            .try_register_publisher(&room, &media, "node1", "user1", "localhost:50051")
            .await
            .unwrap();
    }

    // Concurrently unregister some and register others
    let mut handles = Vec::new();
    for i in 0..5 {
        let reg = registry.clone();
        let room = format!("room{i}");
        let media = format!("media{i}");
        handles.push(tokio::spawn(async move {
            reg.unregister_publisher(&room, &media).await.unwrap();
        }));
    }
    for i in 5..10 {
        let reg = registry.clone();
        let room = format!("room{i}");
        let media = format!("media{i}");
        handles.push(tokio::spawn(async move {
            reg.try_register_publisher(&room, &media, "node1", "user1", "localhost:50051")
                .await
                .unwrap();
        }));
    }

    futures::future::join_all(handles).await;

    // The first 5 should be gone, the next 5 should exist
    for i in 0..5 {
        let room = format!("room{i}");
        let media = format!("media{i}");
        assert!(
            !registry.is_stream_active(&room, &media).await.unwrap(),
            "room{i} should be unregistered"
        );
    }
    for i in 5..10 {
        let room = format!("room{i}");
        let media = format!("media{i}");
        assert!(
            registry.is_stream_active(&room, &media).await.unwrap(),
            "room{i} should be registered"
        );
    }
}

#[tokio::test]
async fn test_concurrent_cleanup_all_for_node() {
    let registry = std::sync::Arc::new(InMemoryStreamRegistry::new());

    // Register on multiple nodes
    for i in 0..5 {
        let room = format!("room{i}");
        registry
            .try_register_publisher(&room, "media1", "node1", "user1", "localhost:50051")
            .await
            .unwrap();
    }
    for i in 5..10 {
        let room = format!("room{i}");
        registry
            .try_register_publisher(&room, "media1", "node2", "user2", "localhost:50052")
            .await
            .unwrap();
    }

    // Concurrently cleanup both nodes
    let reg1 = registry.clone();
    let reg2 = registry.clone();
    let h1 = tokio::spawn(async move {
        reg1.cleanup_all_publishers_for_node("node1").await.unwrap();
    });
    let h2 = tokio::spawn(async move {
        reg2.cleanup_all_publishers_for_node("node2").await.unwrap();
    });

    h1.await.unwrap();
    h2.await.unwrap();

    // All should be cleaned up
    let streams = registry.list_active_streams().await.unwrap();
    assert!(
        streams.is_empty(),
        "All publishers should be cleaned up, got: {streams:?}"
    );
}

#[tokio::test]
async fn test_concurrent_user_publishers() {
    let registry = std::sync::Arc::new(InMemoryStreamRegistry::new());

    // Register multiple publishers for same user concurrently
    let mut handles = Vec::new();
    for i in 0..5 {
        let reg = registry.clone();
        let room = format!("room{i}");
        let media = format!("media{i}");
        handles.push(tokio::spawn(async move {
            reg.try_register_publisher(&room, &media, "node1", "user1", "localhost:50051")
                .await
                .unwrap()
        }));
    }

    futures::future::join_all(handles).await;

    let user_pubs = registry.get_user_publishers("user1").await.unwrap();
    assert_eq!(user_pubs.len(), 5);

    // Unregister all for user concurrently
    registry
        .unregister_all_user_publishers("user1")
        .await
        .unwrap();

    let user_pubs = registry.get_user_publishers("user1").await.unwrap();
    assert!(user_pubs.is_empty());
}

#[tokio::test]
async fn test_refresh_publisher_ttl_no_error() {
    let registry = InMemoryStreamRegistry::new();

    registry
        .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
        .await
        .unwrap();

    // refresh_publisher_ttl should succeed (no-op for in-memory)
    let result = registry
        .refresh_publisher_ttl("room1", "media1", "user1")
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_refresh_publisher_ttl_nonexistent() {
    let registry = InMemoryStreamRegistry::new();

    // Should not error even for non-existent publishers
    let result = registry
        .refresh_publisher_ttl("nonexistent", "media", "user")
        .await;
    assert!(result.is_ok());
}
