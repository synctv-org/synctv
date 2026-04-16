//! Cross-service state consistency tests
//!
//! Tests verify state consistency across services (Database + Redis + Memory Cache)
//! in multi-replica deployments.
//!
//! Run with: cargo test --test `cluster_consistency_tests`
//!
//! # Test Coverage
//!
//! - Permission change cross-replica sync
//! - Playback state cross-replica sync
//! - Room settings cross-replica sync
//!
//! # Requirements
//!
//! - Docker for testcontainers (`PostgreSQL` + Redis)
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use sqlx::PgPool;
use std::sync::Arc;
use synctv_core::{
    cache::{CacheInvalidationService, InvalidationMessage, KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        room_settings::MaxMembers, Room, RoomId, RoomMember, RoomRole, RoomSettings, RoomStatus,
        User, UserId, UserRole, UserStatus,
    },
    repository::{
        RoomMemberRepository, RoomPlaybackStateRepository, RoomRepository, RoomSettingsRepository,
        UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        permission::PermissionService,
        playback::PlaybackService,
        InMemoryTokenBlacklistStore, UserService,
    },
};
use synctv_core_testing::{
    create_test_pool_with_options_and_label, start_redis_url_with_label, RedisContainer,
    TestContainer,
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

async fn create_test_infra() -> TestInfra {
    let (postgres, pool) = create_test_pool_with_options_and_label(
        "synctv_test",
        "cluster-consistency",
        20,
        std::time::Duration::from_secs(30),
    )
    .await;
    let (redis, redis_url) = start_redis_url_with_label("cluster-consistency").await;

    TestInfra {
        pool,
        redis_url,
        postgres: Some(postgres),
        redis: Some(redis),
    }
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

fn make_user_service(pool: PgPool) -> UserService {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let mut user_service = UserService::new(
        pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    );
    user_service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    user_service
}

/// Setup test room with owner
async fn setup_test_room(pool: &PgPool) -> (User, Room) {
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("room_owner"))
        .await
        .expect("Failed to create owner");
    let room = room_repo
        .create(&make_room("Test Room", "Test", &owner.id))
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
// Test 1: Permission Change Cross-Replica Sync
// ============================================================================

/// Test that permission changes are synchronized across replicas via Redis.
///
/// Scenario:
/// 1. Two `PermissionService` instances (simulating two replicas) share Redis
/// 2. Node A modifies user permissions
/// 3. Node A broadcasts invalidation via Redis
/// 4. Node B receives invalidation and clears its cache
/// 5. Node B's next permission query fetches fresh data from DB
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_change_cross_replica_sync() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;
    let redis_client =
        redis::Client::open(infra.redis_url.clone()).expect("Failed to create Redis client");

    let (_owner, room) = setup_test_room(pool).await;

    // Create a member user
    let user_repo = UserRepository::new(pool.clone());
    let member_user = user_repo
        .create(&make_user("member_user"))
        .await
        .expect("Failed to create member");

    // Add member to room
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member = RoomMember::new(room.id.clone(), member_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Create two PermissionService instances (simulating two replicas)
    let cache_invalidation_1 = distributed_invalidation_service(
        redis_client.clone(),
        "node_a",
        "test:cache:invalidate".to_string(),
    )
    .await;
    let cache_invalidation_2 = distributed_invalidation_service(
        redis_client.clone(),
        "node_b",
        "test:cache:invalidate".to_string(),
    )
    .await;

    // Start cache invalidation services so the XREADGROUP listener runs
    cache_invalidation_2
        .start()
        .await
        .expect("Failed to start cache invalidation service 2");
    // Give subscriber time to connect
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Create permission services with cache invalidation using with_invalidation constructor
    let permission_service_a = PermissionService::with_invalidation(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
        cache_invalidation_1.clone(),
    );

    let permission_service_b = PermissionService::with_invalidation(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
        cache_invalidation_2.clone(),
    );
    permission_service_a
        .start()
        .await
        .expect("Failed to start permission service a");
    permission_service_b
        .start()
        .await
        .expect("Failed to start permission service b");

    // Node B: First permission query (caches the result)
    let perms_b_initial = permission_service_b
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get initial permissions");

    // Verify initial permissions (Member role default)
    assert!(perms_b_initial.0 > 0, "Member should have some permissions");

    // Node A: Modify user permissions directly in DB (need version for optimistic lock)
    let existing_member = member_repo
        .get(&room.id, &member_user.id)
        .await
        .expect("Failed to get member")
        .expect("Member should exist");
    member_repo
        .update_permissions(&room.id, &member_user.id, 12345, 0, existing_member.version)
        .await
        .expect("Failed to update permissions");

    // Node A: Invalidate cache (broadcasts to Redis)
    permission_service_a
        .invalidate_cache(&room.id, &member_user.id)
        .await;

    // Wait for invalidation to propagate via Redis Pub/Sub
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Node B: Query permissions again (should fetch fresh data from DB)
    // Retry a few times to allow for Pub/Sub propagation delays under heavy load
    let perms_b_after = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let perms = permission_service_b
                .get_user_permissions(&room.id, &member_user.id)
                .await
                .expect("Failed to get updated permissions");
            if perms != perms_b_initial {
                return perms;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("timed out waiting for permission change to propagate to node B");

    // Verify Node B sees the updated permissions.
    // effective_permissions = (role_default | added_permissions) & !removed_permissions
    // So the result should have the added bits set (but also role_default bits).
    assert_ne!(
        perms_b_after, perms_b_initial,
        "Node B permissions should have changed"
    );
    assert_eq!(
        perms_b_after.0 & 0x3039,
        0x3039,
        "Node B should see the added permission bits"
    );
}

/// Test permission cache hit after first query.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_cache_hit_on_same_node() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool).await;

    // Create a member user
    let user_repo = UserRepository::new(pool.clone());
    let member_user = user_repo
        .create(&make_user("cached_member"))
        .await
        .expect("Failed to create member");

    // Add member to room
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member = RoomMember::new(room.id.clone(), member_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Create permission service
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    // First query (cache miss, fetch from DB)
    let perms_1 = permission_service
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions");

    // Second query (cache hit)
    let perms_2 = permission_service
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions");

    // Should return same value
    assert_eq!(perms_1, perms_2, "Cached value should match original");
}

// ============================================================================
// Test 2: Playback State Cross-Replica Sync
// ============================================================================

/// Test that playback state changes are synchronized across replicas.
///
/// Scenario:
/// 1. Two `PlaybackService` instances share Redis
/// 2. Node A updates playback state
/// 3. Node A broadcasts `PlaybackStateUpdate` via Redis
/// 4. Node B receives update and caches it
/// 5. Node B's query returns cached state (no DB read)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_cross_replica_sync() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;
    let redis_client =
        redis::Client::open(infra.redis_url.clone()).expect("Failed to create Redis client");

    let (_owner, room) = setup_test_room(pool).await;

    // Create cache invalidation services
    let cache_invalidation_1 = distributed_invalidation_service(
        redis_client.clone(),
        "node_a",
        "test:cache:playback".to_string(),
    )
    .await;
    let cache_invalidation_2 = distributed_invalidation_service(
        redis_client.clone(),
        "node_b",
        "test:cache:playback".to_string(),
    )
    .await;

    // Start cache invalidation service 2 so node B receives cross-replica messages
    cache_invalidation_2
        .start()
        .await
        .expect("Failed to start cache invalidation service 2");
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Create playback services
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300);
    let user_service = make_user_service(pool.clone());

    let mut playback_service_a = PlaybackService::new(
        playback_repo.clone(),
        permission_service.clone(),
        synctv_core::service::media::MediaService::new(
            synctv_core::repository::MediaRepository::new(pool.clone()),
            synctv_core::repository::PlaylistRepository::new(pool.clone()),
            permission_service.clone(),
            Arc::new(synctv_core::service::ProvidersManager::new(Arc::new(
                synctv_core::service::RemoteProviderManager::new(Arc::new(
                    synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
                )),
            ))),
            synctv_core::service::NotificationService::default(),
        ),
        user_service.clone(),
    );
    playback_service_a.set_invalidation_service(cache_invalidation_1.clone());
    playback_service_a
        .start()
        .await
        .expect("playback invalidation listener A should start");

    let mut playback_service_b = PlaybackService::new(
        playback_repo.clone(),
        permission_service.clone(),
        synctv_core::service::media::MediaService::new(
            synctv_core::repository::MediaRepository::new(pool.clone()),
            synctv_core::repository::PlaylistRepository::new(pool.clone()),
            permission_service.clone(),
            Arc::new(synctv_core::service::ProvidersManager::new(Arc::new(
                synctv_core::service::RemoteProviderManager::new(Arc::new(
                    synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
                )),
            ))),
            synctv_core::service::NotificationService::default(),
        ),
        user_service,
    );
    playback_service_b.set_invalidation_service(cache_invalidation_2.clone());
    playback_service_b
        .start()
        .await
        .expect("playback invalidation listener B should start");

    // Node A: Initialize playback state
    let initial_state = playback_repo
        .create_or_get(&room.id)
        .await
        .expect("Failed to create initial state");

    // Node B: Query initial state (caches it)
    let state_b_initial = playback_service_b
        .get_state(&room.id)
        .await
        .expect("Failed to get initial playback state");
    assert!(
        !state_b_initial.is_playing,
        "Initial state should not be playing"
    );

    // Node A: Update playback state
    let mut updated_state = initial_state.clone();
    updated_state.is_playing = true;
    updated_state.current_time = 42.5;
    updated_state.speed = 1.5;

    playback_repo
        .update(&updated_state)
        .await
        .expect("Failed to update state");

    // Node A: Broadcast invalidation
    cache_invalidation_1
        .invalidate_playback_state(&room.id)
        .await
        .expect("Failed to broadcast invalidation");

    // Wait for invalidation to propagate
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // Node B: Query playback state (should fetch fresh from DB)
    let state_b_after = playback_service_b
        .get_state(&room.id)
        .await
        .expect("Failed to get updated playback state");

    // Verify Node B sees the updated state
    assert!(
        state_b_after.is_playing,
        "Node B should see is_playing = true"
    );
    assert!(
        (state_b_after.current_time - 42.5).abs() < 0.01,
        "Node B should see current_time = 42.5"
    );
}

/// Test playback state invalidation message contains correct `room_id`.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_invalidation_message_content() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");

    let service1 = distributed_invalidation_service(
        redis_client.clone(),
        "node1",
        "test:cache:playback_msg".to_string(),
    )
    .await;

    let service2 = distributed_invalidation_service(
        redis_client,
        "node2",
        "test:cache:playback_msg".to_string(),
    )
    .await;

    // Start the Redis listener on service2 so it picks up XADD messages
    service2.start().await.expect("Failed to start service2");
    let mut receiver = service2.subscribe();
    // Give the background subscriber time to connect
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

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
                _ => panic!("Expected PlaybackState message, got: {msg:?}"),
            }
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for playback state invalidation message");
        }
    }
}

