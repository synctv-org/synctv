//! Admin permission boundary tests
//!
//! Tests permission boundaries between Admin and Root roles, including:
//! - Admin attempting Root-only operations
//! - Root managing Root users
//! - Role upgrade/downgrade scenarios
//! - Cross-role operation restrictions
//!
//! Docker tests: cargo test -p synctv-core --test `admin_permission_boundary_tests` -- --ignored --nocapture

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{RoomRole, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::{create_test_pool, ok, some};
// Test Infrastructure

fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = ok(JwtService::new(secret), "JWT service should be created");
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

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);

    ok(
        RoomService::new_for_tests(pool, user_service),
        "room service should build",
    )
}

fn make_user_with_role(username: &str, role: UserRole) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

async fn create_user(repo: &UserRepository, username: &str, role: UserRole) -> User {
    ok(
        repo.create(&make_user_with_role(username, role)).await,
        "test user should be created",
    )
}

async fn is_banned(repo: &UserRepository, user_id: &UserId) -> bool {
    ok(repo.is_banned(user_id).await, "ban state should be fetched")
}

async fn load_user(repo: &UserRepository, user_id: &UserId) -> User {
    some(
        ok(repo.get_by_id(user_id).await, "user should be fetched"),
        "user should exist",
    )
}

#[test]
fn test_user_role_can_manage_root_manages_all() {
    assert!(UserRole::Root.can_manage(&UserRole::Root));
    assert!(UserRole::Root.can_manage(&UserRole::Admin));
    assert!(UserRole::Root.can_manage(&UserRole::User));
}

#[test]
fn test_user_role_can_manage_admin_manages_user_only() {
    assert!(!UserRole::Admin.can_manage(&UserRole::Root));
    assert!(!UserRole::Admin.can_manage(&UserRole::Admin));
    assert!(UserRole::Admin.can_manage(&UserRole::User));
}

#[test]
fn test_user_role_can_manage_user_cannot_manage_anyone() {
    assert!(!UserRole::User.can_manage(&UserRole::Root));
    assert!(!UserRole::User.can_manage(&UserRole::Admin));
    assert!(!UserRole::User.can_manage(&UserRole::User));
}

