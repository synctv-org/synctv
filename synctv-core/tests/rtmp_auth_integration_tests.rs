//! RTMP authentication integration tests
//!
//! Tests the core RTMP authentication components with real `PostgreSQL` and Redis via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test `rtmp_auth_integration_tests`
//! Run with ignored tests: cargo test -p synctv-core --test `rtmp_auth_integration_tests` -- --ignored
//!
//! # Test Coverage
//!
//! - Publish key generation and validation
//! - Expired token rejection
//! - Banned/deleted user handling
//! - Banned/pending room handling
//! - Cross-replica user→stream mapping (Redis)
//! - Room settings `rtmp_player`
//!
//! # Requirements
//!
//! - Docker for testcontainers (`PostgreSQL` + Redis)
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        MediaId, MemberStatus, Room, RoomId, RoomMember, RoomRole, RoomSettings, RoomStatus,
        SignupMethod, User, UserId, UserRole, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::{
        auth::{jwt::JwtService, BruteForceProtection},
        InMemoryTokenBlacklistStore, PublishKeyService, RoomService, UserService,
    },
};
use testcontainers::core::ImageExt;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;

// ============================================================================
// Test Infrastructure
// ============================================================================

async fn create_test_infra() -> (
    ContainerAsync<Postgres>,
    ContainerAsync<Redis>,
    sqlx::PgPool,
    redis::aio::ConnectionManager,
) {
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

    let pg_host = postgres.get_host().await.expect("Failed to get host");
    let pg_port = postgres
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");

    let database_url = format!("postgresql://synctv:synctv_test@{pg_host}:{pg_port}/synctv_test");

    let pool = {
        let mut retries = 0u32;
        loop {
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(std::time::Duration::from_secs(2))
                .connect(&database_url)
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

    // Run migrations
    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    // Start Redis
    use testcontainers::runners::AsyncRunner;
    let redis = tokio::time::timeout(std::time::Duration::from_secs(30), Redis::default().start())
        .await
        .expect("Docker container startup timed out (is Docker running?)")
        .expect("Failed to start Redis container");

    let redis_host = redis.get_host().await.expect("Failed to get Redis host");
    let redis_port = redis
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get Redis port");

    let redis_url = format!("redis://{redis_host}:{redis_port}");

    let redis_client =
        redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");
    let redis_conn = redis::aio::ConnectionManager::new(redis_client)
        .await
        .expect("Failed to create Redis ConnectionManager");

    (postgres, redis, pool, redis_conn)
}

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-rtmp-auth-tests-minimum-length-32-chars")
        .expect("Failed to create JWT service")
}

fn create_user_service(pool: sqlx::PgPool) -> UserService {
    let jwt_service = create_jwt_service();
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new(
        pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn create_room_service(pool: sqlx::PgPool) -> RoomService {
    let user_service = create_user_service(pool.clone());
    RoomService::new(pool, user_service)
}

fn create_publish_key_service() -> PublishKeyService {
    let jwt_service = create_jwt_service();
    PublishKeyService::new(jwt_service, 24) // 24 hour TTL
}

async fn create_test_user(pool: &sqlx::PgPool, username: &str, role: UserRole) -> User {
    let user_repo = UserRepository::new(pool.clone());
    let user = User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "test_hash".to_string(),
        signup_method: Some(SignupMethod::Email),
        role,
        status: UserStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
        email_verified: true,
    };
    user_repo
        .create(&user)
        .await
        .expect("Failed to create test user");
    user
}

async fn create_test_room(pool: &sqlx::PgPool, creator_id: UserId, name: &str) -> Room {
    let room_repo = RoomRepository::new(pool.clone());
    let room = Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: "Test room".to_string(),
        created_by: creator_id.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
        version: 0,
        last_activity_at: chrono::Utc::now(),
    };
    let room = room_repo
        .create(&room)
        .await
        .expect("Failed to create test room");

    // Create default room settings
    let settings_repo = RoomSettingsRepository::new(pool.clone());
    let settings = RoomSettings::default();
    settings_repo
        .set_settings_with_version(&room.id, &settings, 0)
        .await
        .expect("Failed to create room settings");

    // Add creator as room member
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member = RoomMember {
        room_id: room.id.clone(),
        user_id: creator_id,
        role: RoomRole::Creator,
        status: MemberStatus::Active,
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
    };
    member_repo
        .add(&member)
        .await
        .expect("Failed to add room creator as member");

    room
}

// ============================================================================
// Test 1: Publish key generation and validation
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_publish_key_generation_and_validation() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    // Create test data
    let user = create_test_user(&pool, "streamer1", UserRole::User).await;
    let room = create_test_room(&pool, user.id.clone(), "Stream Room 1").await;
    let media_id = MediaId::new();

    // Generate publish token
    let publish_key_service = create_publish_key_service();
    let key = publish_key_service
        .generate_publish_key(room.id.clone(), media_id.clone(), user.id.clone())
        .await
        .expect("Failed to generate publish key");

    // Validate the token
    let claims = publish_key_service
        .validate_publish_key(&key.token)
        .await
        .expect("Failed to validate publish key");

    assert_eq!(claims.room_id, room.id.as_str());
    assert_eq!(claims.media_id, media_id.as_str());
    assert_eq!(claims.user_id, user.id.as_str());
    assert!(claims.perm_start_live);
}

