//! Shared Notification Implementation
//!
//! Used by both HTTP and gRPC handlers to avoid duplicating notification logic.

use crate::PublicIdCodec;
use std::sync::Arc;
use synctv_core::models::id::UserId;
use synctv_core::models::notification::{
    MarkAllAsReadRequest, MarkAsReadRequest, Notification, NotificationListQuery,
    NotificationListSortBy, NotificationType as CoreNotificationType,
};
use synctv_core::models::SortDirection as CoreSortDirection;
use synctv_core::models::{PageParams, MAX_PAGE_SIZE};
use synctv_core::service::UserNotificationService;

use crate::impls::ApiError;
use crate::proto::client::SortDirection as ProtoSortDirection;
use crate::proto::client::{
    DeleteAllReadResponse, DeleteNotificationRequest, DeleteNotificationResponse,
    GetNotificationRequest, GetNotificationResponse, ListNotificationsRequest,
    ListNotificationsResponse, MarkAllAsReadRequest as ProtoMarkAllAsReadRequest,
    MarkAllAsReadResponse, MarkAsReadRequest as ProtoMarkAsReadRequest, MarkAsReadResponse,
    NotificationListSortBy as ProtoNotificationListSortBy, NotificationProto,
    NotificationType as ProtoNotificationType,
};

/// Convert a domain Notification to a proto `NotificationProto`.
///
/// Shared by both HTTP and gRPC handlers.
#[must_use]
pub fn notification_to_proto(
    n: Notification,
    public_id_codec: &PublicIdCodec,
) -> NotificationProto {
    let notification_type = match n.notification_type {
        CoreNotificationType::RoomInvitation => ProtoNotificationType::RoomInvitation,
        CoreNotificationType::SystemAnnouncement => ProtoNotificationType::SystemAnnouncement,
        CoreNotificationType::RoomEvent => ProtoNotificationType::RoomEvent,
        CoreNotificationType::PasswordReset => ProtoNotificationType::PasswordReset,
        CoreNotificationType::EmailBind => ProtoNotificationType::EmailBind,
    };

    NotificationProto {
        id: n.id.to_string(),
        user_id: public_id_codec
            .encode_user_id(n.user_id)
            .expect("notification user_id must be encodable"),
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
        Ok(ProtoNotificationType::RoomInvitation) => Ok(CoreNotificationType::RoomInvitation),
        Ok(ProtoNotificationType::SystemAnnouncement) => {
            Ok(CoreNotificationType::SystemAnnouncement)
        }
        Ok(ProtoNotificationType::RoomEvent) => Ok(CoreNotificationType::RoomEvent),
        Ok(ProtoNotificationType::PasswordReset) => Ok(CoreNotificationType::PasswordReset),
        Ok(ProtoNotificationType::EmailBind) => Ok(CoreNotificationType::EmailBind),
        Ok(ProtoNotificationType::Unspecified) | Err(_) => Err(NotificationTypeParseError {
            invalid_value: value,
        }),
    }
}

/// Shared notification operations implementation.
pub struct NotificationApiImpl {
    notification_service: Arc<UserNotificationService>,
    public_id_codec: Arc<PublicIdCodec>,
}

/// Result of listing notifications
pub struct ListNotificationsResult {
    pub notifications: Vec<Notification>,
    pub total: i64,
    pub unread_count: i64,
}

const DEFAULT_NOTIFICATION_PAGE: u32 = 1;
const DEFAULT_NOTIFICATION_PAGE_SIZE: u32 = synctv_core::models::DEFAULT_PAGE_SIZE;

fn positive_i32_to_u32(value: i32, default: u32) -> u32 {
    u32::try_from(value).unwrap_or(default)
}

fn normalized_notification_page_size(value: i32) -> u32 {
    let max_page_size = i32::try_from(MAX_PAGE_SIZE).unwrap_or(i32::MAX);
    let clamped = value.clamp(1, max_page_size);
    positive_i32_to_u32(clamped, DEFAULT_NOTIFICATION_PAGE_SIZE)
}

