//! gRPC `OAuth2` service implementation
//!
//! # HTTP vs gRPC endpoint differences
//!
//! Both transports expose the same logical operations via `OAuth2ApiImpl`, but
//! differ in how they handle the `OAuth2` redirect flow:
//!
//! - **HTTP** (`GET /api/oauth2/:provider/authorize`): The provider name is a
//!   URL path segment and the redirect URL is a query parameter. This is the
//!   natural fit for browser-initiated `OAuth2` flows.
//!
//! - **gRPC** (`GetAuthorizationUrl`): The provider name and redirect URL are
//!   fields in the `GetAuthorizationUrlRequest` message. Mobile/desktop clients
//!   that already use gRPC for all other calls can stay on a single transport.
//!
//! Both transports delegate to the same `OAuth2ApiImpl` implementation, so
//! business logic (token exchange, provider linking) is identical.
//!
//! # Authentication model
//!
//! Public endpoints (no auth required):
//! - `GetAuthorizationUrl` - initiate `OAuth2` login flow
//! - `ExchangeAuthorizationCode` - complete `OAuth2` login flow
//! - `ListAvailableProviders` - discover available providers
//!
//! Authenticated endpoints (JWT required):
//! - `GetAuthorizationUrlForBind` - bind `OAuth2` to existing account
//! - `UnlinkProvider` - remove `OAuth2` binding
//! - `GetLinkedProviders` - list user's `OAuth2` bindings
//!
//! The service is registered WITHOUT a global auth interceptor. Authenticated
//! endpoints perform inline JWT validation using `AuthInterceptor` directly.

use synctv_proto::client::{
    o_auth2_service_server::OAuth2Service, ExchangeAuthorizationCodeRequest,
    ExchangeAuthorizationCodeResponse, GetAuthorizationUrlForBindRequest,
    GetAuthorizationUrlForBindResponse, GetAuthorizationUrlRequest, GetAuthorizationUrlResponse,
    GetLinkedProvidersRequest, GetLinkedProvidersResponse, ListAvailableProvidersRequest,
    ListAvailableProvidersResponse, UnlinkProviderRequest, UnlinkProviderResponse,
};
use tonic::{Request, Response, Status};
use tracing::{debug, error, info};

use std::sync::Arc;
use synctv_core::models::UserId;

use super::map_api_error;

/// gRPC `OAuth2` service with mixed authentication.
///
/// Registered WITHOUT a global auth interceptor. Public endpoints
/// (`GetAuthorizationUrl`, `ExchangeAuthorizationCode`, `ListAvailableProviders`)
/// require no authentication. Private endpoints (`GetAuthorizationUrlForBind`,
/// `UnlinkProvider`, `GetLinkedProviders`) perform inline JWT validation.
pub struct OAuth2GrpcService {
    oauth2_api: Arc<crate::impls::OAuth2ApiImpl>,
    /// Auth interceptor for endpoints that require authentication.
    /// Used inline instead of as a global service interceptor so that
    /// public endpoints remain unauthenticated.
    auth_interceptor: super::interceptors::AuthInterceptor,
}

impl OAuth2GrpcService {
    #[must_use]
    pub const fn new(
        oauth2_api: Arc<crate::impls::OAuth2ApiImpl>,
        auth_interceptor: super::interceptors::AuthInterceptor,
    ) -> Self {
        Self {
            oauth2_api,
            auth_interceptor,
        }
    }

    /// Perform inline JWT auth and extract `user_id` for authenticated endpoints.
    ///
    /// This replaces the global `with_interceptor` pattern so that only the
    /// private endpoints require authentication. Calls `inject_user` on the
    /// request and then reads the resulting `UserContext` from extensions.
    fn require_auth<T: std::fmt::Debug>(
        &self,
        request: Request<T>,
    ) -> Result<(UserId, Request<T>), Status> {
        // inject_user consumes the authenticated identity produced by
        // BlacklistCheckLayer and inserts UserContext into extensions
        let request = self.auth_interceptor.inject_user(request)?;
        let user_context = request
            .extensions()
            .get::<super::interceptors::UserContext>()
            .ok_or_else(|| Status::internal("UserContext missing after auth"))?;
        let user_id = UserId::from_string(user_context.user_id.clone());
        Ok((user_id, request))
    }
}

