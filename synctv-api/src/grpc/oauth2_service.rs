//! gRPC OAuth2 service implementation
//!
//! # HTTP vs gRPC endpoint differences
//!
//! Both transports expose the same logical operations via `OAuth2ApiImpl`, but
//! differ in how they handle the OAuth2 redirect flow:
//!
//! - **HTTP** (`GET /api/oauth2/:provider/authorize`): The provider name is a
//!   URL path segment and the redirect URL is a query parameter. This is the
//!   natural fit for browser-initiated OAuth2 flows.
//!
//! - **gRPC** (`GetAuthorizationUrl`): The provider name and redirect URL are
//!   fields in the `GetAuthorizationUrlRequest` message. Mobile/desktop clients
//!   that already use gRPC for all other calls can stay on a single transport.
//!
//! Both transports delegate to the same `OAuth2ApiImpl` implementation, so
//! business logic (token exchange, provider linking) is identical.

use synctv_proto::client::{
    o_auth2_service_server::OAuth2Service,
    GetAuthorizationUrlRequest, GetAuthorizationUrlResponse,
    GetAuthorizationUrlForBindRequest, GetAuthorizationUrlForBindResponse,
    ExchangeAuthorizationCodeRequest, ExchangeAuthorizationCodeResponse,
    ListAvailableProvidersRequest, ListAvailableProvidersResponse,
    UnlinkProviderRequest, UnlinkProviderResponse,
    GetLinkedProvidersRequest, GetLinkedProvidersResponse,
};
use tonic::{Request, Response, Status};
use tracing::{debug, error, info};

use synctv_core::models::UserId;
use std::sync::Arc;

/// Map a typed `ApiError` to a gRPC `Status` with guaranteed-correct
/// status code mapping (no keyword-based heuristics).
fn impls_err_to_status(err: crate::impls::ApiError) -> Status {
    use crate::impls::ErrorKind;
    let msg = err.to_string();
    match err.classify() {
        ErrorKind::NotFound => Status::not_found(msg),
        ErrorKind::Unauthenticated => Status::unauthenticated(msg),
        ErrorKind::PermissionDenied => Status::permission_denied(msg),
        ErrorKind::AlreadyExists => Status::already_exists(msg),
        ErrorKind::InvalidArgument => Status::invalid_argument(msg),
        ErrorKind::Internal => {
            tracing::error!("Internal error: {msg}");
            Status::internal("Internal error")
        }
    }
}

/// gRPC OAuth2 service
pub struct OAuth2GrpcService {
    oauth2_api: Arc<crate::impls::OAuth2ApiImpl>,
}

impl OAuth2GrpcService {
    pub fn new(oauth2_api: Arc<crate::impls::OAuth2ApiImpl>) -> Self {
        Self { oauth2_api }
    }

    /// Extract `user_id` from `UserContext` (injected by `inject_user` interceptor).
    fn get_user_id(&self, request: &Request<impl std::fmt::Debug>) -> Result<UserId, Status> {
        let user_context = request
            .extensions()
            .get::<super::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;

        Ok(UserId::from_string(user_context.user_id.clone()))
    }
}

#[tonic::async_trait]
impl OAuth2Service for OAuth2GrpcService {
    /// Get authorization URL for OAuth2 login flow
    async fn get_authorization_url(
        &self,
        request: Request<GetAuthorizationUrlRequest>,
    ) -> Result<Response<GetAuthorizationUrlResponse>, Status> {
        let req = request.into_inner();
let (authorization_url, state) = self.oauth2_api
            .get_authorization_url(&req.provider, Some(req.redirect_url).filter(|s| !s.is_empty()))
            .await
            .map_err(|e| {
                error!("Failed to get authorization URL: {}", e);
                impls_err_to_status(e)
            })?;

        debug!("Generated OAuth2 authorization URL for provider: {}", req.provider);

        Ok(Response::new(GetAuthorizationUrlResponse {
            authorization_url,
            state,
        }))
    }

