//! HTTP integration tests for synctv-api
//!
//! Tests the HTTP layer: error responses, security headers, and health endpoints.
//!
//! These tests use `tower::ServiceExt::oneshot()` to test the axum router
//! without starting a TCP server, and do not require database or Redis.

#![allow(clippy::unwrap_used)]
use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware as axum_middleware,
    routing::get,
    Router,
};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn grpc_code_for_http_status(status: StatusCode) -> i32 {
    match status {
        StatusCode::BAD_REQUEST => tonic::Code::InvalidArgument as i32,
        StatusCode::UNAUTHORIZED => tonic::Code::Unauthenticated as i32,
        StatusCode::FORBIDDEN => tonic::Code::PermissionDenied as i32,
        StatusCode::NOT_FOUND => tonic::Code::NotFound as i32,
        StatusCode::CONFLICT => tonic::Code::Aborted as i32,
        StatusCode::TOO_MANY_REQUESTS => tonic::Code::ResourceExhausted as i32,
        StatusCode::SERVICE_UNAVAILABLE => tonic::Code::Unavailable as i32,
        StatusCode::GATEWAY_TIMEOUT | StatusCode::REQUEST_TIMEOUT => {
            tonic::Code::DeadlineExceeded as i32
        }
        _ => tonic::Code::Internal as i32,
    }
}

mod error_responses {
    use super::*;
    use synctv_api::AppError;

    fn error_router(error: &AppError) -> Router {
        let status = error.status();
        let message = error.message().to_string();
        Router::new().route(
            "/test",
            get(move || async move { Err::<String, AppError>(AppError::new(status, message)) }),
        )
    }

    #[tokio::test]
    async fn test_error_response_statuses() {
        let cases = [
            (
                AppError::bad_request("invalid input"),
                StatusCode::BAD_REQUEST,
                Some("invalid input"),
            ),
            (
                AppError::unauthorized("not authenticated"),
                StatusCode::UNAUTHORIZED,
                Some("not authenticated"),
            ),
            (
                AppError::forbidden("access denied"),
                StatusCode::FORBIDDEN,
                Some("access denied"),
            ),
            (
                AppError::not_found("room not found"),
                StatusCode::NOT_FOUND,
                Some("room not found"),
            ),
            (
                AppError::conflict("already exists"),
                StatusCode::CONFLICT,
                None,
            ),
            (
                AppError::rate_limited(30),
                StatusCode::TOO_MANY_REQUESTS,
                Some("30"),
            ),
            (
                AppError::internal_server_error("something broke"),
                StatusCode::INTERNAL_SERVER_ERROR,
                None,
            ),
            (
                AppError::service_unavailable(),
                StatusCode::SERVICE_UNAVAILABLE,
                None,
            ),
        ];

        for (error, expected_status, expected_message_part) in cases {
            let app = error_router(&error);
            let req = Request::get("/test").body(Body::empty()).unwrap();
            let resp = app.oneshot(req).await.unwrap();

            assert_eq!(resp.status(), expected_status);
            let json = body_json(resp).await;
            assert_eq!(json["code"], grpc_code_for_http_status(expected_status));
            if let Some(expected_message_part) = expected_message_part {
                assert!(
                    json["message"]
                        .as_str()
                        .expect("error response should contain a string message")
                        .contains(expected_message_part),
                    "expected error message to contain {expected_message_part:?}, got {:?}",
                    json["message"]
                );
            }
        }
    }

    #[tokio::test]
    async fn test_error_response_json_structure() {
        let app = error_router(&AppError::bad_request("test"));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let json = body_json(resp).await;
        let obj = json.as_object().unwrap();
        assert!(
            obj.contains_key("code"),
            "Response must contain 'code' field"
        );
        assert!(
            obj.contains_key("message"),
            "Response must contain 'message' field"
        );
        assert!(
            obj.contains_key("details"),
            "Response must contain 'details' field"
        );
    }

