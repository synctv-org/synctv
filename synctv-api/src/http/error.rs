// HTTP error handling

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Result type for HTTP handlers
pub type AppResult<T> = Result<T, AppError>;

/// Application error with HTTP status code
#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub message: String,
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    pub fn internal_server_error(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    // Convenience alias
    pub fn internal(message: impl Into<String>) -> Self {
        Self::internal_server_error(message)
    }

    // Common user-facing error messages for consistency
    #[must_use] 
    pub fn invalid_credentials() -> Self {
        Self::unauthorized("Invalid username or password")
    }

    #[must_use] 
    pub fn session_expired() -> Self {
        Self::unauthorized("Your session has expired. Please log in again.")
    }

    #[must_use] 
    pub fn token_invalid() -> Self {
        Self::unauthorized("Invalid or expired token")
    }

    #[must_use] 
    pub fn permission_denied() -> Self {
        Self::forbidden("You do not have permission to perform this action")
    }

    #[must_use] 
    pub fn resource_not_found(resource: &str) -> Self {
        Self::not_found(format!("{resource} not found"))
    }

    #[must_use] 
    pub fn validation_failed(field: &str, reason: &str) -> Self {
        Self::bad_request(format!("Invalid {field}: {reason}"))
    }

    #[must_use] 
    pub fn rate_limited(retry_after: u64) -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many requests. Please try again in {retry_after} seconds."),
        )
    }

    #[must_use] 
    pub fn service_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service temporarily unavailable. Please try again later.",
        )
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

impl std::error::Error for AppError {}

/// Error response JSON structure
#[derive(Debug, Serialize, Deserialize)]
struct ErrorResponse {
    error: String,
    status: u16,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status;
        let body = Json(ErrorResponse {
            error: self.message,
            status: status.as_u16(),
        });

        (status, body).into_response()
    }
}

/// Convert `synctv_core` errors to HTTP errors
impl From<synctv_core::Error> for AppError {
    fn from(err: synctv_core::Error) -> Self {
        use synctv_core::Error;

        match err {
            Error::NotFound(msg) => Self::not_found(msg),
            Error::AlreadyExists(msg) => Self::conflict(msg),
            Error::Authentication(msg) => Self::unauthorized(msg),
            Error::Authorization(msg) => Self::forbidden(msg),
            Error::InvalidInput(msg) => Self::bad_request(msg),
            Error::RateLimited(msg) => Self::new(StatusCode::TOO_MANY_REQUESTS, msg),
            Error::Database(e) => {
                tracing::error!("Database error: {}", e);
                Self::internal_server_error("Database error")
            }
            Error::Redis(e) => {
                tracing::error!("Redis error: {}", e);
                Self::internal_server_error("Service temporarily unavailable")
            }
            Error::Serialization(e) => {
                tracing::error!("Serialization error: {}", e);
                Self::internal_server_error("Data processing error")
            }
            Error::Deserialization { context } => {
                tracing::error!("Deserialization error: {}", context);
                Self::internal_server_error("Data processing error")
            }
            Error::Internal(msg) => {
                tracing::error!("Internal error: {}", msg);
                Self::internal_server_error("Internal server error")
            }
            Error::OptimisticLockConflict => {
                Self::new(StatusCode::CONFLICT, "Resource was modified concurrently, please retry")
            }
        }
    }
}

/// Convert `serde_json` errors to HTTP errors
impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        tracing::error!("JSON serialization/deserialization error: {}", err);
        Self::bad_request("Invalid request data format")
    }
}

/// Convert anyhow errors to HTTP errors
impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        tracing::error!("Anyhow error: {}", err);
        Self::internal_server_error("Internal server error")
    }
}

/// Map a typed `ApiError` to an HTTP `AppError` with guaranteed-correct
/// status code mapping (no keyword-based heuristics).
impl From<crate::impls::ApiError> for AppError {
    fn from(err: crate::impls::ApiError) -> Self {
        use crate::impls::ErrorKind;
        let msg = err.to_string();
        match err.classify() {
            ErrorKind::NotFound => AppError::not_found(msg),
            ErrorKind::Unauthenticated => AppError::unauthorized(msg),
            ErrorKind::PermissionDenied => AppError::forbidden(msg),
            ErrorKind::AlreadyExists => AppError::conflict(msg),
            ErrorKind::InvalidArgument => AppError::bad_request(msg),
            ErrorKind::Internal => {
                tracing::error!("Internal error: {msg}");
                AppError::internal("Internal error")
            }
        }
    }
}

