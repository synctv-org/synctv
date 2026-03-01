//! Multi-replica cluster integration tests
//!
//! These tests verify cross-node coordination by starting multiple
//! `ClusterManager` instances that share a single Redis container
//! (via testcontainers). Each "node" has its own `node_id` but connects
//! to the same Redis, simulating a multi-replica deployment.

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use chrono::Utc;
use synctv_cluster::sync::events::ClusterEvent;
use synctv_cluster::{ClusterConfig, ClusterManager};
use synctv_core::models::id::{RoomId, UserId};
mod integration_test_helpers;
use integration_test_helpers::{create_node, TestRedis};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_hub_connection_manager_state_consistency() {
    use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};

    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None, // No Redis -- single-node mode
        node_id: "test_node".to_string(),
        dedup_window: Duration::from_secs(1),
        cleanup_interval: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
    };

    let manager = ClusterManager::new(config, None, None).await.unwrap();
    let conn_manager = ConnectionManager::new(ConnectionLimits::default());

    let room_id = RoomId::from_string("consistency_room".to_string());
    let user1 = UserId::from_string("user_1".to_string());
    let user2 = UserId::from_string("user_2".to_string());

    // Subscribe two users via ClusterManager (RoomMessageHub)
    let (_rx1, conn_id_1) = manager.subscribe(room_id.clone(), user1.clone()).await;
    let (_rx2, conn_id_2) = manager.subscribe(room_id.clone(), user2.clone()).await;

    // Register connections via ConnectionManager
    conn_manager
        .register(conn_id_1.clone(), user1.clone())
        .await
        .expect("register user1");
    conn_manager
        .join_room(&conn_id_1, room_id.clone())
        .await
        .expect("join room user1");
    conn_manager
        .register(conn_id_2.clone(), user2.clone())
        .await
        .expect("register user2");
    conn_manager
        .join_room(&conn_id_2, room_id.clone())
        .await
        .expect("join room user2");

    // Verify initial state
    let hub_metrics = manager.metrics();
    assert_eq!(hub_metrics.total_connections, 2);
    assert_eq!(hub_metrics.total_rooms, 1);
    assert_eq!(conn_manager.connection_count(), 2);
    assert_eq!(conn_manager.room_connection_count(&room_id), 2);

    // Simulate user1 disconnect: unsubscribe from hub + unregister from connection manager
    manager.unsubscribe(&conn_id_1);
    conn_manager.unregister(&conn_id_1).await;

    // Verify partial state
    let hub_metrics = manager.metrics();
    assert_eq!(
        hub_metrics.total_connections, 1,
        "Hub should have 1 connection"
    );
    assert_eq!(
        conn_manager.connection_count(),
        1,
        "ConnManager should have 1 connection"
    );
    assert_eq!(
        conn_manager.room_connection_count(&room_id),
        1,
        "Room should have 1 connection"
    );

    // Simulate user2 disconnect
    manager.unsubscribe(&conn_id_2);
    conn_manager.unregister(&conn_id_2).await;

    // Verify clean state
    let hub_metrics = manager.metrics();
    assert_eq!(
        hub_metrics.total_connections, 0,
        "Hub should have 0 connections"
    );
    assert_eq!(hub_metrics.total_rooms, 0, "Hub should have 0 rooms");
    assert_eq!(
        conn_manager.connection_count(),
        0,
        "ConnManager should have 0 connections"
    );
    assert_eq!(
        conn_manager.room_connection_count(&room_id),
        0,
        "Room should have 0 connections"
    );
    assert_eq!(
        conn_manager.user_connection_count(&user1),
        0,
        "User1 should have 0 connections"
    );
    assert_eq!(
        conn_manager.user_connection_count(&user2),
        0,
        "User2 should have 0 connections"
    );

    manager.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_rapid_subscribe_unsubscribe_no_leak() {
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        node_id: "test_node".to_string(),
        dedup_window: Duration::from_secs(1),
        cleanup_interval: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
    };

    let manager = ClusterManager::new(config, None, None).await.unwrap();
    let room_id = RoomId::from_string("rapid_room".to_string());

    // Rapidly subscribe and unsubscribe 100 connections
    for i in 0..100 {
        let user = UserId::from_string(format!("rapid_user_{i}"));
        let (_rx, conn_id) = manager.subscribe(room_id.clone(), user).await;
        manager.unsubscribe(&conn_id);
    }

    // After all subscribe/unsubscribe cycles, state should be clean
    let metrics = manager.metrics();
    assert_eq!(
        metrics.total_connections, 0,
        "No connections should remain after rapid subscribe/unsubscribe"
    );
    assert_eq!(
        metrics.total_rooms, 0,
        "No rooms should remain after all subscribers removed"
    );

    manager.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_multi_replica_websocket_connections() {
    let redis = TestRedis::start().await;

    // Create three nodes to simulate three replicas
    let node_a = create_node(&redis.redis_url, "ws_node_a").await;
    let node_b = create_node(&redis.redis_url, "ws_node_b").await;
    let node_c = create_node(&redis.redis_url, "ws_node_c").await;

    let room_id = RoomId::from_string("websocket_room".to_string());

    // Simulate 5 WebSocket clients on node A
    let mut clients_a = Vec::new();
    for i in 0..5 {
        let user_id = UserId::from_string(format!("ws_client_a_{i}"));
        let (rx, conn_id) = node_a.subscribe(room_id.clone(), user_id).await;
        clients_a.push((rx, conn_id));
    }

    // Simulate 5 WebSocket clients on node B
    let mut clients_b = Vec::new();
    for i in 0..5 {
        let user_id = UserId::from_string(format!("ws_client_b_{i}"));
        let (rx, conn_id) = node_b.subscribe(room_id.clone(), user_id).await;
        clients_b.push((rx, conn_id));
    }

    // Simulate 5 WebSocket clients on node C
    let mut clients_c = Vec::new();
    for i in 0..5 {
        let user_id = UserId::from_string(format!("ws_client_c_{i}"));
        let (rx, conn_id) = node_c.subscribe(room_id.clone(), user_id).await;
        clients_c.push((rx, conn_id));
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify all nodes have correct metrics
    let metrics_a = node_a.metrics();
    let metrics_b = node_b.metrics();
    let metrics_c = node_c.metrics();
    assert_eq!(metrics_a.total_connections, 5);
    assert_eq!(metrics_b.total_connections, 5);
    assert_eq!(metrics_c.total_connections, 5);

    // Node A sends a broadcast message
    let broadcast_event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("ws_client_a_0".to_string()),
        username: "client_a_0".to_string(),
        message: "Hello from node A!".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    node_a.broadcast(broadcast_event.clone());

    // All clients on node B and node C should receive the message
    for (rx, _) in &mut clients_b {
        let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Node B client should receive message")
            .expect("Channel not closed");
        assert_eq!(received.event_type(), "chat_message");
    }

    for (rx, _) in &mut clients_c {
        let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Node C client should receive message")
            .expect("Channel not closed");
        assert_eq!(received.event_type(), "chat_message");
    }

    // Node C sends a broadcast message
    let broadcast_event_c = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("ws_client_c_2".to_string()),
        username: "client_c_2".to_string(),
        message: "Hello from node C!".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    node_c.broadcast(broadcast_event_c);

    // All clients on node A and node B should receive the message
    for (rx, _) in &mut clients_a {
        let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Node A client should receive message from C")
            .expect("Channel not closed");
        assert_eq!(received.event_type(), "chat_message");
    }

    for (rx, _) in &mut clients_b {
        let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Node B client should receive message from C")
            .expect("Channel not closed");
        assert_eq!(received.event_type(), "chat_message");
    }

    // Cleanup
    for (_, conn_id) in clients_a {
        node_a.unsubscribe(&conn_id);
    }
    for (_, conn_id) in clients_b {
        node_b.unsubscribe(&conn_id);
    }
    for (_, conn_id) in clients_c {
        node_c.unsubscribe(&conn_id);
    }

    node_a.shutdown().await;
    node_b.shutdown().await;
    node_c.shutdown().await;
}
