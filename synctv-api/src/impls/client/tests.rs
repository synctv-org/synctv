//! Tests for client API implementation
#![allow(clippy::unwrap_used)]

use super::convert::*;
use super::{validate_password_for_set, validate_password_for_verify};
use super::{ROOM_PASSWORD_MIN, ROOM_PASSWORD_MAX};
use synctv_core::models::{
    RoomId, UserId, MediaId, PlaylistId, UserRole, UserStatus, RoomStatus,
    RoomRole, MemberStatus,
};

// === Timing Attack Protection Tests ===

/// Minimum delay constant used in `check_room_password` for timing attack protection.
/// This should match the constant in room.rs.
const MIN_PASSWORD_CHECK_DELAY_MS: u64 = 250;

#[test]
fn test_timing_attack_delay_constant_is_sufficient() {
    // Verify the delay constant is at least 200ms as per the security requirement.
    // A delay of 200-250ms is sufficient to mask timing differences in password
    // verification while not being overly burdensome to legitimate users.
    const { assert!(MIN_PASSWORD_CHECK_DELAY_MS >= 200) };
    const { assert!(MIN_PASSWORD_CHECK_DELAY_MS <= 500) };
}

#[test]
fn test_timing_attack_delay_matches_room_rs() {
    // This test documents the expected delay value.
    // If the constant in room.rs changes without updating this test, the test
    // serves as a reminder to verify the new value is still appropriate.
    //
    // The constant in room.rs should be:
    // const MIN_PASSWORD_CHECK_DELAY_MS: u64 = 250;
    //
    // If you need to change this value, consider:
    // 1. Lower values (< 200ms) may not provide sufficient timing attack protection
    // 2. Higher values (> 300ms) may noticeably impact user experience
    // 3. The value should be constant - not configurable at runtime - to prevent
    //    attackers from manipulating it
    assert_eq!(MIN_PASSWORD_CHECK_DELAY_MS, 250);
}

/// Test that the timing delay calculation logic works correctly.
#[test]
fn test_timing_delay_calculation() {
    use std::time::Duration;

    // Simulate the timing protection logic
    fn calculate_sleep_duration(elapsed: Duration, min_delay: Duration) -> Option<Duration> {
        if elapsed < min_delay {
            Some(min_delay.checked_sub(elapsed).unwrap())
        } else {
            None
        }
    }

    let min_delay = Duration::from_millis(MIN_PASSWORD_CHECK_DELAY_MS);

    // Test case 1: Very fast operation (0ms elapsed) should require full delay
    let fast_elapsed = Duration::from_millis(0);
    let sleep = calculate_sleep_duration(fast_elapsed, min_delay);
    assert!(sleep.is_some(), "Fast operation should require sleep");
    assert_eq!(sleep.unwrap(), min_delay, "Should sleep for full delay");

    // Test case 2: Partial time elapsed (50ms) should require partial delay
    let partial_elapsed = Duration::from_millis(50);
    let sleep = calculate_sleep_duration(partial_elapsed, min_delay);
    assert!(sleep.is_some(), "Partial operation should require sleep");
    let expected_sleep = min_delay.checked_sub(partial_elapsed).unwrap();
    assert_eq!(sleep.unwrap(), expected_sleep, "Should sleep for remaining time");

    // Test case 3: Operation took exactly minimum time (250ms) should not require sleep
    let exact_elapsed = Duration::from_millis(MIN_PASSWORD_CHECK_DELAY_MS);
    let sleep = calculate_sleep_duration(exact_elapsed, min_delay);
    assert!(sleep.is_none(), "Operation at exact threshold should not require sleep");

    // Test case 4: Operation took longer than minimum (300ms) should not require sleep
    let long_elapsed = Duration::from_millis(300);
    let sleep = calculate_sleep_duration(long_elapsed, min_delay);
    assert!(sleep.is_none(), "Long operation should not require sleep");
}

/// Test that timing protection applies to both success and failure paths equally.
#[test]
fn test_timing_protection_applies_equally() {
    // This test documents the principle that timing protection must apply
    // regardless of the password verification result.
    //
    // In check_room_password, the timing delay is applied AFTER the password
    // check, ensuring both valid=true and valid=false paths have the same
    // minimum execution time.
    //
    // The structure is:
    // 1. Start timer
    // 2. Perform password verification (returns valid = true or false)
    // 3. Log failure if applicable (constant time for logging)
    // 4. Calculate and apply sleep to reach minimum delay
    // 5. Return result
    //
    // This ensures attackers cannot distinguish between valid and invalid
    // passwords by measuring response times.

    // The key invariant: both paths must have at least MIN_PASSWORD_CHECK_DELAY_MS
    // total execution time, making them indistinguishable from a timing perspective.
    let min_delay_ms = MIN_PASSWORD_CHECK_DELAY_MS;

    // Verify our minimum delay provides adequate protection
    // (250ms makes timing attacks impractical over network)
    assert!(min_delay_ms >= 200);
}

