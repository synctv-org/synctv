//! Huya provider gRPC transport adapter.

use std::sync::Arc;

use synctv_proto::providers::huya::huya_provider_service_server::HuyaProviderService;
use synctv_proto::providers::huya::{ResolveRequest, ResolveResponse};
use tonic::{Request, Response, Status};

use synctv_api_common::api_runtime::SharedApiRuntime;
use synctv_api_common::impls::{EndpointRateLimitCategory, RequestExecutor};

#[derive(Clone)]
pub struct HuyaProviderGrpcService {
    service: Arc<synctv_core::service::HuyaPlaybackProviderService>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl HuyaProviderGrpcService {
    #[must_use]
    pub fn new(
        shared_api_runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            service: shared_api_runtime.huya_playback_provider_service.clone(),
            request_executor,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl HuyaProviderService for HuyaProviderGrpcService {
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
                        .map(synctv_api_common::providers::huya::resolve_response)
                        .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }
}
