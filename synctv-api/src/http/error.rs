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
    /// Optional application-level error code from `impls::error_codes`.
    /// When set, this is included in the JSON error response for programmatic handling.
    pub error_code: Option<i32>,
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            error_code: None,
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

    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, message)
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

    #[must_use]
    pub fn missing_authorization_header() -> Self {
        Self::unauthorized("Missing Authorization header")
    }

    #[must_use]
    pub fn invalid_authorization_header() -> Self {
        Self::unauthorized("Invalid Authorization header")
    }

    #[must_use]
    pub fn invalid_authorization_header_non_utf8() -> Self {
        Self::unauthorized("Invalid Authorization header: non-UTF-8 value")
    }

    #[must_use]
    pub fn invalid_or_expired_token() -> Self {
        Self::unauthorized("Invalid or expired token")
    }

    #[must_use]
    pub fn invalid_or_expired_ticket() -> Self {
        Self::unauthorized("Invalid or expired ticket")
    }
}

#[must_use]
pub fn map_auth_authorization_error(err: &synctv_core::Error) -> AppError {
    match err {
        synctv_core::Error::Authorization(message) => AppError::forbidden(message.clone()),
        synctv_core::Error::EmailNotVerified => {
            AppError::forbidden("Email not verified. Please verify your email to continue.")
        }
        other => {
            tracing::error!(error = %other, "Unexpected authorization-classified auth error");
            AppError::forbidden("You do not have permission to perform this action")
        }
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
    /// Application-level error code for programmatic handling.
    /// Only present when the error originates from the impls layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<i32>,
    /// Request ID for correlating the error with the request.
    /// Present when the request has passed through `request_id_middleware`.
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status;

        // For server-side failures, sanitize details while preserving retryability /
        // upstream failure semantics for native clients.
        let error_message = if status.is_server_error() {
            tracing::error!(
                status = status.as_u16(),
                original_message = %self.message,
                error_code = ?self.error_code,
                "Server error response"
            );

            match status {
                StatusCode::SERVICE_UNAVAILABLE => {
                    "Service temporarily unavailable. Please try again later.".to_string()
                }
                StatusCode::BAD_GATEWAY => "Upstream service error".to_string(),
                StatusCode::GATEWAY_TIMEOUT => "Upstream service timed out".to_string(),
                _ => "Internal server error".to_string(),
            }
        } else {
            self.message
        };

        let request_id = crate::http::middleware::CURRENT_REQUEST_ID
            .try_with(Clone::clone)
            .ok();

        let body = Json(ErrorResponse {
            error: error_message,
            status: status.as_u16(),
            code: self.error_code,
            request_id,
        });

        (status, body).into_response()
    }
}

