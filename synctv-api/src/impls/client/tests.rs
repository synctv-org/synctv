//! Tests for client API implementation

use super::convert::*;
use super::{validate_password_for_set, validate_password_for_verify};
use super::{ROOM_PASSWORD_MIN, ROOM_PASSWORD_MAX};
use synctv_core::models::{
    RoomId, UserId, MediaId, PlaylistId, UserRole, UserStatus, RoomStatus,
    RoomRole, MemberStatus,
};

// === Password Validation Tests ===

#[test]
fn test_validate_password_for_set_valid() {
    assert!(validate_password_for_set("abcd").is_ok());
    assert!(validate_password_for_set("a".repeat(128).as_str()).is_ok());
    assert!(validate_password_for_set("secure_password_123").is_ok());
}

#[test]
fn test_validate_password_for_set_too_short() {
    let err = validate_password_for_set("abc").unwrap_err();
    assert!(err.to_string().contains("too short"));
}

#[test]
fn test_validate_password_for_set_too_long() {
    let long = "a".repeat(129);
    let err = validate_password_for_set(&long).unwrap_err();
    assert!(err.to_string().contains("too long"));
}

#[test]
fn test_validate_password_for_set_boundary() {
    // Exactly minimum length
    assert!(validate_password_for_set(&"a".repeat(ROOM_PASSWORD_MIN)).is_ok());
    // One below minimum
    assert!(validate_password_for_set(&"a".repeat(ROOM_PASSWORD_MIN - 1)).is_err());
    // Exactly maximum length
    assert!(validate_password_for_set(&"a".repeat(ROOM_PASSWORD_MAX)).is_ok());
    // One above maximum
    assert!(validate_password_for_set(&"a".repeat(ROOM_PASSWORD_MAX + 1)).is_err());
}

#[test]
fn test_validate_password_for_verify_accepts_short() {
    // Verify allows short passwords (just checking user input against stored hash)
    assert!(validate_password_for_verify("a").is_ok());
    assert!(validate_password_for_verify("").is_ok());
}

#[test]
fn test_validate_password_for_verify_rejects_too_long() {
    let long = "a".repeat(129);
    let err = validate_password_for_verify(&long).unwrap_err();
    assert!(err.to_string().contains("too long"));
}

// === Proto Role Conversion Tests ===

#[test]
fn test_proto_role_to_room_role_all_variants() {
    assert_eq!(
        proto_role_to_room_role(synctv_proto::common::RoomMemberRole::Creator as i32).unwrap(),
        RoomRole::Creator
    );
    assert_eq!(
        proto_role_to_room_role(synctv_proto::common::RoomMemberRole::Admin as i32).unwrap(),
        RoomRole::Admin
    );
    assert_eq!(
        proto_role_to_room_role(synctv_proto::common::RoomMemberRole::Member as i32).unwrap(),
        RoomRole::Member
    );
    assert_eq!(
        proto_role_to_room_role(synctv_proto::common::RoomMemberRole::Guest as i32).unwrap(),
        RoomRole::Guest
    );
}

#[test]
fn test_proto_role_to_room_role_invalid() {
    let err = proto_role_to_room_role(999).unwrap_err();
    assert!(err.to_string().contains("Unknown room member role"));
}

#[test]
fn test_proto_role_to_user_role_all_variants() {
    assert_eq!(
        proto_role_to_user_role(synctv_proto::common::UserRole::Root as i32).unwrap(),
        UserRole::Root
    );
    assert_eq!(
        proto_role_to_user_role(synctv_proto::common::UserRole::Admin as i32).unwrap(),
        UserRole::Admin
    );
    assert_eq!(
        proto_role_to_user_role(synctv_proto::common::UserRole::User as i32).unwrap(),
        UserRole::User
    );
}

#[test]
fn test_proto_role_to_user_role_invalid() {
    let err = proto_role_to_user_role(999).unwrap_err();
    assert!(err.to_string().contains("Unknown user role"));
}

#[test]
fn test_room_role_to_proto_roundtrip() {
    for role in [RoomRole::Creator, RoomRole::Admin, RoomRole::Member, RoomRole::Guest] {
        let proto_val = room_role_to_proto(role);
        let back = proto_role_to_room_role(proto_val).unwrap();
        assert_eq!(role, back);
    }
}

#[test]
fn test_user_role_to_proto_roundtrip() {
    for role in [UserRole::Root, UserRole::Admin, UserRole::User] {
        let proto_val = user_role_to_proto(role);
        let back = proto_role_to_user_role(proto_val).unwrap();
        assert_eq!(role, back);
    }
}

// === User Proto Conversion Tests ===

fn make_test_user(role: UserRole, status: UserStatus) -> synctv_core::models::User {
    synctv_core::models::User {
        id: UserId::from_string("test_user_id".to_string()),
        username: "testuser".to_string(),
        email: Some("test@example.com".to_string()),
        password_hash: "hash".to_string(),
        role,
        status,
        signup_method: None,
        email_verified: true,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
        password_changed_at: chrono::Utc::now(),
    }
}

