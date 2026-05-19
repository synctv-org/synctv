//! Cache invalidation tests for multi-replica settings
//!
//! Tests verify cache invalidation works correctly across multiple replicas
//! using Redis Pub/Sub or Streams.
//!
//! Run with: cargo test --test `cache_invalidation_tests`
//! Requires Docker for testcontainers.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::{
    cache::{CacheInvalidationService, InvalidationMessage},
    models::{RoomId, RoomMember, UserId},
    repository::RoomMemberRepository,
};
use synctv_core_testing::{
    create_test_pool_with_options_and_label, redis_connection_manager,
    start_redis_url as start_test_redis_url,
};
use tokio::sync::RwLock;

async fn start_redis() -> (synctv_core_testing::RedisContainer, String) {
    start_test_redis_url().await
}

fn unique_stream_key() -> String {
    format!("test:cache:invalidate:{}", synctv_common::snanoid!(8))
}

async fn distributed_invalidation_service(
    redis_client: redis::Client,
    node_id: &str,
    stream_key: String,
) -> Arc<CacheInvalidationService> {
    Arc::new(CacheInvalidationService::from_runtime(
        synctv_core::direct_runtime(redis_connection_manager(&redis_client).await),
        node_id.to_string(),
        stream_key,
    ))
}

fn shared_runtime_invalidation_service(
    shared_conn: Arc<RwLock<redis::aio::ConnectionManager>>,
    node_id: &str,
    stream_key: String,
) -> Arc<CacheInvalidationService> {
    Arc::new(CacheInvalidationService::from_runtime(
        synctv_core::shared_runtime(shared_conn),
        node_id.to_string(),
        stream_key,
    ))
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_message_serialization() {
    let msg = InvalidationMessage::UserPermission {
        room_id: "room123".to_string(),
        user_id: "user456".to_string(),
    };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    let deserialized: InvalidationMessage =
        serde_json::from_str(&json).expect("Failed to deserialize");

    assert_eq!(msg, deserialized);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_broadcast_received() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_stream_key();

    let service1 =
        distributed_invalidation_service(redis_client.clone(), "node1", stream_key.clone()).await;

    let service2 =
        distributed_invalidation_service(redis_client.clone(), "node2", stream_key).await;

    service1.start().await.expect("Failed to start service1");
    service2.start().await.expect("Failed to start service2");

    // Subscribe to service2's local channel
    let mut receiver = service2.subscribe();

    // Broadcast from service1
    let room_id = RoomId::new();
    let user_id = UserId::new();

    service1
        .invalidate_user_permission(&room_id, &user_id)
        .await
        .expect("Failed to broadcast invalidation");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::UserPermission { room_id: r, user_id: u } => {
                    assert_eq!(r, room_id.to_string());
                    assert_eq!(u, user_id.to_string());
                }
                _ => panic!("Unexpected message type"),
            }
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }

    service1.stop().await;
    service2.stop().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_all_message() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_stream_key();

    let service1 =
        distributed_invalidation_service(redis_client.clone(), "node1", stream_key.clone()).await;

    let service2 =
        distributed_invalidation_service(redis_client.clone(), "node2", stream_key).await;

    service1.start().await.expect("Failed to start service1");
    service2.start().await.expect("Failed to start service2");

    let mut receiver = service2.subscribe();

    // Broadcast invalidate all
    service1
        .invalidate_all()
        .await
        .expect("Failed to broadcast invalidation");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            assert_eq!(msg, InvalidationMessage::All);
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }

    service1.stop().await;
    service2.stop().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_room_permission() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_stream_key();

    let service1 =
        distributed_invalidation_service(redis_client.clone(), "node1", stream_key.clone()).await;

    let service2 = distributed_invalidation_service(redis_client, "node2", stream_key).await;

    service1.start().await.expect("Failed to start service1");
    service2.start().await.expect("Failed to start service2");

    let mut receiver = service2.subscribe();

    let room_id = RoomId::new();
    service1
        .invalidate_room_permission(&room_id)
        .await
        .expect("Failed to broadcast invalidation");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::RoomPermission { room_id: r } => {
                    assert_eq!(r, room_id.to_string());
                }
                _ => panic!("Expected RoomPermission message"),
            }
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }

    service1.stop().await;
    service2.stop().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_multiple_messages() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_stream_key();

    let service1 =
        distributed_invalidation_service(redis_client.clone(), "node1", stream_key.clone()).await;

    let service2 = distributed_invalidation_service(redis_client, "node2", stream_key).await;

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

    let room1 = RoomId::new();
    let room2 = RoomId::new();
    let user1 = UserId::new();

    service1
        .invalidate_room(&room1)
        .await
        .expect("Failed to invalidate room1");
    service1
        .invalidate_room(&room2)
        .await
        .expect("Failed to invalidate room2");
    service1
        .invalidate_user(&user1)
        .await
        .expect("Failed to invalidate user");

    tokio::time::timeout(tokio::time::Duration::from_secs(5), receiver_handle)
        .await
        .expect("Timeout")
        .expect("Receiver task failed");

    let messages = received_messages.read().await;
    assert_eq!(messages.len(), 3, "Should receive 3 messages");

    service1.stop().await;
    service2.stop().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_without_redis() {
    // Service without Redis should work in local-only mode.
    // Note: invalidate_* methods only broadcast remotely via Redis.
    // Without Redis, they are no-ops. The local_sender channel is used
    // only when receiving messages FROM Redis (via the consumer task).
    let service = CacheInvalidationService::new("node1".to_string(), unique_stream_key());

    service
        .start()
        .await
        .expect("Failed to start service without Redis");

    // invalidate_* methods should return Ok (no-op without Redis)
    let room_id = RoomId::new();
    service
        .invalidate_room(&room_id)
        .await
        .expect("Failed to invalidate (should be no-op)");

    let user_id = UserId::new();
    service
        .invalidate_user_permission(&room_id, &user_id)
        .await
        .expect("Failed to invalidate (should be no-op)");

    service
        .invalidate_all()
        .await
        .expect("Failed to invalidate all (should be no-op)");

    service.stop().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_self_origin_not_received() {
    // Messages originating from a node's own node_id should NOT be delivered
    // to that node's local subscriber (the subscriber filters them out).
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");

    let service =
        distributed_invalidation_service(redis_client, "self_node", unique_stream_key()).await;

    service.start().await.expect("Failed to start service");

    let mut receiver = service.subscribe();

    // Broadcast from the SAME node (self-origin)
    let room_id = RoomId::new();
    service
        .invalidate_room(&room_id)
        .await
        .expect("Failed to broadcast");

    // The subscriber should NOT receive the self-originated message.
    // Use a short timeout to verify nothing arrives.
    let result = tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver.recv()).await;

    assert!(
        result.is_err(),
        "Self-originated message should NOT be delivered to the same node's subscriber"
    );

    service.stop().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_broadcast_local() {
    // broadcast_local should deliver to local subscribers without using Redis
    let service = CacheInvalidationService::new(
        "local_node".to_string(),
        "test:cache:local_only".to_string(),
    );

    let mut receiver = service.subscribe();

    let msg = InvalidationMessage::User {
        user_id: "user_local_test".to_string(),
    };
    service
        .broadcast_local(msg.clone())
        .expect("broadcast_local should succeed");

    tokio::select! {
        received = receiver.recv() => {
            let received = received.expect("Should receive local broadcast");
            assert_eq!(received, msg);
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(2)) => {
            panic!("Timeout waiting for local broadcast message");
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_playback_state() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_stream_key();

    let service1 =
        distributed_invalidation_service(redis_client.clone(), "node1", stream_key.clone()).await;

    let service2 = distributed_invalidation_service(redis_client, "node2", stream_key).await;

    service1.start().await.expect("Failed to start service1");
    service2.start().await.expect("Failed to start service2");

    let mut receiver = service2.subscribe();

    let room_id = RoomId::new();
    service1
        .invalidate_playback_state(&room_id)
        .await
        .expect("Failed to broadcast invalidation");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::PlaybackState { room_id: r } => {
                    assert_eq!(r, room_id.to_string());
                }
                _ => panic!("Expected PlaybackState message"),
            }
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }

    service1.stop().await;
    service2.stop().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_with_shared_conn_without_client_still_broadcasts() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let shared_conn = Arc::new(RwLock::new(redis_connection_manager(&redis_client).await));
    let stream_key = unique_stream_key();

    drop(redis_client);
    let service1 =
        shared_runtime_invalidation_service(shared_conn.clone(), "node1", stream_key.clone());
    let service2 = shared_runtime_invalidation_service(shared_conn, "node2", stream_key);

    service1.start().await.expect("Failed to start service1");
    service2.start().await.expect("Failed to start service2");

    let mut receiver = service2.subscribe();
    let room_id = RoomId::new();

    service1
        .invalidate_room(&room_id)
        .await
        .expect("shared-conn-only service should still publish remotely");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::Room { room_id: r } => {
                    assert_eq!(r, room_id.to_string());
                }
                other => panic!("Expected Room invalidation, got {other:?}"),
            }
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for shared-conn invalidation message");
        }
    }

    service1.stop().await;
    service2.stop().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_restart_resets_consumer_group_for_same_node() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_stream_key();
    let node_id = "restart-node";
    let consumer_group = format!("cache-invalidation-{node_id}");

    let mut setup_conn = redis_connection_manager(&redis_client).await;

    let _: String = redis::cmd("XADD")
        .arg(&stream_key)
        .arg("*")
        .arg("origin")
        .arg("other-node")
        .arg("payload")
        .arg(
            serde_json::to_string(&InvalidationMessage::All)
                .expect("Failed to serialize invalidation"),
        )
        .query_async(&mut setup_conn)
        .await
        .expect("Failed to seed stream");

    let _: () = redis::cmd("XGROUP")
        .arg("CREATE")
        .arg(&stream_key)
        .arg(&consumer_group)
        .arg("0")
        .query_async(&mut setup_conn)
        .await
        .expect("Failed to create consumer group");

    let pending_reply: redis::streams::StreamReadReply = redis::cmd("XREADGROUP")
        .arg("GROUP")
        .arg(&consumer_group)
        .arg(node_id)
        .arg("COUNT")
        .arg(10)
        .arg("STREAMS")
        .arg(&stream_key)
        .arg(">")
        .query_async(&mut setup_conn)
        .await
        .expect("Failed to create pending delivery");
    assert_eq!(pending_reply.keys.len(), 1, "expected seeded stream entry");
    assert_eq!(
        pending_reply.keys[0].ids.len(),
        1,
        "expected one pending entry"
    );

    let service =
        distributed_invalidation_service(redis_client.clone(), node_id, stream_key.clone()).await;
    service
        .start()
        .await
        .expect("Failed to start restarted service");

    let mut receiver = service.subscribe();
    assert!(
        tokio::time::timeout(tokio::time::Duration::from_millis(500), receiver.recv())
            .await
            .is_err(),
        "restart should not replay pending invalidations for the same node"
    );

    let pending: Vec<redis::Value> = redis::cmd("XPENDING")
        .arg(&stream_key)
        .arg(&consumer_group)
        .query_async(&mut setup_conn)
        .await
        .expect("Failed to inspect pending state");
    let summary = format!("{pending:?}");
    assert!(
        summary.contains("int(0)"),
        "restart should recreate the consumer group without inherited pending entries, got: {summary}"
    );

    let groups: Vec<Vec<redis::Value>> = redis::cmd("XINFO")
        .arg("GROUPS")
        .arg(&stream_key)
        .query_async(&mut setup_conn)
        .await
        .expect("Failed to inspect consumer groups");
    let groups_summary = format!("{groups:?}");
    assert!(
        groups_summary.contains(&consumer_group),
        "restart must recreate the consumer group: {groups_summary}"
    );

    service.stop().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_stop_destroys_current_consumer_group() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_stream_key();
    let node_id = "shutdown-node";
    let consumer_group = format!("cache-invalidation-{node_id}");

    let service =
        distributed_invalidation_service(redis_client.clone(), node_id, stream_key.clone()).await;
    service.start().await.expect("Failed to start service");
    service.stop().await;

    let mut conn = redis_connection_manager(&redis_client).await;
    let groups: redis::RedisResult<Vec<Vec<redis::Value>>> = redis::cmd("XINFO")
        .arg("GROUPS")
        .arg(&stream_key)
        .query_async(&mut conn)
        .await;

    match groups {
        Ok(groups) => {
            let summary = format!("{groups:?}");
            assert!(
                !summary.contains(&consumer_group),
                "shutdown should remove the node's consumer group: {summary}"
            );
        }
        Err(error) => {
            let message = error.to_string().to_ascii_lowercase();
            assert!(
                message.contains("no such key"),
                "unexpected XINFO GROUPS error after shutdown: {error}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_start_cleans_empty_foreign_orphan_group() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_stream_key();
    let foreign_group = "cache-invalidation-old-node";

    let mut conn = redis_connection_manager(&redis_client).await;

    let _: () = redis::cmd("XGROUP")
        .arg("CREATE")
        .arg(&stream_key)
        .arg(foreign_group)
        .arg("$")
        .arg("MKSTREAM")
        .query_async(&mut conn)
        .await
        .expect("Failed to create orphan foreign group");

    let service =
        distributed_invalidation_service(redis_client.clone(), "current-node", stream_key.clone())
            .await;
    service.start().await.expect("Failed to start service");

    let groups: redis::RedisResult<Vec<Vec<redis::Value>>> = redis::cmd("XINFO")
        .arg("GROUPS")
        .arg(&stream_key)
        .query_async(&mut conn)
        .await;

    let summary = format!("{groups:?}");
    assert!(
        !summary.contains(foreign_group),
        "startup should clean empty foreign orphan groups: {summary}"
    );

    service.stop().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_start_preserves_recent_foreign_group() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_stream_key();
    let foreign_group = "cache-invalidation-remote-node";
    let foreign_consumer = "remote-node";

    let mut conn = redis_connection_manager(&redis_client).await;

    let _: String = redis::cmd("XADD")
        .arg(&stream_key)
        .arg("*")
        .arg("origin")
        .arg("origin-node")
        .arg("payload")
        .arg(
            serde_json::to_string(&InvalidationMessage::All)
                .expect("Failed to serialize invalidation"),
        )
        .query_async(&mut conn)
        .await
        .expect("Failed to seed stream");

    let _: () = redis::cmd("XGROUP")
        .arg("CREATE")
        .arg(&stream_key)
        .arg(foreign_group)
        .arg("0")
        .query_async(&mut conn)
        .await
        .expect("Failed to create foreign group");

    let _: redis::streams::StreamReadReply = redis::cmd("XREADGROUP")
        .arg("GROUP")
        .arg(foreign_group)
        .arg(foreign_consumer)
        .arg("COUNT")
        .arg(1)
        .arg("STREAMS")
        .arg(&stream_key)
        .arg(">")
        .query_async(&mut conn)
        .await
        .expect("Failed to mark foreign consumer as active");

    let service =
        distributed_invalidation_service(redis_client.clone(), "current-node", stream_key.clone())
            .await;
    service.start().await.expect("Failed to start service");

    let groups: Vec<Vec<redis::Value>> = redis::cmd("XINFO")
        .arg("GROUPS")
        .arg(&stream_key)
        .query_async(&mut conn)
        .await
        .expect("Failed to inspect groups after startup");
    let summary = format!("{groups:?}");
    assert!(
        summary.contains(foreign_group),
        "startup must preserve recently active foreign groups: {summary}"
    );

    service.stop().await;
}

// Cache Invalidation Timing Tests

/// Test that cache invalidation happens only AFTER transaction commit.
///
/// Broadcasting invalidation before commit lets other replicas miss cache and
/// repopulate stale state from rows that are still visible in the open transaction.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_after_commit() {
    use synctv_core::{
        cache::{KeyBuilder, UsernameCache},
        config::PasswordComplexityConfig,
        models::{Room, RoomId, User, UserId, UserRole, UserStatus},
        repository::{RoomRepository, UserRepository},
        service::auth::{BruteForceProtection, JwtService},
        service::{InMemoryTokenBlacklistStore, RoomService, UserService},
    };
    let (_postgres, pool) = create_test_pool_with_options_and_label(
        "synctv_test",
        "cache-invalidation-before-commit",
        20,
        std::time::Duration::from_secs(30),
    )
    .await;

    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let user_service = UserService::new(
        &pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    );

    let mut room_service = RoomService::new(pool.clone(), user_service);
    let invalidation_service = Arc::new(CacheInvalidationService::new(
        "room-delete-node".to_string(),
        unique_stream_key(),
    ));
    room_service.set_cache_invalidation(invalidation_service.clone());
    room_service.set_playback_cache_invalidation(invalidation_service.clone());

    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let user_id = UserId::new();
    let _user = user_repo
        .create(&User {
            id: user_id,
            username: "test_user".to_string(),
            email: Some("test@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            email_verified: true,
            signup_method: synctv_core::models::SignupMethod::Email,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
            deleted_at: None,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        })
        .await
        .expect("Failed to create user");

    let room_id = RoomId::new();
    let _room = room_repo
        .create(&Room {
            id: room_id,
            name: "Test Room".to_string(),
            description: "A test room".to_string(),
            created_by: user_id,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
            deleted_at: None,
            is_banned: false,
            status: synctv_core::models::RoomStatus::Active,
            closed_at: None,
            last_activity_at: chrono::Utc::now(),
        })
        .await
        .expect("Failed to create room");

    let member_repo = RoomMemberRepository::new(pool.clone());
    let _member = member_repo
        .add(&RoomMember {
            room_id,
            user_id,
            role: synctv_core::models::RoomRole::Creator,
            status: synctv_core::models::MemberStatus::Active,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: chrono::Utc::now(),
            version: 0,
        })
        .await
        .expect("Failed to create member");

    // Prime the read path before deletion.
    let _ = room_service.get_room(&room_id).await;
    let mut invalidation_rx = invalidation_service.subscribe();

    // Now delete the room - invalidation must not become observable until
    // the soft delete is already committed.
    room_service
        .delete_room(room_id, user_id)
        .await
        .expect("Failed to delete room");

    let observed_room_id = tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        loop {
            let msg = invalidation_rx
                .recv()
                .await
                .expect("invalidation channel open");
            if let InvalidationMessage::Room { room_id } = msg {
                break room_id;
            }
        }
    })
    .await
    .expect("timed out waiting for room invalidation");
    assert_eq!(observed_room_id, room_id.to_string());

    // At the moment invalidation becomes observable, the DB mutation must
    // already be committed.
    let deleted_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM rooms WHERE id = $1")
            .bind(room_id)
            .fetch_optional(&pool)
            .await
            .expect("Failed to query room")
            .flatten();

    assert!(
        deleted_at.is_some(),
        "room must already be soft-deleted when invalidation is observed"
    );

    // Verify cache is invalidated (next read should not return the deleted room)
    let result = room_service.get_room(&room_id).await;
    assert!(result.is_err(), "Deleted room should not be accessible");
    assert!(
        matches!(result.unwrap_err(), synctv_core::Error::NotFound(_)),
        "Should return NotFound"
    );
}

