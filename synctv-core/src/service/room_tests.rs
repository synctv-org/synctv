use super::member_role_policy::validate_override_bits_for_role;
use crate::models::{
    room_settings::{GuestAddedPermissions, MemberAddedPermissions},
    RoomGuestPermissionBits, RoomId, RoomMember, RoomMemberPermissionBits, RoomPermissionSet,
    RoomRole, RoomSettings, RoomStatus, UserId,
};
use crate::service::with_coordination_lock;
use crate::test_helpers::RoomFixture;
use crate::Error;
use async_trait::async_trait;

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
    match result {
        Ok(_) => std::panic::panic_any(context.to_string()),
        Err(error) => error,
    }
}

fn validate_room_settings(settings: &RoomSettings) -> crate::Result<()> {
    crate::models::SettingsValidationContext::with_strict_policy(|ctx| settings.validate(ctx))
}

fn validate_room_name(name: &str) -> crate::Result<()> {
    crate::validation::RoomNameValidator::new()
        .validate(name)
        .map_err(|e| Error::InvalidInput(e.to_string()))
}

#[test]
fn test_empty_room_name_returns_error() {
    let result = validate_room_name("");
    assert!(result.is_err());
    match err(result, "empty room name should fail") {
        Error::InvalidInput(msg) => assert!(
            msg.contains("at least 1") || msg.contains("cannot be empty"),
            "got: {msg}"
        ),
        other => std::panic::panic_any(format!("Expected InvalidInput, got: {other:?}")),
    }
}

#[test]
fn test_room_name_at_max_length_is_ok() {
    let name = "a".repeat(crate::validation::ROOM_NAME_MAX);
    assert!(validate_room_name(&name).is_ok());
}

#[test]
fn test_room_name_exceeding_max_length_returns_error() {
    let name = "a".repeat(crate::validation::ROOM_NAME_MAX + 1);
    let result = validate_room_name(&name);
    assert!(result.is_err());
    match err(result, "too-long room name should fail") {
        Error::InvalidInput(msg) => assert!(
            msg.contains("characters") || msg.contains("long"),
            "got: {msg}"
        ),
        other => std::panic::panic_any(format!("Expected InvalidInput, got: {other:?}")),
    }
}

#[test]
fn test_valid_room_name_is_ok() {
    assert!(validate_room_name("My Room").is_ok());
    assert!(validate_room_name("a").is_ok());
    assert!(validate_room_name("Room with spaces and 123").is_ok());
}

#[test]
fn test_transaction_permission_helper_uses_runtime_member_default() {
    let settings = RoomSettings::default();
    let member = RoomMember::new(
        RoomId::expect_positive(1),
        UserId::expect_positive(1),
        RoomRole::Member,
    );
    let runtime_member_default = RoomPermissionSet(
        RoomPermissionSet::default_member().0
            & !crate::models::RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE,
    );

    assert!(
        RoomPermissionSet::default_member()
            .has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE),
        "static defaults include CREATE_MEDIA_RESOURCE, so this test guards against falling back to them"
    );
    assert!(
        !super::has_room_permission_from_base(
            &settings,
            &member,
            runtime_member_default,
            crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
        ),
        "transactional permission checks must honor runtime role defaults"
    );
}

#[test]
fn test_room_name_counts_unicode_characters_not_bytes() {
    let max_len = crate::validation::ROOM_NAME_MAX;
    let name: String = std::iter::repeat_n('\u{4e00}', max_len).collect();
    assert_eq!(name.chars().count(), max_len);
    assert!(
        validate_room_name(&name).is_ok(),
        "Room name with {max_len} CJK characters should be valid"
    );

    let name_too_long: String = std::iter::repeat_n('\u{4e00}', max_len + 1).collect();
    assert!(
        validate_room_name(&name_too_long).is_err(),
        "Room name with {} CJK characters should be rejected",
        max_len + 1
    );
}

struct FailingCoordinationLock;

