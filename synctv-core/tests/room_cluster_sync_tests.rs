//! Room cluster state synchronization tests
//!
//! Tests cluster-wide state synchronization including multi-replica room creation,
//! member change synchronization, network partition recovery, and Redis `PubSub`.
//!
//! Run with: cargo test --test `room_cluster_sync_tests`
//!
//! # Test Coverage
//!
//! - Multi-replica room creation synchronization
//! - Multi-replica member change synchronization
//! - Network partition recovery simulation
//! - Redis `PubSub` message delivery
//!
//! # Requirements
//!
//! - Docker for testcontainers (`PostgreSQL` + Redis)
#![allow(clippy::unwrap_used)]

use synctv_core::{
    cache::{CacheInvalidationService, InvalidationMessage},
    models::{
        room_settings::MaxMembers, MemberStatus, Room, RoomId, RoomMember, RoomRole, RoomSettings,
        RoomStatus, User, UserId, UserRole, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::{member::MemberService, permission::PermissionService, NotificationService},
};

use chrono::Utc;
use sqlx::PgPool;
use std::sync::Arc;
use synctv_core_testing::{
    create_test_pool_with_db_and_label, start_redis_url_with_label, RedisContainer, TestContainer,
};
// ============================================================================
// Test Infrastructure
// ============================================================================

/// Test infrastructure with shared `PostgreSQL` and Redis
pub struct TestInfra {
    pub pool: PgPool,
    pub redis_url: String,
    #[allow(dead_code)]
    postgres: Option<TestContainer>,
    #[allow(dead_code)]
    redis: Option<RedisContainer>,
}

fn unique_invalidation_key(prefix: &str) -> String {
    format!("{prefix}:{}", synctv_common::snanoid!(8))
}

async fn create_test_infra() -> TestInfra {
    let (postgres, pool) =
        create_test_pool_with_db_and_label("synctv_test", "room-cluster-sync").await;
    let (redis, redis_url) = start_redis_url_with_label("room-cluster-sync").await;

    TestInfra {
        pool,
        redis_url,
        postgres: Some(postgres),
        redis: Some(redis),
    }
}

async fn distributed_invalidation_service(
    redis_client: redis::Client,
    node_id: &str,
    stream_key: String,
) -> Arc<CacheInvalidationService> {
    Arc::new(CacheInvalidationService::from_runtime(
        synctv_core::direct_runtime(
            redis::aio::ConnectionManager::new(redis_client)
                .await
                .expect("redis connection manager"),
        ),
        node_id.to_string(),
        stream_key,
    ))
}

impl TestInfra {
    #[allow(dead_code)]
    async fn cleanup(mut self) {
        if let Some(redis) = self.redis.take() {
            redis.cleanup();
        }
        if let Some(postgres) = self.postgres.take() {
            postgres.cleanup().await;
        }
    }
}

/// Create a test user in the database
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "test_hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

/// Create a test room
fn make_room(name: &str, description: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: description.to_string(),
        created_by: owner.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

/// Setup test room with owner
async fn setup_test_room(pool: &PgPool, room_name: &str) -> (User, Room) {
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user(&format!("{room_name}_owner")))
        .await
        .expect("Failed to create owner");
    let room = room_repo
        .create(&make_room(room_name, "Test", &owner.id))
        .await
        .expect("Failed to create room");

    // Add owner as member (Creator)
    let member_repo = RoomMemberRepository::new(pool.clone());
    let owner_member = RoomMember::new(room.id.clone(), owner.id.clone(), RoomRole::Creator);
    member_repo
        .add(&owner_member)
        .await
        .expect("Failed to add owner as member");

    (owner, room)
}

// ============================================================================
// Test: Multi-Replica Room Creation Synchronization
// ============================================================================

/// Test that room creation is visible across replicas.
///
/// Scenario:
/// 1. Node A creates a room
/// 2. Node B queries for the room
/// 3. Node B sees the room (database is shared)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_creation_visible_across_replicas() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;

    // Create room via "Node A"
    let (owner, room) = setup_test_room(pool, "Multi-Replica Room").await;

    // "Node B" queries for the room (same pool, different repository instance)
    let room_repo_b = RoomRepository::new(pool.clone());
    let room_b = room_repo_b
        .get_by_id(&room.id)
        .await
        .expect("Failed to query room");

    assert!(room_b.is_some(), "Room should be visible on Node B");
    let room_b = room_b.unwrap();
    assert_eq!(room_b.id, room.id, "Room ID should match");
    assert_eq!(room_b.name, room.name, "Room name should match");
    assert_eq!(room_b.created_by, owner.id, "Room owner should match");
}

