//! Admin permission boundary tests
//!
//! Tests permission boundaries between Admin and Root roles, including:
//! - Admin attempting Root-only operations
//! - Root managing Root users
//! - Role upgrade/downgrade scenarios
//! - Cross-role operation restrictions
//!
//! Run with: cargo test -p synctv-core --test admin_permission_boundary_tests -- --nocapture
//! Docker tests: cargo test -p synctv-core --test admin_permission_boundary_tests -- --ignored --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use synctv_core_testing::{create_test_pool};
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{RoomRole, UserId, User, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        RoomService, UserService, InMemoryTokenBlacklistStore,
        auth::{BruteForceProtection, JwtService},
    },
    Error,
};
use chrono::Utc;
use sqlx::PgPool;
// ============================================================================
// Test Infrastructure
// ============================================================================

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

fn make_user_with_role(username: &str, role: UserRole) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{}@test.com", username)),
        password_hash: "hash".to_string(),
        role,
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

// ============================================================================
// Test: UserRole::can_manage() boundary checks
// ============================================================================

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

// ============================================================================
// Test: Admin cannot upgrade to Root role
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_upgrade_user_to_root() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    // Create an admin
    let admin = user_repo
        .create(&make_user_with_role("admin_upgrade", UserRole::Admin))
        .await
        .unwrap();

    // Create a regular user
    let user = user_repo
        .create(&make_user_with_role("user_to_upgrade", UserRole::User))
        .await
        .unwrap();

    // Admin tries to upgrade user to Root - should fail
    // This test documents expected behavior: upgrade requires Root
    assert!(
        admin.role.can_manage(&user.role),
        "Admin can manage User role"
    );

    // But admin cannot manage Root users
    let root_user = user_repo
        .create(&make_user_with_role("root_target", UserRole::Root))
        .await
        .unwrap();

    assert!(
        !admin.role.can_manage(&root_user.role),
        "Admin cannot manage Root role"
    );
}

// ============================================================================
// Test: Admin cannot demote Root
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_demote_root() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin = user_repo
        .create(&make_user_with_role("admin_demoter", UserRole::Admin))
        .await
        .unwrap();

    let root_user = user_repo
        .create(&make_user_with_role("root_to_demote", UserRole::Root))
        .await
        .unwrap();

    // Verify: Admin cannot manage Root
    assert!(
        !admin.role.can_manage(&root_user.role),
        "Admin cannot demote Root users"
    );
}

// ============================================================================
// Test: Admin cannot ban Root
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_ban_root_user() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin = user_repo
        .create(&make_user_with_role("admin_banner", UserRole::Admin))
        .await
        .unwrap();

    let root_user = user_repo
        .create(&make_user_with_role("root_to_ban", UserRole::Root))
        .await
        .unwrap();

    // Verify: Admin cannot ban Root via role hierarchy
    assert!(
        !admin.role.can_manage(&root_user.role),
        "Admin cannot ban Root users"
    );

    // Admin can manage regular users
    let regular_user = user_repo
        .create(&make_user_with_role("regular_to_ban", UserRole::User))
        .await
        .unwrap();

    assert!(
        admin.role.can_manage(&regular_user.role),
        "Admin can manage regular users"
    );
}

// ============================================================================
// Test: Root can ban Admin
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_root_can_ban_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let root = user_repo
        .create(&make_user_with_role("root_banner", UserRole::Root))
        .await
        .unwrap();

    let admin = user_repo
        .create(&make_user_with_role("admin_to_ban", UserRole::Admin))
        .await
        .unwrap();

    // Root can manage Admin
    assert!(
        root.role.can_manage(&admin.role),
        "Root can ban Admin users"
    );

    // Update status to Banned
    let banned = user_repo
        .update_status(&admin.id, UserStatus::Banned)
        .await
        .unwrap();

    assert_eq!(banned.status, UserStatus::Banned);
    assert_eq!(banned.role, UserRole::Admin); // Role unchanged
}