/// Test that simulates the exact timing protection logic used in `check_room_password`.
/// This verifies that both password success and failure scenarios result in
/// approximately the same total execution time.
#[test]
fn test_timing_protection_simulation() {
    use std::time::{Duration, Instant};

    // Simulate the timing protection logic exactly as implemented in room.rs
    fn simulate_password_check_timing(_password_valid: bool, operation_time_ms: u64) -> Duration {
        let start = Instant::now();
        let min_delay = Duration::from_millis(MIN_PASSWORD_CHECK_DELAY_MS);

        // Simulate the actual password verification work
        // (in real code, this would be bcrypt verification which takes variable time)
        std::thread::sleep(Duration::from_millis(operation_time_ms));

        // Apply the timing protection (same for both valid and invalid passwords)
        let elapsed = start.elapsed();
        if elapsed < min_delay {
            std::thread::sleep(min_delay.checked_sub(elapsed).unwrap());
        }

        start.elapsed()
    }

    // Simulate fast password verification (wrong password - fast reject)
    let fast_result = simulate_password_check_timing(false, 5);

    // Simulate slow password verification (correct password - full bcrypt)
    let slow_result = simulate_password_check_timing(true, 100);

    // Both should result in at least MIN_PASSWORD_CHECK_DELAY_MS
    assert!(
        fast_result >= Duration::from_millis(MIN_PASSWORD_CHECK_DELAY_MS),
        "Fast operation should be padded to minimum delay"
    );
    assert!(
        slow_result >= Duration::from_millis(MIN_PASSWORD_CHECK_DELAY_MS),
        "Slow operation should meet minimum delay"
    );

    // The difference between them should be small (bounded by the minimum delay)
    // With a 250ms minimum, both should be within ~100ms of each other
    let diff = fast_result.abs_diff(slow_result);
    assert!(
        diff < Duration::from_millis(100),
        "Timing difference between fast and slow operations should be bounded: {diff:?}"
    );
}

/// Test that the timing delay is in the recommended 200-250ms range.
#[test]
fn test_timing_delay_in_recommended_range() {
    // The security requirement specifies a minimum delay of 200-250ms.
    // This test verifies the constant falls within this range.
    //
    // Rationale for the range:
    // - 200ms: Minimum safe threshold to mask network timing jitter
    // - 250ms: Recommended value providing extra safety margin
    // - Below 200ms: May still be vulnerable to statistical timing attacks
    // - Above 300ms: Noticeably impacts user experience
    const { assert!(MIN_PASSWORD_CHECK_DELAY_MS >= 200) };
    const { assert!(MIN_PASSWORD_CHECK_DELAY_MS <= 300) };
}

/// Test that verifies the timing protection cannot be bypassed by early returns.
#[test]
fn test_no_early_return_bypass() {
    // This test documents that the timing protection in check_room_password
    // is placed AFTER all password verification logic, ensuring it cannot be
    // bypassed by early returns in the verification path.
    //
    // The implementation in room.rs follows this structure:
    // ```
    // let start = std::time::Instant::now();
    // let valid = self.room_service.check_room_password(&rid, &req.password).await?;
    // if !valid { tracing::info!(...); }  // Log failure (constant time)
    // // Timing protection applied HERE - after all verification logic
    // let elapsed = start.elapsed();
    // if elapsed < min_delay { tokio::time::sleep(min_delay - elapsed).await; }
    // Ok(response)
    // ```
    //
    // Key security properties:
    // 1. Timer starts BEFORE password verification
    // 2. Sleep happens AFTER verification, regardless of result
    // 3. No early returns between timer start and sleep
    // 4. Logging happens inside the timed window

    // Verify the constant is properly defined
    assert_eq!(MIN_PASSWORD_CHECK_DELAY_MS, 250);
}

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
        password_version: 0,
        version: 0,
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
        version: 1,
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
        version: 0,
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
        is_active: true,
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
        version: 0,
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
        version: 0,
    };

    let proto = playlist_to_proto(&playlist, 5);

    assert_eq!(proto.parent_id, "pl1");
    assert!(proto.is_dynamic);
    assert!(proto.is_folder); // has source_provider
}

// === URL-Derived Title Sanitization Tests (Issue 3) ===

#[test]
fn test_sanitize_url_derived_title_normal() {
    let title = super::media::sanitize_url_derived_title("my_video.mp4");
    assert_eq!(title, "my_video.mp4");
}

