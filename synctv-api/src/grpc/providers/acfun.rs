use std::sync::Arc;

use synctv_proto::providers::acfun::ac_fun_provider_service_server::AcFunProviderService;
use synctv_proto::providers::acfun::{ResolveRequest, ResolveResponse};
use tonic::{Request, Response, Status};

use crate::api_runtime::SharedApiRuntime;
use crate::impls::{EndpointRateLimitCategory, RequestExecutor};

#[derive(Clone)]
pub struct AcFunProviderGrpcService {
    service: Arc<synctv_core::service::AcFunPlaybackProviderService>,
    request_executor: Arc<RequestExecutor>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

impl AcFunProviderGrpcService {
    #[must_use]
    pub fn new(
        runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        runtime_settings: Arc<crate::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            service: runtime.acfun_playback_provider_service.clone(),
            request_executor,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl AcFunProviderService for AcFunProviderGrpcService {
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
                        .map(crate::impls::providers::acfun::resolve_response)
                        .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }
}
