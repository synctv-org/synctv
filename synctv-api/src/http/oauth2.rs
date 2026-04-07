//! `OAuth2` HTTP handlers
//!
//! Provides `OAuth2` endpoints for browser/frontend-driven `OAuth2` flows.
//! Uses proto-generated types for request/response consistency with gRPC.
//!
//! ## HTTP vs gRPC endpoint mapping
//!
//! | HTTP endpoint                                          | gRPC RPC                         | Auth required |
//! |--------------------------------------------------------|----------------------------------|---------------|
//! | `GET  /api/oauth2/:provider/authorize?redirect_url=`   | `GetAuthorizationUrl`            | No            |
//! | `GET  /api/oauth2/:provider/bind?redirect_url=`        | `GetAuthorizationUrlForBind`     | Yes           |
//! | `POST /api/oauth2/:provider/exchange` (JSON body)      | `ExchangeAuthorizationCode`      | No            |
//! | `GET  /api/oauth2/providers`                           | `ListAvailableProviders`         | No            |
//! | `DELETE /api/oauth2/type/:provider/unlink?provider_user_id=`| `UnlinkProvider`            | Yes           |
//! | `GET  /api/oauth2/linked`                              | `GetLinkedProviders`             | Yes           |
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
use tracing::{debug, error, info};

use synctv_proto::client::{
    ExchangeAuthorizationCodeRequest, ExchangeAuthorizationCodeResponse,
    GetAuthorizationUrlForBindRequest, GetAuthorizationUrlForBindResponse,
    GetAuthorizationUrlRequest, GetAuthorizationUrlResponse, GetLinkedProvidersResponse,
    ListAvailableProvidersResponse, OAuth2ProviderInstancePathRequest,
    OAuth2ProviderTypePathRequest, UnlinkProviderRequest, UnlinkProviderResponse,
};

use super::{error::map_api_error, middleware::AuthUser, AppError, AppResult, AppState};

fn oauth2_unavailable_error() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "OAuth2 is not available on this server.",
    )
}

fn require_oauth2_api(state: &AppState) -> Result<Arc<crate::impls::OAuth2ApiImpl>, AppError> {
    state
        .oauth2_api
        .clone()
        .ok_or_else(oauth2_unavailable_error)
}

fn validate_oauth2_proto_request<T>(request: &T) -> AppResult<()>
where
    T: prost_reflect::ReflectMessage,
{
    crate::impls::validate_proto_request(request).map_err(super::error::map_api_error)
}

fn validate_oauth2_path<T>(path: T) -> AppResult<T>
where
    T: prost_reflect::ReflectMessage,
{
    validate_oauth2_proto_request(&path)?;
    Ok(path)
}

