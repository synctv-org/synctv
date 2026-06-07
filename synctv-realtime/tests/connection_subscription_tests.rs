//! Multi-replica cluster integration tests
//!
//! These tests verify cross-node coordination by starting multiple
//! `RealtimeManager` instances that share a single Redis container
//! (via testcontainers). Each "node" has its own `node_id` but connects
//! to the same Redis, simulating a multi-replica deployment.

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use synctv_core::models::id::{RoomId, UserId};
use synctv_realtime::sync::{events::RealtimeEvent, RoomMessageHub};
use synctv_realtime::{RealtimeConfig, RealtimeManager};
mod integration_test_helpers;
use integration_test_helpers::{broadcast_until_all_clients_receive, create_node, TestRedis};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_hub_connection_manager_state_consistency() {
    use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_node".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await.unwrap();
    let conn_manager = ConnectionManager::new(ConnectionLimits::default());

    let room_id = RoomId::expect_positive(10_000_029);
    let user1 = UserId::expect_positive(10_000_030);
    let user2 = UserId::expect_positive(10_000_031);

    // Subscribe two users via RealtimeManager (RoomMessageHub)
    let (_rx1, conn_id_1) = manager
        .subscribe(room_id, user1)
        .await
        .expect("subscribe should succeed");
    let (_rx2, conn_id_2) = manager
        .subscribe(room_id, user2)
        .await
        .expect("subscribe should succeed");

    // Register connections via ConnectionManager
    conn_manager
        .register(conn_id_1.clone().into_string(), user1)
        .await
        .expect("register user1");
    conn_manager
        .join_room(&conn_id_1, room_id)
        .await
        .expect("join room user1");
    conn_manager
        .register(conn_id_2.clone().into_string(), user2)
        .await
        .expect("register user2");
    conn_manager
        .join_room(&conn_id_2, room_id)
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
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_node".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await.unwrap();
    let room_id = RoomId::expect_positive(10_000_032);

    // Rapidly subscribe and unsubscribe 100 connections
    for i in 0..100 {
        let user = UserId::expect_positive(110_000 + i);
        let (_rx, conn_id) = manager
            .subscribe(room_id, user)
            .await
            .expect("subscribe should succeed");
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

    let node_a = create_node(&redis.redis_url, "ws_node_a").await;
    let node_b = create_node(&redis.redis_url, "ws_node_b").await;
    let node_c = create_node(&redis.redis_url, "ws_node_c").await;

    let room_id = RoomId::expect_positive(10_000_033);

    // Simulate 5 WebSocket clients on node A
    let mut clients_a = Vec::new();
    for i in 0..5 {
        let user_id = UserId::expect_positive(120_000 + i);
        let (rx, conn_id) = node_a
            .subscribe(room_id, user_id)
            .await
            .expect("subscribe should succeed");
        clients_a.push((rx, conn_id));
    }

    // Simulate 5 WebSocket clients on node B
    let mut clients_b = Vec::new();
    for i in 0..5 {
        let user_id = UserId::expect_positive(130_000 + i);
        let (rx, conn_id) = node_b
            .subscribe(room_id, user_id)
            .await
            .expect("subscribe should succeed");
        clients_b.push((rx, conn_id));
    }

    // Simulate 5 WebSocket clients on node C
    let mut clients_c = Vec::new();
    for i in 0..5 {
        let user_id = UserId::expect_positive(140_000 + i);
        let (rx, conn_id) = node_c
            .subscribe(room_id, user_id)
            .await
            .expect("subscribe should succeed");
        clients_c.push((rx, conn_id));
    }

    // Verify all nodes have correct metrics
    let metrics_a = node_a.metrics();
    let metrics_b = node_b.metrics();
    let metrics_c = node_c.metrics();
    assert_eq!(metrics_a.total_connections, 5);
    assert_eq!(metrics_b.total_connections, 5);
    assert_eq!(metrics_c.total_connections, 5);

    // Node A sends a broadcast message
    let message_from_a = "Hello from node A!";
    broadcast_until_all_clients_receive(
        &node_a,
        &mut clients_a,
        message_from_a,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_034),
            username: "client_a_0".to_string(),
            message: message_from_a.to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        },
        "node A local clients",
    )
    .await;
    broadcast_until_all_clients_receive(
        &node_a,
        &mut clients_b,
        message_from_a,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_034),
            username: "client_a_0".to_string(),
            message: message_from_a.to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        },
        "node B clients receiving node A broadcast",
    )
    .await;
    broadcast_until_all_clients_receive(
        &node_a,
        &mut clients_c,
        message_from_a,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_034),
            username: "client_a_0".to_string(),
            message: message_from_a.to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        },
        "node C clients receiving node A broadcast",
    )
    .await;

    // Node C sends a broadcast message
    let message_from_c = "Hello from node C!";
    broadcast_until_all_clients_receive(
        &node_c,
        &mut clients_a,
        message_from_c,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_035),
            username: "client_c_2".to_string(),
            message: message_from_c.to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        },
        "node A clients receiving node C broadcast",
    )
    .await;
    broadcast_until_all_clients_receive(
        &node_c,
        &mut clients_b,
        message_from_c,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_035),
            username: "client_c_2".to_string(),
            message: message_from_c.to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        },
        "node B clients receiving node C broadcast",
    )
    .await;

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