#[test]
fn test_user_to_proto_basic() {
    let user = make_test_user(UserRole::User, UserStatus::Active);
    let proto = user_to_proto(&user);

    assert_eq!(proto.id, "test_user_id");
    assert_eq!(proto.username, "testuser");
    assert_eq!(proto.email, "test@example.com");
    assert_eq!(proto.role, synctv_proto::common::UserRole::User as i32);
    assert_eq!(proto.status, synctv_proto::common::UserStatus::Active as i32);
    assert!(proto.email_verified);
}

#[test]
fn test_user_to_proto_admin_role() {
    let user = make_test_user(UserRole::Admin, UserStatus::Active);
    let proto = user_to_proto(&user);
    assert_eq!(proto.role, synctv_proto::common::UserRole::Admin as i32);
}

#[test]
fn test_user_to_proto_root_role() {
    let user = make_test_user(UserRole::Root, UserStatus::Active);
    let proto = user_to_proto(&user);
    assert_eq!(proto.role, synctv_proto::common::UserRole::Root as i32);
}

#[test]
fn test_user_to_proto_banned_status() {
    let user = make_test_user(UserRole::User, UserStatus::Banned);
    let proto = user_to_proto(&user);
    assert_eq!(proto.status, synctv_proto::common::UserStatus::Banned as i32);
}

#[test]
fn test_user_to_proto_pending_status() {
    let user = make_test_user(UserRole::User, UserStatus::Pending);
    let proto = user_to_proto(&user);
    assert_eq!(proto.status, synctv_proto::common::UserStatus::Pending as i32);
}

#[test]
fn test_user_to_proto_no_email() {
    let mut user = make_test_user(UserRole::User, UserStatus::Active);
    user.email = None;
    let proto = user_to_proto(&user);
    assert_eq!(proto.email, ""); // None -> empty string
}

// === Room Proto Conversion Tests ===

fn make_test_room(status: RoomStatus) -> synctv_core::models::Room {
    synctv_core::models::Room {
        id: RoomId::from_string("test_room_id".to_string()),
        name: "Test Room".to_string(),
        description: "A test room".to_string(),
        created_by: UserId::from_string("creator_id".to_string()),
        status,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    }
}

#[test]
fn test_room_to_proto_basic() {
    let room = make_test_room(RoomStatus::Active);
    let proto = room_to_proto_basic(&room, None, Some(5));

    assert_eq!(proto.id, "test_room_id");
    assert_eq!(proto.name, "Test Room");
    assert_eq!(proto.description, "A test room");
    assert_eq!(proto.created_by, "creator_id");
    assert_eq!(proto.member_count, 5);
    assert!(!proto.is_banned);
}

#[test]
fn test_room_to_proto_no_member_count() {
    let room = make_test_room(RoomStatus::Active);
    let proto = room_to_proto_basic(&room, None, None);
    assert_eq!(proto.member_count, 0); // None -> 0
}

#[test]
fn test_room_to_proto_banned() {
    let mut room = make_test_room(RoomStatus::Active);
    room.is_banned = true;
    let proto = room_to_proto_basic(&room, None, None);
    assert!(proto.is_banned);
}

#[test]
fn test_room_to_proto_default_settings() {
    let room = make_test_room(RoomStatus::Active);
    let proto = room_to_proto_basic(&room, None, None);
    // Settings should be default (serialized default RoomSettings)
    assert!(!proto.settings.is_empty());
}

// === Playback State Conversion Tests ===

#[test]
fn test_playback_state_to_proto() {
    let state = synctv_core::models::RoomPlaybackState {
        room_id: RoomId::from_string("room1".to_string()),
        playing_media_id: Some(MediaId::from_string("media1".to_string())),
        playing_playlist_id: Some(PlaylistId::from_string("pl1".to_string())),
        relative_path: "/video.mp4".to_string(),
        current_time: 120.5,
        speed: 1.5,
        is_playing: true,
        updated_at: chrono::Utc::now(),
        version: 42,
    };

    let proto = playback_state_to_proto(&state);

    assert_eq!(proto.room_id, "room1");
    assert_eq!(proto.playing_media_id, "media1");
    assert_eq!(proto.playing_playlist_id, "pl1");
    assert_eq!(proto.relative_path, "/video.mp4");
    assert!((proto.current_time - 120.5).abs() < f64::EPSILON);
    assert!((proto.speed - 1.5).abs() < f64::EPSILON);
    assert!(proto.is_playing);
    assert_eq!(proto.version, 42);
}

