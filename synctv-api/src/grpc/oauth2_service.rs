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
use synctv_core::Config;

use super::map_api_error;

/// gRPC `OAuth2` service with mixed authentication.
///
/// Registered WITHOUT a global auth interceptor. Public endpoints
/// (`GetAuthorizationUrl`, `ExchangeAuthorizationCode`, `ListAvailableProviders`)
/// require no authentication. Private endpoints (`GetAuthorizationUrlForBind`,
/// `UnlinkProvider`, `GetLinkedProviders`) perform inline JWT validation.
pub struct OAuth2GrpcService {
    oauth2_api: Arc<crate::impls::OAuth2ApiImpl>,
    config: Arc<Config>,
    /// Auth interceptor for endpoints that require authentication.
    /// Used inline instead of as a global service interceptor so that
    /// public endpoints remain unauthenticated.
    auth_interceptor: super::interceptors::AuthInterceptor,
}

impl OAuth2GrpcService {
    #[must_use]
    pub const fn new(
        oauth2_api: Arc<crate::impls::OAuth2ApiImpl>,
        config: Arc<Config>,
        auth_interceptor: super::interceptors::AuthInterceptor,
    ) -> Self {
        Self {
            oauth2_api,
            config,
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

fn validate_oauth2_proto_request<T>(request: &T) -> Result<(), Status>
where
    T: prost_reflect::ReflectMessage,
{
    crate::impls::validate_proto_request(request)
        .map_err(|error| Status::invalid_argument(error.to_string()))
}

fn optional_non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[tonic::async_trait]
impl OAuth2Service for OAuth2GrpcService {
    /// Get authorization URL for `OAuth2` login flow (PUBLIC - no auth required)
    async fn get_authorization_url(
        &self,
        request: Request<GetAuthorizationUrlRequest>,
    ) -> Result<Response<GetAuthorizationUrlResponse>, Status> {
        let req = request.into_inner();
        validate_oauth2_proto_request(&req)?;
        let redirect_url = optional_non_empty_trimmed(&req.redirect_url);
        let (authorization_url, state) = self
            .oauth2_api
            .get_authorization_url(&req.provider, redirect_url)
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
        validate_oauth2_proto_request(&req)?;
        let redirect_url = optional_non_empty_trimmed(&req.redirect_url);

        let (authorization_url, state) = self
            .oauth2_api
            .get_authorization_url_for_bind(&user_id, &req.provider, redirect_url)
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

        let client_ip = super::extract_client_ip(&request, &self.config);
        let req = request.into_inner();
        validate_oauth2_proto_request(&req)?;
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
        validate_oauth2_proto_request(&req)?;
        let provider_user_id = optional_non_empty_trimmed(&req.provider_user_id);

        let result = self
            .oauth2_api
            .unlink_provider(&user_id, &req.provider, provider_user_id.as_deref())
            .await
            .map_err(|e| {
                error!("Failed to unlink OAuth2 provider: {}", e);
                map_api_error(e)
            })?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    #[test]
    fn test_validate_oauth2_proto_request_rejects_invalid_redirect_url() {
        let err = validate_oauth2_proto_request(&GetAuthorizationUrlRequest {
            provider: "github".to_string(),
            redirect_url: "javascript:alert(1)".to_string(),
        })
        .expect_err("invalid redirect URL must be rejected before hitting oauth2 impl");
        assert_eq!(err.code(), Code::InvalidArgument);
        assert!(err.message().contains("redirect_url"));
    }

    #[test]
    fn test_validate_oauth2_proto_request_rejects_invalid_state() {
        let err = validate_oauth2_proto_request(&ExchangeAuthorizationCodeRequest {
            provider: "github".to_string(),
            code: "code.with.dots".to_string(),
            state: "short".to_string(),
        })
        .expect_err("invalid OAuth2 state must be rejected before exchange");
        assert_eq!(err.code(), Code::InvalidArgument);
        assert!(err.message().contains("state"));
    }

    #[test]
    fn test_validate_oauth2_proto_request_rejects_invalid_code() {
        let err = validate_oauth2_proto_request(&ExchangeAuthorizationCodeRequest {
            provider: "github".to_string(),
            code: "code with spaces".to_string(),
            state: "AbCdEfGh1234567890aBcDeFgHiJkLm".to_string(),
        })
        .expect_err("invalid OAuth2 code must be rejected before exchange");
        assert_eq!(err.code(), Code::InvalidArgument);
        assert!(err.message().contains("code"));
    }

    #[test]
    fn test_validate_oauth2_proto_request_rejects_too_long_provider_user_id() {
        let too_long = "a".repeat(257);
        let err = validate_oauth2_proto_request(&UnlinkProviderRequest {
            provider: "github".to_string(),
            provider_user_id: too_long,
        })
        .expect_err("overlong provider_user_id must be rejected");
        assert_eq!(err.code(), Code::InvalidArgument);
        assert!(err.message().contains("provider_user_id"));
    }

    #[test]
    fn test_unlink_missing_binding_maps_to_grpc_not_found() {
        let status = map_api_error(crate::impls::ApiError::NotFound(
            "No binding found for this provider".to_string(),
        ));

        assert_eq!(status.code(), Code::NotFound);
        assert_eq!(status.message(), "No binding found for this provider");
    }
}
