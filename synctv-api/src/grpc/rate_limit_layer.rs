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
use synctv_core::Config;
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
    config: Arc<Config>,
}

impl GrpcRateLimitLayer {
    /// Create a new distributed rate limit layer.
    ///
    /// The tier is determined per-request from the gRPC service path.
    /// The `config` is used for trusted-proxy validation when extracting client IPs.
    pub fn new(rate_limiter: RateLimiter, window_seconds: u64, config: Arc<Config>) -> Self {
        Self {
            rate_limiter: Arc::new(rate_limiter),
            window_seconds,
            config,
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
            config: self.config.clone(),
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
    config: Arc<Config>,
}

/// Extract service and method labels from a gRPC path for metrics.
///
/// gRPC paths follow the format `/<package>.<ServiceName>/<MethodName>`.
/// Returns `(service_name, method_name)` for prometheus labels.
fn extract_grpc_labels(path: &str) -> (String, String) {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() >= 3 {
        let service = parts[1].rsplit('.').next().unwrap_or(parts[1]);
        (service.to_string(), parts[2].to_string())
    } else {
        ("unknown".to_string(), "unknown".to_string())
    }
}

/// Extract the gRPC status code from an HTTP response.
///
/// gRPC uses the `grpc-status` trailer/header. If not present, infer from
/// HTTP status code.
fn grpc_status_from_response(resp: &http::Response<TonicBody>) -> &'static str {
    if let Some(status_header) = resp.headers().get("grpc-status") {
        match status_header.to_str().unwrap_or("") {
            "0" => "ok",
            "1" => "cancelled",
            "2" => "unknown",
            "3" => "invalid_argument",
            "4" => "deadline_exceeded",
            "5" => "not_found",
            "6" => "already_exists",
            "7" => "permission_denied",
            "8" => "resource_exhausted",
            "13" => "internal",
            "14" => "unavailable",
            "16" => "unauthenticated",
            _ => "unknown",
        }
    } else if resp.status().is_success() {
        "ok"
    } else {
        "error"
    }
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
/// 2. Client IP from X-Forwarded-For or X-Real-IP headers (only if from a trusted proxy)
/// 3. Remote socket address (direct connection)
/// 4. "anon:unknown" fallback (only if no IP info available)
///
/// X-Forwarded-For and X-Real-IP headers are only trusted when the request comes
/// from a configured trusted proxy or when development mode is enabled, matching
/// the HTTP middleware pattern.
fn extract_client_id(headers: &http::HeaderMap, config: &Config) -> String {
    // Try authenticated user first - delegate to unified bearer token extraction
    if let Some(id) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            synctv_core::service::auth::JwtValidator::extract_bearer_token(s)
                .ok()
                .map(|token| {
                    let hash = Sha256::digest(token.as_bytes());
                    format!("user:{:x}", hash)
                })
        })
    {
        return id;
    }

