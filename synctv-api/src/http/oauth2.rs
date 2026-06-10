//! `OAuth2` HTTP handlers
//!
//! Provides `OAuth2` endpoints for browser/frontend-driven `OAuth2` flows.
//! Uses proto-generated types for request/response consistency with gRPC.
//!
//! ## HTTP vs gRPC endpoint mapping
//!
//! | HTTP endpoint | gRPC RPC | Auth required |
//! |--------------------------------------------------------|----------------------------------|---------------|
//! | `GET /api/oauth2/:provider/authorize?redirect_url=` | `GetAuthorizationUrl` | No |
//! | `GET /api/oauth2/:provider/bind?redirect_url=` | `GetAuthorizationUrlForBind` | Yes |
//! | `POST /api/oauth2/:provider/exchange` (JSON body) | `ExchangeAuthorizationCode` | No |
//! | `GET /api/oauth2/providers` | `ListAvailableProviders` | No |
//! | `DELETE /api/oauth2/type/:provider/unlink?provider_user_id=`| `UnlinkProvider` | Yes |
//! | `GET /api/oauth2/linked` | `GetLinkedProviders` | Yes |
//!
//! Both transports share the same `OAuth2ApiImpl` backend. HTTP extracts the
//! provider name from URL path segments and optional params from query strings;
//! gRPC encodes everything in the request message. Error responses differ:
//! HTTP returns `AppError` JSON `{error, status}`, gRPC returns `tonic::Status`.
//!
//! See also: [`crate::grpc::oauth2_service`] for the gRPC implementation.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use std::sync::Arc;
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;
use tracing::{debug, error, info};

use synctv_proto::client::{
    ExchangeAuthorizationCodeRequest, ExchangeAuthorizationCodeResponse,
    GetAuthorizationUrlForBindRequest, GetAuthorizationUrlForBindResponse,
    GetAuthorizationUrlRequest, GetAuthorizationUrlResponse, GetLinkedProvidersResponse,
    ListAvailableProvidersResponse, OAuth2ProviderInstancePathRequest,
    OAuth2ProviderTypePathRequest, UnlinkProviderRequest, UnlinkProviderResponse,
};

use super::{error::map_api_error, middleware::RequestMetadata, AppError, AppResult, AppState};
use crate::impls::EndpointRateLimitCategory;

fn oauth2_unavailable_error() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "OAuth2 is not available on this server.",
    )
}

fn require_oauth2_api(state: &AppState) -> Result<Arc<crate::impls::OAuth2ApiImpl>, AppError> {
    state
        .shared_api_runtime
        .oauth2_api
        .clone()
        .ok_or_else(oauth2_unavailable_error)
}

/// Get `OAuth2` authorization URL for login flow
///
/// GET /`api/oauth2/:provider/authorize?redirect_url`=<url>
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/oauth2/{provider}/authorize",
        tag = "OAuth2",
        params(
            ("provider" = String, Path, description = "OAuth2 provider instance name"),
            ("redirect_url" = Option<String>, Query, description = "Optional redirect URL after OAuth2 flow completes")
        ),
        responses(
            (status = 200, description = "OAuth2 authorization URL", body = GetAuthorizationUrlResponse),
            (status = 400, description = "Invalid OAuth2 request", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn get_authorize_url(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<OAuth2ProviderInstancePathRequest>,
    Query(mut req): Query<GetAuthorizationUrlRequest>,
) -> AppResult<Json<GetAuthorizationUrlResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    req.provider = path.provider;
    let provider_for_log = req.provider.clone();

    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));
    let response = state
        .shared_api_runtime
        .request_executor
        .execute_public_with_control(
            &request_meta,
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

    Ok(Json(response))
}

