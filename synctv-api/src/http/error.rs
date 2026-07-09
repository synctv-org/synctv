// HTTP error handling

use axum::{
    body::Body,
    http::{header, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use std::fmt;

/// Result type for HTTP handlers
pub type AppResult<T> = Result<T, AppError>;

/// Application error with HTTP status code
#[derive(Debug)]
pub struct AppError {
    pub api_error: crate::impls::ApiError,
    extra_headers: Vec<(HeaderName, HeaderValue)>,
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        let api_error = api_error_from_status(status, message.into());
        Self {
            api_error: crate::api_error_model::sanitized_api_error(&api_error),
            extra_headers: Vec::new(),
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

    pub fn too_many_requests_with_retry(message: impl Into<String>, retry_after: u64) -> Self {
        let api_error = crate::impls::ApiError::RateLimitedWithRetry {
            message: message.into(),
            retry_after_seconds: retry_after,
        };
        Self {
            api_error: crate::api_error_model::sanitized_api_error(&api_error),
            extra_headers: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.extra_headers.push((name, value));
        self
    }

    #[must_use]
    pub fn status(&self) -> StatusCode {
        crate::api_error_model::GoogleApiError::from_api_error(&self.api_error).http_status
    }

    #[must_use]
    pub fn message(&self) -> &str {
        self.api_error.message()
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
        Self::unauthorized(synctv_common::messages::INVALID_OR_EXPIRED_TOKEN)
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
        Self::too_many_requests_with_retry(
            format!("Too many requests. Please try again in {retry_after} seconds."),
            retry_after,
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
        Self::unauthorized(synctv_common::messages::MISSING_AUTHORIZATION_HEADER)
    }

    #[must_use]
    pub fn invalid_authorization_header() -> Self {
        Self::unauthorized(synctv_common::messages::INVALID_AUTHORIZATION_HEADER)
    }

    #[must_use]
    pub fn invalid_authorization_header_non_utf8() -> Self {
        Self::unauthorized(synctv_common::messages::INVALID_AUTHORIZATION_HEADER_NON_UTF8)
    }

    #[must_use]
    pub fn invalid_or_expired_token() -> Self {
        Self::unauthorized(synctv_common::messages::INVALID_OR_EXPIRED_TOKEN)
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
        synctv_core::Error::KickCooldownDenied => {
            AppError::forbidden(synctv_core::Error::kick_cooldown_denied_message())
        }
        other => {
            tracing::error!(error = %other, "Unexpected authorization-classified auth error");
            AppError::forbidden("You do not have permission to perform this action")
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.api_error.message())
    }
}

impl std::error::Error for AppError {}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let Self {
            api_error,
            extra_headers,
        } = self;
        let request_id = crate::http::middleware::CURRENT_REQUEST_ID
            .try_with(Clone::clone)
            .ok();
        let google_error = crate::api_error_model::GoogleApiError::from_api_error(&api_error)
            .with_request_id(request_id.as_deref());
        let status = google_error.http_status;
        let body = match google_error.to_protojson_bytes() {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::error!(%error, "Failed to serialize google.rpc.Status HTTP error");
                br#"{"code":13,"message":"Internal error"}"#.to_vec()
            }
        };

        let mut response = Response::new(Body::from(body));
        *response.status_mut() = status;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        if let Some(retry_after_seconds) = google_error.retry_after_seconds {
            if let Ok(value) = HeaderValue::from_str(&retry_after_seconds.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        for (name, value) in extra_headers {
            response.headers_mut().insert(name, value);
        }

        response
    }
}

/// Convert `ProviderError` through the API-layer classifier so every transport
/// shares one provider error mapping table.
impl From<synctv_core::provider::ProviderError> for AppError {
    fn from(err: synctv_core::provider::ProviderError) -> Self {
        Self::from(crate::impls::ApiError::from(err))
    }
}

/// Convert `synctv_core` errors through the API-layer classifier so HTTP and
/// gRPC preserve one shared core-error mapping table.
impl From<synctv_core::Error> for AppError {
    fn from(err: synctv_core::Error) -> Self {
        Self::from(crate::impls::ApiError::from(err))
    }
}

/// Convert `serde_json` errors to HTTP errors
impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        tracing::error!("JSON serialization/deserialization error: {}", err);
        Self::from(crate::impls::ApiError::InvalidInput(
            "Invalid request data format".to_string(),
        ))
    }
}

/// Convert anyhow errors to HTTP errors
impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        let chain = err
            .chain()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(" | caused by: ");
        tracing::error!("Anyhow error: {chain}");
        Self::from(crate::impls::ApiError::Internal(
            "Internal error".to_string(),
        ))
    }
}

