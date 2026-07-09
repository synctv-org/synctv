//! Bilibili Provider gRPC Service Implementation

use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::api_runtime::SharedApiRuntime;
use crate::impls::BilibiliApiImpl;
use crate::impls::{EndpointRateLimitCategory, RequestExecutor};

// Import generated proto types from synctv_proto
use synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderService;
use synctv_proto::providers::bilibili::{
    CheckQrRequest, GetBindsRequest, GetBindsResponse, LoginQrRequest, LoginSmsRequest,
    LoginSmsResponse, LogoutRequest, LogoutResponse, ParseRequest, ParseResponse, QrCodeResponse,
    QrStatusResponse, SendSmsRequest, SendSmsResponse, StartSmsLoginRequest, StartSmsLoginResponse,
    UserInfoRequest, UserInfoResponse,
};

/// Bilibili Provider gRPC Service
///
/// Thin wrapper that delegates to `BilibiliApiImpl`.
#[derive(Clone)]
pub struct BilibiliProviderGrpcService {
    api: Arc<BilibiliApiImpl>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

impl BilibiliProviderGrpcService {
    #[must_use]
    pub fn new(
        shared_api_runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<crate::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            api: shared_api_runtime.bilibili_api.clone(),
            request_executor,
            runtime_settings,
        }
    }
}

fn redact_qr_key(key: &str) -> &'static str {
    if key.is_empty() {
        "<empty>"
    } else {
        "[SECRET:redacted]"
    }
}

#[tonic::async_trait]
// Tonic generated service traits require `Result<Response<_>, tonic::Status>`.
// Provider business logic stays in `BilibiliApiImpl`.
#[allow(clippy::result_large_err)]
impl BilibiliProviderService for BilibiliProviderGrpcService {
    async fn parse(
        &self,
        request: Request<ParseRequest>,
    ) -> Result<Response<ParseResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
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
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
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
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
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

    async fn start_sms_login(
        &self,
        request: Request<StartSmsLoginRequest>,
    ) -> Result<Response<StartSmsLoginResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili start SMS login request");
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, _| async move {
                    api.start_sms_login_with_context(
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
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let masked_phone = if req.phone.len() >= 4 {
            format!("****{}", &req.phone[req.phone.len() - 4..])
        } else {
            "****".to_string()
        };
        tracing::info!("gRPC Bilibili send SMS: phone={}", masked_phone);
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, _| async move {
                    api.send_sms_with_context(req, None, Some(&request_control))
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
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili login SMS");
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, authenticated| async move {
                    api.login_sms_with_context(
                        &authenticated.user_id,
                        req,
                        None,
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
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
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
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
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
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
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