    #[tokio::test]
    async fn test_app_error_helpers_return_public_messages() {
        let cases = [
            (
                AppError::permission_denied(),
                StatusCode::FORBIDDEN,
                Vec::new(),
            ),
            (
                AppError::bad_request("Invalid email format"),
                StatusCode::BAD_REQUEST,
                vec!["Invalid email format"],
            ),
            (
                AppError::unauthorized("Token has expired"),
                StatusCode::UNAUTHORIZED,
                vec!["Token has expired"],
            ),
            (
                AppError::forbidden("Admin access required"),
                StatusCode::FORBIDDEN,
                vec!["Admin access required"],
            ),
            (
                AppError::not_found("Room 'abc123' does not exist"),
                StatusCode::NOT_FOUND,
                vec!["Room 'abc123' does not exist"],
            ),
        ];

        for (error, expected_status, expected_parts) in cases {
            let app = error_router(&error);
            let req = Request::get("/test").body(Body::empty()).unwrap();
            let resp = app.oneshot(req).await.unwrap();

            assert_eq!(resp.status(), expected_status);
            let json = body_json(resp).await;
            let message = json["message"].as_str().unwrap();
            for expected_part in expected_parts {
                assert!(
                    message.contains(expected_part),
                    "expected {message:?} to contain {expected_part:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_internal_error_returns_generic_message() {
        let sensitive_msg =
            "Database connection failed: postgres://admin:secret@db.internal:5432/production";
        let app = error_router(&AppError::internal_server_error(sensitive_msg));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(resp).await;
        let error_msg = json["message"].as_str().unwrap();

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

        assert_eq!(
            error_msg, "Internal error",
            "Internal error should return generic message"
        );
    }

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

        let sensitive_msg = "Error: panic! at /etc/config.toml - postgres://admin:password@localhost redis://localhost private_key=abc123";
        let app = error_router(&AppError::internal_server_error(sensitive_msg));
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        let error_msg = json["message"].as_str().unwrap().to_lowercase();

        for pattern in &sensitive_patterns {
            assert!(
                !error_msg.contains(&pattern.to_lowercase()),
                "Response must not contain sensitive pattern: {pattern}"
            );
        }
    }

    #[tokio::test]
    async fn test_server_error_semantics_are_preserved_with_safe_messages() {
        let cases = [
            (
                AppError::service_unavailable(),
                StatusCode::SERVICE_UNAVAILABLE,
                "Service temporarily unavailable. Please try again later.",
            ),
            (
                AppError::new(
                    StatusCode::BAD_GATEWAY,
                    "Upstream nginx error: connection reset by peer",
                ),
                StatusCode::BAD_GATEWAY,
                "Upstream service error",
            ),
            (
                AppError::new(StatusCode::GATEWAY_TIMEOUT, "Upstream timeout after 30s"),
                StatusCode::GATEWAY_TIMEOUT,
                "Upstream service timed out",
            ),
        ];

        for (error, expected_status, expected_message) in cases {
            let app = error_router(&error);
            let req = Request::get("/test").body(Body::empty()).unwrap();
            let resp = app.oneshot(req).await.unwrap();

            assert_eq!(resp.status(), expected_status);
            let json = body_json(resp).await;
            assert_eq!(json["message"], expected_message);
        }
    }
}

mod error_classification {
    use axum::http::StatusCode;
    use synctv_api::{map_api_error, ApiError};
    use synctv_core::Error as CoreError;

    #[test]
    fn test_api_errors_map_to_http_status_and_public_messages() {
        let cases = [
            (
                ApiError::NotFound("room abc123".into()),
                StatusCode::NOT_FOUND,
                Some("room abc123"),
            ),
            (
                ApiError::Authentication("token expired".into()),
                StatusCode::UNAUTHORIZED,
                Some("token expired"),
            ),
            (
                ApiError::Authorization("forbidden".into()),
                StatusCode::FORBIDDEN,
                Some("forbidden"),
            ),
            (
                ApiError::AlreadyExists("username taken".into()),
                StatusCode::CONFLICT,
                Some("username taken"),
            ),
            (
                ApiError::InvalidInput("bad email format".into()),
                StatusCode::BAD_REQUEST,
                Some("bad email format"),
            ),
            (
                ApiError::Internal("Something went wrong".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                Some("Internal error"),
            ),
        ];

        for (api_error, expected_status, expected_message) in cases {
            let err = map_api_error(api_error);
            assert_eq!(err.status(), expected_status);
            if let Some(expected_message) = expected_message {
                assert!(
                    err.message().contains(expected_message),
                    "expected mapped error message to contain {expected_message:?}, got {:?}",
                    err.message()
                );
            }
        }
    }

    #[test]
    fn test_core_errors_map_to_http_statuses() {
        let cases = [
            (
                CoreError::NotFound("room 123".into()),
                StatusCode::NOT_FOUND,
            ),
            (
                CoreError::Authentication("expired".into()),
                StatusCode::UNAUTHORIZED,
            ),
            (
                CoreError::Authorization("denied".into()),
                StatusCode::FORBIDDEN,
            ),
        ];

        for (core_err, expected_status) in cases {
            let app_err = map_api_error(ApiError::from(core_err));
            assert_eq!(app_err.status(), expected_status);
        }
    }
}

mod security_headers {
    use super::*;
    use synctv_api::security_headers_middleware;

    fn security_app() -> Router {
        Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(axum_middleware::from_fn(security_headers_middleware))
    }

    #[tokio::test]
    async fn test_security_header_values() {
        let app = security_app();
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.headers().get("X-Frame-Options").unwrap(), "DENY");
        assert_eq!(
            resp.headers().get("X-Content-Type-Options").unwrap(),
            "nosniff"
        );
        assert_eq!(
            resp.headers().get("Referrer-Policy").unwrap(),
            "strict-origin-when-cross-origin"
        );

        let csp = resp
            .headers()
            .get("Content-Security-Policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("media-src 'none'"));
        assert!(csp.contains("frame-src 'none'"));
        assert!(csp.contains("connect-src 'self' wss: ws:"));

        let pp = resp
            .headers()
            .get("Permissions-Policy")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(pp.contains("camera=()"));
        assert!(pp.contains("microphone=()"));
        assert!(pp.contains("geolocation=()"));

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

        assert_eq!(resp.headers().get("X-Frame-Options").unwrap(), "SAMEORIGIN");
    }

    #[tokio::test]
    async fn test_all_security_headers_present_on_404() {
        let app = Router::new()
            .route("/other", get(|| async { "ok" }))
            .layer(axum_middleware::from_fn(security_headers_middleware));

        let req = Request::get("/nonexistent").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert!(resp.headers().contains_key("X-Frame-Options"));
        assert!(resp.headers().contains_key("X-Content-Type-Options"));
        assert!(resp.headers().contains_key("Referrer-Policy"));
    }
}

mod hsts_headers {
    use synctv_api::hsts_header;

    #[test]
    fn test_hsts_header_variants() {
        let cases = [
            (31_536_000, false, false, "max-age=31536000"),
            (
                31_536_000,
                true,
                false,
                "max-age=31536000; includeSubDomains",
            ),
            (31_536_000, false, true, "max-age=31536000; preload"),
            (
                63_072_000,
                true,
                true,
                "max-age=63072000; includeSubDomains; preload",
            ),
            (0, false, false, "max-age=0"),
        ];

        for (max_age, include_subdomains, preload, expected) in cases {
            assert_eq!(hsts_header(max_age, include_subdomains, preload), expected);
        }
    }
}

mod auth_flow {
    use synctv_core::models::UserId;
    use synctv_core::service::JwtService;

    #[test]
    fn test_cross_secret_rejection() {
        let jwt_a =
            JwtService::new("secret-aaaa-long-enough-for-entropy-check-1234567890").unwrap();
        let jwt_b =
            JwtService::new("secret-bbbb-long-enough-for-entropy-check-1234567890").unwrap();
        let user_id = UserId::new();

        let token = jwt_a.sign_access_token(&user_id, 0).unwrap();
        assert!(
            jwt_b.verify_access_token(&token).is_err(),
            "cross-secret token must be rejected"
        );
    }
}
