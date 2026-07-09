use prost::Message;
use synctv_core::models::{
    ChatEventKind, ChatMessageEvent, ChatMessagePin, ChatMessageWithAttachments, ChatMetadata,
    ChatPinEvent, ChatPinEventKind, ChatPlaybackMetadata as CoreChatPlaybackMetadata,
    ChatPresentationMetadata, ProviderTarget, RoomPlaybackState,
};

use synctv_proto::client::{ClientMessage, ServerMessage};

pub(super) fn validated_room_member_role(role: i32) -> Result<i32, String> {
    let role = synctv_proto::common::RoomMemberRole::try_from(role)
        .map_err(|_| "Room member role is not defined".to_string())?;
    if role == synctv_proto::common::RoomMemberRole::Unspecified {
        return Err("Room member role is unspecified".to_string());
    }
    Ok(role as i32)
}

pub(super) fn required_realtime_text(
    value: &str,
    field_name: &str,
    max_len: usize,
) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > max_len || trimmed.chars().any(char::is_control) {
        return Err(format!(
            "Realtime {field_name} must be 1-{max_len} non-control characters"
        ));
    }
    Ok(trimmed.to_string())
}

fn optional_realtime_text(value: &str, field_name: &str, max_len: usize) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.len() > max_len || trimmed.chars().any(char::is_control) {
        return Err(format!(
            "Realtime {field_name} must be at most {max_len} non-control characters"
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn room_member_event_to_proto(
    event: &synctv_realtime::sync::RealtimeEvent,
    public_id_codec: &synctv_adapter::PublicIdCodec,
    sequence: i64,
) -> Result<Option<synctv_proto::client::RoomMemberEvent>, String> {
    use synctv_proto::client::RoomMemberEventKind;
    use synctv_realtime::sync::RealtimeEvent;

    let encode_room = |id| {
        public_id_codec
            .encode_room_id(id)
            .map_err(|error| format!("Failed to encode room member event room id: {error}"))
    };
    let encode_user = |id| {
        public_id_codec
            .encode_user_id(id)
            .map_err(|error| format!("Failed to encode room member event user id: {error}"))
    };

    let proto = match event {
        RealtimeEvent::UserJoined {
            event_id,
            room_id,
            user_id,
            username,
            remark_name,
            display_tag,
            permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            joined_at,
            timestamp,
            ..
        } => {
            let room_id = encode_room(*room_id)?;
            let user_id = encode_user(*user_id)?;
            let validated_username = required_realtime_text(username, "user username", 50)?;
            let validated_remark_name = optional_realtime_text(remark_name, "member remark", 64)?;
            let validated_display_tag =
                optional_realtime_text(display_tag, "member display tag", 16)?;
            let member = synctv_proto::common::RoomMember {
                room_id: room_id.clone(),
                user_id: user_id.clone(),
                username: validated_username,
                remark_name: validated_remark_name,
                display_tag: validated_display_tag,
                role: validated_room_member_role(*role)?,
                permissions: permissions.0,
                added_permissions: added_permissions.0,
                removed_permissions: removed_permissions.0,
                admin_added_permissions: admin_added_permissions.0,
                admin_removed_permissions: admin_removed_permissions.0,
                joined_at: joined_at.timestamp(),
                is_online: true,
                connection_count: 1,
            };
            synctv_proto::client::RoomMemberEvent {
                event_id: event_id.clone(),
                room_id,
                kind: RoomMemberEventKind::Joined as i32,
                username: member.username.clone(),
                remark_name: member.remark_name.clone(),
                display_tag: member.display_tag.clone(),
                user_id,
                guest_id: String::new(),
                member: Some(member),
                actor_user_id: String::new(),
                reason: String::new(),
                occurred_at: timestamp.timestamp(),
                sequence,
            }
        }
        RealtimeEvent::GuestJoined {
            event_id,
            room_id,
            guest_id,
            username,
            permissions,
            role,
            joined_at,
            timestamp,
            ..
        } => {
            let room_id = encode_room(*room_id)?;
            let guest_id = required_realtime_text(guest_id, "guest id", 128)?;
            let validated_username = required_realtime_text(username, "guest username", 64)?;
            let member = synctv_proto::common::RoomMember {
                room_id: room_id.clone(),
                user_id: guest_id.clone(),
                username: validated_username,
                remark_name: String::new(),
                display_tag: String::new(),
                role: validated_room_member_role(*role)?,
                permissions: permissions.0,
                added_permissions: 0,
                removed_permissions: 0,
                admin_added_permissions: 0,
                admin_removed_permissions: 0,
                joined_at: joined_at.timestamp(),
                is_online: true,
                connection_count: 1,
            };
            synctv_proto::client::RoomMemberEvent {
                event_id: event_id.clone(),
                room_id,
                kind: RoomMemberEventKind::Joined as i32,
                username: member.username.clone(),
                remark_name: member.remark_name.clone(),
                display_tag: String::new(),
                user_id: String::new(),
                guest_id,
                member: Some(member),
                actor_user_id: String::new(),
                reason: String::new(),
                occurred_at: timestamp.timestamp(),
                sequence,
            }
        }
        RealtimeEvent::UserLeft {
            event_id,
            room_id,
            user_id,
            username,
            remark_name,
            display_tag,
            timestamp,
            ..
        } => synctv_proto::client::RoomMemberEvent {
            event_id: event_id.clone(),
            room_id: encode_room(*room_id)?,
            kind: RoomMemberEventKind::Left as i32,
            member: None,
            user_id: encode_user(*user_id)?,
            guest_id: String::new(),
            username: required_realtime_text(username, "user username", 50)?,
            remark_name: optional_realtime_text(remark_name, "member remark", 64)?,
            display_tag: optional_realtime_text(display_tag, "member display tag", 16)?,
            actor_user_id: String::new(),
            reason: String::new(),
            occurred_at: timestamp.timestamp(),
            sequence,
        },
        RealtimeEvent::GuestLeft {
            event_id,
            room_id,
            guest_id,
            username,
            timestamp,
        } => synctv_proto::client::RoomMemberEvent {
            event_id: event_id.clone(),
            room_id: encode_room(*room_id)?,
            kind: RoomMemberEventKind::Left as i32,
            member: None,
            user_id: String::new(),
            guest_id: required_realtime_text(guest_id, "guest id", 128)?,
            username: required_realtime_text(username, "guest username", 64)?,
            remark_name: String::new(),
            display_tag: String::new(),
            actor_user_id: String::new(),
            reason: String::new(),
            occurred_at: timestamp.timestamp(),
            sequence,
        },
        RealtimeEvent::PermissionChanged {
            event_id,
            room_id,
            target_user_id,
            target_username,
            target_remark_name,
            target_display_tag,
            changed_by,
            new_permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            target_is_online,
            target_connection_count,
            timestamp,
            ..
        } => {
            let room_id = encode_room(*room_id)?;
            let user_id = encode_user(*target_user_id)?;
            let validated_username =
                required_realtime_text(target_username, "target username", 50)?;
            let validated_remark_name =
                optional_realtime_text(target_remark_name, "member remark", 64)?;
            let validated_display_tag =
                optional_realtime_text(target_display_tag, "member display tag", 16)?;
            let member = synctv_proto::common::RoomMember {
                room_id: room_id.clone(),
                user_id: user_id.clone(),
                username: validated_username,
                remark_name: validated_remark_name,
                display_tag: validated_display_tag,
                role: validated_room_member_role(*role)?,
                permissions: new_permissions.0,
                added_permissions: added_permissions.0,
                removed_permissions: removed_permissions.0,
                admin_added_permissions: admin_added_permissions.0,
                admin_removed_permissions: admin_removed_permissions.0,
                joined_at: 0,
                is_online: *target_is_online,
                connection_count: i32::try_from(*target_connection_count).unwrap_or(i32::MAX),
            };
            synctv_proto::client::RoomMemberEvent {
                event_id: event_id.clone(),
                room_id,
                kind: RoomMemberEventKind::PermissionChanged as i32,
                username: member.username.clone(),
                remark_name: member.remark_name.clone(),
                display_tag: member.display_tag.clone(),
                user_id,
                guest_id: String::new(),
                member: Some(member),
                actor_user_id: encode_user(*changed_by)?,
                reason: String::new(),
                occurred_at: timestamp.timestamp(),
                sequence,
            }
        }
        RealtimeEvent::KickUserFromRoom {
            event_id,
            room_id,
            user_id,
            reason,
            timestamp,
        } => synctv_proto::client::RoomMemberEvent {
            event_id: event_id.clone(),
            room_id: encode_room(*room_id)?,
            kind: RoomMemberEventKind::Kicked as i32,
            member: None,
            user_id: encode_user(*user_id)?,
            guest_id: String::new(),
            username: String::new(),
            remark_name: String::new(),
            display_tag: String::new(),
            actor_user_id: String::new(),
            reason: required_realtime_text(reason, "kick reason", 500)?,
            occurred_at: timestamp.timestamp(),
            sequence,
        },
        _ => return Ok(None),
    };

    Ok(Some(proto))
}

pub(crate) fn online_event_to_proto(
    event: &synctv_realtime::sync::RealtimeEvent,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<synctv_proto::client::OnlineEvent>, String> {
    use synctv_proto::client::OnlineEventKind;
    use synctv_realtime::sync::RealtimeEvent;

    let encode_room = |id| {
        public_id_codec
            .encode_room_id(id)
            .map_err(|error| format!("Failed to encode online event room id: {error}"))
    };
    let encode_user = |id| {
        public_id_codec
            .encode_user_id(id)
            .map_err(|error| format!("Failed to encode online event user id: {error}"))
    };

    let event = match event {
        RealtimeEvent::UserJoined {
            event_id,
            room_id,
            user_id,
            username,
            role,
            timestamp,
            ..
        } => synctv_proto::client::OnlineEvent {
            event_id: event_id.clone(),
            room_id: encode_room(*room_id)?,
            user_id: encode_user(*user_id)?,
            username: required_realtime_text(username, "online event username", 50)?,
            role: validated_room_member_role(*role)?,
            kind: OnlineEventKind::Joined as i32,
            occurred_at: timestamp.timestamp(),
        },
        RealtimeEvent::UserLeft {
            event_id,
            room_id,
            user_id,
            username,
            role,
            timestamp,
            ..
        } => synctv_proto::client::OnlineEvent {
            event_id: event_id.clone(),
            room_id: encode_room(*room_id)?,
            user_id: encode_user(*user_id)?,
            username: required_realtime_text(username, "online event username", 50)?,
            role: validated_room_member_role(*role)?,
            kind: OnlineEventKind::Left as i32,
            occurred_at: timestamp.timestamp(),
        },
        _ => return Ok(None),
    };

    Ok(Some(event))
}

pub(crate) fn chat_event_kind_to_proto(
    kind: ChatEventKind,
) -> synctv_proto::client::ChatMessageEventKind {
    match kind {
        ChatEventKind::Created => synctv_proto::client::ChatMessageEventKind::Created,
        ChatEventKind::Edited => synctv_proto::client::ChatMessageEventKind::Edited,
        ChatEventKind::Deleted => synctv_proto::client::ChatMessageEventKind::Deleted,
        ChatEventKind::ReactionsChanged => {
            synctv_proto::client::ChatMessageEventKind::ReactionsChanged
        }
    }
}

pub(crate) fn chat_pin_event_kind_to_proto(
    kind: ChatPinEventKind,
) -> synctv_proto::client::ChatPinEventKind {
    match kind {
        ChatPinEventKind::Pinned => synctv_proto::client::ChatPinEventKind::Pinned,
        ChatPinEventKind::Unpinned => synctv_proto::client::ChatPinEventKind::Unpinned,
        ChatPinEventKind::MessageUpdated => synctv_proto::client::ChatPinEventKind::MessageUpdated,
        ChatPinEventKind::MessageDeleted => synctv_proto::client::ChatPinEventKind::MessageDeleted,
    }
}

#[cfg(test)]
pub(crate) fn chat_display_position_from_metadata(
    metadata: Option<&ChatMetadata>,
) -> Result<String, String> {
    synctv_adapter::chat::chat_display_position_from_metadata(metadata)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn chat_display_color_from_metadata(
    metadata: Option<&ChatMetadata>,
) -> Result<String, String> {
    synctv_adapter::chat::chat_display_color_from_metadata(metadata)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn chat_playback_media_id_from_metadata(
    metadata: Option<&ChatMetadata>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<String, String> {
    synctv_adapter::chat::chat_playback_media_id_from_metadata(metadata, public_id_codec)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn chat_playback_playlist_id_from_metadata(
    metadata: Option<&ChatMetadata>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<String, String> {
    synctv_adapter::chat::chat_playback_playlist_id_from_metadata(metadata, public_id_codec)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn chat_playback_metadata_from_metadata(
    metadata: Option<&ChatMetadata>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_adapter::chat::ChatPlaybackMetadata, String> {
    synctv_adapter::chat::chat_playback_metadata_from_metadata(metadata, public_id_codec)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn chat_playback_target_from_metadata(
    metadata: Option<&ChatMetadata>,
) -> Result<Option<ProviderTarget>, String> {
    Ok(synctv_adapter::chat::chat_playback_target_from_metadata(
        metadata,
    ))
}

fn validate_chat_metadata_text(
    value: &str,
    field_name: &str,
    max_len: usize,
) -> Result<Option<String>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > max_len || trimmed.chars().any(char::is_control) {
        return Err(format!(
            "Chat {field_name} must be 1-{max_len} non-control characters"
        ));
    }
    Ok(Some(trimmed.to_string()))
}

pub(crate) fn chat_playback_target_hash(target: &ProviderTarget) -> Result<String, String> {
    synctv_core::models::try_hash_playback_target(Some(target)).map_err(|error| error.to_string())
}

pub(crate) fn chat_metadata_for_send(
    metadata: Option<ChatMetadata>,
    display_position: &str,
    display_color: &str,
    playback_state: Option<&RoomPlaybackState>,
) -> Result<Option<ChatMetadata>, String> {
    let mut metadata = match metadata {
        Some(ChatMetadata::User(metadata)) => metadata,
        Some(ChatMetadata::MemberJoined(_)) => {
            return Err("Client chat metadata must use user metadata".to_string());
        }
        None => synctv_core::models::ChatUserMetadata::default(),
    };
    let display_position = validate_chat_metadata_text(display_position, "display position", 64)?;
    let display_color = validate_chat_metadata_text(display_color, "display color", 64)?;
    let presentation = ChatPresentationMetadata {
        display_position,
        display_color,
    };
    metadata.presentation = (!presentation.is_empty()).then_some(presentation);

    if let Some(state) = playback_state
        .filter(|state| state.playing_media_id.is_some() || state.playing_playlist_id.is_some())
    {
        let position_seconds = state.computed_position().max(0.0);
        metadata.playback = Some(CoreChatPlaybackMetadata {
            media_id: state.playing_media_id,
            playlist_id: state.playing_playlist_id,
            target: state.target.clone(),
            target_hash: state
                .target
                .as_ref()
                .map(chat_playback_target_hash)
                .transpose()?,
            position_seconds: position_seconds.is_finite().then_some(position_seconds),
        });
    } else {
        metadata.playback = None;
    }

    let metadata = ChatMetadata::User(metadata);
    Ok((!metadata.is_empty()).then_some(metadata))
}

pub(crate) fn chat_message_event_to_proto(
    event: &ChatMessageEvent,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_proto::client::ChatMessageEvent, String> {
    let room_id = public_id_codec
        .encode_room_id(event.room_id)
        .map_err(|error| format!("Failed to encode chat event room id: {error}"))?;
    Ok(synctv_proto::client::ChatMessageEvent {
        event_id: event.event_id.clone(),
        room_id,
        kind: chat_event_kind_to_proto(event.kind) as i32,
        message: Some(chat_message_receive_to_proto(
            &event.message,
            public_id_codec,
            String::new(),
        )?),
        occurred_at: event.occurred_at.timestamp(),
        sequence: event.sequence,
    })
}

pub(crate) fn chat_pin_event_to_proto(
    event: &ChatPinEvent,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_proto::client::ChatPinEvent, String> {
    let room_id = public_id_codec
        .encode_room_id(event.room_id)
        .map_err(|error| format!("Failed to encode chat pin event room id: {error}"))?;
    Ok(synctv_proto::client::ChatPinEvent {
        event_id: event.event_id.clone(),
        room_id,
        kind: chat_pin_event_kind_to_proto(event.kind) as i32,
        message: Some(chat_message_receive_to_proto(
            &event.message,
            public_id_codec,
            String::new(),
        )?),
        pin: event
            .pin
            .as_ref()
            .map(|pin| chat_message_pin_to_proto(pin, public_id_codec))
            .transpose()?,
        occurred_at: event.occurred_at.timestamp(),
        sequence: event.sequence,
    })
}

pub(crate) fn chat_message_pin_to_proto(
    pin: &ChatMessagePin,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_proto::client::ChatMessagePin, String> {
    synctv_adapter::chat::chat_message_pin_to_proto(pin, public_id_codec)
        .map_err(|error| error.to_string())
}

pub(crate) fn chat_message_receive_to_proto(
    value: &ChatMessageWithAttachments,
    public_id_codec: &synctv_adapter::PublicIdCodec,
    username: String,
) -> Result<synctv_proto::client::ChatMessageReceive, String> {
    synctv_adapter::chat::chat_message_receive_to_proto(value, public_id_codec, username)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn core_chat_attachment_to_proto(
    attachment: &synctv_core::models::ChatAttachment,
) -> Result<synctv_proto::client::ChatAttachment, String> {
    synctv_adapter::chat::core_chat_attachment_to_proto(attachment)
        .map_err(|error| error.to_string())
}

/// Binary codec for proto messages
pub struct ProtoCodec;

impl ProtoCodec {
    /// Encode `ClientMessage` to binary
    pub fn encode_client_message(msg: &ClientMessage) -> Result<Vec<u8>, String> {
        Ok(msg.encode_to_vec())
    }

    /// Decode `ClientMessage` from binary
    pub fn decode_client_message(data: &[u8]) -> Result<ClientMessage, String> {
        ClientMessage::decode(data).map_err(|e| format!("Failed to decode message: {e}"))
    }

    /// Encode `ServerMessage` to binary
    pub fn encode_server_message(msg: &ServerMessage) -> Result<Vec<u8>, String> {
        Ok(msg.encode_to_vec())
    }

    /// Decode `ServerMessage` from binary
    pub fn decode_server_message(data: &[u8]) -> Result<ServerMessage, String> {
        ServerMessage::decode(data).map_err(|e| format!("Failed to decode message: {e}"))
    }
}
