use prost::Message;
use sha2::{Digest, Sha256};
use synctv_core::models::{
    ChatEventKind, ChatMessageEvent, ChatMessageStatus, NewStoredFile, RoomId, RoomPlaybackState,
};

use synctv_proto::client::{ClientMessage, ServerMessage};

pub(super) fn playback_state_to_proto(
    state: &RoomPlaybackState,
    encode_room: &impl Fn(RoomId) -> Result<String, String>,
    encode_media: &impl Fn(synctv_core::models::MediaId) -> Result<String, String>,
    encode_playlist: &impl Fn(synctv_core::models::PlaylistId) -> Result<String, String>,
) -> Result<synctv_proto::client::PlaybackState, String> {
    let position = state.computed_position();
    if !position.is_finite() || position < 0.0 {
        return Err("Playback position must be a finite non-negative number".to_string());
    }
    if !state.speed.is_finite() || state.speed <= 0.0 {
        return Err("Playback speed must be a finite positive number".to_string());
    }
    if state.version < 0 {
        return Err("Playback version must be non-negative".to_string());
    }

    Ok(synctv_proto::client::PlaybackState {
        room_id: encode_room(state.room_id)?,
        playing_media_id: state
            .playing_media_id
            .as_ref()
            .map(|id| encode_media(*id))
            .transpose()?
            .unwrap_or_default(),
        position,
        speed: state.speed,
        is_playing: state.is_playing,
        updated_at: state.updated_at.timestamp(),
        version: state.version,
        playing_playlist_id: state
            .playing_playlist_id
            .as_ref()
            .map(|id| encode_playlist(*id))
            .transpose()?
            .unwrap_or_default(),
        target: state.target.clone(),
        target_hash: state.target_hash(),
    })
}

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

pub(super) fn encode_non_empty_media_ids(
    media_ids: &[synctv_core::models::MediaId],
    encode_media: &impl Fn(synctv_core::models::MediaId) -> Result<String, String>,
    field_name: &'static str,
) -> Result<Vec<String>, String> {
    if media_ids.is_empty() {
        return Err(format!("Realtime {field_name} media_ids must not be empty"));
    }
    media_ids
        .iter()
        .map(|id| encode_media(*id))
        .collect::<Result<Vec<_>, _>>()
}

pub(super) fn validated_room_settings_json(settings_json: &[u8]) -> Result<Vec<u8>, String> {
    if settings_json.is_empty() {
        return Err("Room settings JSON must not be empty".to_string());
    }
    let value: serde_json::Value = serde_json::from_slice(settings_json)
        .map_err(|error| format!("Room settings JSON is invalid: {error}"))?;
    if !value.is_object() {
        return Err("Room settings JSON must be an object".to_string());
    }
    Ok(settings_json.to_vec())
}

pub(super) fn validated_non_negative_version(
    version: i64,
    field_name: &'static str,
) -> Result<i64, String> {
    if version < 0 {
        return Err(format!("{field_name} version must be non-negative"));
    }
    Ok(version)
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
    metadata: &serde_json::Value,
) -> Result<String, String> {
    chat_presentation_text_from_metadata(metadata, "display_position", "display position", 64)
}

pub(crate) fn chat_display_color_from_metadata(
    metadata: &serde_json::Value,
) -> Result<String, String> {
    chat_presentation_text_from_metadata(metadata, "display_color", "display color", 64)
}

fn chat_presentation_text_from_metadata(
    metadata: &serde_json::Value,
    key: &'static str,
    field_name: &'static str,
    max_len: usize,
) -> Result<String, String> {
    let Some(presentation) = optional_chat_metadata_object(metadata, "presentation")? else {
        return Ok(String::new());
    };
    let Some(value) = presentation.get(key) else {
        return Ok(String::new());
    };
    let raw = value
        .as_str()
        .ok_or_else(|| format!("Chat {field_name} must be a string"))?;
    Ok(validate_chat_metadata_text(raw, field_name, max_len)?.unwrap_or_default())
}

