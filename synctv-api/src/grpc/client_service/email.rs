use tonic::{Request, Response, Status};

use super::{map_api_error, ClientServiceImpl};
use crate::impls::EndpointRateLimitCategory;
use synctv_proto::client::{
    email_service_server::EmailService, ConfirmPasswordResetResponse,
    FinishOpaquePasswordResetRequest, RequestPasswordResetRequest, RequestPasswordResetResponse,
    StartOpaquePasswordResetRequest, StartOpaquePasswordResetResponse,
};

// Delegates to shared EmailApiImpl to avoid duplicating logic with HTTP handlers.
#[tonic::async_trait]
// Tonic generated service traits require `Result<Response<_>, tonic::Status>`.
#[allow(clippy::result_large_err)]
impl EmailService for ClientServiceImpl {
    async fn request_password_reset(
        &self,
        request: Request<RequestPasswordResetRequest>,
    ) -> Result<Response<RequestPasswordResetResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Email,
                move |request_control| async move {
                    email_api
                        .request_password_reset_response_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(result))
    }

    async fn start_opaque_password_reset(
        &self,
        request: Request<StartOpaquePasswordResetRequest>,
    ) -> Result<Response<StartOpaquePasswordResetResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Email,
                move |request_control| async move {
                    email_api
                        .start_opaque_password_reset_response_with_control(
                            req,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(result))
    }

    async fn finish_opaque_password_reset(
        &self,
        request: Request<FinishOpaquePasswordResetRequest>,
    ) -> Result<Response<ConfirmPasswordResetResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Email,
                move |request_control| async move {
                    email_api
                        .finish_opaque_password_reset_response_with_control(
                            req,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(result))
    }
}
