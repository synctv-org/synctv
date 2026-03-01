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
//! 2. **Per-email** (handler-level): 3 requests per hour per email address, preventing
//!    email spam and user enumeration even when the attacker rotates IPs.
//!
//! The per-IP middleware limit is applied externally in `register_all_routes`.
//! The per-email limit is checked inside the handlers that actually send emails
//! (`send_verification_email` and `request_password_reset`).

use axum::{extract::State, response::Json, routing::post, Router};
use synctv_core::service::rate_limit::RateLimitError;

use crate::http::{AppError, AppResult, AppState};
use crate::impls::EmailApiImpl;
use crate::proto::client::{
    ConfirmEmailRequest, ConfirmEmailResponse, ConfirmPasswordResetRequest,
    ConfirmPasswordResetResponse, RequestPasswordResetRequest, RequestPasswordResetResponse,
    SendVerificationEmailRequest, SendVerificationEmailResponse,
};

/// Per-email rate limit: 3 requests per hour.
/// Prevents email spam even when the attacker rotates source IPs.
const EMAIL_ADDR_MAX_REQUESTS: u32 = 3;
const EMAIL_ADDR_WINDOW_SECONDS: u64 = 3600;

/// Build an `EmailApiImpl` from `AppState`, or return an error if email is not configured.
fn require_email_api(state: &AppState) -> Result<EmailApiImpl, AppError> {
    let email_service = state
        .email_service
        .as_ref()
        .ok_or_else(|| AppError::bad_request("Email service not configured"))?;

    // Use the shared EmailTokenService from AppState (created once at startup)
    let email_token_service = state
        .email_token_service
        .as_ref()
        .ok_or_else(|| AppError::bad_request("Email token service not configured"))?;

    Ok(EmailApiImpl::new(
        state.user_service.clone(),
        email_service.clone(),
        email_token_service.clone(),
    ))
}

/// Check per-email rate limit. Returns an `AppError` with 429 and Retry-After
/// if the limit is exceeded.
async fn check_email_rate_limit(state: &AppState, email: &str) -> Result<(), AppError> {
    let normalized = email.to_lowercase();
    let key = format!("email:addr:{normalized}");
    match state
        .rate_limiter
        .check_rate_limit(&key, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
        .await
    {
        Ok(()) => Ok(()),
        Err(RateLimitError::RateLimitExceeded {
            retry_after_seconds,
        }) => Err(AppError::rate_limited(retry_after_seconds)),
        Err(_) => {
            // Unexpected backend error - fail closed
            Err(AppError::rate_limited(1))
        }
    }
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
pub async fn send_verification_email(
    State(state): State<AppState>,
    Json(req): Json<SendVerificationEmailRequest>,
) -> AppResult<Json<SendVerificationEmailResponse>> {
    // Per-email rate limit (handler-level, in addition to per-IP middleware)
    check_email_rate_limit(&state, &req.email).await?;

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
pub async fn request_password_reset(
    State(state): State<AppState>,
    Json(req): Json<RequestPasswordResetRequest>,
) -> AppResult<Json<RequestPasswordResetResponse>> {
    // Per-email rate limit (handler-level, in addition to per-IP middleware)
    check_email_rate_limit(&state, &req.email).await?;

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
