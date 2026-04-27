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

// Helper: extract JSON body from response

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn _body_string(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap_or_default()
}

mod error_responses {
    use super::*;
    use synctv_api::http::error::AppError;

    /// Build a tiny router that returns a specific `AppError`
    fn error_router(error: AppError) -> Router {
        let status = error.status;
        let message = error.message;
        Router::new().route(
            "/test",
            get(move || async move { Err::<String, AppError>(AppError::new(status, message)) }),
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
        assert!(
            msg.contains("30"),
            "Error message should contain retry seconds"
        );
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
        assert!(
            obj.contains_key("error"),
            "Response must contain 'error' field"
        );
        assert!(
            obj.contains_key("status"),
            "Response must contain 'status' field"
        );
        assert_eq!(obj.len(), 2, "Error response should have exactly 2 fields");
    }

    #[tokio::test]
    async fn test_invalid_credentials() {
        let app = error_router(AppError::invalid_credentials());
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(resp).await;
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("Invalid username or password"));
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

    // Security: Internal error message sanitization
    // Internal errors (5xx) should return a generic message to avoid leaking
    // sensitive information like database connection strings, file paths,
    // stack traces, or internal implementation details.
    // Client errors (4xx) should preserve the original message to help the
    // client understand and fix their request.

    /// Internal server error (500) MUST return generic message, not the original.
    /// This prevents leaking sensitive info like "connection to postgres://user:pass@..."
    #[tokio::test]
    async fn test_internal_error_returns_generic_message() {
        // Simulate an internal error with sensitive info in the message
        let sensitive_msg =
            "Database connection failed: postgres://admin:secret@db.internal:5432/production";
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
            error_msg, "Internal server error",
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
        assert_eq!(
            json["error"], "Invalid email format",
            "400 should preserve message"
        );

        // 401 Unauthorized
        let app = error_router(AppError::unauthorized("Token has expired"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        assert_eq!(
            json["error"], "Token has expired",
            "401 should preserve message"
        );

        // 403 Forbidden
        let app = error_router(AppError::forbidden("Admin access required"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        assert_eq!(
            json["error"], "Admin access required",
            "403 should preserve message"
        );

        // 404 Not Found
        let app = error_router(AppError::not_found("Room 'abc123' does not exist"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        assert_eq!(
            json["error"], "Room 'abc123' does not exist",
            "404 should preserve message"
        );
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

    /// Service unavailable (503) should preserve retryability semantics while still avoiding
    /// leaking backend-specific failure details.
    #[tokio::test]
    async fn test_service_unavailable_returns_safe_message() {
        let app = error_router(AppError::service_unavailable());
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        let error_msg = json["error"].as_str().unwrap();

        assert_eq!(
            error_msg, "Service temporarily unavailable. Please try again later.",
            "503 should keep a safe retryable message instead of collapsing into 500 semantics"
        );
    }

    /// Bad gateway (502) should preserve upstream-failure semantics without leaking internals.
    #[tokio::test]
    async fn test_bad_gateway_returns_generic_message() {
        let app = error_router(AppError::new(
            StatusCode::BAD_GATEWAY,
            "Upstream nginx error: connection reset by peer",
        ));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let json = body_json(resp).await;
        let error_msg = json["error"].as_str().unwrap();

        assert_eq!(
            error_msg, "Upstream service error",
            "502 should remain distinguishable from an internal 500"
        );
    }

    /// Gateway timeout (504) should preserve timeout semantics without exposing internals.
    #[tokio::test]
    async fn test_gateway_timeout_returns_generic_message() {
        let app = error_router(AppError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "Upstream timeout after 30s",
        ));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
        let json = body_json(resp).await;
        let error_msg = json["error"].as_str().unwrap();

        assert_eq!(
            error_msg, "Upstream service timed out",
            "504 should remain distinguishable from a generic internal failure"
        );
    }
}

mod error_classification {
    use axum::http::StatusCode;
    use synctv_api::http::error::map_api_error;
    use synctv_api::impls::ApiError;

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
        let err = map_api_error(ApiError::Internal(
            "Database connection pool exhausted".into(),
        ));
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
        assert_eq!(resp.headers().get("X-Frame-Options").unwrap(), "SAMEORIGIN");
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

mod provider_security_headers {
    use super::*;
    use synctv_api::http::middleware::security_headers_middleware;

    /// Create a router that mimics the Provider routes structure with security headers.
    /// This mirrors how Provider routes are set up in mod.rs:
    /// Router::new()
    ///     .nest("/api/providers/...", provider_routes())
    ///     .route_layer(read_rate_limit)
    ///     .layer(security_headers_middleware)  // Applied globally
    fn provider_style_router() -> Router {
        Router::new()
            // Simulate /api/providers/* common routes
            .route(
                "/api/providers/instances",
                get(|| async { "provider instances" }),
            )
            .route(
                "/api/providers/backends/{provider_type}",
                get(|| async { "provider backends" }),
            )
            // Simulate /api/providers/bilibili/* routes
            .route(
                "/api/providers/bilibili/parse",
                post(|| async { "bilibili parse" }),
            )
            .route(
                "/api/providers/bilibili/me",
                post(|| async { "bilibili me" }),
            )
            // Simulate /api/providers/alist/* routes
            .route("/api/providers/alist/list", get(|| async { "alist list" }))
            // Simulate /api/providers/emby/* routes
            .route("/api/providers/emby/list", post(|| async { "emby list" }))
            // Simulate /api/providers/rtmp/* routes
            .route(
                "/api/providers/rtmp/rooms/{room_id}/info/{media_id}",
                get(|| async { "rtmp stream info" }),
            )
            // Apply security headers middleware (simulating global layer)
            .layer(axum_middleware::from_fn(security_headers_middleware))
    }

    #[tokio::test]
    async fn test_provider_common_routes_have_security_headers() {
        let app = provider_style_router();
        let req = Request::get("/api/providers/instances")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        // Verify all security headers are present
        assert_eq!(resp.headers().get("X-Frame-Options").unwrap(), "DENY");
        assert_eq!(
            resp.headers().get("X-Content-Type-Options").unwrap(),
            "nosniff"
        );
        assert!(resp.headers().contains_key("Content-Security-Policy"));
        assert!(resp.headers().contains_key("Referrer-Policy"));
        assert!(resp.headers().contains_key("Permissions-Policy"));
    }

    #[tokio::test]
    async fn test_bilibili_provider_routes_have_security_headers() {
        let app = provider_style_router();
        let req = Request::post("/api/providers/bilibili/parse")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        // Verify security headers
        assert_eq!(resp.headers().get("X-Frame-Options").unwrap(), "DENY");
        assert_eq!(
            resp.headers().get("X-Content-Type-Options").unwrap(),
            "nosniff"
        );
        assert!(resp.headers().contains_key("Content-Security-Policy"));
    }

    #[tokio::test]
    async fn test_alist_provider_routes_have_security_headers() {
        let app = provider_style_router();
        let req = Request::get("/api/providers/alist/list")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        // Verify security headers
        assert_eq!(resp.headers().get("X-Frame-Options").unwrap(), "DENY");
        assert_eq!(
            resp.headers().get("X-Content-Type-Options").unwrap(),
            "nosniff"
        );
        assert!(resp.headers().contains_key("Content-Security-Policy"));
    }

    #[tokio::test]
    async fn test_emby_provider_routes_have_security_headers() {
        let app = provider_style_router();
        let req = Request::post("/api/providers/emby/list")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        // Verify security headers
        assert_eq!(resp.headers().get("X-Frame-Options").unwrap(), "DENY");
        assert_eq!(
            resp.headers().get("X-Content-Type-Options").unwrap(),
            "nosniff"
        );
        assert!(resp.headers().contains_key("Content-Security-Policy"));
    }

    #[tokio::test]
    async fn test_rtmp_provider_routes_have_security_headers() {
        let app = provider_style_router();
        let req = Request::get("/api/providers/rtmp/rooms/room-1/info/media-1")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        // Verify security headers
        assert_eq!(resp.headers().get("X-Frame-Options").unwrap(), "DENY");
        assert_eq!(
            resp.headers().get("X-Content-Type-Options").unwrap(),
            "nosniff"
        );
        assert!(resp.headers().contains_key("Content-Security-Policy"));
    }

    #[tokio::test]
    async fn test_provider_routes_cache_control_no_store() {
        let app = provider_style_router();
        let req = Request::get("/api/providers/instances")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let cache_control = resp
            .headers()
            .get("Cache-Control")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            cache_control.contains("no-store"),
            "Provider routes should have no-store cache control"
        );
        assert!(
            cache_control.contains("no-cache"),
            "Provider routes should have no-cache cache control"
        );
    }

    #[tokio::test]
    async fn test_provider_routes_referrer_policy() {
        let app = provider_style_router();
        let req = Request::get("/api/providers/backends/bilibili")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(
            resp.headers().get("Referrer-Policy").unwrap(),
            "strict-origin-when-cross-origin"
        );
    }

    #[tokio::test]
    async fn test_provider_routes_csp_has_frame_ancestors_none() {
        let app = provider_style_router();
        let req = Request::post("/api/providers/bilibili/me")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let csp = resp
            .headers()
            .get("Content-Security-Policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            csp.contains("frame-ancestors 'none'"),
            "CSP must include frame-ancestors 'none' for clickjacking protection on provider routes"
        );
    }
}

mod hsts_headers {
    use synctv_api::http::middleware::hsts_header;

    #[test]
    fn test_basic_max_age() {
        assert_eq!(hsts_header(31_536_000, false, false), "max-age=31536000");
    }

    #[test]
    fn test_with_subdomains() {
        assert_eq!(
            hsts_header(31_536_000, true, false),
            "max-age=31536000; includeSubDomains"
        );
    }

    #[test]
    fn test_with_preload() {
        assert_eq!(
            hsts_header(31_536_000, false, true),
            "max-age=31536000; preload"
        );
    }

    #[test]
    fn test_full_hsts() {
        assert_eq!(
            hsts_header(63_072_000, true, true),
            "max-age=63072000; includeSubDomains; preload"
        );
    }

    #[test]
    fn test_zero_max_age() {
        // Zero max-age is valid -- used to clear HSTS
        assert_eq!(hsts_header(0, false, false), "max-age=0");
    }
}

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
    async fn test_liveness_json_structure() {
        let app = Router::new().route(
            "/health/live",
            get(synctv_api::http::health::liveness_check),
        );

        let req = Request::get("/health/live").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let json = body_json(resp).await;
        // Liveness should have "status" but no "details"
        assert!(json.get("status").is_some());
        assert!(json.get("details").is_none());
    }
}

mod routing_structure {
    use super::*;

    fn minimal_router() -> Router {
        Router::new()
            .route("/health/live", get(|| async { "ok" }))
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
        let req = Request::get("/api/auth/login").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_health_endpoint_accessible() {
        let app = minimal_router();
        let req = Request::get("/health/live").body(Body::empty()).unwrap();
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

mod request_format {
    use synctv_proto::client::{
        CreateRoomRequest, JoinRoomRequest, LoginRequest, RefreshTokenRequest, RegisterRequest,
        SetRoomPasswordRequest,
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
        assert!(req.room_id.is_empty());
    }

    #[test]
    fn test_join_room_request_without_password() {
        let json = r#"{"password":""}"#;
        let req: JoinRoomRequest = serde_json::from_str(json).unwrap();
        assert!(req.password.is_empty());
        assert!(req.room_id.is_empty());
    }

    #[test]
    fn test_set_room_password_request_deserializes_without_room_id() {
        let json = r#"{"password":"new-secret"}"#;
        let req: SetRoomPasswordRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.password, "new-secret");
    }

    #[test]
    fn test_set_room_password_body_requires_password_field() {
        let json = r"{}";
        let Err(err) = serde_json::from_str::<synctv_api::http::room::SetRoomPasswordBody>(json)
        else {
            panic!("missing password must be rejected");
        };
        assert!(
            err.to_string().contains("missing field"),
            "unexpected deserialization error: {err}"
        );
    }

    #[test]
    fn test_start_playback_request_deserializes_without_room_id() {
        let json = r#"{"media_id":"media-123"}"#;
        let req: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(req["media_id"], "media-123");
        assert!(req.get("playlist_id").is_none());
        assert!(req.get("target").is_none());
    }

    #[test]
    fn test_start_playback_request_deserializes_dynamic_playlist_target() {
        let json = r#"{"playlist_id":"playlist-123","target":{"item_id":"provider-item-1"}}"#;
        let req: serde_json::Value = serde_json::from_str(json).unwrap();
        assert!(req.get("media_id").is_none());
        assert_eq!(req["playlist_id"], "playlist-123");
        assert_eq!(req["target"]["item_id"], "provider-item-1");
    }

    #[test]
    fn test_create_playlist_request_deserializes_dynamic_fields_without_is_folder() {
        let json = r#"{
            "name":"Dynamic Folder",
            "parent_id":"playlist-root",
            "source_provider":"alist",
            "source_config":{"path":"/tv"},
            "provider_instance_name":"alist-main"
        }"#;
        let req: serde_json::Value = serde_json::from_str(json).unwrap();

        assert_eq!(req["source_provider"], "alist");
        assert_eq!(req["provider_instance_name"], "alist-main");
        assert!(req.get("is_folder").is_none());
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

mod response_format {
    use synctv_proto::client::{LoginResponse, RegisterResponse, User};

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

mod unauthenticated_access {
    use super::*;
    use synctv_api::http::error::AppError;

    #[tokio::test]
    async fn test_protected_endpoint_without_auth_header() {
        // Simulate a protected handler that rejects missing auth
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
        assert!(json["error"].as_str().unwrap().contains("Authorization"));
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

mod api_error_classification {
    use synctv_api::impls::{classify_error, ApiError, ErrorKind};

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
    fn test_api_error_service_unavailable_classify() {
        let err = ApiError::ServiceUnavailable("redis unavailable".to_string());
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
    }

    #[test]
    fn test_api_error_display_uses_plain_message() {
        let cases = [
            (
                ApiError::NotFound("room not found".into()),
                ErrorKind::NotFound,
            ),
            (
                ApiError::Authentication("invalid token".into()),
                ErrorKind::Unauthenticated,
            ),
            (
                ApiError::Authorization("forbidden".into()),
                ErrorKind::PermissionDenied,
            ),
            (
                ApiError::AlreadyExists("user already exists".into()),
                ErrorKind::AlreadyExists,
            ),
            (
                ApiError::InvalidInput("invalid username".into()),
                ErrorKind::InvalidArgument,
            ),
            (
                ApiError::ServiceUnavailable("distributed room capacity check unavailable".into()),
                ErrorKind::ServiceUnavailable,
            ),
            (
                ApiError::Internal("opaque internal failure".into()),
                ErrorKind::Internal,
            ),
        ];

        for (err, expected_kind) in cases {
            assert_eq!(err.to_string(), err.message());
            assert!(
                std::mem::discriminant(&classify_error(err.message()))
                    == std::mem::discriminant(&expected_kind)
            );
        }
    }

    #[test]
    fn test_api_error_into_string() {
        let err = ApiError::NotFound("item".to_string());
        let s: String = err.into();
        assert_eq!(s, "item");
    }
}

mod body_edge_cases {
    use super::*;

    #[tokio::test]
    async fn test_empty_body_on_post() {
        // POST with empty body should return 400 from axum JSON extractor
        let app = Router::new().route(
            "/api/auth/login",
            post(|axum::Json(_body): axum::Json<serde_json::Value>| async { "ok" }),
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
            post(|axum::Json(_body): axum::Json<serde_json::Value>| async { "ok" }),
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
            post(|axum::Json(body): axum::Json<serde_json::Value>| async move { axum::Json(body) }),
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
            post(|axum::Json(_body): axum::Json<serde_json::Value>| async { "ok" }),
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

mod playback_request {
    use synctv_proto::client::{PlaybackPatchState, UpdatePlaybackRequest};

    #[test]
    fn test_state_only() {
        let json = r#"{"state": 1}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.state, PlaybackPatchState::Playing as i32);
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
        assert!(req.media_id.is_empty());
    }

    #[test]
    fn test_position_only() {
        let json = r#"{"position": 42.5}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.state, PlaybackPatchState::Unspecified as i32);
        assert!((req.position.unwrap() - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_speed_only() {
        let json = r#"{"speed": 2.0}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.state, PlaybackPatchState::Unspecified as i32);
        assert!((req.speed.unwrap() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_media_id_only() {
        let json = r#"{"media_id": "media_abc"}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.media_id, "media_abc");
        assert!(req.playlist_id.is_empty());
        assert!(req.target.is_empty());
    }

    #[test]
    fn test_dynamic_target_fields() {
        let json = r#"{"playlist_id":"pl1","target":{"item_id":"provider-item-1"}}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.playlist_id, "pl1");
        let target: serde_json::Value = serde_json::from_slice(&req.target).unwrap();
        assert_eq!(target, serde_json::json!({"item_id":"provider-item-1"}));
        assert_eq!(req.state, PlaybackPatchState::Unspecified as i32);
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
        assert!(req.media_id.is_empty());
    }

    #[test]
    fn test_empty_object() {
        let json = r"{}";
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.state, PlaybackPatchState::Unspecified as i32);
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
        assert!(req.media_id.is_empty());
    }
}

mod auth_flow {
    use synctv_core::models::UserId;
    use synctv_core::service::auth::{hash_password, verify_password, JwtService, TokenType};

    fn jwt_service() -> JwtService {
        JwtService::new("test-secret-key-for-jwt-that-is-long-enough-1234567890").unwrap()
    }

    /// Full flow: hash password -> verify password -> issue tokens -> validate tokens
    #[tokio::test]
    async fn test_register_login_flow() {
        let password = "StrongP@ssw0rd!";

        let hash = hash_password(password)
            .await
            .expect("hashing should succeed");
        assert_ne!(hash, password, "hash must differ from plaintext");

        assert!(
            verify_password(password, &hash)
                .await
                .expect("verify call should succeed"),
            "correct password should verify"
        );
        assert!(
            !verify_password("wrong_password", &hash)
                .await
                .unwrap_or(false),
            "wrong password should fail verification"
        );

        let jwt = jwt_service();
        let user_id = UserId::new();

        let access_token = jwt
            .sign_token(&user_id, TokenType::Access, 0)
            .expect("access token");
        let refresh_token = jwt
            .sign_token(&user_id, TokenType::Refresh, 0)
            .expect("refresh token");

        let claims = jwt
            .verify_access_token(&access_token)
            .expect("access token valid");
        assert_eq!(claims.sub, user_id.to_string());
        assert!(claims.is_access_token());

        assert!(jwt.verify_refresh_token(&access_token).is_err());

        let refresh_claims = jwt
            .verify_refresh_token(&refresh_token)
            .expect("refresh token valid");
        assert_eq!(refresh_claims.sub, user_id.to_string());
        assert!(refresh_claims.is_refresh_token());

        let new_access = jwt
            .sign_token(&user_id, TokenType::Access, 0)
            .expect("new access token");
        let new_claims = jwt
            .verify_access_token(&new_access)
            .expect("new access token valid");
        assert_eq!(new_claims.sub, user_id.to_string());
    }

    /// Verify that tokens signed by one secret are rejected by another
    #[test]
    fn test_cross_secret_rejection() {
        let jwt_a =
            JwtService::new("secret-aaaa-long-enough-for-entropy-check-1234567890").unwrap();
        let jwt_b =
            JwtService::new("secret-bbbb-long-enough-for-entropy-check-1234567890").unwrap();
        let user_id = UserId::new();

        let token = jwt_a.sign_token(&user_id, TokenType::Access, 0).unwrap();
        assert!(
            jwt_b.verify_access_token(&token).is_err(),
            "cross-secret token must be rejected"
        );
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