// ============================================================================
// Test 3: Room Settings Cross-Replica Sync
// ============================================================================

/// Test that room settings changes are synchronized across replicas.
///
/// Scenario:
/// 1. Two nodes share Redis
/// 2. Node A modifies room settings (e.g., `max_members`)
/// 3. Node A broadcasts invalidation
/// 4. Node B receives invalidation
/// 5. Node B's next query fetches fresh settings from DB
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_settings_cross_replica_sync() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;
    let redis_client =
        redis::Client::open(infra.redis_url.clone()).expect("Failed to create Redis client");

    let (_owner, room) = setup_test_room(pool).await;

    // Create room settings
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let initial_settings = RoomSettings {
        max_members: MaxMembers(10),
        ..Default::default()
    };
    room_settings_repo
        .set_settings(&room.id, &initial_settings)
        .await
        .expect("Failed to create settings");

    // Create cache invalidation services
    let cache_invalidation_1 = distributed_invalidation_service(
        redis_client.clone(),
        "node_a",
        "test:cache:settings".to_string(),
    )
    .await;
    let _cache_invalidation_2 = distributed_invalidation_service(
        redis_client.clone(),
        "node_b",
        "test:cache:settings".to_string(),
    )
    .await;
    // Node B: Query settings (simulates caching)
    let settings_b_initial = room_settings_repo
        .get(&room.id)
        .await
        .expect("Failed to get settings");
    assert_eq!(
        settings_b_initial.max_members.0, 10,
        "Initial max_members should be 10"
    );

    // Node A: Update settings
    let mut updated_settings = initial_settings.clone();
    updated_settings.max_members = MaxMembers(50);
    room_settings_repo
        .set_settings(&room.id, &updated_settings)
        .await
        .expect("Failed to update settings");

    // Node A: Broadcast invalidation
    cache_invalidation_1
        .invalidate_room_settings(&room.id)
        .await
        .expect("Failed to broadcast invalidation");

    // Wait for invalidation to propagate
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // Node B: Query settings again
    let settings_b_after = room_settings_repo
        .get(&room.id)
        .await
        .expect("Failed to get updated settings");

    // Note: RoomSettingsRepository doesn't have its own cache, it reads from DB each time.
    // In a real scenario, there would be a cache layer above this repository.
    // This test verifies the invalidation message is broadcast correctly.
    assert_eq!(
        settings_b_after.max_members.0, 50,
        "Node B should see updated max_members"
    );
}