pub(crate) fn chat_playback_media_id_from_metadata(
    metadata: &serde_json::Value,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<String, String> {
    let Some(id) = chat_playback_positive_id_from_metadata(metadata, "media_id")? else {
        return Ok(String::new());
    };
    let id = synctv_core::models::MediaId::try_from(id)
        .map_err(|_| "Invalid chat playback media_id".to_string())?;
    public_id_codec
        .encode_media_id(id)
        .map_err(|error| format!("Failed to encode chat playback media id: {error}"))
}

pub(crate) fn chat_playback_playlist_id_from_metadata(
    metadata: &serde_json::Value,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<String, String> {
    let Some(id) = chat_playback_positive_id_from_metadata(metadata, "playlist_id")? else {
        return Ok(String::new());
    };
    let id = synctv_core::models::PlaylistId::try_from(id)
        .map_err(|_| "Invalid chat playback playlist_id".to_string())?;
    public_id_codec
        .encode_playlist_id(id)
        .map_err(|error| format!("Failed to encode chat playback playlist id: {error}"))
}

fn chat_playback_positive_id_from_metadata(
    metadata: &serde_json::Value,
    field: &str,
) -> Result<Option<i64>, String> {
    let Some(playback) = optional_chat_metadata_object(metadata, "playback")? else {
        return Ok(None);
    };
    let Some(value) = playback.get(field) else {
        return Ok(None);
    };
    let raw = value
        .as_str()
        .ok_or_else(|| format!("Chat playback {field} must be a string"))?
        .trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let id = raw
        .parse::<i64>()
        .map_err(|_| format!("Chat playback {field} must be a positive integer"))?;
    if id <= 0 {
        return Err(format!("Chat playback {field} must be positive"));
    }
    Ok(Some(id))
}

pub(crate) struct ChatPlaybackMetadata {
    pub media_id: String,
    pub playlist_id: String,
    pub target: Vec<u8>,
    pub target_hash: String,
    pub position_seconds: Option<f64>,
}

pub(crate) fn chat_playback_metadata_from_metadata(
    metadata: &serde_json::Value,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<ChatPlaybackMetadata, String> {
    let target = chat_playback_target_from_metadata(metadata)?;
    let target_hash = if target.is_empty() {
        String::new()
    } else {
        chat_playback_target_hash(&target)
    };

    Ok(ChatPlaybackMetadata {
        media_id: chat_playback_media_id_from_metadata(metadata, public_id_codec)?,
        playlist_id: chat_playback_playlist_id_from_metadata(metadata, public_id_codec)?,
        target,
        target_hash,
        position_seconds: chat_playback_position_seconds_from_metadata(metadata)?,
    })
}

pub(crate) fn chat_playback_target_from_metadata(
    metadata: &serde_json::Value,
) -> Result<Vec<u8>, String> {
    let Some(playback) = optional_chat_metadata_object(metadata, "playback")? else {
        return Ok(Vec::new());
    };
    let Some(value) = playback.get("target_hex") else {
        return Ok(Vec::new());
    };
    let raw_target = value
        .as_str()
        .ok_or_else(|| "Chat playback target_hex must be a string".to_string())?
        .trim();
    if raw_target.is_empty() {
        return Ok(Vec::new());
    }

    hex::decode(raw_target).map_err(|error| format!("Invalid chat playback target_hex: {error}"))
}

pub(crate) fn chat_playback_position_seconds_from_metadata(
    metadata: &serde_json::Value,
) -> Result<Option<f64>, String> {
    let Some(playback) = optional_chat_metadata_object(metadata, "playback")? else {
        return Ok(None);
    };
    let Some(value) = playback.get("position_seconds") else {
        return Ok(None);
    };
    let seconds = value
        .as_f64()
        .ok_or_else(|| "Chat playback position_seconds must be a number".to_string())?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(
            "Chat playback position_seconds must be a finite non-negative number".to_string(),
        );
    }
    Ok(Some(seconds))
}

fn optional_chat_metadata_object<'a>(
    metadata: &'a serde_json::Value,
    key: &'static str,
) -> Result<Option<&'a serde_json::Map<String, serde_json::Value>>, String> {
    metadata
        .get(key)
        .map(|value| chat_metadata_object(value, key))
        .transpose()
}

