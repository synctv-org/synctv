//! Notification service tests
//!
//! Tests event construction, subscription, event type names, and serialization.
//!
//! Run with: cargo test --test `notification_service_tests`
#![allow(clippy::unwrap_used)]

use synctv_core::models::{RoomId, UserId};
use synctv_core::service::notification::RoomEvent;
use synctv_core::service::NotificationService;

fn create_service() -> NotificationService {
    NotificationService::default()
}

// ============================================================================
// Event construction tests
// ============================================================================

#[tokio::test]
async fn test_notify_user_joined_event_construction() {
    let service = create_service();
    let mut rx = service.subscribe();

    let room_id = RoomId::from_string("test_room".to_string());
    let user_id = UserId::from_string("test_user".to_string());

    service
        .notify_user_joined(&room_id, &user_id, "alice")
        .unwrap();

    let (received_room_id, received_event) =
        tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();

    assert_eq!(received_room_id, room_id);
    match received_event {
        RoomEvent::UserJoined {
            user_id: uid,
            username,
        } => {
            assert_eq!(uid, user_id);
            assert_eq!(username, "alice");
        }
        other => panic!("Expected UserJoined, got: {other:?}"),
    }
}

// ============================================================================
// Subscription tests
// ============================================================================

#[tokio::test]
async fn test_subscribe_receives_events_in_order() {
    let service = create_service();
    let mut rx = service.subscribe();

    let room_id = RoomId::from_string("order_room".to_string());
    let user_id = UserId::from_string("user1".to_string());

    // Send multiple events
    service
        .notify_user_joined(&room_id, &user_id, "alice")
        .unwrap();
    service
        .notify_user_left(&room_id, &user_id, "alice")
        .unwrap();

    // Receive in order
    let (_, event1) = tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv())
        .await
        .unwrap()
        .unwrap();

    let (_, event2) = tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv())
        .await
        .unwrap()
        .unwrap();

    assert!(matches!(event1, RoomEvent::UserJoined { .. }));
    assert!(matches!(event2, RoomEvent::UserLeft { .. }));
}

// ============================================================================
// Event type name tests
// ============================================================================

#[test]
fn test_event_type_names_all_variants() {
    let events_and_names: Vec<(RoomEvent, &str)> = vec![
        (
            RoomEvent::UserJoined {
                user_id: UserId::new(),
                username: "test".to_string(),
            },
            "user_joined",
        ),
        (
            RoomEvent::UserLeft {
                user_id: UserId::new(),
                username: "test".to_string(),
            },
            "user_left",
        ),
        (
            RoomEvent::ChatMessage {
                message_id: "msg1".to_string(),
                user_id: UserId::new(),
                username: "test".to_string(),
                content: "hello".to_string(),
                timestamp: chrono::Utc::now(),
            },
            "chat_message",
        ),
        (
            RoomEvent::Danmaku {
                user_id: UserId::new(),
                username: "test".to_string(),
                content: "hello".to_string(),
                position: "top".to_string(),
                timestamp: chrono::Utc::now(),
            },
            "danmaku",
        ),
        (
            RoomEvent::PlaybackStateChanged {
                playing: true,
                position: 0.0,
                speed: 1.0,
                media_id: None,
            },
            "playback_state_changed",
        ),
        (
            RoomEvent::MediaAdded {
                user_id: UserId::new(),
                username: "test".to_string(),
                media_id: "m1".to_string(),
                title: "Test".to_string(),
                url: "http://example.com".to_string(),
                position: 0.0,
            },
            "media_added",
        ),
        (
            RoomEvent::MediaRemoved {
                user_id: Some(UserId::new()),
                username: "test".to_string(),
                media_id: "m1".to_string(),
            },
            "media_removed",
        ),
        (
            RoomEvent::PlaylistReordered {
                user_id: Some(UserId::new()),
                username: "test".to_string(),
                media_ids: vec!["m1".to_string()],
            },
            "playlist_reordered",
        ),
        (
            RoomEvent::PlaylistDeleted {
                user_id: Some(UserId::new()),
                username: "test".to_string(),
                playlist_id: "pl1".to_string(),
            },
            "playlist_deleted",
        ),
        (
            RoomEvent::PermissionChanged {
                user_id: UserId::new(),
                role: 1,
                effective_permissions: 0,
                added_permissions: 0,
                removed_permissions: 0,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
                updated_by_user_id: UserId::new(),
                updated_by_username: "test".to_string(),
            },
            "permission_changed",
        ),
        (
            RoomEvent::MemberKicked {
                user_id: UserId::new(),
            },
            "member_kicked",
        ),
        (
            RoomEvent::SettingsUpdated {
                settings: serde_json::json!({}),
                user_id: Some(UserId::new()),
                username: "test".to_string(),
            },
            "settings_updated",
        ),
        (RoomEvent::RoomDeleted, "room_deleted"),
        (
            RoomEvent::StreamStarted {
                media_id: "m1".to_string(),
                user_id: UserId::new(),
            },
            "stream_started",
        ),
        (
            RoomEvent::StreamStopped {
                media_id: "m1".to_string(),
                user_id: UserId::new(),
            },
            "stream_stopped",
        ),
    ];

    for (event, expected_name) in events_and_names {
        assert_eq!(
            event.event_type(),
            expected_name,
            "Event type mismatch for {expected_name}"
        );
    }
}

// ============================================================================
// Serialization tests
// ============================================================================

#[test]
fn test_serialization_user_joined_uses_tagged_type() {
    // The RoomEvent enum uses #[serde(tag = "type", content = "data")]
    // so serialization should produce {"type":"UserJoined","data":{...}}
    let event = RoomEvent::UserJoined {
        user_id: UserId::from_string("user123".to_string()),
        username: "testuser".to_string(),
    };

    let json = serde_json::to_string(&event).unwrap();

    // serde(tag = "type") uses the variant name as-is: "UserJoined"
    assert!(
        json.contains(r#""type":"UserJoined""#),
        "JSON should contain \"type\":\"UserJoined\", got: {json}"
    );
    assert!(json.contains("user123"));
    assert!(json.contains("testuser"));
}

#[test]
fn test_serialization_all_variants_produce_valid_json() {
    let events = vec![
        RoomEvent::UserJoined {
            user_id: UserId::new(),
            username: "test".to_string(),
        },
        RoomEvent::UserLeft {
            user_id: UserId::new(),
            username: "test".to_string(),
        },
        RoomEvent::RoomDeleted,
        RoomEvent::StreamStarted {
            media_id: "m1".to_string(),
            user_id: UserId::new(),
        },
    ];

    for event in events {
        let json = event.to_json().unwrap();
        // Verify it's valid JSON by parsing it back
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            parsed.get("type").is_some(),
            "Serialized event should have 'type' field"
        );
    }
}

#[test]
fn test_room_event_deserialization_round_trip() {
    let event = RoomEvent::UserJoined {
        user_id: UserId::from_string("u1".to_string()),
        username: "alice".to_string(),
    };

    let json = serde_json::to_string(&event).unwrap();
    let deserialized: RoomEvent = serde_json::from_str(&json).unwrap();

    match deserialized {
        RoomEvent::UserJoined { user_id, username } => {
            assert_eq!(user_id.as_str(), "u1");
            assert_eq!(username, "alice");
        }
        _ => panic!("Expected UserJoined after round-trip"),
    }
}
