//! Cross-replica room synchronization integration tests.
//!
//! Tests verify that the `RoomMessageHub` correctly synchronizes subscription
//! state across replicas via Redis. Each test starts a Redis container and
//! creates two `RoomMessageHub` instances backed by the same Redis, simulating
//! a multi-replica deployment.
//!
//! Run with: cargo test --package synctv-cluster --test `room_sync_integration_test`

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use synctv_cluster::sync::{
    build_room_message_runtime, room_hub::RoomLifecycleEvent, RoomMessageRuntime,
};
use synctv_core::models::id::{RoomId, UserId};
use synctv_core::SharedStateProfile;
use synctv_core_testing::redis_connection_manager;
mod integration_test_helpers;
use integration_test_helpers::TestRedis;

/// Helper: create a `RoomMessageHub` with Redis backing using the given prefix.
async fn create_hub(redis_url: &str, key_prefix: &str) -> std::sync::Arc<dyn RoomMessageRuntime> {
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let conn = redis_connection_manager(&client).await;
    build_room_message_runtime(&SharedStateProfile::from_runtime(
        Some(synctv_core::direct_runtime(conn)),
        key_prefix,
        true,
    ))
    .expect("shared room runtime should initialize")
}

// Test 1: Subscription state persisted to Redis is visible from another hub

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_subscription_visibility() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test1:").await;
    let hub_b = create_hub(&redis.redis_url, "test1:").await;

    let room_id = RoomId::expect_positive(10_000_070);
    let user_id = UserId::expect_positive(10_000_003);

    // Subscribe on hub A
    let (_rx, conn_id) = {
        let connection_id = "conn_a_1".to_string();
        let rx = hub_a
            .subscribe(room_id, user_id, connection_id.clone())
            .await
            .expect("subscribe should succeed");
        (rx, connection_id)
    };

    // Allow Redis write to propagate (spawned task)
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Hub B should see the subscription via Redis (distributed query)
    let distributed_subs = hub_b.get_room_subscribers_cluster_wide(&room_id).await;
    assert!(
        !distributed_subs.is_empty(),
        "Hub B should see hub A's subscription via Redis, got empty list"
    );

    let (sub_user_id, sub_conn_id) = &distributed_subs[0];
    assert_eq!(*sub_user_id, user_id);
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

// Test 2: Unsubscribe removes state from Redis

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_unsubscribe_removes_redis_state() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test2:").await;
    let hub_b = create_hub(&redis.redis_url, "test2:").await;

    let room_id = RoomId::expect_positive(10_000_071);
    let user_id = UserId::expect_positive(10_000_072);

    // Subscribe on hub A
    let conn_id = "conn_unsub_1".to_string();
    let _rx = hub_a
        .subscribe(room_id, user_id, conn_id.clone())
        .await
        .expect("subscribe should succeed");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify subscription is visible from hub B
    let subs_before = hub_b.get_room_subscribers_cluster_wide(&room_id).await;
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
    let subs_after = hub_b.get_room_subscribers_cluster_wide(&room_id).await;
    assert!(
        subs_after.is_empty(),
        "Hub B should see 0 subscribers after unsubscribe, got {}",
        subs_after.len()
    );
}

// Test 2b: remove_room also removes distributed Redis subscription state

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_remove_room_removes_redis_state_across_replicas() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test2b:").await;
    let hub_b = create_hub(&redis.redis_url, "test2b:").await;

    let room_id = RoomId::expect_positive(10_000_073);
    let user_a = UserId::expect_positive(10_000_074);
    let user_b = UserId::expect_positive(10_000_075);

    let _rx1 = hub_a
        .subscribe(room_id, user_a, "remove_conn_1".to_string())
        .await
        .expect("subscribe should succeed");
    let _rx2 = hub_a
        .subscribe(room_id, user_b, "remove_conn_2".to_string())
        .await
        .expect("subscribe should succeed");

    tokio::time::sleep(Duration::from_millis(250)).await;

    let subs_before = hub_b.get_room_subscribers_cluster_wide(&room_id).await;
    assert_eq!(
        subs_before.len(),
        2,
        "distributed state should contain both subscribers before room removal"
    );

    hub_a.remove_room(&room_id);

    tokio::time::sleep(Duration::from_millis(250)).await;

    let subs_after = hub_b.get_room_subscribers_cluster_wide(&room_id).await;
    assert!(
        subs_after.is_empty(),
        "remove_room must remove Redis subscription state, got {} lingering entries",
        subs_after.len()
    );
}

