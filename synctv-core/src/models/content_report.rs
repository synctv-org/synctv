use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{
    postgres::{PgArgumentBuffer, PgTypeInfo, PgValueRef},
    Decode, Encode, Postgres, Type,
};
use std::str::FromStr;

use super::{ContentReportId, RoomId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum ContentReportTargetType {
    Room = 1,
    User = 2,
    RoomMember = 3,
    ChatMessage = 4,
}

impl ContentReportTargetType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Room => "room",
            Self::User => "user",
            Self::RoomMember => "room_member",
            Self::ChatMessage => "chat_message",
        }
    }
}

impl FromStr for ContentReportTargetType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "room" => Ok(Self::Room),
            "user" => Ok(Self::User),
            "room_member" | "member" => Ok(Self::RoomMember),
            "chat_message" | "message" => Ok(Self::ChatMessage),
            other => Err(format!("Unknown report target type: {other}")),
        }
    }
}

sqlx_i16_enum!(
    ContentReportTargetType,
    "Invalid ContentReportTargetType value",
    {
        Room = 1,
        User = 2,
        RoomMember = 3,
        ChatMessage = 4,
    }
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum ContentReportStatus {
    #[default]
    Open = 1,
    Reviewing = 2,
    Resolved = 3,
    Dismissed = 4,
}

impl ContentReportStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Reviewing => "reviewing",
            Self::Resolved => "resolved",
            Self::Dismissed => "dismissed",
        }
    }
}

sqlx_i16_enum!(ContentReportStatus, "Invalid ContentReportStatus value", {
    Open = 1,
    Reviewing = 2,
    Resolved = 3,
    Dismissed = 4,
});

#[derive(Debug, Clone)]
pub enum ContentReportTarget {
    Room { room_id: RoomId },
    User { user_id: UserId },
    RoomMember { room_id: RoomId, user_id: UserId },
    ChatMessage { room_id: RoomId, message_id: i64 },
}

impl ContentReportTarget {
    #[must_use]
    pub const fn target_type(&self) -> ContentReportTargetType {
        match self {
            Self::Room { .. } => ContentReportTargetType::Room,
            Self::User { .. } => ContentReportTargetType::User,
            Self::RoomMember { .. } => ContentReportTargetType::RoomMember,
            Self::ChatMessage { .. } => ContentReportTargetType::ChatMessage,
        }
    }

    #[must_use]
    pub const fn room_context(&self) -> Option<RoomId> {
        match self {
            Self::Room { room_id }
            | Self::RoomMember { room_id, .. }
            | Self::ChatMessage { room_id, .. } => Some(*room_id),
            Self::User { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreateContentReport {
    pub reporter_user_id: UserId,
    pub target: ContentReportTarget,
    pub reason_code: String,
    pub reason: String,
    pub metadata: ContentReportMetadata,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentReportMetadata {
    pub client_reason: Option<String>,
}

impl ContentReportMetadata {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.client_reason.is_none()
    }
}

impl Type<Postgres> for ContentReportMetadata {
    fn type_info() -> PgTypeInfo {
        <sqlx::types::Json<ContentReportMetadata> as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <sqlx::types::Json<ContentReportMetadata> as Type<Postgres>>::compatible(ty)
    }
}

impl Encode<'_, Postgres> for ContentReportMetadata {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        sqlx::types::Json(self).encode_by_ref(buf)
    }
}

impl<'r> Decode<'r, Postgres> for ContentReportMetadata {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let sqlx::types::Json(metadata) =
            <sqlx::types::Json<Self> as Decode<Postgres>>::decode(value)?;
        Ok(metadata)
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ContentReport {
    pub id: ContentReportId,
    pub reporter_user_id: UserId,
    pub room_id: Option<RoomId>,
    pub target_type: ContentReportTargetType,
    pub target_room_id: Option<RoomId>,
    pub target_user_id: Option<UserId>,
    pub target_member_room_id: Option<RoomId>,
    pub target_member_user_id: Option<UserId>,
    pub target_chat_message_id: Option<i64>,
    pub target_chat_message_created_at: Option<DateTime<Utc>>,
    pub reason_code: String,
    pub reason: String,
    pub metadata: ContentReportMetadata,
    pub status: ContentReportStatus,
    pub reviewed_by: Option<UserId>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub resolution_note: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ContentReportAdminRow {
    pub id: ContentReportId,
    pub reporter_user_id: UserId,
    pub reporter_username: String,
    pub room_id: Option<RoomId>,
    pub room_name: String,
    pub target_type: ContentReportTargetType,
    pub target_room_id: Option<RoomId>,
    pub target_room_name: String,
    pub target_user_id: Option<UserId>,
    pub target_username: String,
    pub target_member_room_id: Option<RoomId>,
    pub target_member_room_name: String,
    pub target_member_user_id: Option<UserId>,
    pub target_member_username: String,
    pub target_chat_message_id: Option<i64>,
    pub target_chat_message_created_at: Option<DateTime<Utc>>,
    pub target_chat_message_preview: String,
    pub reason_code: String,
    pub reason: String,
    pub metadata: ContentReportMetadata,
    pub status: ContentReportStatus,
    pub reviewed_by: Option<UserId>,
    pub reviewed_by_username: String,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub resolution_note: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
