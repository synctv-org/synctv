//! Email verification and password reset endpoints
//!
//! Public endpoints for email verification and password recovery.
//! Delegates to shared `EmailApiImpl` to avoid duplicating logic with gRPC handlers.
//!
//! Uses proto-generated types for request/response to ensure type consistency
//! with gRPC handlers.
//!
//! ## Rate Limiting
//!
//! These endpoints apply two layers of rate limiting:
//! 1. **Per-IP** (via `auth_rate_limit` middleware): 5 req/min shared with other auth endpoints.
//! 2. **Per-email** (inside shared `EmailApiImpl`): 3 requests per hour per email address,
//!    preventing email spam and user enumeration even when the attacker rotates IPs.
//!
//! The per-IP middleware limit is applied externally in `register_all_routes`.
//! The per-email limit is enforced by the shared `EmailApiImpl`, so HTTP and gRPC
//! use the exact same application-layer behavior.

use axum::{extract::State, response::Json, routing::post, Router};

use crate::http::{AppError, AppResult, AppState};
use crate::impls::EmailApiImpl;
use crate::proto::client::{
    ConfirmEmailRequest, ConfirmEmailResponse, ConfirmPasswordResetRequest,
    ConfirmPasswordResetResponse, RequestPasswordResetRequest, RequestPasswordResetResponse,
    SendVerificationEmailRequest, SendVerificationEmailResponse,
};

fn email_api_unavailable_error() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "Email service is not available on this server.",
    )
}

fn email_token_service_unavailable_error() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "Email verification service is not available on this server.",
    )
}

/// Resolve the shared `EmailApiImpl` from `AppState`, or return an error if email is not configured.
fn require_email_api(state: &AppState) -> Result<&std::sync::Arc<EmailApiImpl>, AppError> {
    state.email_api.as_ref().ok_or_else(|| {
        if state.email_token_service.is_none() {
            email_token_service_unavailable_error()
        } else {
            email_api_unavailable_error()
        }
    })
}

/// Create email-related routes
///
/// Rate limiting is applied externally in `create_router` where `AppState` is available.
pub fn create_email_router() -> Router<AppState> {
    Router::new()
        .route("/api/email/verify/send", post(send_verification_email))
        .route("/api/email/verify/confirm", post(confirm_email))
        .route("/api/email/password/reset", post(request_password_reset))
        .route("/api/email/password/confirm", post(confirm_password_reset))
}

/// Send verification email
///
/// POST /api/email/verify/send
/// Public endpoint - no authentication required
///
/// Rate limited per-email (3/hour) in addition to per-IP middleware.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/email/verify/send",
        tag = "Email",
        request_body = SendVerificationEmailRequest,
        responses(
            (status = 200, description = "Verification email accepted", body = SendVerificationEmailResponse),
            (status = 400, description = "Invalid email request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn send_verification_email(
    State(state): State<AppState>,
    Json(req): Json<SendVerificationEmailRequest>,
) -> AppResult<Json<SendVerificationEmailResponse>> {
    let email_api = require_email_api(&state)?;

    let result = email_api
        .send_verification_email(&req.email)
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(SendVerificationEmailResponse {
        message: result.message,
    }))
}

/// Confirm email verification
///
/// POST /api/email/verify/confirm
/// Public endpoint - no authentication required
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/email/verify/confirm",
        tag = "Email",
        request_body = ConfirmEmailRequest,
        responses(
            (status = 200, description = "Email confirmed", body = ConfirmEmailResponse),
            (status = 400, description = "Invalid confirmation request", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn confirm_email(
    State(state): State<AppState>,
    Json(req): Json<ConfirmEmailRequest>,
) -> AppResult<Json<ConfirmEmailResponse>> {
    let email_api = require_email_api(&state)?;

    let result = email_api
        .confirm_email(&req.email, &req.token)
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(ConfirmEmailResponse {
        message: result.message,
        user_id: result.user_id,
    }))
}

/// Request password reset
///
/// POST /api/email/password/reset
/// Public endpoint - no authentication required
///
/// Rate limited per-email (3/hour) in addition to per-IP middleware.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/email/password/reset",
        tag = "Email",
        request_body = RequestPasswordResetRequest,
        responses(
            (status = 200, description = "Password reset email accepted", body = RequestPasswordResetResponse),
            (status = 400, description = "Invalid password reset request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn request_password_reset(
    State(state): State<AppState>,
    Json(req): Json<RequestPasswordResetRequest>,
) -> AppResult<Json<RequestPasswordResetResponse>> {
    let email_api = require_email_api(&state)?;

    let result = email_api
        .request_password_reset(&req.email)
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(RequestPasswordResetResponse {
        message: result.message,
    }))
}

/// Confirm password reset
///
/// POST /api/email/password/confirm
/// Public endpoint - no authentication required
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/email/password/confirm",
        tag = "Email",
        request_body = ConfirmPasswordResetRequest,
        responses(
            (status = 200, description = "Password reset confirmed", body = ConfirmPasswordResetResponse),
            (status = 400, description = "Invalid password reset confirmation", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn confirm_password_reset(
    State(state): State<AppState>,
    Json(req): Json<ConfirmPasswordResetRequest>,
) -> AppResult<Json<ConfirmPasswordResetResponse>> {
    let email_api = require_email_api(&state)?;

    let result = email_api
        .confirm_password_reset(&req.email, &req.token, &req.new_password)
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(ConfirmPasswordResetResponse {
        message: result.message,
        user_id: result.user_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn test_email_service_missing_is_service_unavailable() {
        let err = email_api_unavailable_error();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            err.message,
            "Email service is not available on this server."
        );
    }

    #[test]
    fn test_email_token_service_missing_is_service_unavailable() {
        let err = email_token_service_unavailable_error();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            err.message,
            "Email verification service is not available on this server."
        );
    }
}