/// Convert `ProviderError` to HTTP errors with standard `AppError` format.
///
/// This replaces the old `error_response(parse_provider_error(...))` pattern
/// which used string matching and returned a non-standard JSON format.
impl From<synctv_core::provider::ProviderError> for AppError {
    fn from(err: synctv_core::provider::ProviderError) -> Self {
        use synctv_core::provider::ProviderError;
        match err {
            ProviderError::NetworkError(msg) => Self::new(StatusCode::BAD_GATEWAY, msg),
            ProviderError::ApiError(msg) => Self::new(StatusCode::BAD_GATEWAY, msg),
            ProviderError::UpstreamHttp { status, .. } => {
                tracing::warn!(status = status, "Upstream HTTP error");
                if status == 401 || status == 403 {
                    Self::unauthorized("Provider authentication failed")
                } else if status == 404 {
                    Self::not_found("Provider resource not found")
                } else if status == 408 || status == 429 || status >= 500 {
                    Self::new(
                        StatusCode::BAD_GATEWAY,
                        "Upstream provider service is temporarily unavailable.",
                    )
                } else {
                    Self::bad_request("Upstream provider rejected the request.")
                }
            }
            ProviderError::ParseError(msg) => Self::bad_request(msg),
            ProviderError::InvalidConfig(msg) => Self::bad_request(msg),
            ProviderError::InvalidUrl(msg) => Self::bad_request(msg),
            ProviderError::MissingField(msg) => Self::bad_request(msg),
            ProviderError::NotFound => Self::not_found("Resource not found"),
            ProviderError::InstanceNotFound(msg) => Self::not_found(msg),
            ProviderError::MissingInstance => Self::not_found("Provider instance not configured"),
            ProviderError::AuthRequired => Self::unauthorized("Authentication required"),
            ProviderError::CredentialRequired => Self::unauthorized("Credential required"),
            ProviderError::InvalidCredentialType => Self::bad_request("Invalid credential type"),
            ProviderError::UnsupportedFormat(msg) => Self::bad_request(msg),
            ProviderError::RouteRegistrationFailed(msg) => {
                tracing::error!("Route registration failed: {}", msg);
                Self::internal("Provider route registration failed")
            }
            ProviderError::IoError(e) => {
                tracing::error!("Provider IO error: {}", e);
                Self::internal("Provider IO error")
            }
            ProviderError::JsonError(e) => {
                tracing::error!("Provider JSON error: {}", e);
                Self::bad_request("Invalid data format")
            }
            ProviderError::EncryptionRequired(msg) => {
                tracing::error!("Provider encryption required: {}", msg);
                Self::internal_server_error("Credential encryption not configured")
            }
            ProviderError::CredentialNotFound(msg) => Self::not_found(msg),
            ProviderError::CredentialExpired(msg) => Self::unauthorized(msg),
            ProviderError::Internal(msg) => {
                tracing::error!("Provider internal error: {}", msg);
                Self::internal("Provider internal error")
            }
        }
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
            Error::EmailNotVerified => {
                Self::forbidden("Email not verified. Please verify your email to continue.")
            }
            Error::Authorization(msg) => Self::forbidden(msg),
            Error::InvalidInput(msg) => Self::bad_request(msg),
            Error::RateLimited(msg) => Self::new(StatusCode::TOO_MANY_REQUESTS, msg),
            Error::ServiceUnavailable(msg) => {
                tracing::warn!("Service unavailable: {}", msg);
                Self::service_unavailable()
            }
            Error::LockConflict(msg) => Self::new(
                StatusCode::CONFLICT,
                format!("Resource is being modified concurrently, please retry: {msg}"),
            ),
            Error::Database(e) => {
                tracing::error!("Database error: {}", e);
                Self::service_unavailable()
            }
            Error::Redis(e) => {
                tracing::error!("Redis error: {}", e);
                Self::service_unavailable()
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
            Error::OptimisticLockConflict => Self::new(
                StatusCode::CONFLICT,
                "Resource was modified concurrently, please retry",
            ),
            Error::Timeout(msg) => {
                tracing::warn!("Backend timeout: {}", msg);
                Self::service_unavailable()
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
        let error_code = err.code();
        let msg = err.message().to_string();
        let mut app_err = match err.classify() {
            ErrorKind::NotFound => Self::not_found(msg),
            ErrorKind::Unauthenticated => Self::unauthorized(msg),
            ErrorKind::PermissionDenied => Self::forbidden(msg),
            ErrorKind::AlreadyExists => Self::conflict(msg),
            ErrorKind::InvalidArgument => Self::bad_request(msg),
            ErrorKind::RateLimited => Self::too_many_requests(msg),
            ErrorKind::ServiceUnavailable => Self::service_unavailable(),
            ErrorKind::Internal => {
                tracing::error!("Internal error: {msg}");
                Self::internal("Internal error")
            }
        };
        app_err.error_code = Some(error_code);
        app_err
    }
}

/// Map a typed `ApiError` to an HTTP `AppError`.
///
/// Uses the `From<ApiError> for AppError` impl for guaranteed-correct
/// status code mapping (no keyword-based heuristics).
#[must_use]
pub fn map_api_error(err: crate::impls::ApiError) -> AppError {
    AppError::from(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    // ========== Request ID in error responses ==========

    #[tokio::test]
    async fn test_error_response_includes_request_id() {
        // Create a simple app that returns an error
        let app = axum::Router::new()
            .route(
                "/test",
                axum::routing::get(|| async { AppError::bad_request("invalid input") }),
            )
            .layer(axum::middleware::from_fn(
                crate::http::middleware::request_id_middleware,
            ));

        let request = Request::builder()
            .uri("/test")
            .header("x-request-id", "test-req-123")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Check that the error response includes request_id in JSON body
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["request_id"], "test-req-123");
        assert_eq!(json["error"], "invalid input");
        assert_eq!(json["status"], 400);
    }

    #[tokio::test]
    async fn test_error_response_without_request_id_header() {
        // When no request ID is provided, a generated one should still be included
        let app = axum::Router::new()
            .route(
                "/test",
                axum::routing::get(|| async { AppError::not_found("resource") }),
            )
            .layer(axum::middleware::from_fn(
                crate::http::middleware::request_id_middleware,
            ));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Check that a generated request_id is present
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // request_id should be present and not empty
        let request_id = json["request_id"].as_str();
        assert!(request_id.is_some());
        assert!(!request_id.unwrap().is_empty());
        assert_eq!(json["error"], "resource");
        assert_eq!(json["status"], 404);
    }

    #[tokio::test]
    async fn test_error_response_with_internal_server_error() {
        // 5xx errors should return generic message but still include request_id
        let app = axum::Router::new()
            .route(
                "/test",
                axum::routing::get(|| async {
                    AppError::internal_server_error("database connection failed")
                }),
            )
            .layer(axum::middleware::from_fn(
                crate::http::middleware::request_id_middleware,
            ));

        let request = Request::builder()
            .uri("/test")
            .header("x-request-id", "internal-err-456")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Should include request_id even for 5xx errors
        assert_eq!(json["request_id"], "internal-err-456");
        // Message should be generic for 5xx
        assert_eq!(json["error"], "Internal server error");
        assert_eq!(json["status"], 500);
    }

    #[tokio::test]
    async fn test_error_response_with_error_code() {
        // Test that error codes are preserved along with request_id
        // We use a different approach: use a constant error code
        let app = axum::Router::new()
            .route(
                "/test",
                axum::routing::get(|| async {
                    let mut err = AppError::bad_request("test error");
                    err.error_code = Some(1001);
                    err
                }),
            )
            .layer(axum::middleware::from_fn(
                crate::http::middleware::request_id_middleware,
            ));

        let request = Request::builder()
            .uri("/test")
            .header("x-request-id", "code-test-789")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["request_id"], "code-test-789");
        assert_eq!(json["code"], 1001);
        assert_eq!(json["status"], 400);
    }

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

    #[test]
    fn test_missing_authorization_header_helper() {
        let err = AppError::missing_authorization_header();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "Missing Authorization header");
    }

