use std::sync::Arc;

use synctv_proto::providers::cctv::cctv_provider_service_server::CctvProviderService;
use synctv_proto::providers::cctv::{ResolveRequest, ResolveResponse};
use tonic::{Request, Response, Status};

use synctv_api_common::api_runtime::SharedApiRuntime;
use synctv_api_common::impls::{EndpointRateLimitCategory, RequestExecutor};

#[derive(Clone)]
pub struct CctvProviderGrpcService {
    service: Arc<synctv_core::service::CctvPlaybackProviderService>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl CctvProviderGrpcService {
    #[must_use]
    pub fn new(
        runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            service: runtime.cctv_playback_provider_service.clone(),
            request_executor,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl CctvProviderService for CctvProviderGrpcService {
    async fn resolve(
        &self,
        request: Request<ResolveRequest>,
    ) -> Result<Response<ResolveResponse>, Status> {
        let metadata = super::provider_request_metadata(&request, &self.runtime_settings)?;
        let resource = request.into_inner().resource;
        let service = self.service.clone();
        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |_authenticated| async move {
                    service
                        .resolve_resource(&resource)
                        .await
                        .map(|media| {
                            synctv_api_common::providers::cctv::resolve_response(media, resource)
                        })
                        .map_err(synctv_api_common::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }
}
