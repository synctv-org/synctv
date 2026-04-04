//! Notification models
//!
//! User notifications for room invitations, system announcements, and room events

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::id::UserId;
use crate::models::query::SortDirection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NotificationListSortBy {
    Title,
    UpdatedAt,
    #[default]
    CreatedAt,
}

impl NotificationListSortBy {
    #[must_use]
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::UpdatedAt => "updated_at",
            Self::CreatedAt => "created_at",
        }
    }
}

impl std::str::FromStr for NotificationListSortBy {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "title" => Ok(Self::Title),
            "updated_at" | "updatedat" => Ok(Self::UpdatedAt),
            "created_at" | "createdat" => Ok(Self::CreatedAt),
            other => Err(format!("Unknown notification list sort field: {other}")),
        }
    }
}

impl std::fmt::Display for NotificationListSortBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Title => "title",
            Self::UpdatedAt => "updated_at",
            Self::CreatedAt => "created_at",
        };
        f.write_str(value)
    }
}

/// Notification type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationType {
    /// Room invitation from another user
    RoomInvitation,
    /// System announcement from administrators
    SystemAnnouncement,
    /// Room event (e.g., user joined, media added)
    RoomEvent,
    /// Password reset notification
    PasswordReset,
    /// Email verification reminder
    EmailVerification,
}

impl std::fmt::Display for NotificationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RoomInvitation => write!(f, "room_invitation"),
            Self::SystemAnnouncement => write!(f, "system_announcement"),
            Self::RoomEvent => write!(f, "room_event"),
            Self::PasswordReset => write!(f, "password_reset"),
            Self::EmailVerification => write!(f, "email_verification"),
        }
    }
}

impl std::str::FromStr for NotificationType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "room_invitation" => Ok(Self::RoomInvitation),
            "system_announcement" => Ok(Self::SystemAnnouncement),
            "room_event" => Ok(Self::RoomEvent),
            "password_reset" => Ok(Self::PasswordReset),
            "email_verification" => Ok(Self::EmailVerification),
            _ => Err(anyhow::anyhow!("Invalid notification type: {s}")),
        }
    }
}

// Database mapping: NotificationType <-> VARCHAR
impl sqlx::Type<sqlx::Postgres> for NotificationType {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("varchar")
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        *ty == Self::type_info() || *ty == <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for NotificationType {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let s = self.to_string();
        <String as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&s, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for NotificationType {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        s.parse().map_err(|e: anyhow::Error| e.into())
    }
}

/// Notification model
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: UserId,
    #[sqlx(rename = "type")]
    pub notification_type: NotificationType,
    pub title: String,
    pub content: String,
    pub data: serde_json::Value,
    pub is_read: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Create notification request
#[derive(Debug, Deserialize)]
pub struct CreateNotificationRequest {
    pub user_id: UserId,
    pub notification_type: NotificationType,
    pub title: String,
    pub content: String,
    #[serde(default = "default_empty_data")]
    pub data: serde_json::Value,
}

fn default_empty_data() -> serde_json::Value {
    serde_json::json!({})
}

/// List notifications query parameters
#[derive(Debug, Default, Deserialize)]
pub struct NotificationListQuery {
    pub pagination: super::pagination::PageParams,
    pub is_read: Option<bool>,
    pub notification_type: Option<NotificationType>,
    pub search: Option<String>,
    #[serde(default)]
    pub sort_by: NotificationListSortBy,
    #[serde(default)]
    pub sort_direction: SortDirection,
}

/// Mark notification as read request
#[derive(Debug, Deserialize)]
pub struct MarkAsReadRequest {
    pub notification_ids: Vec<Uuid>,
}

/// Mark all notifications as read request
#[derive(Debug, Deserialize)]
pub struct MarkAllAsReadRequest {
    pub before: Option<chrono::DateTime<chrono::Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_type_display() {
        assert_eq!(
            NotificationType::RoomInvitation.to_string(),
            "room_invitation"
        );
        assert_eq!(
            NotificationType::SystemAnnouncement.to_string(),
            "system_announcement"
        );
        assert_eq!(NotificationType::RoomEvent.to_string(), "room_event");
        assert_eq!(
            NotificationType::PasswordReset.to_string(),
            "password_reset"
        );
        assert_eq!(
            NotificationType::EmailVerification.to_string(),
            "email_verification"
        );
    }