// Test 3: Multiple users across replicas see consistent distributed count

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_multiple_subscribers_distributed_count() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test3:").await;
    let hub_b = create_hub(&redis.redis_url, "test3:").await;

    let room_id = RoomId::expect_positive(10_000_076);

    // Subscribe 2 users on hub A
    let _rx1 = hub_a
        .subscribe(
            room_id,
            UserId::expect_positive(10_000_077),
            "conn_a1".to_string(),
        )
        .await
        .expect("subscribe should succeed");
    let _rx2 = hub_a
        .subscribe(
            room_id,
            UserId::expect_positive(10_000_078),
            "conn_a2".to_string(),
        )
        .await
        .expect("subscribe should succeed");

    // Subscribe 1 user on hub B
    let _rx3 = hub_b
        .subscribe(
            room_id,
            UserId::expect_positive(10_000_079),
            "conn_b1".to_string(),
        )
        .await
        .expect("subscribe should succeed");

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Both hubs should see 3 subscribers via distributed query
    let subs_from_a = hub_a.get_room_subscribers_cluster_wide(&room_id).await;
    let subs_from_b = hub_b.get_room_subscribers_cluster_wide(&room_id).await;

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

    let subs_after = hub_b.get_room_subscribers_cluster_wide(&room_id).await;
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

// Test 4: Room lifecycle events fire correctly during subscribe/unsubscribe

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_room_lifecycle_events_across_replicas() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test4:").await;

    let mut lifecycle_rx = hub_a.subscribe_lifecycle();

    let room_id = RoomId::expect_positive(10_000_080);
    let user_id = UserId::expect_positive(10_000_081);

    // First subscriber should trigger RoomActivated
    let _rx = hub_a
        .subscribe(room_id, user_id, "lc_conn_1".to_string())
        .await
        .expect("subscribe should succeed");

    let event = tokio::time::timeout(Duration::from_secs(2), lifecycle_rx.recv())
        .await
        .expect("Timed out waiting for RoomActivated")
        .expect("Lifecycle channel closed");

    match event {
        RoomLifecycleEvent::RoomActivated(rid) => {
            assert_eq!(rid, room_id);
        }
        other @ RoomLifecycleEvent::RoomDeactivated(_) => {
            panic!("Expected RoomActivated, got {other:?}")
        }
    }

    // Second subscriber in same room should NOT trigger another RoomActivated
    let _rx2 = hub_a
        .subscribe(
            room_id,
            UserId::expect_positive(10_000_082),
            "lc_conn_2".to_string(),
        )
        .await
        .expect("subscribe should succeed");

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
            assert_eq!(rid, room_id);
        }
        other @ RoomLifecycleEvent::RoomActivated(_) => {
            panic!("Expected RoomDeactivated, got {other:?}")
        }
    }
}

// Test 5: audit_redis_subscriptions reports correct counts without populating local

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_audit_redis_subscriptions_reports_without_local_populate() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test5:").await;
    let hub_b = create_hub(&redis.redis_url, "test5:").await;

    let room_id = RoomId::expect_positive(10_000_083);

    // Subscribe 3 users on hub A
    let _rx1 = hub_a
        .subscribe(
            room_id,
            UserId::expect_positive(10_000_084),
            "rec_conn_1".to_string(),
        )
        .await
        .expect("subscribe should succeed");
    let _rx2 = hub_a
        .subscribe(
            room_id,
            UserId::expect_positive(10_000_085),
            "rec_conn_2".to_string(),
        )
        .await
        .expect("subscribe should succeed");
    let _rx3 = hub_a
        .subscribe(
            room_id,
            UserId::expect_positive(10_000_086),
            "rec_conn_3".to_string(),
        )
        .await
        .expect("subscribe should succeed");

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Hub B recovers from Redis -- should report the count but not populate local state
    let recovered = hub_b
        .audit_shared_subscriptions()
        .await
        .expect("audit_shared_subscriptions failed");

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

