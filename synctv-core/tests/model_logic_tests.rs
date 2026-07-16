//! Unit tests for pure model logic (no Docker/database needed)

use synctv_core::models::user::{SignupMethod, User, UserRole, UserStatus};
use synctv_core::models::{
    room_settings::{
        AdminAddedPermissions, AdminRemovedPermissions, MemberAddedPermissions,
        MemberRemovedPermissions,
    },
    RoomAdminPermissionBits, RoomMemberPermissionBits, RoomPermission, RoomPermissionSet, RoomRole,
    RoomSettings, RoomStatus,
};

fn ok<T, E: std::fmt::Debug>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error:?}")),
    }
}

#[test]
fn test_user_role_can_manage() {
    assert!(UserRole::Root.can_manage(&UserRole::Root));
    assert!(UserRole::Root.can_manage(&UserRole::Admin));
    assert!(UserRole::Root.can_manage(&UserRole::User));

    assert!(!UserRole::Admin.can_manage(&UserRole::Root));
    assert!(!UserRole::Admin.can_manage(&UserRole::Admin));
    assert!(UserRole::Admin.can_manage(&UserRole::User));

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
    assert_eq!(ok("ROOT".parse::<UserRole>(), "ROOT role"), UserRole::Root);
    assert_eq!(
        ok("Admin".parse::<UserRole>(), "Admin role"),
        UserRole::Admin
    );
    assert_eq!(ok("USER".parse::<UserRole>(), "USER role"), UserRole::User);
    assert!("unknown".parse::<UserRole>().is_err());
}

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
fn test_user_status_from_str_accepts_canonical_values_case_insensitively() {
    assert_eq!(
        ok("active".parse::<UserStatus>(), "active status"),
        UserStatus::Active
    );
    assert_eq!(
        ok("BANNED".parse::<UserStatus>(), "BANNED status"),
        UserStatus::Banned
    );
    assert!("invalid".parse::<UserStatus>().is_err());
}

fn make_user(role: UserRole, status: UserStatus, signup_method: SignupMethod) -> User {
    let mut user = User::new_with_status("testuser".to_string(), signup_method, status);
    user.role = role;
    user
}

#[test]
fn test_user_can_create_room_role_and_status_interaction() {
    let mut user = make_user(UserRole::Admin, UserStatus::Active, SignupMethod::Email);
    assert!(user.can_create_room(false));
    assert!(user.can_create_room(true));

    user.role = UserRole::Root;
    assert!(user.can_create_room(false));

    user.role = UserRole::User;
    assert!(!user.can_create_room(false));
    assert!(user.can_create_room(true));

    let user = make_user(UserRole::Admin, UserStatus::Banned, SignupMethod::Email);
    assert!(!user.can_create_room(true));
}

#[test]
fn test_user_can_unbind_provider_email_signup() {
    let user = make_user(UserRole::User, UserStatus::Active, SignupMethod::Email);
    assert!(user.can_unbind_provider(0, true));
    assert!(user.can_unbind_provider(1, true));
    assert!(user.can_unbind_provider(0, false));
}

#[test]
fn test_user_can_unbind_provider_oauth2_signup() {
    let user = make_user(UserRole::User, UserStatus::Active, SignupMethod::OAuth2);
    assert!(!user.can_unbind_provider(1, false));
    assert!(user.can_unbind_provider(2, false));
    assert!(!user.can_unbind_provider(1, true));
}

