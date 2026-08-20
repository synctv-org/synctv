//! JWT token and Authorization header validation.
//!
//! This module provides the shared token validation primitives used by
//! request entrypoints.

use super::{jwt::JwtService, Claims};
use crate::{Error, Result};
use std::sync::Arc;

/// JWT validator for authentication credentials.
///
/// This validator provides consistent token extraction and validation for
/// callers that already have a JWT or bearer credential.
#[derive(Clone)]
pub struct JwtValidator {
    jwt_service: Arc<JwtService>,
}

impl JwtValidator {
    /// Create a new JWT validator
    #[must_use]
    pub const fn new(jwt_service: Arc<JwtService>) -> Self {
        Self { jwt_service }
    }

    /// Extract bearer token from Authorization header value
    ///
    /// Uses case-insensitive comparison per RFC 7235 (auth scheme is case-insensitive).
    pub fn extract_bearer_token(auth_value: &str) -> Result<String> {
        if auth_value.len() <= 7 || !auth_value[..7].eq_ignore_ascii_case("Bearer ") {
            return Err(Error::Authentication(
                "Authorization header must start with 'Bearer '".to_string(),
            ));
        }

        let token = auth_value[7..].trim();
        if token.is_empty() {
            return Err(Error::Authentication(
                "Authorization bearer token cannot be empty".to_string(),
            ));
        }

        Ok(token.to_string())
    }

    /// Validate JWT token and return claims
    ///
    /// This is the core validation method. It verifies the token signature,
    /// expiration, and type.
    ///
    /// # Arguments
    /// * `token` - JWT token string
    ///
    /// # Returns
    /// Claims extracted from the token
    pub fn validate_token(&self, token: &str) -> Result<Claims> {
        self.jwt_service.verify_access_token(token)
    }
}

impl std::fmt::Debug for JwtValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtValidator").finish()
    }
}

/// Authorization-header validation methods.
impl JwtValidator {
    /// Validate JWT from an Authorization header value.
    ///
    /// # Arguments
    /// * `auth_header` - Authorization header value (e.g., "Bearer <token>")
    ///
    /// # Returns
    /// Claims extracted from the token
    ///
    /// # Errors
    /// - Missing Authorization header
    /// - Invalid header format
    /// - Invalid token
    pub fn validate_authorization_header(&self, auth_header: &str) -> Result<Claims> {
        let token = Self::extract_bearer_token(auth_header)?;
        self.validate_token(&token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UserId;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn create_test_jwt_service() -> Arc<JwtService> {
        use super::super::jwt::JwtService;
        Arc::new(ok(
            JwtService::new("test-secret-for-validator-that-is-long-enough-1234567890"),
            "JWT service should build",
        ))
    }

    fn create_test_token(jwt_service: &JwtService, user_id: i64) -> String {
        let user_id = UserId::expect_positive(user_id);
        ok(
            jwt_service.sign_access_token(&user_id, 0),
            "access token should sign",
        )
    }

    #[test]
    fn test_extract_bearer_token() {
        let token = ok(
            JwtValidator::extract_bearer_token("Bearer abc123"),
            "Bearer token should extract",
        );
        assert_eq!(token, "abc123");

        let token = ok(
            JwtValidator::extract_bearer_token("bearer def456"),
            "lowercase bearer token should extract",
        );
        assert_eq!(token, "def456");

        let result = JwtValidator::extract_bearer_token("Basic abc123");
        assert!(matches!(result, Err(Error::Authentication(_))));

        let result = JwtValidator::extract_bearer_token("Bearer    ");
        assert!(
            matches!(result, Err(Error::Authentication(message)) if message.contains("cannot be empty"))
        );
    }

    #[test]
    fn test_validate_token() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, 98_001);

        let claims = ok(validator.validate_token(&token), "token should validate");
        assert_eq!(claims.user_id(), UserId::expect_positive(98_001));

        let result = validator.validate_token("invalid.token.here");
        assert!(matches!(result, Err(Error::Authentication(_))));
    }

    #[test]
    fn test_validate_authorization_header() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, 98_002);

        let claims = ok(
            validator.validate_authorization_header(&format!("Bearer {token}")),
            "authorization header bearer token should validate",
        );
        assert_eq!(claims.user_id(), UserId::expect_positive(98_002));

        let result = validator.validate_authorization_header("Basic invalid");
        assert!(matches!(result, Err(Error::Authentication(_))));
    }
}
