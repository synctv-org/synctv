//! Permission check integration tests with database
//!
//! Tests verify Allow/Deny permission pattern and integration with database.
//!
//! Run with: cargo test --test permission_integration_tests
//! Requires Docker for testcontainers.

use synctv_core::{
    models::{Room, RoomId, RoomMember, RoomRole, UserId, MemberStatus, PermissionBits, User, UserStatus, SignupMethod},
    repository::{RoomRepository, RoomMemberRepository, UserRepository},
    service::permission::PermissionService,
};
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

/// Default PostgreSQL version for test containers
const POSTGRES_VERSION: &str = "16-alpine";

async fn create_test_pool() -> (testcontainers::ContainerAsync<Postgres>, PgPool) {
    let container = Postgres::default()
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start postgres");

    let port = container.get_host_port_ipv4(5432).await.expect("Failed to get port");
    let connection_string = format!(
        "postgresql://postgres:postgres@127.0.0.1:{}/postgres",
        port,
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (container, pool)
}

/// Create a test user in the database (required for FK constraints)
async fn create_test_user(pool: &PgPool, user_id: &UserId) {
    let username = format!("test_user_{}", user_id.as_str());
    let user = User {
        id: user_id.clone(),
        username,
        email: Some(format!("{}@test.com", user_id.as_str())),
        password_hash: "test_hash".to_string(),
        signup_method: Some(SignupMethod::Email),
        role: synctv_core::models::UserRole::User,
        status: UserStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
        email_verified: true,
    };
    let user_repo = UserRepository::new(pool.clone());
    user_repo.create(&user).await.expect("Failed to create test user");
}

fn make_member(room_id: RoomId, user_id: UserId, role: RoomRole, status: MemberStatus) -> RoomMember {
    RoomMember {
        room_id,
        user_id,
        role,
        status,
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
    }
}

fn make_room(creator_id: UserId) -> Room {
    Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id,
        status: synctv_core::models::RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    }
}