    /// Get authorization URL for binding OAuth2 provider to existing user account
    async fn get_authorization_url_for_bind(
        &self,
        request: Request<GetAuthorizationUrlForBindRequest>,
    ) -> Result<Response<GetAuthorizationUrlForBindResponse>, Status> {
        // Extract user_id from gRPC metadata (set by auth interceptor) before consuming request
        let user_id = self.get_user_id(&request)?;
        let req = request.into_inner();

        let (authorization_url, state) = self.oauth2_api
            .get_authorization_url_for_bind(&user_id, &req.provider, Some(req.redirect_url).filter(|s| !s.is_empty()))
            .await
            .map_err(|e| {
                error!("Failed to get authorization URL for bind: {}", e);
                impls_err_to_status(e)
            })?;

        debug!(
            "Generated OAuth2 bind URL for provider: {} (user: {})",
            req.provider,
            user_id.as_str()
        );

        Ok(Response::new(GetAuthorizationUrlForBindResponse {
            authorization_url,
            state,
        }))
    }

    /// Exchange authorization code for JWT token
    async fn exchange_authorization_code(
        &self,
        request: Request<ExchangeAuthorizationCodeRequest>,
    ) -> Result<Response<ExchangeAuthorizationCodeResponse>, Status> {
        let req = request.into_inner();
let result = self.oauth2_api
            .exchange_authorization_code(&req.provider, &req.code, &req.state)
            .await
            .map_err(|e| {
                error!("Failed to exchange authorization code: {}", e);
                impls_err_to_status(e)
            })?;

        info!(
            "OAuth2 exchange successful for provider: {} (is_bind: {})",
            req.provider, result.is_bind
        );

        Ok(Response::new(ExchangeAuthorizationCodeResponse {
            access_token: result.access_token.unwrap_or_default(),
            refresh_token: result.refresh_token.unwrap_or_default(),
            expires_in: result.expires_in,
            user_info: result.user_info,
            redirect_url: result.redirect_url.unwrap_or_default(),
            is_bind: result.is_bind,
        }))
    }

    /// List all available OAuth2 provider instances
    async fn list_available_providers(
        &self,
        _request: Request<ListAvailableProvidersRequest>,
    ) -> Result<Response<ListAvailableProvidersResponse>, Status> {
let providers = self.oauth2_api
            .list_available_providers()
            .await
            .map_err(|e| {
                error!("Failed to list available providers: {}", e);
                impls_err_to_status(e)
            })?;

        let response = providers
            .into_iter()
            .map(|p| p.into())
            .collect();

        Ok(Response::new(ListAvailableProvidersResponse {
            providers: response,
        }))
    }

    /// Unlink OAuth2 provider from user account
    async fn unlink_provider(
        &self,
        request: Request<UnlinkProviderRequest>,
    ) -> Result<Response<UnlinkProviderResponse>, Status> {
        // Extract user_id from gRPC metadata (set by auth interceptor) before consuming request
        let user_id = self.get_user_id(&request)?;
        let req = request.into_inner();

        let result = self.oauth2_api
            .unlink_provider(
                &user_id,
                &req.provider,
                Some(&req.provider_user_id).filter(|s| !s.is_empty()).map(|s| s.as_str())
            )
            .await
            .map_err(|e| {
                error!("Failed to unlink OAuth2 provider: {}", e);
                impls_err_to_status(e)
            })?;

        if !result.success {
            return Err(Status::not_found("No binding found for this provider"));
        }

        info!(
            "User {} unlinked OAuth2 provider: {}",
            user_id.as_str(),
            req.provider
        );

        Ok(Response::new(UnlinkProviderResponse {
            success: result.success,
            removed_count: result.removed_count,
        }))
    }

    /// Get linked OAuth2 providers for authenticated user
    async fn get_linked_providers(
        &self,
        request: Request<GetLinkedProvidersRequest>,
    ) -> Result<Response<GetLinkedProvidersResponse>, Status> {
        // Extract user_id from gRPC metadata (set by auth interceptor)
        let user_id = self.get_user_id(&request)?;

        let providers = self.oauth2_api
            .get_linked_providers(&user_id)
            .await
            .map_err(|e| {
                error!("Failed to get linked providers: {}", e);
                impls_err_to_status(e)
            })?;

        let response = providers
            .into_iter()
            .map(|p| p.into())
            .collect();

        Ok(Response::new(GetLinkedProvidersResponse {
            providers: response,
        }))
    }
}