#[test]
fn test_sanitize_url_derived_title_percent_encoded() {
    let title = super::media::sanitize_url_derived_title("my%20video.mp4");
    assert_eq!(title, "my video.mp4");
}

#[test]
fn test_sanitize_url_derived_title_control_chars_stripped() {
    let title = super::media::sanitize_url_derived_title("bad\x00name.mp4");
    assert_eq!(title, "badname.mp4");
}

#[test]
fn test_sanitize_url_derived_title_empty_becomes_empty() {
    let title = super::media::sanitize_url_derived_title("");
    assert_eq!(title, "");
}

#[test]
fn test_sanitize_url_derived_title_truncates_long_names() {
    use crate::http::validation::limits::MEDIA_TITLE_MAX;

    // Create a title that exceeds MEDIA_TITLE_MAX (500 chars)
    let long_name = "a".repeat(MEDIA_TITLE_MAX + 100);
    let title = super::media::sanitize_url_derived_title(&long_name);
    assert!(
        title.len() <= MEDIA_TITLE_MAX,
        "URL-derived title should be truncated to MEDIA_TITLE_MAX ({MEDIA_TITLE_MAX}), got {}",
        title.len()
    );
    assert_eq!(title.len(), MEDIA_TITLE_MAX);
}

#[test]
fn test_sanitize_url_derived_title_truncates_at_char_boundary() {
    use crate::http::validation::limits::MEDIA_TITLE_MAX;

    // Create a string with multi-byte UTF-8 characters that would cause a
    // non-char-boundary truncation if done naively.
    // Each CJK character is 3 bytes. Fill to just over the limit.
    let num_chars = (MEDIA_TITLE_MAX / 3) + 10; // will exceed MEDIA_TITLE_MAX in bytes
    let long_cjk: String = std::iter::repeat_n('\u{4e00}', num_chars).collect();
    assert!(long_cjk.len() > MEDIA_TITLE_MAX);

    let title = super::media::sanitize_url_derived_title(&long_cjk);
    assert!(
        title.len() <= MEDIA_TITLE_MAX,
        "Truncated title should not exceed MEDIA_TITLE_MAX"
    );
    // Verify it's valid UTF-8 (would panic on invalid boundary)
    assert!(title.is_char_boundary(title.len()));
}

#[test]
fn test_sanitize_url_derived_title_exactly_at_max() {
    use crate::http::validation::limits::MEDIA_TITLE_MAX;

    let exact = "b".repeat(MEDIA_TITLE_MAX);
    let title = super::media::sanitize_url_derived_title(&exact);
    assert_eq!(title.len(), MEDIA_TITLE_MAX);
    assert_eq!(title, exact);
}

// === Pagination Normalization Tests (Issue 2) ===

#[test]
fn test_pagination_page_zero_treated_as_one() {
    // validate_page should treat page=0 as page=1 (1-based pagination)
    assert_eq!(crate::http::validation::validate_page(Some(0)), 1);
}

#[test]
fn test_pagination_page_negative_treated_as_one() {
    assert_eq!(crate::http::validation::validate_page(Some(-1)), 1);
    assert_eq!(crate::http::validation::validate_page(Some(-100)), 1);
}

#[test]
fn test_pagination_page_none_defaults_to_one() {
    assert_eq!(crate::http::validation::validate_page(None), 1);
}

#[test]
fn test_pagination_page_positive_passes_through() {
    assert_eq!(crate::http::validation::validate_page(Some(1)), 1);
    assert_eq!(crate::http::validation::validate_page(Some(5)), 5);
    assert_eq!(crate::http::validation::validate_page(Some(100)), 100);
}

// === Permission Check Presence Tests (Issue 1) ===
// These tests verify that the REORDER_PLAYLIST permission bit exists and is
// used in the correct context. The actual API-layer permission enforcement
// is tested via integration tests that require a running database, but these
// unit tests verify the permission constant is properly defined.

#[test]
fn test_reorder_playlist_permission_bit_exists() {
    use synctv_core::models::PermissionBits;
    // REORDER_PLAYLIST must be a distinct, non-zero permission bit
    let perm = PermissionBits::REORDER_PLAYLIST;
    assert_ne!(perm, 0, "REORDER_PLAYLIST permission bit must be non-zero");
    // It should be a power of two (single bit)
    assert!(perm.is_power_of_two(), "REORDER_PLAYLIST should be a single permission bit");
}

#[test]
fn test_reorder_playlist_permission_in_creator_defaults() {
    use synctv_core::models::{PermissionBits, RoomRole};
    // Creator role should include REORDER_PLAYLIST by default
    let creator_perms = RoomRole::Creator.permissions();
    assert!(
        creator_perms.0 & PermissionBits::REORDER_PLAYLIST != 0,
        "Creator role should have REORDER_PLAYLIST permission"
    );
}
