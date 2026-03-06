//! Unit tests for pure model logic (no Docker/database needed)
//!
//! Covers: `UserRole`, `UserStatus`, User permission checks, `RoomStatus`,
//! `PermissionBits`, `RoomRole`, room permission calculations.
//!
//! Run with: cargo test --test `model_logic_tests`
#![allow(clippy::unwrap_used)]

use synctv_core::models::room::RoomSettingsJson;
use synctv_core::models::user::{SignupMethod, User, UserRole, UserStatus};
use synctv_core::models::{PermissionBits, RoomRole, RoomStatus};

// ============================================================================
// UserRole tests
// ============================================================================

#[test]
fn test_user_role_can_manage() {
    // Root can manage everyone
    assert!(UserRole::Root.can_manage(&UserRole::Root));
    assert!(UserRole::Root.can_manage(&UserRole::Admin));
    assert!(UserRole::Root.can_manage(&UserRole::User));

    // Admin can manage Users only
    assert!(!UserRole::Admin.can_manage(&UserRole::Root));
    assert!(!UserRole::Admin.can_manage(&UserRole::Admin));
    assert!(UserRole::Admin.can_manage(&UserRole::User));

    // User cannot manage anyone
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

#[test]
fn test_user_role_as_str_roundtrip() {
    for role in [UserRole::Root, UserRole::Admin, UserRole::User] {
        let s = role.as_str();
        let parsed: UserRole = s.parse().unwrap();
        assert_eq!(parsed, role);
    }
}

#[test]
fn test_user_role_from_str_case_insensitive() {
    assert_eq!("ROOT".parse::<UserRole>().unwrap(), UserRole::Root);
    assert_eq!("Admin".parse::<UserRole>().unwrap(), UserRole::Admin);
    assert_eq!("USER".parse::<UserRole>().unwrap(), UserRole::User);
    assert!("unknown".parse::<UserRole>().is_err());
}

// ============================================================================
// UserStatus tests
// ============================================================================

#[test]
fn test_user_status_can_login() {
    assert!(UserStatus::Active.can_login());
    assert!(!UserStatus::Pending.can_login());
    assert!(!UserStatus::Banned.can_login());
}

#[test]
fn test_user_status_can_create_room() {
    assert!(UserStatus::Active.can_create_room());
    assert!(!UserStatus::Pending.can_create_room());
    assert!(!UserStatus::Banned.can_create_room());
}

#[test]
fn test_user_status_predicates() {
    assert!(UserStatus::Active.is_active());
    assert!(!UserStatus::Active.is_pending());
    assert!(!UserStatus::Active.is_banned());

    assert!(!UserStatus::Pending.is_active());
    assert!(UserStatus::Pending.is_pending());

    assert!(!UserStatus::Banned.is_active());
    assert!(UserStatus::Banned.is_banned());
}

#[test]
fn test_user_status_from_str_roundtrip() {
    for status in [UserStatus::Active, UserStatus::Pending, UserStatus::Banned] {
        let s = status.as_str();
        let parsed: UserStatus = s.parse().unwrap();
        assert_eq!(parsed, status);
    }
    assert!("invalid".parse::<UserStatus>().is_err());
}

// ============================================================================
// SignupMethod tests
// ============================================================================

#[test]
fn test_signup_method_from_str_name() {
    assert_eq!(
        SignupMethod::from_str_name("unknown"),
        Some(SignupMethod::Unknown)
    );
    assert_eq!(
        SignupMethod::from_str_name("email"),
        Some(SignupMethod::Email)
    );
    assert_eq!(
        SignupMethod::from_str_name("password"),
        Some(SignupMethod::Password)
    );
    assert_eq!(
        SignupMethod::from_str_name("oauth2"),
        Some(SignupMethod::OAuth2)
    );
    assert_eq!(
        SignupMethod::from_str_name("admin_created"),
        Some(SignupMethod::AdminCreated)
    );
    assert_eq!(SignupMethod::from_str_name(""), None);
    assert_eq!(SignupMethod::from_str_name("invalid"), None);
}

#[test]
fn test_signup_method_as_str() {
    assert_eq!(SignupMethod::Unknown.as_str(), "unknown");
    assert_eq!(SignupMethod::Email.as_str(), "email");
    assert_eq!(SignupMethod::Password.as_str(), "password");
    assert_eq!(SignupMethod::OAuth2.as_str(), "oauth2");
    assert_eq!(SignupMethod::AdminCreated.as_str(), "admin_created");
}

// ============================================================================
// User model logic tests
// ============================================================================

fn make_user(role: UserRole, status: UserStatus, signup_method: SignupMethod) -> User {
    let mut user = User::new_with_status(
        "testuser".to_string(),
        Some("test@example.com".to_string()),
        "hash".to_string(),
        signup_method,
        status,
    );
    user.role = role;
    user
}

#[test]
fn test_user_new_defaults() {
    let user = User::new(
        "alice".to_string(),
        Some("alice@example.com".to_string()),
        "hash".to_string(),
        SignupMethod::Email,
    );
    assert_eq!(user.role, UserRole::User);
    assert_eq!(user.status, UserStatus::Pending);
    assert!(!user.email_verified);
    assert_eq!(user.password_version, 0);
    assert_eq!(user.version, 0);
    assert!(!user.is_deleted());
}

#[test]
fn test_user_can_create_room_role_and_status_interaction() {
    // Active admin: always can create rooms
    let mut user = make_user(UserRole::Admin, UserStatus::Active, SignupMethod::Email);
    assert!(user.can_create_room(false));
    assert!(user.can_create_room(true));

    // Active root: always can create rooms
    user.role = UserRole::Root;
    assert!(user.can_create_room(false));

    // Active user: depends on allow_user flag
    user.role = UserRole::User;
    assert!(!user.can_create_room(false));
    assert!(user.can_create_room(true));

    // Banned user: never can create rooms
    user.status = UserStatus::Banned;
    assert!(!user.can_create_room(true));

    // Pending admin: cannot create rooms (status blocks it)
    let user = make_user(UserRole::Admin, UserStatus::Pending, SignupMethod::Email);
    assert!(!user.can_create_room(true));
}

#[test]
fn test_user_can_unbind_provider_email_signup() {
    let user = make_user(UserRole::User, UserStatus::Active, SignupMethod::Email);
    // Email users can always unbind OAuth2 providers (they still have email)
    assert!(user.can_unbind_provider(0, true));
    assert!(user.can_unbind_provider(1, true));
    assert!(user.can_unbind_provider(0, false));
}

#[test]
fn test_user_can_unbind_provider_oauth2_signup() {
    let user = make_user(UserRole::User, UserStatus::Active, SignupMethod::OAuth2);
    // OAuth2 user needs at least one OAuth2 or has email
    assert!(!user.can_unbind_provider(1, false)); // Only 1 OAuth2, no email -> cannot unbind
    assert!(user.can_unbind_provider(2, false)); // 2 OAuth2 -> can unbind one
    assert!(user.can_unbind_provider(1, true)); // 1 OAuth2 + email -> can unbind
}

#[test]
fn test_user_can_unbind_provider_email_user_always_can() {
    let user = make_user(UserRole::User, UserStatus::Active, SignupMethod::Email);
    // Email users can always unbind providers
    assert!(user.can_unbind_provider(0, false));
    assert!(user.can_unbind_provider(1, false));
    assert!(user.can_unbind_provider(2, false));
    assert!(user.can_unbind_provider(0, true));
}

#[test]
fn test_user_role_predicates() {
    let mut user = make_user(UserRole::Root, UserStatus::Active, SignupMethod::Email);
    assert!(user.is_root());
    assert!(!user.is_admin());
    assert!(user.is_admin_or_above());

    user.role = UserRole::Admin;
    assert!(!user.is_root());
    assert!(user.is_admin());
    assert!(user.is_admin_or_above());

    user.role = UserRole::User;
    assert!(!user.is_root());
    assert!(!user.is_admin());
    assert!(!user.is_admin_or_above());
}

// ============================================================================
// RoomStatus tests
// ============================================================================

#[test]
fn test_room_status_predicates() {
    assert!(RoomStatus::Active.is_active());
    assert!(!RoomStatus::Active.is_pending());
    assert!(!RoomStatus::Active.is_closed());

    assert!(!RoomStatus::Pending.is_active());
    assert!(RoomStatus::Pending.is_pending());
    assert!(!RoomStatus::Pending.is_closed());

    assert!(!RoomStatus::Closed.is_active());
    assert!(!RoomStatus::Closed.is_pending());
    assert!(RoomStatus::Closed.is_closed());
}

#[test]
fn test_room_status_default() {
    assert_eq!(RoomStatus::default(), RoomStatus::Active);
}

// ============================================================================
// PermissionBits tests
// ============================================================================

#[test]
fn test_permission_bits_has_single() {
    let perms = PermissionBits::new(PermissionBits::SEND_CHAT | PermissionBits::ADD_MOVIE);
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::ADD_MOVIE));
    assert!(!perms.has(PermissionBits::DELETE_MOVIE_ANY));
}

