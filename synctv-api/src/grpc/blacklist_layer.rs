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

use axum::http;
use tonic::body::Body as TonicBody;
use tower::{Layer, Service};

use super::interceptors::SecurityCheckPassed;
use synctv_core::service::{auth::JwtService, SecurityPipeline};

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
/// Returns `Some(token)` for `"Bearer <token>"` (case-insensitive prefix per RFC 7235),
/// or `None` if the header is absent, not a bearer token, or malformed.
fn extract_bearer_token(headers: &http::HeaderMap) -> Option<String> {
    let auth_value = headers.get(http::header::AUTHORIZATION)?;
    let auth_str = auth_value.to_str().ok()?;
    let trimmed = auth_str.trim();
    synctv_core::service::auth::JwtValidator::extract_bearer_token(trimmed).ok()
}

impl<S> Service<http::Request<TonicBody>> for BlacklistCheckService<S>
where
    S: Service<http::Request<TonicBody>, Response = http::Response<TonicBody>>
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

    fn call(&mut self, mut req: http::Request<TonicBody>) -> Self::Future {
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
            if let Some(token) = raw_token {
                // Security check order (matches HTTP AuthUser extractor):
                // 1. JWT verification  2. Password invalidation  3. Banned/deleted user

                // Step 1: Verify JWT and extract claims
                let claims = match jwt_service.verify_access_token(&token) {
                    Ok(claims) => claims,
                    Err(e) => {
                        tracing::warn!("gRPC request rejected: JWT validation failed: {e}");
                        let response =
                            tonic::Status::unauthenticated("Invalid or expired token").into_http();
                        return Ok(response);
                    }
                };

                // Steps 2-3: Shared security pipeline (password invalidation, user status)
                if let Err(e) = security_pipeline.check(&claims).await {
                    tracing::warn!("gRPC request rejected by security pipeline: {e}");
                    let response = tonic::Status::unauthenticated(format!("{e}")).into_http();
                    return Ok(response);
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
        assert_eq!(token.as_deref(), Some("eyJhbGciOiJIUzI1NiJ9.test.sig"));
    }

    #[test]
    fn test_extract_bearer_token_lowercase_prefix() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("bearer my_jwt_token"),
        );
        let token = extract_bearer_token(&headers);
        assert_eq!(token.as_deref(), Some("my_jwt_token"));
    }

    #[test]
    fn test_extract_bearer_token_missing_header() {
        let headers = http::HeaderMap::new();
        assert!(extract_bearer_token(&headers).is_none());
    }

    #[test]
    fn test_extract_bearer_token_non_bearer_scheme() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        assert!(extract_bearer_token(&headers).is_none());
    }

    #[test]
    fn test_extract_bearer_token_empty_token() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Bearer "),
        );
        // "Bearer " has length 7, and the check is `trimmed.len() > 7`
        // so "Bearer " (len=7) should return None (no actual token content)
        assert!(extract_bearer_token(&headers).is_none());
    }

    #[test]
    fn test_extract_bearer_token_whitespace_trimmed() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("  Bearer my_token  "),
        );
        let token = extract_bearer_token(&headers);
        // The function trims the entire auth string, so "  Bearer my_token  "
        // becomes "Bearer my_token" -> extracts "my_token"
        assert_eq!(token.as_deref(), Some("my_token"));
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
        assert_eq!(extract_bearer_token(&headers).as_deref(), Some("token123"));

        // Lowercase (both HTTP and gRPC should accept)
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("bearer token456"),
        );
        assert_eq!(extract_bearer_token(&headers).as_deref(), Some("token456"));

        // No auth header (public endpoint -- both layers should pass through)
        let empty_headers = http::HeaderMap::new();
        assert!(extract_bearer_token(&empty_headers).is_none());
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
}
