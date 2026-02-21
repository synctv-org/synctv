//! MemberService integration tests
//!
//! Tests member management including max members, kick hierarchy, ban/unban,
//! and permission operations with real PostgreSQL via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test member_service_tests -- --nocapture

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, UsernameCache, NoopCacheL2},
    config::PasswordComplexityConfig,
    models::{
        UserId, User, UserRole, UserStatus,
        RoomRole, PermissionBits, MemberStatus,
        room_settings::MaxMembers,
    },
    repository::{UserRepository, RoomMemberRepository},
    service::{
        RoomService, UserService, InMemoryTokenBlacklistStore,
        auth::{JwtService, BruteForceProtection},
    },
    Error,
};
use chrono::Utc;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres.get_host_port_ipv4(5432).await.expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

fn make_user_service(pool: PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
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

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(pool.clone());
    RoomService::new(pool, user_service)
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{}@test.com", username)),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: None,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

// ========== Max Members Test ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_member_respects_max_members() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("max_owner")).await.unwrap();

    // Create room with max_members = 2 (creator counts as 1)
    let mut settings = synctv_core::models::RoomSettings::default();
    settings.max_members = MaxMembers(2);

    let (room, _) = room_service
        .create_room(
            "Max Members Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    // First joiner should succeed (member count: 2)
    let joiner1 = user_repo.create(&make_user("max_joiner1")).await.unwrap();
    let result = room_service
        .join_room(room.id.clone(), joiner1.id.clone(), None)
        .await;
    assert!(result.is_ok(), "First joiner should succeed");

    // Second joiner should fail (member count would be 3, exceeding max 2)
    let joiner2 = user_repo.create(&make_user("max_joiner2")).await.unwrap();
    let result = room_service
        .join_room(room.id.clone(), joiner2.id.clone(), None)
        .await;
    assert!(result.is_err(), "Second joiner should be rejected");
}

// ========== Kick Member Role Hierarchy Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_member_role_hierarchy() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("kick_creator")).await.unwrap();
    let admin = user_repo.create(&make_user("kick_admin")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Kick Hierarchy Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Add admin as member first, then promote to admin
    room_service.join_room(room.id.clone(), admin.id.clone(), None).await.unwrap();

    // Promote to admin role
    let member_service = room_service.member_service();
    member_service.set_member_role(
        room.id.clone(),
        creator.id.clone(),
        admin.id.clone(),
        RoomRole::Admin,
    ).await.unwrap();

    // Admin trying to kick Creator should fail
    let result = member_service.kick_member(
        room.id.clone(),
        admin.id.clone(),
        creator.id.clone(),
    ).await;

    assert!(result.is_err(), "Admin cannot kick Creator");
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(msg.contains("cannot kick") || msg.contains("equal or higher"), "Error should mention role hierarchy: {}", msg);
        }
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_member_creator_can_kick_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo.create(&make_user("kick_c_creator")).await.unwrap();
    let admin = user_repo.create(&make_user("kick_c_admin")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Kick Creator Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), admin.id.clone(), None).await.unwrap();

    // Promote to admin
    let member_service = room_service.member_service();
    member_service.set_member_role(
        room.id.clone(),
        creator.id.clone(),
        admin.id.clone(),
        RoomRole::Admin,
    ).await.unwrap();

    // Creator should be able to kick admin
    let result = member_service.kick_member(
        room.id.clone(),
        creator.id.clone(),
        admin.id.clone(),
    ).await;

    assert!(result.is_ok(), "Creator should be able to kick admin");

    // Admin should no longer be a member
    assert!(!member_repo.is_member(&room.id, &admin.id).await.unwrap());
}

// ========== Ban / Unban Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_sets_status_and_banned_at() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo.create(&make_user("ban_creator")).await.unwrap();
    let target = user_repo.create(&make_user("ban_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Ban Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), target.id.clone(), None).await.unwrap();

    // Ban the member
    let member_service = room_service.member_service();
    member_service.ban_member(
        room.id.clone(),
        creator.id.clone(),
        target.id.clone(),
        Some("Test ban reason".to_string()),
    ).await.unwrap();

    // Verify ban status (use get_any because banned members have left_at set)
    let member = member_repo.get_any(&room.id, &target.id).await.unwrap().unwrap();
    assert_eq!(member.status, MemberStatus::Banned, "Member should be banned");
    assert!(member.banned_at.is_some(), "banned_at should be set");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unban_clears_status() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo.create(&make_user("unban_creator")).await.unwrap();
    let target = user_repo.create(&make_user("unban_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Unban Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), target.id.clone(), None).await.unwrap();

    let member_service = room_service.member_service();

    // Ban first
    member_service.ban_member(
        room.id.clone(),
        creator.id.clone(),
        target.id.clone(),
        None,
    ).await.unwrap();

    // Verify banned (use get_any because banned members have left_at set)
    let member = member_repo.get_any(&room.id, &target.id).await.unwrap().unwrap();
    assert_eq!(member.status, MemberStatus::Banned);

    // Unban
    member_service.unban_member(
        room.id.clone(),
        creator.id.clone(),
        target.id.clone(),
    ).await.unwrap();

    // Verify unbanned (get works now since unban clears left_at)
    let member = member_repo.get(&room.id, &target.id).await.unwrap().unwrap();
    assert_ne!(member.status, MemberStatus::Banned, "Member should no longer be banned");
    assert!(member.banned_at.is_none(), "banned_at should be cleared after unban");
}

// ========== Permission Grant/Revoke Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_grant_permission_bitwise_or() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("grant_creator")).await.unwrap();
    let target = user_repo.create(&make_user("grant_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Grant Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), target.id.clone(), None).await.unwrap();

    let member_service = room_service.member_service();

    // Grant BAN_MEMBER permission
    let updated = member_service.grant_permission(
        room.id.clone(),
        creator.id.clone(),
        target.id.clone(),
        PermissionBits::BAN_MEMBER,
    ).await.unwrap();

    assert!(updated.added_permissions & PermissionBits::BAN_MEMBER != 0,
        "BAN_MEMBER should be in added_permissions");

    // Grant another permission (KICK_USER) - should be bitwise OR'd
    let updated = member_service.grant_permission(
        room.id.clone(),
        creator.id.clone(),
        target.id.clone(),
        PermissionBits::KICK_USER,
    ).await.unwrap();

    assert!(updated.added_permissions & PermissionBits::BAN_MEMBER != 0,
        "BAN_MEMBER should still be set");
    assert!(updated.added_permissions & PermissionBits::KICK_USER != 0,
        "KICK_USER should now also be set");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_revoke_permission() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("revoke_creator")).await.unwrap();
    let target = user_repo.create(&make_user("revoke_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Revoke Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), target.id.clone(), None).await.unwrap();

    let member_service = room_service.member_service();

    // Revoke SEND_CHAT permission (which is in default member permissions)
    let updated = member_service.revoke_permission(
        room.id.clone(),
        creator.id.clone(),
        target.id.clone(),
        PermissionBits::SEND_CHAT,
    ).await.unwrap();

    assert!(updated.removed_permissions & PermissionBits::SEND_CHAT != 0,
        "SEND_CHAT should be in removed_permissions");

    // Verify the effective permission no longer includes SEND_CHAT
    let perm_service = room_service.permission_service();
    let effective = perm_service.get_user_permissions_no_cache(&room.id, &target.id).await.unwrap();
    assert!(!effective.has(PermissionBits::SEND_CHAT),
        "SEND_CHAT should be denied after revocation");
}
