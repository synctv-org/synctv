//! Cache invalidation tests for multi-replica settings (Task #85)
//!
//! Tests verify cache invalidation works correctly across multiple replicas
//! using Redis Pub/Sub or Streams.
//!
//! Run with: cargo test --test cache_invalidation_tests

use synctv_core::{
    cache::{CacheInvalidationService, InvalidationMessage},
    models::{RoomId, UserId},
};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::redis::Redis;
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::test]
async fn test_cache_invalidation_message_serialization() {
    let msg = InvalidationMessage::UserPermission {
        room_id: "room123".to_string(),
        user_id: "user456".to_string(),
    };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    let deserialized: InvalidationMessage = serde_json::from_str(&json)
        .expect("Failed to deserialize");

    assert_eq!(msg, deserialized);
}

#[tokio::test]
async fn test_cache_invalidation_broadcast_received() {
    let docker = Cli::default();
    let redis_container = docker.run(Redis::default());

    let redis_url = format!(
        "redis://127.0.0.1:{}",
        redis_container.get_host_port_ipv4(6379)
    );

    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");

    // Create two invalidation services (simulating two replicas)
    let service1 = Arc::new(CacheInvalidationService::new(
        Some(redis_client.clone()),
        "node1".to_string(),
        "test:cache:invalidate".to_string(),
    ));

    let service2 = Arc::new(CacheInvalidationService::new(
        Some(redis_client.clone()),
        "node2".to_string(),
        "test:cache:invalidate".to_string(),
    ));

    // Start listeners
    service1.start().await.expect("Failed to start service1");
    service2.start().await.expect("Failed to start service2");

    // Subscribe to service2's local channel
    let mut receiver = service2.subscribe();

    // Broadcast from service1
    let room_id = RoomId::new();
    let user_id = UserId::new();

    service1.invalidate_user_permission(&room_id, &user_id)
        .await
        .expect("Failed to broadcast invalidation");

    // Wait for message on service2
    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::UserPermission { room_id: r, user_id: u } => {
                    assert_eq!(r, room_id.as_str());
                    assert_eq!(u, user_id.as_str());
                }
                _ => panic!("Unexpected message type"),
            }
        }
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }
}

#[tokio::test]
async fn test_cache_invalidation_all_message() {
    let docker = Cli::default();
    let redis_container = docker.run(Redis::default());

    let redis_url = format!(
        "redis://127.0.0.1:{}",
        redis_container.get_host_port_ipv4(6379)
    );

    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");

    let service1 = Arc::new(CacheInvalidationService::new(
        Some(redis_client.clone()),
        "node1".to_string(),
        "test:cache:invalidate".to_string(),
    ));

    let service2 = Arc::new(CacheInvalidationService::new(
        Some(redis_client.clone()),
        "node2".to_string(),
        "test:cache:invalidate".to_string(),
    ));

    service1.start().await.expect("Failed to start service1");
    service2.start().await.expect("Failed to start service2");

    let mut receiver = service2.subscribe();

    // Broadcast invalidate all
    service1.invalidate_all()
        .await
        .expect("Failed to broadcast invalidation");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            assert_eq!(msg, InvalidationMessage::All);
        }
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }
}

#[tokio::test]
async fn test_cache_invalidation_room_permission() {
    let docker = Cli::default();
    let redis_container = docker.run(Redis::default());

    let redis_url = format!(
        "redis://127.0.0.1:{}",
        redis_container.get_host_port_ipv4(6379)
    );

    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");

    let service1 = CacheInvalidationService::new(
        Some(redis_client.clone()),
        "node1".to_string(),
        "test:cache:invalidate".to_string(),
    );

    let service2 = CacheInvalidationService::new(
        Some(redis_client),
        "node2".to_string(),
        "test:cache:invalidate".to_string(),
    );

    service1.start().await.expect("Failed to start service1");
    service2.start().await.expect("Failed to start service2");

    let mut receiver = service2.subscribe();

    let room_id = RoomId::new();
    service1.invalidate_room_permission(&room_id)
        .await
        .expect("Failed to broadcast invalidation");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::RoomPermission { room_id: r } => {
                    assert_eq!(r, room_id.as_str());
                }
                _ => panic!("Expected RoomPermission message"),
            }
        }
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }
}

