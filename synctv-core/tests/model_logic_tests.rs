//! Unit tests for pure model logic (no Docker/database needed)
//!
//! Covers: `UserRole`, `UserStatus`, User permission checks, `RoomStatus`,
//! `RoomPermissionSet`, `RoomRole`, room permission calculations.
//!
//! Run with: cargo test --test `model_logic_tests`
#![allow(clippy::unwrap_used)]

use synctv_core::models::room::RoomSettingsJson;
use synctv_core::models::user::{SignupMethod, User, UserRole, UserStatus};
use synctv_core::models::{
    RoomAdminPermissionBits, RoomMemberPermissionBits, RoomPermission, RoomPermissionSet, RoomRole,
    RoomStatus,
};

// UserRole tests

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
fn test_user_role_from_str_case_insensitive() {
    assert_eq!("ROOT".parse::<UserRole>().unwrap(), UserRole::Root);
    assert_eq!("Admin".parse::<UserRole>().unwrap(), UserRole::Admin);
    assert_eq!("USER".parse::<UserRole>().unwrap(), UserRole::User);
    assert!("unknown".parse::<UserRole>().is_err());
}

// UserStatus tests

#[test]
fn test_user_status_can_login() {
    assert!(UserStatus::Active.can_login());
    assert!(!UserStatus::Banned.can_login());
}

#[test]
fn test_user_status_can_create_room() {
    assert!(UserStatus::Active.can_create_room());
    assert!(!UserStatus::Banned.can_create_room());
}

#[test]
fn test_user_status_predicates() {
    assert!(UserStatus::Active.is_active());
    assert!(!UserStatus::Active.is_banned());
    assert!(!UserStatus::Banned.is_active());
    assert!(UserStatus::Banned.is_banned());
}

#[test]
fn test_user_status_from_str_roundtrip() {
    for status in [UserStatus::Active, UserStatus::Banned] {
        let s = status.as_str();
        let parsed: UserStatus = s.parse().unwrap();
        assert_eq!(parsed, status);
    }
    assert!("invalid".parse::<UserStatus>().is_err());
}

// SignupMethod tests

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

// User model logic tests

