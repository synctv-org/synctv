// Authentication HTTP handlers
//
// This layer now uses proto types and delegates to the impls layer for business logic

use axum::{extract::State, Json};

use super::{AppResult, AppState};
use crate::proto::client::{RegisterRequest, RegisterResponse, LoginRequest, LoginResponse, RefreshTokenRequest, RefreshTokenResponse};

/// Register a new user
pub async fn register(
    State(state): State<AppState>,
    Json(mut req): Json<RegisterRequest>,
) -> AppResult<Json<RegisterResponse>> {
    // Validate and sanitize username
    req.username = super::validation::validate_username(&req.username)
        .map_err(|e| super::AppError::bad_request(e.to_string()))?;

    // Validate password strength
    super::validation::validate_password(&req.password)
        .map_err(|e| super::AppError::bad_request(e.to_string()))?;

    let response = state
        .client_api
        .register(req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Login with username and password
pub async fn login(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LoginRequest>,
) -> AppResult<Json<LoginResponse>> {
    // Extract client IP from X-Forwarded-For or X-Real-IP headers.
    // Behind a reverse proxy these headers carry the real client IP.
    let client_ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
        });

    let response = state
        .client_api
        .login(req, client_ip)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Refresh access token using refresh token.
///
/// If the caller includes an `Authorization: Bearer <access_token>` header,
/// the old access token will be blacklisted to prevent further use.
pub async fn refresh_token(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<RefreshTokenRequest>,
) -> AppResult<Json<RefreshTokenResponse>> {
    let old_access_token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| synctv_core::service::auth::JwtValidator::extract_bearer_token(s).ok());

    let response = state
        .client_api
        .refresh_token(req, old_access_token.as_deref())
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
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
        let cloned = req.clone();
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
        let req: RegisterRequest = serde_json::from_str(json).expect("deserialize with extra fields");
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
}
