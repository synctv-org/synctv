//! Cross-replica room synchronization integration tests (R-P2-2)
//!
//! Tests verify that the `RoomMessageHub` correctly synchronizes subscription
//! state across replicas via Redis. Each test starts a Redis container and
//! creates two `RoomMessageHub` instances backed by the same Redis, simulating
//! a multi-replica deployment.
//!
//! Run with: cargo test --package synctv-cluster --test `room_sync_integration_test`

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use synctv_cluster::sync::room_hub::{RoomLifecycleEvent, RoomMessageHub};
use synctv_core::models::id::{RoomId, UserId};
mod integration_test_helpers;
use integration_test_helpers::TestRedis;

/// Helper: create a `RoomMessageHub` with Redis backing using the given prefix.
async fn create_hub(redis_url: &str, key_prefix: &str) -> RoomMessageHub {
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create Redis connection");

    RoomMessageHub::new().with_redis(conn, key_prefix)
}

// ============================================================================
// Test 1: Subscription state persisted to Redis is visible from another hub
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_subscription_visibility() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test1:").await;
    let hub_b = create_hub(&redis.redis_url, "test1:").await;

    let room_id = RoomId::from_string("sync_room".to_string());
    let user_id = UserId::from_string("user_a".to_string());

    // Subscribe on hub A
    let (_rx, conn_id) = {
        let connection_id = "conn_a_1".to_string();
        let rx = hub_a
            .subscribe(room_id.clone(), user_id.clone(), connection_id.clone())
            .await;
        (rx, connection_id)
    };

    // Allow Redis write to propagate (spawned task)
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Hub B should see the subscription via Redis (distributed query)
    let distributed_subs = hub_b.get_room_subscribers_distributed(&room_id).await;
    assert!(
        !distributed_subs.is_empty(),
        "Hub B should see hub A's subscription via Redis, got empty list"
    );

    let (sub_user_id, sub_conn_id) = &distributed_subs[0];
    assert_eq!(sub_user_id.as_str(), "user_a");
    assert_eq!(sub_conn_id, &conn_id);

    // Hub B's local view should be empty (no local subscribers)
    let local_subs = hub_b.get_room_subscribers(&room_id);
    assert!(
        local_subs.is_empty(),
        "Hub B's local subscriber list should be empty"
    );

    // Cleanup
    hub_a.unsubscribe(&conn_id);
}

// ============================================================================
// Test 2: Unsubscribe removes state from Redis
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_unsubscribe_removes_redis_state() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test2:").await;
    let hub_b = create_hub(&redis.redis_url, "test2:").await;

    let room_id = RoomId::from_string("unsub_room".to_string());
    let user_id = UserId::from_string("temp_user".to_string());

    // Subscribe on hub A
    let conn_id = "conn_unsub_1".to_string();
    let _rx = hub_a
        .subscribe(room_id.clone(), user_id.clone(), conn_id.clone())
        .await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify subscription is visible from hub B
    let subs_before = hub_b.get_room_subscribers_distributed(&room_id).await;
    assert_eq!(
        subs_before.len(),
        1,
        "Should see 1 subscriber before unsubscribe"
    );

    // Unsubscribe on hub A
    hub_a.unsubscribe(&conn_id);

    // Allow Redis delete to propagate
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Hub B should no longer see the subscription
    let subs_after = hub_b.get_room_subscribers_distributed(&room_id).await;
    assert!(
        subs_after.is_empty(),
        "Hub B should see 0 subscribers after unsubscribe, got {}",
        subs_after.len()
    );
}

// ============================================================================
// Test 2b: remove_room also removes distributed Redis subscription state
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_remove_room_removes_redis_state_across_replicas() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test2b:").await;
    let hub_b = create_hub(&redis.redis_url, "test2b:").await;

    let room_id = RoomId::from_string("removed_room".to_string());
    let user_a = UserId::from_string("remove_user_a".to_string());
    let user_b = UserId::from_string("remove_user_b".to_string());

    let _rx1 = hub_a
        .subscribe(room_id.clone(), user_a, "remove_conn_1".to_string())
        .await;
    let _rx2 = hub_a
        .subscribe(room_id.clone(), user_b, "remove_conn_2".to_string())
        .await;

    tokio::time::sleep(Duration::from_millis(250)).await;

    let subs_before = hub_b.get_room_subscribers_distributed(&room_id).await;
    assert_eq!(
        subs_before.len(),
        2,
        "distributed state should contain both subscribers before room removal"
    );

    hub_a.remove_room(&room_id);

    tokio::time::sleep(Duration::from_millis(250)).await;

    let subs_after = hub_b.get_room_subscribers_distributed(&room_id).await;
    assert!(
        subs_after.is_empty(),
        "remove_room must remove Redis subscription state, got {} lingering entries",
        subs_after.len()
    );
}