fn chat_metadata_object<'a>(
    value: &'a serde_json::Value,
    key: &'static str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("Chat metadata {key} must be an object"))
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

pub(super) fn optional_chat_metadata_text(
    value: Option<&str>,
    field_name: &str,
    max_len: usize,
) -> Result<Option<String>, String> {
    value
        .map(|value| validate_chat_metadata_text(value, field_name, max_len))
        .transpose()
        .map(Option::flatten)
}

pub(crate) fn chat_playback_target_hash(target: &[u8]) -> String {
    hex::encode(Sha256::digest(target))
}

pub(crate) fn chat_metadata_for_send(
    base: serde_json::Value,
    display_position: &str,
    display_color: &str,
    playback_state: Option<&RoomPlaybackState>,
) -> Result<serde_json::Value, String> {
    let serde_json::Value::Object(mut metadata) = base else {
        return Err("metadata must be a JSON object".to_string());
    };
    metadata.remove("position");
    metadata.remove("color");

    let display_position = validate_chat_metadata_text(display_position, "display position", 64)?;
    let display_color = validate_chat_metadata_text(display_color, "display color", 64)?;
    let mut presentation = serde_json::Map::new();
    if let Some(display_position) = display_position {
        presentation.insert(
            "display_position".to_string(),
            serde_json::Value::String(display_position),
        );
    }
    if let Some(display_color) = display_color {
        presentation.insert(
            "display_color".to_string(),
            serde_json::Value::String(display_color),
        );
    }
    if presentation.is_empty() {
        metadata.remove("presentation");
    } else {
        metadata.insert(
            "presentation".to_string(),
            serde_json::Value::Object(presentation),
        );
    }

    if let Some(state) = playback_state
        .filter(|state| state.playing_media_id.is_some() || state.playing_playlist_id.is_some())
    {
        let mut playback = serde_json::Map::new();
        if let Some(media_id) = state.playing_media_id {
            playback.insert(
                "media_id".to_string(),
                serde_json::Value::String(media_id.as_i64().to_string()),
            );
        }
        if let Some(playlist_id) = state.playing_playlist_id {
            playback.insert(
                "playlist_id".to_string(),
                serde_json::Value::String(playlist_id.as_i64().to_string()),
            );
        }
        if !state.target.is_empty() {
            playback.insert(
                "target_hex".to_string(),
                serde_json::Value::String(hex::encode(&state.target)),
            );
        }
        let position_seconds = state.computed_position().max(0.0);
        if position_seconds.is_finite() {
            playback.insert(
                "position_seconds".to_string(),
                serde_json::json!(position_seconds),
            );
        }
        metadata.insert("playback".to_string(), serde_json::Value::Object(playback));
    } else {
        metadata.remove("playback");
    }

    Ok(serde_json::Value::Object(metadata))
}

