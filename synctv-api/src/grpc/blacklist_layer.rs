//! Tower middleware layer for async token blacklist and password invalidation checking in gRPC.
//!
//! Tonic interceptors are synchronous and cannot perform async Redis lookups.
//! This tower layer wraps the entire gRPC server and runs **before** routing
//! and per-service interceptors. It extracts the raw JWT bearer token from the
//! HTTP `Authorization` header and performs two security checks:
//! 1. Token blacklist check (explicit logout/revocation)
//! 2. Password invalidation check (tokens issued before password change)
//!
//! Requests with blacklisted or invalidated tokens are rejected with `UNAUTHENTICATED` status.
//! Requests without an `Authorization` header (public endpoints) pass through.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::http;
use tonic::body::Body as TonicBody;
use tower::{Layer, Service};

use synctv_core::service::{TokenBlacklistService, auth::JwtService};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Tower layer that wraps a gRPC service with async token blacklist and password invalidation checking.
#[derive(Clone)]
pub struct BlacklistCheckLayer {
    blacklist_service: Arc<TokenBlacklistService>,
    jwt_service: Arc<JwtService>,
}

impl BlacklistCheckLayer {
    pub fn new(blacklist_service: TokenBlacklistService, jwt_service: JwtService) -> Self {
        Self {
            blacklist_service: Arc::new(blacklist_service),
            jwt_service: Arc::new(jwt_service),
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
        }
    }
}

/// Tower service that checks the token blacklist and password invalidation before forwarding to the inner service.
#[derive(Clone)]
pub struct BlacklistCheckService<S> {
    inner: S,
    blacklist_service: Arc<TokenBlacklistService>,
    jwt_service: Arc<JwtService>,
}

/// Extract a bearer token from the HTTP Authorization header value.
///
/// Returns `Some(token)` for `"Bearer <token>"` (case-insensitive prefix),
/// or `None` if the header is absent, not a bearer token, or malformed.
fn extract_bearer_token(headers: &http::HeaderMap) -> Option<String> {
    let auth_value = headers.get(http::header::AUTHORIZATION)?;
    let auth_str = auth_value.to_str().ok()?;
    let trimmed = auth_str.trim();
    if trimmed.len() > 7
        && (trimmed.starts_with("Bearer ") || trimmed.starts_with("bearer "))
    {
        Some(trimmed[7..].to_string())
    } else {
        None
    }
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

        Box::pin(async move {
            if let Some(token) = raw_token {
                // Step 1: Check if token is explicitly blacklisted (logout/revocation)
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

                // Step 2: Check if token was issued before a password change
                // Parse JWT to extract user_id and iat (issued-at timestamp)
                match jwt_service.verify_token(&token) {
                    Ok(claims) => {
                        let user_id = claims.user_id();
                        let token_iat = claims.iat;

                        // Check if all user tokens were invalidated due to password change
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
                                // Token is valid, proceed to inner service
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
                    }
                    Err(e) => {
                        // JWT validation failed - this is a malformed or expired token
                        tracing::warn!("gRPC request rejected: JWT validation failed: {e}");
                        let response = tonic::Status::unauthenticated("Invalid or expired token")
                            .into_http();
                        return Ok(response);
                    }
                }
            }

            inner.call(req).await
        })
    }
}
