//! Unified JWT validation for HTTP and gRPC.
//!
//! This module provides a single source of truth for JWT validation across
//! both transports, eliminating code duplication and ensuring consistent
//! authentication behavior.

use super::{jwt::JwtService, Claims};
use crate::{models::UserId, Error, Result};
use std::sync::Arc;
use tonic::{metadata::MetadataMap, Status};

pub type GrpcStatusResult<T> = std::result::Result<T, Box<Status>>;

/// Unified JWT validator for HTTP and gRPC authentication.
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
        claims.user_id()
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
    /// Convenience method for HTTP call sites that only need the `user_id`.
    ///
    /// # Arguments
    /// * `auth_header` - Authorization header value (e.g., "Bearer <token>")
    ///
    /// # Returns
    /// User ID extracted from the token
    pub fn validate_http_extract_user_id(&self, auth_header: &str) -> Result<UserId> {
        let claims = self.validate_http(auth_header)?;
        claims.user_id()
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
    fn extract_grpc_token(metadata: &MetadataMap) -> Result<String> {
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
        let token = Self::extract_grpc_token(metadata)?;
        self.validate_token(&token)
    }

    /// Validate JWT from gRPC metadata and extract user ID
    ///
    /// Convenience method for gRPC call sites that only need the `user_id`.
    ///
    /// # Arguments
    /// * `metadata` - gRPC request metadata
    ///
    /// # Returns
    /// User ID extracted from the token
    pub fn validate_grpc_extract_user_id(&self, metadata: &MetadataMap) -> Result<UserId> {
        let claims = self.validate_grpc(metadata)?;
        claims.user_id()
    }

    /// Validate JWT from gRPC metadata and return as gRPC Status
    ///
    /// This method is specifically designed for gRPC-facing call sites,
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
    pub fn validate_grpc_as_status(&self, metadata: &MetadataMap) -> GrpcStatusResult<Claims> {
        let token = Self::extract_grpc_token(metadata).map_err(|error| {
            tracing::warn!(error = %error, "gRPC token extraction failed");
            Box::new(Status::unauthenticated("Invalid authorization header"))
        })?;

        self.jwt_service
            .verify_access_token(&token)
            .map_err(|error| {
                tracing::warn!(error = %error, "gRPC token verification failed");
                Box::new(Status::unauthenticated(
                    synctv_common::messages::INVALID_OR_EXPIRED_TOKEN,
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => std::panic::panic_any(context.to_string()),
            Err(error) => error,
        }
    }

    fn create_test_jwt_service() -> Arc<JwtService> {
        use super::super::jwt::JwtService;
        Arc::new(ok(
            JwtService::new("test-secret-for-validator-that-is-long-enough-1234567890"),
            "JWT service should build",
        ))
    }

    fn auth_header(
        value: impl Into<String>,
    ) -> tonic::metadata::MetadataValue<tonic::metadata::Ascii> {
        ok(
            value.into().parse(),
            "authorization metadata value should parse",
        )
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
        assert_eq!(claims.sub, "98001");

        let result = validator.validate_token("invalid.token.here");
        assert!(matches!(result, Err(Error::Authentication(_))));
    }

    #[test]
    fn test_validate_http() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, 98_002);

        let claims = ok(
            validator.validate_http(&format!("Bearer {token}")),
            "HTTP bearer token should validate",
        );
        assert_eq!(claims.sub, "98002");

        let result = validator.validate_http("Basic invalid");
        assert!(matches!(result, Err(Error::Authentication(_))));
    }

    #[test]
    fn test_validate_http_extract_user_id() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, 98_003);

        let user_id = validator.validate_http_extract_user_id(&format!("Bearer {token}"));
        let user_id = ok(user_id, "HTTP bearer token user ID should extract");
        assert_eq!(user_id.to_string(), "98003");
    }

    #[test]
    fn test_validate_grpc() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, 98_004);

        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", auth_header(format!("Bearer {token}")));

        let claims = ok(
            validator.validate_grpc(&metadata),
            "gRPC token should validate",
        );
        assert_eq!(claims.sub, "98004");

        let metadata = MetadataMap::new();
        let result = validator.validate_grpc(&metadata);
        assert!(matches!(result, Err(Error::Authentication(_))));
    }

    #[test]
    fn test_validate_grpc_extract_user_id() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, 98_005);

        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", auth_header(format!("Bearer {token}")));

        let user_id = ok(
            validator.validate_grpc_extract_user_id(&metadata),
            "gRPC token user ID should extract",
        );
        assert_eq!(user_id.to_string(), "98005");
    }

    #[test]
    fn test_validate_grpc_as_status() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service.clone());

        let token = create_test_token(&jwt_service, 98_006);

        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", auth_header(format!("Bearer {token}")));

        let claims = ok(
            validator.validate_grpc_as_status(&metadata),
            "gRPC status validator should accept token",
        );
        assert_eq!(claims.sub, "98006");

        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", auth_header("Bearer invalid"));

        assert_eq!(
            err(
                validator.validate_grpc_as_status(&metadata),
                "invalid token should be rejected"
            )
            .code(),
            tonic::Code::Unauthenticated
        );
    }

    #[test]
    fn test_validate_grpc_as_status_hides_verification_details() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service);

        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", auth_header("Bearer invalid"));

        let status = err(
            validator.validate_grpc_as_status(&metadata),
            "invalid token should be rejected",
        );

        assert_eq!(status.code(), tonic::Code::Unauthenticated);
        assert_eq!(
            status.message(),
            synctv_common::messages::INVALID_OR_EXPIRED_TOKEN
        );
    }

    #[test]
    fn test_validate_grpc_as_status_hides_extraction_details() {
        let jwt_service = create_test_jwt_service();
        let validator = JwtValidator::new(jwt_service);

        let metadata = MetadataMap::new();

        let status = err(
            validator.validate_grpc_as_status(&metadata),
            "missing authorization header should be rejected",
        );

        assert_eq!(status.code(), tonic::Code::Unauthenticated);
        assert_eq!(status.message(), "Invalid authorization header");
    }
}
