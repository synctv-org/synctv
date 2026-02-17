//! Async tower middleware layer for distributed gRPC rate limiting.
//!
//! Replaces the synchronous `GrpcRateLimitInterceptor` with an async tower layer
//! that uses the distributed Redis-backed rate limiter (`RateLimiter::check_rate_limit`).
//! This ensures rate limits are shared across all replicas instead of being per-instance.
//!
//! Falls back to in-memory governor rate limiting when Redis is unavailable,
//! matching the behavior of the HTTP rate limiting middleware.
//!
//! The layer is applied at the server level and determines the appropriate rate limit
//! tier from the gRPC request path (service name), so different services get different
//! rate limits without needing per-service wrapping.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::http;
use sha2::{Digest, Sha256};
use tonic::body::Body as TonicBody;
use tower::{Layer, Service};
use tracing::warn;

use super::interceptors::GrpcRateLimitTier;
use synctv_core::service::RateLimiter;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Tower layer that wraps the gRPC server with async distributed rate limiting.
///
/// Applied at the server level via `Server::builder().layer(rate_limit_layer)`.
/// Determines the rate limit tier from the gRPC request path (service name).
///
/// Uses `RateLimiter::check_rate_limit` which checks Redis first (distributed
/// across replicas) and falls back to in-memory governor if Redis is unavailable.
#[derive(Clone)]
pub struct GrpcRateLimitLayer {
    rate_limiter: Arc<RateLimiter>,
    window_seconds: u64,
}

impl GrpcRateLimitLayer {
    /// Create a new distributed rate limit layer.
    ///
    /// The tier is determined per-request from the gRPC service path.
    pub fn new(rate_limiter: RateLimiter, window_seconds: u64) -> Self {
        Self {
            rate_limiter: Arc::new(rate_limiter),
            window_seconds,
        }
    }
}

impl<S> Layer<S> for GrpcRateLimitLayer {
    type Service = GrpcRateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcRateLimitService {
            inner,
            rate_limiter: self.rate_limiter.clone(),
            window_seconds: self.window_seconds,
        }
    }
}

/// Tower service that applies async distributed rate limiting before forwarding
/// to the inner service.
#[derive(Clone)]
pub struct GrpcRateLimitService<S> {
    inner: S,
    rate_limiter: Arc<RateLimiter>,
    window_seconds: u64,
}

/// Determine the rate limit tier from the gRPC request path.
///
/// gRPC paths follow the format `/<package>.<ServiceName>/<MethodName>`.
/// Maps known service names to their corresponding rate limit tiers.
fn tier_from_path(path: &str) -> Option<GrpcRateLimitTier> {
    // gRPC path format: /synctv.client.AuthService/Login
    // Extract the service portion after the last dot and before the slash
    let service_name = path
        .split('/')
        .nth(1) // "synctv.client.AuthService"
        .and_then(|full| full.rsplit('.').next()); // "AuthService"

    match service_name {
        Some("AuthService") => Some(GrpcRateLimitTier::Auth),
        Some("EmailService") => Some(GrpcRateLimitTier::Email),
        Some("MediaService") => Some(GrpcRateLimitTier::Media),
        Some("UserService") => Some(GrpcRateLimitTier::Write),
        Some("RoomService") => Some(GrpcRateLimitTier::Write),
        Some("AdminService") => Some(GrpcRateLimitTier::Admin),
        Some("PublicService") => Some(GrpcRateLimitTier::Read),
        Some("NotificationService") => Some(GrpcRateLimitTier::Read),
        Some("OAuth2Service") => Some(GrpcRateLimitTier::Auth),
        // Provider services
        Some("AlistProviderService") => Some(GrpcRateLimitTier::Read),
        Some("BilibiliProviderService") => Some(GrpcRateLimitTier::Read),
        Some("EmbyProviderService") => Some(GrpcRateLimitTier::Read),
        // Cluster service uses its own auth; no rate limiting needed
        Some("ClusterService") => None,
        // Unknown services: no rate limiting (they may have their own auth)
        _ => None,
    }
}

/// Extract a stable client identifier from HTTP headers.
///
/// Priority:
/// 1. SHA-256 hash of JWT bearer token (authenticated users)
/// 2. Client IP from X-Forwarded-For or X-Real-IP headers
/// 3. "anon:unknown" fallback (only if no IP info available)
fn extract_client_id(headers: &http::HeaderMap) -> String {
    // Try authenticated user first
    if let Some(id) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            if s.len() > 7 && s[..7].eq_ignore_ascii_case("Bearer ") {
                let token = &s[7..];
                let hash = Sha256::digest(token.as_bytes());
                Some(format!("user:{:x}", hash))
            } else {
                None
            }
        })
    {
        return id;
    }

    // For anonymous requests, use client IP to avoid sharing a single bucket
    if let Some(ip) = headers
        .get("X-Forwarded-For")
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
    {
        return format!("anon:{ip}");
    }
    if let Some(ip) = headers
        .get("X-Real-IP")
        .and_then(|h| h.to_str().ok())
    {
        return format!("anon:{ip}");
    }

    // No client identifier available (no auth, no IP headers).
    // This typically means the deployment is misconfigured (missing trusted proxy
    // headers) or a direct connection without a reverse proxy. Log a warning
    // so operators notice the misconfiguration, and use a shared "unknown" bucket
    // with a tighter effective limit (the shared bucket naturally applies pressure).
    warn!(
        "Rate limit: no client identifier available (no Authorization, X-Forwarded-For, or X-Real-IP). \
         Configure trusted_proxies and ensure your reverse proxy sets X-Forwarded-For."
    );
    "anon:unknown".to_string()
}

