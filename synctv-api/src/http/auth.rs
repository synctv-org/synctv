use axum::{
    extract::{FromRequest, FromRequestParts, Request, State},
    Json,
};
use serde::de::DeserializeOwned;

use super::{AppError, AppResult, AppState};
use crate::impls::{ApiError, EndpointRateLimitCategory};
use synctv_proto::client::{
    ConfirmEmailLoginRequest, ConfirmEmailRegistrationRequest, CreateGuestTokenRequest,
    CreateGuestTokenResponse, FinishMfaPasskeyRequest, FinishOpaqueLoginRequest,
    FinishOpaqueRegistrationRequest, FinishPasskeyLoginRequest, FinishPasskeyRegistrationRequest,
    LoginResponse, LogoutResponse, RefreshTokenRequest, RefreshTokenResponse, RegisterResponse,
    RegisterWithDirectPasswordRequest, RequestEmailLoginRequest, RequestEmailLoginResponse,
    RequestEmailRegistrationRequest, RequestEmailRegistrationResponse, RequestMfaEmailCodeRequest,
    RequestMfaEmailCodeResponse, StartMfaPasskeyRequest, StartMfaPasskeyResponse,
    StartOpaqueLoginResponse, StartOpaqueRegistrationRequest, StartOpaqueRegistrationResponse,
    StartPasskeyLoginResponse, StartPasskeyRegistrationRequest, StartPasskeyRegistrationResponse,
    VerifyMfaEmailCodeRequest,
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
) -> Result<std::net::IpAddr, crate::client_ip::ClientIpHeaderError> {
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/direct-password/register",
        tag = "Auth",
        request_body = RegisterWithDirectPasswordRequest,
        responses(
            (status = 200, description = "Direct password registration succeeded", body = RegisterResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn register_with_direct_password(
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
                let req = parse_auth_json::<RegisterWithDirectPasswordRequest>(request).await?;
                client_api
                    .register_with_direct_password_with_control(
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
        path = "/api/auth/direct-password/login",
        tag = "Auth",
        request_body = synctv_proto::http_serde::LoginWithDirectPasswordRequestDef,
        responses(
            (status = 200, description = "Direct password login succeeded", body = LoginResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Invalid credentials", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn login_with_direct_password(
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
                let req = synctv_proto::client::LoginWithDirectPasswordRequest::try_from(
                    parse_auth_json::<synctv_proto::http_serde::LoginWithDirectPasswordRequestDef>(
                        request,
                    )
                    .await?,
                )
                .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
                client_api
                    .login_with_direct_password_with_control(req, client_ip, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Confirm a passwordless email login token.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/email/confirm",
        tag = "Auth",
        request_body = ConfirmEmailLoginRequest,
        responses(
            (status = 200, description = "Email login confirmed", body = LoginResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Invalid credentials", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn confirm_email_login(
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
                let req = parse_auth_json::<ConfirmEmailLoginRequest>(request).await?;
                client_api
                    .confirm_email_login_with_control(
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Guest access is not allowed", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
        request_body = synctv_proto::http_serde::StartOpaqueLoginRequestDef,
        responses(
            (status = 200, description = "OPAQUE login challenge created", body = StartOpaqueLoginResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
                let req = synctv_proto::client::StartOpaqueLoginRequest::try_from(
                    parse_auth_json::<synctv_proto::http_serde::StartOpaqueLoginRequestDef>(
                        request,
                    )
                    .await?,
                )
                .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Invalid credentials", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Invalid credentials", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
        request_body = synctv_proto::http_serde::StartPasskeyLoginRequestDef,
        responses(
            (status = 200, description = "Passkey login challenge created", body = StartPasskeyLoginResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
                let req = synctv_proto::client::StartPasskeyLoginRequest::try_from(
                    parse_auth_json::<synctv_proto::http_serde::StartPasskeyLoginRequestDef>(
                        request,
                    )
                    .await?,
                )
                .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Invalid credentials", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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

fn require_email_api(
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
                let email_api = require_email_api(&state_for_request)?;
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
        path = "/api/auth/email/registration/request",
        tag = "Auth",
        request_body = RequestEmailRegistrationRequest,
        responses(
            (status = 200, description = "Email registration request accepted", body = RequestEmailRegistrationResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn request_email_registration(
    State(state): State<AppState>,
    request: Request,
) -> AppResult<Json<RequestEmailRegistrationResponse>> {
    let (request_meta, request) = extract_auth_request(&state, request).await?;
    let client_ip = request_meta.client_ip;
    let state_for_request = state.clone();
    let result = state
        .shared_api_runtime
        .client_api
        .execute_public_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |request_control| async move {
                let req = parse_auth_json::<RequestEmailRegistrationRequest>(request).await?;
                let email_api = require_email_api(&state_for_request)?;
                email_api
                    .request_email_registration_with_control(req, client_ip, Some(&request_control))
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(RequestEmailRegistrationResponse {
        message: result.message,
    }))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/email/registration/confirm",
        tag = "Auth",
        request_body = ConfirmEmailRegistrationRequest,
        responses(
            (status = 200, description = "Email registration confirmed", body = RegisterResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn confirm_email_registration(
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
                let req = parse_auth_json::<ConfirmEmailRegistrationRequest>(request).await?;
                client_api
                    .confirm_email_registration_with_direct_password_with_control(
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
        path = "/api/auth/mfa/email/request",
        tag = "Auth",
        request_body = RequestMfaEmailCodeRequest,
        responses(
            (status = 200, description = "MFA email code request accepted", body = RequestMfaEmailCodeResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Email service unavailable", body = synctv_proto::client::ApiErrorResponse)
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
                let email_api = require_email_api(&state_for_request)?;
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Invalid MFA challenge or code", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Email service unavailable", body = synctv_proto::client::ApiErrorResponse)
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
                let email_api = require_email_api(&state_for_request)?;
                let outcome = email_api
                    .verify_mfa_email_code_request_with_control(
                        req,
                        client_ip,
                        Some(&request_control),
                    )
                    .await?;
                crate::impls::client::login_outcome_to_proto(
                    outcome,
                    &state_for_request.shared_api_runtime.public_id_codec,
                )
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Invalid MFA challenge or credential", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 401, description = "Invalid refresh token", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 401, description = "Missing or invalid bearer token", body = synctv_proto::client::ApiErrorResponse)
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

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn test_passkey_http_response_serializes_proto_options_as_json_object() -> TestResult {
        let response = StartPasskeyLoginResponse {
            session_id: "session".to_string(),
            options: br#"{"challenge":"abc","rpId":"app.example.com","allowCredentials":[]}"#
                .to_vec(),
        };

        let value = serde_json::to_value(response)?;
        assert_eq!(value["session_id"], "session");
        assert!(value["options"].is_object());
        assert_eq!(value["options"]["challenge"], "abc");
        assert_eq!(value["options"]["rpId"], "app.example.com");
        Ok(())
    }

    #[test]
    fn test_passkey_http_finish_request_deserializes_credential_json_object() -> TestResult {
        let request: FinishPasskeyLoginRequest = serde_json::from_str(
            r#"{"session_id":"session","credential":{"id":"cred","type":"public-key"}}"#,
        )?;

        assert_eq!(request.session_id, "session");
        let credential: serde_json::Value = serde_json::from_slice(&request.credential)?;
        assert_eq!(credential["id"], "cred");
        assert_eq!(credential["type"], "public-key");
        Ok(())
    }
}