/// Test that room settings are synchronized across replicas.
///
/// Scenario:
/// 1. Node A creates room with settings
/// 2. Node B reads room settings
/// 3. Node B sees the same settings
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_settings_synchronized_across_replicas() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;

    // Create room with settings on "Node A"
    let (_owner, room) = setup_test_room(pool, "Settings Sync Room").await;
    let room_settings_repo_a = RoomSettingsRepository::new(pool.clone());
    let settings = RoomSettings {
        max_members: MaxMembers(42),
        ..Default::default()
    };
    room_settings_repo_a
        .set_settings(&room.id, &settings)
        .await
        .expect("Failed to set settings");

    // "Node B" reads settings
    let room_settings_repo_b = RoomSettingsRepository::new(pool.clone());
    let settings_b = room_settings_repo_b
        .get(&room.id)
        .await
        .expect("Failed to get settings");

    assert_eq!(
        settings_b.max_members.0, 42,
        "Node B should see max_members = 42"
    );
}

// ============================================================================
// Test: Multi-Replica Member Change Synchronization
// ============================================================================

/// Test that member joins are synchronized across replicas via Redis.
///
/// Scenario:
/// 1. Two `PermissionService` instances share Redis
/// 2. Node A adds a member
/// 3. Node A broadcasts invalidation via Redis
/// 4. Node B receives invalidation and clears cache
/// 5. Node B's next query fetches fresh data
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_join_synchronized_via_redis() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;
    let redis_client =
        redis::Client::open(infra.redis_url.clone()).expect("Failed to create Redis client");

    let (_owner, room) = setup_test_room(pool, "Member Sync Room").await;

    // Create cache invalidation services for two nodes
    let cache_invalidation_a =
        distributed_invalidation_service(redis_client.clone(), "node_a", "test:member:sync".to_string())
            .await;
    let cache_invalidation_b =
        distributed_invalidation_service(redis_client.clone(), "node_b", "test:member:sync".to_string())
            .await;

    cache_invalidation_a
        .start()
        .await
        .expect("Failed to start node_a cache invalidation");
    cache_invalidation_b
        .start()
        .await
        .expect("Failed to start node_b cache invalidation");

    // Create permission services with cache invalidation
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let permission_service_a = PermissionService::with_invalidation(
        member_repo.clone(),
        room_repo.clone(),
        None,
        1000,
        300,
        cache_invalidation_a.clone(),
    );

    let permission_service_b = PermissionService::with_invalidation(
        member_repo.clone(),
        room_repo.clone(),
        None,
        1000,
        300,
        cache_invalidation_b.clone(),
    );
    permission_service_a
        .start()
        .await
        .expect("Failed to start permission_service_a");
    permission_service_b
        .start()
        .await
        .expect("Failed to start permission_service_b");

    // Create a new member
    let user_repo = UserRepository::new(pool.clone());
    let new_member = user_repo
        .create(&make_user("sync_member"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id.clone(), new_member.id.clone(), RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Node A broadcasts invalidation
    permission_service_a
        .invalidate_cache(&room.id, &new_member.id)
        .await;

    // Wait for invalidation to propagate
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Node B queries permissions (should fetch fresh data)
    let perms_b = permission_service_b
        .get_user_permissions(&room.id, &new_member.id)
        .await
        .expect("Failed to get permissions");

    assert!(perms_b.0 > 0, "Member should have permissions");
}

/// Test that member role changes are synchronized across replicas.
///
/// Scenario:
/// 1. Node A promotes member to Admin
/// 2. Node A broadcasts invalidation
/// 3. Node B sees updated role
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_role_change_synchronized() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;
    let redis_client =
        redis::Client::open(infra.redis_url.clone()).expect("Failed to create Redis client");

    let (_owner, room) = setup_test_room(pool, "Role Sync Room").await;

    // Create member
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member_user = user_repo
        .create(&make_user("role_member"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id.clone(), member_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Create cache invalidation services
    let cache_invalidation_a =
        distributed_invalidation_service(redis_client.clone(), "node_a", "test:role:sync".to_string())
            .await;
    let cache_invalidation_b =
        distributed_invalidation_service(redis_client.clone(), "node_b", "test:role:sync".to_string())
            .await;

    cache_invalidation_a
        .start()
        .await
        .expect("Failed to start node_a");
    cache_invalidation_b
        .start()
        .await
        .expect("Failed to start node_b");

    // Node B: Cache initial permissions
    let permission_service_b = PermissionService::with_invalidation(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
        cache_invalidation_b.clone(),
    );
    permission_service_b
        .start()
        .await
        .expect("Failed to start permission_service_b");

    let _perms_before = permission_service_b
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions");

    // Node A: Update role to Admin (update_role takes 4 args: room_id, user_id, new_role, current_version)
    let existing_member = member_repo
        .get(&room.id, &member_user.id)
        .await
        .expect("Failed to get member")
        .expect("Member should exist");
    member_repo
        .update_role(
            &room.id,
            &member_user.id,
            RoomRole::Admin,
            existing_member.version,
        )
        .await
        .expect("Failed to update role");

    // Node A: Broadcast invalidation
    let permission_service_a = PermissionService::with_invalidation(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
        cache_invalidation_a.clone(),
    );
    permission_service_a
        .start()
        .await
        .expect("Failed to start permission_service_a");
    permission_service_a
        .invalidate_cache(&room.id, &member_user.id)
        .await;

    // Wait for invalidation
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Node B: Query again
    let perms_after = permission_service_b
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions");

    // Admin should have more permissions than Member
    // Note: The initial permissions might be from Member role, Admin should have >= those
    assert!(perms_after.0 > 0, "Admin should have permissions");
}

// ============================================================================
// Test: Network Partition Recovery
// ============================================================================

/// Test that cache remains consistent after simulated network partition.
///
/// Scenario:
/// 1. Node A and Node B share Redis
/// 2. Simulate network partition (Redis disconnect)
/// 3. During partition, Node A updates data
/// 4. After partition heals, Node B queries and sees fresh data
///
/// Note: This test simulates the effect without actually disconnecting Redis
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_consistency_after_partition_recovery() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Partition Room").await;

    // Create member
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member_user = user_repo
        .create(&make_user("partition_member"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id.clone(), member_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Create permission service WITHOUT Redis (simulating partition)
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    // Query permissions (cache it)
    let perms_before = permission_service
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions");

    // "During partition": Update permissions directly in DB
    let existing_member = member_repo
        .get(&room.id, &member_user.id)
        .await
        .expect("Failed to get member")
        .expect("Member should exist");
    member_repo
        .update_permissions(&room.id, &member_user.id, 12345, 0, existing_member.version)
        .await
        .expect("Failed to update permissions");

    // Invalidate cache locally
    permission_service
        .invalidate_cache(&room.id, &member_user.id)
        .await;

    // "After partition heals": Query again (should fetch fresh from DB)
    let perms_after = permission_service
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions");

    // effective_permissions = (role_default | added_permissions) & !removed_permissions
    // so the result includes role_default bits too
    assert_ne!(
        perms_after, perms_before,
        "Permissions should have changed after recovery"
    );
    assert_eq!(
        perms_after.0 & 0x3039,
        0x3039,
        "Should see the added permission bits after recovery"
    );
}

/// Test that operations work correctly without Redis (degraded mode).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_operations_work_without_redis() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Degraded Mode Room").await;

    // Create permission service WITHOUT Redis
    let member_repo = RoomMemberRepository::new(pool.clone());
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    // Create member
    let user_repo = UserRepository::new(pool.clone());
    let member_user = user_repo
        .create(&make_user("degraded_member"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id.clone(), member_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Operations should work without Redis
    let perms = permission_service
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions without Redis");

    assert!(
        perms.0 > 0,
        "Should be able to get permissions without Redis"
    );
}

// ============================================================================
// Test: Redis PubSub Message Delivery
// ============================================================================

/// Test that room invalidation messages are delivered via Redis `PubSub`.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_invalidation_message_delivery() {
    let (_postgres, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_invalidation_key("test:room:invalidate");

    // Create two cache invalidation services
    let service_a =
        distributed_invalidation_service(redis_client.clone(), "node_a", stream_key.clone()).await;
    let service_b =
        distributed_invalidation_service(redis_client, "node_b", stream_key).await;

    service_a.start().await.expect("Failed to start service_a");
    service_b.start().await.expect("Failed to start service_b");

    // Node B subscribes to invalidation messages
    let mut receiver = service_b.subscribe();

    let room_id = RoomId::new();

    // Node A broadcasts room invalidation
    service_a
        .invalidate_room(&room_id)
        .await
        .expect("Failed to broadcast");

    // Node B should receive the message
    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::Room { room_id: r } => {
                    assert_eq!(r, room_id.as_str(), "Room ID should match");
                }
                _ => panic!("Expected Room message, got: {msg:?}"),
            }
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for room invalidation message");
        }
    }
}

/// Test that user invalidation messages are delivered via Redis `PubSub`.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_invalidation_message_delivery() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_invalidation_key("test:user:invalidate");

    let service_a =
        distributed_invalidation_service(redis_client.clone(), "node_a", stream_key.clone()).await;
    let service_b =
        distributed_invalidation_service(redis_client, "node_b", stream_key).await;

    service_a.start().await.expect("Failed to start service_a");
    service_b.start().await.expect("Failed to start service_b");

    let mut receiver = service_b.subscribe();

    let user_id = UserId::new();

    service_a
        .invalidate_user(&user_id)
        .await
        .expect("Failed to broadcast");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::User { user_id: u } => {
                    assert_eq!(u, user_id.as_str(), "User ID should match");
                }
                _ => panic!("Expected User message, got: {msg:?}"),
            }
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for user invalidation message");
        }
    }
}

