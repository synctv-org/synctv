//! User notification service
//!
//! Manages user notifications for room invitations, system announcements, and room events
//! These are database-backed notifications that persist until read/deleted

use crate::{
    models::{
        id::UserId,
        notification::{
            default_notification_data, CreateNotificationRequest, MarkAllAsReadRequest,
            MarkAsReadRequest, Notification, NotificationListQuery, NotificationType,
        },
    },
    repository::NotificationRepository,
    Error, Result,
};

/// Event emitted when a notification is created, for real-time push.
#[derive(Clone, Debug)]
pub struct NotificationCreatedEvent {
    pub user_id: UserId,
    pub notification: Notification,
}

/// User notification service
#[derive(Clone, Debug)]
pub struct UserNotificationService {
    repository: NotificationRepository,
    /// Broadcast sender for notification creation events.
    /// Subscribers (e.g., the messaging layer) receive events and push them
    /// to connected WebSocket clients in real time.
    event_tx: tokio::sync::broadcast::Sender<NotificationCreatedEvent>,
}

impl UserNotificationService {
    fn affected_rows_to_usize(value: u64, operation: &'static str) -> Result<usize> {
        usize::try_from(value)
            .map_err(|_| Error::Internal(format!("{operation} affected rows exceed usize::MAX")))
    }

    #[must_use]
    pub fn new(repository: NotificationRepository) -> Self {
        let (event_tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            repository,
            event_tx,
        }
    }

    /// Subscribe to notification creation events.
    ///
    /// Returns a receiver that will emit `NotificationCreatedEvent` whenever
    /// a new notification is created. Used by the messaging layer to push
    /// notifications to connected WebSocket clients.
    #[must_use]
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<NotificationCreatedEvent> {
        self.event_tx.subscribe()
    }

    /// Publish a pre-built notification event to live subscribers.
    ///
    /// This is used when the notification has already been persisted and a caller
    /// only needs to fan out the real-time event to active connections.
    #[must_use]
    pub fn publish_realtime_event(&self, event: NotificationCreatedEvent) -> usize {
        Self::publish_realtime_event_to(&self.event_tx, event)
    }

