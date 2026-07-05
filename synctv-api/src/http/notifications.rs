//! User notification HTTP endpoints
//!
//! REST API for managing user notifications.
//! Delegates to `NotificationApiImpl` for shared business logic.
//!
//! Uses proto-generated types for request/response to ensure type consistency
//! with gRPC handlers.

use crate::http::error::AppResult;
use crate::http::middleware::RequestMetadata;
use crate::http::validation::ProtoQuery;
use crate::http::AppState;
use crate::impls::EndpointRateLimitCategory;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use synctv_proto::client::{
    DeleteNotificationRequest, GetNotificationRequest, ListNotificationsResponse,
    MarkAllAsReadRequest, MarkAsReadRequest, NotificationProto,
};

fn get_notification_api(
    state: &AppState,
) -> Result<&crate::impls::NotificationApiImpl, crate::http::AppError> {
    state
        .shared_api_runtime
        .notification_api
        .as_ref()
        .map(std::convert::AsRef::as_ref)
        .ok_or_else(|| {
            crate::http::AppError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Notification service not available",
            )
        })
}

/// GET /api/notifications - List user's notifications
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/notifications",
        tag = "Notification",
        params(synctv_proto::client::ListNotificationsRequest),
        responses(
            (status = 200, description = "Notifications list", body = ListNotificationsResponse),
            (status = 400, description = "Invalid notification filter", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Notification service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_notifications(
    request_meta: RequestMetadata,
    ProtoQuery(query): ProtoQuery<synctv_proto::client::ListNotificationsRequest>,
    State(state): State<AppState>,
) -> AppResult<Json<ListNotificationsResponse>> {
    let api = get_notification_api(&state)?;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));

    let result = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            |auth| async move { api.list_notifications_response(&auth.user_id, query).await },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(result))
}

/// GET /api/notifications/:id - Get a specific notification
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/notifications/{notificationId}",
        tag = "Notification",
        params(
            ("notificationId" = i64, Path, description = "Notification numeric ID")
        ),
        responses(
            (status = 200, description = "Notification details", body = NotificationProto),
            (status = 400, description = "Invalid notification ID", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Notification not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_notification(
    request_meta: RequestMetadata,
    Path(req): Path<GetNotificationRequest>,
    State(state): State<AppState>,
) -> AppResult<Json<NotificationProto>> {
    let api = get_notification_api(&state)?;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));

    let response = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            |auth| async move { api.get_notification_response(&auth.user_id, req).await },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(response))
}

/// POST /api/notifications/read - Mark notifications as read
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/notifications/actions/mark-read",
        tag = "Notification",
        request_body = MarkAsReadRequest,
        responses(
            (status = 204, description = "Notifications marked as read"),
            (status = 400, description = "Invalid notification IDs", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Notification service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn mark_as_read(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<MarkAsReadRequest>,
) -> AppResult<StatusCode> {
    let api = get_notification_api(&state)?;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));

    state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                api.mark_as_read_response(&auth.user_id, req)
                    .await
                    .map(|_| ())
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/notifications/read-all - Mark all notifications as read
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/notifications/read-all",
        tag = "Notification",
        request_body = Option<MarkAllAsReadRequest>,
        responses(
            (status = 204, description = "Notifications marked as read"),
            (status = 400, description = "Invalid timestamp", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Notification service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn mark_all_as_read(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    req: Option<Json<MarkAllAsReadRequest>>,
) -> AppResult<StatusCode> {
    let api = get_notification_api(&state)?;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));

    let req = req.map_or_else(MarkAllAsReadRequest::default, |Json(req)| req);

    state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                api.mark_all_as_read_response(&auth.user_id, req)
                    .await
                    .map(|_| ())
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /api/notifications/:id - Delete a notification
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/notifications/{notificationId}",
        tag = "Notification",
        params(
            ("notificationId" = i64, Path, description = "Notification numeric ID")
        ),
        responses(
            (status = 204, description = "Notification deleted"),
            (status = 400, description = "Invalid notification ID", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Notification not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_notification(
    request_meta: RequestMetadata,
    Path(req): Path<DeleteNotificationRequest>,
    State(state): State<AppState>,
) -> AppResult<StatusCode> {
    let api = get_notification_api(&state)?;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));

    state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                api.delete_notification_response(&auth.user_id, req)
                    .await
                    .map(|_| ())
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /api/notifications/read - Delete all read notifications
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/notifications/read",
        tag = "Notification",
        responses(
            (status = 204, description = "All read notifications deleted"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Notification service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_all_read(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<StatusCode> {
    let api = get_notification_api(&state)?;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));

    state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                api.delete_all_read_response(&auth.user_id)
                    .await
                    .map(|_| ())
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Create the notification read router (GET endpoints -- under read rate limit)
pub(crate) fn create_notification_read_router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/notifications", axum::routing::get(list_notifications))
        .route(
            "/api/notifications/{notificationId}",
            axum::routing::get(get_notification),
        )
}

/// Create the notification write router (POST/DELETE endpoints -- under write rate limit)
pub(crate) fn create_notification_write_router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/notifications/{notificationId}",
            axum::routing::delete(delete_notification),
        )
        .route(
            "/api/notifications/actions/mark-read",
            axum::routing::post(mark_as_read),
        )
        .route(
            "/api/notifications/read",
            axum::routing::delete(delete_all_read),
        )
        .route(
            "/api/notifications/read-all",
            axum::routing::post(mark_all_as_read),
        )
}

#[cfg(test)]
mod tests {
    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn test_list_notifications_request_deserializes_search_and_sort() -> TestResult {
        let query: synctv_proto::client::ListNotificationsRequest =
            serde_json::from_str(r#"{"search":"alert","sortBy":3,"sortDirection":1}"#)?;
        assert_eq!(query.search, "alert");
        assert_eq!(
            query.sort_by,
            synctv_proto::client::NotificationListSortBy::Title as i32
        );
        assert_eq!(
            query.sort_direction,
            synctv_proto::client::SortDirection::Asc as i32
        );
        Ok(())
    }

    #[test]
    fn test_notification_path_requests_deserialize_proto_field_names() -> TestResult {
        let get_request: synctv_proto::client::GetNotificationRequest =
            serde_json::from_str(r#"{"notificationId":42}"#)?;
        assert_eq!(get_request.notification_id, 42);

        let delete_request: synctv_proto::client::DeleteNotificationRequest =
            serde_json::from_str(r#"{"notificationId":42}"#)?;
        assert_eq!(delete_request.notification_id, 42);
        Ok(())
    }
}
