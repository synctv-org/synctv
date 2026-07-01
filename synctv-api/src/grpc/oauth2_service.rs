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
//! - `ExchangeAuthorizationCode` - complete `OAuth2` login or bind flow from code/state
//! - `ListAvailableProviders` - discover available providers
//!
//! Authenticated endpoints (JWT required):
//! - `GetAuthorizationUrlForBind` - bind `OAuth2` to existing account
//! - `UnlinkProvider` - remove `OAuth2` binding
//! - `GetLinkedProviders` - list user's `OAuth2` bindings
//!
//! The service is registered without transport-level auth middleware.
//! Authenticated endpoints call the shared impl-level `RequestExecutor`
//! explicitly so HTTP and gRPC reuse the same validation pipeline.

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
use synctv_core::Config;

use super::map_api_error;
use crate::impls::{EndpointRateLimitCategory, RequestExecutor};

fn map_oauth2_exchange_error(error: crate::impls::ApiError) -> Status {
    map_api_error(error)
}

/// gRPC `OAuth2` service with mixed authentication.
///
/// Registered without transport-level auth middleware. Public endpoints
/// (`GetAuthorizationUrl`, `ExchangeAuthorizationCode`, `ListAvailableProviders`)
/// require no authentication. Private endpoints (`GetAuthorizationUrlForBind`,
/// `UnlinkProvider`, `GetLinkedProviders`) call the shared request executor
/// explicitly.
pub struct OAuth2GrpcService {
    oauth2_api: Arc<crate::impls::OAuth2ApiImpl>,
    config: Arc<Config>,
    request_executor: Arc<RequestExecutor>,
}

impl OAuth2GrpcService {
    #[must_use]
    pub const fn new(
        oauth2_api: Arc<crate::impls::OAuth2ApiImpl>,
        config: Arc<Config>,
        request_executor: Arc<RequestExecutor>,
    ) -> Self {
        Self {
            oauth2_api,
            config,
            request_executor,
        }
    }
}

#[tonic::async_trait]
impl OAuth2Service for OAuth2GrpcService {
    /// Get authorization URL for `OAuth2` login flow (PUBLIC - no auth required)
    async fn get_authorization_url(
        &self,
        request: Request<GetAuthorizationUrlRequest>,
    ) -> Result<Response<GetAuthorizationUrlResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        )?;
        let req = request.into_inner();
        let provider_for_log = req.provider.clone();
        let oauth2_api = Arc::clone(&self.oauth2_api);
        let response = self
            .request_executor
            .execute_public_with_control(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |request_control| async move {
                    oauth2_api
                        .get_authorization_url_response_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(|e| {
                error!("Failed to get authorization URL: {}", e);
                map_api_error(e)
            })?;

        debug!(
            "Generated OAuth2 authorization URL for provider: {}",
            provider_for_log
        );

        Ok(Response::new(response))
    }

    /// Get authorization URL for binding `OAuth2` provider to existing user account
    /// (AUTHENTICATED - requires JWT)
    async fn get_authorization_url_for_bind(
        &self,
        request: Request<GetAuthorizationUrlForBindRequest>,
    ) -> Result<Response<GetAuthorizationUrlForBindResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        )?;
        let req = request.into_inner();
        let provider_for_log = req.provider.clone();
        let oauth2_api = Arc::clone(&self.oauth2_api);

        let response = self
            .request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |request_control, authenticated| async move {
                    oauth2_api
                        .get_authorization_url_for_bind_response_with_control(
                            &authenticated.user_id,
                            req,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(|e| {
                error!("Failed to get authorization URL for bind: {}", e);
                map_api_error(e)
            })?;

        debug!(
            "Generated OAuth2 bind URL for provider: {}",
            provider_for_log
        );

        Ok(Response::new(response))
    }

    /// Exchange authorization code for JWT token (optional auth)
    ///
    /// For login flows, no authentication is required.
    /// For bind flows (`target_user_id` present in stored state), the caller must be
    /// authenticated and the token's user ID must match the `target_user_id`.
    async fn exchange_authorization_code(
        &self,
        request: Request<ExchangeAuthorizationCodeRequest>,
    ) -> Result<Response<ExchangeAuthorizationCodeResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        )?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let oauth2_api = Arc::clone(&self.oauth2_api);
        let response = self
            .request_executor
            .execute_optional_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control, authenticated| async move {
                    let current_user_id = authenticated.as_ref().map(|token| &token.user_id);
                    oauth2_api
                        .exchange_authorization_code_response_with_control(
                            req,
                            current_user_id,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(|e| {
                error!("Failed to exchange authorization code: {}", e);
                map_oauth2_exchange_error(e)
            })?;

        info!(
            "OAuth2 exchange successful (operation: {})",
            response.operation
        );

        Ok(Response::new(response))
    }

    /// List all available `OAuth2` provider instances (PUBLIC - no auth required)
    async fn list_available_providers(
        &self,
        request: Request<ListAvailableProvidersRequest>,
    ) -> Result<Response<ListAvailableProvidersResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        )?;
        let oauth2_api = Arc::clone(&self.oauth2_api);
        let response = self
            .request_executor
            .execute_public(
                &metadata,
                EndpointRateLimitCategory::Read,
                move || async move { oauth2_api.list_available_providers_response().await },
            )
            .await
            .map_err(|e| {
                error!("Failed to list available providers: {}", e);
                map_api_error(e)
            })?;

        Ok(Response::new(response))
    }

    /// Unlink `OAuth2` provider from user account (AUTHENTICATED - requires JWT)
    async fn unlink_provider(
        &self,
        request: Request<UnlinkProviderRequest>,
    ) -> Result<Response<UnlinkProviderResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        )?;
        let req = request.into_inner();
        let provider_for_log = req.provider;
        let oauth2_api = Arc::clone(&self.oauth2_api);

        let response = self
            .request_executor
            .execute_user(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    oauth2_api
                        .unlink_provider_response(&authenticated.user_id, req)
                        .await
                },
            )
            .await
            .map_err(|e| {
                error!("Failed to unlink OAuth2 provider: {}", e);
                map_api_error(e)
            })?;

        info!("OAuth2 provider unlinked: {}", provider_for_log);

        Ok(Response::new(response))
    }

    /// Get linked `OAuth2` providers for authenticated user (AUTHENTICATED - requires JWT)
    async fn get_linked_providers(
        &self,
        request: Request<GetLinkedProvidersRequest>,
    ) -> Result<Response<GetLinkedProvidersResponse>, Status> {
        let metadata = super::request_metadata(
            &request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        )?;

        let response = self
            .request_executor
            .execute_user(&metadata, EndpointRateLimitCategory::Read, {
                let oauth2_api = Arc::clone(&self.oauth2_api);
                move |authenticated| async move {
                    oauth2_api
                        .get_linked_providers_response(&authenticated.user_id)
                        .await
                }
            })
            .await
            .map_err(|e| {
                error!("Failed to get linked providers: {}", e);
                map_api_error(e)
            })?;

        Ok(Response::new(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    #[test]
    fn test_unlink_missing_binding_maps_to_grpc_not_found() {
        let status = map_api_error(crate::impls::ApiError::NotFound(
            "No binding found for this provider".to_string(),
        ));

        assert_eq!(status.code(), Code::NotFound);
        assert_eq!(status.message(), "No binding found for this provider");
    }
}
