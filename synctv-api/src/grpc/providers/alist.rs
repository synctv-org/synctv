//! Alist Provider gRPC Service Implementation

use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::http::AppState;
use crate::impls::AlistApiImpl;
use crate::impls::providers::{extract_instance_name, get_provider_binds};

// Import generated proto types from synctv_proto
use crate::proto::providers::alist::alist_provider_service_server::AlistProviderService;
use crate::proto::providers::alist::{LoginRequest, LoginResponse, ListRequest, ListResponse, GetMeRequest, GetMeResponse, LogoutRequest, LogoutResponse, GetBindsRequest, GetBindsResponse, BindInfo};

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
        let api = AlistApiImpl::new(app_state.alist_provider.clone());
        Self { app_state, api }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl AlistProviderService for AlistProviderGrpcService {
    async fn login(&self, request: Request<LoginRequest>) -> Result<Response<LoginResponse>, Status> {
        let _user_ctx = request.extensions().get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        let req = request.into_inner();
        tracing::info!("gRPC Alist login request: host={}", req.host);
        let instance_name = extract_instance_name(&req.instance_name);

        self.api.login(req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn list(&self, request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let _user_ctx = request.extensions().get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        let req = request.into_inner();
        tracing::info!("gRPC Alist list request: host={}, path={}", req.host, req.path);
        let instance_name = extract_instance_name(&req.instance_name);

        self.api.list(req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn get_me(&self, request: Request<GetMeRequest>) -> Result<Response<GetMeResponse>, Status> {
        let _user_ctx = request.extensions().get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        let req = request.into_inner();
        tracing::info!("gRPC Alist me request: host={}", req.host);
        let instance_name = extract_instance_name(&req.instance_name);

        self.api.get_me(req, instance_name.as_deref())
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn logout(&self, request: Request<LogoutRequest>) -> Result<Response<LogoutResponse>, Status> {
        let _user_ctx = request.extensions().get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        let req = request.into_inner();
        tracing::info!("gRPC Alist logout request");

        self.api.logout(req)
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn get_binds(&self, request: Request<GetBindsRequest>) -> Result<Response<GetBindsResponse>, Status> {
        let auth_context = request.extensions().get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;

        tracing::info!("gRPC Alist get binds request for user: {}", auth_context.user_id);

        let provider_binds = get_provider_binds(
            &self.app_state.user_provider_credential_repository,
            &auth_context.user_id,
            "alist",
            "username",
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to query provider binds: {e}");
            tonic::Status::internal("Internal error")
        })?;

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
