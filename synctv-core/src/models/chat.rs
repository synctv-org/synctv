use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::id::{RoomId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum ChatMessageType {
    Text = 1,
    System = 2,
    Action = 3,
}

impl ChatMessageType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::System => "system",
            Self::Action => "action",
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
            other => Err(format!("Unknown chat message type: {other}")),
        }
    }
}

sqlx_i16_enum!(ChatMessageType, "Invalid chat message type", {
    Text = 1,
    System = 2,
    Action = 3,
});

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatMessage {
    pub id: i64,
    pub room_id: RoomId,
    /// The user who sent this message.
    ///
    /// `None` when the original author has been deleted (`ON DELETE SET NULL`).
    pub user_id: Option<UserId>,
    pub content: String,
    pub message_type: ChatMessageType,
    pub created_at: DateTime<Utc>,
}

impl ChatMessage {
    #[must_use]
    pub fn new(room_id: RoomId, user_id: UserId, content: String) -> Self {
        Self {
            id: 0,
            room_id,
            user_id: Some(user_id),
            content,
            message_type: ChatMessageType::Text,
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendChatRequest {
    pub room_id: RoomId,
    pub content: String,
}

/// Danmaku message (memory-only, not persisted)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DanmakuMessage {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub content: String,
    pub color: String, // hex color
    pub position: DanmakuPosition,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DanmakuPosition {
    Top = 0,
    Bottom = 1,
    Scroll = 2,
}

impl DanmakuPosition {
    /// Returns the string representation used by the notification service.
    ///
    /// These strings match the `RoomEvent::Danmaku.position` field convention.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Scroll => "scroll",
        }
    }
}

impl FromStr for DanmakuPosition {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "top" => Ok(Self::Top),
            "bottom" => Ok(Self::Bottom),
            "scroll" => Ok(Self::Scroll),
            other => Err(format!("Unknown danmaku position: {other}")),
        }
    }
}

impl std::fmt::Display for DanmakuPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl DanmakuMessage {
    #[must_use]
    pub fn new(
        room_id: RoomId,
        user_id: UserId,
        content: String,
        color: String,
        position: DanmakuPosition,
    ) -> Self {
        Self {
            room_id,
            user_id,
            content,
            color,
            position,
            timestamp: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendDanmakuRequest {
    pub room_id: RoomId,
    pub content: String,
    pub color: String,
    pub position: DanmakuPosition,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn danmaku_position_display_and_parse_roundtrip() {
        assert_eq!(DanmakuPosition::Top.to_string(), "top");
        assert_eq!(
            "bottom".parse::<DanmakuPosition>().unwrap(),
            DanmakuPosition::Bottom
        );
        assert_eq!(
            "SCROLL".parse::<DanmakuPosition>().unwrap(),
            DanmakuPosition::Scroll
        );
        assert!("middle".parse::<DanmakuPosition>().is_err());
    }

    #[test]
    fn chat_message_type_display_parse_and_code_roundtrip() {
        assert_eq!(ChatMessageType::Text.to_string(), "text");
        assert_eq!(
            " SYSTEM ".parse::<ChatMessageType>().unwrap(),
            ChatMessageType::System
        );
        assert_eq!(i16::from(ChatMessageType::Action), 3);
        assert_eq!(ChatMessageType::try_from(1).unwrap(), ChatMessageType::Text);
        assert!(ChatMessageType::try_from(99).is_err());
        assert!("notice".parse::<ChatMessageType>().is_err());
    }
}
