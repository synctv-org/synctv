//! `PermissionService` integration tests
//!
//! Tests permission checking with real `PostgreSQL` via testcontainers,
//! verifying that the three-layer permission system works end-to-end.
//!
//! Run with: cargo test -p synctv-core --test `permission_service_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{PermissionBits, RoomRole, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
};
use synctv_core_testing::create_test_pool;
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
        email: Some(format!("{username}@test.com")),
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

// ========== Creator Permissions Test ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_creator_has_all_permissions() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("perm_creator")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Perm Creator Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let perms = perm_service
        .get_user_permissions_no_cache(&room.id, &creator.id)
        .await
        .unwrap();

    assert_eq!(
        perms.0,
        PermissionBits::ALL,
        "Creator should have ALL permissions"
    );

    // Check specific permissions
    assert!(perms.has(PermissionBits::DELETE_ROOM));
    assert!(perms.has(PermissionBits::BAN_MEMBER));
    assert!(perms.has(PermissionBits::KICK_USER));
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::ADD_MOVIE));
    assert!(perms.has(PermissionBits::MANAGE_ADMIN));
    assert!(perms.has(PermissionBits::EXPORT_DATA));
}

// ========== Admin Permissions Test ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_default_bits() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("admin_perm_creator"))
        .await
        .unwrap();
    let admin = user_repo
        .create(&make_user("admin_perm_user"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Admin Perm Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), admin.id.clone(), None)
        .await
        .unwrap();

    // Promote to admin
    room_service
        .member_service()
        .set_member_role(
            room.id.clone(),
            creator.id.clone(),
            admin.id.clone(),
            RoomRole::Admin,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let perms = perm_service
        .get_user_permissions_no_cache(&room.id, &admin.id)
        .await
        .unwrap();

    // Admin should have common admin permissions
    assert!(
        perms.has(PermissionBits::BAN_MEMBER),
        "Admin should have BAN_MEMBER"
    );
    assert!(
        perms.has(PermissionBits::KICK_USER),
        "Admin should have KICK_USER"
    );
    assert!(
        perms.has(PermissionBits::SEND_CHAT),
        "Admin should have SEND_CHAT"
    );

    // Admin should NOT have ALL permissions (that's creator-only)
    assert_ne!(
        perms.0,
        PermissionBits::ALL,
        "Admin should not have ALL permissions"
    );
}

// ========== Member Permissions Test ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_default_bits() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("member_perm_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("member_perm_user"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Member Perm Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let perms = perm_service
        .get_user_permissions_no_cache(&room.id, &member.id)
        .await
        .unwrap();

    // Member should have basic permissions
    assert!(
        perms.has(PermissionBits::SEND_CHAT),
        "Member should have SEND_CHAT"
    );
    assert!(
        perms.has(PermissionBits::ADD_MOVIE),
        "Member should have ADD_MOVIE"
    );
    assert!(
        perms.has(PermissionBits::VIEW_PLAYLIST),
        "Member should have VIEW_PLAYLIST"
    );

    // Member should NOT have admin/creator-only permissions
    assert!(
        !perms.has(PermissionBits::BAN_MEMBER),
        "Member should not have BAN_MEMBER"
    );
    assert!(
        !perms.has(PermissionBits::DELETE_ROOM),
        "Member should not have DELETE_ROOM"
    );
}

// ========== Guest Permissions Test ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_guest_default_no_permissions() {
    // This test verifies that guests get minimal permissions.
    // Guests by default can only VIEW_PLAYLIST (from DEFAULT_GUEST).
    // They should NOT have SEND_CHAT, ADD_MOVIE, BAN_MEMBER, etc.
    //
    // Since we can't easily add a guest member through the RoomService
    // (guests use a different flow), we test this through the
    // PermissionService's calculate_role_default_permissions method.
    let (_container, pool) = create_test_pool().await;
    let room_service = make_room_service(pool);

    let perm_service = room_service.permission_service();
    let settings = synctv_core::models::RoomSettings::default();
    let perms = perm_service.calculate_role_default_permissions(&RoomRole::Guest, &settings);

    // Guest should have VIEW_PLAYLIST
    assert!(
        perms.has(PermissionBits::VIEW_PLAYLIST),
        "Guest should have VIEW_PLAYLIST"
    );

    // Guest should NOT have these
    assert!(
        !perms.has(PermissionBits::SEND_CHAT),
        "Guest should not have SEND_CHAT"
    );
    assert!(
        !perms.has(PermissionBits::ADD_MOVIE),
        "Guest should not have ADD_MOVIE"
    );
    assert!(
        !perms.has(PermissionBits::BAN_MEMBER),
        "Guest should not have BAN_MEMBER"
    );
    assert!(
        !perms.has(PermissionBits::DELETE_ROOM),
        "Guest should not have DELETE_ROOM"
    );
}

