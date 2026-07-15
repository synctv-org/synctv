use std::sync::Arc;

use synctv_proto::providers::douyu::douyu_provider_service_server::DouyuProviderService;
use synctv_proto::providers::douyu::{ResolveRequest, ResolveResponse};
use tonic::{Request, Response, Status};

use crate::api_runtime::SharedApiRuntime;
use crate::impls::{EndpointRateLimitCategory, RequestExecutor};

#[derive(Clone)]
pub struct DouyuProviderGrpcService {
    service: Arc<synctv_core::service::DouyuPlaybackProviderService>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

impl DouyuProviderGrpcService {
    pub fn new(
        runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<crate::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            service: runtime.douyu_playback_provider_service.clone(),
            request_executor,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl DouyuProviderService for DouyuProviderGrpcService {
    async fn resolve(
        &self,
        request: Request<ResolveRequest>,
    ) -> Result<Response<ResolveResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let service = self.service.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |_authenticated| async move {
                    service
                        .resolve_resource(&req.resource)
                        .await
                        .map(crate::impls::providers::douyu::resolve_response)
                        .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }
}
