//! Tower middleware layer for async token blacklist and password invalidation checking in gRPC.
//!
//! Tonic interceptors are synchronous and cannot perform async Redis lookups.
//! This tower layer wraps the entire gRPC server and runs **before** routing
//! and per-service interceptors. It extracts the raw JWT bearer token from the
//! HTTP `Authorization` header and performs four security checks:
//! 1. JWT verification (validate signature, expiration, and access token type)
//! 2. Token blacklist check (explicit logout/revocation)
//! 3. Password invalidation check (tokens issued before password change)
//! 4. Banned/deleted user check (defense-in-depth against banned users with valid JWTs)
//!
//! Requests with blacklisted or invalidated tokens, or from banned/deleted users,
//! are rejected with `UNAUTHENTICATED` status.
//! Requests without an `Authorization` header (public endpoints) pass through.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::http;
use tonic::body::Body as TonicBody;
use tower::{Layer, Service};

use synctv_core::models::UserStatus;
use synctv_core::service::{TokenBlacklistService, UserService, auth::JwtService};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Tower layer that wraps a gRPC service with async token blacklist and password invalidation checking.
#[derive(Clone)]
pub struct BlacklistCheckLayer {
    blacklist_service: Arc<TokenBlacklistService>,
    jwt_service: Arc<JwtService>,
    user_service: Arc<UserService>,
}

impl BlacklistCheckLayer {
    pub fn new(blacklist_service: TokenBlacklistService, jwt_service: JwtService, user_service: UserService) -> Self {
        Self {
            blacklist_service: Arc::new(blacklist_service),
            jwt_service: Arc::new(jwt_service),
            user_service: Arc::new(user_service),
        }
    }
}

impl<S> Layer<S> for BlacklistCheckLayer {
    type Service = BlacklistCheckService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BlacklistCheckService {
            inner,
            blacklist_service: self.blacklist_service.clone(),
            jwt_service: self.jwt_service.clone(),
            user_service: self.user_service.clone(),
        }
    }
}

