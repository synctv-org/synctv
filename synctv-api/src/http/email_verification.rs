//! Email verification and password reset endpoints
//!
//! Public endpoints for email verification and password recovery.
//! Delegates to shared `EmailApiImpl` to avoid duplicating logic with gRPC handlers.
//!
//! Uses proto-generated types for request/response to ensure type consistency
//! with gRPC handlers.

use axum::{
    extract::State,
    response::Json,
    routing::post,
    Router,
};

use crate::http::{AppState, AppError, AppResult};
use crate::impls::EmailApiImpl;
use crate::proto::client::{
    SendVerificationEmailRequest, SendVerificationEmailResponse,
    ConfirmEmailRequest, ConfirmEmailResponse,
    RequestPasswordResetRequest, RequestPasswordResetResponse,
    ConfirmPasswordResetRequest, ConfirmPasswordResetResponse,
};

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