fn optional_non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
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
            (status = 400, description = "Invalid OAuth2 request", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn get_authorize_url(
    State(state): State<AppState>,
    Path(path): Path<OAuth2ProviderInstancePathRequest>,
    Query(mut req): Query<GetAuthorizationUrlRequest>,
) -> AppResult<Json<GetAuthorizationUrlResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    req.provider = validate_oauth2_path(path)?.provider;
    validate_oauth2_proto_request(&req)?;
    let redirect_url = optional_non_empty_trimmed(&req.redirect_url);

    let (authorization_url, state_token) = oauth2_api
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

    Ok(Json(GetAuthorizationUrlResponse {
        authorization_url,
        state: state_token,
    }))
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
            (status = 400, description = "Invalid OAuth2 exchange request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required for bind flow", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn exchange_authorization_code(
    maybe_auth: Option<super::middleware::AuthUser>,
    State(state): State<AppState>,
    connect_info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Path(path): Path<OAuth2ProviderInstancePathRequest>,
    Json(req): Json<ExchangeAuthorizationCodeRequest>,
) -> AppResult<Json<ExchangeAuthorizationCodeResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    let mut req = req;
    req.provider = validate_oauth2_path(path)?.provider;
    validate_oauth2_proto_request(&req)?;

    let current_user_id = maybe_auth.as_ref().map(|a| &a.user_id);

    // Extract client IP for brute-force protection (Issue #24).
    let client_ip = crate::client_ip::extract_client_ip_from_headers(
        &state.config,
        connect_info.0.ip(),
        &headers,
    );

    let result = oauth2_api
        .exchange_authorization_code(
            &req.provider,
            &req.code,
            &req.state,
            current_user_id,
            Some(client_ip),
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

    Ok(Json(ExchangeAuthorizationCodeResponse {
        access_token: result.access_token.unwrap_or_default(),
        refresh_token: result.refresh_token.unwrap_or_default(),
        expires_in: result.expires_in,
        user_info: result.user_info,
        redirect_url: result.redirect_url.unwrap_or_default(),
        is_bind: result.is_bind,
    }))
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
            (status = 400, description = "Invalid OAuth2 bind request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_bind_authorize_url(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(path): Path<OAuth2ProviderInstancePathRequest>,
    Query(mut req): Query<GetAuthorizationUrlForBindRequest>,
) -> AppResult<Json<GetAuthorizationUrlForBindResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    req.provider = validate_oauth2_path(path)?.provider;
    validate_oauth2_proto_request(&req)?;
    let redirect_url = optional_non_empty_trimmed(&req.redirect_url);

    let (authorization_url, state_token) = oauth2_api
        .get_authorization_url_for_bind(&auth.user_id, &req.provider, redirect_url)
        .await
        .map_err(|e| {
            error!("Failed to get authorization URL for bind: {}", e);
            map_api_error(e)
        })?;

    debug!(
        "Generated OAuth2 bind URL for provider: {} (user: {})",
        req.provider,
        auth.user_id.as_str()
    );

    Ok(Json(GetAuthorizationUrlForBindResponse {
        authorization_url,
        state: state_token,
    }))
}

/// Unlink `OAuth2` provider from authenticated user
///
/// DELETE /`api/oauth2/type/:provider/unlink?provider_user_id`=<optional>
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/oauth2/type/{provider}/unlink",
        tag = "OAuth2",
        params(
            ("provider" = String, Path, description = "OAuth2 provider type"),
            ("provider_user_id" = Option<String>, Query, description = "Optional provider user ID to unlink")
        ),
        responses(
            (status = 200, description = "OAuth2 provider unlinked", body = UnlinkProviderResponse),
            (status = 400, description = "Invalid unlink request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn unlink_provider(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(path): Path<OAuth2ProviderTypePathRequest>,
    Query(mut req): Query<UnlinkProviderRequest>,
) -> AppResult<Json<UnlinkProviderResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    req.provider = validate_oauth2_path(path)?.provider;
    validate_oauth2_proto_request(&req)?;
    let provider_user_id = optional_non_empty_trimmed(&req.provider_user_id);

    let result = oauth2_api
        .unlink_provider(&auth.user_id, &req.provider, provider_user_id.as_deref())
        .await
        .map_err(|e| {
            error!("Failed to unlink OAuth2 provider: {}", e);
            map_api_error(e)
        })?;

    info!(
        "User {} unlinked OAuth2 provider: {}",
        auth.user_id.as_str(),
        req.provider
    );

    Ok(Json(UnlinkProviderResponse {
        success: result.success,
        removed_count: result.removed_count,
    }))
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
            (status = 400, description = "OAuth2 is not configured", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn list_available_providers(
    State(state): State<AppState>,
) -> AppResult<Json<ListAvailableProvidersResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;

    let providers = oauth2_api.list_available_providers().await.map_err(|e| {
        error!("Failed to list available providers: {}", e);
        map_api_error(e)
    })?;

    let response = providers
        .into_iter()
        .map(std::convert::Into::into)
        .collect();

    Ok(Json(ListAvailableProvidersResponse {
        providers: response,
    }))
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
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_linked_providers(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<GetLinkedProvidersResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;

    let providers = oauth2_api
        .get_linked_providers(&auth.user_id)
        .await
        .map_err(|e| {
            error!("Failed to get linked providers: {}", e);
            map_api_error(e)
        })?;

    let response = providers
        .into_iter()
        .map(std::convert::Into::into)
        .collect();

    Ok(Json(GetLinkedProvidersResponse {
        providers: response,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn test_optional_non_empty_trimmed() {
        assert_eq!(optional_non_empty_trimmed(""), None);
        assert_eq!(optional_non_empty_trimmed("   "), None);
        assert_eq!(
            optional_non_empty_trimmed("  https://example.com/callback  "),
            Some("https://example.com/callback".to_string())
        );
        assert_eq!(
            optional_non_empty_trimmed("  provider-user-123  "),
            Some("provider-user-123".to_string())
        );
    }

    #[test]
    fn test_validate_oauth2_proto_request_rejects_javascript_redirect_url() {
        let err = validate_oauth2_proto_request(&GetAuthorizationUrlRequest {
            provider: "github".to_string(),
            redirect_url: "javascript:alert(document.cookie)".to_string(),
        })
        .expect_err("dangerous redirect URL must be rejected");

        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("redirect_url"));
    }

    #[test]
    fn test_validate_oauth2_proto_request_accepts_native_app_redirect_url() {
        validate_oauth2_proto_request(&GetAuthorizationUrlForBindRequest {
            provider: "logto1".to_string(),
            redirect_url: "io.github.synctv://oauth2/callback".to_string(),
        })
        .expect("native app redirect URL should remain valid");
    }

    #[test]
    fn test_validate_oauth2_proto_request_rejects_invalid_exchange_code() {
        let err = validate_oauth2_proto_request(&ExchangeAuthorizationCodeRequest {
            provider: "github".to_string(),
            code: "code with spaces".to_string(),
            state: "AbCdEfGh1234567890aBcDeFgHiJkLm".to_string(),
        })
        .expect_err("invalid code must be rejected");

        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("code"));
    }

    #[test]
    fn test_validate_oauth2_proto_request_rejects_too_long_provider_user_id() {
        let err = validate_oauth2_proto_request(&UnlinkProviderRequest {
            provider: "github".to_string(),
            provider_user_id: "a".repeat(257),
        })
        .expect_err("overlong provider_user_id must be rejected");

        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("provider_user_id"));
    }

    #[test]
    fn test_oauth2_provider_instance_path_request_deserializes_proto_field_name() {
        let req: OAuth2ProviderInstancePathRequest =
            serde_json::from_str(r#"{"provider":"github-main"}"#).expect("deserialize");
        assert_eq!(req.provider, "github-main");
    }

    #[test]
    fn test_oauth2_provider_type_path_request_deserializes_proto_field_name() {
        let req: OAuth2ProviderTypePathRequest =
            serde_json::from_str(r#"{"provider":"github"}"#).expect("deserialize");
        assert_eq!(req.provider, "github");
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
