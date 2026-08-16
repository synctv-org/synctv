use futures::FutureExt;
use std::sync::Arc;
use synctv_api_common::api_runtime::SharedApiRuntime;
use synctv_api_common::impls::{EndpointRateLimitCategory, RequestExecutor};
use synctv_api_common::providers::NextcloudApiImpl;
use synctv_proto::providers::nextcloud::nextcloud_provider_service_server::NextcloudProviderService;
use synctv_proto::providers::nextcloud::{
    GetBindsRequest, GetBindsResponse, GetPreviewRequest, ListFavoritesRequest, ListRequest,
    ListResponse, LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, PollLoginFlowRequest,
    StartLoginFlowRequest, StartLoginFlowResponse,
};
use tonic::{Request, Response, Status};

#[derive(Clone)]
pub struct NextcloudProviderGrpcService {
    api: Arc<NextcloudApiImpl>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    resource_state: Arc<super::playback_provider::PlaybackProviderGrpcState>,
}

impl NextcloudProviderGrpcService {
    #[must_use]
    pub fn new(
        shared: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
        resource_state: Arc<super::playback_provider::PlaybackProviderGrpcState>,
    ) -> Self {
        Self {
            api: shared.nextcloud_api.clone(),
            request_executor,
            runtime_settings,
            resource_state,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl NextcloudProviderService for NextcloudProviderGrpcService {
    type GetPreviewStream = super::ProviderResourceResponseStream;

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
    async fn start_login_flow(
        &self,
        request: Request<StartLoginFlowRequest>,
    ) -> Result<Response<StartLoginFlowResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |_auth| async move {
                    api.start_login_flow(req)
                        .await
                        .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }
    async fn poll_login_flow(
        &self,
        request: Request<PollLoginFlowRequest>,
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
                    api.poll_login_flow(auth.user_id(), req, instance.as_deref())
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
    async fn list_favorites(
        &self,
        request: Request<ListFavoritesRequest>,
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
                    api.list_favorites(auth.user_id(), req, instance.as_deref())
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

    async fn get_preview(
        &self,
        request: Request<GetPreviewRequest>,
    ) -> Result<Response<Self::GetPreviewStream>, Status> {
        let metadata = super::provider_stream_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let api = self.api.clone();

        super::execute_provider_resource_stream(
            self.resource_state.clone(),
            metadata,
            move |_control, authenticated| {
                async move { api.preview_action(authenticated.user_id(), req).await }.boxed()
            },
        )
        .await
    }
}
