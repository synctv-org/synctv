use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::{
    file_storage::FileUploadManifestPart,
    id::{MediaId, PlaylistId, RoomId, UserId},
    ProviderTarget, RoomRole,
};

pub const CHAT_CLIENT_MESSAGE_ID_MAX_CHARS: usize = 128;
pub const CHAT_CLIENT_OPERATION_ID_MAX_CHARS: usize = 128;
pub const CHAT_EVENT_ID_MAX_CHARS: usize = 128;
pub const CHAT_EVENT_TYPE_MAX_CHARS: usize = 128;
pub const CHAT_ATTACHMENT_ID_MAX_CHARS: usize = 128;
pub const CHAT_ATTACHMENT_FILENAME_MAX_CHARS: usize = 255;
pub const CHAT_REACTION_KEY_MAX_CHARS: usize = 64;
pub const CHAT_PIN_NOTE_MAX_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum ChatMessageType {
    User = 1,
    SystemMemberJoined = 1001,
    SystemPlaybackChanged = 1002,
}

impl ChatMessageType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::SystemMemberJoined => "system_member_joined",
            Self::SystemPlaybackChanged => "system_playback_changed",
        }
    }

    #[must_use]
    pub const fn is_system(self) -> bool {
        matches!(self, Self::SystemMemberJoined | Self::SystemPlaybackChanged)
    }

    #[must_use]
    pub fn default_visible_types() -> Vec<Self> {
        vec![Self::User]
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
            "user" => Ok(Self::User),
            "system_member_joined" => Ok(Self::SystemMemberJoined),
            "system_playback_changed" => Ok(Self::SystemPlaybackChanged),
            other => Err(format!("Unknown chat message type: {other}")),
        }
    }
}

i16_enum!(ChatMessageType, "Invalid chat message type", {
    User = 1,
    SystemMemberJoined = 1001,
    SystemPlaybackChanged = 1002,
});

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChatUserMetadata {
    pub presentation: Option<ChatPresentationMetadata>,
    pub playback: Option<ChatPlaybackMetadata>,
}