// Test 6: Concurrent subscribe/unsubscribe across replicas maintains consistency

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_concurrent_cross_replica_subscribe_unsubscribe() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test6:").await;
    let hub_b = create_hub(&redis.redis_url, "test6:").await;

    let room_id = RoomId::expect_positive(10_000_087);

    // Subscribe 5 users on hub A and 5 on hub B concurrently
    let mut handles = Vec::new();
    for i in 0..5 {
        let hub = hub_a.clone();
        let rid = room_id;
        handles.push(tokio::spawn(async move {
            let _rx = hub
                .subscribe(
                    rid,
                    UserId::expect_positive(150_000 + i),
                    format!("conn_a_{i}"),
                )
                .await
                .expect("subscribe should succeed");
        }));
    }
    for i in 0..5 {
        let hub = hub_b.clone();
        let rid = room_id;
        handles.push(tokio::spawn(async move {
            let _rx = hub
                .subscribe(
                    rid,
                    UserId::expect_positive(160_000 + i),
                    format!("conn_b_{i}"),
                )
                .await
                .expect("subscribe should succeed");
        }));
    }

    futures::future::join_all(handles).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Distributed view should show 10 subscribers
    let distributed = hub_a.get_room_subscribers_cluster_wide(&room_id).await;
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
    let remaining = hub_b.get_room_subscribers_cluster_wide(&room_id).await;
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

// Test 7: Local stale cleanup must not delete active subscriptions on other replicas

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_stale_cleanup_does_not_delete_other_replica_active_subscriptions() {
    let redis = TestRedis::start().await;

    let hub_a = create_hub(&redis.redis_url, "test7:").await;
    let hub_b = create_hub(&redis.redis_url, "test7:").await;

    let room_id = RoomId::expect_positive(10_000_011);

    let _rx_a = hub_a
        .subscribe(
            room_id,
            UserId::expect_positive(10_000_003),
            "conn_a".to_string(),
        )
        .await
        .expect("subscribe should succeed");
    let _rx_b = hub_b
        .subscribe(
            room_id,
            UserId::expect_positive(10_000_004),
            "conn_b".to_string(),
        )
        .await
        .expect("subscribe should succeed");

    tokio::time::sleep(Duration::from_millis(200)).await;

    hub_a.unsubscribe("conn_a");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let before_cleanup = hub_b.get_room_subscribers_cluster_wide(&room_id).await;
    assert_eq!(
        before_cleanup.len(),
        1,
        "Only hub B's active subscriber should remain after hub A unsubscribes"
    );
    assert_eq!(before_cleanup[0].1, "conn_b");

    let cancel = tokio_util::sync::CancellationToken::new();
    let cleanup_task =
        hub_a.spawn_shared_subscription_cleanup_task(Duration::from_millis(10), cancel.clone());
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    let _ = cleanup_task.await;

    let after_cleanup = hub_b.get_room_subscribers_cluster_wide(&room_id).await;
    assert_eq!(
        after_cleanup.len(),
        1,
        "Hub A cleanup must not delete hub B's active Redis subscription"
    );
    assert_eq!(after_cleanup[0].1, "conn_b");

    hub_b.unsubscribe("conn_b");
}

