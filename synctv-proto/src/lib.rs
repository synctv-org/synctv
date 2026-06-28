#![allow(clippy::missing_errors_doc)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! `SyncTV` Protocol Definitions
//!
//! This crate contains all protobuf definitions and generated code for `SyncTV`'s
//! external APIs.

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub static DESCRIPTOR_POOL: std::sync::LazyLock<prost_reflect::DescriptorPool> =
    std::sync::LazyLock::new(|| {
        prost_reflect::DescriptorPool::decode(FILE_DESCRIPTOR_SET)
            .expect("synctv-proto descriptor pool must decode")
    });

/// Encoded file descriptor set for client/admin/oauth2 proto definitions.
/// Used by tonic-reflection to serve gRPC server reflection.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(
    env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
    "/descriptor.bin"
));

/// Encoded file descriptor set for provider proto definitions.
pub const PROVIDERS_FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(
    env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
    "/descriptor.bin"
));

pub static PROVIDERS_DESCRIPTOR_POOL: std::sync::LazyLock<prost_reflect::DescriptorPool> =
    std::sync::LazyLock::new(|| {
        prost_reflect::DescriptorPool::decode(PROVIDERS_FILE_DESCRIPTOR_SET)
            .expect("synctv-proto provider descriptor pool must decode")
    });

/// Encoded file descriptor set for playback-provider proto definitions.
pub const PLAYBACK_PROVIDER_FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(
    env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
    "/descriptor.bin"
));

pub static PLAYBACK_PROVIDER_DESCRIPTOR_POOL: std::sync::LazyLock<prost_reflect::DescriptorPool> =
    std::sync::LazyLock::new(|| {
        prost_reflect::DescriptorPool::decode(PLAYBACK_PROVIDER_FILE_DESCRIPTOR_SET)
            .expect("synctv-proto playback-provider descriptor pool must decode")
    });

pub fn validate<M: prost_reflect::ReflectMessage>(
    message: &M,
) -> Result<(), prost_protovalidate::Error> {
    prost_protovalidate::validate(message)
}

// Common shared types (enums, RoomMember)
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod common {
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.common.rs"
    ));
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.common.serde.rs"
    ));
}

// Provider source configuration contracts
#[allow(clippy::large_enum_variant)]
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod source_config {
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.source_config.rs"
    ));
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.source_config.serde.rs"
    ));
}

// Client API
#[allow(clippy::large_enum_variant)]
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod client {
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.client.rs"
    ));
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.client.serde.rs"
    ));
}

// Admin API
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod admin {
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.admin.rs"
    ));
    include!(concat!(
        env!("SYNCTV_PROTO_MAIN_OUT_DIR"),
        "/synctv.admin.serde.rs"
    ));
}