// ============================================================================
// Test 3: Multiple users across replicas see consistent distributed count
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_multiple_subscribers_distributed_count() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test3:").await;
    let hub_b = create_hub(&redis.redis_url, "test3:").await;

    let room_id = RoomId::from_string("multi_user_room".to_string());

    // Subscribe 2 users on hub A
    let _rx1 = hub_a
        .subscribe(
            room_id.clone(),
            UserId::from_string("user_a1".to_string()),
            "conn_a1".to_string(),
        )
        .await;
    let _rx2 = hub_a
        .subscribe(
            room_id.clone(),
            UserId::from_string("user_a2".to_string()),
            "conn_a2".to_string(),
        )
        .await;

    // Subscribe 1 user on hub B
    let _rx3 = hub_b
        .subscribe(
            room_id.clone(),
            UserId::from_string("user_b1".to_string()),
            "conn_b1".to_string(),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Both hubs should see 3 subscribers via distributed query
    let subs_from_a = hub_a.get_room_subscribers_distributed(&room_id).await;
    let subs_from_b = hub_b.get_room_subscribers_distributed(&room_id).await;

    assert_eq!(
        subs_from_a.len(),
        3,
        "Hub A distributed query should return 3 subscribers, got {}",
        subs_from_a.len()
    );
    assert_eq!(
        subs_from_b.len(),
        3,
        "Hub B distributed query should return 3 subscribers, got {}",
        subs_from_b.len()
    );

    // Local views should only show their own subscribers
    assert_eq!(
        hub_a.get_room_subscribers(&room_id).len(),
        2,
        "Hub A local should have 2 subscribers"
    );
    assert_eq!(
        hub_b.get_room_subscribers(&room_id).len(),
        1,
        "Hub B local should have 1 subscriber"
    );

    // Unsubscribe one from hub A
    hub_a.unsubscribe("conn_a1");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let subs_after = hub_b.get_room_subscribers_distributed(&room_id).await;
    assert_eq!(
        subs_after.len(),
        2,
        "After unsubscribe, distributed count should be 2, got {}",
        subs_after.len()
    );

    // Cleanup
    hub_a.unsubscribe("conn_a2");
    hub_b.unsubscribe("conn_b1");
}

// ============================================================================
// Test 4: Room lifecycle events fire correctly during subscribe/unsubscribe
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_room_lifecycle_events_across_replicas() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test4:").await;

    let mut lifecycle_rx = hub_a.subscribe_lifecycle();

    let room_id = RoomId::from_string("lifecycle_room".to_string());
    let user_id = UserId::from_string("lifecycle_user".to_string());

    // First subscriber should trigger RoomActivated
    let _rx = hub_a
        .subscribe(room_id.clone(), user_id.clone(), "lc_conn_1".to_string())
        .await;

    let event = tokio::time::timeout(Duration::from_secs(2), lifecycle_rx.recv())
        .await
        .expect("Timed out waiting for RoomActivated")
        .expect("Lifecycle channel closed");

    match event {
        RoomLifecycleEvent::RoomActivated(rid) => {
            assert_eq!(rid.as_str(), "lifecycle_room");
        }
        other @ RoomLifecycleEvent::RoomDeactivated(_) => {
            panic!("Expected RoomActivated, got {other:?}")
        }
    }

    // Second subscriber in same room should NOT trigger another RoomActivated
    let _rx2 = hub_a
        .subscribe(
            room_id.clone(),
            UserId::from_string("lifecycle_user_2".to_string()),
            "lc_conn_2".to_string(),
        )
        .await;

    let no_event = tokio::time::timeout(Duration::from_millis(200), lifecycle_rx.recv()).await;
    assert!(
        no_event.is_err(),
        "Second subscriber should not trigger RoomActivated"
    );

    // Unsubscribe first user -- room still has a subscriber
    hub_a.unsubscribe("lc_conn_1");

    let no_deactivate = tokio::time::timeout(Duration::from_millis(200), lifecycle_rx.recv()).await;
    assert!(
        no_deactivate.is_err(),
        "Room with remaining subscribers should not trigger RoomDeactivated"
    );

    // Unsubscribe last user -- room should trigger RoomDeactivated
    hub_a.unsubscribe("lc_conn_2");

    let deactivate = tokio::time::timeout(Duration::from_secs(2), lifecycle_rx.recv())
        .await
        .expect("Timed out waiting for RoomDeactivated")
        .expect("Lifecycle channel closed");

    match deactivate {
        RoomLifecycleEvent::RoomDeactivated(rid) => {
            assert_eq!(rid.as_str(), "lifecycle_room");
        }
        other @ RoomLifecycleEvent::RoomActivated(_) => {
            panic!("Expected RoomDeactivated, got {other:?}")
        }
    }
}