fn normalize_notification_pagination(
    page: Option<i32>,
    page_size: Option<i32>,
) -> Result<PageParams, ApiError> {
    let page = match page {
        Some(value) if value > 0 => positive_i32_to_u32(value, DEFAULT_NOTIFICATION_PAGE),
        _ => DEFAULT_NOTIFICATION_PAGE,
    };
    let page_size = match page_size {
        Some(value) if value > 0 => normalized_notification_page_size(value),
        _ => DEFAULT_NOTIFICATION_PAGE_SIZE,
    };
    let pagination = PageParams::new(Some(page), Some(page_size));
    pagination.validate().map_err(ApiError::from)?;
    Ok(pagination)
}

fn proto_notification_sort_by_to_core(sort_by: i32) -> NotificationListSortBy {
    match ProtoNotificationListSortBy::try_from(sort_by) {
        Ok(ProtoNotificationListSortBy::Title) => NotificationListSortBy::Title,
        Ok(ProtoNotificationListSortBy::UpdatedAt) => NotificationListSortBy::UpdatedAt,
        _ => NotificationListSortBy::CreatedAt,
    }
}

fn build_notification_list_query(
    req: &ListNotificationsRequest,
) -> Result<NotificationListQuery, ApiError> {
    crate::impls::validate_proto_request(req)?;

    let pagination = normalize_notification_pagination(Some(req.page), Some(req.page_size))?;
    let notification_type = req
        .notification_type
        .map(proto_notification_type_to_core)
        .transpose()
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;

    Ok(NotificationListQuery {
        pagination,
        is_read: req.is_read,
        notification_type,
        search: {
            let trimmed = req.search.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        },
        sort_by: proto_notification_sort_by_to_core(req.sort_by),
        sort_direction: proto_sort_direction_to_core(req.sort_direction),
    })
}

pub(crate) fn build_get_notification_request(
    req: &GetNotificationRequest,
) -> Result<i64, ApiError> {
    crate::impls::validate_proto_request(req)?;
    Ok(req.notification_id)
}

pub(crate) fn build_mark_as_read_request(
    req: &ProtoMarkAsReadRequest,
) -> Result<Vec<i64>, ApiError> {
    crate::impls::validate_proto_request(req)?;
    Ok(req.notification_ids.clone())
}

pub(crate) fn build_delete_notification_request(
    req: &DeleteNotificationRequest,
) -> Result<i64, ApiError> {
    crate::impls::validate_proto_request(req)?;
    Ok(req.notification_id)
}

pub(crate) fn notification_counts_to_proto(
    total: i64,
    unread_count: i64,
) -> Result<(i32, i32), ApiError> {
    let total = i32::try_from(total).map_err(|_| {
        ApiError::Internal("Notification total exceeded int32 response range".to_string())
    })?;
    let unread_count = i32::try_from(unread_count).map_err(|_| {
        ApiError::Internal("Notification unread_count exceeded int32 response range".to_string())
    })?;
    Ok((total, unread_count))
}

fn notification_list_response_to_proto(
    result: ListNotificationsResult,
    public_id_codec: &PublicIdCodec,
) -> Result<ListNotificationsResponse, ApiError> {
    let (total, unread_count) = notification_counts_to_proto(result.total, result.unread_count)?;
    Ok(ListNotificationsResponse {
        notifications: result
            .notifications
            .into_iter()
            .map(|notification| notification_to_proto(notification, public_id_codec))
            .collect(),
        total,
        unread_count,
    })
}

