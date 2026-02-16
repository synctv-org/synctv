//! gRPC NotificationService implementation
//!
//! Thin wrapper that delegates to `NotificationApiImpl` from the shared impls layer,
//! converting between proto types and domain types.

use std::sync::Arc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use synctv_core::models::id::UserId;

use crate::impls::NotificationApiImpl;
use crate::impls::notification::{notification_to_proto, proto_notification_type_to_core};
use crate::proto::client::{
    notification_service_server::NotificationService, DeleteAllReadRequest, DeleteAllReadResponse,
    DeleteNotificationRequest, DeleteNotificationResponse, GetNotificationRequest,
    GetNotificationResponse, ListNotificationsRequest, ListNotificationsResponse,
    MarkAllAsReadRequest, MarkAllAsReadResponse, MarkAsReadRequest, MarkAsReadResponse,
};

/// gRPC NotificationService implementation
#[derive(Clone)]
pub struct NotificationServiceImpl {
    notification_api: Arc<NotificationApiImpl>,
}

impl NotificationServiceImpl {
    #[must_use]
    pub fn new(notification_api: Arc<NotificationApiImpl>) -> Self {
        Self { notification_api }
    }

    /// Extract user_id from UserContext (injected by inject_user interceptor)
    #[allow(clippy::result_large_err)]
    fn get_user_id(&self, request: &Request<impl std::fmt::Debug>) -> Result<UserId, Status> {
        let user_context = request
            .extensions()
            .get::<super::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        Ok(UserId::from_string(user_context.user_id.clone()))
    }
}

/// Map NotificationApiImpl errors to gRPC Status using typed classification
fn api_err(err: crate::impls::ApiError) -> Status {
    use crate::impls::ErrorKind;
    let msg = err.to_string();
    match err.classify() {
        ErrorKind::NotFound => Status::not_found(msg),
        ErrorKind::Unauthenticated => Status::unauthenticated(msg),
        ErrorKind::PermissionDenied => Status::permission_denied(msg),
        ErrorKind::AlreadyExists => Status::already_exists(msg),
        ErrorKind::InvalidArgument => Status::invalid_argument(msg),
        ErrorKind::Internal => {
            tracing::error!("Notification API internal error: {msg}");
            Status::internal("Internal error")
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl NotificationService for NotificationServiceImpl {
    async fn list_notifications(
        &self,
        request: Request<ListNotificationsRequest>,
    ) -> Result<Response<ListNotificationsResponse>, Status> {
        let user_id = self.get_user_id(&request)?;
        let req = request.into_inner();

        let notification_type = req
            .notification_type
            .and_then(proto_notification_type_to_core);

        let result = self
            .notification_api
            .list_notifications(
                &user_id,
                Some(if req.page > 0 { req.page } else { 1 }),
                Some(req.page_size.clamp(1, 100)),
                req.is_read,
                notification_type,
            )
            .await
            .map_err(api_err)?;

        Ok(Response::new(ListNotificationsResponse {
            notifications: result.notifications.into_iter().map(notification_to_proto).collect(),
            total: result.total,
            unread_count: result.unread_count,
        }))
    }

    async fn get_notification(
        &self,
        request: Request<GetNotificationRequest>,
    ) -> Result<Response<GetNotificationResponse>, Status> {
        let user_id = self.get_user_id(&request)?;
        let req = request.into_inner();

        let notification_id = Uuid::parse_str(&req.notification_id)
            .map_err(|_| Status::invalid_argument("Invalid notification_id format"))?;

        let notification = self
            .notification_api
            .get_notification(&user_id, notification_id)
            .await
            .map_err(api_err)?;

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

        let notification_ids: Vec<Uuid> = req
            .notification_ids
            .iter()
            .map(|id| {
                Uuid::parse_str(id)
                    .map_err(|_| Status::invalid_argument(format!("Invalid notification_id: {id}")))
            })
            .collect::<Result<Vec<_>, _>>()?;

        self.notification_api
            .mark_as_read(&user_id, notification_ids)
            .await
            .map_err(api_err)?;

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
            .map_err(api_err)?;

        Ok(Response::new(MarkAllAsReadResponse {}))
    }

    async fn delete_notification(
        &self,
        request: Request<DeleteNotificationRequest>,
    ) -> Result<Response<DeleteNotificationResponse>, Status> {
        let user_id = self.get_user_id(&request)?;
        let req = request.into_inner();

        let notification_id = Uuid::parse_str(&req.notification_id)
            .map_err(|_| Status::invalid_argument("Invalid notification_id format"))?;

        self.notification_api
            .delete_notification(&user_id, notification_id)
            .await
            .map_err(api_err)?;

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
            .map_err(api_err)?;

        Ok(Response::new(DeleteAllReadResponse {}))
    }
}