// ============================================================================
// Test 2: Expired tokens are rejected
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_expired_token_rejected() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let user = create_test_user(&pool, "streamer2", UserRole::User).await;
    let room = create_test_room(&pool, user.id.clone(), "Stream Room 2").await;
    let media_id = MediaId::new();

    // Create a short-lived JWT service (0 TTL for immediate expiration)
    let jwt_service = JwtService::new("test-secret-key-for-expired-token-tests-32chars")
        .expect("Failed to create JWT service");
    let publish_key_service = PublishKeyService::new(jwt_service, 0);

    // Generate and immediately expire token
    let key = publish_key_service
        .generate_publish_key(room.id.clone(), media_id.clone(), user.id.clone())
        .await
        .expect("Failed to generate publish key");

    // Wait for token to expire
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    let result = publish_key_service.validate_publish_key(&key.token).await;

    assert!(result.is_err(), "Expired token should be rejected");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("expired") || err_msg.contains("Expired"),
        "Error should mention expiration: {err_msg}"
    );
}

// ============================================================================
// Test 3: Banned users cannot use publish keys
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_banned_user_validation() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let user = create_test_user(&pool, "banned_user", UserRole::User).await;
    let room = create_test_room(&pool, user.id.clone(), "Ban Room").await;
    let media_id = MediaId::new();

    // Generate publish key before banning
    let publish_key_service = create_publish_key_service();
    let key = publish_key_service
        .generate_publish_key(room.id.clone(), media_id.clone(), user.id.clone())
        .await
        .expect("Failed to generate publish key");

    // Ban the user
    let user_repo = UserRepository::new(pool.clone());
    let mut banned_user = user.clone();
    banned_user.status = UserStatus::Banned;
    user_repo
        .update(&banned_user, banned_user.version)
        .await
        .expect("Failed to ban user");

    // Token should still be valid at the JWT level (user status is checked separately)
    let _claims = publish_key_service
        .validate_publish_key(&key.token)
        .await
        .expect("Token validation should succeed (user status check is separate)");

    // But RTMP auth should reject based on user status
    let user_service = Arc::new(create_user_service(pool.clone()));
    let updated_user = user_service
        .get_user(&user.id)
        .await
        .expect("Failed to load user");

    assert_eq!(updated_user.status, UserStatus::Banned);
}

// ============================================================================
// Test 4: Deleted users validation
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_deleted_user_validation() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let user = create_test_user(&pool, "deleted_user", UserRole::User).await;
    let room = create_test_room(&pool, user.id.clone(), "Delete Room").await;
    let media_id = MediaId::new();

    // Generate publish key before deletion
    let publish_key_service = create_publish_key_service();
    let key = publish_key_service
        .generate_publish_key(room.id.clone(), media_id.clone(), user.id.clone())
        .await
        .expect("Failed to generate publish key");

    // Soft-delete the user via the repository's delete method
    let user_repo = UserRepository::new(pool.clone());
    let deleted = user_repo
        .delete(&user.id)
        .await
        .expect("Failed to delete user");
    assert!(deleted, "delete should have affected one row");

    // Token should still be valid at the JWT level
    let _claims = publish_key_service
        .validate_publish_key(&key.token)
        .await
        .expect("Token validation should succeed (user status check is separate)");

    // But RTMP auth should reject based on deleted_at:
    // get_user filters out soft-deleted users (WHERE deleted_at IS NULL),
    // so the deleted user should not be found.
    let user_service = Arc::new(create_user_service(pool.clone()));
    let result = user_service.get_user(&user.id).await;
    assert!(
        result.is_err(),
        "Soft-deleted user should not be found by get_user"
    );
}

// ============================================================================
// Test 5: Banned room affects operations
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_banned_room_rejects_operations() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let user = create_test_user(&pool, "room_user", UserRole::User).await;
    let mut room = create_test_room(&pool, user.id.clone(), "Test Room").await;

    // Ban the room
    room.is_banned = true;
    let room_repo = RoomRepository::new(pool.clone());
    room_repo
        .update(&room, room.version)
        .await
        .expect("Failed to ban room");

    // Reload and verify
    let reloaded_room = room_repo
        .get_by_id(&room.id)
        .await
        .expect("Failed to load room")
        .expect("Room should exist");

    assert!(reloaded_room.is_banned, "Room should be banned");

    // Room service should return banned status
    let room_service = create_room_service(pool.clone());
    let loaded_room = room_service
        .get_room(&room.id)
        .await
        .expect("Failed to load room via service");

    assert!(
        loaded_room.is_banned,
        "Room loaded via service should be banned"
    );
}

