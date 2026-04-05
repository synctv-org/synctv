//! Alist Provider gRPC Service Implementation

use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::http::AppState;
use crate::impls::providers::{extract_instance_name, get_provider_binds};
use crate::impls::AlistApiImpl;

// Import generated proto types from synctv_proto
use crate::proto::providers::alist::alist_provider_service_server::AlistProviderService;
use crate::proto::providers::alist::{
    BindInfo, GetBindsRequest, GetBindsResponse, GetMeRequest, GetMeResponse, ListRequest,
    ListResponse, LoginRequest, LoginResponse, LogoutRequest, LogoutResponse,
};

use crate::grpc::map_api_error;
use crate::grpc::map_provider_error as api_err;

/// Alist Provider gRPC Service
///
/// Thin wrapper that delegates to `AlistApiImpl`.
#[derive(Clone)]
pub struct AlistProviderGrpcService {
    app_state: Arc<AppState>,
    api: AlistApiImpl,
}

impl AlistProviderGrpcService {
    #[must_use]
    pub fn new(app_state: Arc<AppState>) -> Self {
        let api = AlistApiImpl::new(
            app_state.providers.alist.clone(),
            app_state.user_provider_credential_repository.clone(),
        );
        Self { app_state, api }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl AlistProviderService for AlistProviderGrpcService {
    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?
            .clone();
        let req = request.into_inner();
        tracing::info!("gRPC Alist login request: host={}", req.host);
        let instance_name = extract_instance_name(&req.instance_name);

        self.api
            .login(&user_ctx.user_id, req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn list(&self, request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?
            .clone();
        let req = request.into_inner();
        tracing::info!(
            "gRPC Alist list request: server_id={}, path={}",
            req.server_id,
            req.path
        );
        let instance_name = extract_instance_name(&req.instance_name);

        self.api
            .list(&user_ctx.user_id, req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn get_me(
        &self,
        request: Request<GetMeRequest>,
    ) -> Result<Response<GetMeResponse>, Status> {
        let user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?
            .clone();
        let req = request.into_inner();
        tracing::info!("gRPC Alist me request: server_id={}", req.server_id);
        let instance_name = extract_instance_name(&req.instance_name);

        self.api
            .get_me(&user_ctx.user_id, req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn logout(
        &self,
        request: Request<LogoutRequest>,
    ) -> Result<Response<LogoutResponse>, Status> {
        let user_ctx = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?
            .clone();
        let req = request.into_inner();
        tracing::info!("gRPC Alist logout request");

        self.api
            .logout(&user_ctx.user_id, req)
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn get_binds(
        &self,
        request: Request<GetBindsRequest>,
    ) -> Result<Response<GetBindsResponse>, Status> {
        let auth_context = request
            .extensions()
            .get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;

        tracing::info!(
            "gRPC Alist get binds request for user: {}",
            auth_context.user_id
        );

        let provider_binds = get_provider_binds(
            &self.app_state.user_provider_credential_repository,
            &auth_context.user_id,
            synctv_core::provider::AlistProvider::NAME,
            "username",
        )
        .await
        .map_err(map_api_error)?;

        let binds = provider_binds
            .into_iter()
            .map(|b| BindInfo {
                id: b.id,
                host: b.host,
                username: b.label_value,
                created_at: b.created_at,
            })
            .collect();

        Ok(Response::new(GetBindsResponse { binds }))
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