    #[test]
    fn test_notification_type_from_str() {
        assert_eq!(
            "room_invitation".parse::<NotificationType>().unwrap(),
            NotificationType::RoomInvitation
        );
        assert_eq!(
            "system_announcement".parse::<NotificationType>().unwrap(),
            NotificationType::SystemAnnouncement
        );
        assert_eq!(
            "room_event".parse::<NotificationType>().unwrap(),
            NotificationType::RoomEvent
        );
        assert_eq!(
            "password_reset".parse::<NotificationType>().unwrap(),
            NotificationType::PasswordReset
        );
        assert_eq!(
            "email_verification".parse::<NotificationType>().unwrap(),
            NotificationType::EmailVerification
        );
    }

    #[test]
    fn test_notification_type_from_str_invalid() {
        let result = "invalid_type".parse::<NotificationType>();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid notification type"));
    }

    #[test]
    fn test_notification_type_roundtrip() {
        let types = vec![
            NotificationType::RoomInvitation,
            NotificationType::SystemAnnouncement,
            NotificationType::RoomEvent,
            NotificationType::PasswordReset,
            NotificationType::EmailVerification,
        ];
        for nt in types {
            let s = nt.to_string();
            let parsed: NotificationType = s.parse().unwrap();
            assert_eq!(parsed, nt);
        }
    }

    #[test]
    fn test_notification_type_serde_roundtrip() {
        let nt = NotificationType::RoomInvitation;
        let json = serde_json::to_string(&nt).unwrap();
        assert_eq!(json, "\"room_invitation\"");
        let deserialized: NotificationType = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, nt);
    }

    #[test]
    fn test_create_notification_request_deserialize() {
        let json = serde_json::json!({
            "user_id": "user_123",
            "notification_type": "room_invitation",
            "title": "You have been invited",
            "content": "Join room ABC"
        });
        let req: CreateNotificationRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.user_id.as_str(), "user_123");
        assert_eq!(req.notification_type, NotificationType::RoomInvitation);
        assert_eq!(req.title, "You have been invited");
        assert_eq!(req.content, "Join room ABC");
        // data should default to empty object
        assert_eq!(req.data, serde_json::json!({}));
    }

    #[test]
    fn test_create_notification_request_with_data() {
        let json = serde_json::json!({
            "user_id": "user_456",
            "notification_type": "system_announcement",
            "title": "Maintenance",
            "content": "System will be down",
            "data": {"severity": "high", "eta_minutes": 30}
        });
        let req: CreateNotificationRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.data["severity"], "high");
        assert_eq!(req.data["eta_minutes"], 30);
    }

    #[test]
    fn test_mark_as_read_request_deserialize() {
        let json = serde_json::json!({
            "notification_ids": [
                "550e8400-e29b-41d4-a716-446655440000",
                "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
            ]
        });
        let req: MarkAsReadRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.notification_ids.len(), 2);
    }

    #[test]
    fn test_mark_all_as_read_request_no_before() {
        let json = serde_json::json!({});
        let req: MarkAllAsReadRequest = serde_json::from_value(json).unwrap();
        assert!(req.before.is_none());
    }

    #[test]
    fn test_mark_all_as_read_request_with_before() {
        let json = serde_json::json!({
            "before": "2025-01-01T00:00:00Z"
        });
        let req: MarkAllAsReadRequest = serde_json::from_value(json).unwrap();
        assert!(req.before.is_some());
    }
}