// Test 8: Distributed room lookups prune stale hash members on read

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_distributed_room_lookup_prunes_stale_hash_members() {
    use redis::AsyncCommands;

    let redis = TestRedis::start().await;
    let prefix = "test8:";

    let hub = create_hub(&redis.redis_url, prefix).await;
    let room_id = RoomId::expect_positive(10_000_088);

    let client = redis::Client::open(redis.redis_url.as_str()).expect("redis client");
    let mut conn = redis_connection_manager(&client).await;

    let room_key = format!("{prefix}room_hub:room:{room_id}");
    let valid_conn_key = format!("{prefix}room_hub:conn:conn_valid");
    let wrong_room_conn_key = format!("{prefix}room_hub:conn:conn_wrong_room");
    let valid_user_id = UserId::expect_positive(10_000_089);

    let _: () = conn
        .hset(&room_key, "conn_missing", 10_000_099i64)
        .await
        .unwrap();
    let _: () = conn
        .hset(&room_key, "conn_wrong_room", 10_000_098i64)
        .await
        .unwrap();
    let _: () = conn
        .hset(&room_key, "conn_valid", valid_user_id.get())
        .await
        .unwrap();
    let _: () = conn.set(&valid_conn_key, room_id.get()).await.unwrap();
    let _: () = conn.set(&wrong_room_conn_key, 10_000_092i64).await.unwrap();
    let _: () = conn.expire(&room_key, 180).await.unwrap();
    let _: () = conn.expire(&valid_conn_key, 180).await.unwrap();
    let _: () = conn.expire(&wrong_room_conn_key, 180).await.unwrap();

    let subscribers = hub.get_room_subscribers_cluster_wide(&room_id).await;
    assert_eq!(
        subscribers,
        vec![(valid_user_id, "conn_valid".to_string())],
        "distributed room lookup must prune missing and wrong-room hash members"
    );

    let remaining_members: Vec<(String, i64)> = conn.hgetall(&room_key).await.unwrap();
    assert_eq!(
        remaining_members,
        vec![("conn_valid".to_string(), valid_user_id.get())],
        "room hash should retain only valid subscribers after lazy pruning"
    );
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_audit_redis_subscriptions_prunes_stale_room_directory_members() {
    use redis::AsyncCommands;

    let redis = TestRedis::start().await;
    let prefix = "test9:";

    let hub = create_hub(&redis.redis_url, prefix).await;
    let client = redis::Client::open(redis.redis_url.as_str()).expect("redis client");
    let mut conn = redis_connection_manager(&client).await;

    let live_room_id = RoomId::expect_positive(10_000_093);
    let live_user_id = UserId::expect_positive(10_000_094);
    let live_room_key = format!("{prefix}room_hub:room:{live_room_id}");
    let stale_room_key = format!("{prefix}room_hub:room:10000095");
    let room_index_directory_key = format!("{prefix}room_hub:room_index");

    let _: () = conn
        .hset(&live_room_key, "conn_live", live_user_id.get())
        .await
        .unwrap();
    let _: () = conn.expire(&live_room_key, 180).await.unwrap();
    let _: () = conn
        .sadd(&room_index_directory_key, &live_room_key)
        .await
        .unwrap();
    let _: () = conn
        .sadd(&room_index_directory_key, &stale_room_key)
        .await
        .unwrap();

    let recovered = hub
        .audit_shared_subscriptions()
        .await
        .expect("audit_shared_subscriptions failed");
    assert_eq!(
        recovered, 1,
        "audit should count only the live room subscription"
    );

    let directory_members: Vec<String> = conn.smembers(&room_index_directory_key).await.unwrap();
    assert_eq!(
        directory_members,
        vec![live_room_key],
        "audit should prune stale room directory entries"
    );
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_room_directory_key_uses_crash_safety_ttl() {
    use redis::AsyncCommands;

    let redis = TestRedis::start().await;
    let prefix = "test10:";

    let hub = create_hub(&redis.redis_url, prefix).await;
    let room_id = RoomId::expect_positive(10_000_090);
    let user_id = UserId::expect_positive(10_000_091);

    let _rx = hub
        .subscribe(room_id, user_id, "conn_ttl".to_string())
        .await
        .expect("subscribe should succeed");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = redis::Client::open(redis.redis_url.as_str()).expect("redis client");
    let mut conn = redis_connection_manager(&client).await;

    let directory_key = format!("{prefix}room_hub:room_index");
    let ttl: i64 = conn.ttl(&directory_key).await.expect("room directory TTL");
    assert!(
        (175..=180).contains(&ttl),
        "room directory key should use the short crash-safety TTL, got {ttl}s"
    );
}
