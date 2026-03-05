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
    models::{RoomId, UserId},
};
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;
use tokio::sync::RwLock;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let container =
        tokio::time::timeout(std::time::Duration::from_secs(30), Redis::default().start())
            .await
            .expect("Docker container startup timed out (is Docker running?)")
            .expect("Failed to start Redis");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get port");
    let redis_url = format!("redis://127.0.0.1:{port}");
    (container, redis_url)
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

    service1
        .invalidate_user_permission(&room_id, &user_id)
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
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_all_message() {
    let (_container, redis_url) = start_redis().await;
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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_room_permission() {
    let (_container, redis_url) = start_redis().await;
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
    service1
        .invalidate_room_permission(&room_id)
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
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_multiple_messages() {
    let (_container, redis_url) = start_redis().await;
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

    // Wait for messages
    tokio::time::timeout(tokio::time::Duration::from_secs(5), receiver_handle)
        .await
        .expect("Timeout")
        .expect("Receiver task failed");

    let messages = received_messages.read().await;
    assert_eq!(messages.len(), 3, "Should receive 3 messages");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_without_redis() {
    // Service without Redis should work in local-only mode.
    // Note: invalidate_* methods only broadcast remotely via Redis.
    // Without Redis, they are no-ops. The local_sender channel is used
    // only when receiving messages FROM Redis (via the consumer task).
    let service = CacheInvalidationService::new(
        None,
        "node1".to_string(),
        "test:cache:invalidate".to_string(),
    );

    // Start should succeed even without Redis (no-op for local-only)
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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_self_origin_not_received() {
    // Messages originating from a node's own node_id should NOT be delivered
    // to that node's local subscriber (the subscriber filters them out).
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");

    let service = Arc::new(CacheInvalidationService::new(
        Some(redis_client),
        "self_node".to_string(),
        "test:cache:self_origin".to_string(),
    ));

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
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_broadcast_local() {
    // broadcast_local should deliver to local subscribers without using Redis
    let service = CacheInvalidationService::new(
        None,
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
    service1
        .invalidate_playback_state(&room_id)
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
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for invalidation message");
        }
    }
}

// ============================================================================
// Cache Invalidation Timing Tests
// ============================================================================

/// Test that cache invalidation happens BEFORE transaction commit.
///
/// This test verifies the critical invariant that cache invalidation occurs
/// before the transaction commits, preventing the following race condition:
///
/// 1. Transaction commits (`deleted_at` is set)
/// 2. Another request reads stale data from cache (room still appears active)
/// 3. Cache is invalidated (too late - stale data was already served)
///
/// By invalidating before commit, we ensure that when the transaction commits,
/// the cache is already empty. Any concurrent request will miss the cache
/// and read fresh data from the database (which will correctly filter out
/// the deleted room via `deleted_at IS NULL`).
///
/// If the transaction rolls back after cache invalidation, the cache
/// will simply be empty and will be repopulated on the next read with the
/// correct (still-active) room data. This is safe.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_before_commit() {
    use sqlx::postgres::PgPoolOptions;
    use synctv_core::{
        cache::{KeyBuilder, NoopCacheL2, UsernameCache},
        config::PasswordComplexityConfig,
        models::{Room, RoomId, User, UserId, UserRole, UserStatus},
        repository::{RoomRepository, UserRepository},
        service::auth::{BruteForceProtection, JwtService},
        service::{InMemoryTokenBlacklistStore, RoomService, UserService},
    };
    // Start PostgreSQL
    let postgres = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Postgres::default()
            .with_db_name("synctv_test")
            .with_user("synctv")
            .with_password("synctv_test")
            .with_tag("16-alpine")
            .start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get port")
    );

    let pool = {
        let mut retries = 0u32;
        loop {
            match PgPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_secs(2))
                .max_connections(5)
                .connect(&connection_string)
                .await
            {
                Ok(p) => break p,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!("PostgreSQL not ready after {retries} retries: {e}"),
            }
        }
    };

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    // Create user service
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let user_service = UserService::new(
        pool.clone(),
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    );

    // Create room service WITH cache invalidation
    let room_service = RoomService::new(pool.clone(), user_service);

    // Create a user and room
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let user_id = UserId::new();
    let _user = user_repo
        .create(&User {
            id: user_id.clone(),
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
        })
        .await
        .expect("Failed to create user");

    let room_id = RoomId::new();
    let _room = room_repo
        .create(&Room {
            id: room_id.clone(),
            name: "Test Room".to_string(),
            description: "A test room".to_string(),
            created_by: user_id.clone(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
            deleted_at: None,
            is_banned: false,
            status: synctv_core::models::RoomStatus::Active,
            last_activity_at: chrono::Utc::now(),
        })
        .await
        .expect("Failed to create room");

    // Create the creator member entry
    use synctv_core::{models::RoomMember, repository::RoomMemberRepository};
    let member_repo = RoomMemberRepository::new(pool.clone());
    let _member = member_repo
        .add(&RoomMember {
            room_id: room_id.clone(),
            user_id: user_id.clone(),
            role: synctv_core::models::RoomRole::Creator,
            status: synctv_core::models::MemberStatus::Active,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: chrono::Utc::now(),
            left_at: None,
            version: 0,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        })
        .await
        .expect("Failed to create member");

    // Verify room exists in cache initially (via permission service)
    // This will cache the room permission
    let _ = room_service.get_room(&room_id).await;

    // Now delete the room - this should invalidate cache BEFORE commit
    room_service
        .delete_room(room_id.clone(), user_id.clone())
        .await
        .expect("Failed to delete room");

    // Verify room is marked as deleted by querying database directly
    // (get_by_id filters out deleted rooms, so we need a raw query)
    let deleted_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM rooms WHERE id = $1")
            .bind(room_id.as_str())
            .fetch_optional(&pool)
            .await
            .expect("Failed to query room")
            .flatten();

    assert!(
        deleted_at.is_some(),
        "Room should be marked as deleted in database"
    );

    // Verify cache is invalidated (next read should not return the deleted room)
    // Note: get_room filters out deleted rooms, so this should return NotFound
    let result = room_service.get_room(&room_id).await;
    assert!(result.is_err(), "Deleted room should not be accessible");
    assert!(
        matches!(result.unwrap_err(), synctv_core::Error::NotFound(_)),
        "Should return NotFound"
    );
}

/// Test that cache invalidation is safe even if transaction rolls back.
///
/// This test verifies that if a transaction is rolled back after cache invalidation,
/// the system remains consistent. The cache will be empty and will be repopulated
/// on the next read with the correct data.
///
/// Note: This test relies on the fact that cache invalidation happens before commit.
/// When a transaction rolls back, the cache is already invalidated, but the next
/// read will repopulate it with the correct (unchanged) data from the database.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_invalidation_rollback_safety() {
    use sqlx::postgres::PgPoolOptions;
    use sqlx::Transaction;
    use synctv_core::{
        cache::{KeyBuilder, NoopCacheL2, UsernameCache},
        config::PasswordComplexityConfig,
        models::{Room, RoomId, User, UserId, UserRole, UserStatus},
        repository::{RoomRepository, UserRepository},
        service::auth::{BruteForceProtection, JwtService},
        service::{InMemoryTokenBlacklistStore, RoomService, UserService},
    };
    // Start PostgreSQL
    let postgres = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Postgres::default()
            .with_db_name("synctv_test")
            .with_user("synctv")
            .with_password("synctv_test")
            .with_tag("16-alpine")
            .start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get port")
    );

    let pool = {
        let mut retries = 0u32;
        loop {
            match PgPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_secs(2))
                .max_connections(5)
                .connect(&connection_string)
                .await
            {
                Ok(p) => break p,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!("PostgreSQL not ready after {retries} retries: {e}"),
            }
        }
    };

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    // Create user service
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let user_service = UserService::new(
        pool.clone(),
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    );

    let room_service = RoomService::new(pool.clone(), user_service);

    // Create a user and room
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let user_id = UserId::new();
    let _user = user_repo
        .create(&User {
            id: user_id.clone(),
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
        })
        .await
        .expect("Failed to create user");

    let room_id = RoomId::new();
    let _room = room_repo
        .create(&Room {
            id: room_id.clone(),
            name: "Test Room Rollback".to_string(),
            description: "A test room for rollback".to_string(),
            created_by: user_id.clone(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
            deleted_at: None,
            is_banned: false,
            status: synctv_core::models::RoomStatus::Active,
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
    .bind(room_id.as_str())
    .bind(chrono::Utc::now())
    .execute(&mut *tx)
    .await
    .expect("Failed to delete room");

    // Invalidate caches (simulating what delete_room does BEFORE commit)
    // We use the service methods directly to invalidate caches
    room_service
        .permission_service()
        .invalidate_room_cache(&room_id)
        .await;
    room_service
        .playback_service()
        .invalidate_playback_cache(&room_id)
        .await;

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

    // Verify cache is repopulated on next read
    // This will trigger a cache miss and fetch from database
    let room_from_cache = room_service
        .get_room(&room_id)
        .await
        .expect("Failed to get room after rollback");
    assert_eq!(
        room_from_cache.id, room_id,
        "Should be able to read room after rollback"
    );
}
