use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    models::{
        ChatMessage, ChatMessageEventLog, ChatPlaybackMessagesQuery, ChatReadState,
        CreateChatImageUploadSession, CreateFileUploadSession, DeleteChatMessage, EditChatMessage,
        NewStoredFile, RoomId, SendChatMessage, UserId, CHAT_CLIENT_MESSAGE_ID_MAX_CHARS,
        CHAT_CLIENT_OPERATION_ID_MAX_CHARS, CHAT_IMAGE_ID_MAX_CHARS, CHAT_REACTION_KEY_MAX_CHARS,
        FILE_OBJECT_KEY_MAX_CHARS, FILE_STORAGE_BACKEND_MAX_CHARS,
    },
    Error, Result,
};

use super::MAX_CHAT_IMAGES_PER_MESSAGE;
use crate::service::file_storage::{FILE_OWNERSHIP_PROOF_KEY, FILE_UPLOAD_TOKEN_KEY};
use crate::service::file_upload_policies::chat_image_upload_policy;

pub(super) fn max_messages_to_keep_count(max_messages: u64) -> Result<i32> {
    i32::try_from(max_messages)
        .map_err(|_| Error::InvalidInput("chat max_messages exceeds i32::MAX".to_string()))
}

pub(super) fn validate_chat_playback_query(
    query: ChatPlaybackMessagesQuery,
) -> Result<ChatPlaybackMessagesQuery> {
    let query = query.normalize();
    if query.media_id.is_none() && query.playlist_id.is_none() && query.target.is_none() {
        return Err(Error::InvalidInput(
            "chat playback query requires a media id, playlist id, or target".to_string(),
        ));
    }
    if !query.position_seconds.is_finite()
        || query.position_seconds < 0.0
        || !query.before_seconds.is_finite()
        || query.before_seconds < 0.0
        || !query.after_seconds.is_finite()
        || query.after_seconds < 0.0
    {
        return Err(Error::InvalidInput(
            "chat playback query time window must be non-negative finite seconds".to_string(),
        ));
    }
    if !(1..=500).contains(&query.limit) {
        return Err(Error::InvalidInput(
            "chat playback query limit must be between 1 and 500".to_string(),
        ));
    }
    Ok(query)
}

pub(super) fn empty_read_state(room_id: RoomId, user_id: UserId) -> ChatReadState {
    ChatReadState {
        room_id,
        user_id,
        last_read_message_id: None,
        last_read_message_created_at: None,
        last_read_event_id: None,
        last_read_event_sequence: None,
        updated_at: Utc::now(),
    }
}

pub(super) fn read_state_covers_message(
    state: Option<&ChatReadState>,
    message: &ChatMessage,
    event: Option<&ChatMessageEventLog>,
) -> bool {
    let Some(state) = state else {
        return false;
    };
    if let (Some(current_sequence), Some(target)) = (
        state.last_read_event_sequence,
        event.map(|event| event.sequence),
    ) {
        if current_sequence >= target {
            return true;
        }
    }
    if let (Some(message_id), Some(created_at)) = (
        state.last_read_message_id,
        state.last_read_message_created_at,
    ) {
        let current_cursor = (created_at, message_id);
        let target_cursor = (message.created_at, message.id);
        return current_cursor > target_cursor
            || (event.is_none() && current_cursor == target_cursor);
    }
    false
}

