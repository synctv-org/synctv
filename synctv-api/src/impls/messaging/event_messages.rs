use synctv_proto::client::ServerMessage;
use synctv_realtime::sync::WebRTCSignalKind;

use super::codec::{
    encode_non_empty_media_ids, optional_chat_metadata_text, playback_state_to_proto,
    required_realtime_text, validated_non_negative_version, validated_room_member_role,
    validated_room_settings_json,
};
use super::notifications::system_notification_server_message;

/// Convert a realtime event into one or more server messages.
pub(super) fn realtime_event_to_server_messages(
    event: &synctv_realtime::sync::RealtimeEvent,
    room_id: &str,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<Vec<ServerMessage>, String> {
    use synctv_proto::client::server_message::Message;
    use synctv_proto::client::{
        ChatMessageReceive, ErrorMessage, MediaRemovedBatch, MediaUpdated, PlaybackStateChanged,
        PlaylistCreated, PlaylistDeleted, PlaylistReordered, PlaylistUpdated, RoomSettingsChanged,
        ServerMessage, UserJoinedRoom, UserLeftRoom,
    };
    use synctv_proto::common::RoomMember;
    use synctv_realtime::sync::RealtimeEvent;

    let encode_user = |id| {
        public_id_codec
            .encode_user_id(id)
            .map_err(|error| format!("Failed to encode realtime event user id: {error}"))
    };
    let encode_room = |id| {
        public_id_codec
            .encode_room_id(id)
            .map_err(|error| format!("Failed to encode realtime event room id: {error}"))
    };
    let encode_media = |id| {
        public_id_codec
            .encode_media_id(id)
            .map_err(|error| format!("Failed to encode realtime event media id: {error}"))
    };
    let encode_playlist = |id| {
        public_id_codec
            .encode_playlist_id(id)
            .map_err(|error| format!("Failed to encode realtime event playlist id: {error}"))
    };

    let messages = match event {
        RealtimeEvent::ChatMessage {
            user_id,
            username,
            message,
            timestamp,
            display_position,
            display_color,
            ..
        } => vec![ServerMessage {
            message: Some(Message::Chat(ChatMessageReceive {
                id: event.event_id().to_string(),
                room_id: room_id.to_string(),
                user_id: encode_user(*user_id)?,
                username: required_realtime_text(username, "user username", 50)?,
                content: message.clone(),
                timestamp: timestamp.timestamp(),
                display_position: optional_chat_metadata_text(
                    display_position.as_deref(),
                    "display position",
                    64,
                )?
                .unwrap_or_default(),
                display_color: optional_chat_metadata_text(
                    display_color.as_deref(),
                    "display color",
                    64,
                )?
                .unwrap_or_default(),
                client_message_id: String::new(),
                status: synctv_proto::client::ChatMessageStatus::Active as i32,
                version: 1,
                edited_at: 0,
                deleted_at: 0,
                reply_to_message_id: String::new(),
                images: Vec::new(),
                deleted_by_user_id: String::new(),
                delete_reason: String::new(),
                playback_media_id: String::new(),
                playback_playlist_id: String::new(),
                playback_target: Vec::new(),
                playback_target_hash: String::new(),
                playback_position_seconds: None,
                reactions: Vec::new(),
                reaction_count: 0,
            })),
        }],
        RealtimeEvent::ChatMessageEvent { .. } => Vec::new(),
        RealtimeEvent::PlaybackStateChanged { state, .. } => vec![ServerMessage {
            message: Some(Message::PlaybackState(PlaybackStateChanged {
                room_id: room_id.to_string(),
                state: Some(playback_state_to_proto(
                    state,
                    &encode_room,
                    &encode_media,
                    &encode_playlist,
                )?),
            })),
        }],
        RealtimeEvent::UserJoined {
            user_id,
            username,
            permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            joined_at,
            ..
        } => vec![ServerMessage {
            message: Some(Message::UserJoined(UserJoinedRoom {
                room_id: room_id.to_string(),
                member: Some(RoomMember {
                    room_id: room_id.to_string(),
                    user_id: encode_user(*user_id)?,
                    username: required_realtime_text(username, "user username", 50)?,
                    role: validated_room_member_role(*role)?,
                    permissions: permissions.0,
                    added_permissions: added_permissions.0,
                    removed_permissions: removed_permissions.0,
                    admin_added_permissions: admin_added_permissions.0,
                    admin_removed_permissions: admin_removed_permissions.0,
                    joined_at: joined_at.timestamp(),
                    is_online: true,
                }),
            })),
        }],
        RealtimeEvent::GuestJoined {
            guest_id,
            username,
            permissions,
            role,
            joined_at,
            ..
        } => vec![ServerMessage {
            message: Some(Message::UserJoined(UserJoinedRoom {
                room_id: room_id.to_string(),
                member: Some(RoomMember {
                    room_id: room_id.to_string(),
                    user_id: required_realtime_text(guest_id, "guest id", 128)?,
                    username: required_realtime_text(username, "guest username", 64)?,
                    role: validated_room_member_role(*role)?,
                    permissions: permissions.0,
                    added_permissions: 0,
                    removed_permissions: 0,
                    admin_added_permissions: 0,
                    admin_removed_permissions: 0,
                    joined_at: joined_at.timestamp(),
                    is_online: true,
                }),
            })),
        }],
        RealtimeEvent::UserLeft { user_id, .. } => vec![ServerMessage {
            message: Some(Message::UserLeft(UserLeftRoom {
                room_id: room_id.to_string(),
                user_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::GuestLeft { guest_id, .. } => vec![ServerMessage {
            message: Some(Message::UserLeft(UserLeftRoom {
                room_id: room_id.to_string(),
                user_id: required_realtime_text(guest_id, "guest id", 128)?,
            })),
        }],
        RealtimeEvent::MediaAdded {
            media_id,
            media_title,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaAdded(synctv_proto::client::MediaAdded {
                room_id: room_id.to_string(),
                media_id: encode_media(*media_id)?,
                name: required_realtime_text(media_title, "media title", 512)?,
                creator_username: required_realtime_text(username, "creator username", 50)?,
                creator_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::MediaRemoved {
            media_id,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaRemoved(synctv_proto::client::MediaRemoved {
                room_id: room_id.to_string(),
                media_id: encode_media(*media_id)?,
                removed_by: required_realtime_text(username, "removed-by username", 50)?,
                removed_by_user_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::MediaRemovedBatch {
            media_ids,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaRemovedBatch(MediaRemovedBatch {
                room_id: room_id.to_string(),
                media_ids: encode_non_empty_media_ids(
                    media_ids,
                    &encode_media,
                    "media removed batch",
                )?,
                removed_by: required_realtime_text(username, "removed-by username", 50)?,
                removed_by_user_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::MediaUpdated {
            media_id,
            media_title,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::MediaUpdated(MediaUpdated {
                room_id: room_id.to_string(),
                media_id: encode_media(*media_id)?,
                name: required_realtime_text(media_title, "media title", 512)?,
                updated_by: required_realtime_text(username, "updated-by username", 50)?,
                updated_by_user_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::PlaylistReordered {
            media_ids,
            user_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::PlaylistReordered(PlaylistReordered {
                room_id: room_id.to_string(),
                media_ids: encode_non_empty_media_ids(
                    media_ids,
                    &encode_media,
                    "playlist reorder",
                )?,
                reordered_by: required_realtime_text(username, "reordered-by username", 50)?,
                reordered_by_user_id: encode_user(*user_id)?,
            })),
        }],
        RealtimeEvent::PlaylistCreated { playlist, .. } => vec![ServerMessage {
            message: Some(Message::PlaylistCreated(PlaylistCreated {
                room_id: room_id.to_string(),
                playlist: Some(crate::impls::client::convert::try_playlist_to_proto(
                    playlist,
                    0,
                    public_id_codec,
                )?),
            })),
        }],
        RealtimeEvent::PlaylistUpdated { playlist, .. } => vec![ServerMessage {
            message: Some(Message::PlaylistUpdated(PlaylistUpdated {
                room_id: room_id.to_string(),
                playlist: Some(crate::impls::client::convert::try_playlist_to_proto(
                    playlist,
                    0,
                    public_id_codec,
                )?),
            })),
        }],
        RealtimeEvent::PlaylistDeleted { playlist_id, .. } => vec![ServerMessage {
            message: Some(Message::PlaylistDeleted(PlaylistDeleted {
                room_id: room_id.to_string(),
                playlist_id: encode_playlist(*playlist_id)?,
            })),
        }],
        RealtimeEvent::PermissionChanged {
            target_user_id,
            new_permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            changed_by_username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::PermissionChanged(
                synctv_proto::client::PermissionChanged {
                    room_id: room_id.to_string(),
                    user_id: encode_user(*target_user_id)?,
                    role: validated_room_member_role(*role)?,
                    effective_permissions: new_permissions.0,
                    added_permissions: added_permissions.0,
                    removed_permissions: removed_permissions.0,
                    admin_added_permissions: admin_added_permissions.0,
                    admin_removed_permissions: admin_removed_permissions.0,
                    updated_by: required_realtime_text(
                        changed_by_username,
                        "permission updated-by username",
                        50,
                    )?,
                },
            )),
        }],
        RealtimeEvent::RoomSettingsChanged {
            settings_json,
            version,
            ..
        } => vec![ServerMessage {
            message: Some(Message::RoomSettings(RoomSettingsChanged {
                room_id: room_id.to_string(),
                settings: validated_room_settings_json(settings_json)?,
                version: validated_non_negative_version(*version, "room settings")?,
            })),
        }],
        RealtimeEvent::WebRTCSignaling {
            message_type,
            from,
            to,
            data,
            ..
        } => {
            let msg = match message_type {
                WebRTCSignalKind::Offer => ServerMessage {
                    message: Some(Message::WebrtcOffer(synctv_proto::client::WebRtcOffer {
                        from: required_realtime_text(from, "webrtc from", 256)?,
                        to: required_realtime_text(to, "webrtc to", 256)?,
                        data: data.clone(),
                    })),
                },
                WebRTCSignalKind::Answer => ServerMessage {
                    message: Some(Message::WebrtcAnswer(synctv_proto::client::WebRtcAnswer {
                        from: required_realtime_text(from, "webrtc from", 256)?,
                        to: required_realtime_text(to, "webrtc to", 256)?,
                        data: data.clone(),
                    })),
                },
                WebRTCSignalKind::IceCandidate => ServerMessage {
                    message: Some(Message::WebrtcIceCandidate(
                        synctv_proto::client::WebRtcIceCandidate {
                            from: required_realtime_text(from, "webrtc from", 256)?,
                            to: required_realtime_text(to, "webrtc to", 256)?,
                            data: data.clone(),
                        },
                    )),
                },
            };
            vec![msg]
        }
        RealtimeEvent::WebRTCJoin {
            actor_id,
            conn_id,
            username,
            ..
        } => vec![ServerMessage {
            message: Some(Message::WebrtcJoin(synctv_proto::client::WebRtcJoin {
                user_id: required_realtime_text(actor_id, "webrtc actor id", 128)?,
                conn_id: required_realtime_text(conn_id, "webrtc connection id", 128)?,
                username: required_realtime_text(username, "webrtc username", 64)?,
            })),
        }],
        RealtimeEvent::WebRTCLeave {
            actor_id, conn_id, ..
        } => vec![ServerMessage {
            message: Some(Message::WebrtcLeave(synctv_proto::client::WebRtcLeave {
                user_id: required_realtime_text(actor_id, "webrtc actor id", 128)?,
                conn_id: required_realtime_text(conn_id, "webrtc connection id", 128)?,
            })),
        }],
        RealtimeEvent::SystemNotification {
            message, timestamp, ..
        } => vec![system_notification_server_message(
            message.clone(),
            *timestamp,
        )?],
        RealtimeEvent::RoomDeleted { .. } => {
            // Notify WebSocket clients that the room has been deleted
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room has been deleted".to_string(),
                    code: crate::impls::error_codes::NOT_FOUND,
                    detail: String::new(),
                })),
            }]
        }
        RealtimeEvent::RoomBanned { .. } => {
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room has been banned".to_string(),
                    code: crate::impls::error_codes::FORBIDDEN,
                    detail: String::new(),
                })),
            }]
        }
        RealtimeEvent::RoomOwnerInactive { .. } => {
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room is unavailable because its creator is not active".to_string(),
                    code: crate::impls::error_codes::FORBIDDEN,
                    detail: String::new(),
                })),
            }]
        }
        RealtimeEvent::KickPublisher { .. }
        | RealtimeEvent::KickUser { .. }
        | RealtimeEvent::KickUserFromRoom { .. }
        | RealtimeEvent::RoomCreated { .. }
        | RealtimeEvent::CacheInvalidate { .. }
        | RealtimeEvent::ProviderCredentialChanged { .. }
        | RealtimeEvent::UserNotification { .. } => {
            // Admin/internal events are handled by other channels,
            // not forwarded to WebSocket clients via the room event path
            vec![]
        }
    };
    Ok(messages)
}
