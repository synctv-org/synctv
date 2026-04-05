//! Tower middleware layer for async JWT security checking in gRPC.
//!
//! Tonic interceptors are synchronous and cannot perform async database lookups.
//! This tower layer wraps the entire gRPC server and runs **before** routing
//! and per-service interceptors. It extracts the raw JWT bearer token from the
//! HTTP `Authorization` header and performs security checks:
//! 1. JWT verification (validate signature, expiration, and access token type)
//! 2. Password invalidation check (tokens issued before password change)
//! 3. Banned/deleted user check (defense-in-depth against banned users with valid JWTs)
//!
//! Requests with invalidated tokens or from banned/deleted users are rejected
//! with `UNAUTHENTICATED` status.
//! Requests without an `Authorization` header (public endpoints) pass through.
//!
//! # Layer Ordering Verification
//!
//! This layer injects a `SecurityCheckPassed` marker into request extensions after
//! security checks pass. The `AuthInterceptor` checks for this marker and fails
//! if it's missing, ensuring correct layer ordering at runtime.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::{body::Body as AxumBody, http};
use tower::{Layer, Service};

use super::interceptors::SecurityCheckPassed;
use synctv_core::{
    service::{
        auth::{AuthErrorCategory, JwtService},
        AuthenticatedToken, SecurityPipeline,
    },
    Error as CoreError,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Tower layer that wraps a gRPC service with async token blacklist and password invalidation checking.
#[derive(Clone)]
pub struct BlacklistCheckLayer {
    jwt_service: Arc<JwtService>,
    security_pipeline: Arc<SecurityPipeline>,
}

impl BlacklistCheckLayer {
    #[must_use]
    pub fn new(jwt_service: JwtService, security_pipeline: SecurityPipeline) -> Self {
        Self {
            jwt_service: Arc::new(jwt_service),
            security_pipeline: Arc::new(security_pipeline),
        }
    }
}

impl<S> Layer<S> for BlacklistCheckLayer {
    type Service = BlacklistCheckService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BlacklistCheckService {
            inner,
            jwt_service: self.jwt_service.clone(),
            security_pipeline: self.security_pipeline.clone(),
        }
    }
}

/// Tower service that checks the token blacklist and password invalidation before forwarding to the inner service.
#[derive(Clone)]
pub struct BlacklistCheckService<S> {
    inner: S,
    jwt_service: Arc<JwtService>,
    security_pipeline: Arc<SecurityPipeline>,
}

/// Extract a bearer token from the HTTP Authorization header value.
///
/// Returns:
/// - `Missing` when the header is absent
/// - `Present(token)` for `"Bearer <token>"` (case-insensitive prefix per RFC 7235)
/// - `Malformed` when the header exists but is not a valid Bearer token
#[derive(Debug, Clone, PartialEq, Eq)]
enum BearerTokenState {
    Missing,
    Present(String),
    NonBearer,
    Malformed,
}

fn extract_bearer_token(headers: &http::HeaderMap) -> BearerTokenState {
    let Some(auth_value) = headers.get(http::header::AUTHORIZATION) else {
        return BearerTokenState::Missing;
    };

    let Ok(auth_str) = auth_value.to_str() else {
        return BearerTokenState::Malformed;
    };

    let trimmed_start = auth_str.trim_start();
    if !trimmed_start
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer "))
    {
        return BearerTokenState::NonBearer;
    }

    match synctv_core::service::auth::JwtValidator::extract_bearer_token(trimmed_start.trim_end()) {
        Ok(token) => BearerTokenState::Present(token),
        Err(_) => BearerTokenState::Malformed,
    }
}

