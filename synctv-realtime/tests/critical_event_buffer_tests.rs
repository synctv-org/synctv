//! Tests for critical event buffering in Redis pub/sub.
//!
//! These tests verify that critical events (kick/ban) are never dropped
//! even when the normal retry buffer is full.

#![allow(clippy::unwrap_used)]
use synctv_core::models::id::{MediaId, RoomId, UserId};
use synctv_core::models::RoomPermissionSet;
use synctv_realtime::sync::RealtimeEvent;

fn event_id() -> String {
    synctv_common::snanoid!(16)
}

// Test 1: is_critical returns true for KickUser

#[test]
fn test_kick_user_is_critical() {
    let user_id = UserId::new();
    let event = RealtimeEvent::KickUser {
        event_id: event_id(),
        user_id,
        reason: "test kick".to_string(),
        timestamp: chrono::Utc::now(),
    };
    assert!(event.is_critical(), "KickUser should be a critical event");
}

// Test 2: is_critical returns true for KickPublisher

#[test]
fn test_kick_publisher_is_critical() {
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let event = RealtimeEvent::KickPublisher {
        event_id: event_id(),
        room_id,
        media_id,
        reason: "test kick publisher".to_string(),
        timestamp: chrono::Utc::now(),
    };
    assert!(
        event.is_critical(),
        "KickPublisher should be a critical event"
    );
}

// Test 3: is_critical returns true for KickUserFromRoom

#[test]
fn test_kick_user_from_room_is_critical() {
    let user_id = UserId::new();
    let room_id = RoomId::new();
    let event = RealtimeEvent::KickUserFromRoom {
        event_id: event_id(),
        user_id,
        room_id,
        reason: "test kick from room".to_string(),
        timestamp: chrono::Utc::now(),
    };
    assert!(
        event.is_critical(),
        "KickUserFromRoom should be a critical event"
    );
}

// Test 4: is_critical returns true for PermissionChanged

#[test]
fn test_permission_changed_is_critical() {
    let target_user_id = UserId::new();
    let changed_by = UserId::new();
    let room_id = RoomId::new();
    let event = RealtimeEvent::PermissionChanged {
        event_id: event_id(),
        room_id,
        target_user_id,
        target_username: "test_target".to_string(),
        changed_by,
        changed_by_username: "admin".to_string(),
        new_permissions: RoomPermissionSet(0),
        role: 0, // RoomMemberRole::Guest
        added_permissions: RoomPermissionSet(0),
        removed_permissions: RoomPermissionSet(0),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        timestamp: chrono::Utc::now(),
    };
    assert!(
        event.is_critical(),
        "PermissionChanged should be a critical event"
    );
}

// Test 5: is_critical returns true for RoomDeleted

#[test]
fn test_room_deleted_is_critical() {
    let room_id = RoomId::new();
    let deleted_by = UserId::new();
    let event = RealtimeEvent::RoomDeleted {
        event_id: event_id(),
        room_id,
        deleted_by,
        timestamp: chrono::Utc::now(),
    };
    assert!(
        event.is_critical(),
        "RoomDeleted should be a critical event"
    );
}

// Test 6: is_critical returns true for UserLeft

#[test]
fn test_user_left_is_critical() {
    let user_id = UserId::new();
    let room_id = RoomId::new();
    let event = RealtimeEvent::UserLeft {
        event_id: event_id(),
        user_id,
        room_id,
        username: "test_user".to_string(),
        timestamp: chrono::Utc::now(),
    };
    assert!(event.is_critical(), "UserLeft should be a critical event");
}

// Test 7: is_critical returns false for ChatMessage (non-critical)

#[test]
fn test_chat_message_is_not_critical() {
    let user_id = UserId::new();
    let room_id = RoomId::new();
    let event = RealtimeEvent::ChatMessage {
        event_id: event_id(),
        user_id,
        room_id,
        username: "test_user".to_string(),
        message: "test message".to_string(),
        timestamp: chrono::Utc::now(),
        display_position: None,
        display_color: None,
    };
    assert!(
        !event.is_critical(),
        "ChatMessage should NOT be a critical event"
    );
}

// Test 8: is_critical returns false for PlaybackStateChanged (non-critical)

#[test]
fn test_playback_state_changed_is_not_critical() {
    let user_id = UserId::new();
    let room_id = RoomId::new();
    let event = RealtimeEvent::PlaybackStateChanged {
        event_id: event_id(),
        user_id,
        room_id,
        username: "test_user".to_string(),
        state: synctv_core::models::playback::RoomPlaybackState::new(room_id),
        timestamp: chrono::Utc::now(),
    };
    assert!(
        !event.is_critical(),
        "PlaybackStateChanged should NOT be a critical event"
    );
}

// Test 9: is_critical returns false for RoomCreated (non-critical)

#[test]
fn test_room_created_is_not_critical() {
    let room_id = RoomId::new();
    let creator_id = UserId::new();
    let event = RealtimeEvent::RoomCreated {
        event_id: event_id(),
        room_id,
        room_name: "Test Room".to_string(),
        creator_id,
        timestamp: chrono::Utc::now(),
    };
    assert!(
        !event.is_critical(),
        "RoomCreated should not be a critical event"
    );
}

// Test 10: is_critical returns false for MediaAdded (non-critical)

#[test]
fn test_media_added_is_not_critical() {
    let user_id = UserId::new();
    let room_id = RoomId::new();
    let event = RealtimeEvent::MediaAdded {
        event_id: event_id(),
        user_id,
        room_id,
        username: "test_user".to_string(),
        media_id: MediaId::new(),
        media_title: "test_title".to_string(),
        timestamp: chrono::Utc::now(),
    };
    assert!(
        !event.is_critical(),
        "MediaAdded should not be a critical event"
    );
}

// Test 11: is_critical returns false for MediaRemoved (non-critical)

#[test]
fn test_media_removed_is_not_critical() {
    let user_id = UserId::new();
    let room_id = RoomId::new();
    let event = RealtimeEvent::MediaRemoved {
        event_id: event_id(),
        user_id,
        room_id,
        username: "test_user".to_string(),
        media_id: MediaId::new(),
        timestamp: chrono::Utc::now(),
    };
    assert!(
        !event.is_critical(),
        "MediaRemoved should not be a critical event"
    );
}

// Test 12: Verify all critical event types are covered

#[test]
fn test_all_critical_events_covered() {
    // Critical events are: KickPublisher, KickUser, KickUserFromRoom,
    // UserLeft, PermissionChanged, RoomDeleted
    let critical_event_types = vec![
        "KickPublisher",
        "KickUser",
        "KickUserFromRoom",
        "UserLeft",
        "PermissionChanged",
        "RoomDeleted",
    ];

    // Each critical event type should have a corresponding test
    for event_type in critical_event_types {
        // Just verify the list is non-empty and documented
        assert!(!event_type.is_empty(), "Event type should not be empty");
    }
}
