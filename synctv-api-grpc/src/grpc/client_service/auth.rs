use tonic::{Request, Response, Status};

use super::{map_api_error, ClientServiceImpl};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::client::{
    auth_service_server::AuthService, ConfirmEmailLoginRequest, ConfirmEmailRegistrationRequest,
    CreateGuestTokenRequest, CreateGuestTokenResponse, FinishMfaPasskeyRequest,
    FinishOpaqueLoginRequest, FinishOpaqueRegistrationRequest, FinishPasskeyLoginRequest,
    FinishPasskeyRegistrationRequest, LoginResponse, LoginWithDirectPasswordRequest,
    RefreshTokenRequest, RefreshTokenResponse, RegisterResponse, RegisterWithDirectPasswordRequest,
    RequestEmailLoginRequest, RequestEmailLoginResponse, RequestEmailRegistrationRequest,
    RequestEmailRegistrationResponse, RequestMfaEmailCodeRequest, RequestMfaEmailCodeResponse,
    StartMfaPasskeyRequest, StartMfaPasskeyResponse, StartOpaqueLoginRequest,
    StartOpaqueLoginResponse, StartOpaqueRegistrationRequest, StartOpaqueRegistrationResponse,
    StartPasskeyLoginRequest, StartPasskeyLoginResponse, StartPasskeyRegistrationRequest,
    StartPasskeyRegistrationResponse, VerifyMfaEmailCodeRequest,
};

#[tonic::async_trait]
// Tonic generated service traits require `Result<Response<_>, tonic::Status>`.
// Keep the large error type at this transport boundary; shared business logic
// below this layer returns `ApiError`.
#[allow(clippy::result_large_err)]
impl AuthService for ClientServiceImpl {
    async fn register_with_direct_password(
        &self,
        request: Request<RegisterWithDirectPasswordRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .register_with_direct_password_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn login_with_direct_password(
        &self,
        request: Request<LoginWithDirectPasswordRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .login_with_direct_password_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn request_email_registration(
        &self,
        request: Request<RequestEmailRegistrationRequest>,
    ) -> Result<Response<RequestEmailRegistrationResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let email_api = self.email_api().map_err(map_api_error)?;
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    email_api
                        .request_email_registration_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(RequestEmailRegistrationResponse {
            message: result.message,
        }))
    }

    async fn confirm_email_registration(
        &self,
        request: Request<ConfirmEmailRegistrationRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .confirm_email_registration_with_direct_password_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_opaque_registration(
        &self,
        request: Request<StartOpaqueRegistrationRequest>,
    ) -> Result<Response<StartOpaqueRegistrationResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .start_opaque_registration_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_opaque_registration(
        &self,
        request: Request<FinishOpaqueRegistrationRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .finish_opaque_registration_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn confirm_email_login(
        &self,
        request: Request<ConfirmEmailLoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let email_api = self.email_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .confirm_email_login_with_control(
                            email_api.as_deref(),
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn create_guest_token(
        &self,
        request: Request<CreateGuestTokenRequest>,
    ) -> Result<Response<CreateGuestTokenResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .create_guest_token_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_opaque_login(
        &self,
        request: Request<StartOpaqueLoginRequest>,
    ) -> Result<Response<StartOpaqueLoginResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .start_opaque_login_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_opaque_login(
        &self,
        request: Request<FinishOpaqueLoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .finish_opaque_login_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_passkey_registration(
        &self,
        request: Request<StartPasskeyRegistrationRequest>,
    ) -> Result<Response<StartPasskeyRegistrationResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .start_passkey_registration_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_passkey_registration(
        &self,
        request: Request<FinishPasskeyRegistrationRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .finish_passkey_registration_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_passkey_login(
        &self,
        request: Request<StartPasskeyLoginRequest>,
    ) -> Result<Response<StartPasskeyLoginResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .start_passkey_login_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_passkey_login(
        &self,
        request: Request<FinishPasskeyLoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .finish_passkey_login_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn request_email_login(
        &self,
        request: Request<RequestEmailLoginRequest>,
    ) -> Result<Response<RequestEmailLoginResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let email_api = self.email_api().map_err(map_api_error)?;
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    email_api
                        .request_email_login_with_control(&req.email, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(RequestEmailLoginResponse {
            message: result.message,
        }))
    }

    async fn request_mfa_email_code(
        &self,
        request: Request<RequestMfaEmailCodeRequest>,
    ) -> Result<Response<RequestMfaEmailCodeResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let email_api = self.email_api().map_err(map_api_error)?;
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    email_api
                        .request_mfa_email_code_response_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(result))
    }

    async fn verify_mfa_email_code(
        &self,
        request: Request<VerifyMfaEmailCodeRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let email_api = self.email_api().map_err(map_api_error)?;
        let email_api = email_api.clone();
        let public_id_codec = self.client_api.public_id_codec.clone();
        let executor = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    let outcome = email_api
                        .verify_mfa_email_code_request_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await?;
                    synctv_api_common::impls::client::login_outcome_to_proto(
                        outcome,
                        &public_id_codec,
                    )
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(response))
    }

    async fn start_mfa_passkey(
        &self,
        request: Request<StartMfaPasskeyRequest>,
    ) -> Result<Response<StartMfaPasskeyResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |_request_control| async move {
                    client_api.start_mfa_passkey_with_control(req).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_mfa_passkey(
        &self,
        request: Request<FinishMfaPasskeyRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .finish_mfa_passkey_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn refresh_token(
        &self,
        request: Request<RefreshTokenRequest>,
    ) -> Result<Response<RefreshTokenResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .refresh_token_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}