#[tonic::async_trait]
impl OAuth2Service for OAuth2GrpcService {
    /// Get authorization URL for `OAuth2` login flow (PUBLIC - no auth required)
    async fn get_authorization_url(
        &self,
        request: Request<GetAuthorizationUrlRequest>,
    ) -> Result<Response<GetAuthorizationUrlResponse>, Status> {
        let req = request.into_inner();
        let (authorization_url, state) = self
            .oauth2_api
            .get_authorization_url(
                &req.provider,
                Some(req.redirect_url).filter(|s| !s.is_empty()),
            )
            .await
            .map_err(|e| {
                error!("Failed to get authorization URL: {}", e);
                map_api_error(e)
            })?;

        debug!(
            "Generated OAuth2 authorization URL for provider: {}",
            req.provider
        );

        Ok(Response::new(GetAuthorizationUrlResponse {
            authorization_url,
            state,
        }))
    }

    /// Get authorization URL for binding `OAuth2` provider to existing user account
    /// (AUTHENTICATED - requires JWT)
    async fn get_authorization_url_for_bind(
        &self,
        request: Request<GetAuthorizationUrlForBindRequest>,
    ) -> Result<Response<GetAuthorizationUrlForBindResponse>, Status> {
        let (user_id, request) = self.require_auth(request)?;
        let req = request.into_inner();

        let (authorization_url, state) = self
            .oauth2_api
            .get_authorization_url_for_bind(
                &user_id,
                &req.provider,
                Some(req.redirect_url).filter(|s| !s.is_empty()),
            )
            .await
            .map_err(|e| {
                error!("Failed to get authorization URL for bind: {}", e);
                map_api_error(e)
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

    /// Exchange authorization code for JWT token (optional auth)
    ///
    /// For login flows, no authentication is required.
    /// For bind flows (`bind_user_id` present in stored state), the caller must be
    /// authenticated and the token's user ID must match the `bind_user_id`.
    async fn exchange_authorization_code(
        &self,
        request: Request<ExchangeAuthorizationCodeRequest>,
    ) -> Result<Response<ExchangeAuthorizationCodeResponse>, Status> {
        // Reuse the identity authenticated by BlacklistCheckLayer when present.
        // Public login flows have no authenticated token and therefore keep
        // `current_user_id` as `None`.
        let current_user_id: Option<UserId> = request
            .extensions()
            .get::<synctv_core::service::AuthenticatedToken>()
            .map(|authenticated| authenticated.user_id.clone());

        // Extract client IP for brute-force protection (Issue #24)
        let client_ip = request.remote_addr().map(|addr| addr.ip());
        let req = request.into_inner();
        let result = self
            .oauth2_api
            .exchange_authorization_code(
                &req.provider,
                &req.code,
                &req.state,
                current_user_id.as_ref(),
                client_ip,
            )
            .await
            .map_err(|e| {
                error!("Failed to exchange authorization code: {}", e);
                map_api_error(e)
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

    /// List all available `OAuth2` provider instances (PUBLIC - no auth required)
    async fn list_available_providers(
        &self,
        _request: Request<ListAvailableProvidersRequest>,
    ) -> Result<Response<ListAvailableProvidersResponse>, Status> {
        let providers = self
            .oauth2_api
            .list_available_providers()
            .await
            .map_err(|e| {
                error!("Failed to list available providers: {}", e);
                map_api_error(e)
            })?;

        let response = providers
            .into_iter()
            .map(std::convert::Into::into)
            .collect();

        Ok(Response::new(ListAvailableProvidersResponse {
            providers: response,
        }))
    }

    /// Unlink `OAuth2` provider from user account (AUTHENTICATED - requires JWT)
    async fn unlink_provider(
        &self,
        request: Request<UnlinkProviderRequest>,
    ) -> Result<Response<UnlinkProviderResponse>, Status> {
        let (user_id, request) = self.require_auth(request)?;
        let req = request.into_inner();

        let result = self
            .oauth2_api
            .unlink_provider(
                &user_id,
                &req.provider,
                Some(&req.provider_user_id)
                    .filter(|s| !s.is_empty())
                    .map(std::string::String::as_str),
            )
            .await
            .map_err(|e| {
                error!("Failed to unlink OAuth2 provider: {}", e);
                map_api_error(e)
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

    /// Get linked `OAuth2` providers for authenticated user (AUTHENTICATED - requires JWT)
    async fn get_linked_providers(
        &self,
        request: Request<GetLinkedProvidersRequest>,
    ) -> Result<Response<GetLinkedProvidersResponse>, Status> {
        let (user_id, _request) = self.require_auth(request)?;

        let providers = self
            .oauth2_api
            .get_linked_providers(&user_id)
            .await
            .map_err(|e| {
                error!("Failed to get linked providers: {}", e);
                map_api_error(e)
            })?;

        let response = providers
            .into_iter()
            .map(std::convert::Into::into)
            .collect();

        Ok(Response::new(GetLinkedProvidersResponse {
            providers: response,
        }))
    }
}
