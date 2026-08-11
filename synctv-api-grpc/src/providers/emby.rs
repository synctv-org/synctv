//! Emby Provider gRPC Service Implementation

use std::sync::Arc;
use tonic::{Request, Response, Status};

use synctv_api_common::api_runtime::SharedApiRuntime;
use synctv_api_common::impls::{EndpointRateLimitCategory, RequestExecutor};
use synctv_api_common::providers::EmbyApiImpl;

// Import generated proto types from synctv_proto
use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderService;
use synctv_proto::providers::emby::{
    GetBindsRequest, GetBindsResponse, GetMeRequest, GetMeResponse, ListRequest, ListResponse,
    LoginRequest, LoginResponse, LogoutRequest, LogoutResponse,
};

use crate::grpc::map_api_error;
/// Emby Provider gRPC Service
///
/// Thin wrapper that delegates to `EmbyApiImpl`.
#[derive(Clone)]
pub struct EmbyProviderGrpcService {
    api: Arc<EmbyApiImpl>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl EmbyProviderGrpcService {
    #[must_use]
    pub fn new(
        shared_api_runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            api: shared_api_runtime.emby_api.clone(),
            request_executor,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
// Tonic generated service traits require `Result<Response<_>, tonic::Status>`.
// Provider business logic stays in `EmbyApiImpl`.
#[allow(clippy::result_large_err)]
impl EmbyProviderService for EmbyProviderGrpcService {
    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        tracing::info!("gRPC Emby login request: host={}", req.host);
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, authenticated| async move {
                    api.login_with_context(
                        &authenticated.user_id,
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn list(&self, request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        tracing::info!(
            "gRPC Emby list request: server_id={}, mode={:?}, target_id={}",
            req.server_id,
            req.mode(),
            req.target_id,
        );
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |request_control, authenticated| async move {
                    api.list_with_context(
                        &authenticated.user_id,
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn get_me(
        &self,
        request: Request<GetMeRequest>,
    ) -> Result<Response<GetMeResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        tracing::info!("gRPC Emby me request: server_id={}", req.server_id);
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |request_control, authenticated| async move {
                    api.get_me_with_context(
                        &authenticated.user_id,
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(synctv_api_common::impls::ApiError::from)
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
        tracing::info!("gRPC Emby logout request");
        let api = self.api.clone();

        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |authenticated| async move {
                    api.logout(&authenticated.user_id, req)
                        .await
                        .map_err(synctv_api_common::impls::ApiError::from)
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
        let provider_binds = self
            .request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    api.get_binds(&authenticated.user_id, instance_name.as_deref())
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(provider_binds))
    }
}