// ============================================================================
// Test 5: audit_redis_subscriptions reports correct counts without populating local
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_audit_redis_subscriptions_reports_without_local_populate() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test5:").await;
    let hub_b = create_hub(&redis.redis_url, "test5:").await;

    let room_id = RoomId::from_string("recover_room".to_string());

    // Subscribe 3 users on hub A
    let _rx1 = hub_a
        .subscribe(
            room_id.clone(),
            UserId::from_string("rec_user_1".to_string()),
            "rec_conn_1".to_string(),
        )
        .await;
    let _rx2 = hub_a
        .subscribe(
            room_id.clone(),
            UserId::from_string("rec_user_2".to_string()),
            "rec_conn_2".to_string(),
        )
        .await;
    let _rx3 = hub_a
        .subscribe(
            room_id.clone(),
            UserId::from_string("rec_user_3".to_string()),
            "rec_conn_3".to_string(),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Hub B recovers from Redis -- should report the count but not populate local state
    let recovered = hub_b
        .audit_redis_subscriptions()
        .await
        .expect("audit_redis_subscriptions failed");

    assert_eq!(
        recovered, 3,
        "Should recover 3 subscriptions from Redis, got {recovered}"
    );

    // Local state of hub B should still be empty (audit_redis_subscriptions is read-only)
    assert_eq!(
        hub_b.room_count(),
        0,
        "Hub B local room count should be 0 after audit_redis_subscriptions"
    );
    assert_eq!(
        hub_b.connection_count(),
        0,
        "Hub B local connection count should be 0 after audit_redis_subscriptions"
    );

    // Cleanup
    hub_a.unsubscribe("rec_conn_1");
    hub_a.unsubscribe("rec_conn_2");
    hub_a.unsubscribe("rec_conn_3");
}

// ============================================================================
// Test 6: Concurrent subscribe/unsubscribe across replicas maintains consistency
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_concurrent_cross_replica_subscribe_unsubscribe() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test6:").await;
    let hub_b = create_hub(&redis.redis_url, "test6:").await;

    let room_id = RoomId::from_string("concurrent_room".to_string());

    // Subscribe 5 users on hub A and 5 on hub B concurrently
    let mut handles = Vec::new();
    for i in 0..5 {
        let hub = hub_a.clone();
        let rid = room_id.clone();
        handles.push(tokio::spawn(async move {
            let _rx = hub
                .subscribe(
                    rid,
                    UserId::from_string(format!("user_a_{i}")),
                    format!("conn_a_{i}"),
                )
                .await;
        }));
    }
    for i in 0..5 {
        let hub = hub_b.clone();
        let rid = room_id.clone();
        handles.push(tokio::spawn(async move {
            let _rx = hub
                .subscribe(
                    rid,
                    UserId::from_string(format!("user_b_{i}")),
                    format!("conn_b_{i}"),
                )
                .await;
        }));
    }

    futures::future::join_all(handles).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Distributed view should show 10 subscribers
    let distributed = hub_a.get_room_subscribers_distributed(&room_id).await;
    assert_eq!(
        distributed.len(),
        10,
        "Should see 10 distributed subscribers, got {}",
        distributed.len()
    );

    // Unsubscribe all from hub A
    for i in 0..5 {
        hub_a.unsubscribe(&format!("conn_a_{i}"));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Should now see only 5 (hub B's subscribers)
    let remaining = hub_b.get_room_subscribers_distributed(&room_id).await;
    assert_eq!(
        remaining.len(),
        5,
        "After hub A unsubscribes, should see 5 remaining, got {}",
        remaining.len()
    );

    // Cleanup
    for i in 0..5 {
        hub_b.unsubscribe(&format!("conn_b_{i}"));
    }
}

// ============================================================================
// Test 7: Local stale cleanup must not delete active subscriptions on other replicas
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_stale_cleanup_does_not_delete_other_replica_active_subscriptions() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test7:").await;
    let hub_b = create_hub(&redis.redis_url, "test7:").await;

    let room_id = RoomId::from_string("shared_room".to_string());

    let _rx_a = hub_a
        .subscribe(
            room_id.clone(),
            UserId::from_string("user_a".to_string()),
            "conn_a".to_string(),
        )
        .await;
    let _rx_b = hub_b
        .subscribe(
            room_id.clone(),
            UserId::from_string("user_b".to_string()),
            "conn_b".to_string(),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    hub_a.unsubscribe("conn_a");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let before_cleanup = hub_b.get_room_subscribers_distributed(&room_id).await;
    assert_eq!(
        before_cleanup.len(),
        1,
        "Only hub B's active subscriber should remain after hub A unsubscribes"
    );
    assert_eq!(before_cleanup[0].1, "conn_b");

    let cancel = tokio_util::sync::CancellationToken::new();
    let cleanup_task =
        hub_a.spawn_stale_subscription_cleanup_task(Duration::from_millis(10), cancel.clone());
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    let _ = cleanup_task.await;

    let after_cleanup = hub_b.get_room_subscribers_distributed(&room_id).await;
    assert_eq!(
        after_cleanup.len(),
        1,
        "Hub A cleanup must not delete hub B's active Redis subscription"
    );
    assert_eq!(after_cleanup[0].1, "conn_b");

    hub_b.unsubscribe("conn_b");
}
