use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::api_runtime::SharedApiRuntime;
use crate::grpc::map_api_error;
use crate::impls::{EndpointRateLimitCategory, RequestExecutor};
use synctv_core::Config;

use synctv_proto::providers::rtmp::rtmp_provider_service_server::RtmpProviderService;
use synctv_proto::providers::rtmp::{
    CreatePublishKeyRequest, CreatePublishKeyResponse, GetStreamInfoRequest, GetStreamInfoResponse,
};

#[derive(Clone)]
pub struct RtmpProviderGrpcService {
    api: Arc<crate::impls::ClientApiImpl>,
    request_executor: Arc<RequestExecutor>,
    config: Arc<Config>,
}

impl RtmpProviderGrpcService {
    #[must_use]
    pub fn new(
        shared_api_runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            api: shared_api_runtime.client_api.clone(),
            request_executor,
            config,
        }
    }
}

#[tonic::async_trait]
impl RtmpProviderService for RtmpProviderGrpcService {
    async fn create_publish_key(
        &self,
        request: Request<CreatePublishKeyRequest>,
    ) -> Result<Response<CreatePublishKeyResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();

        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    api.create_publish_key(&authenticated.user_id, req).await
                },
            )
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn get_stream_info(
        &self,
        request: Request<GetStreamInfoRequest>,
    ) -> Result<Response<GetStreamInfoResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let api = self.api.clone();

        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    api.get_stream_info(
                        &authenticated.user_id,
                        req.room_id.as_str(),
                        req.media_id.as_str(),
                    )
                    .await
                },
            )
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }
}