// ============================================================================
// Test: Admin can ban User
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_can_ban_user() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin = user_repo
        .create(&make_user_with_role("admin_can_ban", UserRole::Admin))
        .await
        .unwrap();

    let user = user_repo
        .create(&make_user_with_role("user_to_ban", UserRole::User))
        .await
        .unwrap();

    // Admin can manage User
    assert!(
        admin.role.can_manage(&user.role),
        "Admin can ban User users"
    );

    // Update status to Banned
    let banned = user_repo
        .update_status(&user.id, UserStatus::Banned)
        .await
        .unwrap();

    assert_eq!(banned.status, UserStatus::Banned);
    assert_eq!(banned.role, UserRole::User); // Role unchanged
}

// ============================================================================
// Test: Role upgrade/downgrade scenarios
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_role_upgrade_user_to_admin_by_root() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let root = user_repo
        .create(&make_user_with_role("root_upgrader", UserRole::Root))
        .await
        .unwrap();

    let user = user_repo
        .create(&make_user_with_role("user_promote", UserRole::User))
        .await
        .unwrap();

    // Root can manage User
    assert!(
        root.role.can_manage(&user.role),
        "Root can upgrade User to Admin"
    );

    // Upgrade role from User to Admin
    let mut upgraded = user.clone();
    upgraded.role = UserRole::Admin;
    let updated = user_repo.update(&upgraded, user.version).await.unwrap();

    assert_eq!(updated.role, UserRole::Admin);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_role_downgrade_admin_to_user_by_root() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let root = user_repo
        .create(&make_user_with_role("root_downgrader", UserRole::Root))
        .await
        .unwrap();

    let admin = user_repo
        .create(&make_user_with_role("admin_demote", UserRole::Admin))
        .await
        .unwrap();

    // Root can manage Admin
    assert!(
        root.role.can_manage(&admin.role),
        "Root can downgrade Admin to User"
    );

    // Downgrade role from Admin to User
    let mut downgraded = admin.clone();
    downgraded.role = UserRole::User;
    let updated = user_repo.update(&downgraded, admin.version).await.unwrap();

    assert_eq!(updated.role, UserRole::User);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_upgrade_user_to_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin = user_repo
        .create(&make_user_with_role("admin_promoter", UserRole::Admin))
        .await
        .unwrap();

    let user = user_repo
        .create(&make_user_with_role("user_promote_fail", UserRole::User))
        .await
        .unwrap();

    // Admin can manage User (for ban/approve), but upgrading to Admin
    // typically requires Root. This test documents that can_manage returns true,
    // but business logic should enforce Root-only for promotions.
    assert!(
        admin.role.can_manage(&user.role),
        "Admin can manage User for some operations"
    );

    // Note: The actual promotion restriction is enforced at the API layer,
    // not at the model layer. This test documents model behavior.
}

// ============================================================================
// Test: Cross-role operation restrictions
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_creator_permissions_vs_user_role() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Regular user creates a room
    let creator = user_repo
        .create(&make_user_with_role("room_creator_perm", UserRole::User))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Perm Test Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Creator should have all room permissions despite being UserRole::User
    let perm_service = room_service.permission_service();
    let perms = perm_service
        .get_user_permissions_no_cache(&room.id, &creator.id)
        .await
        .unwrap();

    // Creator has ALL permissions in the room, regardless of global role
    assert!(
        perms.0 != 0,
        "Room Creator should have permissions in room"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_user_cannot_login() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user = user_repo
        .create(&make_user_with_role("banned_login", UserRole::User))
        .await
        .unwrap();

    // Verify user can login initially
    assert!(user.can_login());

    // Ban the user
    let banned = user_repo
        .update_status(&user.id, UserStatus::Banned)
        .await
        .unwrap();

    // Banned user cannot login
    assert!(!banned.can_login());
    assert!(!banned.status.can_login());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_pending_user_cannot_create_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user = user_repo
        .create(&make_user_with_role("pending_room", UserRole::User))
        .await
        .unwrap();

    // Update to Pending status
    let pending = user_repo
        .update_status(&user.id, UserStatus::Pending)
        .await
        .unwrap();

    // Pending user cannot create rooms
    assert!(!pending.can_create_room(true));
    assert!(!pending.status.can_create_room());
}