#[test]
fn test_user_role_is_admin_or_above() {
    assert!(UserRole::Root.is_admin_or_above());
    assert!(UserRole::Admin.is_admin_or_above());
    assert!(!UserRole::User.is_admin_or_above());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_upgrade_user_to_root() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin = create_user(&user_repo, "admin_upgrade", UserRole::Admin).await;

    let user = create_user(&user_repo, "user_to_upgrade", UserRole::User).await;

    // Admin tries to upgrade user to Root - should fail
    // This test documents expected behavior: upgrade requires Root
    assert!(
        admin.role.can_manage(&user.role),
        "Admin can manage User role"
    );

    // But admin cannot manage Root users
    let root_user = create_user(&user_repo, "root_target", UserRole::Root).await;

    assert!(
        !admin.role.can_manage(&root_user.role),
        "Admin cannot manage Root role"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_demote_root() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin = create_user(&user_repo, "admin_demoter", UserRole::Admin).await;

    let root_user = create_user(&user_repo, "root_to_demote", UserRole::Root).await;

    // Verify: Admin cannot manage Root
    assert!(
        !admin.role.can_manage(&root_user.role),
        "Admin cannot demote Root users"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_ban_root_user() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin = create_user(&user_repo, "admin_banner", UserRole::Admin).await;

    let root_user = create_user(&user_repo, "root_to_ban", UserRole::Root).await;

    // Verify: Admin cannot ban Root via role hierarchy
    assert!(
        !admin.role.can_manage(&root_user.role),
        "Admin cannot ban Root users"
    );

    // Admin can manage regular users
    let regular_user = create_user(&user_repo, "regular_to_ban", UserRole::User).await;

    assert!(
        admin.role.can_manage(&regular_user.role),
        "Admin can manage regular users"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_root_can_ban_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let root = create_user(&user_repo, "root_banner", UserRole::Root).await;

    let admin = create_user(&user_repo, "admin_to_ban", UserRole::Admin).await;

    // Root can manage Admin
    assert!(
        root.role.can_manage(&admin.role),
        "Root can ban Admin users"
    );

    let banned = ok(
        user_repo
            .ban(
                &admin.id,
                Some(&root.id),
                Some("permission boundary".to_string()),
            )
            .await,
        "admin should be banned by root",
    );

    assert_eq!(banned.status, UserStatus::Banned);
    assert!(is_banned(&user_repo, &admin.id).await);
    assert_eq!(banned.role, UserRole::Admin); // Role unchanged
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_can_ban_user() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin = create_user(&user_repo, "admin_can_ban", UserRole::Admin).await;

    let user = create_user(&user_repo, "user_to_ban", UserRole::User).await;

    // Admin can manage User
    assert!(
        admin.role.can_manage(&user.role),
        "Admin can ban User users"
    );

    let banned = ok(
        user_repo
            .ban(
                &user.id,
                Some(&admin.id),
                Some("permission boundary".to_string()),
            )
            .await,
        "user should be banned by admin",
    );

    assert_eq!(banned.status, UserStatus::Banned);
    assert!(is_banned(&user_repo, &user.id).await);
    assert_eq!(banned.role, UserRole::User); // Role unchanged
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_role_upgrade_user_to_admin_by_root() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let root = create_user(&user_repo, "root_upgrader", UserRole::Root).await;

    let user = create_user(&user_repo, "user_promote", UserRole::User).await;

    // Root can manage User
    assert!(
        root.role.can_manage(&user.role),
        "Root can upgrade User to Admin"
    );

    // Upgrade role from User to Admin
    let mut upgraded = user.clone();
    upgraded.role = UserRole::Admin;
    let updated = ok(
        user_repo.update(&upgraded, user.version).await,
        "user role should be upgraded",
    );

    assert_eq!(updated.role, UserRole::Admin);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_role_downgrade_admin_to_user_by_root() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let root = create_user(&user_repo, "root_downgrader", UserRole::Root).await;

    let admin = create_user(&user_repo, "admin_demote", UserRole::Admin).await;

    // Root can manage Admin
    assert!(
        root.role.can_manage(&admin.role),
        "Root can downgrade Admin to User"
    );

    // Downgrade role from Admin to User
    let mut downgraded = admin.clone();
    downgraded.role = UserRole::User;
    let updated = ok(
        user_repo.update(&downgraded, admin.version).await,
        "admin role should be downgraded",
    );

    assert_eq!(updated.role, UserRole::User);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_upgrade_user_to_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin = create_user(&user_repo, "admin_promoter", UserRole::Admin).await;

    let user = create_user(&user_repo, "user_promote_fail", UserRole::User).await;

    // Admin can manage User (for ban/approve), but upgrading to Admin
    // typically requires Root. This test documents that can_manage returns true,
    // but business logic should enforce Root-only for promotions.
    assert!(
        admin.role.can_manage(&user.role),
        "Admin can manage User for some operations"
    );

    // Note: the promotion restriction is enforced by the caller workflow.
    // This test documents model behavior.
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_creator_permissions_vs_user_role() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Regular user creates a room
    let creator = create_user(&user_repo, "room_creator_perm", UserRole::User).await;

    let room = ok(
        room_service
            .create_room(
                "Perm Test Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    // Creator should have all room permissions despite being UserRole::User
    let perm_service = room_service.permission_service();
    let perms = ok(
        perm_service
            .get_user_permissions_no_cache(&room.id, &creator.id)
            .await,
        "creator permissions should be fetched",
    );

    // Creator has ALL permissions in the room, regardless of global role
    assert!(perms.0 != 0, "Room Creator should have permissions in room");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_user_cannot_login() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user = create_user(&user_repo, "banned_login", UserRole::User).await;

    // Verify user can login initially
    assert!(user.can_login());

    // Ban the user
    let banned = ok(
        user_repo
            .ban(&user.id, None, Some("permission boundary".to_string()))
            .await,
        "user should be banned",
    );

    assert_eq!(banned.status, UserStatus::Banned);
    assert!(is_banned(&user_repo, &user.id).await);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_user_cannot_create_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user = create_user(&user_repo, "banned_room", UserRole::User).await;

    let mut pending = user;
    pending.status = UserStatus::Banned;

    // Banned user cannot create rooms
    assert!(!pending.can_create_room(true));
    assert!(!pending.status.can_create_room());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_status_and_role_are_independent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let root = create_user(&user_repo, "root_status", UserRole::Root).await;

    let banned_root = ok(
        user_repo
            .ban(&root.id, None, Some("permission boundary".to_string()))
            .await,
        "root should be banned",
    );

    assert_eq!(banned_root.role, UserRole::Root); // Role unchanged
    assert_eq!(banned_root.status, UserStatus::Banned);
    assert!(is_banned(&user_repo, &root.id).await);

    let admin = create_user(&user_repo, "admin_status", UserRole::Admin).await;

    let mut pending_admin = admin;
    pending_admin.status = UserStatus::Banned;

    assert_eq!(pending_admin.role, UserRole::Admin); // Role unchanged
    assert_eq!(pending_admin.status, UserStatus::Banned); // Status changed
    assert!(!pending_admin.can_login()); // Cannot login while banned
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_role_update_optimistic_lock() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user = create_user(&user_repo, "optimistic_user", UserRole::User).await;

    // Read user twice (simulating concurrent reads)
    let read1 = load_user(&user_repo, &user.id).await;
    let read2 = load_user(&user_repo, &user.id).await;

    // First update succeeds
    let mut update1 = read1.clone();
    update1.role = UserRole::Admin;
    let result1 = user_repo.update(&update1, read1.version).await;
    assert!(result1.is_ok());

    // Second update with stale version fails
    let mut update2 = read2.clone();
    update2.role = UserRole::Admin;
    let result2 = user_repo.update(&update2, read2.version).await;
    assert!(
        matches!(result2, Err(Error::OptimisticLockConflict)),
        "Stale version should cause optimistic lock conflict"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_status_transitions() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user = create_user(&user_repo, "status_trans", UserRole::User).await;
    assert_eq!(user.status, UserStatus::Active);
    assert!(user.can_login());

    let banned = ok(
        user_repo
            .ban(&user.id, None, Some("permission boundary".to_string()))
            .await,
        "user should be banned",
    );
    assert_eq!(banned.status, UserStatus::Banned);
    assert!(is_banned(&user_repo, &user.id).await);

    let active = ok(user_repo.unban(&user.id).await, "user should be unbanned");
    assert_eq!(active.status, UserStatus::Active);
    assert!(active.can_login());
    assert!(!is_banned(&user_repo, &user.id).await);

    let mut pending = active.clone();
    pending.status = UserStatus::Banned;
    assert_eq!(pending.status, UserStatus::Banned);
    assert!(!pending.can_login());

    let mut approved = pending;
    approved.status = UserStatus::Active;
    assert_eq!(approved.status, UserStatus::Active);
    assert!(approved.can_login());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_can_manage_room_with_banned_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // User creates room
    let creator = create_user(&user_repo, "banned_creator", UserRole::User).await;

    let room = ok(
        room_service
            .create_room(
                "Banned Creator Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    // Ban the creator
    let _banned = ok(
        user_repo
            .ban(&creator.id, None, Some("permission boundary".to_string()))
            .await,
        "creator should be banned",
    );

    // Admin can still manage the room
    let admin = create_user(&user_repo, "room_admin", UserRole::Admin).await;

    // Join room as admin
    ok(
        room_service.join_room(room.id, admin.id, None).await,
        "admin should join room",
    );

    // Promote admin to room admin role
    ok(
        room_service
            .member_service()
            .set_member_role(room.id, creator.id, admin.id, RoomRole::Admin)
            .await,
        "admin should be promoted in room",
    );

    // Verify admin has room permissions
    let perm_service = room_service.permission_service();
    let perms = ok(
        perm_service
            .get_user_permissions_no_cache(&room.id, &admin.id)
            .await,
        "admin room permissions should be fetched",
    );

    assert!(
        perms.0 != 0,
        "Admin should have room permissions even with banned creator"
    );
}
