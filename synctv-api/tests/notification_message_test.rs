//! Test for notification message handling
//!
//! This test verifies that the Notification variant in `ServerMessage`
//! properly handles user notifications without abusing the `ErrorMessage` variant.

#![allow(clippy::unwrap_used)]
use synctv_api::impls::messaging::ProtoCodec;
use synctv_api::proto::client::{server_message::Message, ServerMessage};

/// Test that notification messages can be encoded and decoded
#[test]
fn test_notification_message_encode_decode() {
    // Create a notification message
    let notification = ServerMessage {
        message: Some(Message::Notification(
            synctv_api::proto::client::UserNotification {
                notification_id: "test-notification-123".to_string(),
                notification_type: "room_invitation".to_string(),
                title: "Room Invitation".to_string(),
                content: "You have been invited to join a room".to_string(),
                data: r#"{"room_id":"room123","room_name":"Test Room","inviter_name":"Alice"}"#
                    .to_string(),
                timestamp: 1_704_067_200_000, // 2024-01-01 00:00:00 UTC
            },
        )),
    };

    // Encode the message
    let encoded = ProtoCodec::encode_server_message(&notification)
        .expect("Failed to encode notification message");

    // Decode the message
    let decoded =
        ProtoCodec::decode_server_message(&encoded).expect("Failed to decode notification message");

    // Verify the decoded message matches the original
    match decoded.message {
        Some(Message::Notification(notif)) => {
            assert_eq!(notif.notification_id, "test-notification-123");
            assert_eq!(notif.notification_type, "room_invitation");
            assert_eq!(notif.title, "Room Invitation");
            assert_eq!(notif.content, "You have been invited to join a room");
            assert_eq!(notif.timestamp, 1_704_067_200_000);
        }
        _ => panic!("Expected Notification variant, got {:?}", decoded.message),
    }
}

/// Test that notification is a distinct variant from error
#[test]
fn test_notification_is_not_error() {
    let notification = ServerMessage {
        message: Some(Message::Notification(
            synctv_api::proto::client::UserNotification {
                notification_id: "notif-123".to_string(),
                notification_type: "system".to_string(),
                title: "Test".to_string(),
                content: "Test notification".to_string(),
                data: String::new(),
                timestamp: 0,
            },
        )),
    };

    // Verify it's NOT an error
    match notification.message {
        Some(Message::Notification(_)) => {
            // Correct - it's a notification
        }
        Some(Message::Error(_)) => {
            panic!("Notification should not be encoded as Error variant");
        }
        _ => {
            panic!("Expected Notification variant");
        }
    }
}

/// Test that notification variant exists in proto
#[test]
fn test_notification_variant_exists() {
    // This test verifies that the Notification variant is available
    // in the server_message::Message enum

    // If this compiles, the Notification variant exists
    let _notification = Message::Notification(synctv_api::proto::client::UserNotification {
        notification_id: String::new(),
        notification_type: String::new(),
        title: String::new(),
        content: String::new(),
        data: String::new(),
        timestamp: 0,
    });
}

/// Test backward compatibility - error messages still work
#[test]
fn test_error_messages_still_work() {
    let error = ServerMessage {
        message: Some(Message::Error(synctv_api::proto::client::ErrorMessage {
            message: "Actual error".to_string(),
            code: synctv_proto::common::ErrorCode::Unauthorized as i32,
            detail: "Invalid token".to_string(),
        })),
    };

    let encoded =
        ProtoCodec::encode_server_message(&error).expect("Failed to encode error message");

    let decoded =
        ProtoCodec::decode_server_message(&encoded).expect("Failed to decode error message");

    match decoded.message {
        Some(Message::Error(err)) => {
            assert_eq!(err.message, "Actual error");
            assert_eq!(
                err.code,
                synctv_proto::common::ErrorCode::Unauthorized as i32
            );
            assert_eq!(err.detail, "Invalid token");
        }
        _ => panic!("Expected Error variant"),
    }
}

/// Test that notification with data field is properly serialized
#[test]
fn test_notification_with_serialized_data() {
    let room_id = "room-abc-123";
    let room_name = "My Awesome Room";
    let inviter = "Bob";

    let data = serde_json::json!({
        "room_id": room_id,
        "room_name": room_name,
        "inviter_name": inviter,
    });

    let notification = ServerMessage {
        message: Some(Message::Notification(
            synctv_api::proto::client::UserNotification {
                notification_id: uuid::Uuid::new_v4().to_string(),
                notification_type: "room_invitation".to_string(),
                title: format!("Room Invitation: {room_name}"),
                content: format!("{inviter} invited you to join the room \"{room_name}\""),
                data: data.to_string(),
                timestamp: chrono::Utc::now().timestamp_millis(),
            },
        )),
    };

    let encoded = ProtoCodec::encode_server_message(&notification)
        .expect("Failed to encode notification with data");

    let decoded = ProtoCodec::decode_server_message(&encoded)
        .expect("Failed to decode notification with data");

    match decoded.message {
        Some(Message::Notification(notif)) => {
            // Verify the JSON data is preserved
            let parsed_data: serde_json::Value = serde_json::from_str(&notif.data)
                .expect("Failed to parse notification data as JSON");

            assert_eq!(parsed_data["room_id"], room_id);
            assert_eq!(parsed_data["room_name"], room_name);
            assert_eq!(parsed_data["inviter_name"], inviter);
        }
        _ => panic!("Expected Notification variant"),
    }
}
