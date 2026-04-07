//! gRPC `NotificationService` implementation
//!
//! Thin wrapper that delegates to `NotificationApiImpl` from the shared impls layer,
//! converting between proto types and domain types.

use std::sync::Arc;
use tonic::{Request, Response, Status};

use synctv_core::models::id::UserId;

use crate::impls::notification::{
    build_delete_notification_request, build_get_notification_request, build_mark_as_read_request,
    notification_to_proto,
};
use crate::impls::NotificationApiImpl;
use crate::proto::client::{
    notification_service_server::NotificationService, DeleteAllReadRequest, DeleteAllReadResponse,
    DeleteNotificationRequest, DeleteNotificationResponse, GetNotificationRequest,
    GetNotificationResponse, ListNotificationsRequest, ListNotificationsResponse,
    MarkAllAsReadRequest, MarkAllAsReadResponse, MarkAsReadRequest, MarkAsReadResponse,
};

/// gRPC `NotificationService` implementation
#[derive(Clone)]
pub struct NotificationServiceImpl {
    notification_api: Arc<NotificationApiImpl>,
}

impl NotificationServiceImpl {
    #[must_use]
    pub const fn new(notification_api: Arc<NotificationApiImpl>) -> Self {
        Self { notification_api }
    }

    /// Extract `user_id` from `UserContext` (injected by `inject_user` interceptor)
    #[allow(clippy::result_large_err)]
    fn get_user_id(&self, request: &Request<impl std::fmt::Debug>) -> Result<UserId, Status> {
        super::interceptors::extract_user_id(request)
    }
}

use super::map_api_error;

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl NotificationService for NotificationServiceImpl {
    async fn list_notifications(
        &self,
        request: Request<ListNotificationsRequest>,
    ) -> Result<Response<ListNotificationsResponse>, Status> {
        let user_id = self.get_user_id(&request)?;
        let req = request.into_inner();

        let result = self
            .notification_api
            .list_notifications(&user_id, req)
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(ListNotificationsResponse {
            notifications: result
                .notifications
                .into_iter()
                .map(notification_to_proto)
                .collect(),
            total: result.total as i32,
            unread_count: result.unread_count as i32,
        }))
    }

    async fn get_notification(
        &self,
        request: Request<GetNotificationRequest>,
    ) -> Result<Response<GetNotificationResponse>, Status> {
        let user_id = self.get_user_id(&request)?;
        let req = request.into_inner();

        let notification_id = build_get_notification_request(req).map_err(map_api_error)?;

        let notification = self
            .notification_api
            .get_notification(&user_id, notification_id)
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(GetNotificationResponse {
            notification: Some(notification_to_proto(notification)),
        }))
    }

    async fn mark_as_read(
        &self,
        request: Request<MarkAsReadRequest>,
    ) -> Result<Response<MarkAsReadResponse>, Status> {
        let user_id = self.get_user_id(&request)?;
        let req = request.into_inner();

        let notification_ids = build_mark_as_read_request(req).map_err(map_api_error)?;

        self.notification_api
            .mark_as_read(&user_id, notification_ids)
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(MarkAsReadResponse {}))
    }

    async fn mark_all_as_read(
        &self,
        request: Request<MarkAllAsReadRequest>,
    ) -> Result<Response<MarkAllAsReadResponse>, Status> {
        let user_id = self.get_user_id(&request)?;
        let req = request.into_inner();

        let before = req
            .before
            .map(|ts| {
                chrono::DateTime::from_timestamp(ts, 0)
                    .ok_or_else(|| Status::invalid_argument("Invalid timestamp"))
            })
            .transpose()?;

        self.notification_api
            .mark_all_as_read(&user_id, before)
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(MarkAllAsReadResponse {}))
    }

    async fn delete_notification(
        &self,
        request: Request<DeleteNotificationRequest>,
    ) -> Result<Response<DeleteNotificationResponse>, Status> {
        let user_id = self.get_user_id(&request)?;
        let req = request.into_inner();

        let notification_id = build_delete_notification_request(req).map_err(map_api_error)?;

        self.notification_api
            .delete_notification(&user_id, notification_id)
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(DeleteNotificationResponse {}))
    }

    async fn delete_all_read(
        &self,
        request: Request<DeleteAllReadRequest>,
    ) -> Result<Response<DeleteAllReadResponse>, Status> {
        let user_id = self.get_user_id(&request)?;

        self.notification_api
            .delete_all_read(&user_id)
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(DeleteAllReadResponse {}))
    }
}
