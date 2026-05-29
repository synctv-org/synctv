//! Email bind and password reset endpoints
//!
//! Public endpoints for password recovery plus shared email runtime helpers.
//! Delegates to shared `EmailApiImpl` to avoid duplicating logic with gRPC handlers.
//!
//! Uses proto-generated types for request/response to ensure type consistency
//! with gRPC handlers.
//!
//! ## Rate Limiting
//!
//! These endpoints apply two layers of rate limiting:
//! 1. **Per-IP** via explicit `RequestExecutor` calls in each handler.
//! 2. **Per-email** inside shared `EmailApiImpl`, preventing email spam and user
//!    enumeration even when the attacker rotates IPs.
//!
//! HTTP and gRPC therefore share the same application-layer behavior while the
//! transport only extracts metadata and request bodies.

use axum::{extract::State, response::Json, routing::post, Router};
use futures::future::BoxFuture;
use futures::FutureExt;
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use crate::http::{
    error::map_api_error, middleware::RequestMetadata, validation::ProtoJson, AppError, AppResult,
    AppState,
};
use crate::impls::{EmailApiImpl, EndpointRateLimitCategory};
use crate::proto::client::{
    ConfirmPasswordResetResponse, FinishOpaquePasswordResetRequest, RequestPasswordResetRequest,
    RequestPasswordResetResponse, StartOpaquePasswordResetRequest,
    StartOpaquePasswordResetResponse,
};

fn email_api_unavailable_error() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE,
    )
}

fn email_token_service_unavailable_error() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "Email token service is not available on this server.",
    )
}

/// Resolve the shared `EmailApiImpl` from `AppState`, or return an error if email is not configured.
fn require_email_api(state: &AppState) -> Result<&std::sync::Arc<EmailApiImpl>, AppError> {
    state.shared_api_runtime.email_api.as_ref().ok_or_else(|| {
        if state.email_token_service.is_none() {
            email_token_service_unavailable_error()
        } else {
            email_api_unavailable_error()
        }
    })
}

fn request_metadata(request_meta: RequestMetadata) -> crate::impls::RequestMetadata {
    request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT))
}

fn execute_email_endpoint<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    operation: F,
) -> BoxFuture<'a, Result<T, AppError>>
where
    T: Send + 'a,
    F: FnOnce(std::sync::Arc<EmailApiImpl>, synctv_core::provider::ExecutionControl) -> Fut
        + Send
        + 'a,
    Fut: std::future::Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let email_api = require_email_api(state)?.clone();
        let executor = state.shared_api_runtime.request_executor.clone();
        executor
            .execute_public_with_control(
                &request_meta,
                EndpointRateLimitCategory::Email,
                move |request_control| operation(email_api, request_control),
            )
            .await
            .map_err(map_api_error)
    }
    .boxed()
}

/// Create email-related routes.
pub fn create_email_router() -> Router<AppState> {
    Router::new()
        .route("/api/email/password/reset", post(request_password_reset))
        .route(
            "/api/email/password/opaque/start",
            post(start_opaque_password_reset),
        )
        .route(
            "/api/email/password/opaque/finish",
            post(finish_opaque_password_reset),
        )
}

/// Request password reset
///
/// POST /api/email/password/reset
/// Public endpoint - no authentication required
///
/// Rate limited per-email in `EmailApiImpl` and per-client via `RequestExecutor`.
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(req): ProtoJson<RequestPasswordResetRequest>,
) -> AppResult<Json<RequestPasswordResetResponse>> {
    let result = execute_email_endpoint(
        &state,
        request_meta,
        move |email_api, request_control| async move {
            email_api
                .request_password_reset_response_with_control(req, Some(&request_control))
                .await
        },
    )
    .await?;

    Ok(Json(result))
}

/// Start OPAQUE password reset
///
/// POST /api/email/password/opaque/start
/// Public endpoint - no authentication required
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/email/password/opaque/start",
        tag = "Email",
        request_body = StartOpaquePasswordResetRequest,
        responses(
            (status = 200, description = "OPAQUE password reset challenge created", body = StartOpaquePasswordResetResponse),
            (status = 400, description = "Invalid password reset request", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn start_opaque_password_reset(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(req): ProtoJson<StartOpaquePasswordResetRequest>,
) -> AppResult<Json<StartOpaquePasswordResetResponse>> {
    let result = execute_email_endpoint(
        &state,
        request_meta,
        move |email_api, request_control| async move {
            email_api
                .start_opaque_password_reset_response_with_control(req, Some(&request_control))
                .await
        },
    )
    .await?;

    Ok(Json(result))
}

/// Finish OPAQUE password reset
///
/// POST /api/email/password/opaque/finish
/// Public endpoint - no authentication required
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/email/password/opaque/finish",
        tag = "Email",
        request_body = FinishOpaquePasswordResetRequest,
        responses(
            (status = 200, description = "Password reset confirmed", body = ConfirmPasswordResetResponse),
            (status = 400, description = "Invalid password reset confirmation", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn finish_opaque_password_reset(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(req): ProtoJson<FinishOpaquePasswordResetRequest>,
) -> AppResult<Json<ConfirmPasswordResetResponse>> {
    let result = execute_email_endpoint(
        &state,
        request_meta,
        move |email_api, request_control| async move {
            email_api
                .finish_opaque_password_reset_response_with_control(req, Some(&request_control))
                .await
        },
    )
    .await?;

    Ok(Json(result))
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
            synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn test_email_token_service_missing_is_service_unavailable() {
        let err = email_token_service_unavailable_error();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            err.message,
            "Email token service is not available on this server."
        );
    }
}
