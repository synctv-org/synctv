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
use synctv_core::models::id::{RoomId, UserId};
mod integration_test_helpers;
use integration_test_helpers::{create_node, TestRedis};

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_critical_events_high_priority() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room_id = RoomId::from_string("critical_room".to_string());
    let user_id = UserId::from_string("listener".to_string());

    let (mut room_rx, conn_id) = node_a.subscribe(room_id.clone(), user_id.clone()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send a critical event (PermissionChanged is marked as critical)
    let critical_event = ClusterEvent::PermissionChanged {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        target_user_id: user_id.clone(),
        target_username: "listener".to_string(),
        changed_by: UserId::from_string("admin".to_string()),
        changed_by_username: "admin".to_string(),
        new_permissions: synctv_core::models::PermissionBits(
            synctv_core::models::PermissionBits::DEFAULT_MEMBER,
        ),
        role: 2, // Member role
        added_permissions: synctv_core::models::PermissionBits::empty(),
        removed_permissions: synctv_core::models::PermissionBits::empty(),
        admin_added_permissions: synctv_core::models::PermissionBits::empty(),
        admin_removed_permissions: synctv_core::models::PermissionBits::empty(),
        timestamp: Utc::now(),
    };

    assert!(
        critical_event.is_critical(),
        "PermissionChanged should be critical"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let received = loop {
        let result = node_b.broadcast(critical_event.clone());

        match tokio::time::timeout(Duration::from_millis(750), room_rx.recv()).await {
            Ok(Some(event)) if event.event_type() == "permission_changed" => break event,
            Ok(Some(_)) => {}
            Ok(None) => panic!("Channel closed"),
            Err(_) => {}
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "Timed out waiting for critical event; last broadcast result: {result:?}"
        );
    };

    assert_eq!(received.event_type(), "permission_changed");

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}