/// Test that a rolled back delete does not broadcast room invalidation.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_rollback_does_not_broadcast() {
    use sqlx::Transaction;
    use synctv_core::{
        cache::{KeyBuilder, UsernameCache},
        config::PasswordComplexityConfig,
        models::{Room, RoomId, User, UserId, UserRole, UserStatus},
        repository::{RoomRepository, UserRepository},
        service::auth::{BruteForceProtection, JwtService},
        service::{InMemoryTokenBlacklistStore, RoomService, UserService},
    };
    let (_postgres, pool) = create_test_pool_with_options_and_label(
        "synctv_test",
        "cache-invalidation-rollback",
        20,
        std::time::Duration::from_secs(30),
    )
    .await;

    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let user_service = UserService::new(
        &pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    );

    let mut room_service = RoomService::new(pool.clone(), user_service);
    let invalidation_service = Arc::new(CacheInvalidationService::new(
        "room-rollback-node".to_string(),
        unique_stream_key(),
    ));
    room_service.set_cache_invalidation(invalidation_service.clone());
    room_service.set_playback_cache_invalidation(invalidation_service.clone());

    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let user_id = UserId::new();
    let _user = user_repo
        .create(&User {
            id: user_id,
            username: "test_user_rollback".to_string(),
            email: Some("test2@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            email_verified: true,
            signup_method: synctv_core::models::SignupMethod::Email,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
            deleted_at: None,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        })
        .await
        .expect("Failed to create user");

    let room_id = RoomId::new();
    let _room = room_repo
        .create(&Room {
            id: room_id,
            name: "Test Room Rollback".to_string(),
            description: "A test room for rollback".to_string(),
            created_by: user_id,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
            deleted_at: None,
            is_banned: false,
            status: synctv_core::models::RoomStatus::Active,
            closed_at: None,
            last_activity_at: chrono::Utc::now(),
        })
        .await
        .expect("Failed to create room");

    // Populate cache by reading the room
    let room_before = room_service
        .get_room(&room_id)
        .await
        .expect("Failed to get room");
    assert_eq!(room_before.id, room_id);
    let mut invalidation_rx = invalidation_service.subscribe();

    // Simulate a transaction rollback scenario by manually running the operations
    // that delete_room does, but rolling back the transaction instead of committing.

    let mut tx: Transaction<sqlx::Postgres> =
        pool.begin().await.expect("Failed to start transaction");

    // Mark room as deleted (same as delete_room does)
    let _deleted = sqlx::query(
        "UPDATE rooms
         SET deleted_at = $2, updated_at = $2
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(room_id)
    .bind(chrono::Utc::now())
    .execute(&mut *tx)
    .await
    .expect("Failed to delete room");

    // Rollback the transaction (simulating a failure)
    tx.rollback().await.expect("Failed to rollback transaction");

    // Verify room is still active (not deleted) in the database
    let room_after = room_repo
        .get_by_id(&room_id)
        .await
        .expect("Failed to fetch room");
    assert!(room_after.is_some(), "Room should still exist");
    let room_after = room_after.unwrap();
    assert!(
        room_after.deleted_at.is_none(),
        "Room should NOT be marked as deleted"
    );

    assert!(
        tokio::time::timeout(
            tokio::time::Duration::from_millis(250),
            invalidation_rx.recv()
        )
        .await
        .is_err(),
        "rolled back delete must not broadcast invalidation"
    );

    // Verify cache still serves the active room after rollback.
    let room_from_cache = room_service
        .get_room(&room_id)
        .await
        .expect("Failed to get room after rollback");
    assert_eq!(
        room_from_cache.id, room_id,
        "Should be able to read room after rollback"
    );
}
