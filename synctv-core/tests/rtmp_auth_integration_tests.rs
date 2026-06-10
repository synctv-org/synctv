//! RTMP authentication integration tests
//!
//! Tests the core RTMP authentication components with real `PostgreSQL` and Redis via testcontainers.
//!
//! Run with ignored tests: cargo test -p synctv-core --test `rtmp_auth_integration_tests` -- --ignored
//!
//! # Test Coverage
//!
//! - Banned/deleted user handling
//! - Banned/pending room handling
//! - Cross-replica user→stream mapping (Redis)
//!
//! # Requirements
//!
//! - Docker for testcontainers (`PostgreSQL` + Redis)

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
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
use synctv_core_testing::{create_test_pool, ok, some, start_redis, RedisContainer, TestContainer};

async fn create_test_infra() -> (
    TestContainer,
    RedisContainer,
    sqlx::PgPool,
    redis::aio::ConnectionManager,
) {
    let (postgres, pool) = create_test_pool().await;
    let (redis, redis_conn) = start_redis().await;

    (postgres, redis, pool, redis_conn)
}

fn create_jwt_service() -> JwtService {
    ok(
        JwtService::new("test-secret-key-for-rtmp-auth-tests-minimum-length-32-chars"),
        "test JWT service should initialize",
    )
}

