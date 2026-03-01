#![cfg_attr(test, allow(clippy::unwrap_used))]
//! `SyncTV` Protocol Definitions
//!
//! This crate contains all protobuf definitions and generated code for `SyncTV`'s
//! external APIs.

/// Encoded file descriptor set for client/admin/oauth2 proto definitions.
/// Used by tonic-reflection to serve gRPC server reflection.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("descriptor.bin");

/// Encoded file descriptor set for provider proto definitions.
pub const PROVIDERS_FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("providers/descriptor.bin");

// Common shared types (enums, RoomMember)
pub mod common {
    include!("synctv.common.rs");
}

// Client API
pub mod client {
    include!("synctv.client.rs");
}

// Admin API
pub mod admin {
    include!("synctv.admin.rs");
}

// Providers
pub mod providers {
    pub mod bilibili {
        include!("providers/synctv.provider.bilibili.rs");
    }

    pub mod alist {
        include!("providers/synctv.provider.alist.rs");
    }

    pub mod emby {
        include!("providers/synctv.provider.emby.rs");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use prost::Message;

    // === Protobuf Serialization Roundtrip Tests ===
    // Verifies encode -> decode produces identical messages for critical types.

    #[test]
    fn roundtrip_user() {
        let user = crate::client::User {
            id: "user-123".into(),
            username: "alice".into(),
            email: "alice@example.com".into(),
            role: crate::common::UserRole::Admin.into(),
            status: crate::common::UserStatus::Active.into(),
            created_at: 1_700_000_000,
            email_verified: true,
        };
        let bytes = user.encode_to_vec();
        let decoded = crate::client::User::decode(bytes.as_slice()).unwrap();
        assert_eq!(user, decoded);
    }

    #[test]
    fn roundtrip_user_public_view() {
        let view = crate::client::UserPublicView {
            id: "user-456".into(),
            username: "bob".into(),
            role: crate::common::UserRole::User.into(),
            created_at: 1_700_000_000,
        };
        let bytes = view.encode_to_vec();
        let decoded = crate::client::UserPublicView::decode(bytes.as_slice()).unwrap();
        assert_eq!(view, decoded);
    }

    #[test]
    fn roundtrip_room() {
        let room = crate::client::Room {
            id: "room-abc".into(),
            name: "Movie Night".into(),
            created_by: "user-123".into(),
            status: crate::common::RoomStatus::Active.into(),
            settings: b"{\"theme\":\"dark\"}".to_vec(),
            created_at: 1_700_000_000,
            member_count: 5,
            description: "Watch movies together".into(),
            updated_at: 1_700_001_000,
            is_banned: false,
        };
        let bytes = room.encode_to_vec();
        let decoded = crate::client::Room::decode(bytes.as_slice()).unwrap();
        assert_eq!(room, decoded);
    }

    #[test]
    fn roundtrip_room_member() {
        let member = crate::common::RoomMember {
            room_id: "room-abc".into(),
            user_id: "user-123".into(),
            username: "alice".into(),
            role: crate::common::RoomMemberRole::Admin.into(),
            permissions: 0xFF,
            added_permissions: 0x0F,
            removed_permissions: 0x01,
            admin_added_permissions: 0x00,
            admin_removed_permissions: 0x00,
            joined_at: 1_700_000_500,
            is_online: true,
        };
        let bytes = member.encode_to_vec();
        let decoded = crate::common::RoomMember::decode(bytes.as_slice()).unwrap();
        assert_eq!(member, decoded);
    }

    #[test]
    fn roundtrip_playback_state() {
        let state = crate::client::PlaybackState {
            room_id: "room-abc".into(),
            playing_media_id: "media-1".into(),
            current_time: 123.456,
            speed: 1.5,
            is_playing: true,
            updated_at: 1_700_000_000,
            version: 42,
            playing_playlist_id: "playlist-1".into(),
            relative_path: "Season 1/ep01.mkv".into(),
        };
        let bytes = state.encode_to_vec();
        let decoded = crate::client::PlaybackState::decode(bytes.as_slice()).unwrap();
        assert_eq!(state, decoded);
    }

