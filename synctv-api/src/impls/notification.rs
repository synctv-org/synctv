//! Shared Notification Implementation
//!
//! Used by both HTTP and gRPC handlers to avoid duplicating notification logic.

use crate::public_id::PublicIdCodec;
use std::sync::Arc;
use synctv_core::models::id::UserId;
use synctv_core::models::notification::{
    MarkAllAsReadRequest, MarkAsReadRequest, Notification, NotificationData, NotificationListQuery,
    NotificationListSortBy, NotificationType as CoreNotificationType,
};
use synctv_core::models::PageParams;
use synctv_core::models::SortDirection as CoreSortDirection;
use synctv_core::service::UserNotificationService;

use crate::impls::ApiError;
use synctv_proto::client::SortDirection as ProtoSortDirection;
use synctv_proto::client::{
    DeleteAllReadResponse, DeleteNotificationRequest, DeleteNotificationResponse,
    GetNotificationRequest, ListNotificationsRequest, ListNotificationsResponse,
    MarkAllAsReadRequest as ProtoMarkAllAsReadRequest, MarkAllAsReadResponse,
    MarkAsReadRequest as ProtoMarkAsReadRequest, MarkAsReadResponse,
    NotificationListSortBy as ProtoNotificationListSortBy, NotificationProto,
    NotificationType as ProtoNotificationType,
};

/// Convert a domain Notification to a proto `NotificationProto`.
pub fn notification_to_proto(
    n: Notification,
    public_id_codec: &PublicIdCodec,
) -> Result<NotificationProto, ApiError> {
    let notification_type = notification_type_to_proto(n.notification_type);
    let data = Some(notification_data_to_proto(&n.data));

    Ok(NotificationProto {
        id: n.id.to_string(),
        user_id: public_id_codec.encode_user_id(n.user_id).map_err(|error| {
            ApiError::Internal(format!(
                "Failed to encode notification user public id: {error}"
            ))
        })?,
        notification_type: notification_type as i32,
        title: n.title,
        content: n.content,
        data,
        is_read: n.is_read,
        created_at: n.created_at.timestamp(),
        updated_at: n.updated_at.timestamp(),
    })
}

pub(crate) fn notification_type_to_proto(
    notification_type: CoreNotificationType,
) -> ProtoNotificationType {
    match notification_type {
        CoreNotificationType::RoomInvitation => ProtoNotificationType::RoomInvitation,
        CoreNotificationType::SystemAnnouncement => ProtoNotificationType::SystemAnnouncement,
        CoreNotificationType::RoomEvent => ProtoNotificationType::RoomEvent,
        CoreNotificationType::PasswordReset => ProtoNotificationType::PasswordReset,
        CoreNotificationType::EmailBind => ProtoNotificationType::EmailBind,
    }
}

