//! Bilibili Provider gRPC Service Implementation

use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::http::AppState;
use crate::impls::providers::extract_instance_name;
use crate::impls::BilibiliApiImpl;

// Import generated proto types from synctv_proto
use crate::proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderService;
use crate::proto::providers::bilibili::{
    CaptchaResponse, CheckQrRequest, GetCaptchaRequest, LoginQrRequest, LoginSmsRequest,
    LoginSmsResponse, LogoutRequest, LogoutResponse, ParseRequest, ParseResponse, QrCodeResponse,
    QrStatusResponse, SendSmsRequest, SendSmsResponse, UserInfoRequest, UserInfoResponse,
};

use crate::grpc::map_provider_error as api_err;

/// Bilibili Provider gRPC Service
///
/// Thin wrapper that delegates to `BilibiliApiImpl`.
#[derive(Clone)]
pub struct BilibiliProviderGrpcService {
    api: BilibiliApiImpl,
}

impl BilibiliProviderGrpcService {
    #[must_use]
    pub fn new(app_state: &Arc<AppState>) -> Self {
        let api = BilibiliApiImpl::new(
            app_state.providers.bilibili.clone(),
            app_state.user_provider_credential_repository.clone(),
        );
        Self { api }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl BilibiliProviderService for BilibiliProviderGrpcService {
    async fn parse(
        &self,
        request: Request<ParseRequest>,
    ) -> Result<Response<ParseResponse>, Status> {
        let user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?
            .clone();
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili parse request: url={}", req.url);
        let instance_name = extract_instance_name(&req.instance_name);

        self.api
            .parse(&user_ctx.user_id, req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn login_qr(
        &self,
        request: Request<LoginQrRequest>,
    ) -> Result<Response<QrCodeResponse>, Status> {
        let _user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili login QR request");
        let instance_name = extract_instance_name(&req.instance_name);

        self.api
            .login_qr(req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn check_qr(
        &self,
        request: Request<CheckQrRequest>,
    ) -> Result<Response<QrStatusResponse>, Status> {
        let user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?
            .clone();
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili check QR: {}", req.key);
        let instance_name = extract_instance_name(&req.instance_name);

        self.api
            .check_qr(&user_ctx.user_id, req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn get_captcha(
        &self,
        request: Request<GetCaptchaRequest>,
    ) -> Result<Response<CaptchaResponse>, Status> {
        let _user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili get captcha request");
        let instance_name = extract_instance_name(&req.instance_name);

        self.api
            .get_captcha(req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn send_sms(
        &self,
        request: Request<SendSmsRequest>,
    ) -> Result<Response<SendSmsResponse>, Status> {
        let _user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        let req = request.into_inner();
        let masked_phone = if req.phone.len() >= 4 {
            format!("****{}", &req.phone[req.phone.len() - 4..])
        } else {
            "****".to_string()
        };
        tracing::info!("gRPC Bilibili send SMS: phone={}", masked_phone);
        let instance_name = extract_instance_name(&req.instance_name);

        self.api
            .send_sms(req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn login_sms(
        &self,
        request: Request<LoginSmsRequest>,
    ) -> Result<Response<LoginSmsResponse>, Status> {
        let user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?
            .clone();
        let req = request.into_inner();
        let masked_phone = if req.phone.len() >= 4 {
            format!("****{}", &req.phone[req.phone.len() - 4..])
        } else {
            "****".to_string()
        };
        tracing::info!("gRPC Bilibili login SMS: phone={}", masked_phone);
        let instance_name = extract_instance_name(&req.instance_name);

        self.api
            .login_sms(&user_ctx.user_id, req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn get_user_info(
        &self,
        request: Request<UserInfoRequest>,
    ) -> Result<Response<UserInfoResponse>, Status> {
        let user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?
            .clone();
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili user info request");
        let instance_name = extract_instance_name(&req.instance_name);

        self.api
            .get_user_info(&user_ctx.user_id, req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn logout(
        &self,
        request: Request<LogoutRequest>,
    ) -> Result<Response<LogoutResponse>, Status> {
        let user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?
            .clone();
        let req = request.into_inner();
        tracing::info!("gRPC Bilibili logout request");

        self.api
            .logout(&user_ctx.user_id, req)
            .await
            .map(Response::new)
            .map_err(api_err)
    }
}