pub(super) fn validate_client_message_id(client_message_id: Option<&str>) -> Result<()> {
    if let Some(id) = client_message_id {
        let len = id.chars().count();
        if !(1..=CHAT_CLIENT_MESSAGE_ID_MAX_CHARS).contains(&len) {
            return Err(Error::InvalidInput(format!(
                "client_message_id must be between 1 and {CHAT_CLIENT_MESSAGE_ID_MAX_CHARS} characters"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_client_operation_id(client_operation_id: Option<&str>) -> Result<()> {
    if let Some(id) = client_operation_id {
        let len = id.chars().count();
        if !(1..=CHAT_CLIENT_OPERATION_ID_MAX_CHARS).contains(&len) {
            return Err(Error::InvalidInput(format!(
                "client_operation_id must be between 1 and {CHAT_CLIENT_OPERATION_ID_MAX_CHARS} characters"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_chat_reaction_key(key: &str) -> Result<()> {
    let len = key.chars().count();
    if !(1..=CHAT_REACTION_KEY_MAX_CHARS).contains(&len) {
        return Err(Error::InvalidInput(format!(
            "reaction_key must be between 1 and {CHAT_REACTION_KEY_MAX_CHARS} characters"
        )));
    }
    if key.trim() != key || key.chars().any(char::is_control) {
        return Err(Error::InvalidInput(
            "reaction_key must not contain control characters or surrounding whitespace"
                .to_string(),
        ));
    }
    Ok(())
}

pub(super) fn normalize_chat_mentions(
    content: &str,
    mentions: &mut Vec<crate::models::ChatMentionInput>,
) -> Result<()> {
    mentions.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.length.cmp(&right.length))
            .then_with(|| left.user_id.cmp(&right.user_id))
    });
    mentions.dedup_by(|left, right| {
        left.user_id == right.user_id && left.start == right.start && left.length == right.length
    });
    if mentions.len() > 20 {
        return Err(Error::InvalidInput(
            "chat message supports at most 20 mentioned users".to_string(),
        ));
    }
    let content_chars = content.chars().collect::<Vec<_>>();
    let mut previous_end = 0_i32;
    for mention in mentions {
        if mention.start < previous_end {
            return Err(Error::InvalidInput(
                "chat mentions must not overlap".to_string(),
            ));
        }
        let start = usize::try_from(mention.start)
            .map_err(|_| Error::InvalidInput("chat mention start is invalid".to_string()))?;
        let length = usize::try_from(mention.length)
            .map_err(|_| Error::InvalidInput("chat mention length is invalid".to_string()))?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| Error::InvalidInput("chat mention range overflow".to_string()))?;
        if end > content_chars.len() {
            return Err(Error::InvalidInput(
                "chat mention range exceeds content length".to_string(),
            ));
        }
        let slice = &content_chars[start..end];
        if slice.first().copied() != Some('@') {
            return Err(Error::InvalidInput(
                "chat mention range must start with @".to_string(),
            ));
        }
        if slice.iter().any(|ch| ch.is_control() || ch.is_whitespace()) {
            return Err(Error::InvalidInput(
                "chat mention range must be a single @ token".to_string(),
            ));
        }
        previous_end = i32::try_from(end)
            .map_err(|_| Error::InvalidInput("chat mention range is too large".to_string()))?;
    }
    Ok(())
}

pub(super) fn chat_image_upload_request_to_file_request(
    request: CreateChatImageUploadSession,
) -> CreateFileUploadSession {
    CreateFileUploadSession {
        user_id: request.user_id,
        storage_scope: chat_file_storage_scope(request.room_id, request.user_id),
        client_file_id: request.client_image_id,
        mime_type: request.mime_type,
        size_bytes: request.size_bytes,
        width: request.width,
        height: request.height,
        checksum_sha256: request.checksum_sha256,
        metadata: request.metadata,
        policy: chat_image_upload_policy(),
    }
}

pub(super) fn chat_file_storage_scope(room_id: RoomId, user_id: UserId) -> String {
    format!("rooms/{}/users/{}", room_id.as_i64(), user_id.as_i64())
}

pub(super) fn validate_chat_metadata(metadata: &serde_json::Value) -> Result<()> {
    if !metadata.is_object() {
        return Err(Error::InvalidInput(
            "chat metadata must be a JSON object".to_string(),
        ));
    }
    Ok(())
}

fn validate_chat_image_mime_type(mime_type: &str) -> Result<()> {
    let policy = chat_image_upload_policy();
    let normalized = mime_type.trim().to_ascii_lowercase();
    let allowed_exact = policy
        .allowed_mime_types
        .iter()
        .any(|allowed| normalized == allowed.trim().to_ascii_lowercase());
    let allowed_prefix = policy
        .allowed_mime_prefixes
        .iter()
        .any(|prefix| normalized.starts_with(&prefix.trim().to_ascii_lowercase()));
    if allowed_exact || allowed_prefix {
        return Ok(());
    }
    Err(Error::InvalidInput(
        "chat image mime_type is not allowed".to_string(),
    ))
}

pub(super) fn validate_chat_images(images: &[NewStoredFile]) -> Result<()> {
    if images.len() > MAX_CHAT_IMAGES_PER_MESSAGE {
        return Err(Error::InvalidInput(format!(
            "Chat messages support at most {MAX_CHAT_IMAGES_PER_MESSAGE} images"
        )));
    }
    let mut image_ids = std::collections::HashSet::with_capacity(images.len());
    let mut object_keys = std::collections::HashSet::with_capacity(images.len());
    for image in images {
        if image.id.trim().is_empty() || image.id.chars().count() > CHAT_IMAGE_ID_MAX_CHARS {
            return Err(Error::InvalidInput(format!(
                "image id must be between 1 and {CHAT_IMAGE_ID_MAX_CHARS} characters"
            )));
        }
        if image.storage_backend.trim().is_empty()
            || image.storage_backend.chars().count() > FILE_STORAGE_BACKEND_MAX_CHARS
            || image.object_key.trim().is_empty()
            || image.object_key.chars().count() > FILE_OBJECT_KEY_MAX_CHARS
        {
            return Err(Error::InvalidInput(format!(
                "file storage_backend must be 1-{FILE_STORAGE_BACKEND_MAX_CHARS} characters and object_key must be 1-{FILE_OBJECT_KEY_MAX_CHARS} characters"
            )));
        }
        if !image_ids.insert(image.id.as_str()) {
            return Err(Error::InvalidInput(
                "duplicate image id in one message".to_string(),
            ));
        }
        if !object_keys.insert(image.object_key.as_str()) {
            return Err(Error::InvalidInput(
                "duplicate image object_key in one message".to_string(),
            ));
        }
        if image.size_bytes.is_some_and(|size| size <= 0)
            || image.width.is_some_and(|width| width <= 0)
            || image.height.is_some_and(|height| height <= 0)
        {
            return Err(Error::InvalidInput(
                "image size and dimensions must be positive".to_string(),
            ));
        }
        if let Some(mime_type) = &image.mime_type {
            validate_chat_image_mime_type(mime_type)?;
        }
        validate_chat_metadata(&image.metadata)?;
    }
    Ok(())
}

pub(super) fn strip_internal_chat_image_metadata(images: &mut [NewStoredFile]) {
    for image in images {
        if let Some(metadata) = image.metadata.as_object_mut() {
            metadata.remove(FILE_UPLOAD_TOKEN_KEY);
            metadata.remove(FILE_OWNERSHIP_PROOF_KEY);
        }
    }
}

pub(super) fn chat_send_request_hash(request: &SendChatMessage) -> Result<String> {
    let payload = json!({
        "content": request.content,
        "message_type": request.message_type,
        "reply_to_message_id": request.reply_to_message_id,
        "metadata": request.metadata,
        "mentions": request.mentions,
        "images": request.images,
    });
    let bytes = serde_json::to_vec(&payload)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub(super) fn chat_edit_request_hash(request: &EditChatMessage) -> Result<String> {
    let payload = json!({
        "message_id": request.message_id,
        "content": request.content,
        "metadata": request.metadata,
        "expected_version": request.expected_version,
    });
    let bytes = serde_json::to_vec(&payload)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub(super) fn chat_delete_request_hash(request: &DeleteChatMessage) -> Result<String> {
    let payload = json!({
        "message_id": request.message_id,
        "reason": request.reason,
        "expected_version": request.expected_version,
    });
    let bytes = serde_json::to_vec(&payload)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub(super) fn ensure_message_owner(message: &ChatMessage, user_id: &UserId) -> Result<()> {
    if message.user_id.as_ref() == Some(user_id) {
        Ok(())
    } else {
        Err(Error::Authorization(
            "Only the sender can edit this message".to_string(),
        ))
    }
}
