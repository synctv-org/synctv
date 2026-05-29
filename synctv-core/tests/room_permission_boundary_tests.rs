//! Room permission boundary tests
//!
//! Tests permission boundaries including role hierarchy enforcement,
//! cross-room permission isolation, and permission escalation prevention.
//!
//! Run with: cargo test --test `room_permission_boundary_tests`
//!
//! # Test Coverage
//!
//! - Admin attempting Owner-only operations
//! - Member attempting Admin operations
//! - Cross-room permission isolation
//! - Permission escalation prevention
//! - Role downgrade protection
//!
//! # Requirements
//!
//! - Docker for testcontainers (`PostgreSQL`)
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    models::{
        AddMemberOptions, Room, RoomId, RoomMember, RoomMemberPermissionBits, RoomPermission,
        RoomRole, RoomStatus, User, UserId, UserRole, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        member::MemberService,
        permission::PermissionService,
        InMemoryTokenBlacklistStore, NotificationService, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::{create_test_database_with_options_and_label, TestDatabase};
// Test Infrastructure

async fn create_test_pool() -> TestDatabase {
    create_test_database_with_options_and_label(
        "synctv_test",
        "room-permission-boundary",
        20,
        std::time::Duration::from_secs(30),
    )
    .await
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
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

/// Create a test room
fn make_room(name: &str, description: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: description.to_string(),
        created_by: *owner,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

/// Setup test room with owner and optional settings
async fn setup_test_room(pool: &PgPool, room_name: &str) -> (User, Room) {
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user(&format!("{room_name}_owner")))
        .await
        .expect("Failed to create owner");
    let room = room_repo
        .create(&make_room(room_name, "Test room", &owner.id))
        .await
        .expect("Failed to create room");

    // Add owner as member (Creator)
    let member_repo = RoomMemberRepository::new(pool.clone());
    let owner_member = RoomMember::new(room.id, owner.id, RoomRole::Creator);
    member_repo
        .add(&owner_member)
        .await
        .expect("Failed to add owner as member");

    (owner, room)
}

/// Create member service for testing
fn make_member_service(pool: PgPool) -> MemberService {
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300);

    let mut member_service = MemberService::new(
        member_repo,
        room_repo,
        permission_service,
        NotificationService::default(),
    );
    member_service.set_room_settings_repo(RoomSettingsRepository::new(pool));
    member_service
}

fn make_user_service(pool: &PgPool) -> UserService {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
    let username_cache =
        synctv_core::cache::UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = std::sync::Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let mut service = UserService::new(
        pool,
        jwt_service,
        username_cache,
        synctv_core::config::PasswordComplexityConfig::default(),
        token_blacklist,
        synctv_core::cache::KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test".to_string()),
    );
    service.set_password_hasher(std::sync::Arc::new(TestPasswordHasher::new()));
    service
}

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);
    let mut service = RoomService::new(pool, user_service);
    service.set_password_hasher(std::sync::Arc::new(TestPasswordHasher::new()));
    service
}