#[test]
fn test_permission_bits_has_all() {
    let perms = PermissionBits::new(
        PermissionBits::SEND_CHAT | PermissionBits::ADD_MOVIE | PermissionBits::PLAY_CONTROL,
    );
    assert!(perms.has_all(PermissionBits::SEND_CHAT | PermissionBits::ADD_MOVIE));
    assert!(!perms.has_all(PermissionBits::SEND_CHAT | PermissionBits::DELETE_MOVIE_ANY));
}

#[test]
fn test_permission_bits_has_any() {
    let perms = PermissionBits::new(PermissionBits::SEND_CHAT);
    assert!(perms.has_any(PermissionBits::SEND_CHAT | PermissionBits::ADD_MOVIE));
    assert!(!perms.has_any(PermissionBits::ADD_MOVIE | PermissionBits::PLAY_CONTROL));
}

#[test]
fn test_permission_bits_grant_and_revoke() {
    let mut perms = PermissionBits::empty();
    assert!(!perms.has(PermissionBits::SEND_CHAT));

    perms.grant(PermissionBits::SEND_CHAT);
    assert!(perms.has(PermissionBits::SEND_CHAT));

    perms.revoke(PermissionBits::SEND_CHAT);
    assert!(!perms.has(PermissionBits::SEND_CHAT));
}

