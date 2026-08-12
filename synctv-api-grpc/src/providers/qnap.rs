use std::sync::Arc;

use synctv_proto::providers::qnap::qnap_provider_service_server::QnapProviderService;
use synctv_proto::providers::qnap::{
    GetBindsRequest, GetBindsResponse, GetCapabilitiesRequest, GetCapabilitiesResponse,
    ListRequest, ListResponse, LoginRequest, LoginResponse, LogoutRequest, LogoutResponse,
};
use tonic::{Request, Response, Status};

use synctv_api_common::api_runtime::SharedApiRuntime;
use synctv_api_common::impls::{EndpointRateLimitCategory, RequestExecutor};
use synctv_api_common::providers::QnapApiImpl;

#[derive(Clone)]
pub struct QnapProviderGrpcService {
    api: Arc<QnapApiImpl>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl QnapProviderGrpcService {
    #[must_use]
    pub fn new(
        shared: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            api: shared.qnap_api.clone(),
            request_executor,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl QnapProviderService for QnapProviderGrpcService {
    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let instance = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |auth| async move {
                    api.login(auth.user_id(), req, instance.as_deref())
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
        let instance = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |auth| async move {
                    api.list(auth.user_id(), req, instance.as_deref())
                        .await
                        .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn get_capabilities(
        &self,
        request: Request<GetCapabilitiesRequest>,
    ) -> Result<Response<GetCapabilitiesResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let instance = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |auth| async move {
                    api.capabilities(auth.user_id(), req, instance.as_deref())
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
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |auth| async move {
                    api.logout(auth.user_id(), req)
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
        let req = request.into_inner();
        let instance = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |auth| async move {
                    api.binds(auth.user_id(), instance.as_deref())
                        .await
                        .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }
}
