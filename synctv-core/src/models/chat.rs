use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::str::FromStr;

use super::{
    file_storage::FileUploadManifestPart,
    id::{MediaId, PlaylistId, RoomId, UserId},
};

pub const CHAT_CLIENT_MESSAGE_ID_MAX_CHARS: usize = 128;
pub const CHAT_CLIENT_OPERATION_ID_MAX_CHARS: usize = 128;
pub const CHAT_EVENT_ID_MAX_CHARS: usize = 128;
pub const CHAT_EVENT_TYPE_MAX_CHARS: usize = 128;
pub const CHAT_ATTACHMENT_ID_MAX_CHARS: usize = 128;
pub const CHAT_ATTACHMENT_FILENAME_MAX_CHARS: usize = 255;
pub const CHAT_REACTION_KEY_MAX_CHARS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum ChatMessageType {
    Text = 1,
    System = 2,
    Action = 3,
    Attachment = 4,
}

impl ChatMessageType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::System => "system",
            Self::Action => "action",
            Self::Attachment => "attachment",
        }
    }
}

impl std::fmt::Display for ChatMessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ChatMessageType {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "system" => Ok(Self::System),
            "action" => Ok(Self::Action),
            "attachment" => Ok(Self::Attachment),
            other => Err(format!("Unknown chat message type: {other}")),
        }
    }
}

sqlx_i16_enum!(ChatMessageType, "Invalid chat message type", {
    Text = 1,
    System = 2,
    Action = 3,
    Attachment = 4,
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum ChatAttachmentKind {
    File = 1,
    Image = 2,
}

impl ChatAttachmentKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Image => "image",
        }
    }

    #[must_use]
    pub fn from_mime_type(mime_type: &str) -> Self {
        if mime_type.trim().to_ascii_lowercase().starts_with("image/") {
            Self::Image
        } else {
            Self::File
        }
    }
}

impl std::fmt::Display for ChatAttachmentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

sqlx_i16_enum!(ChatAttachmentKind, "Invalid chat attachment kind", {
    File = 1,
    Image = 2,
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum ChatMessageStatus {
    Active = 1,
    Edited = 2,
    Deleted = 3,
}

impl ChatMessageStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Edited => "edited",
            Self::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for ChatMessageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ChatMessageStatus {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "edited" => Ok(Self::Edited),
            "deleted" => Ok(Self::Deleted),
            other => Err(format!("Unknown chat message status: {other}")),
        }
    }
}

