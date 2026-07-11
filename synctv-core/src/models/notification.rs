//! Notification models
//!
//! User notifications for room invitations, system announcements, and room events

use crate::models::{id::UserId, query::SortDirection};
use serde::{Deserialize, Serialize};

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum NotificationListSortBy {
        Title => { display: "title", sql: "title" },
        UpdatedAt => { display: "updated_at", sql: "updated_at" },
        CreatedAt => { display: "created_at", sql: "created_at" },
    }
    default = CreatedAt;
    error = "Unknown notification list sort field";
}

/// Notification type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum NotificationType {
    /// Room invitation from another user
    RoomInvitation = 1,
    /// System announcement from administrators
    SystemAnnouncement = 2,
    /// Room event (e.g., user joined, media added)
    RoomEvent = 3,
    /// Password reset notification
    PasswordReset = 4,
    /// Email bind notification
    EmailBind = 5,
}

impl NotificationType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RoomInvitation => "room_invitation",
            Self::SystemAnnouncement => "system_announcement",
            Self::RoomEvent => "room_event",
            Self::PasswordReset => "password_reset",
            Self::EmailBind => "email_bind",
        }
    }
}

impl std::fmt::Display for NotificationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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
            "email_bind" => Ok(Self::EmailBind),
            _ => Err(anyhow::anyhow!("Invalid notification type: {s}")),
        }
    }
}

i16_enum!(NotificationType, "Invalid notification type", {
    RoomInvitation = 1,
    SystemAnnouncement = 2,
    RoomEvent = 3,
    PasswordReset = 4,
    EmailBind = 5,
});

/// Notification model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: i64,
    pub user_id: UserId,
    pub notification_type: NotificationType,
    pub title: String,
    pub content: String,
    pub data: NotificationData,
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
    #[serde(default = "default_notification_data")]
    pub data: NotificationData,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationData {
    pub room_id: Option<String>,
    pub room_name: Option<String>,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub message_id: Option<String>,
    pub action_url: Option<String>,
}

pub(crate) fn default_notification_data() -> NotificationData {
    NotificationData::default()
}

/// Structured filters for listing notifications.
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
    pub notification_ids: Vec<i64>,
}

/// Mark all notifications as read request
#[derive(Debug, Deserialize)]
pub struct MarkAllAsReadRequest {
    pub before: Option<chrono::DateTime<chrono::Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_ok<T>(result: serde_json::Result<T>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn parse_notification_type(value: &str) -> NotificationType {
        match value.parse::<NotificationType>() {
            Ok(notification_type) => notification_type,
            Err(error) => std::panic::panic_any(format!("notification type should parse: {error}")),
        }
    }

    fn parse_notification_type_error(value: &str) -> anyhow::Error {
        match value.parse::<NotificationType>() {
            Ok(_) => std::panic::panic_any("notification type should fail to parse"),
            Err(error) => error,
        }
    }

    #[test]
    fn notification_type_string_contract_is_stable() {
        for (notification_type, value) in [
            (NotificationType::RoomInvitation, "room_invitation"),
            (NotificationType::SystemAnnouncement, "system_announcement"),
            (NotificationType::RoomEvent, "room_event"),
            (NotificationType::PasswordReset, "password_reset"),
            (NotificationType::EmailBind, "email_bind"),
        ] {
            assert_eq!(notification_type.to_string(), value);
            assert_eq!(parse_notification_type(value), notification_type);
        }

        let invalid = parse_notification_type_error("invalid_type");
        assert!(invalid.to_string().contains("Invalid notification type"));
    }

    #[test]
    fn notification_type_serde_roundtrip_uses_snake_case() {
        let notification_type = NotificationType::RoomInvitation;
        let json = json_ok(
            serde_json::to_string(&notification_type),
            "notification type should serialize",
        );
        assert_eq!(json, "\"room_invitation\"");
        assert_eq!(
            json_ok(
                serde_json::from_str::<NotificationType>(&json),
                "notification type should deserialize"
            ),
            notification_type
        );
    }

    #[test]
    fn create_notification_request_defaults_data_to_empty_object() {
        let json = serde_json::json!({
            "user_id": 123,
            "notification_type": "room_invitation",
            "title": "You have been invited",
            "content": "Join room ABC"
        });
        let req: CreateNotificationRequest = json_ok(
            serde_json::from_value(json),
            "notification request should deserialize",
        );
        assert_eq!(req.user_id.as_i64(), 123);
        assert_eq!(req.notification_type, NotificationType::RoomInvitation);
        assert_eq!(req.title, "You have been invited");
        assert_eq!(req.content, "Join room ABC");
        assert_eq!(req.data, NotificationData::default());
    }

    #[test]
    fn create_notification_request_preserves_data_payload() {
        let json = serde_json::json!({
            "user_id": 456,
            "notification_type": "system_announcement",
            "title": "Maintenance",
            "content": "System will be down",
            "data": {"roomId": "room_1", "actionUrl": "/rooms/room_1"}
        });
        let req: CreateNotificationRequest = json_ok(
            serde_json::from_value(json),
            "notification request should deserialize",
        );
        assert_eq!(req.data.room_id.as_deref(), Some("room_1"));
        assert_eq!(req.data.action_url.as_deref(), Some("/rooms/room_1"));
    }

    #[test]
    fn mark_as_read_request_accepts_notification_ids() {
        let json = serde_json::json!({
            "notification_ids": [
                5_508_400,
                6_978_100
            ]
        });
        let req: MarkAsReadRequest = json_ok(
            serde_json::from_value(json),
            "mark-as-read should deserialize",
        );
        assert_eq!(req.notification_ids.len(), 2);
    }

    #[test]
    fn mark_all_as_read_request_accepts_optional_cutoff() {
        let json = serde_json::json!({});
        let req: MarkAllAsReadRequest = json_ok(
            serde_json::from_value(json),
            "mark-all-as-read should deserialize",
        );
        assert!(req.before.is_none());

        let json = serde_json::json!({
            "before": "2025-01-01T00:00:00Z"
        });
        let req: MarkAllAsReadRequest = json_ok(
            serde_json::from_value(json),
            "mark-all-as-read cutoff should deserialize",
        );
        assert!(req.before.is_some());
    }
}