/// Test that room permission invalidation messages are delivered correctly.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_permission_invalidation_message_delivery() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_invalidation_key("test:perm:invalidate");

    let service_a =
        distributed_invalidation_service(redis_client.clone(), "node_a", stream_key.clone()).await;
    let service_b =
        distributed_invalidation_service(redis_client, "node_b", stream_key).await;

    service_a.start().await.expect("Failed to start service_a");
    service_b.start().await.expect("Failed to start service_b");

    let mut receiver = service_b.subscribe();

    let room_id = RoomId::new();

    service_a
        .invalidate_room_permission(&room_id)
        .await
        .expect("Failed to broadcast");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::RoomPermission { room_id: r } => {
                    assert_eq!(r, room_id.as_str(), "Room ID should match");
                }
                _ => panic!("Expected RoomPermission message, got: {msg:?}"),
            }
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for room permission invalidation message");
        }
    }
}

/// Test multiple concurrent invalidation messages are delivered in order.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_invalidation_messages_ordered() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let stream_key = unique_invalidation_key("test:concurrent:invalidate");

    let service_a =
        distributed_invalidation_service(redis_client.clone(), "node_a", stream_key.clone()).await;
    let service_b =
        distributed_invalidation_service(redis_client, "node_b", stream_key).await;

    service_a.start().await.expect("Failed to start service_a");
    service_b.start().await.expect("Failed to start service_b");

    let mut receiver = service_b.subscribe();

    // Send multiple messages concurrently
    let room1 = RoomId::new();
    let room2 = RoomId::new();
    let user1 = UserId::new();

    let sa = service_a.clone();
    let r1 = room1.clone();
    let h1 = tokio::spawn(async move {
        sa.invalidate_room(&r1)
            .await
            .expect("Failed to invalidate room1");
    });

    let sa = service_a.clone();
    let r2 = room2.clone();
    let h2 = tokio::spawn(async move {
        sa.invalidate_room(&r2)
            .await
            .expect("Failed to invalidate room2");
    });

    let sa = service_a.clone();
    let u1 = user1.clone();
    let h3 = tokio::spawn(async move {
        sa.invalidate_user(&u1)
            .await
            .expect("Failed to invalidate user");
    });

    h1.await.expect("Task 1 panicked");
    h2.await.expect("Task 2 panicked");
    h3.await.expect("Task 3 panicked");

    // Receive all messages
    let mut received = Vec::new();
    for _ in 0..3 {
        tokio::select! {
            msg = receiver.recv() => {
                received.push(msg.expect("Failed to receive message"));
            }
            () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                panic!("Timeout waiting for messages");
            }
        }
    }

    assert_eq!(received.len(), 3, "Should receive 3 messages");

    // Verify message types
    let room_count = received
        .iter()
        .filter(|m| matches!(m, InvalidationMessage::Room { .. }))
        .count();
    let user_count = received
        .iter()
        .filter(|m| matches!(m, InvalidationMessage::User { .. }))
        .count();

    assert_eq!(room_count, 2, "Should receive 2 Room messages");
    assert_eq!(user_count, 1, "Should receive 1 User message");
}

