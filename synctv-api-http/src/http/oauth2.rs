//! `OAuth2` HTTP handlers
//!
//! Provides `OAuth2` endpoints for browser/frontend-driven `OAuth2` flows.
//! Uses proto-generated types for request/response consistency with gRPC.
//!
//! ## HTTP vs gRPC endpoint mapping
//!
//! | HTTP endpoint | gRPC RPC | Auth required |
//! |--------------------------------------------------------|----------------------------------|---------------|
//! | `GET /api/oauth2/:provider/authorize?redirectUrl=` | `GetAuthorizationUrl` | No |
//! | `GET /api/oauth2/:provider/bind?redirectUrl=` | `GetAuthorizationUrlForBind` | Yes |
//! | `POST /api/oauth2/exchange` (JSON body) | `ExchangeAuthorizationCode` | No |
//! | `GET /api/oauth2/providers` | `ListAvailableProviders` | No |
//! | `DELETE /api/oauth2/type/:provider/unlink?providerUserId=`| `UnlinkProvider` | Yes |
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
    http::HeaderMap,
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

use super::{error::map_api_error, middleware::RequestMetadata, AppError, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct AuthorizationUrlQuery {
    #[serde(default)]
    redirect_url: Option<String>,
    #[serde(default)]
    native: Option<bool>,
}

impl AuthorizationUrlQuery {
    fn into_request(self, provider: String) -> GetAuthorizationUrlRequest {
        GetAuthorizationUrlRequest {
            provider,
            redirect_url: self.redirect_url,
            native: self.native,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct BindAuthorizationUrlQuery {
    #[serde(default)]
    redirect_url: Option<String>,
    #[serde(default)]
    verification_id: String,
    #[serde(default)]
    native: Option<bool>,
}

impl BindAuthorizationUrlQuery {
    fn into_request(self, provider: String) -> GetAuthorizationUrlForBindRequest {
        GetAuthorizationUrlForBindRequest {
            provider,
            redirect_url: self.redirect_url,
            verification_id: self.verification_id,
            native: self.native,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct UnlinkProviderQuery {
    #[serde(default)]
    provider_user_id: String,
    #[serde(default)]
    verification_id: String,
    #[serde(default)]
    provider_instance_name: String,
}

impl UnlinkProviderQuery {
    fn into_request(self, provider: i32) -> UnlinkProviderRequest {
        UnlinkProviderRequest {
            provider,
            provider_user_id: self.provider_user_id,
            verification_id: self.verification_id,
            provider_instance_name: self.provider_instance_name,
        }
    }
}

fn oauth2_unavailable_error() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "OAuth2 is not available on this server.",
    )
}

fn require_oauth2_api(
    state: &AppState,
) -> Result<Arc<synctv_api_common::impls::OAuth2ApiImpl>, AppError> {
    state
        .shared_api_runtime
        .oauth2_api
        .clone()
        .ok_or_else(oauth2_unavailable_error)
}

fn oauth2_provider_type_path_to_proto(provider: &str) -> Result<i32, AppError> {
    synctv_api_common::impls::validate_proto_request(&OAuth2ProviderTypePathRequest {
        provider: provider.to_string(),
    })
    .map_err(map_api_error)?;
    synctv_api_common::impls::OAuth2ApiImpl::oauth2_provider_name_to_proto(provider)
        .map_err(map_api_error)
}

fn map_oauth2_exchange_error(error: synctv_api_common::impls::ApiError) -> AppError {
    map_api_error(error)
}

fn request_allowed_web_callback(
    redirect_url: Option<&str>,
    native: Option<bool>,
    headers: &HeaderMap,
    direct_peer_ip: Option<std::net::IpAddr>,
    server: &synctv_api_common::ApiServerSettings,
) -> AppResult<Option<String>> {
    let Some(redirect_url) = redirect_url.filter(|_| native != Some(true)) else {
        return Ok(None);
    };
    let Ok(parsed) = url::Url::parse(redirect_url) else {
        return Ok(None);
    };
    if parsed.path() != "/auth.html"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Ok(None);
    }

    let request_scheme = if direct_peer_ip.is_some_and(|ip| server.is_trusted_proxy(&ip)) {
        match super::optional_header_str(headers, &super::X_FORWARDED_PROTO)? {
            Some(value) if value.eq_ignore_ascii_case("http") => "http",
            Some(value) if value.eq_ignore_ascii_case("https") => "https",
            Some(_) => {
                return Err(AppError::bad_request(
                    "x-forwarded-proto must be http or https",
                ));
            }
            None => "http",
        }
    } else {
        "http"
    };
    if !parsed.scheme().eq_ignore_ascii_case(request_scheme) {
        return Ok(None);
    }

    let host = super::required_header_str(headers, "host", "Host header is required")?;
    Ok(
        super::websocket::same_origin_as_host(&parsed, host, Some(request_scheme))?
            .then(|| redirect_url.to_string()),
    )
}

/// Get `OAuth2` authorization URL for login flow
///
/// GET /`api/oauth2/:provider/authorize?redirectUrl`=<url>
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/oauth2/{provider}/authorize",
        tag = "OAuth2",
        params(
            ("provider" = String, Path, description = "OAuth2 provider instance name"),
            AuthorizationUrlQuery
        ),
        responses(
            (status = 200, description = "OAuth2 authorization URL", body = GetAuthorizationUrlResponse),
            (status = 400, description = "Invalid OAuth2 request", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn get_authorize_url(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<OAuth2ProviderInstancePathRequest>,
    Query(query): Query<AuthorizationUrlQuery>,
    headers: HeaderMap,
) -> AppResult<Json<GetAuthorizationUrlResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    let req = query.into_request(path.provider);
    let request_allowed_redirect_url = request_allowed_web_callback(
        req.redirect_url.as_deref(),
        req.native,
        &headers,
        request_meta.0.socket_ip,
        &state.runtime_settings.server,
    )?;
    let provider_for_log = req.provider.clone();

    let request_meta = request_meta.0;
    let response = state
        .shared_api_runtime
        .request_executor
        .execute_public_with_control(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |request_control| async move {
                oauth2_api
                    .get_authorization_url_response_with_control(
                        req,
                        request_allowed_redirect_url,
                        Some(&request_control),
                    )
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
/// POST /api/oauth2/exchange
/// Body: { "code": "xxx", "state": "xxx" }
///
/// For bind flows (where the `OAuth2` state contains a `target_user_id`), the caller
/// must be authenticated and the authenticated user must match the `target_user_id`
/// stored in the state. For login flows, no authentication is required.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/oauth2/exchange",
        tag = "OAuth2",
        request_body = ExchangeAuthorizationCodeRequest,
        responses(
            (status = 200, description = "Authorization code exchanged", body = ExchangeAuthorizationCodeResponse),
            (status = 400, description = "Invalid OAuth2 exchange request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required for bind flow", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn exchange_authorization_code(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    connect_info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ExchangeAuthorizationCodeRequest>,
) -> AppResult<Json<ExchangeAuthorizationCodeResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    let client_ip = synctv_adapter::client_ip::extract_client_ip_from_headers(
        |ip| state.runtime_settings.server.is_trusted_proxy(ip),
        connect_info.0.ip(),
        &headers,
    )
    .map_err(|error| AppError::bad_request(error.to_string()))?;
    let request_meta = request_meta.0;

    let response = state
        .shared_api_runtime
        .request_executor
        .execute_optional_user_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control, authenticated| async move {
                let current_user_id = authenticated
                    .as_ref()
                    .map(synctv_core::service::AuthenticatedToken::user_id);
                oauth2_api
                    .exchange_authorization_code_response_with_control(
                        req,
                        current_user_id.as_ref(),
                        Some(client_ip),
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

    Ok(Json(response))
}

/// Get authorization URL for binding `OAuth2` provider to authenticated user
///
/// GET /`api/oauth2/:provider/bind?redirectUrl`=<url>
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
            BindAuthorizationUrlQuery
        ),
        responses(
            (status = 200, description = "OAuth2 bind authorization URL", body = GetAuthorizationUrlForBindResponse),
            (status = 400, description = "Invalid OAuth2 bind request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
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
    Query(query): Query<BindAuthorizationUrlQuery>,
    headers: HeaderMap,
) -> AppResult<Json<GetAuthorizationUrlForBindResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    let req = query.into_request(path.provider);
    let request_allowed_redirect_url = request_allowed_web_callback(
        req.redirect_url.as_deref(),
        req.native,
        &headers,
        request_meta.0.socket_ip,
        &state.runtime_settings.server,
    )?;
    let provider_for_log = req.provider.clone();

    let request_meta = request_meta.0;
    let response = state
        .shared_api_runtime
        .request_executor
        .execute_user_with_control(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |request_control, authenticated| async move {
                oauth2_api
                    .get_authorization_url_for_bind_response_with_control(
                        &authenticated.user_id(),
                        req,
                        request_allowed_redirect_url,
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
/// DELETE /`api/oauth2/type/:provider/unlink?providerInstanceName`=<optional>&`providerUserId`=<optional>
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/oauth2/type/{provider}/unlink",
        tag = "OAuth2",
        params(
            ("provider" = String, Path, description = "OAuth2 provider type"),
            UnlinkProviderQuery
        ),
        responses(
            (status = 200, description = "OAuth2 provider unlinked", body = UnlinkProviderResponse),
            (status = 400, description = "Invalid unlink request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
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
    Query(query): Query<UnlinkProviderQuery>,
) -> AppResult<Json<UnlinkProviderResponse>> {
    let req = query.into_request(oauth2_provider_type_path_to_proto(&path.provider)?);
    let oauth2_api = require_oauth2_api(&state)?;
    let provider_for_log = req.provider;

    let request_meta = request_meta.0;
    let response = state
        .shared_api_runtime
        .request_executor
        .execute_user(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                oauth2_api
                    .unlink_provider_response(&authenticated.user_id(), req)
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
            (status = 503, description = "OAuth2 is not configured", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn list_available_providers(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<ListAvailableProvidersResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    let request_meta = request_meta.0;

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
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
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
    let request_meta = request_meta.0;

    let response = state
        .shared_api_runtime
        .request_executor
        .execute_user(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                oauth2_api
                    .get_linked_providers_response(&authenticated.user_id())
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
    use axum::http::{header, HeaderMap, StatusCode};

    type TestResult<T = ()> = anyhow::Result<T>;

    #[test]
    fn test_oauth2_route_queries_ignore_path_fields() {
        let authorize = serde_urlencoded::from_str::<AuthorizationUrlQuery>(
            "provider=github-main&redirectUrl=http%3A%2F%2Flocalhost%2Fcallback",
        )
        .expect("unknown path field should be ignored");
        let authorize = authorize.into_request("path-provider".to_string());
        assert_eq!(authorize.provider, "path-provider");
        assert_eq!(
            authorize.redirect_url.as_deref(),
            Some("http://localhost/callback")
        );

        let unlink = serde_urlencoded::from_str::<UnlinkProviderQuery>(
            "provider=1&providerUserId=remote-user-1",
        )
        .expect("unknown path field should be ignored");
        let github_provider =
            synctv_proto::client::OAuth2ProviderType::Oauth2ProviderTypeGithub as i32;
        let unlink = unlink.into_request(github_provider);
        assert_eq!(unlink.provider, github_provider);
        assert_eq!(unlink.provider_user_id, "remote-user-1");
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
        let err = map_api_error(synctv_api_common::impls::ApiError::NotFound(
            "No binding found for this provider".to_string(),
        ));

        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.message(), "No binding found for this provider");
    }

    #[test]
    fn test_oauth2_missing_is_service_unavailable() {
        let err = oauth2_unavailable_error();
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.message(), "OAuth2 is not available on this server.");
    }

    fn callback_headers(host: &str) -> TestResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, host.parse()?);
        Ok(headers)
    }

    #[test]
    fn same_origin_web_callback_is_request_allowed() -> TestResult {
        let mut server = synctv_api_common::ApiServerSettings::default();
        server.trusted_proxies = vec!["127.0.0.1".to_string()];
        let mut headers = callback_headers("app.example.test")?;
        headers.insert("x-forwarded-proto", "https".parse()?);

        let allowed = request_allowed_web_callback(
            Some("https://app.example.test/auth.html"),
            Some(false),
            &headers,
            Some("127.0.0.1".parse()?),
            &server,
        )?;

        assert_eq!(
            allowed.as_deref(),
            Some("https://app.example.test/auth.html")
        );
        Ok(())
    }

    #[test]
    fn web_callback_requires_exact_origin_and_path() -> TestResult {
        let mut server = synctv_api_common::ApiServerSettings::default();
        server.trusted_proxies = vec!["127.0.0.1".to_string()];
        let mut headers = callback_headers("app.example.test:8443")?;
        headers.insert("x-forwarded-proto", "https".parse()?);
        let peer = Some("127.0.0.1".parse()?);

        for redirect in [
            "https://evil.example.test:8443/auth.html",
            "https://app.example.test/auth.html",
            "https://app.example.test:8443/oauth2/callback",
            "https://app.example.test:8443/auth.html?next=/rooms",
            "https://app.example.test:8443/auth.html#fragment",
        ] {
            assert_eq!(
                request_allowed_web_callback(Some(redirect), Some(false), &headers, peer, &server,)?,
                None,
                "unexpectedly allowed {redirect}",
            );
        }
        Ok(())
    }

    #[test]
    fn native_and_untrusted_forwarded_callbacks_are_not_request_allowed() -> TestResult {
        let server = synctv_api_common::ApiServerSettings::default();
        let mut headers = callback_headers("app.example.test")?;
        headers.insert("x-forwarded-proto", "https".parse()?);
        let redirect = Some("https://app.example.test/auth.html");

        assert_eq!(
            request_allowed_web_callback(
                redirect,
                Some(true),
                &headers,
                Some("127.0.0.1".parse()?),
                &server,
            )?,
            None,
        );
        assert_eq!(
            request_allowed_web_callback(
                redirect,
                Some(false),
                &headers,
                Some("198.51.100.10".parse()?),
                &server,
            )?,
            None,
        );
        Ok(())
    }

    #[test]
    fn trusted_proxy_callback_rejects_invalid_forwarded_proto() -> TestResult {
        let mut server = synctv_api_common::ApiServerSettings::default();
        server.trusted_proxies = vec!["127.0.0.1".to_string()];
        let mut headers = callback_headers("app.example.test")?;
        headers.insert("x-forwarded-proto", "javascript".parse()?);

        let error = request_allowed_web_callback(
            Some("https://app.example.test/auth.html"),
            Some(false),
            &headers,
            Some("127.0.0.1".parse()?),
            &server,
        )
        .expect_err("invalid proxy scheme must fail");

        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        Ok(())
    }
}
