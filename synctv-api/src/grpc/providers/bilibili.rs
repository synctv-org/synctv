//! Bilibili Provider gRPC Service Implementation

use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::http::SharedApiRuntime;
use crate::impls::BilibiliApiImpl;
use crate::impls::{EndpointRateLimitCategory, RequestExecutor};
use synctv_core::Config;

// Import generated proto types from synctv_proto
use crate::proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderService;
use crate::proto::providers::bilibili::{
    CaptchaResponse, CheckQrRequest, GetBindsRequest, GetBindsResponse, GetCaptchaRequest,
    LoginQrRequest, LoginSmsRequest, LoginSmsResponse, LogoutRequest, LogoutResponse, ParseRequest,
    ParseResponse, QrCodeResponse, QrStatusResponse, SendSmsRequest, SendSmsResponse,
    UserInfoRequest, UserInfoResponse,
};

/// Bilibili Provider gRPC Service
///
/// Thin wrapper that delegates to `BilibiliApiImpl`.
#[derive(Clone)]
pub struct BilibiliProviderGrpcService {
    api: Arc<BilibiliApiImpl>,
    request_executor: Arc<RequestExecutor>,
    config: Arc<Config>,
}

impl BilibiliProviderGrpcService {
    #[must_use]
    pub fn new(
        shared_api_runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            api: shared_api_runtime.bilibili_api.clone(),
            request_executor,
            config,
        }
    }
}

fn redact_qr_key(key: &str) -> &'static str {
    synctv_core::secrets::mask_secret(key)
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl BilibiliProviderService for BilibiliProviderGrpcService {
    async fn parse(
        &self,
        request: Request<ParseRequest>,
    ) -> Result<Response<ParseResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili parse request: url={}", req.url);
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |request_control, authenticated| async move {
                    api.parse_with_context(
                        &authenticated.user_id,
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn login_qr(
        &self,
        request: Request<LoginQrRequest>,
    ) -> Result<Response<QrCodeResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili login QR request");
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, _| async move {
                    api.login_qr_with_context(req, instance_name.as_deref(), Some(&request_control))
                        .await
                        .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn check_qr(
        &self,
        request: Request<CheckQrRequest>,
    ) -> Result<Response<QrStatusResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili check QR: key={}", redact_qr_key(&req.key));
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, authenticated| async move {
                    api.check_qr_with_context(
                        &authenticated.user_id,
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn get_captcha(
        &self,
        request: Request<GetCaptchaRequest>,
    ) -> Result<Response<CaptchaResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili get captcha request");
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, _| async move {
                    api.get_captcha_with_context(
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn send_sms(
        &self,
        request: Request<SendSmsRequest>,
    ) -> Result<Response<SendSmsResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        let masked_phone = if req.phone.len() >= 4 {
            format!("****{}", &req.phone[req.phone.len() - 4..])
        } else {
            "****".to_string()
        };
        tracing::info!("gRPC Bilibili send SMS: phone={}", masked_phone);
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, _| async move {
                    api.send_sms_with_context(req, instance_name.as_deref(), Some(&request_control))
                        .await
                        .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn login_sms(
        &self,
        request: Request<LoginSmsRequest>,
    ) -> Result<Response<LoginSmsResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        let masked_phone = if req.phone.len() >= 4 {
            format!("****{}", &req.phone[req.phone.len() - 4..])
        } else {
            "****".to_string()
        };
        tracing::info!("gRPC Bilibili login SMS: phone={}", masked_phone);
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, authenticated| async move {
                    api.login_sms_with_context(
                        &authenticated.user_id,
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn get_user_info(
        &self,
        request: Request<UserInfoRequest>,
    ) -> Result<Response<UserInfoResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili user info request");
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |request_control, authenticated| async move {
                    api.get_user_info_with_context(
                        &authenticated.user_id,
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn logout(
        &self,
        request: Request<LogoutRequest>,
    ) -> Result<Response<LogoutResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili logout request");
        let api = self.api.clone();

        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |authenticated| async move {
                    api.logout(&authenticated.user_id, req)
                        .await
                        .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn get_binds(
        &self,
        request: Request<GetBindsRequest>,
    ) -> Result<Response<GetBindsResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.get_ref();
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    api.get_binds(&authenticated.user_id, instance_name.as_deref())
                        .await
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }
}

#[cfg(test)]
mod tests {
    use super::redact_qr_key;

    #[test]
    fn redact_qr_key_never_exposes_original_value() {
        let key = "qr-secret-token-abcdef123456";
        let redacted = redact_qr_key(key);

        assert_eq!(redacted, "[SECRET:redacted]");
        assert!(!redacted.contains(key));
        assert_eq!(redacted, redact_qr_key("short"));
    }
}