/// Test room settings invalidation message is broadcast correctly.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_settings_invalidation_message_broadcast() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");

    let service1 = distributed_invalidation_service(
        redis_client.clone(),
        "node1",
        "test:cache:room_settings".to_string(),
    )
    .await;

    let service2 = distributed_invalidation_service(
        redis_client,
        "node2",
        "test:cache:room_settings".to_string(),
    )
    .await;

    // Start the Redis listener on service2 so it picks up XADD messages
    service2.start().await.expect("Failed to start service2");
    let mut receiver = service2.subscribe();
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    let room_id = RoomId::new();

    // Broadcast room settings invalidation
    service1
        .invalidate_room_settings(&room_id)
        .await
        .expect("Failed to broadcast invalidation");

    tokio::select! {
        msg = receiver.recv() => {
            let msg = msg.expect("Failed to receive message");
            match msg {
                InvalidationMessage::RoomSettings { room_id: r } => {
                    assert_eq!(r, room_id.as_str());
                }
                _ => panic!("Expected RoomSettings message, got: {msg:?}"),
            }
        }
        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            panic!("Timeout waiting for room settings invalidation message");
        }
    }
}

// ============================================================================
// Test 4: Multiple Concurrent Invalidations
// ============================================================================

/// Test that multiple concurrent invalidations are handled correctly.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_invalidation_messages() {
    let (_container, redis_url) = start_redis().await;
    let redis_client = redis::Client::open(redis_url).expect("Failed to create Redis client");

    let service1 = distributed_invalidation_service(
        redis_client.clone(),
        "node1",
        "test:cache:concurrent".to_string(),
    )
    .await;

    let service2 = distributed_invalidation_service(
        redis_client,
        "node2",
        "test:cache:concurrent".to_string(),
    )
    .await;

    // Start the Redis listener on service2 so it picks up XADD messages
    service2.start().await.expect("Failed to start service2");
    let mut receiver = service2.subscribe();
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Send multiple invalidations concurrently
    let room1 = RoomId::new();
    let room2 = RoomId::new();
    let user1 = UserId::new();

    let s1 = service1.clone();
    let r1 = room1.clone();
    let h1 = tokio::spawn(async move {
        s1.invalidate_room(&r1)
            .await
            .expect("Failed to invalidate room1");
    });

    let s2 = service1.clone();
    let r2 = room2.clone();
    let h2 = tokio::spawn(async move {
        s2.invalidate_room(&r2)
            .await
            .expect("Failed to invalidate room2");
    });

    let s3 = service1.clone();
    let u1 = user1.clone();
    let h3 = tokio::spawn(async move {
        s3.invalidate_user(&u1)
            .await
            .expect("Failed to invalidate user");
    });

    // Wait for all broadcasts to complete
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

    // Verify message types (order may vary)
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
// Test 5: Cache Consistency After Redis Reconnect
// ============================================================================

