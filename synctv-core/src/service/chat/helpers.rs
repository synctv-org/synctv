use chrono::{Duration, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    models::{
        ChatAttachment, ChatMessage, ChatMessageEventLog, ChatPlaybackMessagesQuery, ChatReadState,
        ChatSearchMessagesQuery, CreateChatAttachmentUploadSession, CreateFileUploadSession,
        DeleteChatMessage, EditChatMessage, NewStoredFile, PinChatMessage, RoomId, SendChatMessage,
        SubmittedFileReference, SubmittedFileReferenceKind, UnpinChatMessage, UserId,
        CHAT_ATTACHMENT_FILENAME_MAX_CHARS, CHAT_ATTACHMENT_ID_MAX_CHARS,
        CHAT_CLIENT_MESSAGE_ID_MAX_CHARS, CHAT_CLIENT_OPERATION_ID_MAX_CHARS,
        CHAT_REACTION_KEY_MAX_CHARS, FILE_OBJECT_KEY_MAX_CHARS, FILE_STORAGE_BACKEND_MAX_CHARS,
    },
    Error, Result,
};

use super::MAX_CHAT_ATTACHMENTS_PER_MESSAGE;
use crate::service::file_storage::{
    validate_file_mime_type, CreateFileReuseGrant, FileStorageService, FILE_OWNERSHIP_PROOF_KEY,
    FILE_UPLOAD_TOKEN_KEY,
};
use crate::service::file_upload_policies::chat_attachment_upload_policy;

pub(super) const CHAT_ATTACHMENT_REUSE_SOURCE_KIND: &str = "chat_message_attachment";
const CHAT_ATTACHMENT_REUSE_TOKEN_TTL_SECONDS: i64 = 3600;

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