/// Exchange authorization code for JWT token (frontend-driven flow)
///
/// POST /api/oauth2/:provider/exchange
/// Body: { "code": "xxx", "state": "xxx" }
///
/// For bind flows (where the `OAuth2` state contains a `bind_user_id`), the caller
/// must be authenticated and the authenticated user must match the `bind_user_id`
/// stored in the state. For login flows, no authentication is required.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/oauth2/{provider}/exchange",
        tag = "OAuth2",
        params(
            ("provider" = String, Path, description = "OAuth2 provider instance name")
        ),
        request_body = ExchangeAuthorizationCodeRequest,
        responses(
            (status = 200, description = "Authorization code exchanged", body = ExchangeAuthorizationCodeResponse),
            (status = 400, description = "Invalid OAuth2 exchange request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required for bind flow", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn exchange_authorization_code(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    connect_info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Path(path): Path<OAuth2ProviderInstancePathRequest>,
    Json(mut req): Json<ExchangeAuthorizationCodeRequest>,
) -> AppResult<Json<ExchangeAuthorizationCodeResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    req.provider = path.provider;
    let provider_for_log = req.provider.clone();
    let client_ip = crate::client_ip::extract_client_ip_from_headers(
        &state.config,
        connect_info.0.ip(),
        &headers,
    )
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));

    let response = state
        .shared_api_runtime
        .request_executor
        .execute_optional_user_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control, authenticated| async move {
                let current_user_id = authenticated.as_ref().map(|token| &token.user_id);
                oauth2_api
                    .exchange_authorization_code_response_with_control(
                        req,
                        current_user_id,
                        Some(client_ip),
                        Some(&request_control),
                    )
                    .await
            },
        )
        .await
        .map_err(|e| {
            error!("Failed to exchange authorization code: {}", e);
            map_api_error(e)
        })?;

    info!(
        "OAuth2 exchange successful for provider: {} (is_bind: {})",
        provider_for_log, response.is_bind
    );

    Ok(Json(response))
}

/// Get authorization URL for binding `OAuth2` provider to authenticated user
///
/// GET /`api/oauth2/:provider/bind?redirect_url`=<url>
///
/// Requires authentication. The frontend then redirects to the `OAuth2` provider,
/// receives code/state, and calls exchange endpoint which will bind the provider.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/oauth2/{provider}/bind",
        tag = "OAuth2",
        params(
            ("provider" = String, Path, description = "OAuth2 provider instance name"),
            ("redirect_url" = Option<String>, Query, description = "Optional redirect URL after OAuth2 bind flow completes")
        ),
        responses(
            (status = 200, description = "OAuth2 bind authorization URL", body = GetAuthorizationUrlForBindResponse),
            (status = 400, description = "Invalid OAuth2 bind request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_bind_authorize_url(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<OAuth2ProviderInstancePathRequest>,
    Query(mut req): Query<GetAuthorizationUrlForBindRequest>,
) -> AppResult<Json<GetAuthorizationUrlForBindResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    req.provider = path.provider;
    let provider_for_log = req.provider.clone();

    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));
    let response = state
        .shared_api_runtime
        .request_executor
        .execute_user_with_control(
            &request_meta,
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

    Ok(Json(response))
}

/// Unlink `OAuth2` provider from authenticated user
///
/// DELETE /`api/oauth2/type/:provider/unlink?provider_instance_name`=<optional>&`provider_user_id`=<optional>
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/oauth2/type/{provider}/unlink",
        tag = "OAuth2",
        params(
            ("provider" = String, Path, description = "OAuth2 provider type"),
            ("provider_instance_name" = Option<String>, Query, description = "Required when provider_user_id is set; OAuth2 provider instance namespace"),
            ("provider_user_id" = Option<String>, Query, description = "Optional provider user ID to unlink")
        ),
        responses(
            (status = 200, description = "OAuth2 provider unlinked", body = UnlinkProviderResponse),
            (status = 400, description = "Invalid unlink request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn unlink_provider(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<OAuth2ProviderTypePathRequest>,
    Query(mut req): Query<UnlinkProviderRequest>,
) -> AppResult<Json<UnlinkProviderResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    req.provider = path.provider;
    let provider_for_log = req.provider.clone();

    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));
    let response = state
        .shared_api_runtime
        .request_executor
        .execute_user(
            &request_meta,
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

    Ok(Json(response))
}