#[test]
fn test_user_can_unbind_provider_email_user_always_can() {
    let user = make_user(UserRole::User, UserStatus::Active, SignupMethod::Email);
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

#[test]
fn test_room_status_predicates() {
    assert!(RoomStatus::Active.is_active());
    assert!(!RoomStatus::Active.is_closed());

    assert!(!RoomStatus::Closed.is_active());
    assert!(RoomStatus::Closed.is_closed());
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

#[test]
fn test_room_role_permissions_hierarchy() {
    let creator = RoomRole::Creator.permissions();
    let admin = RoomRole::Admin.permissions();
    let member = RoomRole::Member.permissions();
    let guest = RoomRole::Guest.permissions();

    assert_eq!(creator.0, RoomPermissionSet::all().0);

    assert!(admin.has_all(member));

    assert!(member.has_all(guest));
}

#[test]
fn test_room_role_from_str_accepts_canonical_values_case_insensitively() {
    assert_eq!(
        ok("creator".parse::<RoomRole>(), "creator room role"),
        RoomRole::Creator
    );
    assert_eq!(
        ok("ADMIN".parse::<RoomRole>(), "ADMIN room role"),
        RoomRole::Admin
    );
    assert_eq!(
        ok("member".parse::<RoomRole>(), "member room role"),
        RoomRole::Member
    );
    assert_eq!(
        ok("Guest".parse::<RoomRole>(), "Guest room role"),
        RoomRole::Guest
    );
    assert!("invalid_role".parse::<RoomRole>().is_err());
}

#[test]
fn test_effective_permissions_no_overrides() {
    let global = RoomPermissionSet::default_member();
    let effective = RoomSettings::default().admin_permissions(global);
    assert_eq!(effective, global);
}

#[test]
fn test_effective_permissions_add_only() {
    let global = RoomPermissionSet::default_member();
    let added = RoomAdminPermissionBits::PLAY_CONTROL;
    let settings = RoomSettings {
        admin_added_permissions: AdminAddedPermissions(added),
        ..RoomSettings::default()
    };
    let effective = settings.admin_permissions(global);
    assert!(effective.has(RoomPermission::PLAY_CONTROL));
    assert!(effective.has(RoomPermission::CHAT)); // Original preserved
}

#[test]
fn test_effective_permissions_remove_only() {
    let global = RoomPermissionSet::default_member();
    let removed = RoomAdminPermissionBits::CHAT;
    let settings = RoomSettings {
        admin_removed_permissions: AdminRemovedPermissions(removed),
        ..RoomSettings::default()
    };
    let effective = settings.admin_permissions(global);
    assert!(!effective.has(RoomPermission::CHAT));
    assert!(effective.has(RoomPermission::CREATE_MEDIA_RESOURCE));
}

#[test]
fn test_effective_permissions_add_and_remove() {
    let global = RoomPermissionSet::default_member();
    let added = RoomAdminPermissionBits::PLAY_CONTROL;
    let removed = RoomAdminPermissionBits::CHAT;
    let settings = RoomSettings {
        admin_added_permissions: AdminAddedPermissions(added),
        admin_removed_permissions: AdminRemovedPermissions(removed),
        ..RoomSettings::default()
    };
    let effective = settings.admin_permissions(global);
    assert!(effective.has(RoomPermission::PLAY_CONTROL));
    assert!(!effective.has(RoomPermission::CHAT));
    assert!(effective.has(RoomPermission::CREATE_MEDIA_RESOURCE));
}

#[test]
fn test_effective_permissions_remove_overrides_add() {
    let global = RoomPermissionSet::empty();
    let bit = RoomAdminPermissionBits::CHAT;
    let settings = RoomSettings {
        admin_added_permissions: AdminAddedPermissions(bit),
        admin_removed_permissions: AdminRemovedPermissions(bit),
        ..RoomSettings::default()
    };
    let effective = settings.admin_permissions(global);
    assert!(!effective.has(RoomPermission::CHAT));
}

#[test]
fn test_member_permissions_maps_member_bitspace() {
    let settings = RoomSettings {
        member_added_permissions: MemberAddedPermissions(RoomMemberPermissionBits::USE_WEBRTC),
        member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::CHAT),
        ..RoomSettings::default()
    };

    let effective = settings.member_permissions(RoomPermissionSet::default_member());
    assert!(effective.has(RoomPermission::USE_WEBRTC));
    assert!(!effective.has(RoomPermission::CHAT));
}
