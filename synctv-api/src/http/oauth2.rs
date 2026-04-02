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
//! | `DELETE /api/oauth2/:provider/unlink?provider_user_id=`| `UnlinkProvider`                 | Yes           |
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
use serde::Deserialize;
use tracing::{debug, error, info};

use synctv_proto::client::{
    ExchangeAuthorizationCodeRequest, ExchangeAuthorizationCodeResponse,
    GetAuthorizationUrlForBindResponse, GetAuthorizationUrlResponse, GetLinkedProvidersResponse,
    ListAvailableProvidersResponse, UnlinkProviderResponse,
};

use super::{error::map_api_error, middleware::AuthUser, validation, AppResult, AppState};

/// Query params for get authorization URL (converted to proto request)
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct GetAuthUrlQuery {
    pub redirect_url: Option<String>,
}

/// Query params for unlink provider (converted to proto request)
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct UnlinkProviderQuery {
    pub provider_user_id: Option<String>,
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
            GetAuthUrlQuery
        ),
        responses(
            (status = 200, description = "OAuth2 authorization URL", body = GetAuthorizationUrlResponse),
            (status = 400, description = "Invalid OAuth2 request", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn get_authorize_url(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(params): Query<GetAuthUrlQuery>,
) -> AppResult<Json<GetAuthorizationUrlResponse>> {
    let oauth2_api = state
        .oauth2_api
        .as_ref()
        .ok_or_else(|| super::AppError::bad_request("OAuth2 is not configured on this server"))?;

    // Validate redirect_url length and format
    let redirect_url = validation::validate_oauth2_redirect_url(params.redirect_url.as_deref())
        .map_err(|e| super::AppError::bad_request(format!("Invalid redirect_url: {e}")))?;

    let (authorization_url, state_token) = oauth2_api
        .get_authorization_url(&provider, redirect_url)
        .await
        .map_err(|e| {
            error!("Failed to get authorization URL: {}", e);
            map_api_error(e)
        })?;

    debug!(
        "Generated OAuth2 authorization URL for provider: {}",
        provider
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
    Path(provider): Path<String>,
    Json(req): Json<ExchangeAuthorizationCodeRequest>,
) -> AppResult<Json<ExchangeAuthorizationCodeResponse>> {
    let oauth2_api = state
        .oauth2_api
        .as_ref()
        .ok_or_else(|| super::AppError::bad_request("OAuth2 is not configured on this server"))?;

    // Validate state parameter format (CSRF protection)
    // State must be exactly 32 characters from the URL-safe alphabet
    let validated_state = validation::validate_oauth2_state(&req.state)
        .map_err(|e| super::AppError::bad_request(format!("Invalid state parameter: {e}")))?;

    // Validate authorization code format
    let validated_code = validation::validate_oauth2_code(&req.code)
        .map_err(|e| super::AppError::bad_request(format!("Invalid authorization code: {e}")))?;

    let current_user_id = maybe_auth.as_ref().map(|a| &a.user_id);

    // Extract client IP for brute-force protection (Issue #24).
    let client_ip = crate::client_ip::extract_client_ip_from_headers(
        &state.config,
        connect_info.0.ip(),
        &headers,
    );

    let result = oauth2_api
        .exchange_authorization_code(
            &provider,
            &validated_code,
            &validated_state,
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
        provider, result.is_bind
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
            GetAuthUrlQuery
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
    Path(provider): Path<String>,
    Query(params): Query<GetAuthUrlQuery>,
) -> AppResult<Json<GetAuthorizationUrlForBindResponse>> {
    let oauth2_api = state
        .oauth2_api
        .as_ref()
        .ok_or_else(|| super::AppError::bad_request("OAuth2 is not configured on this server"))?;

    // Validate redirect_url length and format
    let redirect_url = validation::validate_oauth2_redirect_url(params.redirect_url.as_deref())
        .map_err(|e| super::AppError::bad_request(format!("Invalid redirect_url: {e}")))?;

    let (authorization_url, state_token) = oauth2_api
        .get_authorization_url_for_bind(&auth.user_id, &provider, redirect_url)
        .await
        .map_err(|e| {
            error!("Failed to get authorization URL for bind: {}", e);
            map_api_error(e)
        })?;

    debug!(
        "Generated OAuth2 bind URL for provider: {} (user: {})",
        provider,
        auth.user_id.as_str()
    );

    Ok(Json(GetAuthorizationUrlForBindResponse {
        authorization_url,
        state: state_token,
    }))
}

/// Unlink `OAuth2` provider from authenticated user
///
/// DELETE /`api/oauth2/:provider/unlink?provider_user_id`=<optional>
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/oauth2/{provider}/unlink",
        tag = "OAuth2",
        params(
            ("provider" = String, Path, description = "OAuth2 provider instance name"),
            UnlinkProviderQuery
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
    Path(provider): Path<String>,
    Query(params): Query<UnlinkProviderQuery>,
) -> AppResult<Json<UnlinkProviderResponse>> {
    let oauth2_api = state
        .oauth2_api
        .as_ref()
        .ok_or_else(|| super::AppError::bad_request("OAuth2 is not configured on this server"))?;

    // Validate provider_user_id length
    let provider_user_id =
        validation::validate_oauth2_provider_user_id(params.provider_user_id.as_deref())
            .map_err(|e| super::AppError::bad_request(format!("Invalid provider_user_id: {e}")))?;

    let result = oauth2_api
        .unlink_provider(&auth.user_id, &provider, provider_user_id.as_deref())
        .await
        .map_err(|e| {
            error!("Failed to unlink OAuth2 provider: {}", e);
            map_api_error(e)
        })?;

    info!(
        "User {} unlinked OAuth2 provider: {}",
        auth.user_id.as_str(),
        provider
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
    let oauth2_api = state
        .oauth2_api
        .as_ref()
        .ok_or_else(|| super::AppError::bad_request("OAuth2 is not configured on this server"))?;

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
    let oauth2_api = state
        .oauth2_api
        .as_ref()
        .ok_or_else(|| super::AppError::bad_request("OAuth2 is not configured on this server"))?;

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
    fn test_validate_oauth2_redirect_url() {
        // None is valid
        assert!(validation::validate_oauth2_redirect_url(None).is_ok());
        assert_eq!(
            validation::validate_oauth2_redirect_url(None).unwrap(),
            None
        );

        // Empty string is treated as None
        assert!(validation::validate_oauth2_redirect_url(Some("")).is_ok());
        assert_eq!(
            validation::validate_oauth2_redirect_url(Some("")).unwrap(),
            None
        );

        // Valid HTTP URL
        let result = validation::validate_oauth2_redirect_url(Some("http://example.com/callback"));
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            Some("http://example.com/callback".to_string())
        );

        // Valid HTTPS URL
        let result = validation::validate_oauth2_redirect_url(Some(
            "https://example.com/callback?state=abc",
        ));
        assert!(result.is_ok());

        // Invalid: not http/https
        assert!(validation::validate_oauth2_redirect_url(Some("ftp://example.com")).is_err());
        assert!(validation::validate_oauth2_redirect_url(Some("javascript:alert(1)")).is_err());
        assert!(validation::validate_oauth2_redirect_url(Some("data:text/html,<script>")).is_err());

        // Invalid: too long
        let long_url = "https://example.com/".to_string()
            + &"a".repeat(validation::limits::OAUTH2_REDIRECT_URL_MAX);
        assert!(validation::validate_oauth2_redirect_url(Some(&long_url)).is_err());

        // Valid: exactly at max length
        let exact_url = "https://example.com/".to_string()
            + &"a".repeat(validation::limits::OAUTH2_REDIRECT_URL_MAX - 20);
        assert!(validation::validate_oauth2_redirect_url(Some(&exact_url)).is_ok());
    }

    #[test]
    fn test_validate_oauth2_provider_user_id() {
        // None is valid
        assert!(validation::validate_oauth2_provider_user_id(None).is_ok());
        assert_eq!(
            validation::validate_oauth2_provider_user_id(None).unwrap(),
            None
        );

        // Empty string is treated as None
        assert!(validation::validate_oauth2_provider_user_id(Some("")).is_ok());
        assert_eq!(
            validation::validate_oauth2_provider_user_id(Some("")).unwrap(),
            None
        );

        // Valid provider user IDs
        assert!(validation::validate_oauth2_provider_user_id(Some("12345")).is_ok());
        assert!(validation::validate_oauth2_provider_user_id(Some("user@example.com")).is_ok());
        assert!(validation::validate_oauth2_provider_user_id(Some("github-user-123")).is_ok());

        // Invalid: too long
        let long_id = "a".repeat(validation::limits::OAUTH2_PROVIDER_USER_ID_MAX + 1);
        assert!(validation::validate_oauth2_provider_user_id(Some(&long_id)).is_err());

        // Valid: exactly at max length
        let exact_id = "a".repeat(validation::limits::OAUTH2_PROVIDER_USER_ID_MAX);
        assert!(validation::validate_oauth2_provider_user_id(Some(&exact_id)).is_ok());
    }

    #[test]
    fn test_redirect_url_validation_rejects_javascript_protocol() {
        // Security: should reject javascript: URLs to prevent XSS
        assert!(validation::validate_oauth2_redirect_url(Some(
            "javascript:alert(document.cookie)"
        ))
        .is_err());
        assert!(validation::validate_oauth2_redirect_url(Some("JAVASCRIPT:alert(1)")).is_err());
        assert!(validation::validate_oauth2_redirect_url(Some("javascript:void(0)")).is_err());
    }

    #[test]
    fn test_redirect_url_validation_rejects_data_protocol() {
        // Security: should reject data: URLs
        assert!(validation::validate_oauth2_redirect_url(Some(
            "data:text/html,<script>alert(1)</script>"
        ))
        .is_err());
        assert!(validation::validate_oauth2_redirect_url(Some(
            "data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg=="
        ))
        .is_err());
    }

    #[test]
    fn test_provider_user_id_sanitization() {
        // Control characters should be removed
        let result = validation::validate_oauth2_provider_user_id(Some("user\x00123"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("user123".to_string()));

        // Whitespace should be trimmed
        let result = validation::validate_oauth2_provider_user_id(Some("  user123  "));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("user123".to_string()));
    }

    #[test]
    fn test_redirect_url_sanitization() {
        // Control characters should be removed
        let result =
            validation::validate_oauth2_redirect_url(Some("https://example.com/callback\x00"));
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            Some("https://example.com/callback".to_string())
        );

        // Whitespace should be trimmed
        let result =
            validation::validate_oauth2_redirect_url(Some("  https://example.com/callback  "));
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            Some("https://example.com/callback".to_string())
        );
    }

    #[test]
    fn test_unlink_missing_binding_maps_to_http_not_found() {
        let err = map_api_error(crate::impls::ApiError::NotFound(
            "No binding found for this provider".to_string(),
        ));

        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.message, "No binding found for this provider");
    }
}
