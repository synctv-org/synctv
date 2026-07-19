use std::sync::Arc;

use synctv_proto::providers::seafile::seafile_provider_service_server::SeafileProviderService;
use synctv_proto::providers::seafile::{
    GetBindsRequest, GetBindsResponse, ListRepositoriesRequest, ListRequest, ListResponse,
    ListStarredRequest, LoginRequest, LoginResponse, LogoutRequest, LogoutResponse,
    UnlockLibraryRequest, UnlockLibraryResponse,
};
use tonic::{Request, Response, Status};

use synctv_api_common::api_runtime::SharedApiRuntime;
use synctv_api_common::impls::{EndpointRateLimitCategory, RequestExecutor};
use synctv_api_common::providers::SeafileApiImpl;

#[derive(Clone)]
pub struct SeafileProviderGrpcService {
    api: Arc<SeafileApiImpl>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl SeafileProviderGrpcService {
    #[must_use]
    pub fn new(
        shared: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            api: shared.seafile_api.clone(),
            request_executor,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl SeafileProviderService for SeafileProviderGrpcService {
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
                    api.login(auth.user_id, req, instance.as_deref())
                        .await
                        .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn unlock_library(
        &self,
        request: Request<UnlockLibraryRequest>,
    ) -> Result<Response<UnlockLibraryResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let instance = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |auth| async move {
                    api.unlock_library(auth.user_id, req, instance.as_deref())
                        .await
                        .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn list_repositories(
        &self,
        request: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let instance = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |auth| async move {
                    api.list_repositories(auth.user_id, req, instance.as_deref())
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
                    api.list(auth.user_id, req, instance.as_deref())
                        .await
                        .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn list_starred(
        &self,
        request: Request<ListStarredRequest>,
    ) -> Result<Response<ListResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let instance = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |auth| async move {
                    api.list_starred(auth.user_id, req, instance.as_deref())
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
        let instance = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |auth| async move {
                    api.logout(auth.user_id, req, instance.as_deref())
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
                    api.binds(auth.user_id, instance.as_deref())
                        .await
                        .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }
}