/// List all available `OAuth2` provider instances
///
/// GET /api/oauth2/providers
///
/// Returns the configured `OAuth2` provider instances that clients can use
/// for login or account binding. No authentication required.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/oauth2/providers",
        tag = "OAuth2",
        responses(
            (status = 200, description = "Available OAuth2 providers", body = ListAvailableProvidersResponse),
            (status = 503, description = "OAuth2 is not configured", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn list_available_providers(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<ListAvailableProvidersResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));

    let response = state
        .shared_api_runtime
        .request_executor
        .execute_public(
            &request_meta,
            EndpointRateLimitCategory::Read,
            || async move { oauth2_api.list_available_providers_response().await },
        )
        .await
        .map_err(|e| {
            error!("Failed to list available providers: {}", e);
            map_api_error(e)
        })?;

    Ok(Json(response))
}

/// Get linked `OAuth2` providers for authenticated user
///
/// GET /api/oauth2/linked
///
/// Requires authentication.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/oauth2/linked",
        tag = "OAuth2",
        responses(
            (status = 200, description = "Linked OAuth2 providers", body = GetLinkedProvidersResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_linked_providers(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<GetLinkedProvidersResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));

    let response = state
        .shared_api_runtime
        .request_executor
        .execute_user(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                oauth2_api
                    .get_linked_providers_response(&authenticated.user_id)
                    .await
            },
        )
        .await
        .map_err(|e| {
            error!("Failed to get linked providers: {}", e);
            map_api_error(e)
        })?;

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    type TestResult<T = ()> = anyhow::Result<T>;

    #[test]
    fn test_exchange_authorization_code_request_deserializes_without_provider() -> TestResult {
        let mut req: ExchangeAuthorizationCodeRequest = serde_json::from_str(
            r#"{"code":"abc123._+-","state":"AbCdEfGh1234567890aBcDeFgHiJkLm"}"#,
        )?;

        assert!(req.provider.is_empty());
        req.provider = "github-main".to_string();
        assert_eq!(req.provider, "github-main");
        assert_eq!(req.code, "abc123._+-");
        assert_eq!(req.state, "AbCdEfGh1234567890aBcDeFgHiJkLm");
        Ok(())
    }

    #[test]
    fn test_oauth2_path_injected_query_requests_deserialize_without_provider() -> TestResult {
        let authorize: GetAuthorizationUrlRequest =
            serde_urlencoded::from_str("redirect_url=http%3A%2F%2Flocalhost%2Fcallback")?;
        assert!(authorize.provider.is_empty());
        assert_eq!(authorize.redirect_url, "http://localhost/callback");

        let bind: GetAuthorizationUrlForBindRequest = serde_urlencoded::from_str(
            "redirect_url=http%3A%2F%2Flocalhost%2Fbind&verification_id=verification-id",
        )?;
        assert!(bind.provider.is_empty());
        assert_eq!(bind.redirect_url, "http://localhost/bind");
        assert_eq!(bind.verification_id, "verification-id");

        let unlink: UnlinkProviderRequest = serde_urlencoded::from_str(
            "provider_user_id=remote-user-1&verification_id=verification-id",
        )?;
        assert!(unlink.provider.is_empty());
        assert_eq!(unlink.verification_id, "verification-id");
        assert_eq!(unlink.provider_user_id, "remote-user-1");
        assert!(unlink.provider_instance_name.is_empty());
        Ok(())
    }

    #[test]
    fn test_oauth2_provider_instance_path_request_deserializes_proto_field_name() -> TestResult {
        let req: OAuth2ProviderInstancePathRequest =
            serde_json::from_str(r#"{"provider":"github-main"}"#)?;
        assert_eq!(req.provider, "github-main");
        Ok(())
    }

    #[test]
    fn test_oauth2_provider_type_path_request_deserializes_proto_field_name() -> TestResult {
        let req: OAuth2ProviderTypePathRequest = serde_json::from_str(r#"{"provider":"github"}"#)?;
        assert_eq!(req.provider, "github");
        Ok(())
    }

    #[test]
    fn test_unlink_missing_binding_maps_to_http_not_found() {
        let err = map_api_error(crate::impls::ApiError::NotFound(
            "No binding found for this provider".to_string(),
        ));

        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.message, "No binding found for this provider");
    }

    #[test]
    fn test_oauth2_missing_is_service_unavailable() {
        let err = oauth2_unavailable_error();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.message, "OAuth2 is not available on this server.");
    }
}