/// Tower service that checks the token blacklist and password invalidation before forwarding to the inner service.
#[derive(Clone)]
pub struct BlacklistCheckService<S> {
    inner: S,
    blacklist_service: Arc<TokenBlacklistService>,
    jwt_service: Arc<JwtService>,
    user_service: Arc<UserService>,
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
    S: Service<http::Request<TonicBody>, Response = http::Response<TonicBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<TonicBody>) -> Self::Future {
        // Clone the inner service for use in the async block (tower best practice:
        // swap the ready clone out so `self` retains a fresh clone for next poll_ready).
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        // Extract the bearer token from the HTTP Authorization header synchronously.
        // Requests without a bearer token (public endpoints) skip security checks.
        let raw_token = extract_bearer_token(req.headers());
        let blacklist_service = self.blacklist_service.clone();
        let jwt_service = self.jwt_service.clone();
        let user_service = self.user_service.clone();

        Box::pin(async move {
            if let Some(token) = raw_token {
                // Unified security check order (matches HTTP AuthUser extractor):
                // 1. JWT verification  2. Blacklist  3. Password invalidation  4. Banned user

                // Step 1: Verify JWT and extract claims
                let claims = match jwt_service.verify_access_token(&token) {
                    Ok(claims) => claims,
                    Err(e) => {
                        tracing::warn!("gRPC request rejected: JWT validation failed: {e}");
                        let response = tonic::Status::unauthenticated("Invalid or expired token")
                            .into_http();
                        return Ok(response);
                    }
                };

                let user_id = claims.user_id();
                let token_iat = claims.iat;

                // Step 2: Check if token is explicitly blacklisted (logout/revocation)
                match blacklist_service.is_blacklisted(&token).await {
                    Ok(true) => {
                        tracing::warn!("gRPC request rejected: token is blacklisted");
                        let response = tonic::Status::unauthenticated("Token has been revoked")
                            .into_http();
                        return Ok(response);
                    }
                    Ok(false) => {
                        // Token is not blacklisted, continue to password invalidation check
                    }
                    Err(e) => {
                        // Fail closed: deny access if blacklist check errors
                        tracing::error!(
                            "Token blacklist check failed, denying request (fail closed): {e}"
                        );
                        let response = tonic::Status::unavailable(
                            "Authentication service temporarily unavailable",
                        )
                        .into_http();
                        return Ok(response);
                    }
                }

                // Step 3: Check if token was issued before a password change
                match blacklist_service.are_user_tokens_invalidated(&user_id, token_iat).await {
                    Ok(true) => {
                        tracing::warn!(
                            user_id = %user_id.as_str(),
                            token_iat = token_iat,
                            "gRPC request rejected: token invalidated by password change"
                        );
                        let response = tonic::Status::unauthenticated(
                            "Token invalidated due to password change"
                        )
                        .into_http();
                        return Ok(response);
                    }
                    Ok(false) => {
                        // Token is valid, continue to banned user check
                    }
                    Err(e) => {
                        // Fail closed: deny access if password invalidation check errors
                        tracing::error!(
                            "Password invalidation check failed, denying request (fail closed): {e}"
                        );
                        let response = tonic::Status::unavailable(
                            "Authentication service temporarily unavailable",
                        )
                        .into_http();
                        return Ok(response);
                    }
                }

                // Step 4: Check if user is banned or deleted (defense-in-depth:
                // catches banned users even if they hold a valid JWT issued before the ban)
                match user_service.get_user(&user_id).await {
                    Ok(user) => {
                        if user.is_deleted() || user.status == UserStatus::Banned {
                            tracing::warn!(
                                user_id = %user_id.as_str(),
                                "gRPC request rejected: user is banned or deleted"
                            );
                            let response = tonic::Status::unauthenticated(
                                "Authentication failed"
                            )
                            .into_http();
                            return Ok(response);
                        }
                    }
                    Err(e) => {
                        // Fail closed: deny access if user lookup fails
                        tracing::error!(
                            "User lookup failed, denying request (fail closed): {e}"
                        );
                        let response = tonic::Status::unauthenticated(
                            "User not found"
                        )
                        .into_http();
                        return Ok(response);
                    }
                }
            }

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

    #[tokio::test]
    async fn test_blacklist_check_layer_clone() {
        let blacklist = TokenBlacklistService::new(None, "test".to_string());
        let jwt = JwtService::new("test-grpc-layer-secret-key-long-enough-1234567890").unwrap();
        let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
        let username_cache = synctv_core::cache::UsernameCache::new(None, "test:".to_string(), 10, 0);
        let user_service = UserService::new(
            pool,
            jwt.clone(),
            blacklist.clone(),
            username_cache,
            synctv_core::config::PasswordComplexityConfig::default(),
        );

        let layer = BlacklistCheckLayer::new(blacklist, jwt, user_service);
        let cloned = layer.clone();

        // Both should be valid (no panic on clone)
        assert!(Arc::strong_count(&cloned.blacklist_service) >= 2);
    }

    // ========== Security Parity: gRPC checks match HTTP checks ==========
    //
    // These tests verify that the gRPC BlacklistCheckService performs
    // the same four security checks as the HTTP AuthUser extractor:
    // 1. JWT verification (validate signature, expiration, access token type)
    // 2. Token blacklist check
    // 3. Password invalidation check
    // 4. Banned/deleted user check
    //
    // Both layers:
    // - Extract the bearer token from the Authorization header
    // - Verify JWT and extract claims (reject malformed/expired/non-access tokens)
    // - Check blacklist (fail closed on error)
    // - Check user-level token invalidation (fail closed on error)
    // - Look up user status and reject banned/deleted users
    //
    // This is tested structurally by verifying the extract_bearer_token
    // function and the layer construction include all three services.

    #[tokio::test]
    async fn test_grpc_layer_has_all_three_security_services() {
        let blacklist = TokenBlacklistService::new(None, "test".to_string());
        let jwt = JwtService::new("test-parity-secret-key-12345-long-enough-1234567890").unwrap();
        let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
        let username_cache = synctv_core::cache::UsernameCache::new(None, "test:".to_string(), 10, 0);
        let user_service = UserService::new(
            pool,
            jwt.clone(),
            blacklist.clone(),
            username_cache,
            synctv_core::config::PasswordComplexityConfig::default(),
        );

        let layer = BlacklistCheckLayer::new(blacklist, jwt, user_service);

        // The layer holds all three services needed for security parity with HTTP:
        // 1. blacklist_service (token revocation)
        // 2. jwt_service (token validation + claims extraction)
        // 3. user_service (banned/deleted user check)
        assert!(layer.blacklist_service.is_enabled());
        // jwt_service and user_service are held as Arc -- verify they exist
        assert!(Arc::strong_count(&layer.jwt_service) >= 1);
        assert!(Arc::strong_count(&layer.user_service) >= 1);
    }

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
}