    // For anonymous requests, extract remote address from tonic's socket peer.
    // Tonic injects the remote address as a header extension during HTTP/2 transport.
    let remote_addr = headers
        .get("x-real-ip-internal")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<std::net::IpAddr>().ok());

    // Only trust X-Forwarded-For/X-Real-IP when from a trusted proxy or in dev mode
    let should_trust_headers = remote_addr.is_some_and(|ip| config.server.is_trusted_proxy(&ip));

    if should_trust_headers {
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
    }

    // Use the direct socket address when headers are not trusted
    if let Some(ip) = remote_addr {
        return format!("anon:{ip}");
    }

    // No client identifier available (no auth, no IP headers, no socket address).
    warn!(
        "Rate limit: no client identifier available (no Authorization, no trusted proxy headers, no socket address). \
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
        let config = self.config.clone();

        // Determine tier from the request path
        let path = req.uri().path().to_string();
        let tier = tier_from_path(&path);

        // Extract service and method names for metrics labels
        let (service_label, method_label) = extract_grpc_labels(&path);

        // If no tier matches (cluster service, unknown paths), skip rate limiting
        // but still record metrics
        let Some(tier) = tier else {
            return Box::pin(async move {
                let result = inner.call(req).await;
                let status = match &result {
                    Ok(resp) => grpc_status_from_response(resp),
                    Err(_) => "error",
                };
                synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
                    .with_label_values(&[&service_label, &method_label, status])
                    .inc();
                result
            });
        };

        let client_id = extract_client_id(req.headers(), &config);

        Box::pin(async move {
            // Use the same key format as HTTP middleware ("ratelimit:{category}:{client_id}")
            // so that rate limits are shared across HTTP and gRPC transports.
            let rate_key = format!("ratelimit:{}:{}", tier.key_suffix(), client_id);

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
                synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
                    .with_label_values(&[&service_label, &method_label, "resource_exhausted"])
                    .inc();
                let response = tonic::Status::resource_exhausted(
                    "Rate limit exceeded. Please retry later.",
                )
                .into_http();
                return Ok(response);
            }

            let result = inner.call(req).await;
            let status = match &result {
                Ok(resp) => grpc_status_from_response(resp),
                Err(_) => "error",
            };
            synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
                .with_label_values(&[&service_label, &method_label, status])
                .inc();
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_grpc_labels() {
        let (service, method) = extract_grpc_labels("/synctv.client.AuthService/Login");
        assert_eq!(service, "AuthService");
        assert_eq!(method, "Login");
    }

    #[test]
    fn test_extract_grpc_labels_unknown_path() {
        let (service, method) = extract_grpc_labels("/");
        assert_eq!(service, "unknown");
        assert_eq!(method, "unknown");
    }

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

    /// Create a default config for tests (no trusted proxies, dev mode off)
    fn test_config() -> Config {
        Config::default()
    }

    /// Create a config with trusted proxies (trusts proxy headers from 127.0.0.1)
    fn trusted_proxy_config() -> Config {
        let mut config = Config::default();
        config.server.trusted_proxies = vec!["127.0.0.1".to_string()];
        config
    }

    #[test]
    fn test_extract_client_id_bearer() {
        let config = test_config();
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Bearer my_token_here".parse().unwrap(),
        );
        let id = extract_client_id(&headers, &config);
        assert!(id.starts_with("user:"));
        assert!(id.len() > 10); // SHA-256 hex is 64 chars
    }

    #[test]
    fn test_extract_client_id_no_auth() {
        let config = test_config();
        let headers = http::HeaderMap::new();
        assert_eq!(extract_client_id(&headers, &config), "anon:unknown");
    }

    #[test]
    fn test_extract_client_id_basic_auth() {
        let config = test_config();
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Basic dXNlcjpwYXNz".parse().unwrap(),
        );
        // Basic auth with no IP headers falls back to anon:unknown
        assert_eq!(extract_client_id(&headers, &config), "anon:unknown");
    }

    #[test]
    fn test_extract_client_id_x_forwarded_for_untrusted() {
        // Without trusted proxies or dev mode, X-Forwarded-For should NOT be trusted
        let config = test_config();
        let mut headers = http::HeaderMap::new();
        headers.insert("X-Forwarded-For", "203.0.113.50, 70.41.3.18".parse().unwrap());
        let id = extract_client_id(&headers, &config);
        // Should fall through to anon:unknown since headers are not trusted
        assert_eq!(id, "anon:unknown");
    }

    #[test]
    fn test_extract_client_id_x_forwarded_for_trusted_proxy() {
        // With trusted proxies and x-real-ip-internal header, X-Forwarded-For is trusted
        let config = trusted_proxy_config();
        let mut headers = http::HeaderMap::new();
        headers.insert("X-Forwarded-For", "203.0.113.50, 70.41.3.18".parse().unwrap());
        headers.insert("x-real-ip-internal", "127.0.0.1".parse().unwrap());
        let id = extract_client_id(&headers, &config);
        assert_eq!(id, "anon:203.0.113.50");
    }

    #[test]
    fn test_extract_client_id_x_real_ip_trusted_proxy() {
        let config = trusted_proxy_config();
        let mut headers = http::HeaderMap::new();
        headers.insert("X-Real-IP", "198.51.100.42".parse().unwrap());
        headers.insert("x-real-ip-internal", "127.0.0.1".parse().unwrap());
        let id = extract_client_id(&headers, &config);
        assert_eq!(id, "anon:198.51.100.42");
    }

    #[test]
    fn test_extract_client_id_bearer_takes_priority_over_ip() {
        let config = trusted_proxy_config();
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Bearer my_token_here".parse().unwrap(),
        );
        headers.insert("X-Forwarded-For", "203.0.113.50".parse().unwrap());
        let id = extract_client_id(&headers, &config);
        assert!(id.starts_with("user:"), "Bearer token should take priority over IP");
    }
}