    #[test]
    fn test_invalid_authorization_header_helper() {
        let err = AppError::invalid_authorization_header();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "Invalid Authorization header");
    }

    #[test]
    fn test_invalid_authorization_header_non_utf8_helper() {
        let err = AppError::invalid_authorization_header_non_utf8();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "Invalid Authorization header: non-UTF-8 value");
    }

    #[test]
    fn test_invalid_or_expired_token_helper() {
        let err = AppError::invalid_or_expired_token();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "Invalid or expired token");
    }

    #[test]
    fn test_invalid_or_expired_ticket_helper() {
        let err = AppError::invalid_or_expired_ticket();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "Invalid or expired ticket");
    }

    #[test]
    fn test_map_auth_authorization_error_preserves_business_message() {
        let err = map_auth_authorization_error(&synctv_core::Error::Authorization(
            "Not a member of this room".to_string(),
        ));

        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.message, "Not a member of this room");
    }

    #[test]
    fn test_map_auth_authorization_error_maps_email_not_verified() {
        let err = map_auth_authorization_error(&synctv_core::Error::EmailNotVerified);

        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(
            err.message,
            "Email not verified. Please verify your email to continue."
        );
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
    fn test_from_core_service_unavailable() {
        let core_err = synctv_core::Error::ServiceUnavailable("redis unavailable".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(app_err.message.contains("temporarily unavailable"));
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
    fn test_from_core_redis_maps_to_service_unavailable() {
        let redis_err = redis::RedisError::from(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "redis temporarily unavailable",
        ));
        let app_err = AppError::from(synctv_core::Error::Redis(redis_err));
        assert_eq!(app_err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            app_err.message.contains("temporarily unavailable"),
            "redis outages should surface as retryable service unavailability"
        );
    }

    #[test]
    fn test_from_core_database_maps_to_service_unavailable() {
        let app_err = AppError::from(synctv_core::Error::Database(sqlx::Error::PoolTimedOut));
        assert_eq!(app_err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            app_err.message.contains("temporarily unavailable"),
            "database pool exhaustion should surface as retryable service unavailability"
        );
    }

    #[test]
    fn test_from_core_timeout_maps_to_service_unavailable() {
        let core_err = synctv_core::Error::Timeout("redis lock renewal timed out".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            app_err.message.contains("temporarily unavailable"),
            "backend timeouts should not be reported as client-side request mistakes"
        );
    }

    #[test]
    fn test_from_core_auth_service_unavailable_stays_service_unavailable() {
        let core_err = synctv_core::Error::ServiceUnavailable(
            "Authentication service unavailable".to_string(),
        );
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            app_err.message.contains("temporarily unavailable"),
            "auth backend outages should surface as HTTP 503, not 401"
        );
    }

    #[test]
    fn test_from_core_optimistic_lock() {
        let core_err = synctv_core::Error::OptimisticLockConflict;
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::CONFLICT);
    }

    #[test]
    fn test_from_core_lock_conflict() {
        let core_err =
            synctv_core::Error::LockConflict("Lock already held: create_room:user1".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status, StatusCode::CONFLICT);
        assert!(app_err.message.contains("please retry"));
    }

    #[test]
    fn test_from_core_email_not_verified() {
        let core_err = synctv_core::Error::EmailNotVerified;
        let app_err = AppError::from(core_err);
        // Should be 403 Forbidden (not 401 Unauthorized) because the user
        // has authenticated successfully but is missing email verification
        assert_eq!(app_err.status, StatusCode::FORBIDDEN);
        assert!(
            app_err.message.contains("verify your email"),
            "Error message should tell the user to verify their email, got: {}",
            app_err.message
        );
    }

    #[test]
    fn test_email_not_verified_distinct_from_auth_failure() {
        let auth_err = synctv_core::Error::Authentication("Authentication failed".to_string());
        let email_err = synctv_core::Error::EmailNotVerified;

        let auth_app = AppError::from(auth_err);
        let email_app = AppError::from(email_err);

        // Different HTTP status codes: 401 vs 403
        assert_ne!(
            auth_app.status, email_app.status,
            "EmailNotVerified (403) must be distinguishable from Authentication (401)"
        );
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

    // ========== From<ApiError> RateLimited ==========

    #[test]
    fn test_from_api_error_rate_limited() {
        let api_err = crate::impls::ApiError::RateLimited("too many requests".to_string());
        let app_err = AppError::from(api_err);
        assert_eq!(
            app_err.status,
            StatusCode::TOO_MANY_REQUESTS,
            "ApiError::RateLimited should map to HTTP 429"
        );
        assert!(app_err.message.contains("too many requests"));
        assert_eq!(
            app_err.error_code,
            Some(crate::impls::error_codes::RESOURCE_EXHAUSTED)
        );
    }

    #[test]
    fn test_from_api_error_service_unavailable() {
        let api_err = crate::impls::ApiError::ServiceUnavailable("redis unavailable".to_string());
        let app_err = AppError::from(api_err);
        assert_eq!(
            app_err.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "ApiError::ServiceUnavailable should map to HTTP 503"
        );
        assert_eq!(
            app_err.error_code,
            Some(crate::impls::error_codes::SERVICE_UNAVAILABLE)
        );
    }

    #[test]
    fn test_from_core_rate_limited_via_api_error() {
        // Test the full chain: synctv_core::Error::RateLimited -> ApiError -> AppError
        let core_err = synctv_core::Error::RateLimited("exceeded quota".to_string());
        let api_err = crate::impls::ApiError::from(core_err);
        let app_err = AppError::from(api_err);
        assert_eq!(
            app_err.status,
            StatusCode::TOO_MANY_REQUESTS,
            "synctv_core::Error::RateLimited should map to HTTP 429 via ApiError"
        );
    }

    #[test]
    fn test_from_provider_upstream_http_400_maps_to_bad_request() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::UpstreamHttp {
            status: 400,
            url: "https://provider.example/internal/path?token=secret".to_string(),
        });
        assert_eq!(app_err.status, StatusCode::BAD_REQUEST);
        assert_eq!(app_err.message, "Upstream provider rejected the request.");
    }

    #[test]
    fn test_from_provider_upstream_http_404_maps_to_not_found() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::UpstreamHttp {
            status: 404,
            url: "https://provider.example/internal/path?token=secret".to_string(),
        });
        assert_eq!(app_err.status, StatusCode::NOT_FOUND);
        assert_eq!(app_err.message, "Provider resource not found");
    }

    #[test]
    fn test_from_provider_upstream_http_503_is_sanitized() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::UpstreamHttp {
            status: 503,
            url: "https://provider.example/internal/path?token=secret".to_string(),
        });
        assert_eq!(app_err.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            app_err.message,
            "Upstream provider service is temporarily unavailable."
        );
    }

    #[test]
    fn test_from_provider_upstream_http_408_maps_to_bad_gateway() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::UpstreamHttp {
            status: 408,
            url: "https://provider.example/internal/path?token=secret".to_string(),
        });
        assert_eq!(app_err.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            app_err.message,
            "Upstream provider service is temporarily unavailable."
        );
    }

    #[test]
    fn test_from_provider_upstream_http_429_maps_to_bad_gateway() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::UpstreamHttp {
            status: 429,
            url: "https://provider.example/internal/path?token=secret".to_string(),
        });
        assert_eq!(app_err.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            app_err.message,
            "Upstream provider service is temporarily unavailable."
        );
    }
}
