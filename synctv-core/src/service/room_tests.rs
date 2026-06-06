use super::RoomService;
use crate::models::{
    room_settings::{GuestAddedPermissions, MemberAddedPermissions},
    RoomGuestPermissionBits, RoomId, RoomMember, RoomMemberPermissionBits, RoomPermissionSet,
    RoomRole, RoomSettings, RoomStatus, UserId,
};
use crate::test_helpers::RoomFixture;
use crate::Error;
use crate::{
    cache::{CacheInvalidationService, KeyBuilder, UsernameCache},
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, UserService,
    },
};
use async_trait::async_trait;
use sqlx::PgPool;
use std::sync::Arc;

/// Replicates the room name validation from `do_create_room`.
fn validate_room_name(name: &str) -> crate::Result<()> {
    crate::validation::RoomNameValidator::new()
        .validate(name)
        .map_err(|e| Error::InvalidInput(e.to_string()))
}

#[test]
fn test_empty_room_name_returns_error() {
    let result = validate_room_name("");
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidInput(msg) => assert!(
            msg.contains("at least 1") || msg.contains("cannot be empty"),
            "got: {msg}"
        ),
        other => panic!("Expected InvalidInput, got: {other:?}"),
    }
}

#[test]
fn test_room_name_at_max_length_is_ok() {
    // Use ROOM_NAME_MAX from validation module (100 characters)
    let name = "a".repeat(crate::validation::ROOM_NAME_MAX);
    assert!(validate_room_name(&name).is_ok());
}

#[test]
fn test_room_name_exceeding_max_length_returns_error() {
    // One over ROOM_NAME_MAX (101 characters)
    let name = "a".repeat(crate::validation::ROOM_NAME_MAX + 1);
    let result = validate_room_name(&name);
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidInput(msg) => assert!(
            msg.contains("characters") || msg.contains("long"),
            "got: {msg}"
        ),
        other => panic!("Expected InvalidInput, got: {other:?}"),
    }
}

#[test]
fn test_valid_room_name_is_ok() {
    assert!(validate_room_name("My Room").is_ok());
    assert!(validate_room_name("a").is_ok());
    assert!(validate_room_name("Room with spaces and 123").is_ok());
}

#[test]
fn test_initial_room_settings_defaults_when_missing() {
    let initialized = super::initial_room_settings(None);

    assert!(initialized.chat_enabled.0);
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
    // Each CJK character is 3 bytes in UTF-8 but 1 character.
    // ROOM_NAME_MAX (100) CJK chars = 300 bytes, should be valid.
    let max_len = crate::validation::ROOM_NAME_MAX;
    let name: String = std::iter::repeat_n('\u{4e00}', max_len).collect();
    assert_eq!(name.chars().count(), max_len);
    assert!(
        validate_room_name(&name).is_ok(),
        "Room name with {max_len} CJK characters should be valid"
    );

    // (ROOM_NAME_MAX + 1) CJK characters should be rejected
    let name_too_long: String = std::iter::repeat_n('\u{4e00}', max_len + 1).collect();
    assert!(
        validate_room_name(&name_too_long).is_err(),
        "Room name with {} CJK characters should be rejected",
        max_len + 1
    );
}

fn make_user_service(pool: &PgPool) -> UserService {
    let jwt_service = JwtService::new("room-service-test-secret-key-32bytes!!").unwrap();
    let username_cache =
        UsernameCache::local_only("room-service:test:username:".to_string(), 128, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let brute_force = BruteForceProtection::in_memory("room-service-test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        KeyBuilder::new("room-service-test"),
        brute_force,
    )
}

#[tokio::test]
async fn standalone_room_service_uses_non_authoritative_fence_by_default() {
    let pool = PgPool::connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused").unwrap();
    let user_service = make_user_service(&pool);
    let room_service =
        RoomService::new_for_tests(pool, user_service).expect("room service should build");

    assert!(
        !room_service.consistency.is_authoritative(),
        "standalone RoomService constructors must not create private authoritative fences"
    );
}

#[tokio::test]
async fn test_cache_invalidation_option_wires_permission_service_for_room_service_new() {
    let pool = PgPool::connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused").unwrap();
    let user_service = make_user_service(&pool);
    let room_service = RoomService::new_with_options(
        pool,
        user_service,
        super::RoomServiceOptions {
            cache_invalidation: Some(Arc::new(CacheInvalidationService::new(
                "room-service-node".to_string(),
                "room-service-stream".to_string(),
            ))),
            ..super::RoomServiceOptions::test_defaults()
        },
    )
    .expect("room service should build");

    assert!(
        room_service.permission_service().has_invalidation_service(),
        "constructor cache invalidation wiring must reach the shared permission service"
    );
}

struct FailingCoordinationLock;

#[async_trait]
impl crate::service::distributed_lock::CoordinationLock for FailingCoordinationLock {
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
async fn test_create_room_uses_injected_coordination_lock_trait_object() {
    let pool = PgPool::connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused").unwrap();
    let user_service = make_user_service(&pool);
    let room_service = RoomService::new_with_options(
        pool,
        user_service,
        super::RoomServiceOptions {
            distributed_lock: Some(Arc::new(FailingCoordinationLock)),
            ..super::RoomServiceOptions::test_defaults()
        },
    )
    .expect("room service should build");

    let error = room_service
        .create_room(
            "locked room".to_string(),
            "desc".to_string(),
            crate::models::UserId::new(),
            None,
            None,
        )
        .await
        .expect_err("lock failure should short-circuit before any database work");

    assert!(
        matches!(error, Error::ServiceUnavailable(ref message) if message.contains("synthetic lock failure")),
        "unexpected error: {error:?}"
    );
}

#[test]
fn test_known_setting_keys_are_valid_via_registry() {
    use crate::models::room_settings::RoomSettingsRegistry;
    let known_keys = [
        ("chat_enabled", "true"),
        (
            "auto_play",
            r#"{"enabled":true,"mode":"sequential","delay":3}"#,
        ),
        ("allow_guest_join", "true"),
        ("max_members", "100"),
    ];
    for (key, val) in &known_keys {
        assert!(
            RoomSettingsRegistry::validate_setting(key, val).is_ok(),
            "Expected key '{key}' with value '{val}' to be valid"
        );
    }
}

#[test]
fn test_unknown_setting_key_returns_error_via_registry() {
    use crate::models::room_settings::RoomSettingsRegistry;
    let result = RoomSettingsRegistry::validate_setting("nonexistent_key", "true");
    assert!(result.is_err());
}

#[test]
fn test_set_by_key_applies_value() {
    let mut settings = RoomSettings::default();
    assert!(settings.chat_enabled.0); // default is true
    settings.set_by_key("chat_enabled", "false").unwrap();
    assert!(!settings.chat_enabled.0);
}

#[test]
fn test_set_by_key_invalid_type_returns_error() {
    let mut settings = RoomSettings::default();
    let result = settings.set_by_key("chat_enabled", "not_a_bool");
    assert!(result.is_err());
}

#[test]
fn test_set_by_key_unknown_key_returns_error() {
    let mut settings = RoomSettings::default();
    let result = settings.set_by_key("nonexistent", "true");
    assert!(result.is_err());
}

#[test]
fn test_set_by_key_max_members() {
    let mut settings = RoomSettings::default();
    settings.set_by_key("max_members", "42").unwrap();
    assert_eq!(settings.max_members.0, 42);
}

#[test]
fn test_set_by_key_max_members_invalid_string() {
    let mut settings = RoomSettings::default();
    let result = settings.set_by_key("max_members", "not_a_number");
    assert!(result.is_err());
}

#[test]
fn test_settings_validate_permissions_guest_escalation_is_rejected() {
    let settings = RoomSettings {
        guest_added_permissions: GuestAddedPermissions(1 << 21),
        ..RoomSettings::default()
    };
    let result = settings.validate_permissions();
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(msg.contains("guest"), "got: {msg}");
        }
        other => panic!("Expected InvalidInput, got: {other:?}"),
    }
}