pub(super) fn validate_chat_search_query(
    mut query: ChatSearchMessagesQuery,
) -> Result<ChatSearchMessagesQuery> {
    query.query = crate::repository::query_builder::normalize_search_text(&query.query)
        .ok_or_else(|| Error::InvalidInput("chat search query is required".to_string()))?;
    let query_chars = query.query.chars().count();
    if !(2..=120).contains(&query_chars) {
        return Err(Error::InvalidInput(
            "chat search query must be between 2 and 120 characters".to_string(),
        ));
    }
    if !(1..=100).contains(&query.limit) {
        return Err(Error::InvalidInput(
            "chat search limit must be between 1 and 100".to_string(),
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

pub(super) fn chat_attachment_upload_request_to_file_request(
    request: CreateChatAttachmentUploadSession,
) -> CreateFileUploadSession {
    CreateFileUploadSession {
        user_id: request.user_id,
        storage_scope: chat_file_storage_scope(request.room_id, request.user_id),
        client_file_id: request.client_attachment_id,
        filename: request.filename,
        mime_type: request.mime_type,
        size_bytes: request.size_bytes,
        width: request.width,
        height: request.height,
        duration_seconds: request.duration_seconds,
        bitrate_bps: request.bitrate_bps,
        parts: request.parts,
        metadata: request.metadata,
        policy: chat_attachment_upload_policy(),
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

fn validate_chat_attachment_mime_type(mime_type: &str) -> Result<()> {
    let policy = chat_attachment_upload_policy();
    validate_file_mime_type(&policy, mime_type)
}

pub(super) fn validate_chat_attachments(attachments: &[NewStoredFile]) -> Result<()> {
    if attachments.len() > MAX_CHAT_ATTACHMENTS_PER_MESSAGE {
        return Err(Error::InvalidInput(format!(
            "Chat messages support at most {MAX_CHAT_ATTACHMENTS_PER_MESSAGE} attachments"
        )));
    }
    let mut attachment_ids = std::collections::HashSet::with_capacity(attachments.len());
    let mut object_keys = std::collections::HashSet::with_capacity(attachments.len());
    for attachment in attachments {
        if attachment.id.trim().is_empty()
            || attachment.id.chars().count() > CHAT_ATTACHMENT_ID_MAX_CHARS
        {
            return Err(Error::InvalidInput(format!(
                "attachment id must be between 1 and {CHAT_ATTACHMENT_ID_MAX_CHARS} characters"
            )));
        }
        if let Some(filename) = &attachment.filename {
            if filename.trim().is_empty()
                || filename.chars().count() > CHAT_ATTACHMENT_FILENAME_MAX_CHARS
                || filename.chars().any(char::is_control)
            {
                return Err(Error::InvalidInput(format!(
                    "attachment filename must be between 1 and {CHAT_ATTACHMENT_FILENAME_MAX_CHARS} characters without control characters"
                )));
            }
        }
        if attachment.storage_backend.trim().is_empty()
            || attachment.storage_backend.chars().count() > FILE_STORAGE_BACKEND_MAX_CHARS
            || attachment.object_key.trim().is_empty()
            || attachment.object_key.chars().count() > FILE_OBJECT_KEY_MAX_CHARS
        {
            return Err(Error::InvalidInput(format!(
                "file storage_backend must be 1-{FILE_STORAGE_BACKEND_MAX_CHARS} characters and object_key must be 1-{FILE_OBJECT_KEY_MAX_CHARS} characters"
            )));
        }
        if !attachment_ids.insert(attachment.id.as_str()) {
            return Err(Error::InvalidInput(
                "duplicate attachment id in one message".to_string(),
            ));
        }
        if !object_keys.insert(attachment.object_key.as_str()) {
            return Err(Error::InvalidInput(
                "duplicate attachment object_key in one message".to_string(),
            ));
        }
        if attachment
            .mime_type
            .as_deref()
            .is_none_or(|mime| mime.trim().is_empty())
        {
            return Err(Error::InvalidInput(
                "attachment mime_type is required".to_string(),
            ));
        }
        if attachment.size_bytes.is_none() {
            return Err(Error::InvalidInput(
                "attachment size_bytes is required".to_string(),
            ));
        }
        if attachment.size_bytes.is_some_and(|size| size <= 0)
            || attachment.width.is_some_and(|width| width <= 0)
            || attachment.height.is_some_and(|height| height <= 0)
        {
            return Err(Error::InvalidInput(
                "attachment size and dimensions must be positive".to_string(),
            ));
        }
        if let Some(mime_type) = &attachment.mime_type {
            validate_chat_attachment_mime_type(mime_type)?;
        }
        validate_chat_metadata(&attachment.metadata)?;
    }
    Ok(())
}

pub(super) fn validate_submitted_chat_attachments(
    attachments: &[SubmittedFileReference],
) -> Result<()> {
    if attachments.len() > MAX_CHAT_ATTACHMENTS_PER_MESSAGE {
        return Err(Error::InvalidInput(format!(
            "Chat messages support at most {MAX_CHAT_ATTACHMENTS_PER_MESSAGE} attachments"
        )));
    }
    let mut attachment_ids = std::collections::HashSet::with_capacity(attachments.len());
    for attachment in attachments {
        match attachment.kind {
            SubmittedFileReferenceKind::Upload => {
                if attachment.id.trim().is_empty()
                    || attachment.id.chars().count() > CHAT_ATTACHMENT_ID_MAX_CHARS
                {
                    return Err(Error::InvalidInput(format!(
                        "attachment id must be between 1 and {CHAT_ATTACHMENT_ID_MAX_CHARS} characters"
                    )));
                }
            }
            SubmittedFileReferenceKind::Reuse => {
                if attachment.id.trim().is_empty() || attachment.id.chars().count() > 4096 {
                    return Err(Error::InvalidInput(
                        "attachment reuse token must be between 1 and 4096 characters".to_string(),
                    ));
                }
            }
        }
        if !attachment_ids.insert(attachment.id.as_str()) {
            return Err(Error::InvalidInput(
                "duplicate attachment id in one message".to_string(),
            ));
        }
    }
    Ok(())
}

pub(super) fn strip_internal_chat_attachment_metadata(attachments: &mut [NewStoredFile]) {
    for attachment in attachments {
        if let Some(metadata) = attachment.metadata.as_object_mut() {
            metadata.remove(FILE_UPLOAD_TOKEN_KEY);
            metadata.remove(FILE_OWNERSHIP_PROOF_KEY);
        }
    }
}

pub(super) fn chat_attachment_reuse_source_id(attachment: &ChatAttachment) -> String {
    format!(
        "{}:{}:{}:{}",
        attachment.room_id.as_i64(),
        attachment.message_id,
        attachment.message_created_at.timestamp_micros(),
        attachment.id
    )
}

pub(super) fn parse_chat_attachment_reuse_source_id(
    source_id: &str,
) -> Result<(RoomId, i64, i64, String)> {
    let mut parts = source_id.splitn(4, ':');
    let room_id = parts
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .map(RoomId::try_from)
        .transpose()
        .map_err(|_| Error::InvalidInput("invalid chat attachment reuse token".to_string()))?
        .ok_or_else(|| Error::InvalidInput("invalid chat attachment reuse token".to_string()))?;
    let message_id = parts
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| Error::InvalidInput("invalid chat attachment reuse token".to_string()))?;
    let message_created_at_micros = parts
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| Error::InvalidInput("invalid chat attachment reuse token".to_string()))?;
    let attachment_id = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Error::InvalidInput("invalid chat attachment reuse token".to_string()))?;
    Ok((
        room_id,
        message_id,
        message_created_at_micros,
        attachment_id,
    ))
}

pub(super) fn attach_chat_attachment_reuse_grants(
    storage: &dyn FileStorageService,
    viewer_user_id: UserId,
    attachments: &mut [ChatAttachment],
) -> Result<()> {
    for attachment in attachments {
        let expires_at = Utc::now() + Duration::seconds(CHAT_ATTACHMENT_REUSE_TOKEN_TTL_SECONDS);
        let storage_scope = chat_file_storage_scope(attachment.room_id, viewer_user_id);
        let grant = storage.create_reuse_grant(CreateFileReuseGrant {
            user_id: viewer_user_id,
            storage_scope: &storage_scope,
            source_kind: CHAT_ATTACHMENT_REUSE_SOURCE_KIND,
            source_id: &chat_attachment_reuse_source_id(attachment),
            expires_at,
        })?;
        attachment.reuse_token = Some(grant.token);
        attachment.reuse_expires_at = Some(grant.expires_at);
    }
    Ok(())
}

pub(super) fn chat_send_request_hash(request: &SendChatMessage) -> Result<String> {
    let payload = json!({
        "content": request.content,
        "message_type": request.message_type,
        "reply_to_message_id": request.reply_to_message_id,
        "metadata": request.metadata,
        "mentions": request.mentions,
        "attachments": request.attachments,
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

pub(super) fn chat_pin_request_hash(request: &PinChatMessage) -> Result<String> {
    let payload = json!({
        "room_id": request.room_id.as_i64(),
        "message_id": request.message_id,
        "note": request.note,
    });
    let bytes = serde_json::to_vec(&payload)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub(super) fn chat_unpin_request_hash(request: &UnpinChatMessage) -> Result<String> {
    let payload = json!({
        "room_id": request.room_id.as_i64(),
        "message_id": request.message_id,
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