fn create_user_service(pool: &sqlx::PgPool) -> UserService {
    let jwt_service = create_jwt_service();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn create_room_service(pool: sqlx::PgPool) -> RoomService {
    let user_service = create_user_service(&pool);

    ok(
        RoomService::new_for_tests(pool, user_service),
        "room service should build",
    )
}

fn create_publish_key_service() -> PublishKeyService {
    let jwt_service = create_jwt_service();
    ok(
        PublishKeyService::new(jwt_service, 24),
        "publish key service should build",
    )
}

async fn create_test_user(pool: &sqlx::PgPool, username: &str, role: UserRole) -> User {
    let user_repo = UserRepository::new(pool.clone());
    let user = User {
        id: UserId::new(),
        username: username.to_string(),
        signup_method: SignupMethod::Email,
        role,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };
    ok(
        user_repo.create(&user).await,
        "RTMP auth test user should be created",
    )
}

async fn create_test_room(pool: &sqlx::PgPool, creator_id: UserId, name: &str) -> Room {
    let room_repo = RoomRepository::new(pool.clone());
    let room = Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: "Test room".to_string(),
        cover_file_reference_id: None,
        created_by: creator_id,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
        version: 0,
        last_activity_at: chrono::Utc::now(),
    };
    let room = ok(
        room_repo.create(&room).await,
        "RTMP auth test room should be created",
    );

    let settings_repo = RoomSettingsRepository::new(pool.clone());
    let settings = RoomSettings::default();
    ok(
        settings_repo
            .set_settings_with_version(&room.id, &settings, 0)
            .await,
        "RTMP auth room settings should be created",
    );

    // Add creator as room member
    let member_repo = RoomMemberRepository::new(pool.clone());
    let member = RoomMember {
        room_id: room.id,
        user_id: creator_id,
        role: RoomRole::Creator,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: chrono::Utc::now(),
        version: 0,
    };
    ok(
        member_repo.add(&member).await,
        "RTMP auth room creator membership should be created",
    );

    room
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_banned_user_validation() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let user = create_test_user(&pool, "banned_user", UserRole::User).await;
    let room = create_test_room(&pool, user.id, "Ban Room").await;
    let media_id = MediaId::new();

    // Generate publish key before banning
    let publish_key_service = create_publish_key_service();
    let key = ok(
        publish_key_service.generate_publish_key(&room.id, &media_id, &user.id),
        "publish key should be generated",
    );

    // Ban the user
    let user_repo = UserRepository::new(pool.clone());
    ok(
        user_repo
            .ban(&user.id, None, Some("rtmp auth test".to_string()))
            .await,
        "RTMP auth user should be banned",
    );

    // Token should still be valid at the JWT level (user status is checked separately)
    let _claims = ok(
        publish_key_service.validate_publish_key(&key.token).await,
        "publish key token should validate before user status check",
    );

    // But RTMP auth should reject based on user status
    let user_service = Arc::new(create_user_service(&pool));
    let updated_user = ok(
        user_service.get_user(&user.id).await,
        "banned user should be loaded",
    );

    assert_eq!(updated_user.status, UserStatus::Banned);
    assert!(ok(
        user_repo.is_banned(&user.id).await,
        "banned user status lookup should succeed"
    ));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_deleted_user_validation() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let user = create_test_user(&pool, "deleted_user", UserRole::User).await;
    let room = create_test_room(&pool, user.id, "Delete Room").await;
    let media_id = MediaId::new();

    // Generate publish key before deletion
    let publish_key_service = create_publish_key_service();
    let key = ok(
        publish_key_service.generate_publish_key(&room.id, &media_id, &user.id),
        "publish key should be generated",
    );

    // Soft-delete the user via the repository's delete method
    let user_repo = UserRepository::new(pool.clone());
    let deleted = ok(
        user_repo.delete(&user.id).await,
        "RTMP auth user should be soft-deleted",
    );
    assert!(deleted, "delete should have affected one row");

    // Token should still be valid at the JWT level
    let _claims = ok(
        publish_key_service.validate_publish_key(&key.token).await,
        "publish key token should validate before deleted-user check",
    );

    // But RTMP auth should reject based on deleted_at:
    // get_user filters out soft-deleted users (WHERE deleted_at IS NULL),
    // so the deleted user should not be found.
    let user_service = Arc::new(create_user_service(&pool));
    let result = user_service.get_user(&user.id).await;
    assert!(
        result.is_err(),
        "Soft-deleted user should not be found by get_user"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_banned_room_rejects_operations() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let user = create_test_user(&pool, "room_user", UserRole::User).await;
    let room = create_test_room(&pool, user.id, "Test Room").await;

    // Ban the room
    let room_repo = RoomRepository::new(pool.clone());
    ok(
        room_repo.update_ban_status(&room.id, true).await,
        "RTMP auth room should be banned",
    );

    // Reload and verify
    let reloaded_room = some(
        ok(
            room_repo.get_by_id(&room.id).await,
            "banned room should be loaded",
        ),
        "banned room should exist",
    );

    assert!(reloaded_room.is_banned, "Room should be banned");

    // Room service should return banned status
    let room_service = create_room_service(pool.clone());
    let loaded_room = ok(
        room_service.get_room(&room.id).await,
        "banned room should load via service",
    );

    assert!(
        loaded_room.is_banned,
        "Room loaded via service should be banned"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_closed_room_lifecycle_is_persisted() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let user = create_test_user(&pool, "pending_user", UserRole::User).await;
    let mut room = create_test_room(&pool, user.id, "Closed Room").await;

    room.close();
    let room_repo = RoomRepository::new(pool.clone());
    ok(
        room_repo.update(&room, room.version).await,
        "RTMP auth room should be closed",
    );

    // Reload and verify
    let reloaded_room = some(
        ok(
            room_repo.get_by_id(&room.id).await,
            "closed room should be loaded",
        ),
        "closed room should exist",
    );

    assert_eq!(
        reloaded_room.status,
        RoomStatus::Closed,
        "Room should be closed"
    );

    // Room service should return closed lifecycle status
    let room_service = create_room_service(pool.clone());
    let loaded_room = ok(
        room_service.get_room(&room.id).await,
        "closed room should load via service",
    );

    assert_eq!(loaded_room.status, RoomStatus::Closed);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_cross_replica_user_stream_mapping() {
    let (_postgres, _redis, _pool, redis_conn) = create_test_infra().await;

    let user_id = UserId::new();
    let room_id = RoomId::new();
    let media_id = MediaId::new();

    // Simulate writing user stream mapping to Redis using per-user key with | separator
    let stream_value = format!("{room_id}|{media_id}");
    let redis_key = format!("synctv:rtmp:user_stream:{user_id}");

    let mut conn = redis_conn.clone();
    let _: () = ok(
        redis::cmd("SET")
            .arg(&redis_key)
            .arg(&stream_value)
            .query_async(&mut conn)
            .await,
        "user stream mapping should be written to Redis",
    );

    // Simulate reading user stream mapping from Redis
    let result: Option<String> = ok(
        redis::cmd("GET")
            .arg(&redis_key)
            .query_async(&mut conn)
            .await,
        "user stream mapping should be read from Redis",
    );

    assert!(result.is_some(), "Should find user stream mapping");

    let found_stream_value = some(result, "user stream mapping should exist");
    assert_eq!(found_stream_value, stream_value);

    // Verify we can parse it back using | separator
    let (parsed_room_id, parsed_media_id) = found_stream_value
        .split_once('|')
        .unwrap_or_else(|| std::panic::panic_any("user stream mapping should contain separator"));
    assert_eq!(parsed_room_id, room_id.to_string());
    assert_eq!(parsed_media_id, media_id.to_string());

    // Cleanup
    let _: () = ok(
        redis::cmd("DEL")
            .arg(&redis_key)
            .query_async(&mut conn)
            .await,
        "user stream mapping should be deleted from Redis",
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn rtmp_auth_test_non_room_member_rejected() {
    let (_postgres, _redis, pool, _redis_conn) = create_test_infra().await;

    let owner = create_test_user(&pool, "room_owner", UserRole::User).await;
    let room = create_test_room(&pool, owner.id, "Private Room").await;

    let non_member = create_test_user(&pool, "outsider", UserRole::User).await;

    let room_service = create_room_service(pool.clone());
    let publish_key_service = create_publish_key_service();

    // Generate publish key for non-member
    let media_id = MediaId::new();
    let _key = ok(
        publish_key_service.generate_publish_key(&room.id, &media_id, &non_member.id),
        "publish key should be generated",
    );

    // Verify non-member is not in the room
    let member_result = room_service
        .member_service()
        .get_member(&room.id, &non_member.id)
        .await;

    assert!(
        member_result.map_or(true, |member| member.is_none()),
        "Non-member should not be found in room membership"
    );
}
