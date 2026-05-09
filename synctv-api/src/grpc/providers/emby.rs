//! Emby Provider gRPC Service Implementation

use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::http::SharedApiRuntime;
use crate::impls::providers::extract_instance_name;
use crate::impls::EmbyApiImpl;
use crate::impls::{EndpointRateLimitCategory, RequestExecutor};
use synctv_core::Config;

// Import generated proto types from synctv_proto
use crate::proto::providers::emby::emby_provider_service_server::EmbyProviderService;
use crate::proto::providers::emby::{
    GetBindsRequest, GetBindsResponse, GetMeRequest, GetMeResponse, ListRequest, ListResponse,
    LoginRequest, LoginResponse, LogoutRequest, LogoutResponse,
};

use crate::grpc::map_api_error;
/// Emby Provider gRPC Service
///
/// Thin wrapper that delegates to `EmbyApiImpl`.
#[derive(Clone)]
pub struct EmbyProviderGrpcService {
    api: Arc<EmbyApiImpl>,
    request_executor: Arc<RequestExecutor>,
    config: Arc<Config>,
}

impl EmbyProviderGrpcService {
    #[must_use]
    pub fn new(
        shared_api_runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            api: shared_api_runtime.emby_api.clone(),
            request_executor,
            config,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl EmbyProviderService for EmbyProviderGrpcService {
    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        tracing::info!("gRPC Emby login request: host={}", req.host);
        let instance_name = extract_instance_name(&req.instance_name);
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, authenticated| async move {
                    api.login_with_context(
                        &authenticated.user_id,
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn list(&self, request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        tracing::info!(
            "gRPC Emby list request: server_id={}, path={}",
            req.server_id,
            req.path
        );
        let instance_name = extract_instance_name(&req.instance_name);
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |request_control, authenticated| async move {
                    api.list_with_context(
                        &authenticated.user_id,
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(crate::impls::ApiError::from)
                },
            )
            .await
            .map(Response::new)
            .map_err(crate::grpc::map_api_error)
    }

    async fn get_me(
        &self,
        request: Request<GetMeRequest>,
    ) -> Result<Response<GetMeResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        tracing::info!("gRPC Emby me request: server_id={}", req.server_id);
        let instance_name = extract_instance_name(&req.instance_name);
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |request_control, authenticated| async move {
                    api.get_me_with_context(
                        &authenticated.user_id,
                        req,
                        instance_name.as_deref(),
                        Some(&request_control),
                    )
                    .await
                    .map_err(crate::impls::ApiError::from)
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
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.into_inner();
        tracing::info!("gRPC Emby logout request");
        let api = self.api.clone();

        self.request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |authenticated| async move {
                    api.logout(&authenticated.user_id, req)
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
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        );
        let req = request.get_ref();
        let instance_name = extract_instance_name(&req.instance_name);
        let api = self.api.clone();
        let provider_binds = self
            .request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    api.get_binds(&authenticated.user_id, instance_name.as_deref())
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(provider_binds))
    }
}

#[cfg(test)]
mod tests {
    use crate::grpc::map_api_error;
    use crate::impls::ApiError;

    #[test]
    fn test_provider_binds_backend_outage_maps_to_unavailable() {
        let status = map_api_error(ApiError::ServiceUnavailable(
            "Provider bind information is temporarily unavailable".into(),
        ));
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(
            status.message(),
            "Provider bind information is temporarily unavailable"
        );
    }
}