// ========== Allow Override Test ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_allow_override_role_default() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("allow_creator")).await.unwrap();
    let member = user_repo.create(&make_user("allow_member")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Allow Override Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    // Member by default should NOT have BAN_MEMBER
    let perm_service = room_service.permission_service();
    let perms_before = perm_service
        .get_user_permissions_no_cache(&room.id, &member.id)
        .await
        .unwrap();
    assert!(!perms_before.has(PermissionBits::BAN_MEMBER));

    // Grant BAN_MEMBER to the member (Allow override)
    room_service
        .member_service()
        .grant_permission(
            room.id.clone(),
            creator.id.clone(),
            member.id.clone(),
            PermissionBits::BAN_MEMBER,
        )
        .await
        .unwrap();

    // Now member should have BAN_MEMBER
    let perms_after = perm_service
        .get_user_permissions_no_cache(&room.id, &member.id)
        .await
        .unwrap();
    assert!(
        perms_after.has(PermissionBits::BAN_MEMBER),
        "Member should have BAN_MEMBER after Allow override"
    );

    // Original permissions should still be present
    assert!(
        perms_after.has(PermissionBits::SEND_CHAT),
        "Member should still have SEND_CHAT"
    );
}

// ========== Deny Override Test ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deny_override_role_default() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("deny_creator")).await.unwrap();
    let member = user_repo.create(&make_user("deny_member")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Deny Override Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    // Member should have SEND_CHAT by default
    let perm_service = room_service.permission_service();
    let perms_before = perm_service
        .get_user_permissions_no_cache(&room.id, &member.id)
        .await
        .unwrap();
    assert!(perms_before.has(PermissionBits::SEND_CHAT));

    // Revoke SEND_CHAT (Deny override)
    room_service
        .member_service()
        .revoke_permission(
            room.id.clone(),
            creator.id.clone(),
            member.id.clone(),
            PermissionBits::SEND_CHAT,
        )
        .await
        .unwrap();

    // Now member should NOT have SEND_CHAT
    let perms_after = perm_service
        .get_user_permissions_no_cache(&room.id, &member.id)
        .await
        .unwrap();
    assert!(
        !perms_after.has(PermissionBits::SEND_CHAT),
        "Member should not have SEND_CHAT after Deny override"
    );

    // Other permissions should still be present
    assert!(
        perms_after.has(PermissionBits::ADD_MOVIE),
        "Member should still have ADD_MOVIE"
    );
}

// ========== check_permissions batch (S11) ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_permissions_batch_all_present() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("batch_perm_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Batch Perm Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();

    // Creator should have all these permissions
    let result = perm_service
        .check_permissions(
            &room.id,
            &creator.id,
            &[
                PermissionBits::SEND_CHAT,
                PermissionBits::ADD_MOVIE,
                PermissionBits::DELETE_ROOM,
            ],
        )
        .await;

    assert!(
        result.is_ok(),
        "Creator should have all checked permissions"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_permissions_batch_one_missing_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("batch_miss_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("batch_miss_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Batch Miss Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    let perm_service = room_service.permission_service();

    // Member should have SEND_CHAT but NOT DELETE_ROOM
    let result = perm_service
        .check_permissions(
            &room.id,
            &member.id,
            &[PermissionBits::SEND_CHAT, PermissionBits::DELETE_ROOM],
        )
        .await;

    assert!(
        result.is_err(),
        "Should fail when any one permission is missing"
    );
}

// ========== check_role (S11) ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_role_creator_passes() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("checkrole_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Check Role Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();

    let result = perm_service
        .check_role(&room.id, &creator.id, RoomRole::Creator)
        .await;

    assert!(result.is_ok(), "Creator should pass check_role for Creator");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_role_member_not_creator_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("checkrole2_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("checkrole2_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Check Role 2 Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    let perm_service = room_service.permission_service();

    let result = perm_service
        .check_role(&room.id, &member.id, RoomRole::Creator)
        .await;

    assert!(
        result.is_err(),
        "Member should NOT pass check_role for Creator"
    );
}

// ========== is_admin_or_creator (S11) ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_is_admin_or_creator_for_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("isadmin_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Is Admin Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let result = perm_service
        .is_admin_or_creator(&room.id, &creator.id)
        .await
        .unwrap();

    assert!(result, "Creator should be admin_or_creator");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_is_admin_or_creator_for_regular_member() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("isadmin2_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("isadmin2_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Is Admin 2 Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let result = perm_service
        .is_admin_or_creator(&room.id, &member.id)
        .await
        .unwrap();

    assert!(!result, "Regular member should NOT be admin_or_creator");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_is_admin_or_creator_for_non_member() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("isadmin3_creator"))
        .await
        .unwrap();
    let outsider = user_repo
        .create(&make_user("isadmin3_outsider"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Is Admin 3 Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let result = perm_service
        .is_admin_or_creator(&room.id, &outsider.id)
        .await
        .unwrap();

    assert!(!result, "Non-member should NOT be admin_or_creator");
}