#[async_trait]
impl crate::service::CoordinationLock for FailingCoordinationLock {
    async fn acquire(&self, key: &str, _ttl_secs: u64) -> crate::Result<Option<String>> {
        Err(Error::ServiceUnavailable(format!(
            "synthetic lock failure for {key}"
        )))
    }

    async fn release(&self, _key: &str, _lock_value: &str) -> crate::Result<bool> {
        Ok(true)
    }
}

#[tokio::test]
async fn coordination_lock_error_short_circuits_room_creation_operation() {
    let error = with_coordination_lock(
        &FailingCoordinationLock,
        "create_room:user-1",
        10,
        || async {
            Err::<(), _>(Error::Internal(
                "operation should not run after lock acquisition failure".to_string(),
            ))
        },
    )
    .await
    .expect_err("lock failure should short-circuit the protected operation");

    assert!(
        matches!(error, Error::ServiceUnavailable(ref message) if message.contains("synthetic lock failure")),
        "unexpected error: {error:?}"
    );
}

#[test]
fn test_settings_validate_permissions_guest_escalation_is_rejected() {
    let settings = RoomSettings {
        guest_added_permissions: GuestAddedPermissions(1 << 21),
        ..RoomSettings::default()
    };
    let result = validate_room_settings(&settings);
    assert!(result.is_err());
    match err(result, "guest permission escalation should fail") {
        Error::InvalidInput(msg) => {
            assert!(msg.contains("guest"), "got: {msg}");
        }
        other => std::panic::panic_any(format!("Expected InvalidInput, got: {other:?}")),
    }
}

#[test]
fn test_settings_validate_permissions_member_escalation_is_rejected() {
    let settings = RoomSettings {
        member_added_permissions: MemberAddedPermissions(1 << 21),
        ..RoomSettings::default()
    };
    let result = validate_room_settings(&settings);
    assert!(result.is_err());
    match err(result, "member permission escalation should fail") {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("lifecycle") || msg.contains("member"),
                "got: {msg}"
            );
        }
        other => std::panic::panic_any(format!("Expected InvalidInput, got: {other:?}")),
    }
}

#[test]
fn test_validate_override_bits_for_guest_rejects_member_bitspace() {
    let result = validate_override_bits_for_role(
        RoomRole::Guest,
        RoomMemberPermissionBits::CREATE_MEDIA_RESOURCE,
        0,
    );
    assert!(result.is_err());
    match err(result, "guest role should reject member bitspace") {
        Error::InvalidInput(message) => {
            assert!(
                message.contains("target role permission bitspace"),
                "got: {message}"
            );
        }
        other => std::panic::panic_any(format!("Expected InvalidInput, got: {other:?}")),
    }
}

#[test]
fn test_validate_override_bits_for_guest_accepts_guest_bitspace() {
    ok(
        validate_override_bits_for_role(
            RoomRole::Guest,
            RoomGuestPermissionBits::VIEW_CHAT_HISTORY,
            RoomGuestPermissionBits::USE_WEBRTC,
        ),
        "guest override bits should validate in the guest bitspace",
    );
}

#[test]
fn test_settings_validate_permissions_within_limits_is_ok() {
    let settings = RoomSettings {
        guest_added_permissions: GuestAddedPermissions(RoomGuestPermissionBits::USE_WEBRTC),
        ..RoomSettings::default()
    };
    assert!(validate_room_settings(&settings).is_ok());
}