    #[test]
    fn roundtrip_chat_message_receive() {
        let msg = crate::client::ChatMessageReceive {
            id: "msg-001".into(),
            room_id: "room-abc".into(),
            user_id: "user-123".into(),
            username: "alice".into(),
            content: "Hello world!".into(),
            timestamp: 1_700_000_000,
            position: Some(42.5),
            color: Some("#FF0000".into()),
        };
        let bytes = msg.encode_to_vec();
        let decoded = crate::client::ChatMessageReceive::decode(bytes.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_client_message_chat() {
        let msg = crate::client::ClientMessage {
            message: Some(crate::client::client_message::Message::Chat(
                crate::client::ChatMessageSend {
                    content: "Hello!".into(),
                    position: Some(10.0),
                    color: None,
                },
            )),
        };
        let bytes = msg.encode_to_vec();
        let decoded = crate::client::ClientMessage::decode(bytes.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_server_message_playback() {
        let msg = crate::client::ServerMessage {
            message: Some(crate::client::server_message::Message::PlaybackState(
                crate::client::PlaybackStateChanged {
                    room_id: "room-abc".into(),
                    state: Some(crate::client::PlaybackState {
                        room_id: "room-abc".into(),
                        playing_media_id: "media-1".into(),
                        current_time: 0.0,
                        speed: 1.0,
                        is_playing: false,
                        updated_at: 0,
                        version: 1,
                        playing_playlist_id: String::new(),
                        relative_path: String::new(),
                    }),
                },
            )),
        };
        let bytes = msg.encode_to_vec();
        let decoded = crate::client::ServerMessage::decode(bytes.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_server_message_error() {
        let msg = crate::client::ServerMessage {
            message: Some(crate::client::server_message::Message::Error(
                crate::client::ErrorMessage {
                    message: "something failed".into(),
                    code: 404,
                    detail: "resource not found".into(),
                },
            )),
        };
        let bytes = msg.encode_to_vec();
        let decoded = crate::client::ServerMessage::decode(bytes.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_ice_server_with_expiry() {
        let server = crate::client::IceServer {
            urls: vec!["turn:turn.example.com:3478".into()],
            username: Some("user".into()),
            credential: Some("pass".into()),
            expiry_time: 1_700_003_600,
        };
        let bytes = server.encode_to_vec();
        let decoded = crate::client::IceServer::decode(bytes.as_slice()).unwrap();
        assert_eq!(server, decoded);
    }

    #[test]
    fn roundtrip_permission_changed() {
        let msg = crate::client::PermissionChanged {
            room_id: "room-abc".into(),
            user_id: "user-123".into(),
            role: crate::common::RoomMemberRole::Member.into(),
            effective_permissions: 0xFF00,
            added_permissions: 0x0F00,
            removed_permissions: 0x0001,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            updated_by: "admin-user".into(),
        };
        let bytes = msg.encode_to_vec();
        let decoded = crate::client::PermissionChanged::decode(bytes.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    // === Default Value Tests ===

    #[test]
    fn default_values_are_zero() {
        let user = crate::client::User::default();
        assert_eq!(user.id, "");
        assert_eq!(user.username, "");
        assert_eq!(user.email, "");
        assert_eq!(user.role, 0); // UNSPECIFIED
        assert_eq!(user.status, 0); // UNSPECIFIED
        assert_eq!(user.created_at, 0);
        assert!(!user.email_verified);
    }

    #[test]
    fn empty_message_decodes_to_default() {
        let decoded = crate::client::User::decode(&[] as &[u8]).unwrap();
        assert_eq!(decoded, crate::client::User::default());
    }

    // === Enum Value Tests ===

    #[test]
    fn enum_roundtrip_user_role() {
        for role in [
            crate::common::UserRole::Unspecified,
            crate::common::UserRole::User,
            crate::common::UserRole::Admin,
            crate::common::UserRole::Root,
        ] {
            let user = crate::client::User {
                role: role.into(),
                ..Default::default()
            };
            let bytes = user.encode_to_vec();
            let decoded = crate::client::User::decode(bytes.as_slice()).unwrap();
            assert_eq!(decoded.role, i32::from(role));
        }
    }

    #[test]
    fn enum_roundtrip_quality_action() {
        for action in [
            crate::client::QualityAction::Unspecified,
            crate::client::QualityAction::None,
            crate::client::QualityAction::ReduceQuality,
            crate::client::QualityAction::ReduceFramerate,
            crate::client::QualityAction::AudioOnly,
        ] {
            let pnq = crate::client::PeerNetworkQuality {
                quality_action: action.into(),
                ..Default::default()
            };
            let bytes = pnq.encode_to_vec();
            let decoded = crate::client::PeerNetworkQuality::decode(bytes.as_slice()).unwrap();
            assert_eq!(decoded.quality_action, i32::from(action));
        }
    }

    #[test]
    fn enum_roundtrip_provider_instance_status() {
        for status in [
            crate::admin::ProviderInstanceStatus::Unspecified,
            crate::admin::ProviderInstanceStatus::Connected,
            crate::admin::ProviderInstanceStatus::Disconnected,
            crate::admin::ProviderInstanceStatus::Error,
        ] {
            let pi = crate::admin::ProviderInstance {
                status: status.into(),
                ..Default::default()
            };
            let bytes = pi.encode_to_vec();
            let decoded = crate::admin::ProviderInstance::decode(bytes.as_slice()).unwrap();
            assert_eq!(decoded.status, i32::from(status));
        }
    }

    // === Unknown Enum Value Behavior ===

    #[test]
    fn unknown_enum_value_preserved() {
        // Simulate a future enum value (999) that this version doesn't know about
        let user = crate::client::User {
            role: 999,
            ..Default::default()
        };
        let bytes = user.encode_to_vec();
        let decoded = crate::client::User::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.role, 999); // prost preserves unknown enum values as i32
    }

    // === JSON Serialization Tests (serde) ===

    #[test]
    fn json_roundtrip_user() {
        let user = crate::client::User {
            id: "user-123".into(),
            username: "alice".into(),
            email: "alice@example.com".into(),
            role: crate::common::UserRole::Admin.into(),
            status: crate::common::UserStatus::Active.into(),
            created_at: 1_700_000_000,
            email_verified: true,
        };
        let json = serde_json::to_string(&user).unwrap();
        let decoded: crate::client::User = serde_json::from_str(&json).unwrap();
        assert_eq!(user, decoded);
    }

    #[test]
    fn json_roundtrip_playback_state() {
        let state = crate::client::PlaybackState {
            room_id: "room-abc".into(),
            playing_media_id: "media-1".into(),
            current_time: 123.456,
            speed: 1.5,
            is_playing: true,
            updated_at: 1_700_000_000,
            version: 42,
            playing_playlist_id: "playlist-1".into(),
            relative_path: String::new(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let decoded: crate::client::PlaybackState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, decoded);
    }

    #[test]
    fn json_roundtrip_client_message() {
        let msg = crate::client::ClientMessage {
            message: Some(crate::client::client_message::Message::Chat(
                crate::client::ChatMessageSend {
                    content: "test".into(),
                    position: None,
                    color: None,
                },
            )),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: crate::client::ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    // === Backward Compatibility: Added Fields ===

    #[test]
    fn old_ice_server_bytes_decode_with_new_field_default() {
        // Simulate an old IceServer without the expiry_time field
        let old_server = crate::client::IceServer {
            urls: vec!["stun:stun.example.com:3478".into()],
            username: None,
            credential: None,
            expiry_time: 0, // default value - as if field didn't exist
        };
        let bytes = old_server.encode_to_vec();

        // New code can still decode it - expiry_time defaults to 0
        let decoded = crate::client::IceServer::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.expiry_time, 0);
        assert_eq!(decoded.urls, vec!["stun:stun.example.com:3478"]);
    }

    #[test]
    fn old_error_message_bytes_decode_with_new_fields_default() {
        // Simulate old ErrorMessage with only message field
        let old_msg = crate::client::ErrorMessage {
            message: "error".into(),
            code: 0,
            detail: String::new(),
        };
        let bytes = old_msg.encode_to_vec();
        let decoded = crate::client::ErrorMessage::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.message, "error");
        assert_eq!(decoded.code, 0);
        assert_eq!(decoded.detail, "");
    }

    // === Notification Variant Tests ===

    #[test]
    fn roundtrip_user_notification() {
        let notification = crate::client::UserNotification {
            notification_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            notification_type: "room_invitation".to_string(),
            title: "Room Invitation".to_string(),
            content: "You have been invited to join a room".to_string(),
            data: r#"{"room_id":"room123","room_name":"Test Room"}"#.to_string(),
            timestamp: 1_704_067_200_000, // 2024-01-01 00:00:00 UTC
        };
        let bytes = notification.encode_to_vec();
        let decoded = crate::client::UserNotification::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.notification_id, notification.notification_id);
        assert_eq!(decoded.notification_type, notification.notification_type);
        assert_eq!(decoded.title, notification.title);
        assert_eq!(decoded.content, notification.content);
        assert_eq!(decoded.data, notification.data);
        assert_eq!(decoded.timestamp, notification.timestamp);
    }

    #[test]
    fn server_message_notification_variant() {
        use crate::client::server_message::Message;

        let notification = crate::client::UserNotification {
            notification_id: "notif-123".to_string(),
            notification_type: "system".to_string(),
            title: "System Update".to_string(),
            content: "Server will restart in 10 minutes".to_string(),
            data: String::new(),
            timestamp: 1_704_067_200_000,
        };

        let server_msg = crate::client::ServerMessage {
            message: Some(Message::Notification(notification.clone())),
        };

        // Encode and decode
        let bytes = server_msg.encode_to_vec();
        let decoded = crate::client::ServerMessage::decode(bytes.as_slice()).unwrap();

        // Verify it's the Notification variant
        match decoded.message {
            Some(Message::Notification(decoded_notif)) => {
                assert_eq!(decoded_notif.notification_id, notification.notification_id);
                assert_eq!(decoded_notif.notification_type, notification.notification_type);
                assert_eq!(decoded_notif.title, notification.title);
                assert_eq!(decoded_notif.content, notification.content);
                assert_eq!(decoded_notif.timestamp, notification.timestamp);
            }
            _ => panic!("Expected Notification variant, got {:?}", decoded.message),
        }
    }

    #[test]
    fn notification_is_distinct_from_error() {
        use crate::client::server_message::Message;

        let notification = crate::client::ServerMessage {
            message: Some(Message::Notification(crate::client::UserNotification {
                notification_id: "notif-123".to_string(),
                notification_type: "system".to_string(),
                title: "Test".to_string(),
                content: "Test notification".to_string(),
                data: String::new(),
                timestamp: 0,
            })),
        };

        let error = crate::client::ServerMessage {
            message: Some(Message::Error(crate::client::ErrorMessage {
                message: "Actual error".to_string(),
                code: 1000,
                detail: "Invalid token".to_string(),
            })),
        };

        // Verify they are different variants
        match &notification.message {
            Some(Message::Notification(_)) => {
                // Correct - it's a notification
            }
            Some(Message::Error(_)) => {
                panic!("Notification should not be Error variant");
            }
            _ => {
                panic!("Expected Notification variant");
            }
        }

        match &error.message {
            Some(Message::Notification(_)) => {
                panic!("Error should not be Notification variant");
            }
            Some(Message::Error(_)) => {
                // Correct - it's an error
            }
            _ => {
                panic!("Expected Error variant");
            }
        }
    }

    // === Admin Proto Tests ===

    #[test]
    fn roundtrip_admin_user() {
        let user = crate::admin::AdminUser {
            id: "user-123".into(),
            username: "admin".into(),
            email: "admin@example.com".into(),
            role: crate::common::UserRole::Root.into(),
            status: crate::common::UserStatus::Active.into(),
            created_at: 1_700_000_000,
            updated_at: 1_700_001_000,
        };
        let bytes = user.encode_to_vec();
        let decoded = crate::admin::AdminUser::decode(bytes.as_slice()).unwrap();
        assert_eq!(user, decoded);
    }

    #[test]
    fn roundtrip_update_user_password_request_with_audit() {
        let req = crate::admin::UpdateUserPasswordRequest {
            user_id: "user-123".into(),
            new_password: "new-secure-pass".into(),
            reason: "security incident".into(),
        };
        let bytes = req.encode_to_vec();
        let decoded = crate::admin::UpdateUserPasswordRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.user_id, "user-123");
        assert_eq!(decoded.reason, "security incident");
    }

    // === Provider Proto Tests ===

    #[test]
    fn roundtrip_bilibili_parse_request() {
        let req = crate::providers::bilibili::ParseRequest {
            url: "https://bilibili.com/video/BV123".into(),
            cookies: std::iter::once(("SESSDATA".into(), "abc123".into())).collect(),
            instance_name: "bilibili_main".into(),
        };
        let bytes = req.encode_to_vec();
        let decoded = crate::providers::bilibili::ParseRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.url, req.url);
        assert_eq!(decoded.cookies.get("SESSDATA"), Some(&"abc123".to_string()));
    }

    #[test]
    fn roundtrip_alist_login_request() {
        let req = crate::providers::alist::LoginRequest {
            host: "https://alist.example.com".into(),
            username: "user".into(),
            password: "pass".into(),
            hashed_password: String::new(),
            instance_name: "alist_main".into(),
        };
        let bytes = req.encode_to_vec();
        let decoded = crate::providers::alist::LoginRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.host, req.host);
        assert_eq!(decoded.username, "user");
        assert_eq!(decoded.password, "pass");
    }

    #[test]
    fn roundtrip_emby_login_request() {
        let req = crate::providers::emby::LoginRequest {
            host: "https://emby.example.com".into(),
            api_key: "secret-api-key".into(),
            instance_name: "emby_main".into(),
        };
        let bytes = req.encode_to_vec();
        let decoded = crate::providers::emby::LoginRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.host, req.host);
        assert_eq!(decoded.api_key, "secret-api-key");
    }
}