sqlx_i16_enum!(ChatMessageStatus, "Invalid chat message status", {
    Active = 1,
    Edited = 2,
    Deleted = 3,
});

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatMessage {
    pub id: i64,
    pub room_id: RoomId,
    /// The user who sent this message.
    ///
    /// `None` when the original author has been deleted (`ON DELETE SET NULL`).
    pub user_id: Option<UserId>,
    pub client_message_id: Option<String>,
    pub content: String,
    pub message_type: ChatMessageType,
    pub status: ChatMessageStatus,
    pub version: i64,
    pub reply_to_message_id: Option<i64>,
    pub reply_to_message_created_at: Option<DateTime<Utc>>,
    pub metadata: JsonValue,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub deleted_by: Option<UserId>,
    pub delete_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ChatMessage {
    #[must_use]
    pub fn new(room_id: RoomId, user_id: UserId, content: String) -> Self {
        Self {
            id: 0,
            room_id,
            user_id: Some(user_id),
            client_message_id: None,
            content,
            message_type: ChatMessageType::Text,
            status: ChatMessageStatus::Active,
            version: 1,
            reply_to_message_id: None,
            reply_to_message_created_at: None,
            metadata: JsonValue::Object(Default::default()),
            edited_at: None,
            deleted_at: None,
            deleted_by: None,
            delete_reason: None,
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendChatRequest {
    pub room_id: RoomId,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatAttachment {
    pub id: String,
    pub kind: ChatAttachmentKind,
    pub room_id: RoomId,
    pub message_id: i64,
    pub message_created_at: DateTime<Utc>,
    pub filename: Option<String>,
    pub storage_backend: String,
    pub object_key: String,
    pub url: Option<String>,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
    #[sqlx(skip)]
    #[serde(default)]
    pub reuse_token: Option<String>,
    #[sqlx(skip)]
    #[serde(default)]
    pub reuse_expires_at: Option<DateTime<Utc>>,
}

impl ChatAttachment {
    #[must_use]
    pub fn file_reference_target(&self) -> super::file_storage::FileReferenceTarget {
        super::file_storage::FileReferenceTarget {
            storage_backend: self.storage_backend.clone(),
            object_key: self.object_key.clone(),
            reference_kind: "chat_message_attachment".to_string(),
            reference_id: format!(
                "{}:{}:{}:{}",
                self.room_id.as_i64(),
                self.message_id,
                self.message_created_at.timestamp_micros(),
                self.id
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateChatAttachmentUploadSession {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub client_attachment_id: Option<String>,
    pub filename: Option<String>,
    pub mime_type: String,
    pub size_bytes: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub parts: Vec<FileUploadManifestPart>,
    pub metadata: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatAttachmentUploadSession {
    pub attachment: super::file_storage::NewStoredFile,
    pub upload_required: bool,
    pub ownership_proof_required: bool,
    pub ownership_proof_nonce: Option<String>,
    pub ownership_proof_ranges: Vec<super::file_storage::FileOwnershipProofRange>,
    pub upload_url: Option<String>,
    pub upload_method: Option<String>,
    pub upload_headers: BTreeMap<String, String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_size_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageWithAttachments {
    pub message: ChatMessage,
    pub attachments: Vec<ChatAttachment>,
    pub reactions: Vec<ChatReactionSummary>,
    pub mentions: Vec<ChatMention>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageContext {
    pub before: Vec<ChatMessageWithAttachments>,
    pub anchor: ChatMessageWithAttachments,
    pub after: Vec<ChatMessageWithAttachments>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendChatMessage {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub client_message_id: Option<String>,
    pub content: String,
    pub message_type: ChatMessageType,
    pub reply_to_message_id: Option<i64>,
    pub metadata: JsonValue,
    pub attachments: Vec<super::file_storage::SubmittedFileReference>,
    pub mentions: Vec<ChatMentionInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMentionInput {
    pub user_id: UserId,
    pub start: i32,
    pub length: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatMention {
    pub room_id: RoomId,
    pub message_id: i64,
    pub message_created_at: DateTime<Utc>,
    pub mentioned_user_id: UserId,
    pub username: Option<String>,
    pub start: i32,
    pub length: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditChatMessage {
    pub room_id: RoomId,
    pub message_id: i64,
    pub user_id: UserId,
    pub client_operation_id: Option<String>,
    pub content: String,
    pub metadata: JsonValue,
    pub expected_version: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteChatMessage {
    pub room_id: RoomId,
    pub message_id: i64,
    pub user_id: UserId,
    pub client_operation_id: Option<String>,
    pub reason: Option<String>,
    pub expected_version: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatReaction {
    pub room_id: RoomId,
    pub message_id: i64,
    pub message_created_at: DateTime<Utc>,
    pub user_id: UserId,
    pub reaction_key: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatReactionSummary {
    pub key: String,
    pub count: i64,
    pub reacted_by_me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatReactionUser {
    pub user_id: UserId,
    pub reaction_key: String,
    pub reacted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ChatReactionUsersCursor {
    pub reacted_at: DateTime<Utc>,
    pub user_id: UserId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatReactionUsersPage {
    pub users: Vec<ChatReactionUser>,
    pub next_cursor: Option<ChatReactionUsersCursor>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetChatReaction {
    pub room_id: RoomId,
    pub message_id: i64,
    pub user_id: UserId,
    pub reaction_key: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
pub enum ChatEventKind {
    Created = 1,
    Edited = 2,
    Deleted = 3,
    ReactionsChanged = 4,
}

sqlx_i16_enum!(ChatEventKind, "Invalid chat event kind", {
    Created = 1,
    Edited = 2,
    Deleted = 3,
    ReactionsChanged = 4,
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageEvent {
    pub event_id: String,
    #[serde(default)]
    pub sequence: i64,
    pub room_id: RoomId,
    pub actor_user_id: UserId,
    pub kind: ChatEventKind,
    pub message: ChatMessageWithAttachments,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageEventLog {
    pub sequence: i64,
    pub event: ChatMessageEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventCursor {
    pub event_id: Option<String>,
    pub sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatReadState {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub last_read_message_id: Option<i64>,
    pub last_read_message_created_at: Option<DateTime<Utc>>,
    pub last_read_event_id: Option<String>,
    pub last_read_event_sequence: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatReadStateWithUnread {
    pub state: ChatReadState,
    pub unread_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageReadReceiptUser {
    pub user: crate::models::User,
    pub read_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageReadReceiptMember {
    pub user: crate::models::User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageReadReceiptsPage {
    pub readers: Vec<ChatMessageReadReceiptUser>,
    pub unread_members: Vec<ChatMessageReadReceiptMember>,
    pub reader_total: i64,
    pub unread_total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkChatRead {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub message_id: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHistoryCursor {
    pub created_at: DateTime<Utc>,
    pub id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatHistoryPage {
    pub messages: Vec<ChatMessageWithAttachments>,
    pub next_cursor: Option<ChatHistoryCursor>,
    pub event_cursor: EventCursor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatPlaybackMessagesQuery {
    pub room_id: RoomId,
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Option<Vec<u8>>,
    pub position_seconds: f64,
    pub before_seconds: f64,
    pub after_seconds: f64,
    pub limit: i32,
    pub include_deleted: bool,
}

impl ChatPlaybackMessagesQuery {
    #[must_use]
    pub fn normalize(mut self) -> Self {
        if self.target.as_ref().is_some_and(Vec::is_empty) {
            self.target = None;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn chat_message_type_display_parse_and_code_roundtrip() {
        assert_eq!(ChatMessageType::Text.to_string(), "text");
        assert_eq!(
            ok(
                " SYSTEM ".parse::<ChatMessageType>(),
                "system type should parse"
            ),
            ChatMessageType::System
        );
        assert_eq!(i16::from(ChatMessageType::Action), 3);
        assert_eq!(
            ok(
                ChatMessageType::try_from(1),
                "chat message type code should parse"
            ),
            ChatMessageType::Text
        );
        assert!(ChatMessageType::try_from(99).is_err());
        assert!("notice".parse::<ChatMessageType>().is_err());
    }
}
