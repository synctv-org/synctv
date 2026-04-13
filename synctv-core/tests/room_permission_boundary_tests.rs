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
        PermissionBits, Room, RoomId, RoomMember, RoomRole, RoomStatus, User, UserId, UserRole,
        UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::{
        member::{AddMemberOptions, MemberService},
        permission::PermissionService,
        NotificationService,
    },
    Error,
};
use synctv_core_testing::{create_test_pool_with_options_and_label, TestContainer};
// ============================================================================
// Test Infrastructure
// ============================================================================

/// Test container wrapper for Postgres
pub struct TestPostgres {
    pub pool: PgPool,
    #[allow(dead_code)]
    container: TestContainer,
}

async fn create_test_pool() -> TestPostgres {
    let (container, pool) = create_test_pool_with_options_and_label(
        "synctv_test",
        "room-permission-boundary",
        20,
        std::time::Duration::from_secs(30),
    )
    .await;

    TestPostgres { pool, container }
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
    let owner_member = RoomMember::new(room.id.clone(), owner.id.clone(), RoomRole::Creator);
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

// ============================================================================
// Test: Admin Attempting Creator-Only Operations
// ============================================================================

/// Test that Admin cannot delete room (Creator-only operation).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_delete_room() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Delete Room Test").await;

    // Create admin
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let admin_user = user_repo
        .create(&make_user("admin_delete"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id.clone(), admin_user.id.clone(), RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    // Admin does not have DELETE_ROOM permission by default
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    let admin_perms = permission_service
        .get_user_permissions(&room.id, &admin_user.id)
        .await
        .expect("Failed to get permissions");

    // Verify Admin doesn't have DELETE_ROOM permission
    assert!(
        !admin_perms.has(PermissionBits::DELETE_ROOM),
        "Admin should not have DELETE_ROOM permission"
    );
}

/// Test that Admin cannot transfer ownership (Creator-only operation).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_transfer_ownership() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Transfer Owner Test").await;

    // Create admin and another member
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let admin_user = user_repo
        .create(&make_user("admin_transfer"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id.clone(), admin_user.id.clone(), RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    let target_user = user_repo
        .create(&make_user("transfer_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id.clone(), target_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Admin tries to set Creator role (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .set_member_role(
            room.id.clone(),
            admin_user.id.clone(),
            target_user.id.clone(),
            RoomRole::Creator,
        )
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

    // Create admin
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let admin_user = user_repo
        .create(&make_user("admin_demote"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id.clone(), admin_user.id.clone(), RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    // Admin tries to demote Creator to Admin (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .set_member_role(
            room.id.clone(),
            admin_user.id.clone(),
            owner.id.clone(),
            RoomRole::Admin,
        )
        .await;

    assert!(result.is_err(), "Admin cannot demote Creator");
}

// ============================================================================
// Test: Member Attempting Admin Operations
// ============================================================================

/// Test that Member cannot kick other members (Admin operation).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_cannot_kick() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Kick Test").await;

    // Create two members
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let kicker = user_repo
        .create(&make_user("member_kicker"))
        .await
        .expect("Failed to create kicker");
    let kicker_member = RoomMember::new(room.id.clone(), kicker.id.clone(), RoomRole::Member);
    member_repo
        .add(&kicker_member)
        .await
        .expect("Failed to add kicker");

    let target = user_repo
        .create(&make_user("kick_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id.clone(), target.id.clone(), RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Member tries to kick another member (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .kick_member(room.id.clone(), kicker.id.clone(), target.id.clone())
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

/// Test that Member cannot ban other members (Admin operation).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_cannot_ban() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Ban Test").await;

    // Create two members
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let banner = user_repo
        .create(&make_user("member_banner"))
        .await
        .expect("Failed to create banner");
    let banner_member = RoomMember::new(room.id.clone(), banner.id.clone(), RoomRole::Member);
    member_repo
        .add(&banner_member)
        .await
        .expect("Failed to add banner");

    let target = user_repo
        .create(&make_user("ban_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id.clone(), target.id.clone(), RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Member tries to ban another member (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .ban_member(
            room.id.clone(),
            banner.id.clone(),
            target.id.clone(),
            Some("Test ban".to_string()),
        )
        .await;

    assert!(result.is_err(), "Member cannot ban other members");
}

/// Test that Member cannot change room settings (Admin operation).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_cannot_change_settings() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Settings Test").await;

    // Create member
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let member_user = user_repo
        .create(&make_user("settings_member"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id.clone(), member_user.id.clone(), RoomRole::Member);
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
        .get_user_permissions(&room.id, &member_user.id)
        .await
        .expect("Failed to get permissions");

    assert!(
        !member_perms.has(PermissionBits::SET_ROOM_SETTINGS),
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

    // Create two members
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let promoter = user_repo
        .create(&make_user("member_promoter"))
        .await
        .expect("Failed to create promoter");
    let promoter_member = RoomMember::new(room.id.clone(), promoter.id.clone(), RoomRole::Member);
    member_repo
        .add(&promoter_member)
        .await
        .expect("Failed to add promoter");

    let target = user_repo
        .create(&make_user("promote_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id.clone(), target.id.clone(), RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Member tries to promote another member to Admin (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .set_member_role(
            room.id.clone(),
            promoter.id.clone(),
            target.id.clone(),
            RoomRole::Admin,
        )
        .await;

    assert!(result.is_err(), "Member cannot promote other members");
}

// ============================================================================
// Test: Cross-Room Permission Isolation
// ============================================================================

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

    // Create Room A with user as Admin
    let (_owner_a, room_a) = setup_test_room(pool, "Room A").await;
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let cross_user = user_repo
        .create(&make_user("cross_room_user"))
        .await
        .expect("Failed to create user");
    let admin_member_a = RoomMember::new(room_a.id.clone(), cross_user.id.clone(), RoomRole::Admin);
    member_repo
        .add(&admin_member_a)
        .await
        .expect("Failed to add to Room A");

    // Create Room B with same user as Member
    let (_owner_b, room_b) = setup_test_room(pool, "Room B").await;
    let member_b = RoomMember::new(room_b.id.clone(), cross_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&member_b)
        .await
        .expect("Failed to add to Room B");

    // Create a target member in Room B
    let target = user_repo
        .create(&make_user("room_b_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room_b.id.clone(), target.id.clone(), RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // User (Admin in Room A) tries to kick member in Room B (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .kick_member(room_b.id.clone(), cross_user.id.clone(), target.id.clone())
        .await;

    assert!(result.is_err(), "Admin in Room A cannot kick in Room B");
}

/// Test that a user banned in one room can still join other rooms.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_isolated_to_single_room() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Create Room A
    let (owner_a, room_a) = setup_test_room(pool, "Ban Room A").await;
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create user and add to Room A
    let banned_user = user_repo
        .create(&make_user("banned_user"))
        .await
        .expect("Failed to create user");
    let member_a = RoomMember::new(room_a.id.clone(), banned_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&member_a)
        .await
        .expect("Failed to add to Room A");

    // Ban user from Room A
    let member_service = make_member_service(pool.clone());
    member_service
        .ban_member(
            room_a.id.clone(),
            owner_a.id.clone(),
            banned_user.id.clone(),
            Some("Test ban".to_string()),
        )
        .await
        .expect("Failed to ban user");

    // Create Room B
    let (_owner_b, room_b) = setup_test_room(pool, "Ban Room B").await;

    // User should be able to join Room B (ban is only in Room A)
    let result = member_service
        .add_member_with_options(
            room_b.id.clone(),
            banned_user.id.clone(),
            RoomRole::Member,
            AddMemberOptions::new(),
        )
        .await;

    assert!(result.is_ok(), "User banned in Room A can join Room B");
}

/// Test that Creator role in one room doesn't grant Creator permissions in another.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_creator_role_not_global() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Create Room A with user as Creator
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
        let member = RoomMember::new(room.id.clone(), creator_user.id.clone(), RoomRole::Creator);
        member_repo
            .add(&member)
            .await
            .expect("Failed to add creator");
        room
    };

    // Create Room B with same user as Member
    let (_owner_b, room_b) = setup_test_room(pool, "Other Room").await;
    let member_b = RoomMember::new(room_b.id.clone(), creator_user.id.clone(), RoomRole::Member);
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
        .get_user_permissions(&room_a.id, &creator_user.id)
        .await
        .expect("Failed to get Room A permissions");

    let perms_b = permission_service
        .get_user_permissions(&room_b.id, &creator_user.id)
        .await
        .expect("Failed to get Room B permissions");

    // Room A permissions should be higher (Creator has all permissions)
    assert!(
        perms_a.has(PermissionBits::DELETE_ROOM),
        "Creator should have DELETE_ROOM in Room A"
    );
    assert!(
        !perms_b.has(PermissionBits::DELETE_ROOM),
        "Member should not have DELETE_ROOM in Room B"
    );
}

// ============================================================================
// Test: Permission Escalation Prevention
// ============================================================================

/// Test that users cannot grant themselves higher permissions.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_cannot_grant_self_permissions() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Self Grant Test").await;

    // Create member
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let member_user = user_repo
        .create(&make_user("self_granter"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id.clone(), member_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&member)
        .await
        .expect("Failed to add member");

    // Member tries to grant themselves BAN_MEMBER permission (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .grant_permission(
            room.id.clone(),
            member_user.id.clone(),
            member_user.id.clone(),
            PermissionBits::BAN_MEMBER,
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

    // Create admin and member
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let admin_user = user_repo
        .create(&make_user("admin_creator"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id.clone(), admin_user.id.clone(), RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    let target_user = user_repo
        .create(&make_user("admin_target"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id.clone(), target_user.id.clone(), RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Admin tries to promote Member to Admin (should fail)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .set_member_role(
            room.id.clone(),
            admin_user.id.clone(),
            target_user.id.clone(),
            RoomRole::Admin,
        )
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

    // Create two members
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let grantor = user_repo
        .create(&make_user("grantor"))
        .await
        .expect("Failed to create grantor");
    let grantor_member = RoomMember::new(room.id.clone(), grantor.id.clone(), RoomRole::Member);
    member_repo
        .add(&grantor_member)
        .await
        .expect("Failed to add grantor");

    let target = user_repo
        .create(&make_user("grantee"))
        .await
        .expect("Failed to create target");
    let target_member = RoomMember::new(room.id.clone(), target.id.clone(), RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .expect("Failed to add target");

    // Member tries to grant permission (should fail - no GRANT_PERMISSION)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .grant_permission(
            room.id.clone(),
            grantor.id.clone(),
            target.id.clone(),
            PermissionBits::SEND_CHAT,
        )
        .await;

    assert!(
        result.is_err(),
        "Member without GRANT_PERMISSION cannot grant"
    );
}

// ============================================================================
// Test: Role Downgrade Protection
// ============================================================================

/// Test that users cannot downgrade someone with equal or higher role.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cannot_downgrade_equal_role() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Downgrade Equal Test").await;

    // Create two admins
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let admin1 = user_repo
        .create(&make_user("admin1_downgrade"))
        .await
        .expect("Failed to create admin1");
    let admin1_member = RoomMember::new(room.id.clone(), admin1.id.clone(), RoomRole::Admin);
    member_repo
        .add(&admin1_member)
        .await
        .expect("Failed to add admin1");

    let admin2 = user_repo
        .create(&make_user("admin2_downgrade"))
        .await
        .expect("Failed to create admin2");
    let admin2_member = RoomMember::new(room.id.clone(), admin2.id.clone(), RoomRole::Admin);
    member_repo
        .add(&admin2_member)
        .await
        .expect("Failed to add admin2");

    // Admin1 tries to downgrade Admin2 (should fail - equal role)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .set_member_role(
            room.id.clone(),
            admin1.id.clone(),
            admin2.id.clone(),
            RoomRole::Member,
        )
        .await;

    assert!(result.is_err(), "Admin cannot downgrade Admin (equal role)");
}

/// Test that kick respects role hierarchy.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_respects_role_hierarchy() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Kick Hierarchy Test").await;

    // Create admin and member
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let admin = user_repo
        .create(&make_user("kick_admin"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id.clone(), admin.id.clone(), RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    let member = user_repo
        .create(&make_user("kick_member"))
        .await
        .expect("Failed to create member");
    let member_member = RoomMember::new(room.id.clone(), member.id.clone(), RoomRole::Member);
    member_repo
        .add(&member_member)
        .await
        .expect("Failed to add member");

    // Member tries to kick Admin (should fail - lower role)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .kick_member(room.id.clone(), member.id.clone(), admin.id.clone())
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

/// Test that ban respects role hierarchy.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_respects_role_hierarchy() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (owner, room) = setup_test_room(pool, "Ban Hierarchy Test").await;

    // Create admin
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let admin = user_repo
        .create(&make_user("ban_admin"))
        .await
        .expect("Failed to create admin");
    let admin_member = RoomMember::new(room.id.clone(), admin.id.clone(), RoomRole::Admin);
    member_repo
        .add(&admin_member)
        .await
        .expect("Failed to add admin");

    // Admin tries to ban Creator (should fail - Creator has higher role)
    let member_service = make_member_service(pool.clone());
    let result = member_service
        .ban_member(
            room.id.clone(),
            admin.id.clone(),
            owner.id.clone(),
            Some("Test ban".to_string()),
        )
        .await;

    assert!(result.is_err(), "Admin cannot ban Creator (role hierarchy)");
}

// ============================================================================
// Test: Permission Revocation Boundary
// ============================================================================

/// Test that revoked permissions are actually denied.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_revoked_permission_denied() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "Revoke Test").await;

    // Create member
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let member_user = user_repo
        .create(&make_user("revoke_member"))
        .await
        .expect("Failed to create member");
    let member = RoomMember::new(room.id.clone(), member_user.id.clone(), RoomRole::Member);
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
        .user_id
        .clone();

    // Revoke SEND_CHAT from member
    let member_service = make_member_service(pool.clone());
    member_service
        .revoke_permission(
            room.id.clone(),
            owner_user_id.clone(),
            member_user.id.clone(),
            PermissionBits::SEND_CHAT,
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
        !effective.has(PermissionBits::SEND_CHAT),
        "SEND_CHAT should be denied after revocation"
    );
}

/// Test that Member without `SEND_CHAT` cannot send messages.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_without_send_chat_cannot_send() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room) = setup_test_room(pool, "No Chat Test").await;

    // Create member with SEND_CHAT revoked
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let muted_user = user_repo
        .create(&make_user("muted_user"))
        .await
        .expect("Failed to create muted user");
    let mut muted_member =
        RoomMember::new(room.id.clone(), muted_user.id.clone(), RoomRole::Member);
    // Revoke SEND_CHAT
    muted_member.removed_permissions = PermissionBits::SEND_CHAT;
    member_repo
        .add(&muted_member)
        .await
        .expect("Failed to add muted user");

    // Verify user doesn't have SEND_CHAT
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    let perms = permission_service
        .get_user_permissions(&room.id, &muted_user.id)
        .await
        .expect("Failed to get permissions");

    assert!(
        !perms.has(PermissionBits::SEND_CHAT),
        "Muted user should not have SEND_CHAT"
    );
}
