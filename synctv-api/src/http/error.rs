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

/// Map impls-layer error strings to appropriate HTTP status codes.
///
/// Uses the shared `classify_error` function from the impls module for
/// consistent error classification across HTTP and gRPC transports.
///
/// This is the client-facing equivalent of `admin_err_to_app_error` in admin.rs.
pub fn impls_err_to_app_error(err: crate::impls::ApiError) -> AppError {
    AppError::from(err)
}

/// Map a typed `ApiError` to an HTTP `AppError` without string-based classification.
///
/// This provides guaranteed-correct status code mapping for callers that
/// propagate `synctv_core::Error` through the `ApiError` type.
pub fn api_err_to_app_error(err: crate::impls::ApiError) -> AppError {
    AppError::from(err)
}