/// Test that Admin cannot delete room (Creator-only operation).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_delete_room() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Delete Room Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let admin_user = user_repo
        .create(&make_user("admin_delete"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id, admin_user.id, RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    let room_service = make_room_service(pool.clone());
    let result = room_service.delete_room(room.id, admin_user.id).await;
    assert!(
        result.is_err(),
        "Room-scoped admin should not be able to delete the room"
    );
}

/// Test that Admin cannot transfer ownership (Creator-only operation).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_transfer_ownership() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Transfer Owner Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let admin_user = user_repo
        .create(&make_user("admin_transfer"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id, admin_user.id, RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    let target_user = user_repo
        .create(&make_user("transfer_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id, target_user.id, RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Admin tries to set Creator role (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .set_member_role(room.id, admin_user.id, target_user.id, RoomRole::Creator)
        .await;

    assert!(result.is_err(), "Admin cannot transfer ownership");
}

/// Test that Admin cannot demote Creator.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_demote_creator() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (owner, room) = setup_test_room(pool, "Demote Creator Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let admin_user = user_repo
        .create(&make_user("admin_demote"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id, admin_user.id, RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    // Admin tries to demote Creator to Admin (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .set_member_role(room.id, admin_user.id, owner.id, RoomRole::Admin)
        .await;

    assert!(result.is_err(), "Admin cannot demote Creator");
}

/// Test that Member cannot kick other members (Admin operation).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_cannot_kick() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Kick Test").await;
    let room_service = make_room_service(pool.clone());

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let kicker = user_repo
        .create(&make_user("member_kicker"))
        .await
        .expect("Failed to create kicker");
    let kicker_member = RoomMember::new(room.id, kicker.id, RoomRole::Member);
    member_repo
        .add(&kicker_member)
        .await
        .expect("Failed to add kicker");

    let target = user_repo
        .create(&make_user("kick_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id, target.id, RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Member tries to kick another member (should fail)
    let result = room_service
        .kick_member(room.id, kicker.id, target.id, 60)
        .await;

    assert!(result.is_err(), "Member cannot kick other members");
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.to_lowercase().contains("permission")
                    || msg.to_lowercase().contains("denied")
                    || msg.contains("KICK"),
                "Error should mention permission: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

/// Test that Member cannot change room settings (Admin operation).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_cannot_change_settings() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Settings Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let member_user = user_repo
        .create(&make_user("settings_member"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id, member_user.id, RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Member does not have SET_ROOM_SETTINGS permission by default
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    let member_perms = permission_service
        .get_user_permissions_eventually_consistent(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions");

    assert!(
        !member_perms.has(RoomPermission::SET_ROOM_SETTINGS),
        "Member should not have SET_ROOM_SETTINGS permission"
    );
}

/// Test that Member cannot promote other members.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_cannot_promote() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Promote Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let promoter = user_repo
        .create(&make_user("member_promoter"))
        .await
        .expect("Failed to create promoter");
    let promoter_member = RoomMember::new(room.id, promoter.id, RoomRole::Member);
    member_repo
        .add(&promoter_member)
        .await
        .expect("Failed to add promoter");

    let target = user_repo
        .create(&make_user("promote_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id, target.id, RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Member tries to promote another member to Admin (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .set_member_role(room.id, promoter.id, target.id, RoomRole::Admin)
        .await;

    assert!(result.is_err(), "Member cannot promote other members");
}

/// Test that room permissions are isolated between rooms.
///
/// Scenario:
/// 1. User is Admin in Room A
/// 2. User is Member in Room B
/// 3. User cannot perform Admin operations in Room B
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cross_room_permission_isolation() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner_a, room_a) = setup_test_room(pool, "Room A").await;
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let cross_user = user_repo
        .create(&make_user("cross_room_user"))
        .await
        .expect("Failed to create user");
    let admin_member_a = RoomMember::new(room_a.id, cross_user.id, RoomRole::Admin);
    member_repo
        .add(&admin_member_a)
        .await
        .expect("Failed to add to Room A");

    let (_owner_b, room_b) = setup_test_room(pool, "Room B").await;
    let room_service = make_room_service(pool.clone());
    let member_b = RoomMember::new(room_b.id, cross_user.id, RoomRole::Member);
    member_repo
        .add(&member_b)
        .await
        .expect("Failed to add to Room B");

    let target = user_repo
        .create(&make_user("room_b_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room_b.id, target.id, RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // User (Admin in Room A) tries to kick member in Room B (should fail)
    let result = room_service
        .kick_member(room_b.id, cross_user.id, target.id, 60)
        .await;

    assert!(result.is_err(), "Admin in Room A cannot kick in Room B");
}

/// Test that a user kicked in one room can still join other rooms.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_cooldown_isolated_to_single_room() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (owner_a, room_a) = setup_test_room(pool, "Kick Room A").await;
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let kicked_user = user_repo
        .create(&make_user("kicked_user"))
        .await
        .expect("Failed to create user");
    let member_a = RoomMember::new(room_a.id, kicked_user.id, RoomRole::Member);
    member_repo
        .add(&member_a)
        .await
        .expect("Failed to add to Room A");

    let room_service = make_room_service(pool.clone());
    room_service
        .kick_member(room_a.id, owner_a.id, kicked_user.id, 3600)
        .await
        .expect("Failed to kick user");

    let (_owner_b, room_b) = setup_test_room(pool, "Kick Room B").await;

    // User should be able to join Room B (kick cooldown is only in Room A)
    let result = make_member_service(pool.clone())
        .add_member_with_options(
            room_b.id,
            kicked_user.id,
            RoomRole::Member,
            AddMemberOptions::new(),
        )
        .await;

    assert!(result.is_ok(), "User kicked in Room A can join Room B");
}

/// Test that Creator role in one room doesn't grant Creator permissions in another.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_creator_role_not_global() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator_user = user_repo
        .create(&make_user("creator_user"))
        .await
        .expect("Failed to create creator");
    let room_a = {
        let room_repo = RoomRepository::new(pool.clone());
        let room = room_repo
            .create(&make_room(
                "Creator Room",
                "Room where user is creator",
                &creator_user.id,
            ))
            .await
            .expect("Failed to create room");
        let member = RoomMember::new(room.id, creator_user.id, RoomRole::Creator);
        member_repo
            .add(&member)
            .await
            .expect("Failed to add creator");
        room
    };

    let (_owner_b, room_b) = setup_test_room(pool, "Other Room").await;
    let member_b = RoomMember::new(room_b.id, creator_user.id, RoomRole::Member);
    member_repo
        .add(&member_b)
        .await
        .expect("Failed to add to Room B");

    // User is Creator in Room A but only Member in Room B
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    let perms_a = permission_service
        .get_user_permissions_eventually_consistent(&room_a.id, &creator_user.id)
        .await
        .expect("Failed to get Room A permissions");

    let perms_b = permission_service
        .get_user_permissions_eventually_consistent(&room_b.id, &creator_user.id)
        .await
        .expect("Failed to get Room B permissions");

    // Room A permissions should be higher (Creator has all admin/member permissions)
    assert!(
        perms_a.has(RoomPermission::KICK_MEMBER),
        "Creator should have admin permissions in Room A"
    );
    assert!(
        !perms_b.has(RoomPermission::KICK_MEMBER),
        "Member should not have admin permissions in Room B"
    );
}

/// Test that users cannot grant themselves higher permissions.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_cannot_grant_self_permissions() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Self Grant Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let member_user = user_repo
        .create(&make_user("self_granter"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id, member_user.id, RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Member tries to grant themselves a permission (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .grant_permission(
            room.id,
            member_user.id,
            member_user.id,
            RoomMemberPermissionBits::USE_WEBRTC,
        )
        .await;

    assert!(
        result.is_err(),
        "Member cannot grant themselves permissions"
    );
}

/// Test that Admin cannot create new Admin (only Creator can).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_create_admin() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Admin Create Admin Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let admin_user = user_repo
        .create(&make_user("admin_creator"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id, admin_user.id, RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    let target_user = user_repo
        .create(&make_user("admin_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id, target_user.id, RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Admin tries to promote Member to Admin (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .set_member_role(room.id, admin_user.id, target_user.id, RoomRole::Admin)
        .await;

    assert!(result.is_err(), "Admin cannot create new Admin");
}

/// Test that permission grant requires `GRANT_PERMISSION`.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_grant_permission_requires_permission() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Grant Permission Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let grantor = user_repo
        .create(&make_user("grantor"))
        .await
        .expect("Failed to create grantor");
    let grantor_member = RoomMember::new(room.id, grantor.id, RoomRole::Member);
    member_repo
        .add(&grantor_member)
        .await
        .expect("Failed to add grantor");

    let target = user_repo
        .create(&make_user("grantee"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id, target.id, RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Member tries to grant permission (should fail - no GRANT_PERMISSION)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .grant_permission(
            room.id,
            grantor.id,
            target.id,
            RoomMemberPermissionBits::CHAT,
        )
        .await;

    assert!(
        result.is_err(),
        "Member without GRANT_PERMISSION cannot grant"
    );
}

/// Test that users cannot downgrade someone with equal or higher role.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cannot_downgrade_equal_role() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Downgrade Equal Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let admin1 = user_repo
        .create(&make_user("admin1_downgrade"))
        .await
        .expect("Failed to create admin1");
    let admin1_member = RoomMember::new(room.id, admin1.id, RoomRole::Admin);
    member_repo
        .add(&admin1_member)
        .await
        .expect("Failed to add admin1");

    let admin2 = user_repo
        .create(&make_user("admin2_downgrade"))
        .await
        .expect("Failed to create admin2");
    let admin2_member = RoomMember::new(room.id, admin2.id, RoomRole::Admin);
    member_repo
        .add(&admin2_member)
        .await
        .expect("Failed to add admin2");

    // Admin1 tries to downgrade Admin2 (should fail - equal role)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .set_member_role(room.id, admin1.id, admin2.id, RoomRole::Member)
        .await;

    assert!(result.is_err(), "Admin cannot downgrade Admin (equal role)");
}

/// Test that kick respects role hierarchy.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_respects_role_hierarchy() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;
    let room_service = make_room_service(pool.clone());

    let (_owner, room) = setup_test_room(pool, "Kick Hierarchy Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let admin = user_repo
        .create(&make_user("kick_admin"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id, admin.id, RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    let member = user_repo
        .create(&make_user("kick_member"))
        .await
        .expect("Failed to create member");
    let member_member = RoomMember::new(room.id, member.id, RoomRole::Member);
    member_repo
        .add(&member_member)
        .await
        .expect("Failed to add member");

    // Member tries to kick Admin (should fail - lower role)
    let result = room_service
        .kick_member(room.id, member.id, admin.id, 60)
        .await;

    assert!(result.is_err(), "Member cannot kick Admin (role hierarchy)");
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.to_lowercase().contains("permission")
                    || msg.to_lowercase().contains("denied")
                    || msg.contains("cannot kick")
                    || msg.contains("higher"),
                "Error should mention permission or role: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

/// Test that revoked permissions are actually denied.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_revoked_permission_denied() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Revoke Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let member_user = user_repo
        .create(&make_user("revoke_member"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id, member_user.id, RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Get owner for operations
    let members = member_repo
        .list_by_room_all(&room.id)
        .await
        .expect("Failed to list members");
    let owner_user_id = members
        .iter()
        .find(|m| m.role == RoomRole::Creator)
        .expect("Creator should exist")
        .user_id;

    // Revoke CHAT from member
    let member_service = make_member_service(pool.clone());
    member_service
        .revoke_permission(
            room.id,
            owner_user_id,
            member_user.id,
            RoomMemberPermissionBits::CHAT,
        )
        .await
        .expect("Failed to revoke permission");

    // Verify permission is denied
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    let effective = permission_service
        .get_user_permissions_no_cache(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions");

    assert!(
        !effective.has(RoomPermission::CHAT),
        "CHAT should be denied after revocation"
    );
}

/// Test that Member without `CHAT` cannot send messages.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_without_chat_cannot_send() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "No Chat Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let muted_user = user_repo
        .create(&make_user("muted_user"))
        .await
        .expect("Failed to create muted user");
    let mut muted_member = RoomMember::new(room.id, muted_user.id, RoomRole::Member);
    // Revoke CHAT
    muted_member.removed_permissions = RoomMemberPermissionBits::CHAT;
    member_repo
        .add(&muted_member)
        .await
        .expect("Failed to add muted user");

    // Verify user doesn't have CHAT
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    let perms = permission_service
        .get_user_permissions_eventually_consistent(&room.id, &muted_user.id)
        .await
        .expect("Failed to get permissions");

    assert!(
        !perms.has(RoomPermission::CHAT),
        "Muted user should not have CHAT"
    );
}
