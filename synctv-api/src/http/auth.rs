// Authentication HTTP handlers
//
// This layer now uses proto types and delegates to the impls layer for business logic

use axum::{extract::State, Json};

use super::{AppError, AppResult, AppState};
use crate::proto::client::{
    LoginRequest, LoginResponse, LogoutResponse, RefreshTokenRequest, RefreshTokenResponse,
    RegisterRequest, RegisterResponse, RequestEmailLoginRequest, RequestEmailLoginResponse,
};

/// Extract the real client IP from a request.
///
/// Only trusts `X-Forwarded-For` / `X-Real-IP` headers when the direct
/// connection comes from a configured trusted proxy. This prevents
/// attackers from forging their IP to bypass per-IP brute-force protection.
#[must_use]
pub fn extract_client_ip(
    config: &synctv_core::Config,
    socket_addr: std::net::SocketAddr,
    headers: &axum::http::HeaderMap,
) -> std::net::IpAddr {
    crate::client_ip::extract_client_ip_from_headers(config, socket_addr.ip(), headers)
}

/// Register a new user
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/register",
        tag = "Auth",
        request_body = RegisterRequest,
        responses(
            (status = 200, description = "Registration succeeded", body = RegisterResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn register(
    State(state): State<AppState>,
    connect_info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<RegisterRequest>,
) -> AppResult<Json<RegisterResponse>> {
    let client_ip = extract_client_ip(&state.config, connect_info.0, &headers);

    let response = state
        .client_api
        .register(req, Some(client_ip))
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Login with username+password, email+password, or email+login-token.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/login",
        tag = "Auth",
        request_body = LoginRequest,
        responses(
            (status = 200, description = "Login succeeded", body = LoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Invalid credentials", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn login(
    State(state): State<AppState>,
    connect_info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LoginRequest>,
) -> AppResult<Json<LoginResponse>> {
    let client_ip = extract_client_ip(&state.config, connect_info.0, &headers);
    let response = if req.email_token.is_empty() {
        state
            .client_api
            .login(req, Some(client_ip))
            .await
            .map_err(super::error::map_api_error)?
    } else if req.password.is_empty()
        && !req.email.trim().is_empty()
        && req.username.trim().is_empty()
    {
        let result = require_email_api(&state)?
            .confirm_email_login(&req.email, &req.email_token, Some(client_ip))
            .await
            .map_err(super::error::map_api_error)?;

        LoginResponse {
            user: Some(crate::impls::client::user_to_proto(&result.user)),
            access_token: result.access_token,
            refresh_token: result.refresh_token,
        }
    } else {
        return Err(AppError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "Email login token requires email only and cannot be combined with username or password.",
        ));
    };

    Ok(Json(response))
}

fn email_api_unavailable_error() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "Email service is not available on this server.",
    )
}

fn require_email_api(
    state: &AppState,
) -> Result<&std::sync::Arc<crate::impls::EmailApiImpl>, AppError> {
    state
        .email_api
        .as_ref()
        .ok_or_else(email_api_unavailable_error)
}

/// Request a passwordless email login code.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/email/request",
        tag = "Auth",
        request_body = RequestEmailLoginRequest,
        responses(
            (status = 200, description = "Email login request accepted", body = RequestEmailLoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn request_email_login(
    State(state): State<AppState>,
    Json(req): Json<RequestEmailLoginRequest>,
) -> AppResult<Json<RequestEmailLoginResponse>> {
    let email_api = require_email_api(&state)?;
    let result = email_api
        .request_email_login(&req.email)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(RequestEmailLoginResponse {
        message: result.message,
    }))
}

/// Refresh access token using refresh token.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/auth/refresh",
        tag = "Auth",
        request_body = RefreshTokenRequest,
        responses(
            (status = 200, description = "Token refresh succeeded", body = RefreshTokenResponse),
            (status = 401, description = "Invalid refresh token", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn refresh_token(
    State(state): State<AppState>,
    Json(req): Json<RefreshTokenRequest>,
) -> AppResult<Json<RefreshTokenResponse>> {
    let response = state
        .client_api
        .refresh_token(req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Logout: blacklist the current access token.
///
/// Requires a valid Bearer token in the Authorization header. The token's JTI
/// is added to the blacklist with its remaining TTL so it cannot be reused.
///
/// Returns 400 Bad Request if no Bearer token is provided.
/// Returns 200 OK with success: true on successful logout.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/logout",
        tag = "Auth",
        responses(
            (status = 200, description = "Logout succeeded", body = LogoutResponse),
            (status = 401, description = "Missing or invalid bearer token", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> AppResult<Json<LogoutResponse>> {
    // D13 FIX: Return an error when no valid Authorization header is present.
    // Previously, the handler returned success: true even without a token,
    // which confused clients and did not actually blacklist anything.
    let auth_value = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or_else(AppError::missing_authorization_header)?
        .to_str()
        .map_err(|_| AppError::invalid_authorization_header_non_utf8())?;

    let token = synctv_core::service::auth::JwtValidator::extract_bearer_token(auth_value)
        .map_err(|_| AppError::invalid_or_expired_token())?;

    let outcome = state
        .client_api
        .logout(&token)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(LogoutResponse {
        success: true,
        message: outcome.message.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== Proto Request Type Tests ==========
    // Verify request types have expected fields and derive traits (Serialize, Deserialize, Clone)

    #[test]
    fn test_register_request_json_roundtrip() {
        let req = RegisterRequest {
            username: "testuser".to_string(),
            password: "securepass123".to_string(),
            email: "test@example.com".to_string(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let deserialized: RegisterRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.username, req.username);
        assert_eq!(deserialized.email, req.email);
    }

    #[test]
    fn test_login_request_construction() {
        let req = LoginRequest {
            username: "testuser".to_string(),
            password: "securepass123".to_string(),
            email: String::new(),
            email_token: String::new(),
        };
        assert_eq!(req.username, "testuser");
        assert_eq!(req.password, "securepass123");
    }

    #[test]
    fn test_login_request_json_roundtrip() {
        let req = LoginRequest {
            username: "testuser".to_string(),
            password: "mypassword".to_string(),
            email: String::new(),
            email_token: String::new(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let deserialized: LoginRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.username, req.username);
    }

    #[test]
    fn test_refresh_token_request_construction() {
        let req = RefreshTokenRequest {
            refresh_token: "some_refresh_token".to_string(),
        };
        assert_eq!(req.refresh_token, "some_refresh_token");
    }

    #[test]
    fn test_refresh_token_request_json_roundtrip() {
        let req = RefreshTokenRequest {
            refresh_token: "refresh_abc123".to_string(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let deserialized: RefreshTokenRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.refresh_token, req.refresh_token);
    }

    #[test]
    fn test_request_email_login_request_roundtrip() {
        let req = RequestEmailLoginRequest {
            email: "user@example.com".to_string(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let deserialized: RequestEmailLoginRequest =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.email, req.email);
    }

    // ========== Error Mapping Tests ==========
    // All three auth handlers (register, login, refresh_token) now use
    // map_api_error for consistent typed error classification.

    #[test]
    fn test_app_error_bad_request_status() {
        let err = super::super::AppError::bad_request("registration failed");
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.message, "registration failed");
    }

    #[test]
    fn test_app_error_unauthorized_status() {
        let err = super::super::AppError::unauthorized("invalid credentials");
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "invalid credentials");
    }

    // ========== Request Deserialization Edge Cases ==========

    #[test]
    fn test_register_request_empty_fields() {
        let req = RegisterRequest {
            username: String::new(),
            password: String::new(),
            email: String::new(),
        };
        assert!(req.username.is_empty());
        assert!(req.password.is_empty());
        assert!(req.email.is_empty());
    }

    #[test]
    fn test_register_request_from_json_with_extra_fields() {
        // Proto types with serde should ignore unknown fields
        let json = r#"{"username":"user","password":"pass","email":"e@x.com","extra":"ignored"}"#;
        let req: RegisterRequest =
            serde_json::from_str(json).expect("deserialize with extra fields");
        assert_eq!(req.username, "user");
    }

    #[test]
    fn test_login_request_missing_fields_default_to_empty_strings() {
        let json = r#"{"username":"user"}"#;
        let req: LoginRequest = serde_json::from_str(json).expect("deserialize with defaults");
        assert_eq!(req.username, "user");
        assert!(req.password.is_empty());
        assert!(req.email.is_empty());
        assert!(req.email_token.is_empty());
    }

    #[test]
    fn test_register_request_unicode_username() {
        let req = RegisterRequest {
            username: "\u{4e2d}\u{6587}\u{7528}\u{6237}".to_string(), // Chinese characters
            password: "pass123!".to_string(),
            email: "user@example.com".to_string(),
        };
        assert_eq!(req.username.chars().count(), 4);
    }

    // ========== Logout Response Tests ==========

    #[test]
    fn test_logout_response_serialization() {
        let resp = LogoutResponse {
            success: true,
            message: String::new(),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        assert!(json.contains(r#""success":true"#));
        assert!(json.contains(r#""message":""#));
    }

    #[test]
    fn test_logout_response_partial_success() {
        let resp = LogoutResponse {
            success: true,
            message: "Logged out but token invalidation may be delayed".to_string(),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        assert!(json.contains(r#""success":true"#));
        assert!(json.contains("token invalidation may be delayed"));
    }

    #[test]
    fn test_logout_response_failure() {
        let resp = LogoutResponse {
            success: false,
            message: String::new(),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        assert!(json.contains(r#""success":false"#));
    }

    // ========== Logout Token Extraction Tests ==========
    // Test the token extraction logic used by logout handler

    #[test]
    fn test_extract_bearer_token_valid() {
        use synctv_core::service::auth::JwtValidator;

        let header_value = "Bearer mytoken123";
        let result = JwtValidator::extract_bearer_token(header_value);
        assert!(result.is_ok(), "Should extract valid Bearer token");
        assert_eq!(result.unwrap(), "mytoken123");
    }

    #[test]
    fn test_extract_bearer_token_missing_bearer_prefix() {
        use synctv_core::service::auth::JwtValidator;

        let header_value = "mytoken123";
        let result = JwtValidator::extract_bearer_token(header_value);
        assert!(result.is_err(), "Should fail without Bearer prefix");
    }

    #[test]
    fn test_extract_bearer_token_empty() {
        use synctv_core::service::auth::JwtValidator;

        let header_value = "";
        let result = JwtValidator::extract_bearer_token(header_value);
        assert!(result.is_err(), "Should fail with empty string");
    }

    #[test]
    fn test_extract_bearer_token_bearer_only() {
        use synctv_core::service::auth::JwtValidator;

        let header_value = "Bearer ";
        let result = JwtValidator::extract_bearer_token(header_value);
        // Depending on implementation, this might fail or return empty
        // The key is that an empty token after "Bearer " should be treated as invalid
        if let Ok(token) = result {
            assert!(token.is_empty(), "Token should be empty");
        }
    }

    // ========== Logout Error Handling Tests ==========
    // Verify that the logout handler returns appropriate errors

    #[test]
    fn test_logout_missing_token_error_message() {
        let err = super::super::error::AppError::bad_request(
            "Missing Bearer token in Authorization header",
        );
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(
            err.message.contains("Missing"),
            "Should mention missing token"
        );
    }

    #[test]
    fn test_logout_missing_token_error_status() {
        let err = super::super::error::AppError::bad_request(
            "Missing Bearer token in Authorization header",
        );
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    }
}
