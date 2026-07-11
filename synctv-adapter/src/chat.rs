use crate::PublicIdCodec;
use synctv_core::models::{
    ChatAttachment, ChatAttachmentKind, ChatEventKind, ChatMessagePin, ChatMessageStatus,
    ChatMessageType, ChatMessageWithAttachments, ChatMetadata, ChatPinEventKind, FileMetadata,
    FileObjectAccess, FileObjectKind, FileObjectVariant, ProviderTarget,
};
use synctv_proto::client as client_proto;

use crate::{AdapterError, AdapterResult};

fn invalid_chat_mapping(message: impl Into<String>) -> AdapterError {
    AdapterError::invalid_input(message)
}

fn proto_encode_error(kind: &str, error: &str) -> AdapterError {
    invalid_chat_mapping(format!("Failed to encode {kind} public id: {error}"))
}

fn encode_media_id_for_proto(
    id: synctv_core::models::MediaId,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<String> {
    public_id_codec
        .encode_media_id(id)
        .map_err(|error| proto_encode_error("media", &error))
}

fn encode_playlist_id_for_proto(
    id: synctv_core::models::PlaylistId,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<String> {
    public_id_codec
        .encode_playlist_id(id)
        .map_err(|error| proto_encode_error("playlist", &error))
}

fn encode_room_id_for_proto(
    id: synctv_core::models::RoomId,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<String> {
    public_id_codec
        .encode_room_id(id)
        .map_err(|error| proto_encode_error("room", &error))
}

fn encode_user_id_for_proto(
    id: synctv_core::models::UserId,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<String> {
    public_id_codec
        .encode_user_id(id)
        .map_err(|error| proto_encode_error("user", &error))
}

#[must_use]
pub fn chat_event_kind_to_proto(kind: ChatEventKind) -> client_proto::ChatMessageEventKind {
    match kind {
        ChatEventKind::Created => client_proto::ChatMessageEventKind::Created,
        ChatEventKind::Edited => client_proto::ChatMessageEventKind::Edited,
        ChatEventKind::Deleted => client_proto::ChatMessageEventKind::Deleted,
        ChatEventKind::ReactionsChanged => client_proto::ChatMessageEventKind::ReactionsChanged,
    }
}

#[must_use]
pub fn chat_message_type_to_proto(message_type: ChatMessageType) -> client_proto::ChatMessageType {
    match message_type {
        ChatMessageType::User => client_proto::ChatMessageType::User,
        ChatMessageType::SystemMemberJoined => client_proto::ChatMessageType::SystemMemberJoined,
    }
}

#[must_use]
pub fn chat_pin_event_kind_to_proto(kind: ChatPinEventKind) -> client_proto::ChatPinEventKind {
    match kind {
        ChatPinEventKind::Pinned => client_proto::ChatPinEventKind::Pinned,
        ChatPinEventKind::Unpinned => client_proto::ChatPinEventKind::Unpinned,
        ChatPinEventKind::MessageUpdated => client_proto::ChatPinEventKind::MessageUpdated,
        ChatPinEventKind::MessageDeleted => client_proto::ChatPinEventKind::MessageDeleted,
    }
}

#[must_use]
pub fn chat_status_to_proto(status: ChatMessageStatus) -> client_proto::ChatMessageStatus {
    match status {
        ChatMessageStatus::Active => client_proto::ChatMessageStatus::Active,
        ChatMessageStatus::Edited => client_proto::ChatMessageStatus::Edited,
        ChatMessageStatus::Deleted => client_proto::ChatMessageStatus::Deleted,
    }
}

pub fn chat_display_position_from_metadata(
    metadata: Option<&ChatMetadata>,
) -> AdapterResult<String> {
    chat_presentation_text_from_metadata(
        metadata
            .and_then(ChatMetadata::user)
            .and_then(|metadata| metadata.presentation.as_ref())
            .and_then(|presentation| presentation.display_position.as_deref()),
        "display position",
        64,
    )
}

pub fn chat_display_color_from_metadata(metadata: Option<&ChatMetadata>) -> AdapterResult<String> {
    chat_presentation_text_from_metadata(
        metadata
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
) -> AdapterResult<String> {
    value
        .map(|raw| validate_chat_metadata_text(raw, field_name, max_len))
        .transpose()
        .map(|value| value.flatten().unwrap_or_default())
}

fn validate_chat_metadata_text(
    value: &str,
    field_name: &str,
    max_len: usize,
) -> AdapterResult<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > max_len || trimmed.chars().any(char::is_control) {
        return Err(invalid_chat_mapping(format!(
            "Chat {field_name} must be 1-{max_len} non-control characters"
        )));
    }
    Ok(Some(trimmed.to_string()))
}

pub fn chat_playback_media_id_from_metadata(
    metadata: Option<&ChatMetadata>,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<String> {
    let Some(id) = metadata
        .and_then(ChatMetadata::user)
        .and_then(|metadata| metadata.playback.as_ref())
        .and_then(|playback| playback.media_id)
    else {
        return Ok(String::new());
    };
    encode_media_id_for_proto(id, public_id_codec)
}

pub fn chat_playback_playlist_id_from_metadata(
    metadata: Option<&ChatMetadata>,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<String> {
    let Some(id) = metadata
        .and_then(ChatMetadata::user)
        .and_then(|metadata| metadata.playback.as_ref())
        .and_then(|playback| playback.playlist_id)
    else {
        return Ok(String::new());
    };
    encode_playlist_id_for_proto(id, public_id_codec)
}

pub struct ChatPlaybackMetadata {
    pub media_id: String,
    pub playlist_id: String,
    pub target: Option<client_proto::ProviderTarget>,
    pub target_hash: String,
    pub position_seconds: Option<f64>,
}

pub fn chat_playback_metadata_from_metadata(
    metadata: Option<&ChatMetadata>,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<ChatPlaybackMetadata> {
    let target = chat_playback_target_from_metadata(metadata);
    let target_hash = target
        .as_ref()
        .map(chat_playback_target_hash)
        .transpose()?
        .unwrap_or_default();

    Ok(ChatPlaybackMetadata {
        media_id: chat_playback_media_id_from_metadata(metadata, public_id_codec)?,
        playlist_id: chat_playback_playlist_id_from_metadata(metadata, public_id_codec)?,
        target: target.as_ref().map(provider_target_to_proto),
        target_hash,
        position_seconds: chat_playback_position_seconds_from_metadata(metadata)?,
    })
}

#[must_use]
pub fn chat_playback_target_from_metadata(
    metadata: Option<&ChatMetadata>,
) -> Option<ProviderTarget> {
    metadata
        .and_then(ChatMetadata::user)
        .and_then(|metadata| metadata.playback.as_ref())
        .and_then(|playback| playback.target.clone())
}

pub fn chat_playback_position_seconds_from_metadata(
    metadata: Option<&ChatMetadata>,
) -> AdapterResult<Option<f64>> {
    let Some(seconds) = metadata
        .and_then(ChatMetadata::user)
        .and_then(|metadata| metadata.playback.as_ref())
        .and_then(|playback| playback.position_seconds)
    else {
        return Ok(None);
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(invalid_chat_mapping(
            "Chat playback position_seconds must be a finite non-negative number",
        ));
    }
    Ok(Some(seconds))
}

pub fn chat_playback_target_hash(target: &ProviderTarget) -> AdapterResult<String> {
    synctv_core::models::try_hash_playback_target(Some(target))
        .map_err(|error| invalid_chat_mapping(error.to_string()))
}

pub fn chat_message_pin_to_proto(
    pin: &ChatMessagePin,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<client_proto::ChatMessagePin> {
    Ok(client_proto::ChatMessagePin {
        pinned_by_user_id: pin
            .pinned_by
            .map(|id| encode_user_id_for_proto(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        pinned_by_username: pin.pinned_by_username.clone().unwrap_or_default(),
        note: pin.note.clone().unwrap_or_default(),
        pinned_at: pin.pinned_at.timestamp(),
    })
}

pub fn chat_message_receive_to_proto(
    value: &ChatMessageWithAttachments,
    public_id_codec: &PublicIdCodec,
    username: String,
) -> AdapterResult<client_proto::ChatMessageReceive> {
    let message = &value.message;
    let room_id = encode_room_id_for_proto(message.room_id, public_id_codec)?;
    let user_id = message
        .user_id
        .map(|id| encode_user_id_for_proto(id, public_id_codec))
        .transpose()?
        .unwrap_or_default();
    let deleted_by_user_id = message
        .deleted_by
        .map(|id| encode_user_id_for_proto(id, public_id_codec))
        .transpose()?
        .unwrap_or_default();
    let reactions = value
        .reactions
        .iter()
        .map(chat_reaction_summary_to_proto)
        .collect::<AdapterResult<Vec<_>>>()?;
    let reaction_count = chat_reaction_count(&reactions)?;
    let mentions = value
        .mentions
        .iter()
        .map(|mention| {
            Ok(client_proto::ChatMention {
                user_id: encode_user_id_for_proto(mention.mentioned_user_id, public_id_codec)?,
                username: mention.username.clone().unwrap_or_default(),
                start: mention.start,
                length: mention.length,
            })
        })
        .collect::<AdapterResult<Vec<_>>>()?;
    let playback =
        chat_playback_metadata_from_metadata(message.metadata.as_ref(), public_id_codec)?;

    Ok(client_proto::ChatMessageReceive {
        id: message.id.to_string(),
        room_id,
        user_id,
        username,
        content: message.content.clone(),
        timestamp: message.created_at.timestamp(),
        display_position: chat_display_position_from_metadata(message.metadata.as_ref())?,
        display_color: chat_display_color_from_metadata(message.metadata.as_ref())?,
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
            .collect::<AdapterResult<Vec<_>>>()?,
        deleted_by_user_id,
        delete_reason: message.delete_reason.clone().unwrap_or_default(),
        playback_media_id: playback.media_id,
        playback_playlist_id: playback.playlist_id,
        playback_target: playback.target,
        playback_target_hash: playback.target_hash,
        playback_position_seconds: playback.position_seconds,
        reactions,
        reaction_count,
        metadata: chat_metadata_to_proto(message.metadata.as_ref(), public_id_codec)?,
        mentions,
        pin: value
            .pin
            .as_ref()
            .map(|pin| chat_message_pin_to_proto(pin, public_id_codec))
            .transpose()?,
        message_type: chat_message_type_to_proto(message.message_type) as i32,
    })
}

pub fn core_chat_attachment_to_proto(
    attachment: &ChatAttachment,
) -> AdapterResult<client_proto::ChatAttachment> {
    Ok(client_proto::ChatAttachment {
        id: attachment.id.clone(),
        url: chat_attachment_url_field(attachment)?,
        object_access: attachment
            .object_access
            .as_ref()
            .map(file_object_access_to_proto),
        mime_type: required_chat_attachment_mime_type(attachment)?,
        size_bytes: required_chat_attachment_size_bytes(attachment)?,
        width: attachment.width.unwrap_or_default(),
        height: attachment.height.unwrap_or_default(),
        metadata: file_metadata_to_proto(&attachment.metadata)?,
        filename: attachment.filename.clone().unwrap_or_default(),
        kind: chat_attachment_kind_to_proto(attachment.kind) as i32,
        reuse_token: attachment.reuse_token.clone().unwrap_or_default(),
        reuse_expires_at: attachment
            .reuse_expires_at
            .map(|expires_at| expires_at.timestamp()),
        variants: file_object_variants_from_metadata(&attachment.metadata)?,
    })
}

fn chat_attachment_url_field(attachment: &ChatAttachment) -> AdapterResult<String> {
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
                .and_then(render_file_object_access_url)
        })
        .ok_or_else(|| invalid_chat_mapping("chat attachment url is missing"))?;
    if url.is_empty() {
        return Err(invalid_chat_mapping("chat attachment url is empty"));
    }
    Ok(url)
}

fn required_chat_attachment_mime_type(attachment: &ChatAttachment) -> AdapterResult<String> {
    let mime_type = attachment
        .mime_type
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| {
            invalid_chat_mapping(format!(
                "chat attachment {} is missing mime_type",
                attachment.id
            ))
        })?;
    if mime_type.is_empty() {
        return Err(invalid_chat_mapping(format!(
            "chat attachment {} has empty mime_type",
            attachment.id
        )));
    }
    Ok(mime_type.to_string())
}

fn required_chat_attachment_size_bytes(attachment: &ChatAttachment) -> AdapterResult<i64> {
    match attachment.size_bytes {
        Some(size_bytes) if size_bytes > 0 => Ok(size_bytes),
        _ => Err(invalid_chat_mapping(format!(
            "chat attachment {} is missing valid size_bytes",
            attachment.id
        ))),
    }
}

#[must_use]
pub fn chat_attachment_kind_to_proto(kind: ChatAttachmentKind) -> client_proto::ChatAttachmentKind {
    match kind {
        ChatAttachmentKind::File => client_proto::ChatAttachmentKind::File,
        ChatAttachmentKind::Image => client_proto::ChatAttachmentKind::Image,
        ChatAttachmentKind::Audio => client_proto::ChatAttachmentKind::Audio,
    }
}

pub fn chat_reaction_summary_to_proto(
    reaction: &synctv_core::models::ChatReactionSummary,
) -> AdapterResult<client_proto::ChatReactionSummary> {
    let key = reaction.key.trim();
    if key.is_empty() {
        return Err(invalid_chat_mapping("chat reaction summary key is empty"));
    }
    if reaction.count < 0 {
        return Err(invalid_chat_mapping(format!(
            "chat reaction summary '{}' has negative count",
            reaction.key
        )));
    }
    Ok(client_proto::ChatReactionSummary {
        key: key.to_string(),
        count: reaction.count,
        reacted_by_me: reaction.reacted_by_me,
    })
}

pub fn chat_reaction_count(reactions: &[client_proto::ChatReactionSummary]) -> AdapterResult<i32> {
    reactions
        .iter()
        .try_fold(0_i64, |total, reaction| {
            if reaction.count < 0 {
                return Err(invalid_chat_mapping(format!(
                    "chat reaction summary '{}' has negative count",
                    reaction.key
                )));
            }
            total
                .checked_add(reaction.count)
                .ok_or_else(|| invalid_chat_mapping("chat reaction count exceeds i64::MAX"))
        })?
        .try_into()
        .map_err(|_| invalid_chat_mapping("chat reaction count exceeds i32::MAX"))
}

pub fn chat_metadata_to_proto(
    metadata: Option<&ChatMetadata>,
    public_id_codec: &PublicIdCodec,
) -> AdapterResult<Option<client_proto::ChatMetadata>> {
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    if metadata.is_empty() {
        return Ok(None);
    }
    match metadata {
        ChatMetadata::User(user) => Ok(Some(client_proto::ChatMetadata {
            metadata: Some(client_proto::chat_metadata::Metadata::User(
                client_proto::ChatUserMetadata {
                    presentation: user.presentation.as_ref().map(|presentation| {
                        client_proto::ChatPresentationMetadata {
                            display_position: presentation.display_position.clone(),
                            display_color: presentation.display_color.clone(),
                        }
                    }),
                    playback: user
                        .playback
                        .as_ref()
                        .map(|playback| {
                            let media_id = playback
                                .media_id
                                .map(|id| encode_media_id_for_proto(id, public_id_codec))
                                .transpose()?
                                .unwrap_or_default();
                            let playlist_id = playback
                                .playlist_id
                                .map(|id| encode_playlist_id_for_proto(id, public_id_codec))
                                .transpose()?
                                .unwrap_or_default();
                            Ok::<_, AdapterError>(client_proto::ChatPlaybackMetadata {
                                media_id,
                                playlist_id,
                                target: playback.target.as_ref().map(provider_target_to_proto),
                                position_seconds: playback.position_seconds,
                            })
                        })
                        .transpose()?,
                },
            )),
        })),
        ChatMetadata::MemberJoined(payload) => Ok(Some(client_proto::ChatMetadata {
            metadata: Some(client_proto::chat_metadata::Metadata::MemberJoined(
                client_proto::ChatMemberJoinedMetadata {
                    user_id: encode_user_id_for_proto(payload.user_id, public_id_codec)?,
                    username: payload.username.clone(),
                    actor_user_id: payload
                        .actor_user_id
                        .map(|id| encode_user_id_for_proto(id, public_id_codec))
                        .transpose()?
                        .unwrap_or_default(),
                    actor_username: payload.actor_username.clone().unwrap_or_default(),
                    role: i32::from(payload.role),
                },
            )),
        })),
    }
}

#[must_use]
pub fn provider_target_to_proto(target: &ProviderTarget) -> client_proto::ProviderTarget {
    match target {
        ProviderTarget::Alist(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Alist(
                client_proto::AlistTarget {
                    relative_path: target.relative_path.clone(),
                },
            )),
        },
        ProviderTarget::Emby(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Emby(
                client_proto::EmbyTarget {
                    item_id: target.item_id.clone(),
                },
            )),
        },
        ProviderTarget::Cloudreve(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Cloudreve(
                client_proto::CloudreveTarget {
                    relative_path: target.relative_path.clone(),
                },
            )),
        },
    }
}

pub fn file_metadata_to_proto(
    metadata: &FileMetadata,
) -> AdapterResult<Option<client_proto::FileMetadata>> {
    let public = metadata.public();
    if public == FileMetadata::default() {
        return Ok(None);
    }
    Ok(Some(client_proto::FileMetadata {
        width: public.width,
        height: public.height,
        duration_seconds: public.audio.as_ref().map(|audio| audio.duration_seconds),
        bitrate_bps: public.audio.as_ref().map(|audio| audio.bitrate_bps),
        blurhash: public.blurhash,
    }))
}

pub fn file_object_variant_to_proto(
    variant: &FileObjectVariant,
) -> AdapterResult<client_proto::FileObjectVariant> {
    let metadata = FileMetadata {
        width: variant.metadata.width,
        height: variant.metadata.height,
        blurhash: variant.metadata.blurhash.clone(),
        ..Default::default()
    };
    let object_access = variant
        .object_access
        .as_ref()
        .map(file_object_access_to_proto);
    let url = variant
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            variant
                .object_access
                .as_ref()
                .and_then(render_file_object_access_url)
        })
        .unwrap_or_default();
    Ok(client_proto::FileObjectVariant {
        key: variant.variant_key.clone(),
        label: variant.label.clone(),
        url,
        mime_type: variant.mime_type.clone(),
        size_bytes: variant.size_bytes,
        width: variant.width.unwrap_or_default(),
        height: variant.height.unwrap_or_default(),
        is_original: variant.is_original,
        lossy: variant.lossy,
        quality: variant.quality,
        metadata: file_metadata_to_proto(&metadata)?,
        object_access,
    })
}

pub fn file_object_variants_from_metadata(
    metadata: &FileMetadata,
) -> AdapterResult<Vec<client_proto::FileObjectVariant>> {
    metadata
        .variants
        .iter()
        .map(file_object_variant_to_proto)
        .collect()
}

#[must_use]
pub fn file_object_access_to_proto(access: &FileObjectAccess) -> client_proto::FileObjectAccess {
    client_proto::FileObjectAccess {
        object_kind: file_object_access_kind_to_proto(access.object_kind) as i32,
        encoded_object_key: access.encoded_object_key.clone(),
        read_token: access.read_token.clone(),
    }
}

#[must_use]
pub const fn file_object_route_prefix(kind: FileObjectKind) -> Option<&'static str> {
    match kind {
        FileObjectKind::ChatAttachment => Some("/api/chat/attachment-objects"),
        FileObjectKind::UserAvatar => Some("/api/user/avatar-objects"),
        FileObjectKind::MediaCover => Some("/api/media/cover-objects"),
        FileObjectKind::MediaThumbnail => Some("/api/media/thumbnail-objects"),
        FileObjectKind::RoomCover => Some("/api/room/cover-objects"),
        FileObjectKind::PlaylistCover => Some("/api/playlist/cover-objects"),
        FileObjectKind::Generic => None,
    }
}

#[must_use]
pub fn render_file_object_access_url(access: &FileObjectAccess) -> Option<String> {
    Some(format!(
        "{}/{encoded_object_key}?token={read_token}",
        file_object_route_prefix(access.object_kind)?,
        encoded_object_key = access.encoded_object_key,
        read_token = access.read_token
    ))
}

#[must_use]
pub const fn file_object_access_kind_to_proto(
    kind: FileObjectKind,
) -> client_proto::FileObjectAccessKind {
    match kind {
        FileObjectKind::ChatAttachment => client_proto::FileObjectAccessKind::ChatAttachment,
        FileObjectKind::UserAvatar => client_proto::FileObjectAccessKind::UserAvatar,
        FileObjectKind::MediaCover => client_proto::FileObjectAccessKind::MediaCover,
        FileObjectKind::MediaThumbnail => client_proto::FileObjectAccessKind::MediaThumbnail,
        FileObjectKind::RoomCover => client_proto::FileObjectAccessKind::RoomCover,
        FileObjectKind::PlaylistCover => client_proto::FileObjectAccessKind::PlaylistCover,
        FileObjectKind::Generic => client_proto::FileObjectAccessKind::Generic,
    }
}
