//! Shared Notification Implementation
//!
//! Used by both HTTP and gRPC handlers to avoid duplicating notification logic.

use std::sync::Arc;
use synctv_core::models::id::UserId;
use synctv_core::models::notification::{
    MarkAllAsReadRequest, MarkAsReadRequest, Notification, NotificationListQuery,
    NotificationType as CoreNotificationType,
};
use synctv_core::models::{PageParams, MAX_PAGE_SIZE};
use synctv_core::service::UserNotificationService;
use uuid::Uuid;

use crate::impls::ApiError;
use crate::proto::client::{NotificationProto, NotificationType as ProtoNotificationType};

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
        data: serde_json::to_vec(&n.data).unwrap_or_else(|e| {
            tracing::warn!(
                notification_id = %n.id,
                error = %e,
                "Failed to serialize notification data, using empty bytes"
            );
            Vec::new()
        }),
        is_read: n.is_read,
        created_at: n.created_at.timestamp(),
        updated_at: n.updated_at.timestamp(),
    }
}

/// Error type for notification type parsing failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationTypeParseError {
    pub invalid_value: i32,
}

impl std::fmt::Display for NotificationTypeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Invalid notification type: {} (must be 1-5)",
            self.invalid_value
        )
    }
}

impl std::error::Error for NotificationTypeParseError {}