#[tokio::test]
async fn test_cache_invalidation_multiple_messages() {
    let docker = Cli::default();
    let redis_container = docker.run(Redis::default());

    let redis_url = format!(
        "redis://127.0.0.1:{}",
        redis_container.get_host_port_ipv4(6379)
    );

    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");

    let service1 = Arc::new(CacheInvalidationService::new(
        Some(redis_client.clone()),
        "node1".to_string(),
        "test:cache:invalidate".to_string(),
    ));

    let service2 = Arc::new(CacheInvalidationService::new(
        Some(redis_client),
        "node2".to_string(),
        "test:cache:invalidate".to_string(),
    ));

    service1.start().await.expect("Failed to start service1");
    service2.start().await.expect("Failed to start service2");

    let received_messages = Arc::new(RwLock::new(Vec::new()));
    let received_clone = received_messages.clone();

    // Spawn receiver task
    let mut receiver = service2.subscribe();
    let receiver_handle = tokio::spawn(async move {
        for _ in 0..3 {
            if let Ok(msg) = receiver.recv().await {
                received_clone.write().await.push(msg);
            }
        }
    });

    // Send multiple invalidation messages
    let room1 = RoomId::new();
    let room2 = RoomId::new();
    let user1 = UserId::new();

    service1.invalidate_room(&room1).await.expect("Failed to invalidate room1");
    service1.invalidate_room(&room2).await.expect("Failed to invalidate room2");
    service1.invalidate_user(&user1).await.expect("Failed to invalidate user");

    // Wait for messages
    tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        receiver_handle
    ).await.expect("Timeout").expect("Receiver task failed");

    let messages = received_messages.read().await;
    assert_eq!(messages.len(), 3, "Should receive 3 messages");
}

#[tokio::test]
async fn test_cache_invalidation_without_redis() {
    // Service without Redis should work in local-only mode
    let service = CacheInvalidationService::new(
        None,
        "node1".to_string(),
        "test:cache:invalidate".to_string(),
    );

    service.start().await.expect("Failed to start service without Redis");

    let mut receiver = service.subscribe();

    // Local broadcast should still work
    let room_id = RoomId::new();
    service.invalidate_room(&room_id).await.expect("Failed to invalidate");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive local message");
            match msg {
                InvalidationMessage::Room { room_id: r } => {
                    assert_eq!(r, room_id.as_str());
                }
                _ => panic!("Expected Room message"),
            }
        }
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
            panic!("Timeout waiting for local message");
        }
    }
}

#[tokio::test]
async fn test_cache_invalidation_playback_state() {
    let docker = Cli::default();
    let redis_container = docker.run(Redis::default());

    let redis_url = format!(
        "redis://127.0.0.1:{}",
        redis_container.get_host_port_ipv4(6379)
    );

    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");

    let service1 = CacheInvalidationService::new(
        Some(redis_client.clone()),
        "node1".to_string(),
        "test:cache:invalidate".to_string(),
    );

    let service2 = CacheInvalidationService::new(
        Some(redis_client),
        "node2".to_string(),
        "test:cache:invalidate".to_string(),
    );

    service1.start().await.expect("Failed to start service1");
    service2.start().await.expect("Failed to start service2");

    let mut receiver = service2.subscribe();

    let room_id = RoomId::new();
    service1.invalidate_playback_state(&room_id)
        .await
        .expect("Failed to broadcast invalidation");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::PlaybackState { room_id: r } => {
                    assert_eq!(r, room_id.as_str());
                }
                _ => panic!("Expected PlaybackState message"),
            }
        }
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }
}