    fn publish_realtime_event_to(
        event_tx: &tokio::sync::broadcast::Sender<NotificationCreatedEvent>,
        event: NotificationCreatedEvent,
    ) -> usize {
        match event_tx.send(event) {
            Ok(subscriber_count) => subscriber_count,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    "Failed to publish user notification realtime event"
                );
                0
            }
        }
    }

    /// Create a new notification
    pub async fn create(&self, req: CreateNotificationRequest) -> Result<Notification> {
        let notification = self.repository.create(&req).await?;

        let subscriber_count = self.publish_realtime_event(NotificationCreatedEvent {
            user_id: req.user_id,
            notification: notification.clone(),
        });
        if subscriber_count == 0 {
            tracing::debug!(
                user_id = %req.user_id,
                notification_id = %notification.id,
                "User notification created event had no local subscribers"
            );
        }

        Ok(notification)
    }

    /// Create a room invitation notification
    pub async fn create_room_invitation(
        &self,
        user_id: UserId,
        room_id: String,
        room_name: String,
        inviter_name: String,
    ) -> Result<Notification> {
        let data = serde_json::json!({
            "room_id": room_id,
            "room_name": room_name,
            "inviter_name": inviter_name,
        });

        let req = CreateNotificationRequest {
            user_id,
            notification_type: NotificationType::RoomInvitation,
            title: format!("Room Invitation: {room_name}"),
            content: format!("{inviter_name} invited you to join the room \"{room_name}\""),
            data,
        };

        self.create(req).await
    }

    fn system_announcement_request(
        user_id: UserId,
        title: String,
        content: String,
        data: Option<serde_json::Value>,
    ) -> CreateNotificationRequest {
        CreateNotificationRequest {
            user_id,
            notification_type: NotificationType::SystemAnnouncement,
            title,
            content,
            data: data.unwrap_or_else(default_notification_data),
        }
    }

    /// Create a system announcement
    pub async fn create_system_announcement(
        &self,
        user_id: UserId,
        title: String,
        content: String,
        data: Option<serde_json::Value>,
    ) -> Result<Notification> {
        let req = Self::system_announcement_request(user_id, title, content, data);

        self.create(req).await
    }

    /// Create a room event notification
    pub async fn create_room_event(
        &self,
        user_id: UserId,
        room_id: String,
        room_name: String,
        event: String,
    ) -> Result<Notification> {
        let data = serde_json::json!({
            "room_id": room_id,
            "room_name": room_name,
            "event": event,
        });

        let req = CreateNotificationRequest {
            user_id,
            notification_type: NotificationType::RoomEvent,
            title: format!("Room Event: {room_name}"),
            content: event,
            data,
        };

        self.create(req).await
    }

    /// Get notification by ID
    pub async fn get(&self, user_id: &UserId, notification_id: i64) -> Result<Notification> {
        self.repository
            .get_by_user_and_id(user_id, notification_id)
            .await?
            .ok_or_else(|| Error::NotFound("Notification not found".to_string()))
    }

    /// List notifications for a user (single query with COUNT(*) `OVER()`)
    pub async fn list(
        &self,
        user_id: &UserId,
        query: NotificationListQuery,
    ) -> Result<(Vec<Notification>, i64)> {
        query.pagination.validate()?;
        self.repository
            .list_by_user_with_count(user_id, &query)
            .await
    }

    /// Get unread count for a user
    pub async fn get_unread_count(&self, user_id: &UserId) -> Result<i64> {
        self.repository.count_unread(user_id).await
    }

    /// Mark notifications as read
    pub async fn mark_as_read(&self, user_id: &UserId, req: MarkAsReadRequest) -> Result<usize> {
        let affected = self
            .repository
            .mark_as_read(user_id, &req.notification_ids)
            .await?;
        Self::affected_rows_to_usize(affected, "mark notifications as read")
    }

    /// Mark all notifications as read
    pub async fn mark_all_as_read(
        &self,
        user_id: &UserId,
        req: MarkAllAsReadRequest,
    ) -> Result<usize> {
        let affected = self
            .repository
            .mark_all_as_read(user_id, req.before)
            .await?;
        Self::affected_rows_to_usize(affected, "mark all notifications as read")
    }

    /// Delete a notification
    pub async fn delete(&self, user_id: &UserId, notification_id: i64) -> Result<()> {
        self.repository.delete(user_id, notification_id).await
    }

    /// Delete all read notifications
    pub async fn delete_all_read(&self, user_id: &UserId) -> Result<usize> {
        let affected = self.repository.delete_all_read(user_id).await?;
        Self::affected_rows_to_usize(affected, "delete all read notifications")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn test_notification_type_from_str() {
        assert_eq!(
            ok(
                "room_invitation".parse::<NotificationType>(),
                "notification type should parse",
            ),
            NotificationType::RoomInvitation
        );
    }

    #[tokio::test]
    async fn publish_realtime_event_reports_subscriber_count() {
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let event = NotificationCreatedEvent {
            user_id: UserId::expect_positive(1),
            notification: Notification {
                id: 2,
                user_id: UserId::expect_positive(1),
                notification_type: NotificationType::SystemAnnouncement,
                title: "title".to_string(),
                content: "content".to_string(),
                data: serde_json::Value::Null,
                is_read: false,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        };

        assert_eq!(
            UserNotificationService::publish_realtime_event_to(&event_tx, event.clone()),
            0
        );

        let _receiver = event_tx.subscribe();
        assert_eq!(
            UserNotificationService::publish_realtime_event_to(&event_tx, event),
            1
        );
    }

    #[test]
    fn system_announcement_request_defaults_missing_data_to_empty_object() {
        let req = UserNotificationService::system_announcement_request(
            UserId::expect_positive(1),
            "title".to_string(),
            "content".to_string(),
            None,
        );

        assert_eq!(req.notification_type, NotificationType::SystemAnnouncement);
        assert_eq!(req.data, serde_json::json!({}));
    }
}
