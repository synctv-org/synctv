//! Permission check integration tests with database
//!
//! Tests verify Allow/Deny permission pattern and integration with database.
//!
//! Requires Docker for testcontainers.

use sqlx::PgPool;
use synctv_core::{
    models::{
        Room, RoomId, RoomMember, RoomMemberPermissionBits, RoomPermission, RoomPermissionSet,
        RoomRole, SignupMethod, User, UserId, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, UserRepository},
    service::PermissionService,
};
use synctv_core_testing::{create_test_pool, ok};

/// Create a test user in the database (required for FK constraints)
async fn create_test_user(pool: &PgPool, user_id: &UserId) -> UserId {
    let username = format!("test_user_{user_id}");
    let user = User {
        id: *user_id,
        username,
        signup_method: SignupMethod::Email,
        role: synctv_core::models::UserRole::User,
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
    let user_repo = UserRepository::new(pool.clone());
    ok(
        user_repo.create(&user).await,
        "permission test user should be created",
    )
    .id
}

fn make_member(room_id: RoomId, user_id: UserId, role: RoomRole) -> RoomMember {
    RoomMember {
        room_id,
        user_id,
        role,
        status: synctv_core::models::MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        remark_name: String::new(),
        display_tag: String::new(),
        joined_at: chrono::Utc::now(),
        version: 0,
    }
}

fn make_room(creator_id: UserId) -> Room {
    Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: creator_id,
        status: synctv_core::models::RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
        version: 0,
        last_activity_at: chrono::Utc::now(),
    }
}

fn make_perm_service(
    member_repo: RoomMemberRepository,
    room_repo: RoomRepository,
) -> PermissionService {
    ok(
        PermissionService::new(member_repo, room_repo, None, 1000, 300),
        "permission service should build",
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_check_with_database_member() {
    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = create_test_user(&pool, &UserId::new()).await;
    let room = make_room(creator_id);
    let room = ok(
        room_repo.create(&room).await,
        "permission test room should be created",
    );

    let user_id = create_test_user(&pool, &UserId::new()).await;
    let mut member = make_member(room.id, user_id, RoomRole::Member);
    member.added_permissions =
        RoomMemberPermissionBits::CHAT | RoomMemberPermissionBits::CREATE_MEDIA_RESOURCE;
    ok(
        member_repo.add(&member).await,
        "permission test member should be created",
    );

    let perm_service = make_perm_service(member_repo, room_repo);

    // check_permission_no_cache returns Ok(()) if granted, Err if denied
    ok(
        perm_service
            .check_permission_no_cache(&room.id, &user_id, RoomPermission::CHAT)
            .await,
        "user should have CHAT permission",
    );

    ok(
        perm_service
            .check_permission_no_cache(&room.id, &user_id, RoomPermission::CREATE_MEDIA_RESOURCE)
            .await,
        "user should have CREATE_MEDIA_RESOURCE permission",
    );

    let kick_result = perm_service
        .check_permission_no_cache(&room.id, &user_id, RoomPermission::KICK_MEMBER)
        .await;
    assert!(
        kick_result.is_err(),
        "User should not have KICK_MEMBER permission"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_allow_deny_pattern() {
    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = create_test_user(&pool, &UserId::new()).await;
    let room = make_room(creator_id);
    let room = ok(
        room_repo.create(&room).await,
        "permission test room should be created",
    );

    let admin_id = create_test_user(&pool, &UserId::new()).await;
    let admin_member = make_member(room.id, admin_id, RoomRole::Admin);
    ok(
        member_repo.add(&admin_member).await,
        "permission test admin should be created",
    );

    let guest_id = create_test_user(&pool, &UserId::new()).await;
    let guest_member = make_member(room.id, guest_id, RoomRole::Guest);
    ok(
        member_repo.add(&guest_member).await,
        "permission test guest should be created",
    );

    let perm_service = make_perm_service(member_repo, room_repo);

    ok(
        perm_service
            .check_permission_no_cache(&room.id, &admin_id, RoomPermission::KICK_MEMBER)
            .await,
        "admin should be able to kick members",
    );

    let guest_chat_result = perm_service
        .check_permission_no_cache(&room.id, &guest_id, RoomPermission::CHAT)
        .await;
    assert!(
        guest_chat_result.is_err(),
        "Guest should not be able to send chat"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_removed_member_denied() {
    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = create_test_user(&pool, &UserId::new()).await;
    let room = make_room(creator_id);
    let room = ok(
        room_repo.create(&room).await,
        "permission test room should be created",
    );

    let user_id = create_test_user(&pool, &UserId::new()).await;
    let member = make_member(room.id, user_id, RoomRole::Member);
    ok(
        member_repo.add(&member).await,
        "permission test member should be created",
    );
    ok(
        member_repo.remove(&room.id, &user_id).await,
        "permission test membership should be removed",
    );

    let perm_service = make_perm_service(member_repo, room_repo);

    let result = perm_service
        .check_permission_no_cache(&room.id, &user_id, RoomPermission::CHAT)
        .await;
    assert!(
        result.is_err(),
        "Removed member should not have permissions"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_non_member_denied() {
    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = create_test_user(&pool, &UserId::new()).await;
    let room = make_room(creator_id);
    let room = ok(
        room_repo.create(&room).await,
        "permission test room should be created",
    );

    let perm_service = make_perm_service(member_repo, room_repo);

    let non_member_id = UserId::new();
    let result = perm_service
        .check_permission_no_cache(&room.id, &non_member_id, RoomPermission::CHAT)
        .await;
    assert!(result.is_err(), "Non-member should not have permissions");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_bit_operations() {
    let mut perms = RoomPermissionSet(0);

    perms.grant(RoomPermission::CHAT);
    assert!(perms.has(RoomPermission::CHAT));

    perms.grant(RoomPermission::CREATE_MEDIA_RESOURCE);
    assert!(perms.has(RoomPermission::CHAT));
    assert!(perms.has(RoomPermission::CREATE_MEDIA_RESOURCE));

    perms.revoke(RoomPermission::CHAT);
    assert!(!perms.has(RoomPermission::CHAT));
    assert!(perms.has(RoomPermission::CREATE_MEDIA_RESOURCE));

    perms = RoomPermissionSet::default_admin();
    assert!(perms.has(RoomPermission::CHAT));
    assert!(perms.has(RoomPermission::KICK_MEMBER));
    assert!(perms.has(RoomPermission::SET_ROOM_SETTINGS));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_permission_checks() {
    use std::sync::Arc;

    let (_container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_id = create_test_user(&pool, &UserId::new()).await;
    let room = make_room(creator_id);
    let room = ok(
        room_repo.create(&room).await,
        "permission test room should be created",
    );

    let user_id = create_test_user(&pool, &UserId::new()).await;
    let member = make_member(room.id, user_id, RoomRole::Member);
    ok(
        member_repo.add(&member).await,
        "permission test member should be created",
    );

    let perm_service = Arc::new(make_perm_service(member_repo, room_repo));

    let mut handles = vec![];
    for _ in 0..10 {
        let service = perm_service.clone();
        let room_id = room.id;
        let uid = user_id;

        let handle = tokio::spawn(async move {
            ok(
                service
                    .check_permission(&room_id, &uid, RoomPermission::CHAT)
                    .await,
                "permission check should succeed",
            );
        });
        handles.push(handle);
    }

    // All checks should complete without error (Ok(()) means permission granted)
    for handle in handles {
        ok(handle.await, "permission check task should join");
    }
}
