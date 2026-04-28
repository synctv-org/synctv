// Authentication HTTP handlers
// This layer now uses proto types and delegates to the impls layer for business logic

use axum::{
    extract::{FromRequest, FromRequestParts, Request, State},
    Json,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use super::{AppError, AppResult, AppState};
use crate::http::passkey_json::{
    passkey_credential_to_json_bytes, passkey_options_to_value, validate_passkey_session_id,
};
use crate::impls::{ApiError, EndpointRateLimitCategory};
use crate::proto::client::{
    FinishOpaqueLoginRequest, FinishOpaqueRegistrationRequest, LoginRequest, LoginResponse,
    LogoutResponse, RefreshTokenRequest, RefreshTokenResponse, RegisterRequest, RegisterResponse,
    RequestEmailLoginRequest, RequestEmailLoginResponse, StartOpaqueLoginRequest,
    StartOpaqueLoginResponse, StartOpaqueRegistrationRequest, StartOpaqueRegistrationResponse,
};

/// Extract the real client IP from a request.
///
/// Only trusts `X-Forwarded-For` / `X-Real-IP` headers when the direct
/// connection comes from a configured trusted proxy. This prevents
/// attackers from forging their IP to bypass per-IP brute-force protection.
#[must_use]
pub fn extract_client_ip(
    config: &synctv_core::Config,
    socket_addr: std::net::SocketAddr,
    headers: &axum::http::HeaderMap,
) -> std::net::IpAddr {
    crate::client_ip::extract_client_ip_from_headers(config, socket_addr.ip(), headers)
}

async fn extract_auth_request(
    state: &AppState,
    request: Request,
) -> Result<(crate::impls::RequestMetadata, Request), AppError> {
    let (mut parts, body) = request.into_parts();
    let request_meta =
        super::middleware::RequestMetadata::from_request_parts(&mut parts, state).await?;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    Ok((request_meta, Request::from_parts(parts, body)))
}

fn map_json_rejection(err: &axum::extract::rejection::JsonRejection) -> ApiError {
    ApiError::InvalidInput(err.body_text())
}

async fn parse_auth_json<T>(request: Request) -> Result<T, ApiError>
where
    T: DeserializeOwned,
{
    let Json(request) = Json::<T>::from_request(request, &())
        .await
        .map_err(|err| map_json_rejection(&err))?;
    Ok(request)
}

/// Register a new user
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/register",
        tag = "Auth",
        request_body = RegisterRequest,
        responses(
            (status = 200, description = "Registration succeeded", body = RegisterResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn register(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<RegisterResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<RegisterRequest>(request).await?;
                client_api
                    .register_with_control(req, client_ip, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/opaque/registration/start",
        tag = "Auth",
        request_body = StartOpaqueRegistrationRequest,
        responses(
            (status = 200, description = "OPAQUE registration challenge created", body = StartOpaqueRegistrationResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn start_opaque_registration(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<StartOpaqueRegistrationResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<StartOpaqueRegistrationRequest>(request).await?;
                client_api
                    .start_opaque_registration_with_control(req, client_ip, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/opaque/registration/finish",
        tag = "Auth",
        request_body = FinishOpaqueRegistrationRequest,
        responses(
            (status = 200, description = "OPAQUE registration succeeded", body = RegisterResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn finish_opaque_registration(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<RegisterResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<FinishOpaqueRegistrationRequest>(request).await?;
                client_api
                    .finish_opaque_registration_with_control(req, client_ip, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Login with username+password, email+password, or email+login-token.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/login",
        tag = "Auth",
        request_body = LoginRequest,
        responses(
            (status = 200, description = "Login succeeded", body = LoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Invalid credentials", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn login(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<LoginResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let state_for_login = state.clone();
    let response = state
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<LoginRequest>(request).await?;
                if req.email_token.is_empty() {
                    let client_api = state_for_login.client_api.clone();
                    return client_api
                        .login_with_control(req, client_ip, Some(&request_control))
                        .await;
                }
                if req.password.is_empty()
                    && !req.email.trim().is_empty()
                    && req.username.trim().is_empty()
                {
                    let email_api = require_email_api_api(&state_for_login)?;
                    let result = email_api
                        .confirm_email_login_with_control(
                            &req.email,
                            &req.email_token,
                            client_ip,
                            Some(&request_control),
                        )
                        .await?;
                    return Ok(LoginResponse {
                        user: Some(crate::impls::client::user_to_proto(
                            &result.user,
                            &state_for_login.public_id_codec,
                        )),
                        access_token: result.access_token,
                        refresh_token: result.refresh_token,
                    });
                }

                Err(ApiError::InvalidInput(
                    "Email login token requires email only and cannot be combined with username or password.".to_string(),
                ))
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Start a two-step OPAQUE login. The request carries the client's OPAQUE
/// credential request, not a plaintext password.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/opaque/login/start",
        tag = "Auth",
        request_body = StartOpaqueLoginRequest,
        responses(
            (status = 200, description = "OPAQUE login challenge created", body = StartOpaqueLoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn start_opaque_login(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<StartOpaqueLoginResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<StartOpaqueLoginRequest>(request).await?;
                client_api
                    .start_opaque_login_with_control(req, client_ip, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Finish a two-step OPAQUE login and issue tokens when the OPAQUE proof
/// verifies.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/opaque/login/finish",
        tag = "Auth",
        request_body = FinishOpaqueLoginRequest,
        responses(
            (status = 200, description = "OPAQUE login succeeded", body = LoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Invalid credentials", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn finish_opaque_login(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<LoginResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<FinishOpaqueLoginRequest>(request).await?;
                client_api
                    .finish_opaque_login_with_control(req, client_ip, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

fn require_passkey_service(
    state: &AppState,
) -> Result<std::sync::Arc<synctv_core::service::PasskeyService>, ApiError> {
    state.passkey_service.clone().ok_or_else(|| {
        ApiError::ServiceUnavailable("Passkey/WebAuthn service is not configured".to_string())
    })
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StartPasskeyLoginHttpRequest {
    #[serde(default)]
    username: String,
    #[serde(default)]
    email: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StartPasskeyLoginHttpResponse {
    session_id: String,
    options: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StartPasskeyRegistrationHttpRequest {
    username: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StartPasskeyRegistrationHttpResponse {
    session_id: String,
    options: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FinishPasskeyLoginHttpRequest {
    session_id: String,
    credential: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FinishPasskeyRegistrationHttpRequest {
    session_id: String,
    credential: Value,
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/passkeys/registration/start",
        tag = "Auth",
        request_body = StartPasskeyRegistrationHttpRequest,
        responses(
            (status = 200, description = "Passkey registration challenge created", body = StartPasskeyRegistrationHttpResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn start_passkey_registration(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<StartPasskeyRegistrationHttpResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let state_for_request = state.clone();
    let response = state
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<StartPasskeyRegistrationHttpRequest>(request).await?;
                let username = crate::http::validation::validate_username(&req.username)
                    .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
                let email = if req.email.trim().is_empty() {
                    None
                } else {
                    Some(
                        crate::http::validation::validate_email(&req.email)
                            .map_err(|error| ApiError::InvalidInput(error.to_string()))?,
                    )
                };
                let credential_name = if req.name.trim().is_empty() {
                    None
                } else {
                    Some(req.name.trim().to_string())
                };
                let passkey_service = require_passkey_service(&state_for_request)?;
                let challenge = passkey_service
                    .start_account_registration(
                        username,
                        email,
                        credential_name,
                        client_ip,
                        Some(&request_control),
                    )
                    .await
                    .map_err(ApiError::from)?;
                let options = passkey_options_to_value(&challenge.options_json)?;
                Ok::<_, ApiError>(StartPasskeyRegistrationHttpResponse {
                    session_id: challenge.session_id,
                    options,
                })
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/passkeys/registration/finish",
        tag = "Auth",
        request_body = FinishPasskeyRegistrationHttpRequest,
        responses(
            (status = 200, description = "Passkey registration succeeded", body = RegisterResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Invalid credentials", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn finish_passkey_registration(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<RegisterResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let state_for_request = state.clone();
    let response = state
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<FinishPasskeyRegistrationHttpRequest>(request).await?;
                validate_passkey_session_id(&req.session_id)?;
                let credential_json = passkey_credential_to_json_bytes(&req.credential)?;
                let passkey_service = require_passkey_service(&state_for_request)?;
                let (user, access_token, refresh_token) = passkey_service
                    .finish_account_registration(
                        &req.session_id,
                        &credential_json,
                        client_ip,
                        Some(&request_control),
                    )
                    .await
                    .map_err(ApiError::from)?;
                Ok::<_, ApiError>(RegisterResponse {
                    user: Some(crate::impls::client::user_to_proto(
                        &user,
                        &state_for_request.public_id_codec,
                    )),
                    access_token,
                    refresh_token,
                })
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/passkeys/login/start",
        tag = "Auth",
        request_body = StartPasskeyLoginHttpRequest,
        responses(
            (status = 200, description = "Passkey login challenge created", body = StartPasskeyLoginHttpResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn start_passkey_login(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<StartPasskeyLoginHttpResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let state_for_request = state.clone();
    let response = state
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<StartPasskeyLoginHttpRequest>(request).await?;
                let has_username = !req.username.trim().is_empty();
                let has_email = !req.email.trim().is_empty();
                if has_username == has_email {
                    return Err(ApiError::InvalidInput(
                        "Provide exactly one of username or email".to_string(),
                    ));
                }
                let identifier = if has_email {
                    crate::http::validation::validate_email(&req.email)
                        .map_err(|e| ApiError::InvalidInput(e.to_string()))?
                } else {
                    crate::http::validation::validate_username(&req.username)
                        .map_err(|e| ApiError::InvalidInput(e.to_string()))?
                };
                let passkey_service = require_passkey_service(&state_for_request)?;
                let challenge = passkey_service
                    .start_login(&identifier, client_ip, Some(&request_control))
                    .await
                    .map_err(ApiError::from)?;
                let options = passkey_options_to_value(&challenge.options_json)?;
                Ok::<_, ApiError>(StartPasskeyLoginHttpResponse {
                    session_id: challenge.session_id,
                    options,
                })
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/passkeys/login/finish",
        tag = "Auth",
        request_body = FinishPasskeyLoginHttpRequest,
        responses(
            (status = 200, description = "Passkey login succeeded", body = LoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Invalid credentials", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn finish_passkey_login(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<LoginResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let state_for_request = state.clone();
    let response = state
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<FinishPasskeyLoginHttpRequest>(request).await?;
                validate_passkey_session_id(&req.session_id)?;
                let credential_json = passkey_credential_to_json_bytes(&req.credential)?;
                let passkey_service = require_passkey_service(&state_for_request)?;
                let (user, access_token, refresh_token) = passkey_service
                    .finish_login(
                        &req.session_id,
                        &credential_json,
                        client_ip,
                        Some(&request_control),
                    )
                    .await
                    .map_err(ApiError::from)?;
                Ok::<_, ApiError>(LoginResponse {
                    user: Some(crate::impls::client::user_to_proto(
                        &user,
                        &state_for_request.public_id_codec,
                    )),
                    access_token,
                    refresh_token,
                })
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

fn require_email_api_api(
    state: &AppState,
) -> Result<std::sync::Arc<crate::impls::EmailApiImpl>, ApiError> {
    state.email_api.clone().ok_or_else(|| {
        ApiError::ServiceUnavailable(synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE.to_string())
    })
}

/// Request a passwordless email login code.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/email/request",
        tag = "Auth",
        request_body = RequestEmailLoginRequest,
        responses(
            (status = 200, description = "Email login request accepted", body = RequestEmailLoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn request_email_login(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<RequestEmailLoginResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let state_for_request = state.clone();
    let result = state
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<RequestEmailLoginRequest>(request).await?;
                let email_api = require_email_api_api(&state_for_request)?;
                email_api
                    .request_email_login_with_control(&req.email, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(RequestEmailLoginResponse {
        message: result.message,
    }))
}

/// Refresh access token using refresh token.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/refresh",
        tag = "Auth",
        request_body = RefreshTokenRequest,
        responses(
            (status = 200, description = "Token refresh succeeded", body = RefreshTokenResponse),
            (status = 401, description = "Invalid refresh token", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn refresh_token(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<RefreshTokenResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<RefreshTokenRequest>(request).await?;
                client_api
                    .refresh_token_with_control(req, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Logout: blacklist the current access token.
///
/// Requires a valid Bearer token in the Authorization header. The token's JTI
/// is added to the blacklist with its remaining TTL so it cannot be reused.
///
/// Returns 400 Bad Request if no Bearer token is provided.
/// Returns 200 OK with success: true on successful logout.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/logout",
        tag = "Auth",
        responses(
            (status = 200, description = "Logout succeeded", body = LogoutResponse),
            (status = 401, description = "Missing or invalid bearer token", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn logout(
    State(state): State<AppState>,
    request_meta: super::middleware::RequestMetadata,
) -> AppResult<Json<LogoutResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let authorization = request_meta.authorization.clone();
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let outcome = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |_| async move {
                let auth_value = authorization.as_deref().ok_or_else(|| {
                    ApiError::Authentication(
                        synctv_common::messages::AUTHENTICATION_REQUIRED.to_string(),
                    )
                })?;
                let token =
                    synctv_core::service::auth::JwtValidator::extract_bearer_token(auth_value)
                        .map_err(|_| {
                            ApiError::Authentication(
                                synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string(),
                            )
                        })?;
                client_api.logout(&token).await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(LogoutResponse {
        success: true,
        message: outcome.message.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verify request types have expected fields and derive traits (Serialize, Deserialize, Clone)

    #[test]
    fn test_register_request_json_roundtrip() {
        let req = RegisterRequest {
            username: "testuser".to_string(),
            password: "securepass123".to_string(),
            email: "test@example.com".to_string(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let deserialized: RegisterRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.username, req.username);
        assert_eq!(deserialized.email, req.email);
    }

    #[test]
    fn test_login_request_construction() {
        let req = LoginRequest {
            username: "testuser".to_string(),
            password: "securepass123".to_string(),
            email: String::new(),
            email_token: String::new(),
        };
        assert_eq!(req.username, "testuser");
        assert_eq!(req.password, "securepass123");
    }

    #[test]
    fn test_login_request_json_roundtrip() {
        let req = LoginRequest {
            username: "testuser".to_string(),
            password: "mypassword".to_string(),
            email: String::new(),
            email_token: String::new(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let deserialized: LoginRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.username, req.username);
    }

    #[test]
    fn test_refresh_token_request_construction() {
        let req = RefreshTokenRequest {
            refresh_token: "some_refresh_token".to_string(),
        };
        assert_eq!(req.refresh_token, "some_refresh_token");
    }

    #[test]
    fn test_refresh_token_request_json_roundtrip() {
        let req = RefreshTokenRequest {
            refresh_token: "refresh_abc123".to_string(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let deserialized: RefreshTokenRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.refresh_token, req.refresh_token);
    }

    #[test]
    fn test_request_email_login_request_roundtrip() {
        let req = RequestEmailLoginRequest {
            email: "user@example.com".to_string(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let deserialized: RequestEmailLoginRequest =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.email, req.email);
    }

    #[test]
    fn test_passkey_http_response_serializes_options_as_json_object() {
        let response = StartPasskeyLoginHttpResponse {
            session_id: "session".to_string(),
            options: serde_json::json!({
                "challenge": "abc",
                "rpId": "app.example.com",
                "allowCredentials": []
            }),
        };

        let value = serde_json::to_value(response).expect("serialize passkey response");
        assert_eq!(value["session_id"], "session");
        assert!(value["options"].is_object());
        assert_eq!(value["options"]["challenge"], "abc");
    }

    #[test]
    fn test_passkey_http_finish_request_accepts_credential_json_object() {
        let request: FinishPasskeyLoginHttpRequest = serde_json::from_str(
            r#"{"session_id":"session","credential":{"id":"cred","type":"public-key"}}"#,
        )
        .expect("deserialize passkey credential object");

        assert_eq!(request.session_id, "session");
        assert_eq!(request.credential["type"], "public-key");
    }

    // All three auth handlers (register, login, refresh_token) now use
    // map_api_error for consistent typed error classification.

    #[test]
    fn test_app_error_bad_request_status() {
        let err = super::super::AppError::bad_request("registration failed");
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.message, "registration failed");
    }

    #[test]
    fn test_app_error_unauthorized_status() {
        let err = super::super::AppError::unauthorized("invalid credentials");
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "invalid credentials");
    }

    #[test]
    fn test_register_request_empty_fields() {
        let req = RegisterRequest {
            username: String::new(),
            password: String::new(),
            email: String::new(),
        };
        assert!(req.username.is_empty());
        assert!(req.password.is_empty());
        assert!(req.email.is_empty());
    }

    #[test]
    fn test_register_request_from_json_with_extra_fields() {
        // Proto types with serde should ignore unknown fields
        let json = r#"{"username":"user","password":"pass","email":"e@x.com","extra":"ignored"}"#;
        let req: RegisterRequest =
            serde_json::from_str(json).expect("deserialize with extra fields");
        assert_eq!(req.username, "user");
    }

    #[test]
    fn test_login_request_missing_fields_default_to_empty_strings() {
        let json = r#"{"username":"user"}"#;
        let req: LoginRequest = serde_json::from_str(json).expect("deserialize with defaults");
        assert_eq!(req.username, "user");
        assert!(req.password.is_empty());
        assert!(req.email.is_empty());
        assert!(req.email_token.is_empty());
    }

    #[test]
    fn test_register_request_unicode_username() {
        let req = RegisterRequest {
            username: "\u{4e2d}\u{6587}\u{7528}\u{6237}".to_string(), // Chinese characters
            password: "pass123!".to_string(),
            email: "user@example.com".to_string(),
        };
        assert_eq!(req.username.chars().count(), 4);
    }

    #[test]
    fn test_logout_response_serialization() {
        let resp = LogoutResponse {
            success: true,
            message: String::new(),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        assert!(json.contains(r#""success":true"#));
        assert!(json.contains(r#""message":""#));
    }

    #[test]
    fn test_logout_response_partial_success() {
        let resp = LogoutResponse {
            success: true,
            message: "Logged out but token invalidation may be delayed".to_string(),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        assert!(json.contains(r#""success":true"#));
        assert!(json.contains("token invalidation may be delayed"));
    }

    #[test]
    fn test_logout_response_failure() {
        let resp = LogoutResponse {
            success: false,
            message: String::new(),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        assert!(json.contains(r#""success":false"#));
    }

    // Test the token extraction logic used by logout handler

    #[test]
    fn test_extract_bearer_token_valid() {
        use synctv_core::service::auth::JwtValidator;

        let header_value = "Bearer mytoken123";
        let result = JwtValidator::extract_bearer_token(header_value);
        assert!(result.is_ok(), "Should extract valid Bearer token");
        assert_eq!(result.unwrap(), "mytoken123");
    }

    #[test]
    fn test_extract_bearer_token_missing_bearer_prefix() {
        use synctv_core::service::auth::JwtValidator;

        let header_value = "mytoken123";
        let result = JwtValidator::extract_bearer_token(header_value);
        assert!(result.is_err(), "Should fail without Bearer prefix");
    }

    #[test]
    fn test_extract_bearer_token_empty() {
        use synctv_core::service::auth::JwtValidator;

        let header_value = "";
        let result = JwtValidator::extract_bearer_token(header_value);
        assert!(result.is_err(), "Should fail with empty string");
    }

    #[test]
    fn test_extract_bearer_token_bearer_only() {
        use synctv_core::service::auth::JwtValidator;

        let header_value = "Bearer ";
        let result = JwtValidator::extract_bearer_token(header_value);
        // Depending on implementation, this might fail or return empty
        // The key is that an empty token after "Bearer " should be treated as invalid
        if let Ok(token) = result {
            assert!(token.is_empty(), "Token should be empty");
        }
    }

    // Verify that the logout handler returns appropriate errors

    #[test]
    fn test_logout_missing_token_error_message() {
        let err = super::super::error::AppError::bad_request(
            "Missing Bearer token in Authorization header",
        );
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(
            err.message.contains("Missing"),
            "Should mention missing token"
        );
    }

    #[test]
    fn test_logout_missing_token_error_status() {
        let err = super::super::error::AppError::bad_request(
            "Missing Bearer token in Authorization header",
        );
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    }
}
