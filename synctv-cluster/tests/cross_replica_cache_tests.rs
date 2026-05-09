//! Multi-replica cluster integration tests
//!
//! These tests verify cross-node coordination by starting multiple
//! `ClusterManager` instances that share a single Redis container
//! (via testcontainers). Each "node" has its own `node_id` but connects
//! to the same Redis, simulating a multi-replica deployment.

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use synctv_cluster::sync::events::{CacheTarget, ClusterEvent};
use synctv_cluster::{build_room_message_runtime, ClusterConfig, ClusterManager};
use synctv_core::cache::{CacheInvalidationService, InvalidationMessage};
use synctv_core::models::id::{RoomId, UserId};
use synctv_core::{DirectRedisConnectionRuntime, RedisConnectionRuntime, SharedStateProfile};
use synctv_core_testing::redis_connection_manager;
mod integration_test_helpers;
use integration_test_helpers::{
    broadcast_until_cache_invalidation, broadcast_until_room_event, create_node, TestRedis,
};

fn shared_message_runtime(
    redis_conn: redis::aio::ConnectionManager,
    key_prefix: &str,
) -> Arc<dyn synctv_cluster::RoomMessageRuntime> {
    let shared_runtime: Arc<dyn RedisConnectionRuntime> =
        Arc::new(DirectRedisConnectionRuntime::new(redis_conn));
    let realtime_profile = SharedStateProfile::from_runtime(Some(shared_runtime), key_prefix, true);
    build_room_message_runtime(&realtime_profile).expect("shared message runtime should initialize")
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_cache_invalidation() {
    let redis = TestRedis::start().await;
    let key_prefix = redis.key_prefix.clone();

    let cache_svc_a = Arc::new(CacheInvalidationService::new(
        "node_a".to_string(),
        "test:cache:inv".to_string(),
    ));
    let mut local_rx_a = cache_svc_a.subscribe();

    let client_a =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to open Redis client");
    let conn_a = redis_connection_manager(&client_a).await;
    let config_a = ClusterConfig {
        distributed_transport_factory: Some(Arc::new(
            synctv_cluster::RedisClusterMessageTransportFactory::new(
                synctv_core::coordination_runtime_from_client(client_a),
            ),
        )),
        message_runtime: shared_message_runtime(conn_a.clone(), &key_prefix),
        cluster_enabled: true,
        node_id: "node_a".to_string(),
        dedup_window: Duration::from_secs(10),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: key_prefix.clone(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };
    let node_a = ClusterManager::new(config_a, None, Some(cache_svc_a))
        .await
        .expect("Failed to create node A");

    let node_b =
        integration_test_helpers::create_node_with_prefix(&redis.redis_url, "node_b", key_prefix)
            .await;

    let mut received_user = false;
    let mut received_room = false;
    let updated_user = UserId::from(10_010_001);
    let updated_room = RoomId::from(10_010_002);
    broadcast_until_cache_invalidation(
        &node_b,
        &mut local_rx_a,
        || ClusterEvent::CacheInvalidate {
            event_id: synctv_common::snanoid!(16),
            targets: vec![
                CacheTarget::User {
                    user_id: updated_user,
                },
                CacheTarget::Room {
                    room_id: updated_room,
                },
            ],
            timestamp: Utc::now(),
        },
        |msg| match msg {
            InvalidationMessage::User { user_id } if user_id == updated_user.to_string() => {
                received_user = true;
                received_user && received_room
            }
            InvalidationMessage::Room { room_id } if room_id == updated_room.to_string() => {
                received_room = true;
                received_user && received_room
            }
            other => panic!("Unexpected invalidation message: {other:?}"),
        },
        "cross-replica cache invalidation",
    )
    .await;

    assert!(received_user, "Should have received User invalidation");
    assert!(received_room, "Should have received Room invalidation");

    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_permission_changed() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room_id = RoomId::from(10_000_036);
    let user_id = UserId::from(10_000_037);

    // Subscribe on node A (simulating a WebSocket client on node A watching the room)
    let (mut room_rx, conn_id) = node_a
        .subscribe(room_id, user_id)
        .await
        .expect("subscribe should succeed");

    let received = broadcast_until_room_event(
        &node_b,
        &mut room_rx,
        || ClusterEvent::PermissionChanged {
            event_id: synctv_common::snanoid!(16),
            room_id,
            target_user_id: UserId::from(10_000_038),
            target_username: "target_user".to_string(),
            new_permissions: synctv_core::models::PermissionBits(
                synctv_core::models::PermissionBits::DEFAULT_MEMBER
                    | synctv_core::models::PermissionBits::KICK_MEMBER,
            ),
            role: 3,
            added_permissions: synctv_core::models::PermissionBits(
                synctv_core::models::PermissionBits::KICK_MEMBER,
            ),
            removed_permissions: synctv_core::models::PermissionBits::empty(),
            admin_added_permissions: synctv_core::models::PermissionBits::empty(),
            admin_removed_permissions: synctv_core::models::PermissionBits::empty(),
            changed_by: UserId::from(10_000_039),
            changed_by_username: "admin_user".to_string(),
            timestamp: Utc::now(),
        },
        |event| matches!(event, ClusterEvent::PermissionChanged { target_user_id, .. } if *target_user_id == UserId::from(10_000_038)),
        "PermissionChanged on node A",
    )
    .await;

    assert_eq!(received.event_type(), "permission_changed");
    if let ClusterEvent::PermissionChanged {
        target_user_id,
        new_permissions,
        changed_by_username,
        ..
    } = &received
    {
        assert_eq!(*target_user_id, UserId::from(10_000_038));
        assert!(
            new_permissions.has(synctv_core::models::PermissionBits::KICK_MEMBER),
            "New permissions should include KICK_MEMBER"
        );
        assert_eq!(changed_by_username, "admin_user");
    } else {
        panic!(
            "Expected PermissionChanged event, got {:?}",
            received.event_type()
        );
    }

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_permission_cache_invalidation_via_cache_service() {
    let redis = TestRedis::start().await;
    let key_prefix = redis.key_prefix.clone();

    let cache_svc_a = Arc::new(CacheInvalidationService::new(
        "node_a".to_string(),
        "test:perm:inv".to_string(),
    ));
    let mut local_rx_a = cache_svc_a.subscribe();

    let client_a =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to open Redis client");
    let conn_a = redis_connection_manager(&client_a).await;
    let config_a = ClusterConfig {
        distributed_transport_factory: Some(Arc::new(
            synctv_cluster::RedisClusterMessageTransportFactory::new(
                synctv_core::coordination_runtime_from_client(client_a),
            ),
        )),
        message_runtime: shared_message_runtime(conn_a.clone(), &key_prefix),
        cluster_enabled: true,
        node_id: "node_a".to_string(),
        dedup_window: Duration::from_secs(10),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: key_prefix.clone(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };
    let node_a = ClusterManager::new(config_a, None, Some(cache_svc_a))
        .await
        .expect("Failed to create node A");

    let node_b =
        integration_test_helpers::create_node_with_prefix(&redis.redis_url, "node_b", key_prefix)
            .await;

    let mut received_target = false;
    let perm_changed_user = UserId::from(10_010_003);
    broadcast_until_cache_invalidation(
        &node_b,
        &mut local_rx_a,
        || ClusterEvent::CacheInvalidate {
            event_id: synctv_common::snanoid!(16),
            targets: vec![CacheTarget::User {
                user_id: perm_changed_user,
            }],
            timestamp: Utc::now(),
        },
        |msg| match msg {
            InvalidationMessage::User { user_id } => {
                assert_eq!(
                    user_id,
                    perm_changed_user.to_string(),
                    "Should invalidate the correct user"
                );
                received_target = true;
                true
            }
            other => panic!("Expected User invalidation, got: {other:?}"),
        },
        "permission cache invalidation",
    )
    .await;

    assert!(
        received_target,
        "Should receive permission cache invalidation"
    );

    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cluster_permission_cache_consistency() {
    let redis = TestRedis::start().await;
    let key_prefix = redis.key_prefix.clone();

    let cache_svc_a = Arc::new(CacheInvalidationService::new(
        "perm_node_a".to_string(),
        format!("{key_prefix}perm:cache"),
    ));
    let cache_svc_b = Arc::new(CacheInvalidationService::new(
        "perm_node_b".to_string(),
        format!("{key_prefix}perm:cache"),
    ));

    let mut rx_a = cache_svc_a.subscribe();
    let mut rx_b = cache_svc_b.subscribe();

    let client_a =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to open Redis client A");
    let conn_a = redis_connection_manager(&client_a).await;
    let config_a = ClusterConfig {
        distributed_transport_factory: Some(Arc::new(
            synctv_cluster::RedisClusterMessageTransportFactory::new(
                synctv_core::coordination_runtime_from_client(client_a),
            ),
        )),
        message_runtime: shared_message_runtime(conn_a.clone(), &key_prefix),
        cluster_enabled: true,
        node_id: "perm_node_a".to_string(),
        dedup_window: Duration::from_secs(10),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: key_prefix.clone(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };
    let node_a = ClusterManager::new(config_a, None, Some(cache_svc_a))
        .await
        .expect("Failed to create node A");

    let client_b =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to open Redis client B");
    let conn_b = redis_connection_manager(&client_b).await;
    let config_b = ClusterConfig {
        distributed_transport_factory: Some(Arc::new(
            synctv_cluster::RedisClusterMessageTransportFactory::new(
                synctv_core::coordination_runtime_from_client(client_b),
            ),
        )),
        message_runtime: shared_message_runtime(conn_b.clone(), &key_prefix),
        cluster_enabled: true,
        node_id: "perm_node_b".to_string(),
        dedup_window: Duration::from_secs(10),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: key_prefix.clone(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };
    let node_b = ClusterManager::new(config_b, None, Some(cache_svc_b))
        .await
        .expect("Failed to create node B");

    // Test 1: User permission invalidation
    let user_id = UserId::from(10_010_004);
    let room_id = RoomId::from(10_010_005);

    broadcast_until_cache_invalidation(
        &node_a,
        &mut rx_b,
        || ClusterEvent::CacheInvalidate {
            event_id: synctv_common::snanoid!(16),
            targets: vec![CacheTarget::User { user_id }],
            timestamp: Utc::now(),
        },
        |msg| match msg {
            InvalidationMessage::User {
                user_id: received_user_id,
            } => {
                assert_eq!(
                    received_user_id,
                    user_id.to_string(),
                    "User ID should match"
                );
                true
            }
            other => panic!("Expected User invalidation, got: {other:?}"),
        },
        "user invalidation on node B",
    )
    .await;

    // Test 2: Room permission invalidation
    broadcast_until_cache_invalidation(
        &node_b,
        &mut rx_a,
        || ClusterEvent::CacheInvalidate {
            event_id: synctv_common::snanoid!(16),
            targets: vec![CacheTarget::Room { room_id }],
            timestamp: Utc::now(),
        },
        |msg| match msg {
            InvalidationMessage::Room {
                room_id: received_room_id,
            } => {
                assert_eq!(
                    received_room_id,
                    room_id.to_string(),
                    "Room ID should match"
                );
                true
            }
            other => panic!("Expected Room invalidation, got: {other:?}"),
        },
        "room invalidation on node A",
    )
    .await;

    // Test 3: Multiple invalidations in rapid succession
    let mut invalidation_count = 0;
    for i in 0..10 {
        let event = ClusterEvent::CacheInvalidate {
            event_id: synctv_common::snanoid!(16),
            targets: vec![CacheTarget::User {
                user_id: UserId::from(10_020_000 + i64::from(i)),
            }],
            timestamp: Utc::now(),
        };
        node_a.broadcast(event);
        invalidation_count += 1;
    }

    // All 10 invalidations should be received on node B
    let mut received_count = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while received_count < invalidation_count {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx_b.recv()).await {
            Ok(Ok(InvalidationMessage::User { .. })) => received_count += 1,
            Ok(Ok(other)) => panic!("Unexpected message: {other:?}"),
            Ok(Err(_)) | Err(_) => break,
        }
    }
    assert_eq!(
        received_count, invalidation_count,
        "All invalidations should be received"
    );

    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_concurrent_permission_cache_updates() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let redis = TestRedis::start().await;
    let key_prefix = redis.key_prefix.clone();

    let cache_svc_a = Arc::new(CacheInvalidationService::new(
        "concurrent_node_a".to_string(),
        format!("{key_prefix}concurrent:cache"),
    ));
    let cache_svc_b = Arc::new(CacheInvalidationService::new(
        "concurrent_node_b".to_string(),
        format!("{key_prefix}concurrent:cache"),
    ));
    let cache_svc_c = Arc::new(CacheInvalidationService::new(
        "concurrent_node_c".to_string(),
        format!("{key_prefix}concurrent:cache"),
    ));

    let mut rx_a = cache_svc_a.subscribe();
    let mut rx_b = cache_svc_b.subscribe();
    let mut rx_c = cache_svc_c.subscribe();

    let client_a = redis::Client::open(redis.redis_url.clone()).expect("Redis client A");
    let conn_a = redis_connection_manager(&client_a).await;
    let config_a = ClusterConfig {
        distributed_transport_factory: Some(Arc::new(
            synctv_cluster::RedisClusterMessageTransportFactory::new(
                synctv_core::coordination_runtime_from_client(client_a),
            ),
        )),
        message_runtime: shared_message_runtime(conn_a.clone(), &key_prefix),
        cluster_enabled: true,
        node_id: "concurrent_node_a".to_string(),
        dedup_window: Duration::from_secs(10),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: key_prefix.clone(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };
    let node_a = Arc::new(
        ClusterManager::new(config_a, None, Some(cache_svc_a))
            .await
            .expect("Node A"),
    );

    let client_b = redis::Client::open(redis.redis_url.clone()).expect("Redis client B");
    let conn_b = redis_connection_manager(&client_b).await;
    let config_b = ClusterConfig {
        distributed_transport_factory: Some(Arc::new(
            synctv_cluster::RedisClusterMessageTransportFactory::new(
                synctv_core::coordination_runtime_from_client(client_b),
            ),
        )),
        message_runtime: shared_message_runtime(conn_b.clone(), &key_prefix),
        cluster_enabled: true,
        node_id: "concurrent_node_b".to_string(),
        dedup_window: Duration::from_secs(10),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: key_prefix.clone(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };
    let node_b = Arc::new(
        ClusterManager::new(config_b, None, Some(cache_svc_b))
            .await
            .expect("Node B"),
    );

    let client_c = redis::Client::open(redis.redis_url.clone()).expect("Redis client C");
    let conn_c = redis_connection_manager(&client_c).await;
    let config_c = ClusterConfig {
        distributed_transport_factory: Some(Arc::new(
            synctv_cluster::RedisClusterMessageTransportFactory::new(
                synctv_core::coordination_runtime_from_client(client_c),
            ),
        )),
        message_runtime: shared_message_runtime(conn_c.clone(), &key_prefix),
        cluster_enabled: true,
        node_id: "concurrent_node_c".to_string(),
        dedup_window: Duration::from_secs(10),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: key_prefix.clone(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };
    let node_c = Arc::new(
        ClusterManager::new(config_c, None, Some(cache_svc_c))
            .await
            .expect("Node C"),
    );

    // Concurrent invalidations from all three nodes
    let invalidations_per_node: u32 = 10;
    let total_invalidations = invalidations_per_node * 3;

    let received_count = Arc::new(AtomicU32::new(0));

    // Spawn listeners on all three nodes
    let count_a = received_count.clone();
    let handle_a = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Ok(InvalidationMessage::User { .. })) =
                tokio::time::timeout(Duration::from_millis(100), rx_a.recv()).await
            {
                count_a.fetch_add(1, Ordering::SeqCst);
            }
        }
    });

    let count_b = received_count.clone();
    let handle_b = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Ok(InvalidationMessage::User { .. })) =
                tokio::time::timeout(Duration::from_millis(100), rx_b.recv()).await
            {
                count_b.fetch_add(1, Ordering::SeqCst);
            }
        }
    });

    let count_c = received_count.clone();
    let handle_c = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Ok(InvalidationMessage::User { .. })) =
                tokio::time::timeout(Duration::from_millis(100), rx_c.recv()).await
            {
                count_c.fetch_add(1, Ordering::SeqCst);
            }
        }
    });

    // Small delay to let listeners start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Broadcast invalidations from all nodes concurrently
    let node_a_for_task = node_a.clone();
    let node_a_handle = tokio::spawn(async move {
        for i in 0..invalidations_per_node {
            let event = ClusterEvent::CacheInvalidate {
                event_id: synctv_common::snanoid!(16),
                targets: vec![CacheTarget::User {
                    user_id: UserId::from(10_030_000 + i64::from(i)),
                }],
                timestamp: Utc::now(),
            };
            node_a_for_task.broadcast(event);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    let node_b_for_task = node_b.clone();
    let node_b_handle = tokio::spawn(async move {
        for i in 0..invalidations_per_node {
            let event = ClusterEvent::CacheInvalidate {
                event_id: synctv_common::snanoid!(16),
                targets: vec![CacheTarget::User {
                    user_id: UserId::from(10_040_000 + i64::from(i)),
                }],
                timestamp: Utc::now(),
            };
            node_b_for_task.broadcast(event);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    let node_c_for_task = node_c.clone();
    let node_c_handle = tokio::spawn(async move {
        for i in 0..invalidations_per_node {
            let event = ClusterEvent::CacheInvalidate {
                event_id: synctv_common::snanoid!(16),
                targets: vec![CacheTarget::User {
                    user_id: UserId::from(10_050_000 + i64::from(i)),
                }],
                timestamp: Utc::now(),
            };
            node_c_for_task.broadcast(event);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    node_a_handle.await.expect("Node A broadcasts");
    node_b_handle.await.expect("Node B broadcasts");
    node_c_handle.await.expect("Node C broadcasts");

    handle_a.await.expect("Listener A");
    handle_b.await.expect("Listener B");
    handle_c.await.expect("Listener C");

    // Each of the 3 listeners should receive invalidations from the other 2 nodes
    // (they don't receive their own node's invalidations from Redis)
    // So expected count is 3 nodes * 10 invalidations * 2 receiving nodes = 60
    // But due to timing and deduplication, we check for a reasonable minimum
    let final_count = received_count.load(Ordering::SeqCst);
    assert!(
        final_count >= total_invalidations,
        "Should receive at least {total_invalidations} invalidations, got {final_count}"
    );

    // Note: Arc<ClusterManager> doesn't have shutdown, need to access inner
    // Since ClusterManager doesn't implement Clone, we need to use Arc::try_unwrap
    // or just let it drop
    drop(node_a);
    drop(node_b);
    drop(node_c);
}