#[test]
fn test_permission_bits_toggle() {
    let mut perms = PermissionBits::empty();
    perms.toggle(PermissionBits::SEND_CHAT);
    assert!(perms.has(PermissionBits::SEND_CHAT));
    perms.toggle(PermissionBits::SEND_CHAT);
    assert!(!perms.has(PermissionBits::SEND_CHAT));
}

#[test]
fn test_permission_bits_set() {
    let mut perms = PermissionBits::empty();
    perms.set(PermissionBits::SEND_CHAT, true);
    assert!(perms.has(PermissionBits::SEND_CHAT));
    perms.set(PermissionBits::SEND_CHAT, false);
    assert!(!perms.has(PermissionBits::SEND_CHAT));
}

#[test]
fn test_permission_bits_defaults() {
    // Default member permissions should include basic capabilities
    let member = PermissionBits::new(PermissionBits::DEFAULT_MEMBER);
    assert!(member.has(PermissionBits::SEND_CHAT));
    assert!(member.has(PermissionBits::ADD_MOVIE));
    assert!(member.has(PermissionBits::VIEW_PLAYLIST));
    assert!(!member.has(PermissionBits::KICK_MEMBER));
    assert!(!member.has(PermissionBits::DELETE_ROOM));

    // Default admin permissions should be a superset of member
    let admin = PermissionBits::new(PermissionBits::DEFAULT_ADMIN);
    assert!(admin.has(PermissionBits::SEND_CHAT));
    assert!(admin.has(PermissionBits::KICK_MEMBER));
    assert!(admin.has(PermissionBits::BAN_MEMBER));
    assert!(admin.has(PermissionBits::PLAY_CONTROL));
    assert!(!admin.has(PermissionBits::DELETE_ROOM)); // Only creator

    // Default guest permissions should be minimal
    let guest = PermissionBits::new(PermissionBits::DEFAULT_GUEST);
    assert!(guest.has(PermissionBits::VIEW_PLAYLIST));
    assert!(!guest.has(PermissionBits::SEND_CHAT));
    assert!(!guest.has(PermissionBits::ADD_MOVIE));
}