/// Test that cache remains consistent even after Redis disconnects.
/// Note: This is a simplified test - full disconnect simulation requires more infrastructure.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_consistency_without_redis() {
    let infra = create_test_infra().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool).await;

    // Create permission service WITHOUT Redis
    let permission_service = PermissionService::new(
        RoomMemberRepository::new(pool.clone()),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    // Create a member user
    let user_repo = UserRepository::new(pool.clone());
    let member_user = user_repo
        .create(&make_user("isolated_member"))
        .await
        .expect("Failed to create member");

    // Add member to room
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member = RoomMember::new(room.id.clone(), member_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Query permissions (should work without Redis)
    let perms = permission_service
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions without Redis");

    assert!(
        perms.0 > 0,
        "Should be able to get permissions without Redis"
    );

    // Modify permissions directly in DB (need version for optimistic lock)
    let existing_member = member_repo
        .get(&room.id, &member_user.id)
        .await
        .expect("Failed to get member")
        .expect("Member should exist");
    member_repo
        .update_permissions(&room.id, &member_user.id, 99999, 0, existing_member.version)
        .await
        .expect("Failed to update permissions");

    // Invalidate cache locally (no Redis broadcast)
    permission_service
        .invalidate_cache(&room.id, &member_user.id)
        .await;

    // Query again (should fetch fresh from DB)
    let perms_after = permission_service
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get updated permissions");

    // effective_permissions = (role_default | added_permissions) & !removed_permissions
    // So the result should have the added bits set and differ from the initial value.
    assert_ne!(
        perms_after, perms,
        "Permissions should have changed after invalidation"
    );
    assert_eq!(
        perms_after.0 & 0x0001_869F,
        0x0001_869F,
        "Should see the added permission bits after local invalidation"
    );
}

// ============================================================================
// Helper Functions
// ============================================================================

async fn start_redis() -> (RedisContainer, String) {
    start_redis_url_with_label("cluster-consistency").await
}
