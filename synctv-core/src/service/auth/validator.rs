//! Unified JWT validation for HTTP and gRPC
//!
//! This module provides a single source of truth for JWT validation across
//! both HTTP middleware and gRPC interceptors, eliminating code duplication
//! and ensuring consistent authentication behavior.

use super::{jwt::JwtService, Claims};
use crate::{models::UserId, Error, Result};
use std::sync::Arc;
use tonic::{metadata::MetadataMap, Status};

/// Unified JWT validator for HTTP and gRPC authentication
///
/// This validator provides consistent token extraction and validation
/// for both HTTP (Authorization header) and gRPC (metadata) contexts.
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

        Ok(auth_value[7..].to_string())
    }

    /// Validate JWT token and return claims
    ///
    /// This is the core validation method used by both HTTP and gRPC validators.
    /// It verifies the token signature, expiration, and type.
    ///
    /// # Arguments
    /// * `token` - JWT token string
    ///
    /// # Returns
    /// Claims extracted from the token
    pub fn validate_token(&self, token: &str) -> Result<Claims> {
        self.jwt_service.verify_access_token(token)
    }

    /// Validate JWT token and return user ID
    ///
    /// Convenience method that extracts just the `user_id` from the token.
    ///
    /// # Arguments
    /// * `token` - JWT token string
    ///
    /// # Returns
    /// User ID extracted from the token
    pub fn validate_and_extract_user_id(&self, token: &str) -> Result<UserId> {
        let claims = self.validate_token(token)?;
        Ok(UserId::from_string(claims.sub))
    }
}

impl std::fmt::Debug for JwtValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtValidator").finish()
    }
}

/// HTTP-specific validation methods
impl JwtValidator {
    /// Validate JWT from HTTP Authorization header
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
    pub fn validate_http(&self, auth_header: &str) -> Result<Claims> {
        let token = Self::extract_bearer_token(auth_header)?;
        self.validate_token(&token)
    }

    /// Validate JWT from HTTP Authorization header and extract user ID
    ///
    /// Convenience method for HTTP middleware that only needs the `user_id`.
    ///
    /// # Arguments
    /// * `auth_header` - Authorization header value (e.g., "Bearer <token>")
    ///
    /// # Returns
    /// User ID extracted from the token
    pub fn validate_http_extract_user_id(&self, auth_header: &str) -> Result<UserId> {
        let claims = self.validate_http(auth_header)?;
        Ok(UserId::from_string(claims.sub))
    }
}

/// gRPC-specific validation methods
impl JwtValidator {
    /// Extract authorization token from gRPC metadata
    ///
    /// # Arguments
    /// * `metadata` - gRPC request metadata
    ///
    /// # Returns
    /// Extracted token string
    ///
    /// # Errors
    /// - Missing authorization header
    /// - Invalid header format
    fn extract_grpc_token(&self, metadata: &MetadataMap) -> Result<String> {
        let auth_header = metadata
            .get("authorization")
            .ok_or_else(|| Error::Authentication("Missing authorization header".to_string()))?
            .to_str()
            .map_err(|_| {
                Error::Authentication("Invalid authorization header format".to_string())
            })?;

        Self::extract_bearer_token(auth_header)
    }

    /// Validate JWT from gRPC metadata
    ///
    /// # Arguments
    /// * `metadata` - gRPC request metadata
    ///
    /// # Returns
    /// Claims extracted from the token
    ///
    /// # Errors
    /// - Missing authorization header
    /// - Invalid header format
    /// - Invalid token
    pub fn validate_grpc(&self, metadata: &MetadataMap) -> Result<Claims> {
        let token = self.extract_grpc_token(metadata)?;
        self.validate_token(&token)
    }