impl<S> Service<http::Request<TonicBody>> for GrpcRateLimitService<S>
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

    fn call(&mut self, req: http::Request<TonicBody>) -> Self::Future {
        // Clone the inner service (tower best practice: swap ready clone out)
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        let rate_limiter = self.rate_limiter.clone();
        let window_seconds = self.window_seconds;

        // Determine tier from the request path
        let path = req.uri().path().to_string();
        let tier = tier_from_path(&path);

        // If no tier matches (cluster service, unknown paths), skip rate limiting
        let Some(tier) = tier else {
            return Box::pin(async move { inner.call(req).await });
        };

        let client_id = extract_client_id(req.headers());

        Box::pin(async move {
            let rate_key = format!("grpc:{}:{}", client_id, tier.key_suffix());

            if let Err(_e) = rate_limiter
                .check_rate_limit(&rate_key, tier.max_requests(), window_seconds)
                .await
            {
                warn!(
                    client_id = %client_id,
                    tier = ?tier,
                    max_requests = tier.max_requests(),
                    path = %path,
                    "gRPC distributed rate limit exceeded"
                );
                let response = tonic::Status::resource_exhausted(
                    "Rate limit exceeded. Please retry later.",
                )
                .into_http();
                return Ok(response);
            }

            inner.call(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_from_path_auth() {
        assert_eq!(
            tier_from_path("/synctv.client.AuthService/Login"),
            Some(GrpcRateLimitTier::Auth)
        );
    }

    #[test]
    fn test_tier_from_path_email() {
        assert_eq!(
            tier_from_path("/synctv.client.EmailService/SendVerification"),
            Some(GrpcRateLimitTier::Email)
        );
    }

    #[test]
    fn test_tier_from_path_media() {
        assert_eq!(
            tier_from_path("/synctv.client.MediaService/AddMedia"),
            Some(GrpcRateLimitTier::Media)
        );
    }

    #[test]
    fn test_tier_from_path_user() {
        assert_eq!(
            tier_from_path("/synctv.client.UserService/GetUser"),
            Some(GrpcRateLimitTier::Write)
        );
    }

    #[test]
    fn test_tier_from_path_room() {
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/CreateRoom"),
            Some(GrpcRateLimitTier::Write)
        );
    }

    #[test]
    fn test_tier_from_path_admin() {
        assert_eq!(
            tier_from_path("/synctv.admin.AdminService/ListUsers"),
            Some(GrpcRateLimitTier::Admin)
        );
    }

    #[test]
    fn test_tier_from_path_public() {
        assert_eq!(
            tier_from_path("/synctv.client.PublicService/ListRooms"),
            Some(GrpcRateLimitTier::Read)
        );
    }

    #[test]
    fn test_tier_from_path_notification() {
        assert_eq!(
            tier_from_path("/synctv.client.NotificationService/GetNotifications"),
            Some(GrpcRateLimitTier::Read)
        );
    }

    #[test]
    fn test_tier_from_path_oauth2() {
        assert_eq!(
            tier_from_path("/synctv.client.OAuth2Service/GetAuthUrl"),
            Some(GrpcRateLimitTier::Auth)
        );
    }

    #[test]
    fn test_tier_from_path_providers() {
        assert_eq!(
            tier_from_path("/synctv.providers.alist.AlistProviderService/ListFiles"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.bilibili.BilibiliProviderService/Search"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.emby.EmbyProviderService/GetItem"),
            Some(GrpcRateLimitTier::Read)
        );
    }

    #[test]
    fn test_tier_from_path_cluster_no_rate_limit() {
        assert_eq!(
            tier_from_path("/synctv.cluster.ClusterService/Heartbeat"),
            None
        );
    }

    #[test]
    fn test_tier_from_path_unknown() {
        assert_eq!(tier_from_path("/unknown.UnknownService/Method"), None);
    }

    #[test]
    fn test_tier_from_path_empty() {
        assert_eq!(tier_from_path(""), None);
    }

    #[test]
    fn test_extract_client_id_bearer() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Bearer my_token_here".parse().unwrap(),
        );
        let id = extract_client_id(&headers);
        assert!(id.starts_with("user:"));
        assert!(id.len() > 10); // SHA-256 hex is 64 chars
    }

    #[test]
    fn test_extract_client_id_no_auth() {
        let headers = http::HeaderMap::new();
        assert_eq!(extract_client_id(&headers), "anon:unknown");
    }

    #[test]
    fn test_extract_client_id_basic_auth() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Basic dXNlcjpwYXNz".parse().unwrap(),
        );
        // Basic auth with no IP headers falls back to anon:unknown
        assert_eq!(extract_client_id(&headers), "anon:unknown");
    }

    #[test]
    fn test_extract_client_id_x_forwarded_for() {
        let mut headers = http::HeaderMap::new();
        headers.insert("X-Forwarded-For", "203.0.113.50, 70.41.3.18".parse().unwrap());
        let id = extract_client_id(&headers);
        assert_eq!(id, "anon:203.0.113.50");
    }

    #[test]
    fn test_extract_client_id_x_real_ip() {
        let mut headers = http::HeaderMap::new();
        headers.insert("X-Real-IP", "198.51.100.42".parse().unwrap());
        let id = extract_client_id(&headers);
        assert_eq!(id, "anon:198.51.100.42");
    }

    #[test]
    fn test_extract_client_id_bearer_takes_priority_over_ip() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Bearer my_token_here".parse().unwrap(),
        );
        headers.insert("X-Forwarded-For", "203.0.113.50".parse().unwrap());
        let id = extract_client_id(&headers);
        assert!(id.starts_with("user:"), "Bearer token should take priority over IP");
    }
}