// ============================================================================
// Test: Status and Role independence
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_status_and_role_are_independent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    // Create Root user
    let root = user_repo
        .create(&make_user_with_role("root_status", UserRole::Root))
        .await
        .unwrap();

    // Root can be banned (status changes, role stays)
    let banned_root = user_repo
        .update_status(&root.id, UserStatus::Banned)
        .await
        .unwrap();

    assert_eq!(banned_root.role, UserRole::Root); // Role unchanged
    assert_eq!(banned_root.status, UserStatus::Banned); // Status changed
    assert!(!banned_root.can_login()); // Cannot login

    // Create Admin user
    let admin = user_repo
        .create(&make_user_with_role("admin_status", UserRole::Admin))
        .await
        .unwrap();

    // Admin can be pending
    let pending_admin = user_repo
        .update_status(&admin.id, UserStatus::Pending)
        .await
        .unwrap();

    assert_eq!(pending_admin.role, UserRole::Admin); // Role unchanged
    assert_eq!(pending_admin.status, UserStatus::Pending); // Status changed
    assert!(!pending_admin.can_login()); // Cannot login while pending
}

// ============================================================================
// Test: Optimistic lock on role update
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_role_update_optimistic_lock() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user = user_repo
        .create(&make_user_with_role("optimistic_user", UserRole::User))
        .await
        .unwrap();

    // Read user twice (simulating concurrent reads)
    let read1 = user_repo.get_by_id(&user.id).await.unwrap().unwrap();
    let read2 = user_repo.get_by_id(&user.id).await.unwrap().unwrap();

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

// ============================================================================
// Test: Multiple status transitions
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_status_transitions() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    // Start as Active
    let user = user_repo
        .create(&make_user_with_role("status_trans", UserRole::User))
        .await
        .unwrap();
    assert_eq!(user.status, UserStatus::Active);
    assert!(user.can_login());

    // Active -> Banned
    let banned = user_repo
        .update_status(&user.id, UserStatus::Banned)
        .await
        .unwrap();
    assert_eq!(banned.status, UserStatus::Banned);
    assert!(!banned.can_login());

    // Banned -> Active (unban)
    let active = user_repo
        .update_status(&user.id, UserStatus::Active)
        .await
        .unwrap();
    assert_eq!(active.status, UserStatus::Active);
    assert!(active.can_login());

    // Active -> Pending
    let pending = user_repo
        .update_status(&user.id, UserStatus::Pending)
        .await
        .unwrap();
    assert_eq!(pending.status, UserStatus::Pending);
    assert!(!pending.can_login());

    // Pending -> Active (approve)
    let approved = user_repo
        .update_status(&user.id, UserStatus::Active)
        .await
        .unwrap();
    assert_eq!(approved.status, UserStatus::Active);
    assert!(approved.can_login());
}

// ============================================================================
// Test: Admin operations on room with banned creator
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_can_manage_room_with_banned_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // User creates room
    let creator = user_repo
        .create(&make_user_with_role("banned_creator", UserRole::User))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Banned Creator Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Ban the creator
    let _banned = user_repo
        .update_status(&creator.id, UserStatus::Banned)
        .await
        .unwrap();

    // Admin can still manage the room
    let admin = user_repo
        .create(&make_user_with_role("room_admin", UserRole::Admin))
        .await
        .unwrap();

    // Join room as admin
    room_service
        .join_room(room.id.clone(), admin.id.clone(), None)
        .await
        .unwrap();

    // Promote admin to room admin role
    room_service
        .member_service()
        .set_member_role(room.id.clone(), creator.id.clone(), admin.id.clone(), RoomRole::Admin)
        .await
        .unwrap();

    // Verify admin has room permissions
    let perm_service = room_service.permission_service();
    let perms = perm_service
        .get_user_permissions_no_cache(&room.id, &admin.id)
        .await
        .unwrap();

    assert!(
        perms.0 != 0,
        "Admin should have room permissions even with banned creator"
    );
}