#[test]
fn test_admin_permissions_with_added_and_removed() {
    let settings = RoomSettings {
        admin_added_permissions: crate::models::room_settings::AdminAddedPermissions(
            crate::models::RoomAdminPermissionBits::PLAY_CONTROL,
        ),
        admin_removed_permissions: crate::models::room_settings::AdminRemovedPermissions(
            crate::models::RoomAdminPermissionBits::CHAT,
        ),
        ..RoomSettings::default()
    };
    let base = RoomPermissionSet(
        crate::models::RoomAdminPermissionBits::CHAT
            | crate::models::RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE,
    );

    let result = settings.admin_permissions(base);
    // Should have CREATE_MEDIA_RESOURCE and PLAY_CONTROL, but not CHAT
    assert!(result.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    assert!(result.has(crate::models::RoomPermission::PLAY_CONTROL));
    assert!(!result.has(crate::models::RoomPermission::CHAT));
}

#[test]
fn test_guest_permissions_capped_at_guest_ceiling() {
    let settings = RoomSettings::default();
    let base = RoomPermissionSet(0);
    let result = settings.guest_permissions(base);
    // Default guest added permissions are 0, so result should be 0
    assert_eq!(result.0, 0);
}

#[test]
fn test_room_ban_sets_is_banned_and_preserves_status() {
    let mut room = RoomFixture::new().build();
    assert_eq!(room.status, RoomStatus::Active);
    assert!(!room.is_banned);

    room.ban();
    assert!(room.is_banned);
    // Status is unchanged -- banning is orthogonal to lifecycle status
    assert_eq!(room.status, RoomStatus::Active);
}

#[test]
fn test_room_unban_clears_is_banned_and_preserves_status() {
    let mut room = RoomFixture::new().build();
    room.ban();
    assert!(room.is_banned);

    room.unban();
    assert!(!room.is_banned);
    assert_eq!(room.status, RoomStatus::Active);
}

#[test]
fn test_room_is_active_considers_lifecycle_and_deleted() {
    let mut room = RoomFixture::new().build();
    assert!(room.is_active());

    // Ban is independent moderation state, not lifecycle state.
    room.ban();
    assert!(room.is_active());
    room.unban();
    assert!(room.is_active());

    // Deleted room is not active
    room.deleted_at = Some(crate::SystemClock.now());
    assert!(!room.is_active());
}

#[test]
fn test_room_is_active_requires_open_lifecycle() {
    use crate::models::Room;

    let room = Room::new("test".to_string(), crate::models::UserId::new());
    assert!(room.is_active());

    // Closed rooms have closed_at set.
    let mut closed_room = room;
    closed_room.close();
    assert!(!closed_room.is_active());
}

#[test]
fn test_room_member_add_and_remove_permissions() {
    use crate::models::{RoomId, RoomMember, RoomRole, UserId};

    let mut member = RoomMember::new(
        RoomId::expect_positive(1),
        UserId::expect_positive(1),
        RoomRole::Member,
    );
    assert_eq!(member.added_permissions, 0);
    assert_eq!(member.removed_permissions, 0);

    member.add_permissions(crate::models::RoomMemberPermissionBits::USE_WEBRTC);
    assert_eq!(
        member.added_permissions,
        crate::models::RoomMemberPermissionBits::USE_WEBRTC
    );

    member.remove_permissions(crate::models::RoomMemberPermissionBits::CHAT);
    assert_eq!(
        member.removed_permissions,
        crate::models::RoomMemberPermissionBits::CHAT
    );

    let effective = member.effective_permissions(RoomPermissionSet::default_member());
    assert!(effective.has(crate::models::RoomPermission::USE_WEBRTC));
    assert!(!effective.has(crate::models::RoomPermission::CHAT));
}

/// Replicates the `room_creation.enabled` guard logic
/// from `do_create_room` for unit testing without a database.
fn check_room_creation_allowed(enabled: bool) -> crate::Result<()> {
    if !enabled {
        return Err(Error::Authorization(
            "Room creation is currently disabled".to_string(),
        ));
    }
    Ok(())
}

#[test]
fn test_room_creation_blocked_when_disabled() {
    let result = check_room_creation_allowed(false);
    assert!(
        result.is_err(),
        "should reject when room_creation.enabled=false"
    );
    match err(result, "disabled room creation should fail") {
        Error::Authorization(msg) => {
            assert!(msg.contains("disabled"), "got: {msg}");
        }
        other => std::panic::panic_any(format!("Expected Authorization, got: {other:?}")),
    }
}

#[test]
fn test_room_creation_allowed_when_enabled() {
    assert!(check_room_creation_allowed(true).is_ok());
}