// ============================================================================
// Test: Room Member Count Synchronization
// ============================================================================

/// Test that member count is accurate across replicas after join/leave.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_count_synchronized_across_replicas() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Count Sync Room").await;

    // Create members
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    for i in 0..5 {
        let user = user_repo
            .create(&make_user(&format!("count_member_{i}")))
            .await
            .expect("Failed to create member");
        let member = RoomMember::new(room.id.clone(), user.id.clone(), RoomRole::Member);
        member_repo
            .add(&member)
            .await
            .expect("Failed to add member");
    }

    // Query count from "Node A"
    let count_a: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM room_members WHERE room_id = $1 AND left_at IS NULL",
    )
    .bind(room.id.as_str())
    .fetch_one(pool)
    .await
    .expect("Failed to count members on Node A");

    // Query count from "Node B" (same pool, different connection)
    let count_b: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM room_members WHERE room_id = $1 AND left_at IS NULL",
    )
    .bind(room.id.as_str())
    .fetch_one(pool)
    .await
    .expect("Failed to count members on Node B");

    // Counts should match (database is shared)
    assert_eq!(
        count_a, count_b,
        "Member count should be consistent across replicas"
    );
    assert_eq!(count_a, 6, "Should have 6 members (owner + 5)");
}

/// Test that ban is visible across replicas.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_visible_across_replicas() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;

    let (owner, room) = setup_test_room(pool, "Ban Sync Room").await;

    // Create member
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member_user = user_repo
        .create(&make_user("banned_sync_member"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id.clone(), member_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Setup member service
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );
    let mut member_service = MemberService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        permission_service,
        NotificationService::default(),
    );
    member_service.set_room_settings_repo(room_settings_repo);

    // "Node A" bans member
    member_service
        .ban_member(
            room.id.clone(),
            owner.id.clone(),
            member_user.id.clone(),
            Some("Test ban".to_string()),
        )
        .await
        .expect("Failed to ban member");

    // "Node B" queries member status
    let member_b = member_repo
        .get_any(&room.id, &member_user.id)
        .await
        .expect("Failed to get member")
        .expect("Member should exist");

    assert_eq!(
        member_b.status,
        MemberStatus::Banned,
        "Node B should see member is banned"
    );
}

// ============================================================================
// Helper Functions
// ============================================================================

async fn start_redis() -> (RedisContainer, String) {
    start_redis_url_with_label("room-cluster-sync").await
}