// Providers
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod providers {
    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod common {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.common.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.common.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod rtmp {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.rtmp.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.rtmp.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod bilibili {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.bilibili.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.bilibili.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod alist {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.alist.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.alist.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod emby {
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.emby.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PROVIDERS_OUT_DIR"),
            "/synctv.provider.emby.serde.rs"
        ));
    }
}

// Playback provider playback resources
#[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
#[allow(clippy::pedantic)]
pub mod playback_provider {
    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod common {
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.common.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.common.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod direct_url {
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.direct_url.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.direct_url.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod alist {
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.alist.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.alist.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod emby {
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.emby.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.emby.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod bilibili {
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.bilibili.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.bilibili.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod rtmp {
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.rtmp.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.rtmp.serde.rs"
        ));
    }

    #[cfg_attr(feature = "openapi", allow(clippy::large_stack_arrays))]
    #[allow(clippy::pedantic)]
    pub mod live_proxy {
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.live_proxy.rs"
        ));
        include!(concat!(
            env!("SYNCTV_PROTO_PLAYBACK_PROVIDER_OUT_DIR"),
            "/synctv.playback_provider.live_proxy.serde.rs"
        ));
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::too_many_lines)]
mod tests {
    use prost::Message;

    fn validation_error_text(error: &prost_protovalidate::Error) -> String {
        error.to_string()
    }

    fn direct_url_media_source_config(url: &str) -> crate::source_config::MediaSourceConfig {
        crate::source_config::MediaSourceConfig {
            provider: Some(
                crate::source_config::media_source_config::Provider::DirectUrl(
                    crate::source_config::DirectUrlMediaSourceConfig {
                        medias: vec![crate::source_config::DirectUrlMediaResourceConfig {
                            name: String::new(),
                            url: url.to_string(),
                            headers: std::collections::HashMap::default(),
                            format: String::new(),
                        }],
                        default_media_index: None,
                        subtitles: Vec::new(),
                        default_subtitle_index: None,
                        danmakus: Vec::new(),
                        default_danmaku_index: None,
                        is_live: None,
                        duration_seconds: None,
                        prefer_proxy: None,
                    },
                ),
            ),
        }
    }

    fn alist_media_source_config(path: &str) -> crate::source_config::MediaSourceConfig {
        crate::source_config::MediaSourceConfig {
            provider: Some(crate::source_config::media_source_config::Provider::Alist(
                crate::source_config::AlistMediaSourceConfig {
                    server_id: "alist-main".to_string(),
                    path: path.to_string(),
                    password: None,
                },
            )),
        }
    }

    fn alist_playlist_source_config(path: &str) -> crate::source_config::PlaylistSourceConfig {
        crate::source_config::PlaylistSourceConfig {
            provider: Some(
                crate::source_config::playlist_source_config::Provider::Alist(
                    crate::source_config::AlistPlaylistSourceConfig {
                        server_id: "alist-main".to_string(),
                        path: path.to_string(),
                        password: None,
                    },
                ),
            ),
        }
    }

    fn room_settings() -> crate::client::RoomSettings {
        crate::client::RoomSettings {
            allow_guest_join: true,
            max_members: 8,
            require_approval: false,
            allow_auto_join: true,
            chat_enabled: true,
            auto_play: Some(crate::client::AutoPlaySettings {
                enabled: true,
                mode: crate::client::PlayMode::Sequential as i32,
                delay: 0,
            }),
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            member_added_permissions: 0,
            member_removed_permissions: 0,
            guest_added_permissions: 0,
            guest_removed_permissions: 0,
        }
    }

    fn room_settings_patch() -> crate::client::RoomSettingsPatch {
        crate::client::RoomSettingsPatch {
            allow_guest_join: Some(true),
            max_members: Some(8),
            require_approval: None,
            allow_auto_join: None,
            chat_enabled: Some(true),
            auto_play: Some(crate::client::AutoPlaySettingsPatch {
                enabled: Some(true),
                mode: Some(crate::client::PlayMode::Sequential as i32),
                delay: Some(0),
            }),
            admin_added_permissions: None,
            admin_removed_permissions: None,
            member_added_permissions: None,
            member_removed_permissions: None,
            guest_added_permissions: None,
            guest_removed_permissions: None,
        }
    }

    fn emby_target(item_id: &str) -> crate::client::ProviderTarget {
        crate::client::ProviderTarget {
            target: Some(crate::client::provider_target::Target::Emby(
                crate::client::EmbyTarget {
                    item_id: item_id.to_string(),
                },
            )),
        }
    }

    fn alist_target(relative_path: &str) -> crate::client::ProviderTarget {
        crate::client::ProviderTarget {
            target: Some(crate::client::provider_target::Target::Alist(
                crate::client::AlistTarget {
                    relative_path: relative_path.to_string(),
                },
            )),
        }
    }

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
            is_banned: false,
            avatar_url: "https://cdn.example.com/avatar.webp".into(),
            avatar: None,
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
            avatar_url: "https://cdn.example.com/bob.webp".into(),
            avatar: None,
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
            settings: Some(room_settings()),
            created_at: 1_700_000_000,
            member_count: 5,
            description: "Synchronized movie room".into(),
            updated_at: 1_700_001_000,
            is_banned: false,
            availability: crate::client::ResourceAvailability::Available.into(),
            version: 7,
            cover: None,
            presence: None,
            creator: None,
            category: None,
            labels: Vec::new(),
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
            connection_count: 2,
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
            position: 123.456,
            speed: 1.5,
            is_playing: true,
            updated_at: 1_700_000_000,
            version: 42,
            playing_playlist_id: "playlist-1".into(),
            target: Some(emby_target("provider-item-42")),
            target_hash: "sample-target-hash".into(),
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
            display_position: "top".into(),
            display_color: "#ffffff".into(),
            client_message_id: "client-1".into(),
            status: crate::client::ChatMessageStatus::Active as i32,
            version: 1,
            edited_at: 0,
            deleted_at: 0,
            reply_to_message_id: String::new(),
            attachments: Vec::new(),
            deleted_by_user_id: String::new(),
            delete_reason: String::new(),
            playback_media_id: String::new(),
            playback_playlist_id: String::new(),
            playback_target: None,
            playback_target_hash: String::new(),
            playback_position_seconds: None,
            reactions: vec![crate::client::ChatReactionSummary {
                key: "like".into(),
                count: 2,
                reacted_by_me: true,
            }],
            reaction_count: 2,
            metadata: None,
            mentions: vec![crate::client::ChatMention {
                user_id: "usr_1".into(),
                username: "alice".into(),
                start: 0,
                length: 6,
            }],
            pin: None,
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
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: "client-1".into(),
                    attachments: Vec::new(),
                    reply_to_message_id: String::new(),
                    metadata: None,
                    mentions: Vec::new(),
                },
            )),
        };
        let bytes = msg.encode_to_vec();
        let decoded = crate::client::ClientMessage::decode(bytes.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_server_message_playback_state_resource() {
        let msg = crate::client::ServerMessage {
            message: Some(crate::client::server_message::Message::ResourceEvent(
                crate::client::ResourceEvent {
                    observe_id: "playback_state".into(),
                    payload: Some(crate::client::resource_event::Payload::PlaybackState(
                        crate::client::PlaybackState {
                            room_id: "room-abc".into(),
                            playing_media_id: "media-1".into(),
                            position: 0.0,
                            speed: 1.0,
                            is_playing: false,
                            updated_at: 0,
                            version: 1,
                            playing_playlist_id: String::new(),
                            target: None,
                            target_hash: String::new(),
                        },
                    )),
                    event_cursor: None,
                },
            )),
        };
        let bytes = msg.encode_to_vec();
        let decoded = crate::client::ServerMessage::decode(bytes.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_room_settings_resource_payload() {
        let msg = crate::client::ResourceEvent {
            observe_id: "room_settings".into(),
            payload: Some(crate::client::resource_event::Payload::RoomSettings(
                crate::client::GetRoomSettingsResponse {
                    settings: Some(room_settings()),
                    version: 1,
                },
            )),
            event_cursor: None,
        };
        let bytes = msg.encode_to_vec();
        let decoded = crate::client::ResourceEvent::decode(bytes.as_slice()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_room_member_event() {
        let msg = crate::client::RoomMemberEvent {
            event_id: "evt-123".into(),
            room_id: "room-abc".into(),
            kind: crate::client::RoomMemberEventKind::Joined.into(),
            member: Some(crate::common::RoomMember {
                room_id: "room-abc".into(),
                user_id: "user-123".into(),
                username: "member".into(),
                role: crate::common::RoomMemberRole::Member.into(),
                permissions: 0xFF00,
                added_permissions: 0,
                removed_permissions: 0,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
                joined_at: 1,
                is_online: true,
                connection_count: 1,
            }),
            user_id: "user-123".into(),
            guest_id: String::new(),
            username: "member".into(),
            actor_user_id: String::new(),
            reason: String::new(),
            occurred_at: 1,
            sequence: 2,
        };
        let bytes = msg.encode_to_vec();
        let decoded = crate::client::RoomMemberEvent::decode(bytes.as_slice()).unwrap();
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
    fn roundtrip_ice_server() {
        let server = crate::client::IceServer {
            urls: vec![
                "stun:stun.example.com:3478".into(),
                "stun:stun-backup.example.com:3478".into(),
            ],
            username: Some("turn-user".into()),
            credential: Some("turn-secret".into()),
        };
        let bytes = server.encode_to_vec();
        let decoded = crate::client::IceServer::decode(bytes.as_slice()).unwrap();
        assert_eq!(server, decoded);
    }

    #[test]
    fn default_values_are_zero() {
        let user = crate::client::User::default();
        assert_eq!(user.id, "");
        assert_eq!(user.username, "");
        assert_eq!(user.email, "");
        assert_eq!(user.role, 0); // UNSPECIFIED
        assert_eq!(user.status, 0); // UNSPECIFIED
        assert_eq!(user.created_at, 0);
        assert!(!user.is_banned);
    }

    #[test]
    fn empty_message_decodes_to_default() {
        let decoded = crate::client::User::decode(&[] as &[u8]).unwrap();
        assert_eq!(decoded, crate::client::User::default());
    }

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
    fn enum_roundtrip_provider_instance_status() {
        for status in [
            crate::providers::common::ProviderInstanceStatus::Unspecified,
            crate::providers::common::ProviderInstanceStatus::Connected,
            crate::providers::common::ProviderInstanceStatus::Disconnected,
            crate::providers::common::ProviderInstanceStatus::Error,
        ] {
            let pi = crate::providers::common::ProviderInstance {
                status: status.into(),
                ..Default::default()
            };
            let bytes = pi.encode_to_vec();
            let decoded =
                crate::providers::common::ProviderInstance::decode(bytes.as_slice()).unwrap();
            assert_eq!(decoded.status, i32::from(status));
        }
    }

    #[test]
    fn json_roundtrip_user() {
        let user = crate::client::User {
            id: "user-123".into(),
            username: "alice".into(),
            email: "alice@example.com".into(),
            role: crate::common::UserRole::Admin.into(),
            status: crate::common::UserStatus::Active.into(),
            created_at: 1_700_000_000,
            is_banned: false,
            avatar_url: "https://cdn.example.com/avatar.webp".into(),
            avatar: None,
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
            position: 123.456,
            speed: 1.5,
            is_playing: true,
            updated_at: 1_700_000_000,
            version: 42,
            playing_playlist_id: "playlist-1".into(),
            target: None,
            target_hash: String::new(),
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
                    display_position: String::new(),
                    display_color: String::new(),
                    client_message_id: String::new(),
                    attachments: Vec::new(),
                    reply_to_message_id: String::new(),
                    metadata: None,
                    mentions: Vec::new(),
                },
            )),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: crate::client::ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_user_notification() {
        let notification = crate::client::UserNotification {
            notification_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            notification_type: crate::client::NotificationType::RoomInvitation as i32,
            title: "Room Invitation".to_string(),
            content: "You have been invited to join a room".to_string(),
            data: Some(crate::client::NotificationData {
                room_id: Some("room123".to_string()),
                room_name: Some("Test Room".to_string()),
                ..Default::default()
            }),
            timestamp: 1_704_067_200, // 2024-01-01 00:00:00 UTC (seconds)
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
            notification_type: crate::client::NotificationType::SystemAnnouncement as i32,
            title: "System Update".to_string(),
            content: "Server will restart in 10 minutes".to_string(),
            data: Some(crate::client::NotificationData::default()),
            timestamp: 1_704_067_200,
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
                assert_eq!(
                    decoded_notif.notification_type,
                    notification.notification_type
                );
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
                notification_type: crate::client::NotificationType::SystemAnnouncement as i32,
                title: "Test".to_string(),
                content: "Test notification".to_string(),
                data: Some(crate::client::NotificationData::default()),
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

    /// Verify that UserNotification.timestamp uses Unix seconds (not milliseconds).
    /// This test ensures consistency with other timestamp fields in the proto.
    /// A valid Unix timestamp in seconds for year 2024 should be around 1.7 billion,
    /// while milliseconds would be around 1.7 trillion.
    #[test]
    fn notification_timestamp_uses_seconds_not_millis() {
        // 2024-01-01 00:00:00 UTC in SECONDS
        let timestamp_seconds = 1_704_067_200_i64;

        let notification = crate::client::UserNotification {
            notification_id: "test-id".to_string(),
            notification_type: crate::client::NotificationType::SystemAnnouncement as i32,
            title: "Test".to_string(),
            content: "Test".to_string(),
            data: Some(crate::client::NotificationData::default()),
            timestamp: timestamp_seconds,
        };

        // Roundtrip encode/decode
        let bytes = notification.encode_to_vec();
        let decoded = crate::client::UserNotification::decode(bytes.as_slice()).unwrap();

        // Verify the timestamp is preserved as seconds
        assert_eq!(decoded.timestamp, timestamp_seconds);

        // Sanity check: a second-based timestamp for 2024 should be < 2 billion
        // A millisecond-based timestamp would be > 1 trillion
        assert!(
            decoded.timestamp < 2_000_000_000,
            "Timestamp {} looks like milliseconds, expected seconds",
            decoded.timestamp
        );
    }

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
            is_banned: false,
            banned_at: 0,
            banned_by: String::new(),
            banned_reason: String::new(),
            avatar_url: "https://cdn.example.com/admin.webp".into(),
            presence: None,
        };
        let bytes = user.encode_to_vec();
        let decoded = crate::admin::AdminUser::decode(bytes.as_slice()).unwrap();
        assert_eq!(user, decoded);
    }

    #[test]
    fn roundtrip_set_user_password_request_with_audit() {
        let req = crate::admin::SetUserPasswordRequest {
            user_id: "user-123".into(),
            password: "NewPassword123!".into(),
            reason: "security incident".into(),
        };
        let bytes = req.encode_to_vec();
        let decoded = crate::admin::SetUserPasswordRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.user_id, "user-123");
        assert_eq!(decoded.password, "NewPassword123!");
        assert_eq!(decoded.reason, "security incident");
    }

    #[test]
    fn http_json_start_playback_request_accepts_structured_target() {
        let json =
            r#"{"playlistId":"playlist-123","target":{"emby":{"itemId":"provider-item-1"}}}"#;

        let decoded: crate::client::StartPlaybackRequest =
            serde_json::from_str(json).expect("HTTP JSON should deserialize into proto request");

        assert_eq!(decoded.playlist_id, "playlist-123");
        assert_eq!(decoded.target, Some(emby_target("provider-item-1")));
    }

    #[test]
    fn http_json_create_playlist_request_accepts_proto_json_source_config() {
        let json = r#"{"name":"Season 1","sourceProvider":3,"sourceConfig":{"alist":{"serverId":"alist-main","path":"/tv"}}}"#;

        let decoded: crate::client::CreatePlaylistRequest =
            serde_json::from_str(json).expect("HTTP JSON should deserialize into proto request");

        assert_eq!(decoded.name, "Season 1");
        assert_eq!(
            decoded.source_provider,
            crate::source_config::SourceProvider::Alist as i32
        );
        assert_eq!(
            decoded.source_config,
            Some(alist_playlist_source_config("/tv"))
        );

        let encoded = serde_json::to_value(&decoded).expect("request should serialize");
        assert_eq!(
            encoded["sourceProvider"],
            crate::source_config::SourceProvider::Alist as i32
        );
        assert_eq!(
            encoded["sourceConfig"],
            serde_json::to_value(alist_playlist_source_config("/tv"))
                .expect("source config should serialize")
        );
    }

    #[test]
    fn http_json_create_playlist_request_rejects_untyped_source_config() {
        let json = r#"{"name":"Season 1","sourceProvider":3,"sourceConfig":[1,2,3]}"#;

        serde_json::from_str::<crate::client::CreatePlaylistRequest>(json)
            .expect_err("source_config requires a typed provider object");
    }

    #[test]
    fn http_json_provider_target_serialization_uses_object() {
        let response = crate::client::PlaylistBrowsePathNode {
            playlist_id: "playlist-1".to_string(),
            name: "Season 1".to_string(),
            target: Some(alist_target("/Season 1")),
        };

        let json = serde_json::to_value(&response).expect("target should serialize");

        assert_eq!(
            json["target"],
            serde_json::to_value(alist_target("/Season 1")).expect("target should serialize")
        );
    }

    #[test]
    fn http_json_empty_bytes_field_is_omitted() {
        let response = crate::client::PlaylistBrowsePathNode {
            playlist_id: "playlist-1".to_string(),
            name: "Season 1".to_string(),
            target: None,
        };

        let json = serde_json::to_value(&response).expect("empty bytes should serialize");

        assert!(json.get("target").is_none());
    }

    #[test]
    fn http_json_edit_media_request_allows_path_supplied_media_id() {
        let json = r#"{"name":"Updated title"}"#;

        let decoded: crate::client::EditMediaRequest =
            serde_json::from_str(json).expect("media_id should be allowed to come from the path");

        assert_eq!(decoded.media_id, "");
        assert_eq!(decoded.name, "Updated title");
    }

    #[test]
    fn http_json_set_user_password_request_accepts_password_and_reason_fields() {
        let json = r#"{"password":"NewPassword123!","reason":"support reset"}"#;

        let decoded: crate::admin::SetUserPasswordRequest =
            serde_json::from_str(json).expect("HTTP JSON should deserialize into proto");

        assert_eq!(decoded.password, "NewPassword123!");
        assert_eq!(decoded.reason, "support reset");
    }

    #[test]
    fn http_json_update_user_username_request_accepts_new_username_field() {
        let json = r#"{"newUsername":"patched-name"}"#;

        let decoded: crate::admin::UpdateUserUsernameRequest =
            serde_json::from_str(json).expect("HTTP JSON should deserialize into proto");

        assert_eq!(decoded.user_id, "");
        assert_eq!(decoded.new_username, "patched-name");
    }

    #[test]
    fn http_json_ban_user_request_allows_path_supplied_user_id() {
        let json = r#"{"reason":"spam"}"#;

        let decoded: crate::admin::BanUserRequest =
            serde_json::from_str(json).expect("user_id should be allowed to come from the path");

        assert_eq!(decoded.user_id, "");
        assert_eq!(decoded.reason, "spam");
    }

    #[test]
    fn http_json_update_room_password_request_accepts_new_password_field() {
        let json = r#"{"newPassword":"new-room-password"}"#;

        let decoded: crate::admin::UpdateRoomPasswordRequest =
            serde_json::from_str(json).expect("HTTP JSON should deserialize into proto");

        assert_eq!(decoded.room_id, "");
        assert_eq!(decoded.new_password, "new-room-password");
    }

    #[test]
    fn http_json_update_room_settings_request_accepts_structured_settings() {
        let json = r#"{"settings":{"allowGuestJoin":true,"maxMembers":8,"chatEnabled":true,"autoPlay":{"enabled":true,"mode":1,"delay":0}}}"#;

        let decoded: crate::admin::UpdateRoomSettingsRequest =
            serde_json::from_str(json).expect("HTTP JSON should deserialize into proto request");

        assert_eq!(decoded.room_id, "");
        assert_eq!(decoded.settings, Some(room_settings_patch()));
    }

    #[test]
    fn http_json_client_update_room_settings_request_accepts_structured_settings() {
        let json = r#"{"settings":{"allowGuestJoin":true,"maxMembers":8,"chatEnabled":true,"autoPlay":{"enabled":true,"mode":1,"delay":0}}}"#;

        let decoded: crate::client::UpdateRoomSettingsRequest =
            serde_json::from_str(json).expect("HTTP JSON should deserialize into proto request");

        assert_eq!(decoded.settings, Some(room_settings_patch()));
    }

    #[test]
    fn http_json_client_update_room_settings_request_rejects_array_settings() {
        let json = r#"{"settings":"WzEsMiwzXQ=="}"#;

        serde_json::from_str::<crate::client::UpdateRoomSettingsRequest>(json)
            .expect_err("room settings must be a structured object");
    }

    #[test]
    fn http_json_update_user_role_request_accepts_numeric_role() {
        let json = format!(r#"{{"role":{}}}"#, crate::common::UserRole::Admin as i32);

        let decoded: crate::admin::UpdateUserRoleRequest =
            serde_json::from_str(&json).expect("numeric role should deserialize into proto enum");

        assert_eq!(decoded.user_id, "");
        assert_eq!(decoded.role, crate::common::UserRole::Admin as i32);
    }

    #[test]
    fn http_json_update_user_role_request_rejects_string_role() {
        let err =
            serde_json::from_str::<crate::admin::UpdateUserRoleRequest>(r#"{"role":"admin"}"#)
                .expect_err("string role should be rejected");

        assert!(err.is_data());
    }

    #[test]
    fn http_json_update_member_permissions_request_accepts_numeric_role() {
        let json = format!(
            r#"{{"role":{},"addedPermissions":7}}"#,
            crate::common::RoomMemberRole::Guest as i32
        );

        let decoded: crate::client::UpdateMemberPermissionsRequest = serde_json::from_str(&json)
            .expect("numeric room role should deserialize into proto enum");

        assert_eq!(decoded.user_id, "");
        assert_eq!(decoded.role, crate::common::RoomMemberRole::Guest as i32);
        assert_eq!(decoded.added_permissions, 7);
    }

    #[test]
    fn http_json_update_member_permissions_request_defaults_missing_role() {
        let decoded: crate::client::UpdateMemberPermissionsRequest =
            serde_json::from_str(r#"{"addedPermissions":7}"#)
                .expect("missing role should default to unspecified");

        assert_eq!(
            decoded.role,
            crate::common::RoomMemberRole::Unspecified as i32
        );
        assert_eq!(decoded.added_permissions, 7);
    }

    #[test]
    fn http_json_update_member_permissions_request_rejects_string_role() {
        let err = serde_json::from_str::<crate::client::UpdateMemberPermissionsRequest>(
            r#"{"role":"guest","addedPermissions":7}"#,
        )
        .expect_err("string room role should be rejected");

        assert!(err.is_data());
    }

    #[test]
    fn http_json_kick_member_request_defaults_user_id_only() {
        let decoded: crate::client::KickMemberRequest =
            serde_json::from_str(r#"{"kickCooldownSeconds":300}"#)
                .expect("path-populated user_id should default");

        assert_eq!(decoded.user_id, "");
        assert_eq!(decoded.kick_cooldown_seconds, 300);
    }

    #[test]
    fn http_json_admin_kick_stream_request_defaults_reason() {
        let json = r#"{"roomId":"room-1","mediaId":"media-1"}"#;

        let decoded: crate::admin::KickStreamRequest =
            serde_json::from_str(json).expect("optional reason should default");

        assert_eq!(decoded.room_id, "room-1");
        assert_eq!(decoded.media_id, "media-1");
        assert_eq!(decoded.reason, "");
    }

    #[test]
    fn http_json_admin_create_user_request_defaults_optional_email() {
        let json = r#"{"username":"alice","role":3,"status":1}"#;

        let decoded: crate::admin::CreateUserRequest =
            serde_json::from_str(json).expect("optional email should default");

        assert_eq!(decoded.username, "alice");
        assert_eq!(decoded.email, "");
        assert_eq!(decoded.role, crate::common::UserRole::User as i32);
        assert_eq!(decoded.status, crate::common::UserStatus::Active as i32);
    }

    #[test]
    fn http_json_provider_common_add_provider_instance_request_defaults_optional_scalars() {
        let json = r#"{"name":"emby-main","endpoint":"https://provider.example.com"}"#;

        let decoded: crate::providers::common::AddProviderInstanceRequest =
            serde_json::from_str(json).expect("optional provider fields should default");

        assert_eq!(decoded.name, "emby-main");
        assert_eq!(decoded.endpoint, "https://provider.example.com");
        assert_eq!(decoded.comment, "");
        assert_eq!(decoded.timeout_seconds, 0);
        assert!(!decoded.tls);
        assert!(!decoded.insecure_tls);
        assert!(decoded.providers.is_empty());
        assert_eq!(decoded.jwt_secret, None);
        assert_eq!(decoded.custom_ca, None);
    }

    #[test]
    fn http_json_provider_common_update_provider_instance_request_defaults_path_and_repeated_fields(
    ) {
        let json = r#"{"endpoint":"https://provider.example.com"}"#;

        let decoded: crate::providers::common::UpdateProviderInstanceRequest =
            serde_json::from_str(json)
                .expect("path-populated provider update fields should default");

        assert!(decoded.name.is_empty());
        assert_eq!(
            decoded.endpoint.as_deref(),
            Some("https://provider.example.com")
        );
        assert!(decoded.providers.is_empty());
        assert_eq!(decoded.jwt_secret, None);
        assert_eq!(decoded.custom_ca, None);
        assert_eq!(decoded.clear_comment, None);
        assert_eq!(decoded.clear_jwt_secret, None);
        assert_eq!(decoded.clear_custom_ca, None);
    }

    #[test]
    fn validate_admin_create_user_request_allows_empty_optional_email() {
        crate::validate(&crate::admin::CreateUserRequest {
            username: "admin_created".into(),
            email: String::new(),
            role: crate::common::UserRole::Admin as i32,
            status: crate::common::UserStatus::Active as i32,
            password: String::new(),
        })
        .unwrap();
    }

    #[test]
    fn http_json_move_playlist_request_deserializes_flat_anchor_fields() {
        let decoded: crate::client::MovePlaylistRequest =
            serde_json::from_str(r#"{"beforePlaylistId":"playlist-2"}"#)
                .expect("flat oneof fields should deserialize");

        assert!(decoded.playlist_id.is_empty());
        assert_eq!(
            decoded.anchor,
            Some(
                crate::client::move_playlist_request::Anchor::BeforePlaylistId(
                    "playlist-2".to_string()
                )
            )
        );
    }

    #[test]
    fn http_json_move_playlist_request_defaults_missing_anchor_for_later_validation() {
        let decoded: crate::client::MovePlaylistRequest =
            serde_json::from_str(r"{}").expect("transport deserialization should succeed");

        assert!(decoded.playlist_id.is_empty());
        assert!(decoded.anchor.is_none());
    }

    #[test]
    fn http_json_move_playlist_request_rejects_multiple_anchors() {
        let err = serde_json::from_str::<crate::client::MovePlaylistRequest>(
            r#"{"beforePlaylistId":"playlist-1","afterPlaylistId":"playlist-2"}"#,
        )
        .expect_err("multiple anchors must be rejected");

        assert!(err.is_data());
    }

    #[test]
    fn http_json_move_playlist_request_rejects_unknown_fields() {
        let err = serde_json::from_str::<crate::client::MovePlaylistRequest>(
            r#"{"before":"playlist-2","playlistId":"playlist-1"}"#,
        )
        .expect_err("unknown JSON fields should be rejected");

        assert!(err.is_data());
    }

    #[test]
    fn http_json_create_room_request_defaults_optional_fields() {
        let decoded: crate::client::CreateRoomRequest =
            serde_json::from_str(r#"{"name":"Movie Night"}"#)
                .expect("missing create-room optional fields should default");

        assert_eq!(decoded.name, "Movie Night");
        assert!(decoded.settings.is_none());
        assert_eq!(decoded.description, "");
    }

    #[test]
    fn http_json_create_room_request_accepts_structured_settings() {
        let decoded: crate::client::CreateRoomRequest = serde_json::from_str(
            r#"{"name":"Movie Night","description":"","settings":{"allowGuestJoin":true,"maxMembers":8,"allowAutoJoin":true,"chatEnabled":true,"autoPlay":{"enabled":true,"mode":1,"delay":0}}}"#,
        )
        .expect("room settings object should deserialize");

        assert_eq!(decoded.settings, Some(room_settings()));
    }

    #[test]
    fn roundtrip_bilibili_parse_request() {
        let req = crate::providers::bilibili::ParseRequest {
            url: "https://bilibili.com/video/BV123".into(),
            instance_name: "bilibili_main".into(),
        };
        let bytes = req.encode_to_vec();
        let decoded = crate::providers::bilibili::ParseRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.url, req.url);
        assert_eq!(decoded.instance_name, req.instance_name);
    }

    #[test]
    fn roundtrip_alist_login_request() {
        let req = crate::providers::alist::LoginRequest {
            host: "https://alist.example.com".into(),
            username: "user".into(),
            credential: Some(
                crate::providers::alist::login_request::Credential::Password("pass".into()),
            ),
            otp_code: String::new(),
            otp_secret: String::new(),
            instance_name: "alist_main".into(),
        };
        let bytes = req.encode_to_vec();
        let decoded = crate::providers::alist::LoginRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.host, req.host);
        assert_eq!(decoded.username, "user");
        assert_eq!(
            decoded.credential,
            Some(crate::providers::alist::login_request::Credential::Password("pass".into()))
        );
    }

    #[test]
    fn roundtrip_emby_login_request() {
        let req = crate::providers::emby::LoginRequest {
            host: "https://emby.example.com".into(),
            username: "admin".into(),
            credential: Some(crate::providers::emby::login_request::Credential::ApiKey(
                "secret-api-key".into(),
            )),
            instance_name: "emby_main".into(),
        };
        let bytes = req.encode_to_vec();
        let decoded = crate::providers::emby::LoginRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.host, req.host);
        assert_eq!(decoded.username, "admin");
        assert_eq!(
            decoded.credential,
            Some(crate::providers::emby::login_request::Credential::ApiKey(
                "secret-api-key".into()
            ))
        );
    }

    #[test]
    fn http_json_alist_login_request_defaults_optional_fields() {
        let json = r#"{"host":"https://alist.example.com","username":"user","password":"pass"}"#;

        let decoded: crate::providers::alist::LoginRequest =
            serde_json::from_str(json).expect("missing optional provider fields should default");

        assert_eq!(decoded.host, "https://alist.example.com");
        assert_eq!(decoded.username, "user");
        assert_eq!(
            decoded.credential,
            Some(crate::providers::alist::login_request::Credential::Password("pass".into()))
        );
        assert_eq!(decoded.instance_name, "");
    }

    #[test]
    fn http_json_alist_list_request_defaults_optional_query_like_fields() {
        let json = r#"{"serverId":"server-1","path":"/tv"}"#;

        let decoded: crate::providers::alist::ListRequest =
            serde_json::from_str(json).expect("missing optional provider fields should default");

        assert_eq!(decoded.server_id, "server-1");
        assert_eq!(decoded.path, "/tv");
        assert_eq!(decoded.password, "");
        assert_eq!(decoded.page, 0);
        assert_eq!(decoded.per_page, 0);
        assert!(!decoded.refresh);
        assert_eq!(decoded.instance_name, "");
    }

    #[test]
    fn http_json_bilibili_login_qr_request_defaults_instance_name() {
        let decoded: crate::providers::bilibili::LoginQrRequest =
            serde_json::from_str("{}").expect("missing optional instance_name should default");

        assert_eq!(decoded.instance_name, "");
    }

    #[test]
    fn http_json_emby_login_request_defaults_instance_name() {
        let json =
            r#"{"host":"https://emby.example.com","username":"admin","apiKey":"secret-api-key"}"#;

        let decoded: crate::providers::emby::LoginRequest =
            serde_json::from_str(json).expect("missing optional instance_name should default");

        assert_eq!(decoded.host, "https://emby.example.com");
        assert_eq!(decoded.username, "admin");
        assert_eq!(
            decoded.credential,
            Some(crate::providers::emby::login_request::Credential::ApiKey(
                "secret-api-key".into()
            ))
        );
        assert_eq!(decoded.instance_name, "");
    }

    #[test]
    fn http_json_provider_login_requests_reject_duplicate_credentials() {
        let alist_err = serde_json::from_str::<crate::providers::alist::LoginRequest>(
            r#"{"host":"https://alist.example.com","username":"user","password":"pass","hashedPassword":"hash"}"#,
        )
        .expect_err("alist login should reject multiple credentials");
        assert!(alist_err.is_data());

        let emby_err = serde_json::from_str::<crate::providers::emby::LoginRequest>(
            r#"{"host":"https://emby.example.com","username":"admin","password":"pass","apiKey":"token"}"#,
        )
        .expect_err("emby login should reject multiple credentials");
        assert!(emby_err.is_data());
    }

    #[test]
    fn http_json_provider_requests_reject_missing_required_fields() {
        let emby: crate::providers::emby::GetMeRequest =
            serde_json::from_str("{}").expect("proto3 defaults missing strings");
        let emby_err =
            crate::validate(&emby).expect_err("missing server_id should fail validation");
        assert!(validation_error_text(&emby_err).contains("server_id"));

        let alist: crate::providers::alist::LoginRequest =
            serde_json::from_str("{}").expect("proto3 defaults missing strings");
        let alist_err =
            crate::validate(&alist).expect_err("missing host and username should fail validation");
        let alist_error = validation_error_text(&alist_err);
        assert!(alist_error.contains("host") || alist_error.contains("username"));

        let bilibili: crate::providers::bilibili::CheckQrRequest =
            serde_json::from_str("{}").expect("proto3 defaults missing strings");
        let bilibili_err =
            crate::validate(&bilibili).expect_err("missing QR key should fail validation");
        assert!(validation_error_text(&bilibili_err).contains("key"));
    }

    #[test]
    fn validate_opaque_registration_request_rejects_invalid_username_email_and_payload() {
        let request = crate::client::StartOpaqueRegistrationRequest {
            username: "ab".into(),
            email: Some("not-an-email".into()),
            registration_request: Vec::new(),
        };

        let error = validation_error_text(&crate::validate(&request).unwrap_err());

        assert!(error.contains("username"), "{error}");
        assert!(error.contains("email"), "{error}");
        assert!(error.contains("registration_request"), "{error}");
    }

    #[test]
    fn validate_opaque_registration_request_accepts_valid_payload() {
        let request = crate::client::StartOpaqueRegistrationRequest {
            username: "valid_user".into(),
            email: Some("valid@example.com".into()),
            registration_request: vec![1],
        };

        crate::validate(&request).unwrap();
    }

    #[test]
    fn http_json_start_passkey_login_accepts_discoverable_payload() {
        let request =
            serde_json::from_str::<crate::client::StartPasskeyLoginRequest>("{}").unwrap();

        assert!(request.identifier.is_none());
        crate::validate(&request).unwrap();
    }

    #[test]
    fn http_json_login_identifier_transports_accept_flat_fields() {
        let opaque: crate::client::StartOpaqueLoginRequest =
            serde_json::from_str(r#"{"email":"alice@example.com","credentialRequest":"AQID"}"#)
                .expect("OPAQUE login request should deserialize flat email");
        assert!(matches!(
            opaque.identifier,
            Some(crate::client::start_opaque_login_request::Identifier::Email(ref email))
                if email == "alice@example.com"
        ));
        assert_eq!(opaque.credential_request, vec![1, 2, 3]);

        let direct_password: crate::client::LoginWithDirectPasswordRequest =
            serde_json::from_str(r#"{"username":"alice","password":"StrongPass1"}"#)
                .expect("direct password login request should deserialize flat username");
        assert!(matches!(
            direct_password.identifier,
            Some(crate::client::login_with_direct_password_request::Identifier::Username(
                ref username
            )) if username == "alice"
        ));
        assert_eq!(direct_password.password, "StrongPass1");

        let passkey: crate::client::StartPasskeyLoginRequest =
            serde_json::from_str(r#"{"username":"alice"}"#)
                .expect("passkey login request should deserialize flat username");
        assert!(matches!(
            passkey.identifier,
            Some(crate::client::start_passkey_login_request::Identifier::Username(ref username))
                if username == "alice"
        ));
    }

    #[test]
    fn http_json_direct_password_and_email_registration_payloads_are_supported() {
        let direct_registration: crate::client::RegisterWithDirectPasswordRequest =
            serde_json::from_str(
                r#"{"username":"alice","email":"alice@example.com","password":"StrongPass1"}"#,
            )
            .expect("direct password registration should deserialize");
        assert_eq!(direct_registration.username, "alice");
        assert_eq!(
            direct_registration.email.as_deref(),
            Some("alice@example.com")
        );
        assert_eq!(direct_registration.password, "StrongPass1");
        crate::validate(&direct_registration).expect("direct registration should validate");

        let direct_username_login: crate::client::LoginWithDirectPasswordRequest =
            serde_json::from_str(r#"{"username":"alice","password":"StrongPass1"}"#)
                .expect("direct password username login should deserialize");
        assert!(matches!(
            direct_username_login.identifier,
            Some(crate::client::login_with_direct_password_request::Identifier::Username(
                ref username
            )) if username == "alice"
        ));
        assert_eq!(direct_username_login.password, "StrongPass1");
        crate::validate(&direct_username_login)
            .expect("direct password username login should validate");

        let direct_email_login: crate::client::LoginWithDirectPasswordRequest =
            serde_json::from_str(r#"{"email":"alice@example.com","password":"StrongPass1"}"#)
                .expect("direct password email login should deserialize");
        assert!(matches!(
            direct_email_login.identifier,
            Some(crate::client::login_with_direct_password_request::Identifier::Email(ref email))
                if email == "alice@example.com"
        ));
        assert_eq!(direct_email_login.password, "StrongPass1");
        crate::validate(&direct_email_login).expect("direct password email login should validate");

        let email_registration: crate::client::RequestEmailRegistrationRequest =
            serde_json::from_str(r#"{"username":"alice","email":"alice@example.com"}"#)
                .expect("email registration request should deserialize");
        assert_eq!(email_registration.username, "alice");
        assert_eq!(email_registration.email, "alice@example.com");
        crate::validate(&email_registration).expect("email registration request should validate");

        let email_confirmation: crate::client::ConfirmEmailRegistrationRequest =
            serde_json::from_str(r#"{"emailToken":"token-123","password":"StrongPass1"}"#)
                .expect("email registration confirmation should deserialize");
        assert_eq!(email_confirmation.email_token, "token-123");
        assert_eq!(email_confirmation.password, "StrongPass1");
        crate::validate(&email_confirmation)
            .expect("email registration confirmation should validate");
    }

    #[test]
    fn http_json_login_identifier_transports_reject_unknown_fields() {
        assert!(
            serde_json::from_str::<crate::client::StartOpaqueLoginRequest>(
                r#"{"email":"alice@example.com","credentialRequest":"AQID","extra":true}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<crate::client::LoginWithDirectPasswordRequest>(
                r#"{"username":"alice","password":"StrongPass1","extra":true}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<crate::client::StartPasskeyLoginRequest>(
                r#"{"username":"alice","extra":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn http_json_provider_login_requests_reject_unknown_fields() {
        assert!(
            serde_json::from_str::<crate::providers::alist::LoginRequest>(
                r#"{"host":"https://alist.example.com","username":"alice","password":"password","extra":true}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<crate::providers::emby::LoginRequest>(
                r#"{"host":"https://emby.example.com","username":"alice","apiKey":"key","extra":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn validate_start_passkey_registration_allows_empty_optional_email() {
        let request = crate::client::StartPasskeyRegistrationRequest {
            username: "valid_user".into(),
            email: String::new(),
            name: String::new(),
        };

        crate::validate(&request).unwrap();
    }

    #[test]
    fn validate_set_username_request_rejects_invalid_username() {
        let request = crate::client::SetUsernameRequest {
            new_username: "bad name".into(),
        };

        let error = validation_error_text(&crate::validate(&request).unwrap_err());

        assert!(error.contains("new_username"), "{error}");
    }

    #[test]
    fn validate_create_room_request_rejects_html_name_and_long_description() {
        let request = crate::client::CreateRoomRequest {
            name: "<script>alert(1)</script>".into(),
            settings: None,
            description: "x".repeat(501),
            password: String::new(),
            category_id: String::new(),
            label_ids: Vec::new(),
        };

        let error = validation_error_text(&crate::validate(&request).unwrap_err());

        assert!(error.contains("name"), "{error}");
        assert!(error.contains("description"), "{error}");
    }

    #[test]
    fn validate_join_room_request_requires_room_id_length() {
        let request = crate::client::JoinRoomRequest {
            room_id: String::new(),
            password: String::new(),
        };

        let error = validation_error_text(&crate::validate(&request).unwrap_err());
        assert!(error.contains("room_id"), "{error}");
    }

    #[test]
    fn validate_create_user_request_rejects_undefined_enum_values() {
        let request = crate::admin::CreateUserRequest {
            username: "valid_user".into(),
            email: "valid@example.com".into(),
            role: 99,
            status: 99,
            password: String::new(),
        };

        let error = validation_error_text(&crate::validate(&request).unwrap_err());

        assert!(error.contains("role"), "{error}");
        assert!(error.contains("status"), "{error}");
    }

    #[test]
    fn validate_list_notifications_request_rejects_invalid_pagination_and_enums() {
        let request = crate::client::ListNotificationsRequest {
            page: -1,
            page_size: 101,
            is_read: None,
            notification_type: Some(crate::client::NotificationType::Unspecified as i32),
            search: String::new(),
            sort_by: 99,
            sort_direction: 99,
        };

        let error = validation_error_text(&crate::validate(&request).unwrap_err());

        assert!(error.contains("page"), "{error}");
        assert!(error.contains("page_size"), "{error}");
        assert!(error.contains("notification_type"), "{error}");
        assert!(error.contains("sort_by"), "{error}");
        assert!(error.contains("sort_direction"), "{error}");
    }

    #[test]
    fn validate_list_notifications_request_accepts_defaultable_values() {
        let request = crate::client::ListNotificationsRequest {
            page: 0,
            page_size: 0,
            is_read: Some(true),
            notification_type: Some(crate::client::NotificationType::RoomInvitation as i32),
            search: "alert".into(),
            sort_by: crate::client::NotificationListSortBy::Unspecified as i32,
            sort_direction: crate::client::SortDirection::Unspecified as i32,
        };

        crate::validate(&request).unwrap();
    }

    #[test]
    fn validate_room_list_requests_reject_invalid_pagination_and_enums() {
        let list_rooms = crate::client::ListRoomsRequest {
            page: -1,
            page_size: 101,
            search: String::new(),
            sort_by: 99,
            sort_direction: 99,
            category_id: String::new(),
            label_ids: Vec::new(),
        };
        let my_rooms = crate::client::ListMyRoomsRequest {
            page: -1,
            page_size: 101,
            search: String::new(),
            status: 99,
            is_banned: None,
            relation: 99,
            sort_by: 99,
            sort_direction: 99,
        };
        let members = crate::client::GetRoomMembersRequest {
            page: -1,
            page_size: 101,
            search: String::new(),
            role: Some(99),
            sort_by: 99,
            sort_direction: 99,
        };

        for error in [
            validation_error_text(&crate::validate(&list_rooms).unwrap_err()),
            validation_error_text(&crate::validate(&my_rooms).unwrap_err()),
            validation_error_text(&crate::validate(&members).unwrap_err()),
        ] {
            assert!(error.contains("page"), "{error}");
            assert!(error.contains("page_size"), "{error}");
        }
    }

    #[test]
    fn validate_room_list_requests_accept_defaultable_values() {
        crate::validate(&crate::client::ListRoomsRequest {
            page: 0,
            page_size: 0,
            search: "room".into(),
            sort_by: crate::client::RoomListSortBy::Unspecified as i32,
            sort_direction: crate::client::SortDirection::Unspecified as i32,
            category_id: String::new(),
            label_ids: Vec::new(),
        })
        .unwrap();

        crate::validate(&crate::client::ListMyRoomsRequest {
            page: 0,
            page_size: 0,
            search: String::new(),
            status: crate::common::RoomStatus::Unspecified as i32,
            is_banned: Some(false),
            relation: crate::client::MyRoomRelation::All as i32,
            sort_by: crate::client::MyRoomListSortBy::Unspecified as i32,
            sort_direction: crate::client::SortDirection::Unspecified as i32,
        })
        .unwrap();

        crate::validate(&crate::client::GetRoomMembersRequest {
            page: 0,
            page_size: 0,
            search: String::new(),
            role: Some(crate::common::RoomMemberRole::Member as i32),
            sort_by: crate::client::RoomMemberListSortBy::Unspecified as i32,
            sort_direction: crate::client::SortDirection::Unspecified as i32,
        })
        .unwrap();
    }

    #[test]
    fn validate_playlist_list_requests_reject_invalid_pagination_and_enums() {
        let playlists = crate::client::ListPlaylistsRequest {
            parent_id: String::new(),
            page: -1,
            page_size: 101,
            search: String::new(),
            source_provider: crate::source_config::SourceProvider::Unspecified as i32,
            provider_instance_name: String::new(),
            dynamic_only: None,
            sort_by: 99,
            sort_direction: 99,
            availability: 99,
        };
        let items = crate::client::ListPlaylistItemsRequest {
            playlist_id: String::new(),
            target: None,
            page: -1,
            page_size: 101,
            search: String::new(),
            source_provider: crate::source_config::SourceProvider::Unspecified as i32,
            provider_instance_name: String::new(),
            sort_by: 99,
            sort_direction: 99,
            availability: 99,
            refresh: false,
        };

        for error in [
            validation_error_text(&crate::validate(&playlists).unwrap_err()),
            validation_error_text(&crate::validate(&items).unwrap_err()),
        ] {
            assert!(error.contains("page"), "{error}");
            assert!(error.contains("page_size"), "{error}");
        }
    }

    #[test]
    fn validate_playlist_list_requests_accept_defaultable_values() {
        crate::validate(&crate::client::ListPlaylistsRequest {
            parent_id: String::new(),
            page: 0,
            page_size: 0,
            search: String::new(),
            source_provider: crate::source_config::SourceProvider::Unspecified as i32,
            provider_instance_name: String::new(),
            dynamic_only: Some(true),
            sort_by: crate::client::PlaylistListSortBy::Unspecified as i32,
            sort_direction: crate::client::SortDirection::Unspecified as i32,
            availability: crate::client::ResourceAvailabilityFilter::All as i32,
        })
        .unwrap();

        crate::validate(&crate::client::ListPlaylistItemsRequest {
            playlist_id: String::new(),
            target: None,
            page: 0,
            page_size: 0,
            search: String::new(),
            source_provider: crate::source_config::SourceProvider::Unspecified as i32,
            provider_instance_name: String::new(),
            sort_by: crate::client::MediaListSortBy::Unspecified as i32,
            sort_direction: crate::client::SortDirection::Unspecified as i32,
            availability: crate::client::ResourceAvailabilityFilter::All as i32,
            refresh: false,
        })
        .unwrap();
    }

    #[test]
    fn validate_admin_list_requests_reject_invalid_pagination_and_enums() {
        let provider_instances = crate::providers::common::ListProviderInstancesRequest {
            page: -1,
            page_size: 101,
            provider_type: crate::source_config::SourceProvider::Unspecified as i32,
            search: String::new(),
            enabled: None,
            tls: None,
            sort_by: 99,
            sort_direction: 99,
        };
        let users = crate::admin::ListUsersRequest {
            page: -1,
            page_size: 101,
            status: 99,
            role: 99,
            search: String::new(),
            is_banned: None,
            sort_by: 99,
            sort_direction: 99,
        };
        let user_rooms = crate::admin::GetUserRoomsRequest {
            user_id: "abc123def456".into(),
            page: -1,
            page_size: 101,
            status: 99,
            search: String::new(),
            is_banned: None,
            sort_by: 99,
            sort_direction: 99,
        };
        let rooms = crate::admin::ListRoomsRequest {
            page: -1,
            page_size: 101,
            status: 99,
            search: String::new(),
            creator_id: String::new(),
            is_banned: None,
            sort_by: 99,
            sort_direction: 99,
            category_id: String::new(),
            label_ids: Vec::new(),
        };
        let members = crate::admin::GetRoomMembersRequest {
            room_id: "abc123def456".into(),
            page: -1,
            page_size: 101,
            search: String::new(),
            role: 99,
            sort_by: 99,
            sort_direction: 99,
        };
        let admins = crate::admin::ListAdminsRequest {
            page: -1,
            page_size: 101,
            search: String::new(),
            sort_by: 99,
            sort_direction: 99,
        };
        let streams = crate::admin::ListActiveStreamsRequest {
            page: -1,
            page_size: 101,
            room_id: String::new(),
            user_id: String::new(),
            node_id: String::new(),
            search: String::new(),
            sort_by: 99,
            sort_direction: 99,
        };

        for error in [
            validation_error_text(&crate::validate(&provider_instances).unwrap_err()),
            validation_error_text(&crate::validate(&users).unwrap_err()),
            validation_error_text(&crate::validate(&user_rooms).unwrap_err()),
            validation_error_text(&crate::validate(&rooms).unwrap_err()),
            validation_error_text(&crate::validate(&members).unwrap_err()),
            validation_error_text(&crate::validate(&admins).unwrap_err()),
            validation_error_text(&crate::validate(&streams).unwrap_err()),
        ] {
            assert!(error.contains("page"), "{error}");
            assert!(error.contains("page_size"), "{error}");
        }
    }

    #[test]
    fn validate_admin_list_requests_accept_defaultable_values() {
        crate::validate(&crate::providers::common::ListProviderInstancesRequest {
            page: 0,
            page_size: 0,
            provider_type: crate::source_config::SourceProvider::Alist as i32,
            search: "edge".into(),
            enabled: Some(true),
            tls: Some(true),
            sort_by: crate::providers::common::ProviderInstanceListSortBy::Unspecified as i32,
            sort_direction: crate::providers::common::SortDirection::Unspecified as i32,
        })
        .unwrap();

        crate::validate(&crate::admin::ListUsersRequest {
            page: 0,
            page_size: 0,
            status: crate::common::UserStatus::Unspecified as i32,
            role: crate::common::UserRole::Unspecified as i32,
            search: "admin".into(),
            is_banned: Some(false),
            sort_by: crate::admin::UserListSortBy::Unspecified as i32,
            sort_direction: crate::admin::SortDirection::Unspecified as i32,
        })
        .unwrap();

        crate::validate(&crate::admin::GetUserRoomsRequest {
            user_id: "usr_abc123def456".into(),
            page: 0,
            page_size: 0,
            status: crate::common::RoomStatus::Unspecified as i32,
            search: String::new(),
            is_banned: Some(false),
            sort_by: crate::admin::RoomListSortBy::Unspecified as i32,
            sort_direction: crate::admin::SortDirection::Unspecified as i32,
        })
        .unwrap();

        crate::validate(&crate::admin::ListRoomsRequest {
            page: 0,
            page_size: 0,
            status: crate::common::RoomStatus::Unspecified as i32,
            search: "room".into(),
            creator_id: String::new(),
            is_banned: Some(false),
            sort_by: crate::admin::RoomListSortBy::Unspecified as i32,
            sort_direction: crate::admin::SortDirection::Unspecified as i32,
            category_id: String::new(),
            label_ids: Vec::new(),
        })
        .unwrap();

        crate::validate(&crate::admin::GetRoomMembersRequest {
            room_id: "room_abc123def456".into(),
            page: 0,
            page_size: 0,
            search: String::new(),
            role: crate::common::RoomMemberRole::Member as i32,
            sort_by: crate::admin::RoomMemberListSortBy::Unspecified as i32,
            sort_direction: crate::admin::SortDirection::Unspecified as i32,
        })
        .unwrap();

        crate::validate(&crate::admin::ListAdminsRequest {
            page: 0,
            page_size: 0,
            search: "root".into(),
            sort_by: crate::admin::UserListSortBy::Unspecified as i32,
            sort_direction: crate::admin::SortDirection::Unspecified as i32,
        })
        .unwrap();

        crate::validate(&crate::admin::ListActiveStreamsRequest {
            page: 0,
            page_size: 0,
            room_id: String::new(),
            user_id: String::new(),
            node_id: String::new(),
            search: "stream".into(),
            sort_by: crate::admin::ActiveStreamListSortBy::Unspecified as i32,
            sort_direction: crate::admin::SortDirection::Unspecified as i32,
        })
        .unwrap();
    }

    #[test]
    fn validate_list_room_streams_request_rejects_invalid_pagination() {
        let request = crate::client::ListRoomStreamsRequest {
            page: -1,
            page_size: 101,
            search: String::new(),
            sort_by: crate::client::RoomStreamListSortBy::Unspecified as i32,
            sort_direction: crate::client::SortDirection::Unspecified as i32,
        };

        let error = validation_error_text(&crate::validate(&request).unwrap_err());

        assert!(error.contains("page"), "{error}");
        assert!(error.contains("page_size"), "{error}");
    }

    #[test]
    fn validate_list_room_streams_request_accepts_defaultable_values() {
        crate::validate(&crate::client::ListRoomStreamsRequest {
            page: 0,
            page_size: 0,
            search: String::new(),
            sort_by: crate::client::RoomStreamListSortBy::Unspecified as i32,
            sort_direction: crate::client::SortDirection::Unspecified as i32,
        })
        .unwrap();
    }

    #[test]
    fn validate_move_playlist_request_requires_anchor() {
        let error = validation_error_text(
            &crate::validate(&crate::client::MovePlaylistRequest {
                playlist_id: "playlist-1".into(),
                anchor: None,
            })
            .unwrap_err(),
        );

        assert!(error.contains("anchor"), "{error}");
    }

    #[test]
    fn validate_create_playlist_request_rejects_long_name_and_incomplete_dynamic_fields() {
        let error = validation_error_text(
            &crate::validate(&crate::client::CreatePlaylistRequest {
                name: "a".repeat(256),
                parent_id: String::new(),
                source_provider: crate::source_config::SourceProvider::Alist as i32,
                source_config: None,
                provider_instance_name: String::new(),
                description: String::new(),
            })
            .unwrap_err(),
        );

        assert!(
            error.contains("name") || error.contains("dynamic"),
            "{error}"
        );
    }

    #[test]
    fn validate_create_playlist_request_allows_missing_provider_instance_for_default_provider() {
        crate::validate(&crate::client::CreatePlaylistRequest {
            name: "Dynamic".into(),
            parent_id: String::new(),
            source_provider: crate::source_config::SourceProvider::Alist as i32,
            source_config: Some(alist_playlist_source_config("/tv")),
            provider_instance_name: String::new(),
            description: String::new(),
        })
        .expect("dynamic playlist should allow default provider instance");
    }

    #[test]
    fn validate_update_playlist_request_rejects_long_name_when_present() {
        let error = validation_error_text(
            &crate::validate(&crate::client::UpdatePlaylistRequest {
                playlist_id: "playlist-1".into(),
                name: "a".repeat(256),
                description: String::new(),
            })
            .unwrap_err(),
        );

        assert!(error.contains("name"), "{error}");
    }

    #[test]
    fn validate_move_media_request_rejects_conflicting_scope_and_selection_modes() {
        let error = validation_error_text(
            &crate::validate(&crate::client::MoveMediaRequest {
                media_ids: vec!["media-1".into()],
                source_playlist_id: Some("playlist-1".into()),
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: None,
                after_media_id: None,
            })
            .unwrap_err(),
        );

        assert!(error.contains("move_media.source_scope"), "{error}");
    }

    #[test]
    fn validate_move_media_request_rejects_missing_media_ids_for_explicit_move() {
        let error = validation_error_text(
            &crate::validate(&crate::client::MoveMediaRequest {
                media_ids: Vec::new(),
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: None,
                after_media_id: None,
            })
            .unwrap_err(),
        );

        assert!(error.contains("move_media.explicit_selection"), "{error}");
    }

    #[test]
    fn validate_move_media_request_rejects_multiple_anchor_ids() {
        let error = validation_error_text(
            &crate::validate(&crate::client::MoveMediaRequest {
                media_ids: vec!["media-1".into()],
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: Some("media-before".into()),
                after_media_id: Some("media-after".into()),
            })
            .unwrap_err(),
        );

        assert!(error.contains("move_media.anchor"), "{error}");
    }

    #[test]
    fn validate_move_media_request_rejects_batch_size_above_limit() {
        let error = validation_error_text(
            &crate::validate(&crate::client::MoveMediaRequest {
                media_ids: (0..101).map(|idx| format!("media-{idx}")).collect(),
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: None,
                after_media_id: None,
            })
            .unwrap_err(),
        );

        assert!(error.contains("media_ids"), "{error}");
    }

    #[test]
    fn validate_add_media_request_allows_missing_provider_instance_for_default_provider() {
        crate::validate(&crate::client::AddMediaRequest {
            playlist_id: None,
            source_provider: crate::source_config::SourceProvider::Alist as i32,
            provider_instance_name: String::new(),
            source_config: Some(alist_media_source_config("/tv")),
            name: String::new(),
            description: String::new(),
        })
        .expect("provider-backed media add should allow default provider instance");
    }

    #[test]
    fn validate_add_media_request_rejects_oversized_title() {
        let error = validation_error_text(
            &crate::validate(&crate::client::AddMediaRequest {
                playlist_id: None,
                source_provider: crate::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                source_config: Some(direct_url_media_source_config(
                    "https://example.com/video.mp4",
                )),
                name: "a".repeat(501),
                description: String::new(),
            })
            .unwrap_err(),
        );

        assert!(error.contains("name"), "{error}");
    }

    #[test]
    fn validate_add_media_batch_request_rejects_too_many_items() {
        let template = crate::client::AddMediaRequest {
            playlist_id: None,
            source_provider: crate::source_config::SourceProvider::Unspecified as i32,
            provider_instance_name: String::new(),
            source_config: Some(direct_url_media_source_config(
                "https://example.com/video.mp4",
            )),
            name: String::new(),
            description: String::new(),
        };
        let error = validation_error_text(
            &crate::validate(&crate::client::AddMediaBatchRequest {
                items: vec![template; 101],
            })
            .unwrap_err(),
        );

        assert!(error.contains("items"), "{error}");
    }

    #[test]
    fn validate_start_playback_request_rejects_multiple_targets() {
        let error = validation_error_text(
            &crate::validate(&crate::client::StartPlaybackRequest {
                media_id: "media-1".into(),
                playlist_id: "playlist-1".into(),
                target: None,
            })
            .unwrap_err(),
        );

        assert!(
            error.contains("start_playback") || error.contains("media_id"),
            "{error}"
        );
    }

    #[test]
    fn validate_start_playback_request_rejects_static_media_with_target() {
        let error = validation_error_text(
            &crate::validate(&crate::client::StartPlaybackRequest {
                media_id: "media-1".into(),
                playlist_id: String::new(),
                target: Some(alist_target("/tv")),
            })
            .unwrap_err(),
        );

        assert!(error.contains("target"), "{error}");
    }

    #[test]
    fn validate_start_playback_request_rejects_dynamic_playlist_without_target() {
        let error = validation_error_text(
            &crate::validate(&crate::client::StartPlaybackRequest {
                media_id: String::new(),
                playlist_id: "playlist-1".into(),
                target: None,
            })
            .unwrap_err(),
        );

        assert!(error.contains("target"), "{error}");
    }

    #[test]
    fn validate_delete_entries_request_rejects_empty_target_set() {
        let error = validation_error_text(
            &crate::validate(&crate::client::DeleteEntriesRequest {
                playlist_ids: Vec::new(),
                media_ids: Vec::new(),
                force: false,
            })
            .unwrap_err(),
        );

        assert!(
            error.contains("delete_entries") || error.contains("playlist_ids"),
            "{error}"
        );
    }

    #[test]
    fn validate_delete_entries_request_rejects_batch_size_above_limit() {
        let error = validation_error_text(
            &crate::validate(&crate::client::DeleteEntriesRequest {
                playlist_ids: (0..51).map(|idx| format!("playlist-{idx}")).collect(),
                media_ids: (0..50).map(|idx| format!("media-{idx}")).collect(),
                force: true,
            })
            .unwrap_err(),
        );

        assert!(
            error.contains("delete_entries") || error.contains("playlist_ids"),
            "{error}"
        );
    }

    #[test]
    fn opaque_binary_fields_use_base64_http_json() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};

        let decoded: crate::client::StartOpaqueLoginRequest =
            serde_json::from_str(r#"{"username":"alice","credentialRequest":"AQID/w=="}"#)
                .expect("OPAQUE login request should deserialize from base64 bytes");
        assert_eq!(decoded.credential_request, vec![1, 2, 3, 255]);

        let decoded: crate::client::StartOpaquePasswordUpdateRequest = serde_json::from_str(
            r#"{
                "credentialRequest":"BAUG",
                "registrationRequest":"BwgJ",
                "verificationMethod":1,
                "emailToken":""
            }"#,
        )
        .expect("OPAQUE password update request should deserialize from base64 bytes");
        assert_eq!(decoded.credential_request, vec![4, 5, 6]);
        assert_eq!(decoded.registration_request, vec![7, 8, 9]);

        let decoded: crate::client::FinishOpaquePasswordUpdateRequest = serde_json::from_str(
            r#"{
                "sessionId":"opaque-session",
                "credentialFinalization":"Cgs=",
                "registrationUpload":"DA0=",
                "passkeySessionId":""
            }"#,
        )
        .expect("OPAQUE password update finish should deserialize from base64 bytes");
        assert_eq!(decoded.credential_finalization, vec![10, 11]);
        assert_eq!(decoded.registration_upload, vec![12, 13]);

        let decoded: crate::client::FinishSensitiveOperationVerificationRequest =
            serde_json::from_str(
                r#"{
                    "sessionId":"sensitive-session",
                    "method":1,
                    "password":"secret",
                    "emailToken":"",
                    "passkeySessionId":""
                }"#,
            )
            .expect("Sensitive password verification should allow omitted passkey credential");
        assert!(decoded.passkey_credential.is_none());

        let decoded: crate::client::FinishSensitiveOperationVerificationRequest =
            serde_json::from_str(
                r#"{
                    "sessionId":"sensitive-session",
                    "method":2,
                    "password":"",
                    "emailToken":"",
                    "passkeySessionId":"passkey-session",
                    "passkeyCredential":{
                        "id":"credential",
                        "rawId":"cmF3",
                        "response":{
                            "authenticatorData":"YXV0aA",
                            "clientDataJSON":"Y2xpZW50",
                            "signature":"c2ln"
                        },
                        "type":1
                    }
                }"#,
            )
            .expect("Sensitive passkey verification should deserialize structured credential");
        let credential = decoded
            .passkey_credential
            .expect("passkey credential should be present");
        assert_eq!(credential.id, "credential");
        assert_eq!(credential.raw_id, b"raw");

        let decoded: crate::client::SendChatMessageRequest = serde_json::from_str(
            r#"{
                "content":"hello",
                "clientMessageId":"client-message-1"
            }"#,
        )
        .expect("Plain text chat send should allow omitted optional payload fields");
        assert!(decoded.attachments.is_empty());
        assert!(decoded.metadata.is_none());
        assert!(decoded.reply_to_message_id.is_empty());
        assert!(decoded.display_position.is_empty());
        assert!(decoded.display_color.is_empty());

        let decoded: crate::client::SendChatMessageRequest = serde_json::from_str(
            r#"{
                "content":"",
                "clientMessageId":"client-message-2",
                "attachments":[{
                    "id":"attachment-1"
                }]
            }"#,
        )
        .expect("Chat attachment send should allow omitted attachment metadata");
        assert_eq!(decoded.attachments.len(), 1);
        assert_eq!(decoded.attachments[0].id, "attachment-1");
        assert_eq!(
            decoded.attachments[0].kind,
            crate::client::ChatAttachmentReferenceKind::Unspecified as i32
        );

        let decoded: crate::client::SendChatMessageRequest = serde_json::from_str(
            r#"{
                "content":"",
                "clientMessageId":"client-message-3",
                "attachments":[{
                    "id":"reuse-token",
                    "kind":2
                }]
            }"#,
        )
        .expect("Chat attachment send should allow reuse references");
        assert_eq!(decoded.attachments.len(), 1);
        assert_eq!(decoded.attachments[0].id, "reuse-token");
        assert_eq!(
            decoded.attachments[0].kind,
            crate::client::ChatAttachmentReferenceKind::Reuse as i32
        );

        let decoded: crate::client::EditChatMessageRequest = serde_json::from_str(
            r#"{
                "messageId":"msg-1",
                "content":"edited",
                "expectedVersion":"1",
                "clientOperationId":"edit-1"
            }"#,
        )
        .expect("Chat edit should allow omitted metadata");
        assert!(decoded.metadata.is_none());

        let decoded: crate::client::ListPinnedChatMessagesRequest =
            serde_json::from_str(r"{}").expect("Pinned chat list should allow omitted limit");
        assert_eq!(decoded.limit, 0);

        let decoded: crate::client::PinChatMessageRequest = serde_json::from_str(
            r#"{"note":"important"}"#,
        )
        .expect("Pin request should allow path-supplied message_id and omitted operation id");
        assert_eq!(decoded.message_id, "");
        assert_eq!(decoded.note, "important");
        assert_eq!(decoded.client_operation_id, "");

        let decoded: crate::client::UnpinChatMessageRequest = serde_json::from_str(r"{}")
            .expect("Unpin request should allow path-supplied message_id and omitted operation id");
        assert_eq!(decoded.message_id, "");
        assert_eq!(decoded.client_operation_id, "");

        let decoded: crate::client::GetChatPlaybackMessagesRequest =
            serde_json::from_str(r#"{"playbackMediaId":"med_probe"}"#)
                .expect("Chat playback query should allow omitted optional filters");
        assert_eq!(decoded.playback_media_id, "med_probe");
        assert!(decoded.playback_playlist_id.is_empty());
        assert!(decoded.playback_target.is_none());
        assert_eq!(decoded.position_seconds, 0.0);
        assert_eq!(decoded.before_seconds, 0.0);
        assert_eq!(decoded.after_seconds, 0.0);
        assert_eq!(decoded.limit, 0);
        assert!(!decoded.include_deleted);

        let decoded: crate::client::MarkAllAsReadRequest = serde_json::from_str(r"{}")
            .expect("Notification mark-all request should allow omitted cutoff");
        assert_eq!(decoded.before, None);

        let decoded: crate::admin::AddMemberRequest = serde_json::from_str(
            r#"{
                "roomId":"room_probe",
                "userId":"usr_probe"
            }"#,
        )
        .expect("Admin add-member should allow omitted role and notify");
        assert_eq!(decoded.role, 0);
        assert!(!decoded.notify);

        let decoded: crate::admin::UpdateMemberPermissionsRequest = serde_json::from_str(
            r#"{
                "roomId":"room_probe",
                "userId":"usr_probe"
            }"#,
        )
        .expect("Admin member permission update should allow omitted permission overrides");
        assert_eq!(decoded.role, 0);
        assert_eq!(decoded.added_permissions, 0);
        assert_eq!(decoded.removed_permissions, 0);
        assert_eq!(decoded.admin_added_permissions, 0);
        assert_eq!(decoded.admin_removed_permissions, 0);

        let decoded: crate::providers::common::ListAvailableProviderInstancesRequest =
            serde_json::from_str(r"{}")
                .expect("Provider available instances query should allow omitted provider type");
        assert_eq!(
            decoded.provider_type,
            crate::source_config::SourceProvider::Unspecified as i32
        );

        let decoded: crate::providers::common::ListProviderInstancesRequest =
            serde_json::from_str(r"{}")
                .expect("Provider instances query should allow omitted filters");
        assert_eq!(decoded.page, 0);
        assert_eq!(decoded.page_size, 0);
        assert_eq!(
            decoded.provider_type,
            crate::source_config::SourceProvider::Unspecified as i32
        );
        assert!(decoded.search.is_empty());
        assert_eq!(decoded.enabled, None);
        assert_eq!(decoded.tls, None);

        let decoded: crate::client::StartOpaquePasswordResetRequest = serde_json::from_str(
            r#"{
                "email":"alice@example.com",
                "token":"reset-token",
                "registrationRequest":"Dg8="
            }"#,
        )
        .expect("OPAQUE password reset start should deserialize from base64 bytes");
        assert_eq!(decoded.registration_request, vec![14, 15]);

        let decoded: crate::client::FinishOpaquePasswordResetRequest = serde_json::from_str(
            r#"{
                "sessionId":"opaque-reset-session",
                "registrationUpload":"EBE="
            }"#,
        )
        .expect("OPAQUE password reset finish should deserialize from base64 bytes");
        assert_eq!(decoded.registration_upload, vec![16, 17]);

        let response = crate::client::StartOpaqueLoginResponse {
            session_id: "opaque-session".to_string(),
            credential_response: vec![1, 2, 3],
        };
        let json = serde_json::to_value(response).expect("serialize OPAQUE response");
        assert_eq!(json["credentialResponse"], STANDARD.encode([1, 2, 3]));
    }

    #[test]
    fn opaque_binary_fields_reject_http_byte_arrays() {
        let error = serde_json::from_str::<crate::client::StartOpaqueLoginRequest>(
            r#"{"username":"alice","credentialRequest":[1,2,3,255]}"#,
        )
        .expect_err("OPAQUE binary fields should reject JSON byte arrays");

        assert!(
            error.to_string().contains("invalid type")
                || error.to_string().contains("expected a string"),
            "unexpected error: {error}"
        );
    }
}