#[test]
fn test_settings_validate_permissions_member_escalation_is_rejected() {
    let settings = RoomSettings {
        member_added_permissions: MemberAddedPermissions(1 << 21),
        ..RoomSettings::default()
    };
    let result = settings.validate_permissions();
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("lifecycle") || msg.contains("member"),
                "got: {msg}"
            );
        }
        other => panic!("Expected InvalidInput, got: {other:?}"),
    }
}

#[test]
fn test_validate_override_bits_for_guest_rejects_member_bitspace() {
    let result = RoomService::validate_override_bits_for_role(
        RoomRole::Guest,
        RoomMemberPermissionBits::CREATE_MEDIA_RESOURCE,
        0,
    );
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidInput(message) => {
            assert!(
                message.contains("target role permission bitspace"),
                "got: {message}"
            );
        }
        other => panic!("Expected InvalidInput, got: {other:?}"),
    }
}

#[test]
fn test_validate_override_bits_for_guest_accepts_guest_bitspace() {
    RoomService::validate_override_bits_for_role(
        RoomRole::Guest,
        RoomGuestPermissionBits::VIEW_CHAT_HISTORY,
        RoomGuestPermissionBits::USE_WEBRTC,
    )
    .expect("guest override bits should validate in the guest bitspace");
}

#[test]
fn test_settings_validate_permissions_within_limits_is_ok() {
    let settings = RoomSettings {
        guest_added_permissions: GuestAddedPermissions(RoomGuestPermissionBits::USE_WEBRTC),
        ..RoomSettings::default()
    };
    assert!(settings.validate_permissions().is_ok());
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
    room.deleted_at = Some(chrono::Utc::now());
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

/// Replicates the `allow_room_creation` / `disable_create_room` guard logic
/// from `do_create_room` for unit testing without a database.
fn check_room_creation_allowed(
    disable_create_room: bool,
    allow_room_creation: bool,
) -> crate::Result<()> {
    if disable_create_room {
        return Err(Error::Authorization(
            "Room creation is currently disabled".to_string(),
        ));
    }
    if !allow_room_creation {
        return Err(Error::Authorization(
            "Room creation is currently disabled".to_string(),
        ));
    }
    Ok(())
}

#[test]
fn test_room_creation_blocked_when_disable_create_room_is_true() {
    let result = check_room_creation_allowed(true, true);
    assert!(
        result.is_err(),
        "Should reject when disable_create_room=true"
    );
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(msg.contains("disabled"), "got: {msg}");
        }
        other => panic!("Expected Authorization, got: {other:?}"),
    }
}

#[test]
fn test_room_creation_blocked_when_allow_room_creation_is_false() {
    let result = check_room_creation_allowed(false, false);
    assert!(
        result.is_err(),
        "Should reject when allow_room_creation=false"
    );
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(msg.contains("disabled"), "got: {msg}");
        }
        other => panic!("Expected Authorization, got: {other:?}"),
    }
}

#[test]
fn test_disable_create_room_takes_precedence_over_allow() {
    // Even if allow_room_creation=true, disable_create_room=true should block
    let result = check_room_creation_allowed(true, true);
    assert!(
        result.is_err(),
        "disable_create_room=true should take precedence over allow_room_creation=true"
    );
}
