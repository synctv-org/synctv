//! Twitch provider gRPC transport adapter.

use std::sync::Arc;

use synctv_proto::providers::twitch::twitch_provider_service_server::TwitchProviderService;
use synctv_proto::providers::twitch::{
    BindRequest, BindResponse, GetBindsRequest, GetBindsResponse, ListCategoryStreamsRequest,
    ListCategoryStreamsResponse, ListChannelItemsRequest, ListChannelItemsResponse,
    ListFollowedLiveRequest, ListFollowedLiveResponse, ListScheduleRequest, ListScheduleResponse,
    ListTopCategoriesRequest, ListTopCategoriesResponse, ResolveRequest, ResolveResponse,
    SearchLiveChannelsRequest, SearchLiveChannelsResponse, UnbindRequest, UnbindResponse,
};
use tonic::{Request, Response, Status};

use crate::api_runtime::SharedApiRuntime;
use crate::impls::{EndpointRateLimitCategory, RequestExecutor, TwitchApiImpl};

#[derive(Clone)]
pub struct TwitchProviderGrpcService {
    api: Arc<TwitchApiImpl>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

impl TwitchProviderGrpcService {
    #[must_use]
    pub fn new(
        shared_api_runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<crate::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            api: shared_api_runtime.twitch_api.clone(),
            request_executor,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl TwitchProviderService for TwitchProviderGrpcService {
    async fn bind(&self, request: Request<BindRequest>) -> Result<Response<BindResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |authenticated| async move {
                    api.bind(authenticated.user_id, req, instance_name.as_deref())
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
        let instance_name = super::provider_instance_name(&request.get_ref().instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    api.get_binds(authenticated.user_id, instance_name.as_deref())
                        .await
                        .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn unbind(
        &self,
        request: Request<UnbindRequest>,
    ) -> Result<Response<UnbindResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |authenticated| async move {
                    api.unbind(authenticated.user_id, req)
                        .await
                        .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn resolve(
        &self,
        request: Request<ResolveRequest>,
    ) -> Result<Response<ResolveResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    api.resolve(authenticated.user_id, req, instance_name.as_deref())
                        .await
                        .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn list_channel_items(
        &self,
        request: Request<ListChannelItemsRequest>,
    ) -> Result<Response<ListChannelItemsResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    api.list_channel_items(authenticated.user_id, req, instance_name.as_deref())
                        .await
                        .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn list_followed_live(
        &self,
        request: Request<ListFollowedLiveRequest>,
    ) -> Result<Response<ListFollowedLiveResponse>, Status> {
        self.execute_read(request, |api, user_id, req, instance_name| async move {
            api.list_followed_live(user_id, req, instance_name.as_deref())
                .await
        })
        .await
    }

    async fn list_category_streams(
        &self,
        request: Request<ListCategoryStreamsRequest>,
    ) -> Result<Response<ListCategoryStreamsResponse>, Status> {
        self.execute_read(request, |api, user_id, req, instance_name| async move {
            api.list_category_streams(user_id, req, instance_name.as_deref())
                .await
        })
        .await
    }

    async fn list_top_categories(
        &self,
        request: Request<ListTopCategoriesRequest>,
    ) -> Result<Response<ListTopCategoriesResponse>, Status> {
        self.execute_read(request, |api, user_id, req, instance_name| async move {
            api.list_top_categories(user_id, req, instance_name.as_deref())
                .await
        })
        .await
    }

    async fn search_live_channels(
        &self,
        request: Request<SearchLiveChannelsRequest>,
    ) -> Result<Response<SearchLiveChannelsResponse>, Status> {
        self.execute_read(request, |api, user_id, req, instance_name| async move {
            api.search_live_channels(user_id, req, instance_name.as_deref())
                .await
        })
        .await
    }

    async fn list_schedule(
        &self,
        request: Request<ListScheduleRequest>,
    ) -> Result<Response<ListScheduleResponse>, Status> {
        self.execute_read(request, |api, user_id, req, instance_name| async move {
            api.list_schedule(user_id, req, instance_name.as_deref())
                .await
        })
        .await
    }
}

impl TwitchProviderGrpcService {
    async fn execute_read<Req, Res, F, Fut>(
        &self,
        request: Request<Req>,
        call: F,
    ) -> Result<Response<Res>, Status>
    where
        Req: TwitchInstanceRequest + Send + 'static,
        Res: Send + 'static,
        F: FnOnce(Arc<TwitchApiImpl>, synctv_core::models::UserId, Req, Option<String>) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = Result<Res, synctv_core::provider::ProviderError>>
            + Send
            + 'static,
    {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let instance_name = super::provider_instance_name(req.instance_name())?;
        let api = self.api.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    call(api, authenticated.user_id, req, instance_name)
                        .await
                        .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }
}

trait TwitchInstanceRequest {
    fn instance_name(&self) -> &str;
}

macro_rules! impl_twitch_instance_request {
    ($($request:ty),+ $(,)?) => {
        $(
            impl TwitchInstanceRequest for $request {
                fn instance_name(&self) -> &str {
                    &self.instance_name
                }
            }
        )+
    };
}

impl_twitch_instance_request!(
    ListFollowedLiveRequest,
    ListCategoryStreamsRequest,
    ListTopCategoriesRequest,
    SearchLiveChannelsRequest,
    ListScheduleRequest,
);
