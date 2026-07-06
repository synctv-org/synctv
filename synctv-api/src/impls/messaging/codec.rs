use prost::Message;
use synctv_core::models::{
    ChatEventKind, ChatMessageEvent, ChatMessagePin, ChatMessageStatus, ChatMessageType,
    ChatMessageWithAttachments, ChatMetadata, ChatPinEvent, ChatPinEventKind,
    ChatPlaybackMetadata as CoreChatPlaybackMetadata, ChatPresentationMetadata, ProviderTarget,
    RoomPlaybackState,
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
    public_id_codec: &crate::public_id::PublicIdCodec,
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
    public_id_codec: &crate::public_id::PublicIdCodec,
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

pub(crate) fn chat_message_type_to_proto(
    message_type: ChatMessageType,
) -> synctv_proto::client::ChatMessageType {
    match message_type {
        ChatMessageType::User => synctv_proto::client::ChatMessageType::User,
        ChatMessageType::SystemMemberJoined => {
            synctv_proto::client::ChatMessageType::SystemMemberJoined
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

pub(crate) fn chat_status_to_proto(
    status: ChatMessageStatus,
) -> synctv_proto::client::ChatMessageStatus {
    match status {
        ChatMessageStatus::Active => synctv_proto::client::ChatMessageStatus::Active,
        ChatMessageStatus::Edited => synctv_proto::client::ChatMessageStatus::Edited,
        ChatMessageStatus::Deleted => synctv_proto::client::ChatMessageStatus::Deleted,
    }
}

pub(crate) fn chat_display_position_from_metadata(
    metadata: &Option<ChatMetadata>,
) -> Result<String, String> {
    chat_presentation_text_from_metadata(
        metadata
            .as_ref()
            .and_then(ChatMetadata::user)
            .and_then(|metadata| metadata.presentation.as_ref())
            .and_then(|presentation| presentation.display_position.as_deref()),
        "display position",
        64,
    )
}

pub(crate) fn chat_display_color_from_metadata(
    metadata: &Option<ChatMetadata>,
) -> Result<String, String> {
    chat_presentation_text_from_metadata(
        metadata
            .as_ref()
            .and_then(ChatMetadata::user)
            .and_then(|metadata| metadata.presentation.as_ref())
            .and_then(|presentation| presentation.display_color.as_deref()),
        "display color",
        64,
    )
}

fn chat_presentation_text_from_metadata(
    value: Option<&str>,
    field_name: &'static str,
    max_len: usize,
) -> Result<String, String> {
    value
        .map(|raw| validate_chat_metadata_text(raw, field_name, max_len))
        .transpose()
        .map(|value| value.flatten().unwrap_or_default())
}

pub(crate) fn chat_playback_media_id_from_metadata(
    metadata: &Option<ChatMetadata>,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<String, String> {
    let Some(id) = metadata
        .as_ref()
        .and_then(ChatMetadata::user)
        .and_then(|metadata| metadata.playback.as_ref())
        .and_then(|playback| playback.media_id)
    else {
        return Ok(String::new());
    };
    public_id_codec
        .encode_media_id(id)
        .map_err(|error| format!("Failed to encode chat playback media id: {error}"))
}

pub(crate) fn chat_playback_playlist_id_from_metadata(
    metadata: &Option<ChatMetadata>,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<String, String> {
    let Some(id) = metadata
        .as_ref()
        .and_then(ChatMetadata::user)
        .and_then(|metadata| metadata.playback.as_ref())
        .and_then(|playback| playback.playlist_id)
    else {
        return Ok(String::new());
    };
    public_id_codec
        .encode_playlist_id(id)
        .map_err(|error| format!("Failed to encode chat playback playlist id: {error}"))
}

pub(crate) struct ChatPlaybackMetadata {
    pub media_id: String,
    pub playlist_id: String,
    pub target: Option<synctv_proto::client::ProviderTarget>,
    pub target_hash: String,
    pub position_seconds: Option<f64>,
}

pub(crate) fn chat_playback_metadata_from_metadata(
    metadata: &Option<ChatMetadata>,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<ChatPlaybackMetadata, String> {
    let target = chat_playback_target_from_metadata(metadata)?;
    let target_hash = target
        .as_ref()
        .map(chat_playback_target_hash)
        .transpose()?
        .unwrap_or_default();

    Ok(ChatPlaybackMetadata {
        media_id: chat_playback_media_id_from_metadata(metadata, public_id_codec)?,
        playlist_id: chat_playback_playlist_id_from_metadata(metadata, public_id_codec)?,
        target: target
            .as_ref()
            .map(crate::impls::client::convert::provider_target_to_proto),
        target_hash,
        position_seconds: chat_playback_position_seconds_from_metadata(metadata)?,
    })
}

pub(crate) fn chat_playback_target_from_metadata(
    metadata: &Option<ChatMetadata>,
) -> Result<Option<ProviderTarget>, String> {
    Ok(metadata
        .as_ref()
        .and_then(ChatMetadata::user)
        .and_then(|metadata| metadata.playback.as_ref())
        .and_then(|playback| playback.target.clone()))
}

pub(crate) fn chat_playback_position_seconds_from_metadata(
    metadata: &Option<ChatMetadata>,
) -> Result<Option<f64>, String> {
    let Some(seconds) = metadata
        .as_ref()
        .and_then(ChatMetadata::user)
        .and_then(|metadata| metadata.playback.as_ref())
        .and_then(|playback| playback.position_seconds)
    else {
        return Ok(None);
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(
            "Chat playback position_seconds must be a finite non-negative number".to_string(),
        );
    }
    Ok(Some(seconds))
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
    public_id_codec: &crate::public_id::PublicIdCodec,
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
    public_id_codec: &crate::public_id::PublicIdCodec,
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
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<synctv_proto::client::ChatMessagePin, String> {
    Ok(synctv_proto::client::ChatMessagePin {
        pinned_by_user_id: pin
            .pinned_by
            .map(|id| {
                public_id_codec
                    .encode_user_id(id)
                    .map_err(|error| format!("Failed to encode pinned_by user id: {error}"))
            })
            .transpose()?
            .unwrap_or_default(),
        pinned_by_username: pin.pinned_by_username.clone().unwrap_or_default(),
        note: pin.note.clone().unwrap_or_default(),
        pinned_at: pin.pinned_at.timestamp(),
    })
}

pub(crate) fn chat_message_receive_to_proto(
    value: &ChatMessageWithAttachments,
    public_id_codec: &crate::public_id::PublicIdCodec,
    username: String,
) -> Result<synctv_proto::client::ChatMessageReceive, String> {
    let message = &value.message;
    let room_id = public_id_codec
        .encode_room_id(message.room_id)
        .map_err(|error| format!("Failed to encode chat message room id: {error}"))?;
    let user_id = message
        .user_id
        .map(|id| {
            public_id_codec
                .encode_user_id(id)
                .map_err(|error| format!("Failed to encode chat message user id: {error}"))
        })
        .transpose()?
        .unwrap_or_default();
    let deleted_by_user_id = message
        .deleted_by
        .map(|id| {
            public_id_codec.encode_user_id(id).map_err(|error| {
                format!("Failed to encode chat message deleted_by user id: {error}")
            })
        })
        .transpose()?
        .unwrap_or_default();
    let reactions = value
        .reactions
        .iter()
        .map(crate::impls::client::chat_reaction_summary_to_proto)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let reaction_count =
        crate::impls::client::chat_reaction_count(&reactions).map_err(|e| e.to_string())?;
    let mentions = value
        .mentions
        .iter()
        .map(|mention| {
            Ok(synctv_proto::client::ChatMention {
                user_id: public_id_codec
                    .encode_user_id(mention.mentioned_user_id)
                    .map_err(|error| format!("Failed to encode mention user id: {error}"))?,
                username: mention.username.clone().unwrap_or_default(),
                start: mention.start,
                length: mention.length,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let playback = chat_playback_metadata_from_metadata(&message.metadata, public_id_codec)?;
    Ok(synctv_proto::client::ChatMessageReceive {
        id: message.id.to_string(),
        room_id,
        user_id,
        username,
        content: message.content.clone(),
        timestamp: message.created_at.timestamp(),
        display_position: chat_display_position_from_metadata(&message.metadata)?,
        display_color: chat_display_color_from_metadata(&message.metadata)?,
        client_message_id: message.client_message_id.clone().unwrap_or_default(),
        status: chat_status_to_proto(message.status) as i32,
        version: message.version,
        edited_at: message.edited_at.map_or(0, |ts| ts.timestamp()),
        deleted_at: message.deleted_at.map_or(0, |ts| ts.timestamp()),
        reply_to_message_id: message
            .reply_to_message_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        attachments: value
            .attachments
            .iter()
            .map(core_chat_attachment_to_proto)
            .collect::<Result<Vec<_>, _>>()?,
        deleted_by_user_id,
        delete_reason: message.delete_reason.clone().unwrap_or_default(),
        playback_media_id: playback.media_id,
        playback_playlist_id: playback.playlist_id,
        playback_target: playback.target,
        playback_target_hash: playback.target_hash,
        playback_position_seconds: playback.position_seconds,
        reactions,
        reaction_count,
        metadata: crate::impls::client::convert::chat_metadata_to_proto(
            &message.metadata,
            public_id_codec,
        )
        .map_err(|error| error.to_string())?,
        mentions,
        pin: value
            .pin
            .as_ref()
            .map(|pin| chat_message_pin_to_proto(pin, public_id_codec))
            .transpose()?,
        message_type: chat_message_type_to_proto(message.message_type) as i32,
    })
}

pub(crate) fn core_chat_attachment_to_proto(
    attachment: &synctv_core::models::ChatAttachment,
) -> Result<synctv_proto::client::ChatAttachment, String> {
    Ok(synctv_proto::client::ChatAttachment {
        id: attachment.id.clone(),
        url: chat_attachment_url_field(attachment)?,
        object_access: attachment
            .object_access
            .as_ref()
            .map(crate::impls::stored_files::file_object_access_to_proto),
        mime_type: required_chat_attachment_mime_type(attachment)?,
        size_bytes: required_chat_attachment_size_bytes(attachment)?,
        width: attachment.width.unwrap_or_default(),
        height: attachment.height.unwrap_or_default(),
        metadata: crate::impls::client::convert::file_metadata_to_proto(&attachment.metadata)
            .map_err(|error| error.to_string())?,
        filename: attachment.filename.clone().unwrap_or_default(),
        kind: chat_attachment_kind_to_proto(attachment.kind) as i32,
        reuse_token: attachment.reuse_token.clone().unwrap_or_default(),
        reuse_expires_at: attachment
            .reuse_expires_at
            .map(|expires_at| expires_at.timestamp()),
        variants: crate::impls::client::convert::file_object_variants_from_metadata(
            &attachment.metadata,
            "chat attachment",
        )
        .map_err(|error| error.to_string())?,
    })
}

fn chat_attachment_url_field(
    attachment: &synctv_core::models::ChatAttachment,
) -> Result<String, String> {
    let url = attachment
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            attachment
                .object_access
                .as_ref()
                .and_then(crate::impls::stored_files::render_file_object_access_url)
        })
        .ok_or_else(|| "chat attachment url is missing".to_string())?;
    if url.is_empty() {
        return Err("chat attachment url is empty".to_string());
    }
    Ok(url)
}

fn required_chat_attachment_mime_type(
    attachment: &synctv_core::models::ChatAttachment,
) -> Result<String, String> {
    let mime_type = attachment
        .mime_type
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| format!("chat attachment {} is missing mime_type", attachment.id))?;
    if mime_type.is_empty() {
        return Err(format!(
            "chat attachment {} has empty mime_type",
            attachment.id
        ));
    }
    Ok(mime_type.to_string())
}

fn required_chat_attachment_size_bytes(
    attachment: &synctv_core::models::ChatAttachment,
) -> Result<i64, String> {
    match attachment.size_bytes {
        Some(size_bytes) if size_bytes > 0 => Ok(size_bytes),
        _ => Err(format!(
            "chat attachment {} is missing valid size_bytes",
            attachment.id
        )),
    }
}

pub(crate) fn chat_attachment_kind_to_proto(
    kind: synctv_core::models::ChatAttachmentKind,
) -> synctv_proto::client::ChatAttachmentKind {
    match kind {
        synctv_core::models::ChatAttachmentKind::File => {
            synctv_proto::client::ChatAttachmentKind::File
        }
        synctv_core::models::ChatAttachmentKind::Image => {
            synctv_proto::client::ChatAttachmentKind::Image
        }
        synctv_core::models::ChatAttachmentKind::Audio => {
            synctv_proto::client::ChatAttachmentKind::Audio
        }
    }
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
