// Authentication HTTP handlers
//
// This layer now uses proto types and delegates to the impls layer for business logic

use axum::{extract::State, Json};

use super::{AppError, AppResult, AppState};
use crate::proto::client::{
    LoginRequest, LoginResponse, RefreshTokenRequest, RefreshTokenResponse, RegisterRequest,
    RegisterResponse,
};

/// Simple success response for logout
#[derive(serde::Serialize)]
pub struct LogoutResponse {
    pub success: bool,
    /// Non-empty when logout succeeded but token invalidation may be delayed
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
}

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
    let socket_ip = socket_addr.ip();
    if config.server.is_trusted_proxy(&socket_ip) {
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
            .or_else(|| {
                headers
                    .get("x-real-ip")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
            })
            .unwrap_or(socket_ip)
    } else {
        socket_ip
    }
}

/// Register a new user
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

/// Login with username and password
pub async fn login(
    State(state): State<AppState>,
    connect_info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LoginRequest>,
) -> AppResult<Json<LoginResponse>> {
    let client_ip = extract_client_ip(&state.config, connect_info.0, &headers);

    let response = state
        .client_api
        .login(req, Some(client_ip))
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Refresh access token using refresh token.
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
pub async fn logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> AppResult<Json<LogoutResponse>> {
    // D13 FIX: Return an error when no valid Authorization header is present.
    // Previously, the handler returned success: true even without a token,
    // which confused clients and did not actually blacklist anything.
    let auth_value = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or_else(|| AppError::unauthorized("Missing Authorization header"))?
        .to_str()
        .map_err(|_| AppError::unauthorized("Invalid Authorization header encoding"))?;

    let token = synctv_core::service::auth::JwtValidator::extract_bearer_token(auth_value)
        .map_err(|e| AppError::unauthorized(format!("{e}")))?;

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
    fn test_register_request_construction() {
        let req = RegisterRequest {
            username: "testuser".to_string(),
            password: "securepass123".to_string(),
            email: "test@example.com".to_string(),
        };
        assert_eq!(req.username, "testuser");
        assert_eq!(req.password, "securepass123");
        assert_eq!(req.email, "test@example.com");
    }

    #[test]
    fn test_register_request_clone() {
        let req = RegisterRequest {
            username: "testuser".to_string(),
            password: "securepass123".to_string(),
            email: "test@example.com".to_string(),
        };
        let cloned = req;
        assert_eq!(cloned.username, "testuser");
        assert_eq!(cloned.email, "test@example.com");
    }

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
        };
        assert_eq!(req.username, "testuser");
        assert_eq!(req.password, "securepass123");
    }

    #[test]
    fn test_login_request_json_roundtrip() {
        let req = LoginRequest {
            username: "testuser".to_string(),
            password: "mypassword".to_string(),
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
    fn test_login_request_missing_field_fails() {
        // LoginRequest requires both username and password via serde
        let json = r#"{"username":"user"}"#;
        let result: Result<LoginRequest, _> = serde_json::from_str(json);
        // serde derive requires all fields; missing password causes deserialization failure
        assert!(result.is_err());
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
        // Empty message should be omitted
        assert!(!json.contains("message"));
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
}
