//! gRPC `NotificationService` implementation
//!
//! Thin wrapper that delegates to `NotificationApiImpl` from the shared impls layer,
//! converting between proto types and domain types.

use std::sync::Arc;
use tonic::{Request, Response, Status};

use synctv_core::Config;

use crate::impls::notification::{
    build_delete_notification_request, build_get_notification_request, build_mark_as_read_request,
    notification_counts_to_proto, notification_to_proto,
};
use crate::impls::NotificationApiImpl;
use crate::impls::{EndpointRateLimitCategory, RequestExecutor};
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
    request_executor: Arc<RequestExecutor>,
    config: Arc<Config>,
}

impl NotificationServiceImpl {
    #[must_use]
    pub const fn new(
        notification_api: Arc<NotificationApiImpl>,
        request_executor: Arc<RequestExecutor>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            notification_api,
            request_executor,
            config,
        }
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
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        let notification_api = Arc::clone(&self.notification_api);
        let result = self
            .request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    notification_api
                        .list_notifications(&authenticated.user_id, req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        let (total, unread_count) = notification_counts_to_proto(result.total, result.unread_count)
            .map_err(map_api_error)?;

        Ok(Response::new(ListNotificationsResponse {
            notifications: result
                .notifications
                .into_iter()
                .map(|notification| {
                    notification_to_proto(notification, self.notification_api.public_id_codec())
                })
                .collect(),
            total,
            unread_count,
        }))
    }

    async fn get_notification(
        &self,
        request: Request<GetNotificationRequest>,
    ) -> Result<Response<GetNotificationResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        let notification_api = Arc::clone(&self.notification_api);

        let notification_id = build_get_notification_request(&req).map_err(map_api_error)?;

        let notification = self
            .request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    notification_api
                        .get_notification(&authenticated.user_id, notification_id)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(GetNotificationResponse {
            notification: Some(notification_to_proto(
                notification,
                self.notification_api.public_id_codec(),
            )),
        }))
    }

    async fn mark_as_read(
        &self,
        request: Request<MarkAsReadRequest>,
    ) -> Result<Response<MarkAsReadResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        let notification_api = Arc::clone(&self.notification_api);

        let notification_ids = build_mark_as_read_request(&req).map_err(map_api_error)?;

        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    notification_api
                        .mark_as_read(&authenticated.user_id, notification_ids)
                        .await?;
                    Ok(())
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(MarkAsReadResponse {}))
    }

    async fn mark_all_as_read(
        &self,
        request: Request<MarkAllAsReadRequest>,
    ) -> Result<Response<MarkAllAsReadResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        let notification_api = Arc::clone(&self.notification_api);

        let before = req
            .before
            .map(|ts| {
                chrono::DateTime::from_timestamp(ts, 0)
                    .ok_or_else(|| Status::invalid_argument("Invalid timestamp"))
            })
            .transpose()?;

        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    notification_api
                        .mark_all_as_read(&authenticated.user_id, before)
                        .await?;
                    Ok(())
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(MarkAllAsReadResponse {}))
    }

    async fn delete_notification(
        &self,
        request: Request<DeleteNotificationRequest>,
    ) -> Result<Response<DeleteNotificationResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        let notification_api = Arc::clone(&self.notification_api);

        let notification_id = build_delete_notification_request(&req).map_err(map_api_error)?;

        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    notification_api
                        .delete_notification(&authenticated.user_id, notification_id)
                        .await?;
                    Ok(())
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(DeleteNotificationResponse {}))
    }

    async fn delete_all_read(
        &self,
        request: Request<DeleteAllReadRequest>,
    ) -> Result<Response<DeleteAllReadResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        );
        let notification_api = Arc::clone(&self.notification_api);

        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    notification_api
                        .delete_all_read(&authenticated.user_id)
                        .await?;
                    Ok(())
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(DeleteAllReadResponse {}))
    }
}