    /// Validate JWT from gRPC metadata and extract user ID
    ///
    /// Convenience method for gRPC interceptors that only need the `user_id`.
    ///
    /// # Arguments
    /// * `metadata` - gRPC request metadata
    ///
    /// # Returns
    /// User ID extracted from the token
    pub fn validate_grpc_extract_user_id(&self, metadata: &MetadataMap) -> Result<UserId> {
        let claims = self.validate_grpc(metadata)?;
        Ok(UserId::from_string(claims.sub))
    }

    /// Validate JWT from gRPC metadata and return as gRPC Status
    ///
    /// This method is specifically designed for gRPC interceptors,
    /// returning `tonic::Status` instead of `crate::Error`.
    ///
    /// # Arguments
    /// * `metadata` - gRPC request metadata
    ///
    /// # Returns
    /// Claims extracted from the token
    ///
    /// # Errors
    /// - `tonic::Status::unauthenticated` for any validation failure
    #[allow(clippy::result_large_err)]
    pub fn validate_grpc_as_status(
        &self,
        metadata: &MetadataMap,
    ) -> std::result::Result<Claims, Status> {
        let token = self
            .extract_grpc_token(metadata)
            .map_err(|e| Status::unauthenticated(format!("Token extraction failed: {e}")))?;

        self.jwt_service
            .verify_access_token(&token)
            .map_err(|e| Status::unauthenticated(format!("Token verification failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_jwt_service() -> Arc<JwtService> {
        use super::super::jwt::JwtService;
        Arc::new(
            JwtService::new("test-secret-for-validator-that-is-long-enough-1234567890").unwrap(),
        )
    }

    fn create_test_token(jwt_service: &JwtService, user_id: &str) -> String {
        use super::super::jwt::TokenType;
        let user_id = UserId::from_string(user_id.to_string());
        jwt_service
            .sign_token(&user_id, TokenType::Access, 0)
            .unwrap()
    }

    #[test]
    fn test_extract_bearer_token() {
        // Valid "Bearer " format
        let token = JwtValidator::extract_bearer_token("Bearer abc123").unwrap();
        assert_eq!(token, "abc123");

        // Valid "bearer " format
        let token = JwtValidator::extract_bearer_token("bearer def456").unwrap();
        assert_eq!(token, "def456");

        // Invalid format
        let result = JwtValidator::extract_bearer_token("Basic abc123");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_token() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, "user123");

        // Valid token
        let claims = validator.validate_token(&token).unwrap();
        assert_eq!(claims.sub, "user123");

        // Invalid token
        let result = validator.validate_token("invalid.token.here");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_http() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, "user456");

        // Valid HTTP authorization header
        let claims = validator.validate_http(&format!("Bearer {token}")).unwrap();
        assert_eq!(claims.sub, "user456");

        // Invalid format
        let result = validator.validate_http("Basic invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_http_extract_user_id() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, "user789");

        let user_id = validator
            .validate_http_extract_user_id(&format!("Bearer {token}"))
            .unwrap();
        assert_eq!(user_id.as_str(), "user789");
    }

    #[test]
    fn test_validate_grpc() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, "user999");

        // Valid gRPC metadata
        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", format!("Bearer {token}").parse().unwrap());

        let claims = validator.validate_grpc(&metadata).unwrap();
        assert_eq!(claims.sub, "user999");

        // Missing authorization
        let metadata = MetadataMap::new();
        let result = validator.validate_grpc(&metadata);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_grpc_extract_user_id() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, "user111");

        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", format!("Bearer {token}").parse().unwrap());

        let user_id = validator.validate_grpc_extract_user_id(&metadata).unwrap();
        assert_eq!(user_id.as_str(), "user111");
    }

    #[test]
    fn test_validate_grpc_as_status() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, "user222");

        // Valid token
        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", format!("Bearer {token}").parse().unwrap());

        let claims = validator.validate_grpc_as_status(&metadata).unwrap();
        assert_eq!(claims.sub, "user222");

        // Invalid token
        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", "Bearer invalid".parse().unwrap());

        let result = validator.validate_grpc_as_status(&metadata);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }
}
