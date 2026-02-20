//! Shared Notification Implementation
//!
//! Used by both HTTP and gRPC handlers to avoid duplicating notification logic.

use std::sync::Arc;
use synctv_core::models::id::UserId;
use synctv_core::models::notification::{
    MarkAllAsReadRequest, MarkAsReadRequest, Notification, NotificationListQuery,
    NotificationType as CoreNotificationType,
};
use synctv_core::service::UserNotificationService;
use uuid::Uuid;

use crate::impls::ApiError;
use crate::proto::client::{
    NotificationProto, NotificationType as ProtoNotificationType,
};

/// Convert a domain Notification to a proto `NotificationProto`.
///
/// Shared by both HTTP and gRPC handlers.
#[must_use] 
pub fn notification_to_proto(n: Notification) -> NotificationProto {
    let notification_type = match n.notification_type {
        CoreNotificationType::RoomInvitation => ProtoNotificationType::RoomInvitation,
        CoreNotificationType::SystemAnnouncement => ProtoNotificationType::SystemAnnouncement,
        CoreNotificationType::RoomEvent => ProtoNotificationType::RoomEvent,
        CoreNotificationType::PasswordReset => ProtoNotificationType::PasswordReset,
        CoreNotificationType::EmailVerification => ProtoNotificationType::EmailVerification,
    };

    NotificationProto {
        id: n.id.to_string(),
        user_id: n.user_id.as_str().to_string(),
        notification_type: notification_type as i32,
        title: n.title,
        content: n.content,
        data: serde_json::to_vec(&n.data).unwrap_or_default(),
        is_read: n.is_read,
        created_at: n.created_at.timestamp(),
        updated_at: n.updated_at.timestamp(),
    }
}

/// Convert a proto `NotificationType` enum value to a domain `NotificationType`.
///
/// Shared by both HTTP and gRPC handlers.
#[must_use] 
pub fn proto_notification_type_to_core(value: i32) -> Option<CoreNotificationType> {
    match ProtoNotificationType::try_from(value) {
        Ok(ProtoNotificationType::RoomInvitation) => Some(CoreNotificationType::RoomInvitation),
        Ok(ProtoNotificationType::SystemAnnouncement) => Some(CoreNotificationType::SystemAnnouncement),
        Ok(ProtoNotificationType::RoomEvent) => Some(CoreNotificationType::RoomEvent),
        Ok(ProtoNotificationType::PasswordReset) => Some(CoreNotificationType::PasswordReset),
        Ok(ProtoNotificationType::EmailVerification) => Some(CoreNotificationType::EmailVerification),
        _ => None,
    }
}

/// Shared notification operations implementation.
pub struct NotificationApiImpl {
    notification_service: Arc<UserNotificationService>,
}

/// Result of listing notifications
pub struct ListNotificationsResult {
    pub notifications: Vec<Notification>,
    pub total: i64,
    pub unread_count: i64,
}

impl NotificationApiImpl {
    #[must_use]
    pub const fn new(notification_service: Arc<UserNotificationService>) -> Self {
        Self {
            notification_service,
        }
    }

    /// List notifications for a user with pagination and filters.
    pub async fn list_notifications(
        &self,
        user_id: &UserId,
        page: Option<i32>,
        page_size: Option<i32>,
        is_read: Option<bool>,
        notification_type: Option<CoreNotificationType>,
    ) -> Result<ListNotificationsResult, ApiError> {
        let query = NotificationListQuery {
            pagination: synctv_core::models::PageParams::new(
                page.map(|p| p as u32),
                page_size.map(|s| s as u32),
            ),
            is_read,
            notification_type,
        };

        let (notifications, total) = self
            .notification_service
            .list(user_id, query)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to list notifications: {e}")))?;

        let unread_count = self
            .notification_service
            .get_unread_count(user_id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to get unread count: {e}")))?;

        Ok(ListNotificationsResult {
            notifications,
            total,
            unread_count,
        })
    }

    /// Get a single notification by ID.
    pub async fn get_notification(
        &self,
        user_id: &UserId,
        notification_id: Uuid,
    ) -> Result<Notification, ApiError> {
        self.notification_service
            .get(user_id, notification_id)
            .await
            .map_err(|e| match e {
                synctv_core::Error::NotFound(_) => {
                    ApiError::NotFound("Notification not found".to_string())
                }
                other => ApiError::Internal(format!("Failed to get notification: {other}")),
            })
    }

    /// Mark specific notifications as read.
    pub async fn mark_as_read(
        &self,
        user_id: &UserId,
        notification_ids: Vec<Uuid>,
    ) -> Result<(), ApiError> {
        self.notification_service
            .mark_as_read(user_id, MarkAsReadRequest { notification_ids })
            .await
            .map(|_| ())
            .map_err(|e| ApiError::Internal(format!("Failed to mark notifications as read: {e}")))
    }

    /// Mark all notifications as read, optionally before a timestamp.
    pub async fn mark_all_as_read(
        &self,
        user_id: &UserId,
        before: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), ApiError> {
        self.notification_service
            .mark_all_as_read(user_id, MarkAllAsReadRequest { before })
            .await
            .map(|_| ())
            .map_err(|e| ApiError::Internal(format!("Failed to mark all notifications as read: {e}")))
    }

    /// Delete a specific notification.
    pub async fn delete_notification(
        &self,
        user_id: &UserId,
        notification_id: Uuid,
    ) -> Result<(), ApiError> {
        self.notification_service
            .delete(user_id, notification_id)
            .await
            .map_err(|e| match e {
                synctv_core::Error::NotFound(_) => {
                    ApiError::NotFound("Notification not found".to_string())
                }
                other => ApiError::Internal(format!("Failed to delete notification: {other}")),
            })
    }

    /// Delete all read notifications for a user.
    pub async fn delete_all_read(&self, user_id: &UserId) -> Result<(), ApiError> {
        self.notification_service
            .delete_all_read(user_id)
            .await
            .map(|_| ())
            .map_err(|e| ApiError::Internal(format!("Failed to delete all read notifications: {e}")))
    }
}