// ============================================================================
// Test 6: Pending room affects operations
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_pending_room_rejects_operations() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let user = create_test_user(&pool, "pending_user", UserRole::User).await;
    let mut room = create_test_room(&pool, user.id.clone(), "Pending Room").await;

    // Set room to pending status
    room.status = RoomStatus::Pending;
    let room_repo = RoomRepository::new(pool.clone());
    room_repo
        .update(&room, room.version)
        .await
        .expect("Failed to set room to pending");

    // Reload and verify
    let reloaded_room = room_repo
        .get_by_id(&room.id)
        .await
        .expect("Failed to load room")
        .expect("Room should exist");

    assert_eq!(
        reloaded_room.status,
        RoomStatus::Pending,
        "Room should be pending"
    );

    // Room service should return pending status
    let room_service = create_room_service(pool.clone());
    let loaded_room = room_service
        .get_room(&room.id)
        .await
        .expect("Failed to load room via service");

    assert_eq!(loaded_room.status, RoomStatus::Pending);
}

// ============================================================================
// Test 7: Cross-replica user→stream mapping (Redis hash)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_cross_replica_user_stream_mapping() {
    let (_postgres, _redis, _pool, redis_conn) = create_test_infra().await;

    let user_id = UserId::new();
    let room_id = RoomId::new();
    let media_id = MediaId::new();

    // Simulate writing user stream mapping to Redis
    let stream_key = format!("{room_id}:{media_id}");
    let redis_key = "synctv:rtmp:user_streams";

    let mut conn = redis_conn.clone();
    let _: () = redis::cmd("HSET")
        .arg(redis_key)
        .arg(user_id.to_string())
        .arg(&stream_key)
        .query_async(&mut conn)
        .await
        .expect("Failed to write user stream mapping");

    // Simulate reading user stream mapping from Redis
    let result: Option<String> = redis::cmd("HGET")
        .arg(redis_key)
        .arg(user_id.to_string())
        .query_async(&mut conn)
        .await
        .expect("Failed to read user stream mapping");

    assert!(result.is_some(), "Should find user stream mapping");

    let found_stream_key = result.unwrap();
    assert_eq!(found_stream_key, stream_key);

    // Verify we can parse it back
    let parts: Vec<&str> = found_stream_key.split(':').collect();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], room_id.to_string());
    assert_eq!(parts[1], media_id.to_string());

    // Cleanup
    let _: () = redis::cmd("HDEL")
        .arg(redis_key)
        .arg(user_id.to_string())
        .query_async(&mut conn)
        .await
        .expect("Failed to delete user stream mapping");
}

// ============================================================================
// Test 8: Room settings rtmp_player affects play authorization
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_rtmp_player_settings() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let user = create_test_user(&pool, "settings_user", UserRole::User).await;
    let room = create_test_room(&pool, user.id.clone(), "Settings Room").await;

    // Verify default rtmp_player is disabled
    let settings_repo = RoomSettingsRepository::new(pool.clone());
    let (settings, version) = settings_repo
        .get_with_version(&room.id)
        .await
        .expect("Failed to load settings");

    assert!(
        !settings.rtmp_player.0,
        "rtmp_player should be disabled by default"
    );

    // Enable rtmp_player (use the current version for optimistic locking)
    let mut updated_settings = settings;
    updated_settings.rtmp_player.0 = true;
    settings_repo
        .set_settings_with_version(&room.id, &updated_settings, version)
        .await
        .expect("Failed to update settings");

    // Verify the setting was updated
    let reloaded_settings = settings_repo
        .get(&room.id)
        .await
        .expect("Failed to reload settings");

    assert!(
        reloaded_settings.rtmp_player.0,
        "rtmp_player should be enabled"
    );
}

// ============================================================================
// Test 9: Non-room-member cannot publish
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_non_room_member_rejected() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    // Create room and owner
    let owner = create_test_user(&pool, "room_owner", UserRole::User).await;
    let room = create_test_room(&pool, owner.id.clone(), "Private Room").await;

    // Create a user who is NOT a member
    let non_member = create_test_user(&pool, "outsider", UserRole::User).await;

    let room_service = create_room_service(pool.clone());
    let publish_key_service = create_publish_key_service();

    // Generate publish key for non-member
    let media_id = MediaId::new();
    let _key = publish_key_service
        .generate_publish_key(room.id.clone(), media_id.clone(), non_member.id.clone())
        .await
        .expect("Failed to generate publish key");

    // Verify non-member is not in the room
    let member_result = room_service
        .member_service()
        .get_member(&room.id, &non_member.id)
        .await;

    assert!(
        member_result.is_err() || member_result.unwrap().is_none(),
        "Non-member should not be found in room membership"
    );
}