// ============================================================================
// RoomRole tests
// ============================================================================

#[test]
fn test_room_role_permissions_hierarchy() {
    let creator = RoomRole::Creator.permissions();
    let admin = RoomRole::Admin.permissions();
    let member = RoomRole::Member.permissions();
    let guest = RoomRole::Guest.permissions();

    // Creator has all permissions
    assert_eq!(creator.0, u64::MAX);

    // Admin is a superset of member
    assert!(admin.has_all(member.0));

    // Member is a superset of guest
    assert!(member.has_all(guest.0));
}

#[test]
fn test_room_role_from_str_roundtrip() {
    for role in [
        RoomRole::Creator,
        RoomRole::Admin,
        RoomRole::Member,
        RoomRole::Guest,
    ] {
        let s = role.to_string();
        let parsed: RoomRole = s.parse().unwrap();
        assert_eq!(parsed, role);
    }
    assert!("invalid_role".parse::<RoomRole>().is_err());
}

// ============================================================================
// Room permission calculation (effective_permissions_for_role)
// ============================================================================

#[test]
fn test_effective_permissions_no_overrides() {
    let global = PermissionBits::new(PermissionBits::DEFAULT_MEMBER);
    let effective = RoomSettingsJson::effective_permissions_for_role(global, None, None);
    assert_eq!(effective, global);
}

#[test]
fn test_effective_permissions_add_only() {
    let global = PermissionBits::new(PermissionBits::DEFAULT_MEMBER);
    let added = PermissionBits::PLAY_CONTROL;
    let effective = RoomSettingsJson::effective_permissions_for_role(global, Some(added), None);
    assert!(effective.has(PermissionBits::PLAY_CONTROL));
    assert!(effective.has(PermissionBits::SEND_CHAT)); // Original preserved
}

#[test]
fn test_effective_permissions_remove_only() {
    let global = PermissionBits::new(PermissionBits::DEFAULT_MEMBER);
    let removed = PermissionBits::SEND_CHAT;
    let effective = RoomSettingsJson::effective_permissions_for_role(global, None, Some(removed));
    assert!(!effective.has(PermissionBits::SEND_CHAT)); // Removed
    assert!(effective.has(PermissionBits::ADD_MOVIE)); // Other preserved
}

#[test]
fn test_effective_permissions_add_and_remove() {
    let global = PermissionBits::new(PermissionBits::DEFAULT_MEMBER);
    let added = PermissionBits::PLAY_CONTROL;
    let removed = PermissionBits::SEND_CHAT;
    let effective =
        RoomSettingsJson::effective_permissions_for_role(global, Some(added), Some(removed));
    assert!(effective.has(PermissionBits::PLAY_CONTROL)); // Added
    assert!(!effective.has(PermissionBits::SEND_CHAT)); // Removed
    assert!(effective.has(PermissionBits::ADD_MOVIE)); // Unchanged
}

#[test]
fn test_effective_permissions_remove_overrides_add() {
    // If the same bit is both added and removed, remove wins (applied second)
    let global = PermissionBits::empty();
    let bit = PermissionBits::SEND_CHAT;
    let effective = RoomSettingsJson::effective_permissions_for_role(global, Some(bit), Some(bit));
    assert!(!effective.has(PermissionBits::SEND_CHAT));
}
