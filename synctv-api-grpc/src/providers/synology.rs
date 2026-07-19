use std::sync::Arc;

use synctv_proto::providers::synology::synology_provider_service_server::SynologyProviderService;
use synctv_proto::providers::synology::{
    GetBindsRequest, GetBindsResponse, ListEpisodesRequest, ListFilesRequest, ListFilesResponse,
    ListHomeVideosRequest, ListLibrariesRequest, ListLibrariesResponse, ListMoviesRequest,
    ListTvRecordingsRequest, ListTvShowsRequest, ListVideoItemsResponse, LoginRequest,
    LoginResponse, LogoutRequest, LogoutResponse,
};
use tonic::{Request, Response, Status};

use synctv_api_common::api_runtime::SharedApiRuntime;
use synctv_api_common::impls::{EndpointRateLimitCategory, RequestExecutor};
use synctv_api_common::providers::SynologyApiImpl;

#[derive(Clone)]
pub struct SynologyProviderGrpcService {
    api: Arc<SynologyApiImpl>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl SynologyProviderGrpcService {
    #[must_use]
    pub fn new(
        shared: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            api: shared.synology_api.clone(),
            request_executor,
            runtime_settings,
        }
    }
}

macro_rules! impl_synology_provider_service {
    ($(($name:ident, $request:ty, $response:ty, $method:ident)),+ $(,)?) => {
        #[tonic::async_trait]
        #[allow(clippy::result_large_err)]
        impl SynologyProviderService for SynologyProviderGrpcService {
            async fn login(
                &self,
                request: Request<LoginRequest>,
            ) -> Result<Response<LoginResponse>, Status> {
                let metadata =
                    super::provider_request_metadata(&request, &self.runtime_settings)?;
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

            $(
                async fn $name(
                    &self,
                    request: Request<$request>,
                ) -> Result<Response<$response>, Status> {
                    let metadata =
                        super::provider_request_metadata(&request, &self.runtime_settings)?;
                    let req = request.into_inner();
                    let instance = super::provider_instance_name(&req.instance_name)?;
                    let api = self.api.clone();
                    self.request_executor
                        .execute_user(
                            &metadata,
                            EndpointRateLimitCategory::Read,
                            move |auth| async move {
                                api.$method(auth.user_id, req, instance.as_deref())
                                    .await
                                    .map_err(synctv_api_common::impls::ApiError::from)
                            },
                        )
                        .await
                        .map(Response::new)
                        .map_err(crate::grpc::map_api_error)
                }
            )+

            async fn logout(
                &self,
                request: Request<LogoutRequest>,
            ) -> Result<Response<LogoutResponse>, Status> {
                let metadata =
                    super::provider_request_metadata(&request, &self.runtime_settings)?;
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
                let metadata =
                    super::provider_request_metadata(&request, &self.runtime_settings)?;
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
    };
}

impl_synology_provider_service!(
    (list_files, ListFilesRequest, ListFilesResponse, list_files),
    (
        list_libraries,
        ListLibrariesRequest,
        ListLibrariesResponse,
        list_libraries
    ),
    (
        list_movies,
        ListMoviesRequest,
        ListVideoItemsResponse,
        list_movies
    ),
    (
        list_tv_shows,
        ListTvShowsRequest,
        ListVideoItemsResponse,
        list_tv_shows
    ),
    (
        list_episodes,
        ListEpisodesRequest,
        ListVideoItemsResponse,
        list_episodes
    ),
    (
        list_home_videos,
        ListHomeVideosRequest,
        ListVideoItemsResponse,
        list_home_videos
    ),
    (
        list_tv_recordings,
        ListTvRecordingsRequest,
        ListVideoItemsResponse,
        list_tv_recordings
    ),
);