/// Map a typed `ApiError` to an HTTP `AppError` with guaranteed-correct
/// status code mapping (no keyword-based heuristics).
impl From<crate::impls::ApiError> for AppError {
    fn from(err: crate::impls::ApiError) -> Self {
        let mut app_err = Self {
            api_error: crate::api_error_model::sanitized_api_error(&err),
            extra_headers: Vec::new(),
        };
        if let crate::impls::ApiError::RangeNotSatisfiable { total_size } = err {
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{}", total_size.max(0))) {
                app_err = app_err.with_header(header::CONTENT_RANGE, value);
            }
        }
        app_err
    }
}

fn api_error_from_status(status: StatusCode, message: String) -> crate::impls::ApiError {
    match status {
        StatusCode::BAD_REQUEST => crate::impls::ApiError::InvalidInput(message),
        StatusCode::UNAUTHORIZED => crate::impls::ApiError::Authentication(message),
        StatusCode::FORBIDDEN => crate::impls::ApiError::Authorization(message),
        StatusCode::NOT_FOUND => crate::impls::ApiError::NotFound(message),
        StatusCode::CONFLICT => crate::impls::ApiError::Conflict(message),
        StatusCode::TOO_MANY_REQUESTS => crate::impls::ApiError::RateLimited(message),
        StatusCode::SERVICE_UNAVAILABLE => crate::impls::ApiError::ServiceUnavailable(message),
        StatusCode::BAD_GATEWAY => {
            tracing::warn!(error = %message, "Hiding upstream bad gateway details from response");
            crate::impls::ApiError::BadGateway("Upstream service error".to_string())
        }
        StatusCode::REQUEST_TIMEOUT => crate::impls::ApiError::RequestTimeout(message),
        StatusCode::GATEWAY_TIMEOUT => {
            tracing::warn!(error = %message, "Hiding upstream timeout details from response");
            crate::impls::ApiError::Timeout("Upstream service timed out".to_string())
        }
        status if status.is_server_error() => crate::impls::ApiError::Internal(message),
        _ => crate::impls::ApiError::InvalidInput(message),
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

    type TestResult<T = ()> = anyhow::Result<T>;

    fn error_info_metadata<'a>(json: &'a serde_json::Value, key: &str) -> Option<&'a str> {
        json["details"]
            .as_array()?
            .iter()
            .find(|detail| {
                detail["@type"].as_str() == Some("type.googleapis.com/google.rpc.ErrorInfo")
            })?
            .get("metadata")?
            .get(key)?
            .as_str()
    }

    #[tokio::test]
    async fn test_error_response_includes_request_id() -> TestResult {
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
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;

        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;

        assert_eq!(
            error_info_metadata(&json, "requestId"),
            Some("test-req-123")
        );
        assert_eq!(json["message"], "invalid input");
        assert_eq!(json["code"], tonic::Code::InvalidArgument as i32);
        Ok(())
    }

    #[tokio::test]
    async fn test_error_response_without_request_id_header() -> TestResult {
        let app = axum::Router::new()
            .route(
                "/test",
                axum::routing::get(|| async { AppError::not_found("resource") }),
            )
            .layer(axum::middleware::from_fn(
                crate::http::middleware::request_id_middleware,
            ));

        let request = Request::builder().uri("/test").body(Body::empty())?;

        let response = app.oneshot(request).await?;

        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;

        assert!(matches!(
            error_info_metadata(&json, "requestId"),
            Some(request_id) if !request_id.is_empty()
        ));
        assert_eq!(json["message"], "resource");
        assert_eq!(json["code"], tonic::Code::NotFound as i32);
        Ok(())
    }

    #[tokio::test]
    async fn test_error_response_with_internal_server_error() -> TestResult {
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
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;

        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;

        assert_eq!(
            error_info_metadata(&json, "requestId"),
            Some("internal-err-456")
        );
        assert_eq!(json["message"], "Internal error");
        assert_eq!(json["code"], tonic::Code::Internal as i32);
        Ok(())
    }

    #[tokio::test]
    async fn test_error_response_with_error_code() -> TestResult {
        let app = axum::Router::new()
            .route(
                "/test",
                axum::routing::get(|| async {
                    AppError::from(crate::impls::ApiError::Authentication(
                        "test error".to_string(),
                    ))
                }),
            )
            .layer(axum::middleware::from_fn(
                crate::http::middleware::request_id_middleware,
            ));

        let request = Request::builder()
            .uri("/test")
            .header("x-request-id", "code-test-789")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;

        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;

        assert_eq!(
            error_info_metadata(&json, "requestId"),
            Some("code-test-789")
        );
        assert_eq!(error_info_metadata(&json, "errorCode"), Some("1000"));
        assert_eq!(json["code"], tonic::Code::Unauthenticated as i32);
        Ok(())
    }

    #[test]
    fn test_bad_request() {
        let err = AppError::bad_request("invalid field");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "invalid field");
    }

    #[test]
    fn test_unauthorized() {
        let err = AppError::unauthorized("not logged in");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.message(), "not logged in");
    }

    #[test]
    fn test_forbidden() {
        let err = AppError::forbidden("no access");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        assert_eq!(err.message(), "no access");
    }

    #[test]
    fn test_not_found() {
        let err = AppError::not_found("room not found");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.message(), "room not found");
    }

    #[test]
    fn test_conflict() {
        let err = AppError::conflict("already exists");
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(err.message(), "already exists");
    }

    #[test]
    fn test_internal_server_error() {
        let err = AppError::internal_server_error("boom");
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.message(), "Internal error");
    }

    #[test]
    fn test_missing_authorization_header_helper() {
        let err = AppError::missing_authorization_header();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            err.message(),
            synctv_common::messages::MISSING_AUTHORIZATION_HEADER
        );
    }

    #[test]
    fn test_invalid_authorization_header_helper() {
        let err = AppError::invalid_authorization_header();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            err.message(),
            synctv_common::messages::INVALID_AUTHORIZATION_HEADER
        );
    }

    #[test]
    fn test_invalid_authorization_header_non_utf8_helper() {
        let err = AppError::invalid_authorization_header_non_utf8();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            err.message(),
            synctv_common::messages::INVALID_AUTHORIZATION_HEADER_NON_UTF8
        );
    }

    #[test]
    fn test_invalid_or_expired_token_helper() {
        let err = AppError::invalid_or_expired_token();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            err.message(),
            synctv_common::messages::INVALID_OR_EXPIRED_TOKEN
        );
    }

    #[test]
    fn test_invalid_or_expired_ticket_helper() {
        let err = AppError::invalid_or_expired_ticket();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.message(), "Invalid or expired ticket");
    }

    #[test]
    fn test_map_auth_authorization_error_preserves_business_message() {
        let err = map_auth_authorization_error(&synctv_core::Error::Authorization(
            "Not a member of this room".to_string(),
        ));

        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        assert_eq!(err.message(), "Not a member of this room");
    }

    #[test]
    fn test_invalid_credentials() {
        let err = AppError::invalid_credentials();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert!(err.message().contains("Invalid username or password"));
    }

    #[test]
    fn test_session_expired() {
        let err = AppError::session_expired();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert!(err.message().contains("expired"));
    }

    #[test]
    fn test_token_invalid() {
        let err = AppError::token_invalid();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert!(err.message().contains("Invalid"));
    }

    #[test]
    fn test_permission_denied() {
        let err = AppError::permission_denied();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        assert!(err.message().contains("permission"));
    }

    #[test]
    fn test_resource_not_found() {
        let err = AppError::resource_not_found("Room");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.message(), "Room not found");
    }

    #[test]
    fn test_validation_failed() {
        let err = AppError::validation_failed("email", "must contain @");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert!(err.message().contains("email"));
        assert!(err.message().contains("must contain @"));
    }

    #[test]
    fn test_rate_limited() {
        let err = AppError::rate_limited(60);
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(err.message().contains("60 seconds"));
    }

    #[test]
    fn test_service_unavailable() {
        let err = AppError::service_unavailable();
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(err.message().contains("temporarily unavailable"));
    }

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

    #[test]
    fn test_from_core_not_found() {
        let core_err = synctv_core::Error::NotFound("room".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_from_core_already_exists() {
        let core_err = synctv_core::Error::AlreadyExists("user".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn test_from_core_authentication() {
        let core_err = synctv_core::Error::Authentication("bad token".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_from_core_authorization() {
        let core_err = synctv_core::Error::Authorization("denied".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_from_core_kick_cooldown_denied() {
        let app_err = AppError::from(synctv_core::Error::kick_cooldown_denied());

        assert_eq!(app_err.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            app_err.message(),
            synctv_core::Error::kick_cooldown_denied_message()
        );
    }

    #[test]
    fn test_from_core_invalid_input() {
        let core_err = synctv_core::Error::InvalidInput("bad field".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_from_core_rate_limited() {
        let core_err = synctv_core::Error::RateLimited("too fast".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn test_from_core_service_unavailable() {
        let core_err = synctv_core::Error::ServiceUnavailable("redis unavailable".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(app_err.message(), "redis unavailable");
    }

    #[test]
    fn test_from_core_internal() {
        let core_err = synctv_core::Error::Internal("something broke".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // Internal error messages should NOT leak to the client
        assert_eq!(app_err.message(), "Internal error");
        assert_eq!(
            app_err.api_error.code(),
            crate::impls::error_codes::INTERNAL_ERROR
        );
    }

    #[test]
    fn test_from_core_redis_timeout_internal_uses_api_classifier() {
        let core_err = synctv_core::Error::Internal("Redis timeout: store session".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            app_err.api_error.code(),
            crate::impls::error_codes::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn test_from_core_redis_maps_to_service_unavailable() {
        let redis_err = redis::RedisError::from(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "redis temporarily unavailable",
        ));
        let app_err = AppError::from(synctv_core::Error::Redis(redis_err));
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            app_err.message().contains("temporarily unavailable"),
            "redis outages should surface as retryable service unavailability"
        );
    }

    #[test]
    fn test_from_core_database_maps_to_service_unavailable() {
        let app_err = AppError::from(synctv_core::Error::Database(sqlx::Error::PoolTimedOut));
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            app_err.message().contains("temporarily unavailable"),
            "database pool exhaustion should surface as retryable service unavailability"
        );
    }

    #[test]
    fn test_from_core_timeout_maps_to_gateway_timeout() {
        let core_err = synctv_core::Error::Timeout("redis lock renewal timed out".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(app_err.message(), "redis lock renewal timed out");
    }

    #[test]
    fn test_from_core_auth_service_unavailable_stays_service_unavailable() {
        let core_err = synctv_core::Error::ServiceUnavailable(
            "Authentication service unavailable".to_string(),
        );
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(app_err.message(), "Authentication service unavailable");
    }

    #[test]
    fn test_from_core_optimistic_lock() {
        let core_err = synctv_core::Error::OptimisticLockConflict;
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn test_from_core_lock_conflict() {
        let core_err =
            synctv_core::Error::LockConflict("Lock already held: create_room:user1".to_string());
        let app_err = AppError::from(core_err);
        assert_eq!(app_err.status(), StatusCode::CONFLICT);
        assert_eq!(app_err.message(), "Lock already held: create_room:user1");
        assert_eq!(
            app_err.api_error.code(),
            crate::impls::error_codes::CONFLICT
        );
    }

    #[test]
    fn test_from_serde_json_error() -> TestResult {
        let Err(json_err) = serde_json::from_str::<serde_json::Value>("invalid json {{{") else {
            return Err(anyhow::anyhow!("invalid JSON should fail to parse"));
        };
        let app_err = AppError::from(json_err);
        assert_eq!(app_err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(app_err.message(), "Invalid request data format");
        Ok(())
    }

    #[test]
    fn test_from_api_error_rate_limited() {
        let api_err = crate::impls::ApiError::RateLimited("too many requests".to_string());
        let app_err = AppError::from(api_err);
        assert_eq!(
            app_err.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "ApiError::RateLimited should map to HTTP 429"
        );
        assert!(app_err.message().contains("too many requests"));
        assert_eq!(
            app_err.api_error.code(),
            crate::impls::error_codes::RESOURCE_EXHAUSTED
        );
        assert_eq!(app_err.api_error.retry_after_seconds(), None);
    }

    #[tokio::test]
    async fn test_from_api_error_rate_limited_with_retry_sets_header() {
        let app_err = AppError::from(crate::impls::ApiError::RateLimitedWithRetry {
            message: "too many requests".to_string(),
            retry_after_seconds: 17,
        });
        let response = app_err.into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from_static("17"))
        );
    }

    #[test]
    fn test_from_api_error_service_unavailable() {
        let api_err = crate::impls::ApiError::ServiceUnavailable("redis unavailable".to_string());
        let app_err = AppError::from(api_err);
        assert_eq!(
            app_err.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "ApiError::ServiceUnavailable should map to HTTP 503"
        );
        assert_eq!(
            app_err.api_error.code(),
            crate::impls::error_codes::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn test_from_api_error_timeout() {
        let api_err = crate::impls::ApiError::Timeout("request budget exceeded".to_string());
        let app_err = AppError::from(api_err);
        assert_eq!(app_err.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(app_err.message(), "request budget exceeded");
        assert_eq!(app_err.api_error.code(), crate::impls::error_codes::TIMEOUT);
    }

    #[test]
    fn test_from_core_rate_limited_via_api_error() {
        // Test the full chain: synctv_core::Error::RateLimited -> ApiError -> AppError
        let core_err = synctv_core::Error::RateLimited("exceeded quota".to_string());
        let api_err = crate::impls::ApiError::from(core_err);
        let app_err = AppError::from(api_err);
        assert_eq!(
            app_err.status(),
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
        assert_eq!(app_err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(app_err.message(), "Upstream provider rejected the request.");
    }

    #[test]
    fn test_from_provider_network_error_maps_to_service_unavailable() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::NetworkError(
            "connection refused".to_string(),
        ));
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(app_err.message(), "connection refused");
    }

    #[test]
    fn test_from_provider_api_error_maps_to_service_unavailable() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::ApiError(
            "upstream provider down".to_string(),
        ));
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(app_err.message(), "upstream provider down");
    }

    #[test]
    fn test_from_provider_upstream_http_404_maps_to_not_found() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::UpstreamHttp {
            status: 404,
            url: "https://provider.example/internal/path?token=secret".to_string(),
        });
        assert_eq!(app_err.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            app_err.message(),
            synctv_common::messages::PROVIDER_RESOURCE_NOT_FOUND
        );
    }

    #[test]
    fn test_from_provider_upstream_http_503_maps_to_service_unavailable() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::UpstreamHttp {
            status: 503,
            url: "https://provider.example/internal/path?token=secret".to_string(),
        });
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            app_err.message(),
            "Upstream provider service is temporarily unavailable."
        );
    }

    #[test]
    fn test_from_provider_upstream_http_408_maps_to_service_unavailable() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::UpstreamHttp {
            status: 408,
            url: "https://provider.example/internal/path?token=secret".to_string(),
        });
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            app_err.message(),
            "Upstream provider service is temporarily unavailable."
        );
    }

    #[test]
    fn test_from_provider_upstream_http_409_maps_to_conflict() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::UpstreamHttp {
            status: 409,
            url: "https://provider.example/internal/path?token=secret".to_string(),
        });
        assert_eq!(app_err.status(), StatusCode::CONFLICT);
        assert_eq!(
            app_err.message(),
            "Upstream provider reported a request conflict."
        );
    }

    #[test]
    fn test_from_provider_upstream_http_429_maps_to_too_many_requests() {
        let app_err = AppError::from(synctv_core::provider::ProviderError::UpstreamHttp {
            status: 429,
            url: "https://provider.example/internal/path?token=secret".to_string(),
        });
        assert_eq!(app_err.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            app_err.message(),
            "Upstream provider rate limited the request."
        );
    }
}