fn make_perm_service(member_repo: RoomMemberRepository, room_repo: RoomRepository) -> PermissionService {
    PermissionService::new(
        member_repo,
        room_repo,
        None,
        1000,
        300,
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_check_with_database_member() {
    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = UserId::new();
    create_test_user(&pool, &creator_id).await;
    let room = make_room(creator_id);
    room_repo.create(&room).await.expect("Failed to create room");

    let user_id = UserId::new();
    create_test_user(&pool, &user_id).await;
    let mut member = make_member(room.id.clone(), user_id.clone(), RoomRole::Member, MemberStatus::Active);
    member.added_permissions = PermissionBits::SEND_CHAT | PermissionBits::ADD_MEDIA;
    member_repo.add(&member).await.expect("Failed to create member");

    let perm_service = make_perm_service(member_repo, room_repo);

    // check_permission_no_cache returns Ok(()) if granted, Err if denied
    perm_service.check_permission_no_cache(&room.id, &user_id, PermissionBits::SEND_CHAT)
        .await
        .expect("User should have SEND_CHAT permission");

    perm_service.check_permission_no_cache(&room.id, &user_id, PermissionBits::ADD_MEDIA)
        .await
        .expect("User should have ADD_MEDIA permission");

    let kick_result = perm_service.check_permission_no_cache(&room.id, &user_id, PermissionBits::KICK_MEMBER)
        .await;
    assert!(kick_result.is_err(), "User should not have KICK_MEMBER permission");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_allow_deny_pattern() {
    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = UserId::new();
    create_test_user(&pool, &creator_id).await;
    let room = make_room(creator_id);
    room_repo.create(&room).await.expect("Failed to create room");

    let admin_id = UserId::new();
    create_test_user(&pool, &admin_id).await;
    let admin_member = make_member(room.id.clone(), admin_id.clone(), RoomRole::Admin, MemberStatus::Active);
    member_repo.add(&admin_member).await.expect("Failed to create admin");

    let guest_id = UserId::new();
    create_test_user(&pool, &guest_id).await;
    let guest_member = make_member(room.id.clone(), guest_id.clone(), RoomRole::Guest, MemberStatus::Active);
    member_repo.add(&guest_member).await.expect("Failed to create guest");

    let perm_service = make_perm_service(member_repo, room_repo);

    perm_service.check_permission_no_cache(&room.id, &admin_id, PermissionBits::KICK_MEMBER)
        .await
        .expect("Admin should be able to kick members");

    let guest_chat_result = perm_service.check_permission_no_cache(&room.id, &guest_id, PermissionBits::SEND_CHAT)
        .await;
    assert!(guest_chat_result.is_err(), "Guest should not be able to send chat");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_banned_member_denied() {
    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = UserId::new();
    create_test_user(&pool, &creator_id).await;
    let room = make_room(creator_id.clone());
    room_repo.create(&room).await.expect("Failed to create room");

    let user_id = UserId::new();
    create_test_user(&pool, &user_id).await;
    // First add as active member, then ban them (the add() method doesn't support setting banned_at/left_at)
    let member = make_member(room.id.clone(), user_id.clone(), RoomRole::Member, MemberStatus::Active);
    member_repo.add(&member).await.expect("Failed to create member");
    // Now ban the member (this sets banned_at, left_at, and status properly)
    member_repo.ban_member(&room.id, &user_id, &creator_id, Some("Test ban".to_string())).await.expect("Failed to ban member");

    let perm_service = make_perm_service(member_repo, room_repo);

    let result = perm_service.check_permission_no_cache(&room.id, &user_id, PermissionBits::SEND_CHAT)
        .await;
    assert!(result.is_err(), "Banned member should not have permissions");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_non_member_denied() {
    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = UserId::new();
    create_test_user(&pool, &creator_id).await;
    let room = make_room(creator_id);
    room_repo.create(&room).await.expect("Failed to create room");

    let perm_service = make_perm_service(member_repo, room_repo);

    let non_member_id = UserId::new();
    let result = perm_service.check_permission_no_cache(&room.id, &non_member_id, PermissionBits::SEND_CHAT)
        .await;
    assert!(result.is_err(), "Non-member should not have permissions");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_bit_operations() {
    let mut perms = PermissionBits(0);

    perms.grant(PermissionBits::SEND_CHAT);
    assert!(perms.has(PermissionBits::SEND_CHAT));

    perms.grant(PermissionBits::ADD_MEDIA);
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::ADD_MEDIA));

    perms.revoke(PermissionBits::SEND_CHAT);
    assert!(!perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::ADD_MEDIA));

    perms = PermissionBits(PermissionBits::DEFAULT_ADMIN);
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::KICK_MEMBER));
    assert!(perms.has(PermissionBits::SET_ROOM_SETTINGS));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_permission_checks() {
    use std::sync::Arc;

    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = UserId::new();
    create_test_user(&pool, &creator_id).await;
    let room = make_room(creator_id);
    room_repo.create(&room).await.expect("Failed to create room");

    let user_id = UserId::new();
    create_test_user(&pool, &user_id).await;
    let member = make_member(room.id.clone(), user_id.clone(), RoomRole::Member, MemberStatus::Active);
    member_repo.add(&member).await.expect("Failed to create member");

    let perm_service = Arc::new(make_perm_service(member_repo, room_repo));

    let mut handles = vec![];
    for _ in 0..10 {
        let service = perm_service.clone();
        let room_id = room.id.clone();
        let uid = user_id.clone();

        let handle = tokio::spawn(async move {
            service.check_permission(&room_id, &uid, PermissionBits::SEND_CHAT)
                .await
                .expect("Permission check should succeed")
        });
        handles.push(handle);
    }

    // All checks should complete without error (Ok(()) means permission granted)
    for handle in handles {
        handle.await.expect("Join failed");
    }
}
