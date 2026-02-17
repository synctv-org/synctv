//! Emby Provider gRPC Service Implementation

use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::http::AppState;
use crate::impls::EmbyApiImpl;
use crate::impls::providers::{extract_instance_name, get_provider_binds};

// Import generated proto types from synctv_proto
use crate::proto::providers::emby::emby_provider_service_server::EmbyProviderService;
use crate::proto::providers::emby::{LoginRequest, LoginResponse, ListRequest, ListResponse, GetMeRequest, GetMeResponse, LogoutRequest, LogoutResponse, GetBindsRequest, GetBindsResponse, BindInfo};

use crate::grpc::map_provider_error as api_err;

/// Emby Provider gRPC Service
///
/// Thin wrapper that delegates to `EmbyApiImpl`.
#[derive(Clone)]
pub struct EmbyProviderGrpcService {
    app_state: Arc<AppState>,
    api: EmbyApiImpl,
}

impl EmbyProviderGrpcService {
    #[must_use]
    pub fn new(app_state: Arc<AppState>) -> Self {
        let api = EmbyApiImpl::new(app_state.emby_provider.clone());
        Self { app_state, api }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl EmbyProviderService for EmbyProviderGrpcService {
    async fn login(&self, request: Request<LoginRequest>) -> Result<Response<LoginResponse>, Status> {
        let _user_ctx = request.extensions().get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        let req = request.into_inner();
        tracing::info!("gRPC Emby login request: host={}", req.host);
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
        tracing::info!("gRPC Emby list request: host={}, path={}", req.host, req.path);
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
        tracing::info!("gRPC Emby me request: host={}", req.host);
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
        tracing::info!("gRPC Emby logout request");

        self.api.logout(req)
            .await
            .map(Response::new)
            .map_err(api_err)
    }

    async fn get_binds(&self, request: Request<GetBindsRequest>) -> Result<Response<GetBindsResponse>, Status> {
        let auth_context = request.extensions().get::<crate::grpc::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;

        tracing::info!("gRPC Emby get binds request for user: {}", auth_context.user_id);

        let provider_binds = get_provider_binds(
            &self.app_state.user_provider_credential_repository,
            &auth_context.user_id,
            "emby",
            "emby_user_id",
        )
        .await
        .map_err(api_err)?;

        let binds = provider_binds
            .into_iter()
            .map(|b| BindInfo {
                id: b.id,
                host: b.host,
                user_id: b.label_value,
                created_at: b.created_at,
            })
            .collect();

        Ok(Response::new(GetBindsResponse { binds }))
    }
}