pub(crate) fn notification_data_to_proto(
    data: &NotificationData,
) -> synctv_proto::client::NotificationData {
    synctv_proto::client::NotificationData {
        room_id: data.room_id.clone(),
        room_name: data.room_name.clone(),
        user_id: data.user_id.clone(),
        username: data.username.clone(),
        message_id: data.message_id.clone(),
        action_url: data.action_url.clone(),
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
const MAX_PAGE_SIZE_I32: i32 = 100;

fn positive_i32_to_u32(value: i32, default: u32) -> u32 {
    u32::try_from(value).unwrap_or(default)
}

fn normalized_notification_page_size(value: i32) -> u32 {
    let clamped = value.clamp(1, MAX_PAGE_SIZE_I32);
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

fn proto_notification_sort_by_to_core(sort_by: i32) -> Result<NotificationListSortBy, ApiError> {
    match ProtoNotificationListSortBy::try_from(sort_by).map_err(|_| {
        ApiError::InvalidInput("Unsupported notification list sort field".to_string())
    })? {
        ProtoNotificationListSortBy::Unspecified | ProtoNotificationListSortBy::CreatedAt => {
            Ok(NotificationListSortBy::CreatedAt)
        }
        ProtoNotificationListSortBy::Title => Ok(NotificationListSortBy::Title),
        ProtoNotificationListSortBy::UpdatedAt => Ok(NotificationListSortBy::UpdatedAt),
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
        sort_by: proto_notification_sort_by_to_core(req.sort_by)?,
        sort_direction: proto_sort_direction_to_core(req.sort_direction)?,
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
    let notifications = result
        .notifications
        .into_iter()
        .map(|notification| notification_to_proto(notification, public_id_codec))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ListNotificationsResponse {
        notifications,
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
    ) -> Result<NotificationProto, ApiError> {
        let notification_id = build_get_notification_request(&req)?;
        let notification = self.get_notification(user_id, notification_id).await?;
        notification_to_proto(notification, &self.public_id_codec)
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

pub fn proto_sort_direction_to_core(sort_direction: i32) -> Result<CoreSortDirection, ApiError> {
    match ProtoSortDirection::try_from(sort_direction)
        .map_err(|_| ApiError::InvalidInput("Unsupported sort direction".to_string()))?
    {
        ProtoSortDirection::Unspecified | ProtoSortDirection::Desc => Ok(CoreSortDirection::Desc),
        ProtoSortDirection::Asc => Ok(CoreSortDirection::Asc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn api_ok<T>(result: Result<T, ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn api_err<T>(result: Result<T, ApiError>) -> TestResult<ApiError> {
        match result {
            Ok(_) => Err(test_error("expected API error result")),
            Err(error) => Ok(error),
        }
    }

    fn invalid_notification_type_err(
        result: Result<CoreNotificationType, NotificationTypeParseError>,
    ) -> TestResult<NotificationTypeParseError> {
        match result {
            Ok(_) => Err(test_error("expected invalid notification type")),
            Err(error) => Ok(error),
        }
    }

    #[test]
    fn test_normalize_notification_pagination_defaults_negative_inputs() -> TestResult {
        let pagination = api_ok(normalize_notification_pagination(Some(-5), Some(-100)))?;
        assert_eq!(pagination.page, 1);
        assert_eq!(pagination.page_size, synctv_core::models::DEFAULT_PAGE_SIZE);
        Ok(())
    }

    #[test]
    fn test_normalize_notification_pagination_rejects_excessive_offset() -> TestResult {
        let err = api_err(normalize_notification_pagination(Some(1002), Some(100)))?;
        assert!(matches!(err, ApiError::InvalidInput(_)));
        Ok(())
    }

    #[test]
    fn test_proto_notification_type_to_core_valid_types() {
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
    fn test_proto_notification_type_to_core_unspecified_rejected() -> TestResult {
        let result = proto_notification_type_to_core(0);
        assert!(result.is_err());
        let err = invalid_notification_type_err(result)?;
        assert_eq!(err.invalid_value, 0);
        assert!(err.to_string().contains("Invalid notification type"));
        assert!(err.to_string().contains("must be 1-5"));
        Ok(())
    }

    #[test]
    fn test_proto_notification_type_to_core_unknown_type_rejected() -> TestResult {
        let result = proto_notification_type_to_core(-1);
        assert!(result.is_err());
        let err = invalid_notification_type_err(result)?;
        assert_eq!(err.invalid_value, -1);

        let result = proto_notification_type_to_core(999);
        assert!(result.is_err());
        let err = invalid_notification_type_err(result)?;
        assert_eq!(err.invalid_value, 999);
        Ok(())
    }

    #[test]
    fn test_notification_type_roundtrip() -> TestResult {
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
            let converted_back = proto_notification_type_to_core(proto_value).map_err(|error| {
                test_error(format!(
                    "valid notification type failed conversion: {error}"
                ))
            })?;
            assert_eq!(converted_back, core_type);
        }
        Ok(())
    }

    #[test]
    fn notification_query_enum_mappers_reject_unknown_values_and_preserve_defaults() -> TestResult {
        assert_eq!(
            api_ok(proto_notification_sort_by_to_core(
                ProtoNotificationListSortBy::Unspecified as i32
            ))?,
            NotificationListSortBy::CreatedAt
        );
        assert_eq!(
            api_ok(proto_sort_direction_to_core(
                ProtoSortDirection::Unspecified as i32
            ))?,
            CoreSortDirection::Desc
        );

        assert!(matches!(
            proto_notification_sort_by_to_core(99),
            Err(ApiError::InvalidInput(message)) if message.contains("notification list sort")
        ));
        assert!(matches!(
            proto_sort_direction_to_core(99),
            Err(ApiError::InvalidInput(message)) if message.contains("sort direction")
        ));
        Ok(())
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
    fn test_build_notification_list_query_normalizes_defaults() -> TestResult {
        let query = api_ok(build_notification_list_query(&ListNotificationsRequest {
            page: 0,
            page_size: 0,
            is_read: Some(true),
            notification_type: Some(ProtoNotificationType::RoomInvitation as i32),
            search: "  alert  ".to_string(),
            sort_by: ProtoNotificationListSortBy::Unspecified as i32,
            sort_direction: ProtoSortDirection::Unspecified as i32,
        }))?;

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
        Ok(())
    }

    #[test]
    fn test_build_notification_list_query_rejects_invalid_proto_request() -> TestResult {
        let error = api_err(build_notification_list_query(&ListNotificationsRequest {
            page: -1,
            page_size: 101,
            is_read: None,
            notification_type: Some(ProtoNotificationType::Unspecified as i32),
            search: "a".repeat(101),
            sort_by: 99,
            sort_direction: 99,
        }))?;

        match error {
            ApiError::InvalidInput(message) => {
                assert!(message.contains("page"), "{message}");
                assert!(message.contains("page_size"), "{message}");
                assert!(message.contains("search"), "{message}");
                assert!(message.contains("notification_type"), "{message}");
                assert!(message.contains("sort_by"), "{message}");
                assert!(message.contains("sort_direction"), "{message}");
            }
            other => return Err(test_error(format!("expected invalid input, got {other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_build_get_notification_request_accepts_numeric_id() -> TestResult {
        let notification_id = api_ok(build_get_notification_request(&GetNotificationRequest {
            notification_id: 42,
        }))?;

        assert_eq!(notification_id, 42);
        Ok(())
    }

    #[test]
    fn test_build_get_notification_request_rejects_invalid_numeric_id() -> TestResult {
        let error = api_err(build_get_notification_request(&GetNotificationRequest {
            notification_id: 0,
        }))?;

        assert!(matches!(error, ApiError::InvalidInput(_)));
        Ok(())
    }

    #[test]
    fn test_build_mark_as_read_request_accepts_numeric_ids() -> TestResult {
        let notification_ids = api_ok(build_mark_as_read_request(&ProtoMarkAsReadRequest {
            notification_ids: vec![42, 43],
        }))?;

        assert_eq!(notification_ids, vec![42, 43]);
        Ok(())
    }

    #[test]
    fn test_build_mark_as_read_request_rejects_invalid_numeric_id() -> TestResult {
        let error = api_err(build_mark_as_read_request(&ProtoMarkAsReadRequest {
            notification_ids: vec![0],
        }))?;

        assert!(matches!(error, ApiError::InvalidInput(_)));
        Ok(())
    }

    #[test]
    fn test_build_delete_notification_request_accepts_numeric_id() -> TestResult {
        let notification_id = api_ok(build_delete_notification_request(
            &DeleteNotificationRequest {
                notification_id: 42,
            },
        ))?;

        assert_eq!(notification_id, 42);
        Ok(())
    }

    #[test]
    fn test_build_delete_notification_request_rejects_invalid_numeric_id() -> TestResult {
        let error = api_err(build_delete_notification_request(
            &DeleteNotificationRequest { notification_id: 0 },
        ))?;

        assert!(matches!(error, ApiError::InvalidInput(_)));
        Ok(())
    }
}