/// Convert a proto `NotificationType` enum value to a domain `NotificationType`.
///
/// Shared by both HTTP and gRPC handlers.
///
/// # Errors
///
/// Returns `NotificationTypeParseError` if the value is `Unspecified` (0) or an unknown type.
/// Known valid values are 1-5 corresponding to the proto enum variants.
pub fn proto_notification_type_to_core(
    value: i32,
) -> Result<CoreNotificationType, NotificationTypeParseError> {
    match ProtoNotificationType::try_from(value) {
        Ok(ProtoNotificationType::Unspecified) => Err(NotificationTypeParseError {
            invalid_value: value,
        }),
        Ok(ProtoNotificationType::RoomInvitation) => Ok(CoreNotificationType::RoomInvitation),
        Ok(ProtoNotificationType::SystemAnnouncement) => {
            Ok(CoreNotificationType::SystemAnnouncement)
        }
        Ok(ProtoNotificationType::RoomEvent) => Ok(CoreNotificationType::RoomEvent),
        Ok(ProtoNotificationType::PasswordReset) => Ok(CoreNotificationType::PasswordReset),
        Ok(ProtoNotificationType::EmailVerification) => Ok(CoreNotificationType::EmailVerification),
        Err(_) => Err(NotificationTypeParseError {
            invalid_value: value,
        }),
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

const DEFAULT_NOTIFICATION_PAGE: i32 = 1;
const DEFAULT_NOTIFICATION_PAGE_SIZE: i32 = synctv_core::models::DEFAULT_PAGE_SIZE as i32;

fn normalize_notification_pagination(
    page: Option<i32>,
    page_size: Option<i32>,
) -> Result<PageParams, ApiError> {
    let page = page
        .unwrap_or(DEFAULT_NOTIFICATION_PAGE)
        .max(DEFAULT_NOTIFICATION_PAGE) as u32;
    let page_size = page_size
        .unwrap_or(DEFAULT_NOTIFICATION_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE as i32) as u32;
    let pagination = PageParams::new(Some(page), Some(page_size));
    pagination.validate().map_err(ApiError::from)?;
    Ok(pagination)
}

fn map_notification_lookup_error(err: synctv_core::Error) -> ApiError {
    match err {
        synctv_core::Error::NotFound(_) => ApiError::NotFound("Notification not found".to_string()),
        other => ApiError::from(other),
    }
}

fn map_notification_mutation_error(err: synctv_core::Error) -> ApiError {
    ApiError::from(err)
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
        let pagination = normalize_notification_pagination(page, page_size)?;
        let query = NotificationListQuery {
            pagination,
            is_read,
            notification_type,
        };

        let (notifications, total) = self
            .notification_service
            .list(user_id, query)
            .await
            .map_err(ApiError::from)?;

        let unread_count = self
            .notification_service
            .get_unread_count(user_id)
            .await
            .map_err(map_notification_mutation_error)?;

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
            .map_err(map_notification_lookup_error)
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
            .map_err(map_notification_mutation_error)
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
            .map_err(map_notification_mutation_error)
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
            .map_err(map_notification_lookup_error)
    }

    /// Delete all read notifications for a user.
    pub async fn delete_all_read(&self, user_id: &UserId) -> Result<(), ApiError> {
        self.notification_service
            .delete_all_read(user_id)
            .await
            .map(|_| ())
            .map_err(map_notification_mutation_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_notification_pagination_clamps_negative_inputs() {
        let pagination =
            normalize_notification_pagination(Some(-5), Some(-100)).expect("pagination");
        assert_eq!(pagination.page, 1);
        assert_eq!(pagination.page_size, 1);
    }

    #[test]
    fn test_normalize_notification_pagination_rejects_excessive_offset() {
        let err = normalize_notification_pagination(Some(1002), Some(100)).unwrap_err();
        assert!(matches!(err, ApiError::InvalidInput(_)));
    }

    #[test]
    fn test_proto_notification_type_to_core_valid_types() {
        // Test all valid notification types (1-5)
        assert_eq!(
            proto_notification_type_to_core(1),
            Ok(CoreNotificationType::RoomInvitation)
        );
        assert_eq!(
            proto_notification_type_to_core(2),
            Ok(CoreNotificationType::SystemAnnouncement)
        );
        assert_eq!(
            proto_notification_type_to_core(3),
            Ok(CoreNotificationType::RoomEvent)
        );
        assert_eq!(
            proto_notification_type_to_core(4),
            Ok(CoreNotificationType::PasswordReset)
        );
        assert_eq!(
            proto_notification_type_to_core(5),
            Ok(CoreNotificationType::EmailVerification)
        );
    }

    #[test]
    fn test_proto_notification_type_to_core_unspecified_rejected() {
        // Unspecified (0) should be rejected
        let result = proto_notification_type_to_core(0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.invalid_value, 0);
        assert!(err.to_string().contains("Invalid notification type"));
        assert!(err.to_string().contains("must be 1-5"));
    }

    #[test]
    fn test_proto_notification_type_to_core_unknown_type_rejected() {
        // Unknown types (negative numbers) should be rejected
        let result = proto_notification_type_to_core(-1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.invalid_value, -1);

        // Unknown types (large positive numbers) should be rejected
        let result = proto_notification_type_to_core(999);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.invalid_value, 999);
    }

    #[test]
    fn test_notification_type_parse_error_display() {
        let err = NotificationTypeParseError { invalid_value: 42 };
        let display = format!("{err}");
        assert_eq!(display, "Invalid notification type: 42 (must be 1-5)");
    }

    #[test]
    fn test_notification_type_roundtrip() {
        // Test that converting to proto and back preserves the type
        let types = [
            CoreNotificationType::RoomInvitation,
            CoreNotificationType::SystemAnnouncement,
            CoreNotificationType::RoomEvent,
            CoreNotificationType::PasswordReset,
            CoreNotificationType::EmailVerification,
        ];

        for core_type in types {
            let proto_value = match core_type {
                CoreNotificationType::RoomInvitation => 1,
                CoreNotificationType::SystemAnnouncement => 2,
                CoreNotificationType::RoomEvent => 3,
                CoreNotificationType::PasswordReset => 4,
                CoreNotificationType::EmailVerification => 5,
            };
            let converted_back = proto_notification_type_to_core(proto_value).unwrap();
            assert_eq!(converted_back, core_type);
        }
    }

    #[test]
    fn test_notification_lookup_backend_outage_maps_to_service_unavailable() {
        let mapped =
            map_notification_lookup_error(synctv_core::Error::Database(sqlx::Error::PoolTimedOut));

        assert!(
            matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "Service temporarily unavailable. Please try again later."),
            "notification lookup backend failures must remain service unavailable, got: {mapped:?}"
        );
    }

    #[test]
    fn test_notification_lookup_not_found_stays_not_found() {
        let mapped =
            map_notification_lookup_error(synctv_core::Error::NotFound("missing".to_string()));

        assert!(
            matches!(mapped, ApiError::NotFound(ref msg) if msg == "Notification not found"),
            "missing notifications must remain not found, got: {mapped:?}"
        );
    }

    #[test]
    fn test_notification_mutation_backend_outage_maps_to_service_unavailable() {
        let mapped = map_notification_mutation_error(synctv_core::Error::Database(
            sqlx::Error::PoolTimedOut,
        ));

        assert!(
            matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "Service temporarily unavailable. Please try again later."),
            "notification mutation backend failures must remain service unavailable, got: {mapped:?}"
        );
    }
}