fn make_user(role: UserRole, status: UserStatus, signup_method: SignupMethod) -> User {
    let mut user = User::new_with_status("testuser".to_string(), signup_method, status);
    user.role = role;
    user
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

    // Banned admin: cannot create rooms (status blocks it)
    let user = make_user(UserRole::Admin, UserStatus::Banned, SignupMethod::Email);
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
    // OAuth2 signup users must keep at least one OAuth2 identity.
    assert!(!user.can_unbind_provider(1, false)); // Only 1 OAuth2 -> cannot unbind
    assert!(user.can_unbind_provider(2, false)); // 2 OAuth2 -> can unbind one
    assert!(!user.can_unbind_provider(1, true)); // Email does not replace signup OAuth2
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

// RoomStatus tests

#[test]
fn test_room_status_predicates() {
    assert!(RoomStatus::Active.is_active());
    assert!(!RoomStatus::Active.is_closed());

    assert!(!RoomStatus::Closed.is_active());
    assert!(RoomStatus::Closed.is_closed());
}

// RoomPermissionSet tests

#[test]
fn test_permission_bits_has_single() {
    let perms = RoomPermissionSet::new(
        RoomAdminPermissionBits::CHAT | RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE,
    );
    assert!(perms.has(RoomPermission::CHAT));
    assert!(perms.has(RoomPermission::CREATE_MEDIA_RESOURCE));
    assert!(!perms.has(RoomPermission::DELETE_MEDIA_RESOURCE_ANY));
}

#[test]
fn test_permission_bits_has_all() {
    let perms = RoomPermissionSet::new(
        RoomAdminPermissionBits::CHAT
            | RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
            | RoomAdminPermissionBits::PLAY_CONTROL,
    );
    assert!(perms.has_all(RoomPermissionSet::new(
        RoomAdminPermissionBits::CHAT | RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
    )));
    assert!(!perms.has_all(RoomPermissionSet::new(
        RoomAdminPermissionBits::CHAT | RoomAdminPermissionBits::DELETE_MEDIA_RESOURCE_ANY
    )));
}

#[test]
fn test_permission_bits_has_any() {
    let perms = RoomPermissionSet::new(RoomAdminPermissionBits::CHAT);
    assert!(perms.has_any(RoomPermissionSet::new(
        RoomAdminPermissionBits::CHAT | RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
    )));
    assert!(!perms.has_any(RoomPermissionSet::new(
        RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE | RoomAdminPermissionBits::PLAY_CONTROL
    )));
}

#[test]
fn test_permission_bits_grant_and_revoke() {
    let mut perms = RoomPermissionSet::empty();
    assert!(!perms.has(RoomPermission::CHAT));

    perms.grant(RoomPermission::CHAT);
    assert!(perms.has(RoomPermission::CHAT));

    perms.revoke(RoomPermission::CHAT);
    assert!(!perms.has(RoomPermission::CHAT));
}

#[test]
fn test_permission_bits_toggle() {
    let mut perms = RoomPermissionSet::empty();
    perms.toggle(RoomPermission::CHAT);
    assert!(perms.has(RoomPermission::CHAT));
    perms.toggle(RoomPermission::CHAT);
    assert!(!perms.has(RoomPermission::CHAT));
}

#[test]
fn test_permission_bits_set() {
    let mut perms = RoomPermissionSet::empty();
    perms.set(RoomPermission::CHAT, true);
    assert!(perms.has(RoomPermission::CHAT));
    perms.set(RoomPermission::CHAT, false);
    assert!(!perms.has(RoomPermission::CHAT));
}

// RoomRole tests

#[test]
fn test_room_role_permissions_hierarchy() {
    let creator = RoomRole::Creator.permissions();
    let admin = RoomRole::Admin.permissions();
    let member = RoomRole::Member.permissions();
    let guest = RoomRole::Guest.permissions();

    // Creator has all permissions
    assert_eq!(creator.0, RoomPermissionSet::all().0);

    // Admin is a superset of member
    assert!(admin.has_all(member));

    // Member is a superset of guest
    assert!(member.has_all(guest));
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

// Room permission calculation (effective_permissions_for_role)

#[test]
fn test_effective_permissions_no_overrides() {
    let global = RoomPermissionSet::default_member();
    let effective = RoomSettingsJson::effective_permissions_for_role(global, None, None);
    assert_eq!(effective, global);
}

#[test]
fn test_effective_permissions_add_only() {
    let global = RoomPermissionSet::default_member();
    let added = RoomAdminPermissionBits::PLAY_CONTROL;
    let effective = RoomSettingsJson::effective_permissions_for_role(global, Some(added), None);
    assert!(effective.has(RoomPermission::PLAY_CONTROL));
    assert!(effective.has(RoomPermission::CHAT)); // Original preserved
}

#[test]
fn test_effective_permissions_remove_only() {
    let global = RoomPermissionSet::default_member();
    let removed = RoomAdminPermissionBits::CHAT;
    let effective = RoomSettingsJson::effective_permissions_for_role(global, None, Some(removed));
    assert!(!effective.has(RoomPermission::CHAT)); // Removed
    assert!(effective.has(RoomPermission::CREATE_MEDIA_RESOURCE)); // Other preserved
}

#[test]
fn test_effective_permissions_add_and_remove() {
    let global = RoomPermissionSet::default_member();
    let added = RoomAdminPermissionBits::PLAY_CONTROL;
    let removed = RoomAdminPermissionBits::CHAT;
    let effective =
        RoomSettingsJson::effective_permissions_for_role(global, Some(added), Some(removed));
    assert!(effective.has(RoomPermission::PLAY_CONTROL)); // Added
    assert!(!effective.has(RoomPermission::CHAT)); // Removed
    assert!(effective.has(RoomPermission::CREATE_MEDIA_RESOURCE)); // Unchanged
}

#[test]
fn test_effective_permissions_remove_overrides_add() {
    // If the same bit is both added and removed, remove wins (applied second)
    let global = RoomPermissionSet::empty();
    let bit = RoomAdminPermissionBits::CHAT;
    let effective = RoomSettingsJson::effective_permissions_for_role(global, Some(bit), Some(bit));
    assert!(!effective.has(RoomPermission::CHAT));
}

#[test]
fn test_member_permissions_maps_member_bitspace() {
    let settings = RoomSettingsJson {
        member_added_permissions: Some(RoomMemberPermissionBits::USE_WEBRTC),
        member_removed_permissions: Some(RoomMemberPermissionBits::CHAT),
        ..RoomSettingsJson::default()
    };

    let effective = settings.member_permissions(RoomPermissionSet::default_member());
    assert!(effective.has(RoomPermission::USE_WEBRTC));
    assert!(!effective.has(RoomPermission::CHAT));
}
