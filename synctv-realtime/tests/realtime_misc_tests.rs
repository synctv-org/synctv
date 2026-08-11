//! Multi-replica cluster integration tests
//!
//! These tests verify cross-node coordination by starting multiple
//! `RealtimeManager` instances that share a single Redis container
//! (via testcontainers). Each "node" has its own `node_id` but connects
//! to the same Redis, simulating a multi-replica deployment.

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use chrono::Utc;
use synctv_core::models::id::{RoomId, UserId};
use synctv_realtime::sync::RealtimeEvent;
mod integration_test_helpers;
use integration_test_helpers::{create_node, user_actor, TestRedis};

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_critical_events_high_priority() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room_id = RoomId::expect_positive(10_000_025);
    let user_id = UserId::expect_positive(10_000_026);

    let (mut room_rx, conn_id) = node_a
        .subscribe(room_id, user_actor(user_id))
        .await
        .expect("subscribe should succeed");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let critical_event = RealtimeEvent::PermissionChanged {
        event_id: synctv_common::snanoid!(16),
        room_id,
        target_user_id: user_id,
        target_username: "listener".to_string(),
        target_remark_name: String::new(),
        target_display_tag: String::new(),
        changed_by: UserId::expect_positive(10_000_027),
        changed_by_username: "admin".to_string(),
        role_changed: false,
        new_permissions: synctv_core::models::RoomPermissionSet::default_member(),
        role: 2, // Member role
        added_permissions: synctv_core::models::RoomPermissionSet::empty(),
        removed_permissions: synctv_core::models::RoomPermissionSet::empty(),
        admin_added_permissions: synctv_core::models::RoomPermissionSet::empty(),
        admin_removed_permissions: synctv_core::models::RoomPermissionSet::empty(),
        target_is_online: true,
        target_connection_count: 1,
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
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => panic!("Channel closed"),
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