/// Map a typed `ApiError` to an HTTP `AppError`.
///
/// Uses the `From<ApiError> for AppError` impl for guaranteed-correct
/// status code mapping (no keyword-based heuristics).
pub fn map_api_error(err: crate::impls::ApiError) -> AppError {
    AppError::from(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== AppError construction ==========

    #[test]
    fn test_bad_request() {
        let err = AppError::bad_request("invalid field");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert_eq!(err.message, "invalid field");
    }

    #[test]
    fn test_unauthorized() {
        let err = AppError::unauthorized("not logged in");
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "not logged in");
    }

    #[test]
    fn test_forbidden() {
        let err = AppError::forbidden("no access");
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.message, "no access");
    }

    #[test]
    fn test_not_found() {
        let err = AppError::not_found("room not found");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.message, "room not found");
    }

    #[test]
    fn test_conflict() {
        let err = AppError::conflict("already exists");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.message, "already exists");
    }

    #[test]
    fn test_internal_server_error() {
        let err = AppError::internal_server_error("boom");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.message, "boom");
    }

    #[test]
    fn test_internal_alias() {
        let err = AppError::internal("oops");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.message, "oops");
    }

    // ========== Common user-facing errors ==========

    #[test]
    fn test_invalid_credentials() {
        let err = AppError::invalid_credentials();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert!(err.message.contains("Invalid username or password"));
    }

    #[test]
    fn test_session_expired() {
        let err = AppError::session_expired();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert!(err.message.contains("expired"));
    }

    #[test]
    fn test_token_invalid() {
        let err = AppError::token_invalid();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert!(err.message.contains("Invalid"));
    }

    #[test]
    fn test_permission_denied() {
        let err = AppError::permission_denied();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert!(err.message.contains("permission"));
    }

    #[test]
    fn test_resource_not_found() {
        let err = AppError::resource_not_found("Room");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.message, "Room not found");
    }

    #[test]
    fn test_validation_failed() {
        let err = AppError::validation_failed("email", "must contain @");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("email"));
        assert!(err.message.contains("must contain @"));
    }

    #[test]
    fn test_rate_limited() {
        let err = AppError::rate_limited(60);
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(err.message.contains("60 seconds"));
    }

    #[test]
    fn test_service_unavailable() {
        let err = AppError::service_unavailable();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(err.message.contains("temporarily unavailable"));
    }

    // ========== Display trait ==========

    #[test]
    fn test_display() {
        let err = AppError::bad_request("test error");
        let display = err.to_string();
        assert!(display.contains("400"));
        assert!(display.contains("test error"));
    }

    // ========== IntoResponse ==========

    #[test]
    fn test_into_response_status_code() {
        use axum::response::IntoResponse;

        let err = AppError::not_found("missing");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_into_response_bad_request() {
        use axum::response::IntoResponse;

        let err = AppError::bad_request("invalid");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ========== From<synctv_core::Error> ==========

    #[test]
    fn test_from_core_not_found() {
        let core_err = synctv_core::Error::NotFound("room".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_from_core_already_exists() {
        let core_err = synctv_core::Error::AlreadyExists("user".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::CONFLICT);
    }

    #[test]
    fn test_from_core_authentication() {
        let core_err = synctv_core::Error::Authentication("bad token".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_from_core_authorization() {
        let core_err = synctv_core::Error::Authorization("denied".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_from_core_invalid_input() {
        let core_err = synctv_core::Error::InvalidInput("bad field".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_from_core_rate_limited() {
        let core_err = synctv_core::Error::RateLimited("too fast".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn test_from_core_internal() {
        let core_err = synctv_core::Error::Internal("something broke".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::INTERNAL_SERVER_ERROR);
        // Internal error messages should NOT leak to the client
        assert_eq!(app_err.message, "Internal server error");
    }

    #[test]
    fn test_from_core_optimistic_lock() {
        let core_err = synctv_core::Error::OptimisticLockConflict;
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::CONFLICT);
    }

    // ========== From<serde_json::Error> ==========

    #[test]
    fn test_from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json {{{").unwrap_err();
        let app_err = AppError::from(json_err);
        assert_eq!(app_err.status, StatusCode::BAD_REQUEST);
        // Should not leak serde error details
        assert_eq!(app_err.message, "Invalid request data format");
    }
}
