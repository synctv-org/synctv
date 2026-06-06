//! Alist Provider gRPC Service Implementation

use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::http::SharedApiRuntime;
use crate::impls::AlistApiImpl;
use crate::impls::{EndpointRateLimitCategory, RequestExecutor};
use synctv_core::Config;

// Import generated proto types from synctv_proto
use crate::proto::providers::alist::alist_provider_service_server::AlistProviderService;
use crate::proto::providers::alist::{
    GetBindsRequest, GetBindsResponse, GetMeRequest, GetMeResponse, ListRequest, ListResponse,
    LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, SearchRequest, SearchResponse,
};

use crate::grpc::map_api_error;
/// Alist Provider gRPC Service
///
/// Thin wrapper that delegates to `AlistApiImpl`.
#[derive(Clone)]
pub struct AlistProviderGrpcService {
    api: Arc<AlistApiImpl>,
    request_executor: Arc<RequestExecutor>,
    config: Arc<Config>,
}

impl AlistProviderGrpcService {
    #[must_use]
    pub fn new(
        shared_api_runtime: &Arc<SharedApiRuntime>,
        request_executor: Arc<RequestExecutor>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            api: shared_api_runtime.alist_api.clone(),
            request_executor,
            config,
        }
    }
}

#[tonic::async_trait]
// Tonic generated service traits require `Result<Response<_>, tonic::Status>`.
// Provider business logic stays in `AlistApiImpl`.
#[allow(clippy::result_large_err)]
impl AlistProviderService for AlistProviderGrpcService {
    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        )?;
        let req = request.into_inner();
        tracing::info!("gRPC Alist login request: host={}", req.host);
        let instance_name = super::provider_instance_name(&req.instance_name)?;
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
        )?;
        let req = request.into_inner();
        tracing::info!(
            "gRPC Alist list request: server_id={}, path={}",
            req.server_id,
            req.path
        );
        let instance_name = super::provider_instance_name(&req.instance_name)?;
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

    async fn search(
        &self,
        request: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        let metadata = crate::grpc::request_metadata(
            &request,
            &self.config,
            Some(crate::grpc::grpc_unary_request_timeout()),
        )?;
        let req = request.into_inner();
        tracing::info!(
            "gRPC Alist search request: server_id={}, parent={}, keywords={}",
            req.server_id,
            req.parent,
            req.keywords
        );
        let instance_name = super::provider_instance_name(&req.instance_name)?;
        let api = self.api.clone();

        self.request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |request_control, authenticated| async move {
                    api.search_with_context(
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
        )?;
        let req = request.into_inner();
        tracing::info!("gRPC Alist me request: server_id={}", req.server_id);
        let instance_name = super::provider_instance_name(&req.instance_name)?;
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
        )?;
        let req = request.into_inner();
        tracing::info!("gRPC Alist logout request");
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
        )?;
        let req = request.get_ref();
        let instance_name = super::provider_instance_name(&req.instance_name)?;
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
