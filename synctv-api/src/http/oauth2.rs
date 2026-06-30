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
//! | `POST /api/oauth2/:provider/exchange` (JSON body) | `ExchangeAuthorizationCode` | No |
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
    Json,
};
use std::sync::Arc;
use tracing::{debug, error, info};

use synctv_proto::client::{
    ExchangeAuthorizationCodeRequest, ExchangeAuthorizationCodeResponse,
    GetAuthorizationUrlForBindRequest, GetAuthorizationUrlForBindResponse,
    GetAuthorizationUrlRequest, GetAuthorizationUrlResponse, GetLinkedProvidersResponse,
    ListAvailableProvidersResponse, OAuth2ProviderInstancePathRequest, OAuth2ProviderType,
    OAuth2ProviderTypePathRequest, UnlinkProviderRequest, UnlinkProviderResponse,
};

use super::{error::map_api_error, middleware::RequestMetadata, AppError, AppResult, AppState};
use crate::impls::EndpointRateLimitCategory;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct AuthorizationUrlQuery {
    #[serde(default)]
    redirect_url: String,
}

impl AuthorizationUrlQuery {
    fn into_request(self, provider: String) -> GetAuthorizationUrlRequest {
        GetAuthorizationUrlRequest {
            provider,
            redirect_url: self.redirect_url,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct BindAuthorizationUrlQuery {
    #[serde(default)]
    redirect_url: String,
    #[serde(default)]
    verification_id: String,
}

impl BindAuthorizationUrlQuery {
    fn into_request(self, provider: String) -> GetAuthorizationUrlForBindRequest {
        GetAuthorizationUrlForBindRequest {
            provider,
            redirect_url: self.redirect_url,
            verification_id: self.verification_id,
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

fn require_oauth2_api(state: &AppState) -> Result<Arc<crate::impls::OAuth2ApiImpl>, AppError> {
    state
        .shared_api_runtime
        .oauth2_api
        .clone()
        .ok_or_else(oauth2_unavailable_error)
}

fn oauth2_provider_type_path_to_proto(provider: &str) -> Result<i32, AppError> {
    crate::impls::validate_proto_request(&OAuth2ProviderTypePathRequest {
        provider: provider.to_string(),
    })
    .map_err(map_api_error)?;
    let provider = synctv_core::models::OAuth2Provider::from_str_name(provider)
        .ok_or_else(|| AppError::bad_request("Invalid OAuth2 provider type"))?;
    let proto = match provider {
        synctv_core::models::OAuth2Provider::QQ => OAuth2ProviderType::Oauth2ProviderTypeQq,
        synctv_core::models::OAuth2Provider::GitHub => OAuth2ProviderType::Oauth2ProviderTypeGithub,
        synctv_core::models::OAuth2Provider::Google => OAuth2ProviderType::Oauth2ProviderTypeGoogle,
        synctv_core::models::OAuth2Provider::Microsoft => {
            OAuth2ProviderType::Oauth2ProviderTypeMicrosoft
        }
        synctv_core::models::OAuth2Provider::Discord => {
            OAuth2ProviderType::Oauth2ProviderTypeDiscord
        }
        synctv_core::models::OAuth2Provider::Casdoor => {
            OAuth2ProviderType::Oauth2ProviderTypeCasdoor
        }
        synctv_core::models::OAuth2Provider::Logto => OAuth2ProviderType::Oauth2ProviderTypeLogto,
        synctv_core::models::OAuth2Provider::Oidc => OAuth2ProviderType::Oauth2ProviderTypeOidc,
        synctv_core::models::OAuth2Provider::Feishu => OAuth2ProviderType::Oauth2ProviderTypeFeishu,
        synctv_core::models::OAuth2Provider::Gitee => OAuth2ProviderType::Oauth2ProviderTypeGitee,
    };
    Ok(proto as i32)
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
            (status = 400, description = "Invalid OAuth2 request", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn get_authorize_url(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<OAuth2ProviderInstancePathRequest>,
    Query(query): Query<AuthorizationUrlQuery>,
) -> AppResult<Json<GetAuthorizationUrlResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    let req = query.into_request(path.provider);
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
    let request_meta = request_meta.0;

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
    Query(query): Query<BindAuthorizationUrlQuery>,
) -> AppResult<Json<GetAuthorizationUrlForBindResponse>> {
    let oauth2_api = require_oauth2_api(&state)?;
    let req = query.into_request(path.provider);
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
    let request_meta = request_meta.0;

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
    fn test_exchange_authorization_code_request_deserializes_proto_body() -> TestResult {
        let mut req: ExchangeAuthorizationCodeRequest = serde_json::from_str(
            r#"{"provider":"body-provider","code":"abc123._+-","state":"AbCdEfGh1234567890aBcDeFgHiJkLm"}"#,
        )?;
        req.provider = "github-main".to_string();
        assert_eq!(req.provider, "github-main");
        assert_eq!(req.code, "abc123._+-");
        assert_eq!(req.state, "AbCdEfGh1234567890aBcDeFgHiJkLm");
        Ok(())
    }

    #[test]
    fn test_oauth2_route_queries_deserialize_strict_public_fields() -> TestResult {
        let authorize: AuthorizationUrlQuery =
            serde_urlencoded::from_str("redirectUrl=http%3A%2F%2Flocalhost%2Fcallback")?;
        let authorize = authorize.into_request("github-main".to_string());
        assert_eq!(authorize.provider, "github-main");
        assert_eq!(authorize.redirect_url, "http://localhost/callback");

        let bind: BindAuthorizationUrlQuery = serde_urlencoded::from_str(
            "redirectUrl=http%3A%2F%2Flocalhost%2Fbind&verificationId=verification-id",
        )?;
        let bind = bind.into_request("github-main".to_string());
        assert_eq!(bind.provider, "github-main");
        assert_eq!(bind.redirect_url, "http://localhost/bind");
        assert_eq!(bind.verification_id, "verification-id");

        let unlink: UnlinkProviderQuery = serde_urlencoded::from_str(
            "providerUserId=remote-user-1&verificationId=verification-id&providerInstanceName=github-main",
        )?;
        let unlink = unlink.into_request(OAuth2ProviderType::Oauth2ProviderTypeGithub as i32);
        assert_eq!(
            unlink.provider,
            OAuth2ProviderType::Oauth2ProviderTypeGithub as i32
        );
        assert_eq!(unlink.verification_id, "verification-id");
        assert_eq!(unlink.provider_user_id, "remote-user-1");
        assert_eq!(unlink.provider_instance_name, "github-main");
        Ok(())
    }

    #[test]
    fn test_oauth2_route_queries_reject_path_fields() {
        let authorize = serde_urlencoded::from_str::<AuthorizationUrlQuery>(
            "provider=github-main&redirectUrl=http%3A%2F%2Flocalhost%2Fcallback",
        );
        assert!(authorize.is_err());

        let unlink = serde_urlencoded::from_str::<UnlinkProviderQuery>(
            "provider=1&providerUserId=remote-user-1",
        );
        assert!(unlink.is_err());
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
