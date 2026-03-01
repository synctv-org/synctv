//! HTTP integration tests for synctv-api
//!
//! Tests the HTTP layer: error responses, security headers, health endpoints,
//! routing structure, and request/response format validation.
//!
//! These tests use `tower::ServiceExt::oneshot()` to test the axum router
//! without starting a TCP server, and do not require database or Redis.

#![allow(clippy::unwrap_used)]
use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

// ============================================================================
// Helper: extract JSON body from response
// ============================================================================

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn _body_string(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap_or_default()
}

// ============================================================================
// Module: AppError serialization and HTTP status mapping
// ============================================================================

mod error_responses {
    use super::*;
    use synctv_api::http::error::AppError;

    /// Build a tiny router that returns a specific `AppError`
    fn error_router(error: AppError) -> Router {
        let status = error.status;
        let message = error.message;
        Router::new().route(
            "/test",
            get(move || async move {
                Err::<String, AppError>(AppError::new(status, message))
            }),
        )
    }

    #[tokio::test]
    async fn test_bad_request_returns_400() {
        let app = error_router(AppError::bad_request("invalid input"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert_eq!(json["status"], 400);
        assert_eq!(json["error"], "invalid input");
    }

    #[tokio::test]
    async fn test_unauthorized_returns_401() {
        let app = error_router(AppError::unauthorized("not authenticated"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(resp).await;
        assert_eq!(json["status"], 401);
        assert_eq!(json["error"], "not authenticated");
    }

    #[tokio::test]
    async fn test_forbidden_returns_403() {
        let app = error_router(AppError::forbidden("access denied"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let json = body_json(resp).await;
        assert_eq!(json["status"], 403);
        assert_eq!(json["error"], "access denied");
    }

    #[tokio::test]
    async fn test_not_found_returns_404() {
        let app = error_router(AppError::not_found("room not found"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_json(resp).await;
        assert_eq!(json["status"], 404);
        assert_eq!(json["error"], "room not found");
    }

    #[tokio::test]
    async fn test_conflict_returns_409() {
        let app = error_router(AppError::conflict("already exists"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json = body_json(resp).await;
        assert_eq!(json["status"], 409);
    }

    #[tokio::test]
    async fn test_rate_limited_returns_429() {
        let app = error_router(AppError::rate_limited(30));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let json = body_json(resp).await;
        assert_eq!(json["status"], 429);
        let msg = json["error"].as_str().unwrap();
        assert!(msg.contains("30"), "Error message should contain retry seconds");
    }

    #[tokio::test]
    async fn test_internal_server_error_returns_500() {
        let app = error_router(AppError::internal_server_error("something broke"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(resp).await;
        assert_eq!(json["status"], 500);
    }

    #[tokio::test]
    async fn test_service_unavailable_returns_503() {
        let app = error_router(AppError::service_unavailable());
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        assert_eq!(json["status"], 503);
    }

    #[tokio::test]
    async fn test_error_response_json_structure() {
        // Verify the JSON structure has exactly "error" and "status" fields
        let app = error_router(AppError::bad_request("test"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let json = body_json(resp).await;
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("error"), "Response must contain 'error' field");
        assert!(obj.contains_key("status"), "Response must contain 'status' field");
        assert_eq!(obj.len(), 2, "Error response should have exactly 2 fields");
    }

    // === Convenience constructors ===

    #[tokio::test]
    async fn test_invalid_credentials() {
        let app = error_router(AppError::invalid_credentials());
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(resp).await;
        assert!(json["error"].as_str().unwrap().contains("Invalid username or password"));
    }

    #[tokio::test]
    async fn test_session_expired() {
        let app = error_router(AppError::session_expired());
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(resp).await;
        assert!(json["error"].as_str().unwrap().contains("expired"));
    }

    #[tokio::test]
    async fn test_token_invalid() {
        let app = error_router(AppError::token_invalid());
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_permission_denied() {
        let app = error_router(AppError::permission_denied());
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_resource_not_found() {
        let app = error_router(AppError::resource_not_found("Room"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_json(resp).await;
        assert!(json["error"].as_str().unwrap().contains("Room"));
    }

    #[tokio::test]
    async fn test_validation_failed() {
        let app = error_router(AppError::validation_failed("email", "must be valid"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        let msg = json["error"].as_str().unwrap();
        assert!(msg.contains("email"), "Should mention field name");
        assert!(msg.contains("must be valid"), "Should mention reason");
    }

    // ========================================================================
    // Security: Internal error message sanitization
    // ========================================================================
    //
    // Internal errors (5xx) should return a generic message to avoid leaking
    // sensitive information like database connection strings, file paths,
    // stack traces, or internal implementation details.
    //
    // Client errors (4xx) should preserve the original message to help the
    // client understand and fix their request.

    /// Internal server error (500) MUST return generic message, not the original.
    /// This prevents leaking sensitive info like "connection to postgres://user:pass@..."
    #[tokio::test]
    async fn test_internal_error_returns_generic_message() {
        // Simulate an internal error with sensitive info in the message
        let sensitive_msg = "Database connection failed: postgres://admin:secret@db.internal:5432/production";
        let app = error_router(AppError::internal_server_error(sensitive_msg));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(resp).await;
        let error_msg = json["error"].as_str().unwrap();

        // The response MUST NOT contain the sensitive message
        assert!(
            !error_msg.contains("postgres://"),
            "Internal error response must not leak database connection strings"
        );
        assert!(
            !error_msg.contains("secret"),
            "Internal error response must not leak passwords"
        );
        assert!(
            !error_msg.contains("db.internal"),
            "Internal error response must not leak internal hostnames"
        );

        // The response should be a generic message
        assert_eq!(
            error_msg,
            "Internal server error",
            "Internal error should return generic message"
        );
    }

    /// Client errors (4xx) MUST preserve the original message to help clients.
    #[tokio::test]
    async fn test_client_errors_preserve_original_messages() {
        // 400 Bad Request
        let app = error_router(AppError::bad_request("Invalid email format"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["error"], "Invalid email format", "400 should preserve message");

        // 401 Unauthorized
        let app = error_router(AppError::unauthorized("Token has expired"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["error"], "Token has expired", "401 should preserve message");

        // 403 Forbidden
        let app = error_router(AppError::forbidden("Admin access required"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["error"], "Admin access required", "403 should preserve message");

        // 404 Not Found
        let app = error_router(AppError::not_found("Room 'abc123' does not exist"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["error"], "Room 'abc123' does not exist", "404 should preserve message");
    }

    /// Sensitive patterns must NEVER appear in any error response.
    /// This is a defense-in-depth test to catch accidental leakage.
    #[tokio::test]
    async fn test_sensitive_info_never_leaked_in_responses() {
        let sensitive_patterns = [
            "password",
            "secret",
            "api_key",
            "private_key",
            "postgres://",
            "mysql://",
            "redis://",
            "mongodb://",
            "/etc/",
            "stack trace",
            "Error:",
            "panic!",
        ];

        // Test internal error with all sensitive patterns
        let sensitive_msg = "Error: panic! at /etc/config.toml - postgres://admin:password@localhost redis://localhost private_key=abc123";
        let app = error_router(AppError::internal_server_error(sensitive_msg));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        let error_msg = json["error"].as_str().unwrap().to_lowercase();

        for pattern in &sensitive_patterns {
            assert!(
                !error_msg.contains(&pattern.to_lowercase()),
                "Response must not contain sensitive pattern: {pattern}"
            );
        }
    }

    /// Service unavailable (503) is a server error, so it returns generic message.
    /// This is consistent with security best practices - all 5xx errors get generic messages.
    #[tokio::test]
    async fn test_service_unavailable_returns_safe_message() {
        let app = error_router(AppError::service_unavailable());
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        let error_msg = json["error"].as_str().unwrap();

        // 503 is a server error (5xx), so it returns generic message for security
        assert_eq!(
            error_msg,
            "Internal server error",
            "503 should return generic message (all 5xx errors are sanitized)"
        );
    }

    /// Bad gateway (502) should also return generic message.
    #[tokio::test]
    async fn test_bad_gateway_returns_generic_message() {
        let app = error_router(AppError::new(StatusCode::BAD_GATEWAY, "Upstream nginx error: connection reset by peer"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let json = body_json(resp).await;
        let error_msg = json["error"].as_str().unwrap();

        // 502 is a server error, should return generic message
        assert_eq!(
            error_msg,
            "Internal server error",
            "502 should return generic message"
        );
    }

    /// Gateway timeout (504) should also return generic message.
    #[tokio::test]
    async fn test_gateway_timeout_returns_generic_message() {
        let app = error_router(AppError::new(StatusCode::GATEWAY_TIMEOUT, "Upstream timeout after 30s"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
        let json = body_json(resp).await;
        let error_msg = json["error"].as_str().unwrap();

        // 504 is a server error, should return generic message
        assert_eq!(
            error_msg,
            "Internal server error",
            "504 should return generic message"
        );
    }
}

// ============================================================================
// Module: Error classification (map_api_error)
// ============================================================================

mod error_classification {
    use synctv_api::http::error::map_api_error;
    use synctv_api::impls::ApiError;
    use axum::http::StatusCode;

    #[test]
    fn test_not_found_error_maps_to_404() {
        let err = map_api_error(ApiError::NotFound("User not found".into()));
        assert_eq!(err.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_unauthenticated_error_maps_to_401() {
        let err = map_api_error(ApiError::Authentication("Unauthenticated".into()));
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_permission_denied_maps_to_403() {
        let err = map_api_error(ApiError::Authorization("Permission denied".into()));
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_already_exists_maps_to_409() {
        let err = map_api_error(ApiError::AlreadyExists("User already exists".into()));
        assert_eq!(err.status, StatusCode::CONFLICT);
    }

    #[test]
    fn test_invalid_argument_maps_to_400() {
        let err = map_api_error(ApiError::InvalidInput("Password too short".into()));
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_internal_error_maps_to_500() {
        let err = map_api_error(ApiError::Internal("Something went wrong".into()));
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_not_found_message_preserved() {
        let err = map_api_error(ApiError::NotFound("room abc123".into()));
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert!(err.message.contains("room abc123"));
    }

    #[test]
    fn test_authentication_message_preserved() {
        let err = map_api_error(ApiError::Authentication("token expired".into()));
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert!(err.message.contains("token expired"));
    }

    #[test]
    fn test_authorization_message_preserved() {
        let err = map_api_error(ApiError::Authorization("forbidden".into()));
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert!(err.message.contains("forbidden"));
    }

    #[test]
    fn test_already_exists_message_preserved() {
        let err = map_api_error(ApiError::AlreadyExists("username taken".into()));
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(err.message.contains("username taken"));
    }

    #[test]
    fn test_invalid_input_message_preserved() {
        let err = map_api_error(ApiError::InvalidInput("bad email format".into()));
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("bad email format"));
    }

    #[test]
    fn test_internal_error_message_is_generic() {
        // Internal errors should NOT leak the original message to clients
        let err = map_api_error(ApiError::Internal("Database connection pool exhausted".into()));
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.message, "Internal error");
    }

    #[test]
    fn test_from_core_error_not_found() {
        let core_err = synctv_core::Error::NotFound("room 123".into());
        let api_err = ApiError::from(core_err);
        let app_err = map_api_error(api_err);
        assert_eq!(app_err.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_from_core_error_authentication() {
        let core_err = synctv_core::Error::Authentication("expired".into());
        let api_err = ApiError::from(core_err);
        let app_err = map_api_error(api_err);
        assert_eq!(app_err.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_from_core_error_authorization() {
        let core_err = synctv_core::Error::Authorization("denied".into());
        let api_err = ApiError::from(core_err);
        let app_err = map_api_error(api_err);
        assert_eq!(app_err.status, StatusCode::FORBIDDEN);
    }
}

// ============================================================================
// Module: Security headers middleware
// ============================================================================

mod security_headers {
    use super::*;
    use synctv_api::http::middleware::security_headers_middleware;

    fn security_app() -> Router {
        Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(axum_middleware::from_fn(security_headers_middleware))
    }

    #[tokio::test]
    async fn test_x_frame_options_deny() {
        let app = security_app();
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.headers().get("X-Frame-Options").unwrap(), "DENY");
    }

    #[tokio::test]
    async fn test_x_content_type_options_nosniff() {
        let app = security_app();
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(
            resp.headers().get("X-Content-Type-Options").unwrap(),
            "nosniff"
        );
    }

    #[tokio::test]
    async fn test_xss_protection() {
        let app = security_app();
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(
            resp.headers().get("X-XSS-Protection").unwrap(),
            "1; mode=block"
        );
    }

    #[tokio::test]
    async fn test_content_security_policy_present() {
        let app = security_app();
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let csp = resp
            .headers()
            .get("Content-Security-Policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("media-src * blob:"));
        assert!(csp.contains("connect-src 'self' wss: ws:"));
    }

    #[tokio::test]
    async fn test_referrer_policy() {
        let app = security_app();
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(
            resp.headers().get("Referrer-Policy").unwrap(),
            "strict-origin-when-cross-origin"
        );
    }

    #[tokio::test]
    async fn test_permissions_policy() {
        let app = security_app();
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let pp = resp
            .headers()
            .get("Permissions-Policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(pp.contains("camera=()"));
        assert!(pp.contains("microphone=()"));
        assert!(pp.contains("geolocation=()"));
    }

    #[tokio::test]
    async fn test_cache_control_no_store() {
        let app = security_app();
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let cc = resp
            .headers()
            .get("Cache-Control")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(cc.contains("no-store"));
        assert!(cc.contains("no-cache"));
        assert!(cc.contains("must-revalidate"));
    }

    #[tokio::test]
    async fn test_pragma_no_cache() {
        let app = security_app();
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.headers().get("Pragma").unwrap(), "no-cache");
    }

    #[tokio::test]
    async fn test_does_not_overwrite_existing_x_frame_options() {
        let app = Router::new()
            .route(
                "/test",
                get(|| async {
                    (
                        [(
                            axum::http::header::HeaderName::from_static("x-frame-options"),
                            "SAMEORIGIN",
                        )],
                        "ok",
                    )
                }),
            )
            .layer(axum_middleware::from_fn(security_headers_middleware));

        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        // Should preserve the handler's value
        assert_eq!(
            resp.headers().get("X-Frame-Options").unwrap(),
            "SAMEORIGIN"
        );
    }

    #[tokio::test]
    async fn test_all_security_headers_present_on_404() {
        // Security headers should be added even for 404 responses
        let app = Router::new()
            .route("/other", get(|| async { "ok" }))
            .layer(axum_middleware::from_fn(security_headers_middleware));

        let req = Request::get("/nonexistent").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        // Even on 404, security headers should be present
        assert!(resp.headers().contains_key("X-Frame-Options"));
        assert!(resp.headers().contains_key("X-Content-Type-Options"));
        assert!(resp.headers().contains_key("Referrer-Policy"));
    }
}

// ============================================================================
// Module: HSTS header generation
// ============================================================================

mod hsts_headers {
    use synctv_api::http::middleware::hsts_header;

    #[test]
    fn test_basic_max_age() {
        assert_eq!(hsts_header(31536000, false, false), "max-age=31536000");
    }

    #[test]
    fn test_with_subdomains() {
        assert_eq!(
            hsts_header(31536000, true, false),
            "max-age=31536000; includeSubDomains"
        );
    }

    #[test]
    fn test_with_preload() {
        assert_eq!(
            hsts_header(31536000, false, true),
            "max-age=31536000; preload"
        );
    }

    #[test]
    fn test_full_hsts() {
        assert_eq!(
            hsts_header(63072000, true, true),
            "max-age=63072000; includeSubDomains; preload"
        );
    }

    #[test]
    fn test_zero_max_age() {
        // Zero max-age is valid -- used to clear HSTS
        assert_eq!(hsts_header(0, false, false), "max-age=0");
    }

    #[test]
    fn test_production_defaults() {
        // The values used in apply_global_layers
        let header = hsts_header(63_072_000, true, false);
        assert_eq!(header, "max-age=63072000; includeSubDomains");
    }
}

// ============================================================================
// Module: Health endpoints (liveness)
// ============================================================================

mod health_endpoints {
    use super::*;

    /// Liveness endpoint does not need `AppState` -- test it directly
    #[tokio::test]
    async fn test_liveness_returns_200() {
        let app = Router::new().route(
            "/health/live",
            get(synctv_api::http::health::liveness_check),
        );

        let req = Request::get("/health/live").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_liveness_alias() {
        let app = Router::new().route(
            "/health",
            get(synctv_api::http::health::liveness_check),
        );

        let req = Request::get("/health").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_liveness_json_structure() {
        let app = Router::new().route(
            "/health",
            get(synctv_api::http::health::liveness_check),
        );

        let req = Request::get("/health").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let json = body_json(resp).await;
        // Liveness should have "status" but no "details"
        assert!(json.get("status").is_some());
        assert!(json.get("details").is_none());
    }
}

// ============================================================================
// Module: Routing structure (non-existent routes return 404)
// ============================================================================

mod routing_structure {
    use super::*;

    fn minimal_router() -> Router {
        Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/api/auth/login", post(|| async { "login" }))
            .route("/api/rooms", get(|| async { "rooms" }))
    }

    #[tokio::test]
    async fn test_unknown_route_returns_404() {
        let app = minimal_router();
        let req = Request::get("/api/nonexistent")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_wrong_method_returns_405() {
        let app = minimal_router();
        // GET on a POST-only route
        let req = Request::get("/api/auth/login")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_health_endpoint_accessible() {
        let app = minimal_router();
        let req = Request::get("/health").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_rooms_get_accessible() {
        let app = minimal_router();
        let req = Request::get("/api/rooms").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }
}

// ============================================================================
// Module: Request/response format validation (proto type serialization)
// ============================================================================

mod request_format {
    use synctv_proto::client::{
        RegisterRequest, LoginRequest, CreateRoomRequest,
        JoinRoomRequest, RefreshTokenRequest,
    };

    #[test]
    fn test_register_request_deserializes() {
        let json = r#"{"username":"test","password":"secret123","email":"test@example.com"}"#;
        let req: RegisterRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.username, "test");
        assert_eq!(req.password, "secret123");
        assert_eq!(req.email, "test@example.com");
    }

    #[test]
    fn test_register_request_with_explicit_empty_fields() {
        let json = r#"{"username":"","password":"","email":""}"#;
        let req: RegisterRequest = serde_json::from_str(json).unwrap();
        assert!(req.username.is_empty());
        assert!(req.password.is_empty());
        assert!(req.email.is_empty());
    }

    #[test]
    fn test_login_request_deserializes() {
        let json = r#"{"username":"test","password":"secret123"}"#;
        let req: LoginRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.username, "test");
        assert_eq!(req.password, "secret123");
    }

    #[test]
    fn test_create_room_request_deserializes() {
        let json = r#"{"name":"Movie Night","password":"","settings":[],"description":""}"#;
        let req: CreateRoomRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.name, "Movie Night");
    }

    #[test]
    fn test_join_room_request_with_password() {
        let json = r#"{"password":"room_pass"}"#;
        let req: JoinRoomRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.password, "room_pass");
    }

    #[test]
    fn test_join_room_request_without_password() {
        let json = r#"{"password":""}"#;
        let req: JoinRoomRequest = serde_json::from_str(json).unwrap();
        assert!(req.password.is_empty());
    }

    #[test]
    fn test_refresh_token_request() {
        let json = r#"{"refresh_token":"some_refresh_token"}"#;
        let req: RefreshTokenRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.refresh_token, "some_refresh_token");
    }

    #[test]
    fn test_register_request_roundtrip() {
        let req = RegisterRequest {
            username: "alice".to_string(),
            password: "password123".to_string(),
            email: "alice@example.com".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: RegisterRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, deserialized);
    }
}

// ============================================================================
// Module: Response format validation
// ============================================================================

mod response_format {
    use synctv_proto::client::{
        RegisterResponse, LoginResponse, User,
    };

    #[test]
    fn test_register_response_serializes() {
        let resp = RegisterResponse {
            user: Some(User {
                id: "user_123".to_string(),
                username: "alice".to_string(),
                ..Default::default()
            }),
            access_token: "access_abc".to_string(),
            refresh_token: "refresh_xyz".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["access_token"], "access_abc");
        assert_eq!(json["refresh_token"], "refresh_xyz");
        assert_eq!(json["user"]["id"], "user_123");
        assert_eq!(json["user"]["username"], "alice");
    }

    #[test]
    fn test_login_response_serializes() {
        let resp = LoginResponse {
            user: Some(User {
                id: "user_456".to_string(),
                username: "bob".to_string(),
                ..Default::default()
            }),
            access_token: "at_123".to_string(),
            refresh_token: "rt_456".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json["user"].is_object());
        assert!(!json["access_token"].as_str().unwrap().is_empty());
    }

    #[test]
    fn test_error_response_format() {
        // Verify the error response format matches what clients expect
        let json = serde_json::json!({
            "error": "some error message",
            "status": 400
        });
        assert!(json["error"].is_string());
        assert!(json["status"].is_number());
    }
}

// ============================================================================
// Module: Rate limit config defaults
// ============================================================================

mod rate_limit_config {
    use synctv_api::http::middleware::RateLimitConfig;

    #[test]
    fn test_default_auth_rate_limit() {
        let config = RateLimitConfig::default();
        assert_eq!(config.auth_max_requests, 5);
        assert_eq!(config.auth_window_seconds, 60);
    }

    #[test]
    fn test_default_write_rate_limit() {
        let config = RateLimitConfig::default();
        assert_eq!(config.write_max_requests, 30);
        assert_eq!(config.write_window_seconds, 60);
    }

    #[test]
    fn test_default_read_rate_limit() {
        let config = RateLimitConfig::default();
        assert_eq!(config.read_max_requests, 100);
        assert_eq!(config.read_window_seconds, 60);
    }

    #[test]
    fn test_default_media_rate_limit() {
        let config = RateLimitConfig::default();
        assert_eq!(config.media_max_requests, 20);
        assert_eq!(config.media_window_seconds, 60);
    }

    #[test]
    fn test_default_websocket_rate_limit() {
        let config = RateLimitConfig::default();
        assert_eq!(config.websocket_max_requests, 10);
        assert_eq!(config.websocket_window_seconds, 60);
    }

    #[test]
    fn test_auth_is_stricter_than_read() {
        let config = RateLimitConfig::default();
        assert!(config.auth_max_requests < config.read_max_requests);
    }

    #[test]
    fn test_auth_is_stricter_than_write() {
        let config = RateLimitConfig::default();
        assert!(config.auth_max_requests < config.write_max_requests);
    }

    #[test]
    fn test_websocket_is_stricter_than_streaming() {
        let config = RateLimitConfig::default();
        assert!(config.websocket_max_requests < config.streaming_max_requests);
    }

    #[test]
    fn test_all_windows_are_60_seconds() {
        let config = RateLimitConfig::default();
        assert_eq!(config.auth_window_seconds, 60);
        assert_eq!(config.write_window_seconds, 60);
        assert_eq!(config.read_window_seconds, 60);
        assert_eq!(config.media_window_seconds, 60);
        assert_eq!(config.admin_window_seconds, 60);
        assert_eq!(config.streaming_window_seconds, 60);
        assert_eq!(config.websocket_window_seconds, 60);
    }
}

// ============================================================================
// Module: Unauthenticated endpoint behavior
// ============================================================================

mod unauthenticated_access {
    use super::*;
    use synctv_api::http::error::AppError;

    #[tokio::test]
    async fn test_protected_endpoint_without_auth_header() {
        // Simulate a handler that requires AuthUser via a guard
        let app = Router::new().route(
            "/api/user",
            get(|| async {
                Err::<String, AppError>(AppError::unauthorized("Missing Authorization header"))
            }),
        );

        let req = Request::get("/api/user").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(resp).await;
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("Authorization"));
    }

    #[tokio::test]
    async fn test_protected_endpoint_with_invalid_token() {
        let app = Router::new().route(
            "/api/user",
            get(|| async {
                Err::<String, AppError>(AppError::unauthorized("Invalid or expired token"))
            }),
        );

        let req = Request::get("/api/user")
            .header("Authorization", "Bearer invalid_token_here")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}

// ============================================================================
// Module: ApiError type classification
// ============================================================================

mod api_error_classification {
    use synctv_api::impls::{ApiError, ErrorKind, classify_error};

    #[test]
    fn test_api_error_not_found_classify() {
        let err = ApiError::NotFound("room".to_string());
        assert!(matches!(err.classify(), ErrorKind::NotFound));
        assert_eq!(err.message(), "room");
    }

    #[test]
    fn test_api_error_authentication_classify() {
        let err = ApiError::Authentication("bad token".to_string());
        assert!(matches!(err.classify(), ErrorKind::Unauthenticated));
    }

    #[test]
    fn test_api_error_authorization_classify() {
        let err = ApiError::Authorization("denied".to_string());
        assert!(matches!(err.classify(), ErrorKind::PermissionDenied));
    }

    #[test]
    fn test_api_error_already_exists_classify() {
        let err = ApiError::AlreadyExists("duplicate".to_string());
        assert!(matches!(err.classify(), ErrorKind::AlreadyExists));
    }

    #[test]
    fn test_api_error_invalid_input_classify() {
        let err = ApiError::InvalidInput("bad field".to_string());
        assert!(matches!(err.classify(), ErrorKind::InvalidArgument));
    }

    #[test]
    fn test_api_error_internal_classify() {
        let err = ApiError::Internal("db error".to_string());
        assert!(matches!(err.classify(), ErrorKind::Internal));
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn test_api_error_display_roundtrips() {
        // Ensure Display output classifies correctly when parsed back
        let cases: &[(ApiError, fn(&ErrorKind) -> bool)] = &[
            (ApiError::NotFound("x".into()), |k| matches!(k, ErrorKind::NotFound)),
            (ApiError::Authentication("x".into()), |k| matches!(k, ErrorKind::Unauthenticated)),
            (ApiError::Authorization("x".into()), |k| matches!(k, ErrorKind::PermissionDenied)),
            (ApiError::AlreadyExists("x".into()), |k| matches!(k, ErrorKind::AlreadyExists)),
            (ApiError::InvalidInput("x".into()), |k| matches!(k, ErrorKind::InvalidArgument)),
            (ApiError::Internal("x".into()), |k| matches!(k, ErrorKind::Internal)),
        ];
        for (err, check) in cases {
            let s = err.to_string();
            let kind = classify_error(&s);
            assert!(check(&kind), "ApiError '{s}' misclassified after roundtrip");
        }
    }

    #[test]
    fn test_api_error_into_string() {
        let err = ApiError::NotFound("item".to_string());
        let s: String = err.into();
        assert!(s.starts_with("Not found: "));
    }
}

// ============================================================================
// Module: HTTP request body edge cases
// ============================================================================

mod body_edge_cases {
    use super::*;

    #[tokio::test]
    async fn test_empty_body_on_post() {
        // POST with empty body should return 400 from axum JSON extractor
        let app = Router::new().route(
            "/api/auth/login",
            post(
                |axum::Json(_body): axum::Json<serde_json::Value>| async { "ok" },
            ),
        );

        let req = Request::post("/api/auth/login")
            .header("Content-Type", "application/json")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        // Empty body with JSON content type should fail deserialization
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_malformed_json_body() {
        let app = Router::new().route(
            "/api/auth/login",
            post(
                |axum::Json(_body): axum::Json<serde_json::Value>| async { "ok" },
            ),
        );

        let req = Request::post("/api/auth/login")
            .header("Content-Type", "application/json")
            .body(Body::from("{invalid json"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_valid_json_body() {
        let app = Router::new().route(
            "/api/auth/login",
            post(
                |axum::Json(body): axum::Json<serde_json::Value>| async move {
                    axum::Json(body)
                },
            ),
        );

        let req = Request::post("/api/auth/login")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"username":"test","password":"pass"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["username"], "test");
    }

    #[tokio::test]
    async fn test_wrong_content_type() {
        let app = Router::new().route(
            "/api/test",
            post(
                |axum::Json(_body): axum::Json<serde_json::Value>| async { "ok" },
            ),
        );

        let req = Request::post("/api/test")
            .header("Content-Type", "text/plain")
            .body(Body::from(r#"{"key":"value"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        // axum should reject non-JSON content type
        assert!(
            resp.status() == StatusCode::UNSUPPORTED_MEDIA_TYPE
                || resp.status() == StatusCode::BAD_REQUEST
        );
    }
}

// ============================================================================
// Module: UpdatePlaybackRequest deserialization
// ============================================================================

mod playback_request {
    use synctv_api::http::room::UpdatePlaybackRequest;

    #[test]
    fn test_state_only() {
        let json = r#"{"state": "playing"}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.state.as_deref(), Some("playing"));
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
        assert!(req.media_id.is_none());
    }

    #[test]
    fn test_position_only() {
        let json = r#"{"position": 42.5}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!(req.state.is_none());
        assert!((req.position.unwrap() - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_speed_only() {
        let json = r#"{"speed": 2.0}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!((req.speed.unwrap() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_media_id_only() {
        let json = r#"{"media_id": "media_abc"}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.media_id.as_deref(), Some("media_abc"));
    }

    #[test]
    fn test_combined_fields() {
        let json = r#"{"state":"paused","position":10.0,"speed":1.5,"media_id":"m1"}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.state.as_deref(), Some("paused"));
        assert!((req.position.unwrap() - 10.0).abs() < f64::EPSILON);
        assert!((req.speed.unwrap() - 1.5).abs() < f64::EPSILON);
        assert_eq!(req.media_id.as_deref(), Some("m1"));
    }

    #[test]
    fn test_empty_object() {
        let json = r"{}";
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!(req.state.is_none());
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
        assert!(req.media_id.is_none());
    }
}

// ============================================================================
// Module: User update request deserialization
// ============================================================================

// ============================================================================
// Module: Complete authentication flow (no DB required)
// ============================================================================

mod auth_flow {
    use synctv_core::service::auth::{JwtService, TokenType, hash_password, verify_password};
    use synctv_core::models::UserId;

    fn jwt_service() -> JwtService {
        JwtService::new("test-secret-key-for-jwt-that-is-long-enough-1234567890").unwrap()
    }

    /// Full flow: hash password -> verify password -> issue tokens -> validate tokens
    #[tokio::test]
    async fn test_register_login_flow() {
        let password = "StrongP@ssw0rd!";

        // 1. Registration: hash the password (simulates register endpoint)
        let hash = hash_password(password).await.expect("hashing should succeed");
        assert_ne!(hash, password, "hash must differ from plaintext");

        // 2. Login: verify the password
        assert!(
            verify_password(password, &hash).await.expect("verify call should succeed"),
            "correct password should verify"
        );
        assert!(
            !verify_password("wrong_password", &hash).await.unwrap_or(false),
            "wrong password should fail verification"
        );

        // 3. Issue access + refresh tokens
        let jwt = jwt_service();
        let user_id = UserId::new();

        let access_token = jwt.sign_token(&user_id, TokenType::Access, 0).expect("access token");
        let refresh_token = jwt.sign_token(&user_id, TokenType::Refresh, 0).expect("refresh token");

        // 4. Validate access token (simulates auth middleware)
        let claims = jwt.verify_access_token(&access_token).expect("access token valid");
        assert_eq!(claims.sub, user_id.as_str());
        assert!(claims.is_access_token());

        // 5. Access token cannot be used as refresh token
        assert!(jwt.verify_refresh_token(&access_token).is_err());

        // 6. Validate refresh token (simulates token refresh endpoint)
        let refresh_claims = jwt.verify_refresh_token(&refresh_token).expect("refresh token valid");
        assert_eq!(refresh_claims.sub, user_id.as_str());
        assert!(refresh_claims.is_refresh_token());

        // 7. Issue new access token from refresh (simulates refresh flow)
        let new_access = jwt.sign_token(&user_id, TokenType::Access, 0).expect("new access token");
        let new_claims = jwt.verify_access_token(&new_access).expect("new access token valid");
        assert_eq!(new_claims.sub, user_id.as_str());
    }

    /// Verify that tokens signed by one secret are rejected by another
    #[test]
    fn test_cross_secret_rejection() {
        let jwt_a = JwtService::new("secret-aaaa-long-enough-for-entropy-check-1234567890").unwrap();
        let jwt_b = JwtService::new("secret-bbbb-long-enough-for-entropy-check-1234567890").unwrap();
        let user_id = UserId::new();

        let token = jwt_a.sign_token(&user_id, TokenType::Access, 0).unwrap();
        assert!(jwt_b.verify_access_token(&token).is_err(), "cross-secret token must be rejected");
    }

    /// Verify password validation rejects common weak passwords at the API layer
    #[test]
    fn test_weak_password_rejected_at_validation() {
        use synctv_api::http::validation::validate_password;

        // Common passwords should fail
        assert!(validate_password("password").is_err());
        assert!(validate_password("123456").is_err());
        assert!(validate_password("qwerty").is_err());

        // Non-common passwords that meet length requirement should pass
        assert!(validate_password("MyUniquePass1!").is_ok());
    }

    /// Guest token flow: issue -> validate -> verify room binding
    #[test]
    fn test_guest_auth_flow() {
        let jwt = jwt_service();
        let room_id = synctv_core::models::RoomId::new();

        // Issue guest token
        let token = jwt.sign_guest_token(&room_id).expect("guest token");

        // Validate
        let claims = jwt.verify_guest_token(&token).expect("guest token valid");
        assert!(claims.is_guest());
        assert_eq!(claims.room_id(), room_id);

        // Guest token must not pass as regular access token
        assert!(jwt.verify_access_token(&token).is_err());
    }
}

mod user_request {
    use synctv_api::http::user::UpdateUserRequest;

    #[test]
    fn test_username_only() {
        let json = r#"{"username": "newname"}"#;
        let req: UpdateUserRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.username.as_deref(), Some("newname"));
        assert!(req.password.is_none());
        assert!(req.old_password.is_none());
    }

    #[test]
    fn test_password_change() {
        let json = r#"{"password": "new_pass", "old_password": "old_pass"}"#;
        let req: UpdateUserRequest = serde_json::from_str(json).unwrap();
        assert!(req.username.is_none());
        assert_eq!(req.password.as_deref(), Some("new_pass"));
        assert_eq!(req.old_password.as_deref(), Some("old_pass"));
    }

    #[test]
    fn test_empty_update() {
        let json = r"{}";
        let req: UpdateUserRequest = serde_json::from_str(json).unwrap();
        assert!(req.username.is_none());
        assert!(req.password.is_none());
        assert!(req.old_password.is_none());
    }
}