fn security_pipeline_error_status(err: &CoreError) -> tonic::Status {
    match SecurityPipeline::classify_auth_error(err) {
        AuthErrorCategory::Authentication => {
            tonic::Status::unauthenticated("Authentication failed")
        }
        AuthErrorCategory::Authorization => super::map_auth_authorization_error(err),
        AuthErrorCategory::Unavailable => {
            tonic::Status::unavailable("Authentication service unavailable")
        }
        AuthErrorCategory::Internal => tonic::Status::internal("Internal error"),
    }
}

impl<S> Service<http::Request<AxumBody>> for BlacklistCheckService<S>
where
    S: Service<http::Request<AxumBody>, Response = http::Response<AxumBody>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<AxumBody>) -> Self::Future {
        // Clone the inner service for use in the async block (tower best practice:
        // swap the ready clone out so `self` retains a fresh clone for next poll_ready).
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        // Extract the bearer token from the HTTP Authorization header synchronously.
        // Requests without a bearer token (public endpoints) skip security checks.
        let raw_token = extract_bearer_token(req.headers());
        let jwt_service = self.jwt_service.clone();
        let security_pipeline = self.security_pipeline.clone();

        Box::pin(async move {
            match raw_token {
                BearerTokenState::Missing => {}
                BearerTokenState::NonBearer => {}
                BearerTokenState::Malformed => {
                    tracing::warn!(
                        "gRPC request rejected: malformed bearer authorization metadata"
                    );
                    let response = tonic::Status::unauthenticated("Invalid authorization header")
                        .into_http::<tonic::body::Body>()
                        .map(AxumBody::new);
                    return Ok(response);
                }
                BearerTokenState::Present(token) => {
                    // Security check order (matches HTTP AuthUser extractor):
                    // 1. JWT verification  2. Password invalidation  3. Banned/deleted user

                    // Step 1: Verify JWT and extract claims
                    let claims = match jwt_service.verify_access_token(&token) {
                        Ok(claims) => claims,
                        Err(e) => {
                            tracing::warn!("gRPC request rejected: JWT validation failed: {e}");
                            let response =
                                tonic::Status::unauthenticated("Invalid or expired token")
                                    .into_http::<tonic::body::Body>()
                                    .map(AxumBody::new);
                            return Ok(response);
                        }
                    };

                    // Steps 2-3: Shared security pipeline (password invalidation, user status)
                    let authenticated_token: AuthenticatedToken =
                        match security_pipeline.check(&claims).await {
                            Ok(authenticated_token) => authenticated_token,
                            Err(e) => {
                                tracing::warn!("gRPC request rejected by security pipeline: {e}");
                                let response = security_pipeline_error_status(&e)
                                    .into_http::<tonic::body::Body>()
                                    .map(AxumBody::new);
                                return Ok(response);
                            }
                        };

                    // Preserve the authenticated identity so downstream gRPC
                    // interceptors and handlers can reuse it without re-running
                    // JWT verification or the security pipeline.
                    req.extensions_mut().insert(authenticated_token);
                }
            }

            // Inject SecurityCheckPassed marker into request extensions.
            // This signals to AuthInterceptor that security checks have passed.
            // The marker is always injected (even for public endpoints without auth)
            // so that AuthInterceptor can verify layer ordering is correct.
            req.extensions_mut().insert(SecurityCheckPassed);

            inner.call(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== extract_bearer_token Tests ==========

    #[test]
    fn test_extract_bearer_token_valid() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Bearer eyJhbGciOiJIUzI1NiJ9.test.sig"),
        );
        let token = extract_bearer_token(&headers);
        assert_eq!(
            token,
            BearerTokenState::Present("eyJhbGciOiJIUzI1NiJ9.test.sig".to_string())
        );
    }

    #[test]
    fn test_extract_bearer_token_lowercase_prefix() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("bearer my_jwt_token"),
        );
        let token = extract_bearer_token(&headers);
        assert_eq!(token, BearerTokenState::Present("my_jwt_token".to_string()));
    }

    #[test]
    fn test_extract_bearer_token_missing_header() {
        let headers = http::HeaderMap::new();
        assert_eq!(extract_bearer_token(&headers), BearerTokenState::Missing);
    }

    #[test]
    fn test_extract_bearer_token_non_bearer_scheme() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        assert_eq!(extract_bearer_token(&headers), BearerTokenState::NonBearer);
    }

    #[test]
    fn test_extract_bearer_token_empty_token() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Bearer "),
        );
        assert_eq!(extract_bearer_token(&headers), BearerTokenState::Malformed);
    }

    #[test]
    fn test_extract_bearer_token_whitespace_trimmed() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("  Bearer my_token  "),
        );
        let token = extract_bearer_token(&headers);
        assert_eq!(token, BearerTokenState::Present("my_token".to_string()));
    }

    #[test]
    fn test_extract_bearer_token_invalid_utf8_is_malformed() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_bytes(b"Bearer \xff").expect("header value"),
        );

        assert_eq!(extract_bearer_token(&headers), BearerTokenState::Malformed);
    }

    #[test]
    fn test_extract_bearer_token_whitespace_only_token_is_malformed() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Bearer    "),
        );

        assert_eq!(extract_bearer_token(&headers), BearerTokenState::Malformed);
    }

    // ========== BlacklistCheckLayer Construction ==========
    //
    // Note: UserService now requires a Redis ConnectionManager, so structural
    // tests that just verify clone/Arc counts need a real Redis connection.
    // These tests are covered by integration tests with TestInfra instead.

    #[test]
    fn test_grpc_extract_bearer_matches_http_pattern() {
        // Verify that the gRPC bearer extraction uses the same pattern
        // as the HTTP middleware (case-insensitive "Bearer " prefix).
        let mut headers = http::HeaderMap::new();

        // Standard case
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Bearer token123"),
        );
        assert_eq!(
            extract_bearer_token(&headers),
            BearerTokenState::Present("token123".to_string())
        );

        // Lowercase (both HTTP and gRPC should accept)
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("bearer token456"),
        );
        assert_eq!(
            extract_bearer_token(&headers),
            BearerTokenState::Present("token456".to_string())
        );

        // No auth header (public endpoint -- both layers should pass through)
        let empty_headers = http::HeaderMap::new();
        assert_eq!(
            extract_bearer_token(&empty_headers),
            BearerTokenState::Missing
        );
    }

    // ========== SecurityCheckPassed Marker Tests ==========

    #[test]
    fn test_security_check_passed_marker_type_available() {
        // Verify that the SecurityCheckPassed marker type is accessible
        // from the interceptors module
        use crate::grpc::interceptors::SecurityCheckPassed;
        let marker = SecurityCheckPassed;
        // Just verify we can create and use it
        assert!(format!("{marker:?}").contains("SecurityCheckPassed"));
    }

    #[test]
    fn test_http_extensions_can_hold_security_marker() {
        // Verify that http::Extensions can hold the SecurityCheckPassed marker
        use crate::grpc::interceptors::SecurityCheckPassed;
        let mut extensions = http::Extensions::new();
        extensions.insert(SecurityCheckPassed);
        assert!(
            extensions.get::<SecurityCheckPassed>().is_some(),
            "Extensions should contain SecurityCheckPassed"
        );
    }

    #[test]
    fn test_security_pipeline_error_status_preserves_authorization_message() {
        let status = security_pipeline_error_status(&CoreError::Authorization(
            "Not a member of this room".to_string(),
        ));

        assert_eq!(status.code(), tonic::Code::PermissionDenied);
        assert_eq!(status.message(), "Not a member of this room");
    }

    #[test]
    fn test_security_pipeline_error_status_maps_email_not_verified() {
        let status = security_pipeline_error_status(&CoreError::EmailNotVerified);

        assert_eq!(status.code(), tonic::Code::PermissionDenied);
        assert_eq!(
            status.message(),
            "Email not verified. Please verify your email to continue."
        );
    }
}
