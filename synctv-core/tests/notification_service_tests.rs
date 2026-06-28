//! Notification service tests
//!
//! Tests event construction, subscription, event type names, and serialization.

use synctv_core::models::{MediaId, PlaylistId, RoomId, UserId};
use synctv_core::service::notification::RoomEvent;
use synctv_core::service::NotificationService;
use synctv_core_testing::ok;

fn create_service() -> NotificationService {
    NotificationService::default()
}

// Event construction tests

#[tokio::test]
async fn test_notify_user_joined_event_construction() {
    let service = create_service();
    let mut rx = service.subscribe();

    let room_id = RoomId::expect_positive(1);
    let user_id = UserId::expect_positive(2);

    assert_eq!(service.notify_user_joined(&room_id, &user_id, "alice"), 1);

    let (received_room_id, received_event) = ok(
        ok(
            tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await,
            "user joined event should arrive before timeout",
        ),
        "user joined event should be received",
    );

    assert_eq!(received_room_id, room_id);
    match received_event {
        RoomEvent::UserJoined {
            user_id: uid,
            username,
        } => {
            assert_eq!(uid, user_id);
            assert_eq!(username, "alice");
        }
        other => std::panic::panic_any(format!("Expected UserJoined, got: {other:?}")),
    }
}

// Subscription tests

#[tokio::test]
async fn test_subscribe_receives_events_in_order() {
    let service = create_service();
    let mut rx = service.subscribe();

    let room_id = RoomId::expect_positive(3);
    let user_id = UserId::expect_positive(4);

    assert_eq!(service.notify_user_joined(&room_id, &user_id, "alice"), 1);
    assert_eq!(service.notify_user_left(&room_id, &user_id, "alice"), 1);

    // Receive in order
    let (_, event1) = ok(
        ok(
            tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await,
            "first room event should arrive before timeout",
        ),
        "first room event should be received",
    );

    let (_, event2) = ok(
        ok(
            tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await,
            "second room event should arrive before timeout",
        ),
        "second room event should be received",
    );

    assert!(matches!(event1, RoomEvent::UserJoined { .. }));
    assert!(matches!(event2, RoomEvent::UserLeft { .. }));
}

// Event type name tests

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
            RoomEvent::MediaAdded {
                user_id: UserId::new(),
                username: "test".to_string(),
                media_id: MediaId::expect_positive(1),
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
                media_id: MediaId::expect_positive(1),
            },
            "media_removed",
        ),
        (
            RoomEvent::PlaylistReordered {
                user_id: Some(UserId::new()),
                username: "test".to_string(),
                media_ids: vec![MediaId::expect_positive(1)],
            },
            "playlist_reordered",
        ),
        (
            RoomEvent::PlaylistDeleted {
                user_id: Some(UserId::new()),
                username: "test".to_string(),
                playlist_id: PlaylistId::expect_positive(1),
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
                settings: synctv_core::models::RoomSettings::default(),
                version: 1,
                user_id: Some(UserId::new()),
                username: "test".to_string(),
            },
            "settings_updated",
        ),
        (RoomEvent::RoomDeleted, "room_deleted"),
        (
            RoomEvent::StreamStarted {
                media_id: MediaId::expect_positive(1),
                user_id: UserId::new(),
            },
            "stream_started",
        ),
        (
            RoomEvent::StreamStopped {
                media_id: MediaId::expect_positive(1),
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

#[test]
fn test_serialization_user_joined_uses_tagged_type() {
    let event = RoomEvent::UserJoined {
        user_id: UserId::expect_positive(123),
        username: "testuser".to_string(),
    };

    let json = ok(
        serde_json::to_string(&event),
        "user joined event should serialize",
    );

    assert!(
        json.contains(r#""type":"userJoined""#),
        "JSON should contain \"type\":\"userJoined\", got: {json}"
    );
    assert!(json.contains("123"));
    assert!(json.contains("testuser"));
}