#[test]
fn test_playback_state_to_proto_no_media() {
    let state = synctv_core::models::RoomPlaybackState::new(
        RoomId::from_string("room1".to_string()),
    );
    let proto = playback_state_to_proto(&state);

    assert_eq!(proto.playing_media_id, ""); // None -> empty string
    assert_eq!(proto.playing_playlist_id, "");
    assert!(!proto.is_playing);
}

// === Media Proto Conversion Tests ===

fn make_test_media() -> synctv_core::models::Media {
    synctv_core::models::Media {
        id: MediaId::from_string("media1".to_string()),
        playlist_id: PlaylistId::from_string("pl1".to_string()),
        room_id: RoomId::from_string("room1".to_string()),
        creator_id: Some(UserId::from_string("user1".to_string())),
        name: "Test Video".to_string(),
        position: 3,
        source_provider: "bilibili".to_string(),
        source_config: serde_json::json!({"bvid": "BV1234"}),
        provider_instance_name: Some("bili_main".to_string()),
        added_at: chrono::Utc::now(),
    }
}

#[test]
fn test_media_to_proto_basic() {
    let media = make_test_media();
    let proto = media_to_proto(&media);

    assert_eq!(proto.id, "media1");
    assert_eq!(proto.room_id, "room1");
    assert_eq!(proto.provider, "bilibili");
    assert_eq!(proto.title, "Test Video");
    assert_eq!(proto.position, 3);
    assert_eq!(proto.added_by, "user1");
    assert_eq!(proto.provider_instance_name, "bili_main");
}

#[test]
fn test_media_to_proto_no_instance_name() {
    let mut media = make_test_media();
    media.provider_instance_name = None;
    let proto = media_to_proto(&media);
    assert_eq!(proto.provider_instance_name, "");
}

// === Room Member Conversion Tests ===

fn make_test_member(role: RoomRole) -> synctv_core::models::RoomMemberWithUser {
    synctv_core::models::RoomMemberWithUser {
        room_id: RoomId::from_string("room1".to_string()),
        user_id: UserId::from_string("user1".to_string()),
        username: "alice".to_string(),
        role,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: chrono::Utc::now(),
        is_online: true,
        banned_at: None,
        banned_reason: None,
    }
}

#[test]
fn test_room_member_to_proto() {
    let member = make_test_member(RoomRole::Member);
    let role_default = RoomRole::Member.permissions();
    let proto = room_member_to_proto(member, role_default);

    assert_eq!(proto.room_id, "room1");
    assert_eq!(proto.user_id, "user1");
    assert_eq!(proto.username, "alice");
    assert_eq!(proto.role, synctv_proto::common::RoomMemberRole::Member as i32);
    assert!(proto.is_online);
}

#[test]
fn test_room_member_to_proto_creator() {
    let member = make_test_member(RoomRole::Creator);
    let role_default = RoomRole::Creator.permissions();
    let proto = room_member_to_proto(member, role_default);
    assert_eq!(proto.role, synctv_proto::common::RoomMemberRole::Creator as i32);
}

#[test]
fn test_room_member_to_proto_custom_permissions() {
    let mut member = make_test_member(RoomRole::Member);
    member.added_permissions = 0xFF;
    member.removed_permissions = 0x0F;
    let role_default = RoomRole::Member.permissions();
    let proto = room_member_to_proto(member, role_default);
    assert_eq!(proto.added_permissions, 0xFF);
    assert_eq!(proto.removed_permissions, 0x0F);
}

// === Playlist Conversion Tests ===

#[test]
fn test_playlist_to_proto() {
    let playlist = synctv_core::models::Playlist {
        id: PlaylistId::from_string("pl1".to_string()),
        room_id: RoomId::from_string("room1".to_string()),
        creator_id: Some(UserId::from_string("user1".to_string())),
        name: "My Playlist".to_string(),
        parent_id: None,
        position: 0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    let proto = playlist_to_proto(&playlist, 10);

    assert_eq!(proto.id, "pl1");
    assert_eq!(proto.room_id, "room1");
    assert_eq!(proto.name, "My Playlist");
    assert_eq!(proto.parent_id, "");
    assert_eq!(proto.item_count, 10);
    // No parent_id means it could be a root folder
    assert!(proto.is_folder);
}

#[test]
fn test_playlist_to_proto_dynamic() {
    let playlist = synctv_core::models::Playlist {
        id: PlaylistId::from_string("pl2".to_string()),
        room_id: RoomId::from_string("room1".to_string()),
        creator_id: Some(UserId::from_string("user1".to_string())),
        name: "Bilibili Folder".to_string(),
        parent_id: Some(PlaylistId::from_string("pl1".to_string())),
        position: 1,
        source_provider: Some("bilibili".to_string()),
        source_config: Some(serde_json::json!({})),
        provider_instance_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    let proto = playlist_to_proto(&playlist, 5);

    assert_eq!(proto.parent_id, "pl1");
    assert!(proto.is_dynamic);
    assert!(proto.is_folder); // has source_provider
}