pub(crate) fn chat_message_event_to_proto(
    event: &ChatMessageEvent,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::ChatMessageEvent, String> {
    let message = &event.message.message;
    let room_id = public_id_codec
        .encode_room_id(message.room_id)
        .map_err(|error| format!("Failed to encode chat event room id: {error}"))?;
    let user_id = message
        .user_id
        .map(|id| {
            public_id_codec
                .encode_user_id(id)
                .map_err(|error| format!("Failed to encode chat event user id: {error}"))
        })
        .transpose()?
        .unwrap_or_default();
    let deleted_by_user_id = message
        .deleted_by
        .map(|id| {
            public_id_codec
                .encode_user_id(id)
                .map_err(|error| format!("Failed to encode chat event deleted_by user id: {error}"))
        })
        .transpose()?
        .unwrap_or_default();
    let reactions = event
        .message
        .reactions
        .iter()
        .map(crate::impls::client::chat_reaction_summary_to_proto)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let reaction_count =
        crate::impls::client::chat_reaction_count(&reactions).map_err(|e| e.to_string())?;
    let playback = chat_playback_metadata_from_metadata(&message.metadata, public_id_codec)?;
    Ok(synctv_proto::client::ChatMessageEvent {
        event_id: event.event_id.clone(),
        room_id: room_id.clone(),
        kind: chat_event_kind_to_proto(event.kind) as i32,
        message: Some(synctv_proto::client::ChatMessageReceive {
            id: message.id.to_string(),
            room_id,
            user_id,
            username: String::new(),
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
            images: event
                .message
                .images
                .iter()
                .map(core_chat_image_to_proto)
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
        }),
        occurred_at: event.occurred_at.timestamp(),
        sequence: event.sequence,
    })
}

pub(crate) fn core_chat_image_to_proto(
    image: &synctv_core::models::ChatImage,
) -> Result<synctv_proto::client::ChatImage, String> {
    Ok(synctv_proto::client::ChatImage {
        id: image.id.clone(),
        storage_backend: image.storage_backend.clone(),
        object_key: image.object_key.clone(),
        url: required_chat_image_url(image)?,
        mime_type: required_chat_image_mime_type(image)?,
        size_bytes: required_chat_image_size_bytes(image)?,
        width: required_chat_image_dimension(image, image.width, "width")?,
        height: required_chat_image_dimension(image, image.height, "height")?,
        metadata: crate::impls::client::convert::json_to_vec(
            &image.metadata,
            "chat image metadata",
        )
        .map_err(|error| error.to_string())?,
    })
}

fn required_chat_image_url(image: &synctv_core::models::ChatImage) -> Result<String, String> {
    let url = image
        .url
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| "chat image url is missing".to_string())?;
    if url.is_empty() {
        return Err("chat image url is empty".to_string());
    }
    Ok(url.to_string())
}

fn required_chat_image_mime_type(image: &synctv_core::models::ChatImage) -> Result<String, String> {
    let mime_type = image
        .mime_type
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| format!("chat image {} is missing mime_type", image.id))?;
    if mime_type.is_empty() {
        return Err(format!("chat image {} has empty mime_type", image.id));
    }
    Ok(mime_type.to_string())
}

fn required_chat_image_size_bytes(image: &synctv_core::models::ChatImage) -> Result<i64, String> {
    match image.size_bytes {
        Some(size_bytes) if size_bytes > 0 => Ok(size_bytes),
        _ => Err(format!(
            "chat image {} is missing valid size_bytes",
            image.id
        )),
    }
}

fn required_chat_image_dimension(
    image: &synctv_core::models::ChatImage,
    value: Option<i32>,
    field: &'static str,
) -> Result<i32, String> {
    match value {
        Some(value) if value > 0 => Ok(value),
        _ => Err(format!("chat image {} is missing valid {field}", image.id)),
    }
}

pub(crate) fn proto_chat_image_to_core(
    image: &synctv_proto::client::ChatImage,
) -> Result<NewStoredFile, String> {
    let metadata = if image.metadata.is_empty() {
        serde_json::Value::Object(Default::default())
    } else {
        serde_json::from_slice(&image.metadata).map_err(|error| error.to_string())?
    };
    Ok(NewStoredFile {
        id: image.id.clone(),
        storage_backend: image.storage_backend.clone(),
        object_key: image.object_key.clone(),
        url: (!image.url.trim().is_empty()).then(|| image.url.clone()),
        mime_type: (!image.mime_type.trim().is_empty()).then(|| image.mime_type.clone()),
        size_bytes: (image.size_bytes > 0).then_some(image.size_bytes),
        width: (image.width > 0).then_some(image.width),
        height: (image.height > 0).then_some(image.height),
        metadata,
    })
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