impl ChatUserMetadata {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.presentation.is_none() && self.playback.is_none()
    }

    #[must_use]
    pub fn normalized_for_storage(&self) -> crate::Result<Self> {
        let mut metadata = self.clone();
        if let Some(playback) = metadata.playback.as_mut() {
            playback.normalize_target_hash()?;
        }
        Ok(metadata)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMemberJoinedMetadata {
    pub user_id: UserId,
    pub username: String,
    pub actor_user_id: Option<UserId>,
    pub actor_username: Option<String>,
    pub role: RoomRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackChangeReason {
    Selected,
    Next,
    Previous,
    HistoryEntry,
    AutoAdvance,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPlaybackChangedMetadata {
    pub from: Option<ChatPlaybackMetadata>,
    pub to: ChatPlaybackMetadata,
    pub reason: PlaybackChangeReason,
    pub actor_user_id: Option<UserId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum ChatAttachmentKind {
    File = 1,
    Image = 2,
    Audio = 3,
}

impl ChatAttachmentKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Image => "image",
            Self::Audio => "audio",
        }
    }

    #[must_use]
    pub fn from_mime_type(mime_type: &str) -> Self {
        let mime_type = mime_type.trim().to_ascii_lowercase();
        if mime_type.starts_with("image/") {
            Self::Image
        } else if mime_type.starts_with("audio/") {
            Self::Audio
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

i16_enum!(ChatAttachmentKind, "Invalid chat attachment kind", {
    File = 1,
    Image = 2,
    Audio = 3,
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

i16_enum!(ChatMessageStatus, "Invalid chat message status", {
    Active = 1,
    Edited = 2,
    Deleted = 3,
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChatMetadata {
    User(ChatUserMetadata),
    MemberJoined(ChatMemberJoinedMetadata),
    PlaybackChanged(ChatPlaybackChangedMetadata),
}

impl ChatMetadata {
    #[must_use]
    pub fn normalized_for_optional_storage(metadata: Option<&Self>) -> crate::Result<Option<Self>> {
        metadata
            .filter(|metadata| !metadata.is_empty())
            .map(Self::normalized_for_storage)
            .transpose()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::User(metadata) if metadata.is_empty())
    }

    #[must_use]
    pub fn normalized_for_storage(&self) -> crate::Result<Self> {
        match self {
            Self::User(metadata) => Ok(Self::User(metadata.normalized_for_storage()?)),
            Self::MemberJoined(metadata) => Ok(Self::MemberJoined(metadata.clone())),
            Self::PlaybackChanged(metadata) => Ok(Self::PlaybackChanged(metadata.clone())),
        }
    }

    #[must_use]
    pub const fn message_type(&self) -> ChatMessageType {
        match self {
            Self::User(_) => ChatMessageType::User,
            Self::MemberJoined(_) => ChatMessageType::SystemMemberJoined,
            Self::PlaybackChanged(_) => ChatMessageType::SystemPlaybackChanged,
        }
    }

    #[must_use]
    pub const fn user(&self) -> Option<&ChatUserMetadata> {
        match self {
            Self::User(metadata) => Some(metadata),
            Self::MemberJoined(_) | Self::PlaybackChanged(_) => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChatPresentationMetadata {
    pub display_position: Option<String>,
    pub display_color: Option<String>,
}

impl ChatPresentationMetadata {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.display_position.is_none() && self.display_color.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPlaybackMetadata {
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    #[serde(default)]
    pub target: Option<ProviderTarget>,
    #[serde(default)]
    pub target_hash: Option<String>,
    #[serde(default)]
    pub position_seconds: Option<f64>,
    #[serde(default)]
    pub media_name: Option<String>,
    #[serde(default)]
    pub playlist_name: Option<String>,
}

impl ChatPlaybackMetadata {
    #[must_use]
    pub fn position_for_source(
        position_seconds: f64,
        is_live: Option<bool>,
        duration_seconds: Option<f64>,
    ) -> Option<f64> {
        if is_live == Some(true) || !position_seconds.is_finite() {
            return None;
        }

        let position_seconds = position_seconds.max(0.0);
        let duration_seconds =
            duration_seconds.filter(|duration| duration.is_finite() && *duration > 0.0);
        Some(duration_seconds.map_or(position_seconds, |duration| position_seconds.min(duration)))
    }

    pub fn normalize_target_hash(&mut self) -> crate::Result<()> {
        self.target_hash = self
            .target
            .as_ref()
            .map(|target| crate::models::try_hash_playback_target(Some(target)))
            .transpose()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    pub metadata: Option<ChatMetadata>,
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
            message_type: ChatMessageType::User,
            status: ChatMessageStatus::Active,
            version: 1,
            reply_to_message_id: None,
            reply_to_message_created_at: None,
            metadata: None,
            edited_at: None,
            deleted_at: None,
            deleted_by: None,
            delete_reason: None,
            created_at: crate::SystemClock.now(),
        }
    }

    #[must_use]
    pub const fn is_system(&self) -> bool {
        self.message_type.is_system()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageSelection {
    pub include_message_types: Vec<ChatMessageType>,
}

impl ChatMessageSelection {
    #[must_use]
    pub fn user_default() -> Self {
        Self {
            include_message_types: ChatMessageType::default_visible_types(),
        }
    }

    #[must_use]
    pub fn message_type_codes(&self) -> Vec<i16> {
        self.effective_message_types()
            .iter()
            .copied()
            .map(i16::from)
            .collect()
    }

    #[must_use]
    pub fn message_type_strings(&self) -> Vec<String> {
        self.effective_message_types()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[must_use]
    pub fn includes(&self, message_type: ChatMessageType) -> bool {
        self.effective_message_types().contains(&message_type)
    }

    #[must_use]
    fn effective_message_types(&self) -> Vec<ChatMessageType> {
        if self.include_message_types.is_empty() {
            ChatMessageType::default_visible_types()
        } else {
            self.include_message_types.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub id: String,
    pub kind: ChatAttachmentKind,
    pub room_id: RoomId,
    pub message_id: i64,
    pub message_created_at: DateTime<Utc>,
    pub filename: Option<String>,
    pub storage_backend: String,
    pub object_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_access: Option<super::file_storage::FileObjectAccess>,
    pub url: Option<String>,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub metadata: super::file_storage::FileMetadata,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub reuse_token: Option<String>,
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
    pub duration_seconds: Option<i32>,
    pub bitrate_bps: Option<i32>,
    pub parts: Vec<FileUploadManifestPart>,
    pub metadata: super::file_storage::FileMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageWithAttachments {
    pub message: ChatMessage,
    pub attachments: Vec<ChatAttachment>,
    pub reactions: Vec<ChatReactionSummary>,
    pub mentions: Vec<ChatMention>,
    pub pin: Option<ChatMessagePin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessagePin {
    pub room_id: RoomId,
    pub message_id: i64,
    pub message_created_at: DateTime<Utc>,
    pub pinned_by: Option<UserId>,
    pub pinned_by_username: Option<String>,
    pub note: Option<String>,
    pub pinned_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatPinnedMessage {
    pub pin: ChatMessagePin,
    pub message: ChatMessageWithAttachments,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinChatMessage {
    pub room_id: RoomId,
    pub message_id: i64,
    pub user_id: UserId,
    pub client_operation_id: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnpinChatMessage {
    pub room_id: RoomId,
    pub message_id: i64,
    pub user_id: UserId,
    pub client_operation_id: Option<String>,
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
    pub metadata: Option<ChatMetadata>,
    pub attachments: Vec<super::file_storage::SubmittedFileReference>,
    pub mentions: Vec<ChatMentionInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMentionInput {
    pub user_id: UserId,
    pub start: i32,
    pub length: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    pub metadata: Option<ChatMetadata>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReactionSummary {
    pub key: String,
    pub count: i64,
    pub reacted_by_me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

i16_enum!(ChatEventKind, "Invalid chat event kind", {
    Created = 1,
    Edited = 2,
    Deleted = 3,
    ReactionsChanged = 4,
});

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct ChatMessageEventLog {
    pub sequence: i64,
    pub event: ChatMessageEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
pub enum ChatMessageOperationKind {
    Edit = 1,
    Delete = 2,
    Pin = 3,
    Unpin = 4,
}

impl From<ChatMessageOperationKind> for i16 {
    fn from(value: ChatMessageOperationKind) -> Self {
        value as Self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
pub enum ChatPinEventKind {
    Pinned = 1,
    Unpinned = 2,
    MessageUpdated = 3,
    MessageDeleted = 4,
}

impl ChatPinEventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pinned => "chat_pin_pinned",
            Self::Unpinned => "chat_pin_unpinned",
            Self::MessageUpdated => "chat_pin_message_updated",
            Self::MessageDeleted => "chat_pin_message_deleted",
        }
    }
}

impl From<ChatPinEventKind> for i16 {
    fn from(value: ChatPinEventKind) -> Self {
        value as Self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPinEvent {
    pub event_id: String,
    #[serde(default)]
    pub sequence: i64,
    pub room_id: RoomId,
    pub actor_user_id: UserId,
    pub kind: ChatPinEventKind,
    pub message: ChatMessageWithAttachments,
    pub pin: Option<ChatMessagePin>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPinEventLog {
    pub sequence: i64,
    pub event: ChatPinEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventCursor {
    pub event_id: Option<String>,
    pub sequence: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct ChatSearchMessagesQuery {
    pub room_id: RoomId,
    pub query: String,
    pub cursor: Option<ChatHistoryCursor>,
    pub limit: i32,
    pub include_deleted: bool,
    pub user_id: Option<UserId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSearchMessagesPage {
    pub messages: Vec<ChatMessageWithAttachments>,
    pub next_cursor: Option<ChatHistoryCursor>,
    pub event_cursor: EventCursor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatPlaybackMessagesQuery {
    pub room_id: RoomId,
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Option<super::ProviderTarget>,
    pub selection: ChatMessageSelection,
    pub position_seconds: f64,
    pub before_seconds: f64,
    pub after_seconds: f64,
    pub limit: i32,
    pub include_deleted: bool,
}

impl ChatPlaybackMessagesQuery {
    #[must_use]
    pub fn normalize(self) -> Self {
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
        assert_eq!(ChatMessageType::User.to_string(), "user");
        assert_eq!(
            ok(
                " SYSTEM_MEMBER_JOINED ".parse::<ChatMessageType>(),
                "system member joined type should parse"
            ),
            ChatMessageType::SystemMemberJoined
        );
        assert_eq!(i16::from(ChatMessageType::User), 1);
        assert_eq!(
            ok(
                ChatMessageType::try_from(1),
                "chat message type code should parse"
            ),
            ChatMessageType::User
        );
        assert!(ChatMessageType::try_from(99).is_err());
        assert!("notice".parse::<ChatMessageType>().is_err());
    }

    #[test]
    fn chat_metadata_normalized_for_storage_derives_target_hash() {
        let target = ProviderTarget::alist("/media/episode-1.mp4".to_string());
        let metadata = ChatMetadata::User(ChatUserMetadata {
            playback: Some(ChatPlaybackMetadata {
                media_id: None,
                playlist_id: None,
                target: Some(target.clone()),
                target_hash: None,
                position_seconds: Some(12.5),
                media_name: None,
                playlist_name: None,
            }),
            ..Default::default()
        });

        let normalized = metadata
            .normalized_for_storage()
            .expect("target hash should compute");
        let ChatMetadata::User(normalized) = normalized else {
            panic!("normalized metadata should remain user metadata");
        };
        assert_eq!(
            normalized
                .playback
                .and_then(|playback| playback.target_hash),
            Some(
                crate::models::try_hash_playback_target(Some(&target))
                    .expect("target hash should compute")
            )
        );
    }

    #[test]
    fn chat_metadata_ignores_unknown_persisted_fields() {
        let metadata: ChatMetadata = serde_json::from_value(serde_json::json!({
            "type": "user",
            "futureUserField": true,
            "playback": {
                "target": {
                    "provider": "alist",
                    "relativePath": "/media/episode-1.mp4",
                    "futureTargetField": "ignored"
                },
                "targetHash": "stored-hash",
                "positionSeconds": 12.5,
                "futurePlaybackField": {"version": 2}
            }
        }))
        .expect("chat metadata should ignore unknown persisted fields");

        let ChatMetadata::User(metadata) = metadata else {
            panic!("expected user chat metadata");
        };
        let playback = metadata
            .playback
            .expect("playback metadata should be retained");
        assert_eq!(playback.target_hash.as_deref(), Some("stored-hash"));
        assert_eq!(playback.position_seconds, Some(12.5));
        assert_eq!(
            playback.target,
            Some(ProviderTarget::alist("/media/episode-1.mp4".to_string()))
        );
    }

    #[test]
    fn chat_playback_position_is_clamped_to_known_duration() {
        assert_eq!(
            ChatPlaybackMetadata::position_for_source(130.0, Some(false), Some(120.0)),
            Some(120.0)
        );
        assert_eq!(
            ChatPlaybackMetadata::position_for_source(100.0, Some(false), Some(120.0)),
            Some(100.0)
        );
    }

    #[test]
    fn chat_playback_position_uses_clock_when_duration_is_unknown() {
        assert_eq!(
            ChatPlaybackMetadata::position_for_source(130.0, Some(false), None),
            Some(130.0)
        );
        assert_eq!(
            ChatPlaybackMetadata::position_for_source(130.0, None, Some(f64::NAN)),
            Some(130.0)
        );
    }

    #[test]
    fn chat_playback_position_is_absent_for_live_sources() {
        assert_eq!(
            ChatPlaybackMetadata::position_for_source(130.0, Some(true), Some(120.0)),
            None
        );
    }
}
