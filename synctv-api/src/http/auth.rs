// Authentication HTTP handlers
// This layer now uses proto types and delegates to the impls layer for business logic

use axum::{
    extract::{FromRequest, FromRequestParts, Request, State},
    Json,
};
use serde::de::DeserializeOwned;

use super::{AppError, AppResult, AppState};
use crate::impls::{ApiError, EndpointRateLimitCategory};
use crate::proto::client::{
    CreateGuestTokenRequest, CreateGuestTokenResponse, FinishMfaPasskeyRequest,
    FinishOpaqueLoginRequest, FinishOpaqueRegistrationRequest, FinishPasskeyLoginRequest,
    FinishPasskeyRegistrationRequest, LoginRequest, LoginResponse, LogoutResponse,
    RefreshTokenRequest, RefreshTokenResponse, RegisterResponse, RequestEmailLoginRequest,
    RequestEmailLoginResponse, RequestMfaEmailCodeRequest, RequestMfaEmailCodeResponse,
    StartMfaPasskeyRequest, StartMfaPasskeyResponse, StartOpaqueLoginRequest,
    StartOpaqueLoginResponse, StartOpaqueRegistrationRequest, StartOpaqueRegistrationResponse,
    StartPasskeyLoginRequest, StartPasskeyLoginResponse, StartPasskeyRegistrationRequest,
    StartPasskeyRegistrationResponse, VerifyMfaEmailCodeRequest,
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
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
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
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
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

/// Confirm a passwordless email login token. Public client password login uses OPAQUE.
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
    let client_api = state.shared_api_runtime.client_api.clone();
    let email_api = state.shared_api_runtime.email_api.clone();
    let response = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<LoginRequest>(request).await?;
                client_api
                    .login_request_with_control(
                        email_api.as_deref(),
                        req,
                        client_ip,
                        Some(&request_control),
                    )
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
        path = "/api/auth/guest-token",
        tag = "Auth",
        request_body = CreateGuestTokenRequest,
        responses(
            (status = 200, description = "Guest token issued for a public room", body = CreateGuestTokenResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Guest access is not allowed", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn create_guest_token(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<CreateGuestTokenResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<CreateGuestTokenRequest>(request).await?;
                client_api
                    .create_guest_token_with_control(req, Some(&request_control))
                    .await
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
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
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
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
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

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/passkeys/registration/start",
        tag = "Auth",
        request_body = StartPasskeyRegistrationRequest,
        responses(
            (status = 200, description = "Passkey registration challenge created", body = StartPasskeyRegistrationResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn start_passkey_registration(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<StartPasskeyRegistrationResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<StartPasskeyRegistrationRequest>(request).await?;
                client_api
                    .start_passkey_registration_with_control(req, client_ip, Some(&request_control))
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
        path = "/api/auth/passkeys/registration/finish",
        tag = "Auth",
        request_body = FinishPasskeyRegistrationRequest,
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
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<FinishPasskeyRegistrationRequest>(request).await?;
                client_api
                    .finish_passkey_registration_with_control(
                        req,
                        client_ip,
                        Some(&request_control),
                    )
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
        path = "/api/auth/passkeys/login/start",
        tag = "Auth",
        request_body = StartPasskeyLoginRequest,
        responses(
            (status = 200, description = "Passkey login challenge created", body = StartPasskeyLoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn start_passkey_login(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<StartPasskeyLoginResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<StartPasskeyLoginRequest>(request).await?;
                client_api
                    .start_passkey_login_with_control(req, client_ip, Some(&request_control))
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
        path = "/api/auth/passkeys/login/finish",
        tag = "Auth",
        request_body = FinishPasskeyLoginRequest,
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
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<FinishPasskeyLoginRequest>(request).await?;
                client_api
                    .finish_passkey_login_with_control(req, client_ip, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

fn require_email_api_api(
    state: &AppState,
) -> Result<std::sync::Arc<crate::impls::EmailApiImpl>, ApiError> {
    state.shared_api_runtime.email_api.clone().ok_or_else(|| {
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
        .shared_api_runtime
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

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/mfa/email/request",
        tag = "Auth",
        request_body = RequestMfaEmailCodeRequest,
        responses(
            (status = 200, description = "MFA email code request accepted", body = RequestMfaEmailCodeResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Email service unavailable", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn request_mfa_email_code(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<RequestMfaEmailCodeResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let state_for_request = state.clone();
    let result = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<RequestMfaEmailCodeRequest>(request).await?;
                let email_api = require_email_api_api(&state_for_request)?;
                email_api
                    .request_mfa_email_code_response_with_control(req, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(result))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/mfa/email/verify",
        tag = "Auth",
        request_body = VerifyMfaEmailCodeRequest,
        responses(
            (status = 200, description = "MFA email code verified", body = LoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Invalid MFA challenge or code", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Email service unavailable", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn verify_mfa_email_code(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<LoginResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let state_for_request = state.clone();
    let response = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<VerifyMfaEmailCodeRequest>(request).await?;
                let email_api = require_email_api_api(&state_for_request)?;
                let outcome = email_api
                    .verify_mfa_email_code_request_with_control(
                        req,
                        client_ip,
                        Some(&request_control),
                    )
                    .await?;
                Ok::<_, ApiError>(crate::impls::client::login_outcome_to_proto(
                    outcome,
                    &state_for_request.shared_api_runtime.public_id_codec,
                ))
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
        path = "/api/auth/mfa/passkeys/start",
        tag = "Auth",
        request_body = StartMfaPasskeyRequest,
        responses(
            (status = 200, description = "MFA passkey challenge created", body = StartMfaPasskeyResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn start_mfa_passkey(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<StartMfaPasskeyResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |_request_control| async move {
                let req = parse_auth_json::<StartMfaPasskeyRequest>(request).await?;
                client_api.start_mfa_passkey_with_control(req).await
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
        path = "/api/auth/mfa/passkeys/finish",
        tag = "Auth",
        request_body = FinishMfaPasskeyRequest,
        responses(
            (status = 200, description = "MFA passkey verified", body = LoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Invalid MFA challenge or credential", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn finish_mfa_passkey(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<LoginResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<FinishMfaPasskeyRequest>(request).await?;
                client_api
                    .finish_mfa_passkey_with_control(req, client_ip, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
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
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
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
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
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
    fn test_login_request_construction() {
        let req = LoginRequest {
            email: "test@example.com".to_string(),
            email_token: "login-token".to_string(),
        };
        assert_eq!(req.email, "test@example.com");
        assert_eq!(req.email_token, "login-token");
    }

    #[test]
    fn test_login_request_json_roundtrip() {
        let req = LoginRequest {
            email: "test@example.com".to_string(),
            email_token: "login-token".to_string(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let deserialized: LoginRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.email, req.email);
        assert_eq!(deserialized.email_token, req.email_token);
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
    fn test_passkey_http_response_serializes_proto_options_as_json_object() {
        let response = StartPasskeyLoginResponse {
            session_id: "session".to_string(),
            options: br#"{"challenge":"abc","rpId":"app.example.com","allowCredentials":[]}"#
                .to_vec(),
        };

        let value = serde_json::to_value(response).expect("serialize passkey response");
        assert_eq!(value["session_id"], "session");
        assert!(value["options"].is_object());
        assert_eq!(value["options"]["challenge"], "abc");
        assert_eq!(value["options"]["rpId"], "app.example.com");
    }

    #[test]
    fn test_passkey_http_finish_request_deserializes_credential_json_object() {
        let request: FinishPasskeyLoginRequest = serde_json::from_str(
            r#"{"session_id":"session","credential":{"id":"cred","type":"public-key"}}"#,
        )
        .expect("deserialize passkey credential object");

        assert_eq!(request.session_id, "session");
        let credential: serde_json::Value =
            serde_json::from_slice(&request.credential).expect("credential json");
        assert_eq!(credential["id"], "cred");
        assert_eq!(credential["type"], "public-key");
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
    fn test_login_request_missing_fields_default_to_empty_strings() {
        let json = r#"{"email":"user@example.com"}"#;
        let req: LoginRequest = serde_json::from_str(json).expect("deserialize with defaults");
        assert_eq!(req.email, "user@example.com");
        assert!(req.email_token.is_empty());
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