fn proto_mark_all_before(
    req: &ProtoMarkAllAsReadRequest,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    req.before
        .map(|ts| {
            chrono::DateTime::from_timestamp(ts, 0)
                .ok_or_else(|| ApiError::InvalidInput("Invalid timestamp".to_string()))
        })
        .transpose()
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
    pub const fn new(
        notification_service: Arc<UserNotificationService>,
        public_id_codec: Arc<PublicIdCodec>,
    ) -> Self {
        Self {
            notification_service,
            public_id_codec,
        }
    }

    #[must_use]
    pub fn public_id_codec(&self) -> &PublicIdCodec {
        &self.public_id_codec
    }

    /// List notifications for a user with pagination and filters.
    pub async fn list_notifications(
        &self,
        user_id: &UserId,
        req: ListNotificationsRequest,
    ) -> Result<ListNotificationsResult, ApiError> {
        let query = build_notification_list_query(&req)?;

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

    pub async fn list_notifications_response(
        &self,
        user_id: &UserId,
        req: ListNotificationsRequest,
    ) -> Result<ListNotificationsResponse, ApiError> {
        let result = self.list_notifications(user_id, req).await?;
        notification_list_response_to_proto(result, &self.public_id_codec)
    }

    /// Get a single notification by ID.
    pub async fn get_notification(
        &self,
        user_id: &UserId,
        notification_id: i64,
    ) -> Result<Notification, ApiError> {
        self.notification_service
            .get(user_id, notification_id)
            .await
            .map_err(map_notification_lookup_error)
    }

    pub async fn get_notification_response(
        &self,
        user_id: &UserId,
        req: GetNotificationRequest,
    ) -> Result<GetNotificationResponse, ApiError> {
        let notification_id = build_get_notification_request(&req)?;
        let notification = self.get_notification(user_id, notification_id).await?;
        Ok(GetNotificationResponse {
            notification: Some(notification_to_proto(notification, &self.public_id_codec)),
        })
    }

    /// Mark specific notifications as read.
    pub async fn mark_as_read(
        &self,
        user_id: &UserId,
        notification_ids: Vec<i64>,
    ) -> Result<(), ApiError> {
        self.notification_service
            .mark_as_read(user_id, MarkAsReadRequest { notification_ids })
            .await
            .map(|_| ())
            .map_err(map_notification_mutation_error)
    }

    pub async fn mark_as_read_response(
        &self,
        user_id: &UserId,
        req: ProtoMarkAsReadRequest,
    ) -> Result<MarkAsReadResponse, ApiError> {
        let notification_ids = build_mark_as_read_request(&req)?;
        self.mark_as_read(user_id, notification_ids).await?;
        Ok(MarkAsReadResponse {})
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

    pub async fn mark_all_as_read_response(
        &self,
        user_id: &UserId,
        req: ProtoMarkAllAsReadRequest,
    ) -> Result<MarkAllAsReadResponse, ApiError> {
        let before = proto_mark_all_before(&req)?;
        self.mark_all_as_read(user_id, before).await?;
        Ok(MarkAllAsReadResponse {})
    }

    /// Delete a specific notification.
    pub async fn delete_notification(
        &self,
        user_id: &UserId,
        notification_id: i64,
    ) -> Result<(), ApiError> {
        self.notification_service
            .delete(user_id, notification_id)
            .await
            .map_err(map_notification_lookup_error)
    }

    pub async fn delete_notification_response(
        &self,
        user_id: &UserId,
        req: DeleteNotificationRequest,
    ) -> Result<DeleteNotificationResponse, ApiError> {
        let notification_id = build_delete_notification_request(&req)?;
        self.delete_notification(user_id, notification_id).await?;
        Ok(DeleteNotificationResponse {})
    }

    /// Delete all read notifications for a user.
    pub async fn delete_all_read(&self, user_id: &UserId) -> Result<(), ApiError> {
        self.notification_service
            .delete_all_read(user_id)
            .await
            .map(|_| ())
            .map_err(map_notification_mutation_error)
    }

    pub async fn delete_all_read_response(
        &self,
        user_id: &UserId,
    ) -> Result<DeleteAllReadResponse, ApiError> {
        self.delete_all_read(user_id).await?;
        Ok(DeleteAllReadResponse {})
    }
}

pub fn proto_sort_direction_to_core(sort_direction: i32) -> CoreSortDirection {
    match ProtoSortDirection::try_from(sort_direction) {
        Ok(ProtoSortDirection::Asc) => CoreSortDirection::Asc,
        _ => CoreSortDirection::Desc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_notification_pagination_defaults_negative_inputs() {
        let pagination =
            normalize_notification_pagination(Some(-5), Some(-100)).expect("pagination");
        assert_eq!(pagination.page, 1);
        assert_eq!(pagination.page_size, synctv_core::models::DEFAULT_PAGE_SIZE);
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
            Ok(CoreNotificationType::EmailBind)
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
            CoreNotificationType::EmailBind,
        ];

        for core_type in types {
            let proto_value = match core_type {
                CoreNotificationType::RoomInvitation => 1,
                CoreNotificationType::SystemAnnouncement => 2,
                CoreNotificationType::RoomEvent => 3,
                CoreNotificationType::PasswordReset => 4,
                CoreNotificationType::EmailBind => 5,
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

    #[test]
    fn test_build_notification_list_query_normalizes_defaults() {
        let query = build_notification_list_query(&ListNotificationsRequest {
            page: 0,
            page_size: 0,
            is_read: Some(true),
            notification_type: Some(ProtoNotificationType::RoomInvitation as i32),
            search: "  alert  ".to_string(),
            sort_by: ProtoNotificationListSortBy::Unspecified as i32,
            sort_direction: ProtoSortDirection::Unspecified as i32,
        })
        .unwrap();

        assert_eq!(query.pagination.page, 1);
        assert_eq!(
            query.pagination.page_size,
            synctv_core::models::DEFAULT_PAGE_SIZE
        );
        assert_eq!(query.is_read, Some(true));
        assert_eq!(
            query.notification_type,
            Some(CoreNotificationType::RoomInvitation)
        );
        assert_eq!(query.search.as_deref(), Some("alert"));
        assert_eq!(query.sort_by, NotificationListSortBy::CreatedAt);
        assert_eq!(query.sort_direction, CoreSortDirection::Desc);
    }

    #[test]
    fn test_build_notification_list_query_rejects_invalid_proto_request() {
        let error = build_notification_list_query(&ListNotificationsRequest {
            page: -1,
            page_size: 101,
            is_read: None,
            notification_type: Some(ProtoNotificationType::Unspecified as i32),
            search: "a".repeat(101),
            sort_by: 99,
            sort_direction: 99,
        })
        .unwrap_err();

        match error {
            ApiError::InvalidInput(message) => {
                assert!(message.contains("page"), "{message}");
                assert!(message.contains("page_size"), "{message}");
                assert!(message.contains("search"), "{message}");
                assert!(message.contains("notification_type"), "{message}");
                assert!(message.contains("sort_by"), "{message}");
                assert!(message.contains("sort_direction"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn test_build_get_notification_request_accepts_numeric_id() {
        let notification_id = build_get_notification_request(&GetNotificationRequest {
            notification_id: 42,
        })
        .expect("numeric notification ID should be accepted");

        assert_eq!(notification_id, 42);
    }

    #[test]
    fn test_build_get_notification_request_rejects_invalid_numeric_id() {
        let error = build_get_notification_request(&GetNotificationRequest { notification_id: 0 })
            .unwrap_err();

        assert!(matches!(error, ApiError::InvalidInput(_)));
    }

    #[test]
    fn test_build_mark_as_read_request_accepts_numeric_ids() {
        let notification_ids = build_mark_as_read_request(&ProtoMarkAsReadRequest {
            notification_ids: vec![42, 43],
        })
        .expect("numeric notification IDs should be accepted");

        assert_eq!(notification_ids, vec![42, 43]);
    }

    #[test]
    fn test_build_mark_as_read_request_rejects_invalid_numeric_id() {
        let error = build_mark_as_read_request(&ProtoMarkAsReadRequest {
            notification_ids: vec![0],
        })
        .unwrap_err();

        assert!(matches!(error, ApiError::InvalidInput(_)));
    }

    #[test]
    fn test_build_delete_notification_request_accepts_numeric_id() {
        let notification_id = build_delete_notification_request(&DeleteNotificationRequest {
            notification_id: 42,
        })
        .expect("numeric notification ID should be accepted");

        assert_eq!(notification_id, 42);
    }

    #[test]
    fn test_build_delete_notification_request_rejects_invalid_numeric_id() {
        let error =
            build_delete_notification_request(&DeleteNotificationRequest { notification_id: 0 })
                .unwrap_err();

        assert!(matches!(error, ApiError::InvalidInput(_)));
    }
}
