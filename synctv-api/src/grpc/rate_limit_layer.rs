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

use axum::{body::Body as AxumBody, extract::ConnectInfo, http};
use tower::{Layer, Service};
use tracing::warn;

use super::interceptors::GrpcRateLimitTier;
use synctv_core::service::auth::JwtValidator;
use synctv_core::service::{RateLimitError, RateLimiter};
use synctv_core::Config;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Tower layer that wraps the gRPC server with async distributed rate limiting.
///
/// Applied at the server level via `Server::builder().layer(rate_limit_layer)`.
/// Determines the rate limit tier from the gRPC request path (service name).
///
/// Uses `RateLimiter::check_rate_limit` which checks Redis first (distributed
/// across replicas) and falls back to in-memory governor if Redis is unavailable.
///
/// # Security
///
/// For authenticated requests, the rate limit key is derived from the verified
/// `user_id` in the JWT claims, not from the token hash. This ensures that a user
/// with multiple valid tokens shares a single rate limit quota, preventing quota
/// bypass through multiple token issuance.
#[derive(Clone)]
pub struct GrpcRateLimitLayer {
    rate_limiter: Arc<RateLimiter>,
    config: Arc<Config>,
    jwt_validator: Arc<JwtValidator>,
    strict_distributed: bool,
}

impl GrpcRateLimitLayer {
    /// Create a new distributed rate limit layer.
    ///
    /// The tier is determined per-request from the gRPC service path.
    /// Rate limit values (`max_requests`, `window_seconds`) are read from
    /// `config.grpc_rate_limits` per tier.
    ///
    /// # Arguments
    /// * `rate_limiter` - The rate limiter service
    /// * `config` - Application configuration
    /// * `jwt_validator` - JWT validator for extracting `user_id` from tokens
    #[must_use]
    pub fn new(
        rate_limiter: RateLimiter,
        config: Arc<Config>,
        jwt_validator: JwtValidator,
    ) -> Self {
        let strict_distributed = config.cluster_runtime_enabled();
        Self {
            rate_limiter: Arc::new(rate_limiter),
            config,
            jwt_validator: Arc::new(jwt_validator),
            strict_distributed,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub const fn uses_strict_distributed(&self) -> bool {
        self.strict_distributed
    }
}

impl<S> Layer<S> for GrpcRateLimitLayer {
    type Service = GrpcRateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcRateLimitService {
            inner,
            rate_limiter: self.rate_limiter.clone(),
            config: self.config.clone(),
            jwt_validator: self.jwt_validator.clone(),
            strict_distributed: self.strict_distributed,
        }
    }
}

/// Tower service that applies async distributed rate limiting before forwarding
/// to the inner service.
#[derive(Clone)]
pub struct GrpcRateLimitService<S> {
    inner: S,
    rate_limiter: Arc<RateLimiter>,
    config: Arc<Config>,
    jwt_validator: Arc<JwtValidator>,
    strict_distributed: bool,
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

/// Map a raw `grpc-status` numeric value to a human-readable label.
fn grpc_status_code_to_label(code: &str) -> &'static str {
    match code {
        "0" => "ok",
        "1" => "cancelled",
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
}

fn rate_limit_error_response(
    err: &RateLimitError,
    service_label: &str,
    method_label: &str,
) -> http::Response<AxumBody> {
    let status = match err {
        RateLimitError::RateLimitExceeded { .. } => {
            tonic::Status::resource_exhausted("Rate limit exceeded. Please retry later.")
        }
        RateLimitError::BackendUnavailable(_) => {
            tonic::Status::unavailable("Rate limit service temporarily unavailable.")
        }
        RateLimitError::RedisError(_) => tonic::Status::internal("Internal error"),
    };
    let status_label = match status.code() {
        tonic::Code::ResourceExhausted => "resource_exhausted",
        tonic::Code::Unavailable => "unavailable",
        _ => "internal",
    };
    synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
        .with_label_values(&[service_label, method_label, status_label])
        .inc();
    status.into_http::<tonic::body::Body>().map(AxumBody::new)
}

/// Best-effort gRPC status label extraction that never consumes the response body.
///
/// Collecting the body to inspect trailers breaks streaming RPCs such as
/// `RoomService/MessageStream`, because the middleware would block until the stream
/// finishes before returning headers to the client. For observability we prefer a
/// conservative label over interfering with transport semantics, so this helper only
/// inspects response headers and falls back to the HTTP status code.
fn grpc_status_label_from_response(resp: &http::Response<AxumBody>) -> &'static str {
    if let Some(status_val) = resp.headers().get("grpc-status") {
        return grpc_status_code_to_label(status_val.to_str().unwrap_or(""));
    }

    if resp.status().is_success() {
        "ok"
    } else {
        "error"
    }
}

/// Determine the rate limit tier from the gRPC request path.
///
/// gRPC paths follow the format `/<package>.<ServiceName>/<MethodName>`.
/// Maps known service names (and specific methods) to their corresponding rate limit tiers.
///
/// For `UserService` and `RoomService`, read-only RPCs are classified as Read tier
/// while mutation RPCs remain at Write tier.
fn tier_from_path(path: &str) -> Option<GrpcRateLimitTier> {
    // gRPC path format: /synctv.client.AuthService/Login
    let parts: Vec<&str> = path.split('/').collect();
    let service_name = parts.get(1).and_then(|full| full.rsplit('.').next());
    let method_name = parts.get(2).copied();

    match service_name {
        Some("AuthService") => Some(GrpcRateLimitTier::Auth),
        Some("EmailService") => Some(GrpcRateLimitTier::Email),
        Some("UserService") => Some(user_service_tier(method_name)),
        Some("RoomService") => Some(room_service_tier(method_name)),
        Some("AdminService") => Some(GrpcRateLimitTier::Admin),
        Some("NotificationService") => Some(notification_service_tier(method_name)),
        Some("OAuth2Service") => Some(oauth2_service_tier(method_name)),
        // Provider services
        Some("AlistProviderService") => Some(alist_provider_tier(method_name)),
        Some("BilibiliProviderService") => Some(bilibili_provider_tier(method_name)),
        Some("EmbyProviderService") => Some(emby_provider_tier(method_name)),
        // Internal infrastructure services are explicitly exempt.
        Some("ClusterService" | "StreamRelayService" | "Health" | "ServerReflection") | None => {
            None
        }
        // Unknown services default to the conservative read tier so newly added
        // business RPCs cannot silently bypass rate limiting.
        Some(_) => Some(GrpcRateLimitTier::Read),
    }
}

/// Classify `UserService` methods into Read or Write tiers.
///
/// Read-only methods use the more permissive Read tier. All other methods
/// (mutations) default to Write tier.
fn user_service_tier(method: Option<&str>) -> GrpcRateLimitTier {
    match method {
        Some("GetProfile" | "GetUser" | "GetRoom" | "ListMyRooms" | "GetSettings") => {
            GrpcRateLimitTier::Read
        }
        _ => GrpcRateLimitTier::Write,
    }
}

/// Classify `RoomService` methods into Read or Write tiers.
///
/// Read-only methods use the more permissive Read tier. Media and playback
/// mutations are isolated into the Media tier. All other methods default to Write.
fn room_service_tier(method: Option<&str>) -> GrpcRateLimitTier {
    match method {
        Some(
            "AddMedia" | "DeleteMedia" | "DeleteEntries" | "AddMediaBatch" | "CreatePlaylist"
            | "UpdatePlaylist" | "DeletePlaylist" | "ReorderMediaBatch" | "StartPlayback"
            | "StopPlayback" | "UpdatePlayback" | "MoveMedia" | "SwapMedia" | "EditMedia"
            | "ClearPlaylist" | "CreatePublishKey",
        ) => GrpcRateLimitTier::Media,
        Some(
            "GetRoomSettings" | "GetRoomMembers" | "GetChatHistory" | "GetIceServers"
            | "GetPlaylist" | "GetPlayback" | "GetStreamInfo" | "ListRoomStreams" | "ListPlaylists"
            | "ListPlaylistItems",
        ) => GrpcRateLimitTier::Read,
        _ => GrpcRateLimitTier::Write,
    }
}

/// Classify `NotificationService` methods into Read or Write tiers.
fn notification_service_tier(method: Option<&str>) -> GrpcRateLimitTier {
    match method {
        Some("ListNotifications" | "GetNotification") => GrpcRateLimitTier::Read,
        _ => GrpcRateLimitTier::Write,
    }
}

/// Classify `OAuth2Service` methods into Read, Auth, or Write tiers.
///
/// Public discovery/login bootstrap endpoints should align with the HTTP router:
/// - `GetAuthorizationUrl`, `ListAvailableProviders`: Read tier
/// - `ExchangeAuthorizationCode`: Auth tier
/// - Authenticated account-management methods: Write tier
fn oauth2_service_tier(method: Option<&str>) -> GrpcRateLimitTier {
    match method {
        Some("GetAuthorizationUrl" | "ListAvailableProviders" | "GetLinkedProviders") => {
            GrpcRateLimitTier::Read
        }
        Some("GetAuthorizationUrlForBind" | "UnlinkProvider") => GrpcRateLimitTier::Write,
        _ => GrpcRateLimitTier::Auth,
    }
}

fn alist_provider_tier(method: Option<&str>) -> GrpcRateLimitTier {
    match method {
        Some("Login" | "Logout") => GrpcRateLimitTier::Auth,
        _ => GrpcRateLimitTier::Read,
    }
}

fn bilibili_provider_tier(method: Option<&str>) -> GrpcRateLimitTier {
    match method {
        Some("LoginQr" | "CheckQr" | "GetCaptcha" | "SendSms" | "LoginSms" | "Logout") => {
            GrpcRateLimitTier::Auth
        }
        _ => GrpcRateLimitTier::Read,
    }
}

fn emby_provider_tier(method: Option<&str>) -> GrpcRateLimitTier {
    match method {
        Some("Login" | "Logout") => GrpcRateLimitTier::Auth,
        _ => GrpcRateLimitTier::Read,
    }
}

/// Derive a stable rate limit key for a verified user.
///
/// Uses the verified `user_id` from JWT claims, ensuring that all tokens
/// belonging to the same user share a single rate limit quota.
///
/// Returns `"user:{user_id}"` format.
fn user_rate_limit_key(user_id: &str) -> String {
    format!("user:{user_id}")
}

/// Extract a stable client identifier from HTTP headers.
///
/// Priority:
/// 1. Verified `user_id` from JWT (authenticated users) - validates the JWT signature
///    and extracts the `user_id` from claims. This ensures all tokens belonging to
///    the same user share a single rate limit quota.
/// 2. Client IP from X-Forwarded-For or X-Real-IP headers (only if from a trusted proxy)
/// 3. Remote socket address (direct connection)
/// 4. "anon:unknown" fallback (only if no IP info available)
///
/// X-Forwarded-For and X-Real-IP headers are only trusted when the request comes
/// from a configured trusted proxy or when development mode is enabled, matching
/// the HTTP middleware pattern.
///
/// # Security
///
/// This function validates the JWT before extracting `user_id` to prevent spoofing.
/// Invalid or expired tokens fall back to IP-based rate limiting.
fn extract_client_id<B>(
    req: &http::Request<B>,
    config: &Config,
    jwt_validator: &JwtValidator,
) -> String {
    let headers = req.headers();
    // For authenticated requests, validate JWT and extract verified user_id.
    // This ensures that all tokens belonging to the same user share a single
    // rate limit quota, preventing quota bypass through multiple tokens.
    if let Some(auth_header) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        // First extract the bearer token
        if let Ok(token) =
            synctv_core::service::auth::JwtValidator::extract_bearer_token(auth_header)
        {
            // Validate the JWT and extract user_id from claims
            // This ensures the token is authentic and the user_id is verified
            if let Ok(claims) = jwt_validator.validate_token(&token) {
                return user_rate_limit_key(&claims.sub);
            }
            // If JWT validation fails (expired, invalid signature, etc.),
            // fall through to IP-based rate limiting
        }
    }

    // For anonymous requests, extract the peer address from tonic's connection metadata.
    let remote_addr = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip())
        .or_else(|| {
            req.extensions()
                .get::<tonic::transport::server::TcpConnectInfo>()
                .and_then(tonic::transport::server::TcpConnectInfo::remote_addr)
                .map(|addr| addr.ip())
        });

    // Only trust X-Forwarded-For/X-Real-IP when from a trusted proxy or in dev mode
    let should_trust_headers = remote_addr.is_some_and(|ip| config.server.is_trusted_proxy(&ip));

    if should_trust_headers {
        if let Some(ip) = headers
            .get("X-Forwarded-For")
            .and_then(|h| h.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
        {
            return format!("anon:{ip}");
        }
        if let Some(ip) = headers
            .get("X-Real-IP")
            .and_then(|h| h.to_str().ok())
            .map(str::trim)
            .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
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

impl<S> Service<http::Request<AxumBody>> for GrpcRateLimitService<S>
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

    // WIRING VERIFICATION (Issue #21):
    // This GrpcRateLimitService is applied at the Server level in grpc/mod.rs via:
    //   Server::builder().layer(blacklist_layer).layer(distributed_rate_limit_layer)
    // All registered gRPC services pass through this tower layer before reaching handlers.
    // When the rate limit is exceeded, `tonic::Status::resource_exhausted` is returned
    // immediately without calling `inner.call()`, so the request is fully rejected.
    fn call(&mut self, req: http::Request<AxumBody>) -> Self::Future {
        // Clone the inner service (tower best practice: swap ready clone out)
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        let rate_limiter = self.rate_limiter.clone();
        let config = self.config.clone();
        let jwt_validator = self.jwt_validator.clone();

        // Determine tier from the request path
        let path = req.uri().path().to_string();
        let tier = tier_from_path(&path);

        // Extract service and method names for metrics labels
        let (service_label, method_label) = extract_grpc_labels(&path);

        // If no tier matches (cluster service, unknown paths), skip rate limiting
        // but still record metrics
        let Some(tier) = tier else {
            return Box::pin(async move {
                match inner.call(req).await {
                    Ok(resp) => {
                        let status = grpc_status_label_from_response(&resp);
                        let status_str = status.to_string();
                        synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
                            .with_label_values(&[&service_label, &method_label, &status_str])
                            .inc();
                        Ok(resp)
                    }
                    Err(e) => {
                        let error_str = "error".to_string();
                        synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
                            .with_label_values(&[&service_label, &method_label, &error_str])
                            .inc();
                        Err(e)
                    }
                }
            });
        };

        let client_id = extract_client_id(&req, &config, &jwt_validator);
        let grpc_rate_config = config.grpc_rate_limits.clone();
        let strict_distributed = self.strict_distributed;

        Box::pin(async move {
            // Use the same key format as HTTP middleware ("ratelimit:{category}:{client_id}")
            // so that rate limits are shared across HTTP and gRPC transports.
            let rate_key = format!("ratelimit:{}:{}", tier.key_suffix(), client_id);
            let max_reqs = tier.max_requests(&grpc_rate_config);
            let win_secs = tier.window_seconds(&grpc_rate_config);

            let rate_limit_result = if strict_distributed {
                rate_limiter
                    .check_rate_limit_distributed(&rate_key, max_reqs, win_secs)
                    .await
            } else {
                rate_limiter
                    .check_rate_limit(&rate_key, max_reqs, win_secs)
                    .await
            };

            if let Err(err) = rate_limit_result {
                warn!(
                    client_id = %client_id,
                    tier = ?tier,
                    max_requests = max_reqs,
                    path = %path,
                    error = %err,
                    "gRPC distributed rate limit rejected request"
                );
                return Ok(rate_limit_error_response(
                    &err,
                    &service_label,
                    &method_label,
                ));
            }

            match inner.call(req).await {
                Ok(resp) => {
                    let status = grpc_status_label_from_response(&resp);
                    let status_str = status.to_string();
                    synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
                        .with_label_values(&[&service_label, &method_label, &status_str])
                        .inc();
                    Ok(resp)
                }
                Err(e) => {
                    let error_str = String::from("error");
                    synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
                        .with_label_values(&[&service_label, &method_label, &error_str])
                        .inc();
                    Err(e)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use synctv_core::models::UserId;
    use synctv_core::service::auth::{jwt::TokenType, JwtService};
    use synctv_core::service::RateLimitBackend;
    use synctv_core::Result as CoreResult;
    use tower::service_fn;

    fn request_with_headers(headers: http::HeaderMap) -> http::Request<AxumBody> {
        let request = http::Request::builder()
            .uri("/synctv.client.AuthService/Login")
            .body(AxumBody::empty())
            .unwrap();
        let (mut parts, body) = request.into_parts();
        parts.headers = headers;
        http::Request::from_parts(parts, body)
    }

    fn request_with_peer(headers: http::HeaderMap, peer: SocketAddr) -> http::Request<AxumBody> {
        let mut request = request_with_headers(headers);
        request
            .extensions_mut()
            .insert(tonic::transport::server::TcpConnectInfo {
                local_addr: None,
                remote_addr: Some(peer),
            });
        request
    }

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
            tier_from_path("/synctv.client.RoomService/AddMedia"),
            Some(GrpcRateLimitTier::Media)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/DeleteMedia"),
            Some(GrpcRateLimitTier::Media)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/AddMediaBatch"),
            Some(GrpcRateLimitTier::Media)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/DeleteEntries"),
            Some(GrpcRateLimitTier::Media)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/StartPlayback"),
            Some(GrpcRateLimitTier::Media)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/CreatePlaylist"),
            Some(GrpcRateLimitTier::Media)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/UpdatePlaylist"),
            Some(GrpcRateLimitTier::Media)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/DeletePlaylist"),
            Some(GrpcRateLimitTier::Media)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/ReorderMediaBatch"),
            Some(GrpcRateLimitTier::Media)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/CreatePublishKey"),
            Some(GrpcRateLimitTier::Media)
        );
    }

    #[test]
    fn test_tier_from_path_user_read() {
        assert_eq!(
            tier_from_path("/synctv.client.v1.UserService/GetProfile"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.v1.UserService/GetRoom"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.v1.UserService/ListMyRooms"),
            Some(GrpcRateLimitTier::Read)
        );
    }

    #[test]
    fn test_tier_from_path_user_write() {
        assert_eq!(
            tier_from_path("/synctv.client.UserService/UpdateProfile"),
            Some(GrpcRateLimitTier::Write)
        );
    }

    #[test]
    fn test_tier_from_path_room_read() {
        assert_eq!(
            tier_from_path("/synctv.client.v1.RoomService/GetRoomSettings"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.v1.RoomService/GetRoomMembers"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.v1.RoomService/GetChatHistory"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.v1.RoomService/GetIceServers"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/GetPlayback"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/GetStreamInfo"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/ListRoomStreams"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/ListPlaylists"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/ListPlaylistItems"),
            Some(GrpcRateLimitTier::Read)
        );
    }

    #[test]
    fn test_tier_from_path_room_write() {
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/UpdateRoomSettings"),
            Some(GrpcRateLimitTier::Write)
        );
        assert_eq!(
            tier_from_path("/synctv.client.RoomService/SendMessage"),
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
            tier_from_path("/synctv.client.NotificationService/ListNotifications"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.NotificationService/MarkAsRead"),
            Some(GrpcRateLimitTier::Write)
        );
        assert_eq!(
            tier_from_path("/synctv.client.NotificationService/DeleteNotification"),
            Some(GrpcRateLimitTier::Write)
        );
    }

    #[test]
    fn test_tier_from_path_oauth2() {
        assert_eq!(
            tier_from_path("/synctv.client.OAuth2Service/GetAuthorizationUrl"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.OAuth2Service/ListAvailableProviders"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.client.OAuth2Service/ExchangeAuthorizationCode"),
            Some(GrpcRateLimitTier::Auth)
        );
        assert_eq!(
            tier_from_path("/synctv.client.OAuth2Service/GetAuthorizationUrlForBind"),
            Some(GrpcRateLimitTier::Write)
        );
        assert_eq!(
            tier_from_path("/synctv.client.OAuth2Service/GetLinkedProviders"),
            Some(GrpcRateLimitTier::Read)
        );
    }

    #[test]
    fn test_tier_from_path_providers() {
        assert_eq!(
            tier_from_path("/synctv.providers.alist.AlistProviderService/List"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.alist.AlistProviderService/Login"),
            Some(GrpcRateLimitTier::Auth)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.alist.AlistProviderService/Logout"),
            Some(GrpcRateLimitTier::Auth)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.bilibili.BilibiliProviderService/Parse"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.bilibili.BilibiliProviderService/LoginQr"),
            Some(GrpcRateLimitTier::Auth)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.bilibili.BilibiliProviderService/CheckQr"),
            Some(GrpcRateLimitTier::Auth)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.bilibili.BilibiliProviderService/GetCaptcha"),
            Some(GrpcRateLimitTier::Auth)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.bilibili.BilibiliProviderService/SendSms"),
            Some(GrpcRateLimitTier::Auth)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.bilibili.BilibiliProviderService/LoginSms"),
            Some(GrpcRateLimitTier::Auth)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.bilibili.BilibiliProviderService/Logout"),
            Some(GrpcRateLimitTier::Auth)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.emby.EmbyProviderService/GetMe"),
            Some(GrpcRateLimitTier::Read)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.emby.EmbyProviderService/Login"),
            Some(GrpcRateLimitTier::Auth)
        );
        assert_eq!(
            tier_from_path("/synctv.providers.emby.EmbyProviderService/Logout"),
            Some(GrpcRateLimitTier::Auth)
        );
    }

    #[test]
    fn test_tier_from_path_cluster_no_rate_limit() {
        assert_eq!(
            tier_from_path("/synctv.cluster.ClusterService/GetNodes"),
            None
        );
    }

    #[test]
    fn test_tier_from_path_internal_services_are_exempt() {
        assert_eq!(tier_from_path("/grpc.health.v1.Health/Check"), None);
        assert_eq!(
            tier_from_path("/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"),
            None
        );
        assert_eq!(
            tier_from_path("/synctv.livestream.StreamRelayService/RelayStream"),
            None
        );
    }

    #[test]
    fn test_tier_from_path_empty() {
        assert_eq!(tier_from_path(""), None);
    }

    // ============== Test helper functions ==============

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

    /// Create a test JWT service with a known secret
    fn create_test_jwt_service() -> Arc<JwtService> {
        Arc::new(
            JwtService::new("test-secret-for-rate-limit-layer-tests-1234567890abcdef").unwrap(),
        )
    }

    /// Create a test JWT validator
    fn create_test_jwt_validator(jwt_service: &Arc<JwtService>) -> JwtValidator {
        JwtValidator::new(jwt_service.clone())
    }

    /// Create a valid test token for a specific `user_id`
    fn create_test_token(jwt_service: &JwtService, user_id: &str) -> String {
        let user_id = UserId::from_string(user_id.to_string());
        jwt_service
            .sign_token(&user_id, TokenType::Access, 0)
            .unwrap()
    }

    /// Create an invalid/fake token that won't pass validation
    fn fake_token(suffix: &str) -> String {
        format!("eyJhbGciOiJIUzI1NiJ9.payload.fakesig-{suffix}")
    }

    // ============== Rate limit key tests ==============

    #[test]
    fn test_user_rate_limit_key_format() {
        let key = user_rate_limit_key("user123");
        assert_eq!(key, "user:user123");
    }

    #[test]
    fn test_user_rate_limit_key_consistent() {
        // Same user_id should always produce the same key
        let key1 = user_rate_limit_key("user456");
        let key2 = user_rate_limit_key("user456");
        assert_eq!(key1, key2);
    }

    // ============== extract_client_id tests with verified JWT ==============

    #[test]
    fn test_extract_client_id_valid_jwt_returns_user_key() {
        // When a valid JWT is provided, the client ID should be "user:{user_id}"
        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let token = create_test_token(&jwt_service, "user123");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );

        let id = extract_client_id(&request_with_headers(headers), &config, &jwt_validator);
        assert_eq!(id, "user:user123", "Expected user:user123, got: {id}");
    }

    #[test]
    fn test_extract_client_id_same_user_different_tokens_same_key() {
        // SECURITY: Multiple tokens for the same user should produce the same rate limit key
        // This prevents users from bypassing rate limits by using multiple valid tokens
        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        // Create two different tokens for the same user
        let token1 = create_test_token(&jwt_service, "userABC");
        let token2 = create_test_token(&jwt_service, "userABC");

        // Tokens should be different (different iat)
        assert_ne!(token1, token2, "Tokens should be different");

        let mut headers1 = http::HeaderMap::new();
        headers1.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {token1}").parse().unwrap(),
        );

        let mut headers2 = http::HeaderMap::new();
        headers2.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {token2}").parse().unwrap(),
        );

        let id1 = extract_client_id(&request_with_headers(headers1), &config, &jwt_validator);
        let id2 = extract_client_id(&request_with_headers(headers2), &config, &jwt_validator);

        // Both should produce the same key (user:userABC)
        assert_eq!(
            id1, "user:userABC",
            "First token should produce user:userABC"
        );
        assert_eq!(
            id2, "user:userABC",
            "Second token should produce user:userABC"
        );
        assert_eq!(
            id1, id2,
            "Different tokens for same user must produce same rate limit key"
        );
    }

    #[test]
    fn test_extract_client_id_different_users_different_keys() {
        // Different users should have different rate limit keys
        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let token1 = create_test_token(&jwt_service, "user1");
        let token2 = create_test_token(&jwt_service, "user2");

        let mut headers1 = http::HeaderMap::new();
        headers1.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {token1}").parse().unwrap(),
        );

        let mut headers2 = http::HeaderMap::new();
        headers2.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {token2}").parse().unwrap(),
        );

        let id1 = extract_client_id(&request_with_headers(headers1), &config, &jwt_validator);
        let id2 = extract_client_id(&request_with_headers(headers2), &config, &jwt_validator);

        assert_ne!(
            id1, id2,
            "Different users must have different rate limit keys"
        );
        assert_eq!(id1, "user:user1");
        assert_eq!(id2, "user:user2");
    }

    #[test]
    fn test_extract_client_id_invalid_jwt_falls_back_to_ip() {
        // When JWT validation fails, should fall back to IP-based rate limiting
        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        // Use a fake/invalid token
        let token = fake_token("invalid");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );

        let id = extract_client_id(&request_with_headers(headers), &config, &jwt_validator);
        // Should fall back to anon:unknown since no valid JWT and no IP info
        assert_eq!(
            id, "anon:unknown",
            "Invalid JWT should fall back to anonymous"
        );
    }

    #[test]
    fn test_extract_client_id_no_auth() {
        // No auth header should return anon:unknown
        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let headers = http::HeaderMap::new();
        let id = extract_client_id(&request_with_headers(headers), &config, &jwt_validator);
        assert_eq!(id, "anon:unknown");
    }

    #[test]
    fn test_extract_client_id_basic_auth_falls_back() {
        // Basic auth (not bearer) should fall back to IP-based
        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Basic dXNlcjpwYXNz".parse().unwrap(),
        );

        let id = extract_client_id(&request_with_headers(headers), &config, &jwt_validator);
        assert_eq!(id, "anon:unknown");
    }

    #[test]
    fn test_extract_client_id_x_forwarded_for_untrusted() {
        // Without trusted proxies, X-Forwarded-For should NOT be trusted
        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let mut headers = http::HeaderMap::new();
        headers.insert(
            "X-Forwarded-For",
            "203.0.113.50, 70.41.3.18".parse().unwrap(),
        );

        let id = extract_client_id(&request_with_headers(headers), &config, &jwt_validator);
        assert_eq!(id, "anon:unknown");
    }

    #[test]
    fn test_extract_client_id_x_forwarded_for_trusted_proxy() {
        // With a trusted proxy peer address, X-Forwarded-For is trusted.
        let config = trusted_proxy_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let mut headers = http::HeaderMap::new();
        headers.insert(
            "X-Forwarded-For",
            "203.0.113.50, 70.41.3.18".parse().unwrap(),
        );

        let id = extract_client_id(
            &request_with_peer(headers, "127.0.0.1:50051".parse().unwrap()),
            &config,
            &jwt_validator,
        );
        assert_eq!(id, "anon:203.0.113.50");
    }

    #[test]
    fn test_extract_client_id_x_real_ip_trusted_proxy() {
        let config = trusted_proxy_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let mut headers = http::HeaderMap::new();
        headers.insert("X-Real-IP", "198.51.100.42".parse().unwrap());

        let id = extract_client_id(
            &request_with_peer(headers, "127.0.0.1:50051".parse().unwrap()),
            &config,
            &jwt_validator,
        );
        assert_eq!(id, "anon:198.51.100.42");
    }

    #[test]
    fn test_extract_client_id_invalid_forwarded_for_falls_back_to_x_real_ip() {
        let config = trusted_proxy_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let mut headers = http::HeaderMap::new();
        headers.insert("X-Forwarded-For", "not-an-ip".parse().unwrap());
        headers.insert("X-Real-IP", "198.51.100.42".parse().unwrap());

        let id = extract_client_id(
            &request_with_peer(headers, "127.0.0.1:50051".parse().unwrap()),
            &config,
            &jwt_validator,
        );
        assert_eq!(id, "anon:198.51.100.42");
    }

    #[test]
    fn test_extract_client_id_invalid_proxy_headers_fall_back_to_peer_ip() {
        let config = trusted_proxy_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let mut headers = http::HeaderMap::new();
        headers.insert("X-Forwarded-For", "garbage".parse().unwrap());
        headers.insert("X-Real-IP", "still-not-an-ip".parse().unwrap());

        let id = extract_client_id(
            &request_with_peer(headers, "127.0.0.1:50051".parse().unwrap()),
            &config,
            &jwt_validator,
        );
        assert_eq!(id, "anon:127.0.0.1");
    }

    #[test]
    fn test_extract_client_id_valid_jwt_takes_priority_over_ip() {
        // Valid JWT takes priority over IP-based key even when trusted proxy headers are present
        let config = trusted_proxy_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let token = create_test_token(&jwt_service, "priority_user");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers.insert("X-Forwarded-For", "203.0.113.50".parse().unwrap());

        let id = extract_client_id(&request_with_headers(headers), &config, &jwt_validator);
        // Must be user-based, not IP-based
        assert_eq!(
            id, "user:priority_user",
            "JWT user should take priority over IP"
        );
    }

    #[test]
    fn test_extract_client_id_invalid_jwt_with_ip_falls_back_to_ip() {
        // Invalid JWT should fall back to IP when available
        let config = trusted_proxy_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let token = fake_token("invalid");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers.insert("X-Forwarded-For", "203.0.113.50".parse().unwrap());
        headers.insert("x-real-ip-internal", "127.0.0.1".parse().unwrap());

        let id = extract_client_id(
            &request_with_peer(headers, "127.0.0.1:50051".parse().unwrap()),
            &config,
            &jwt_validator,
        );
        // Invalid JWT should fall back to IP-based rate limiting
        assert_eq!(
            id, "anon:203.0.113.50",
            "Invalid JWT should fall back to IP"
        );
    }

    #[test]
    fn test_extract_client_id_uses_tonic_peer_addr_for_anonymous_requests() {
        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        let id = extract_client_id(
            &request_with_peer(
                http::HeaderMap::new(),
                "198.51.100.42:44321".parse().unwrap(),
            ),
            &config,
            &jwt_validator,
        );
        assert_eq!(id, "anon:198.51.100.42");
    }

    #[test]
    fn test_grpc_rate_limit_layer_uses_strict_distributed_in_cluster_mode() {
        let mut config = test_config();
        config.cluster.enabled = true;
        config.server.cluster_secret = "cluster-secret-for-test".to_string();

        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);
        let layer = GrpcRateLimitLayer::new(
            RateLimiter::local_only("cluster-grpc:".to_string()),
            Arc::new(config),
            jwt_validator,
        );

        assert!(
            layer.uses_strict_distributed(),
            "cluster mode gRPC rate limiting must fail closed instead of degrading to per-replica in-memory quotas"
        );
    }

    #[test]
    fn test_grpc_rate_limit_layer_keeps_best_effort_mode_in_standalone() {
        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);
        let layer = GrpcRateLimitLayer::new(
            RateLimiter::local_only("standalone-grpc:".to_string()),
            Arc::new(config),
            jwt_validator,
        );

        assert!(
            !layer.uses_strict_distributed(),
            "standalone gRPC rate limiting may use best-effort local quotas"
        );
    }

    enum StubStrictResult {
        RateLimited,
        BackendUnavailable,
    }

    struct StubRateLimitBackend {
        strict_result: StubStrictResult,
    }

    #[async_trait]
    impl RateLimitBackend for StubRateLimitBackend {
        async fn check(
            &self,
            _key: &str,
            _max_requests: u32,
            _window_seconds: u64,
        ) -> Result<(), RateLimitError> {
            Ok(())
        }

        async fn check_strict(
            &self,
            _key: &str,
            _max_requests: u32,
            _window_seconds: u64,
        ) -> Result<(), RateLimitError> {
            match self.strict_result {
                StubStrictResult::RateLimited => Err(RateLimitError::RateLimitExceeded {
                    retry_after_seconds: 1,
                }),
                StubStrictResult::BackendUnavailable => Err(RateLimitError::BackendUnavailable(
                    "redis unavailable".to_string(),
                )),
            }
        }

        async fn get_quota(
            &self,
            _key: &str,
            max_requests: u32,
            _window_seconds: u64,
        ) -> CoreResult<(u32, u64)> {
            Ok((max_requests, 0))
        }

        async fn reset(&self, _key: &str) -> CoreResult<()> {
            Ok(())
        }

        async fn health_check(&self) -> Result<(), String> {
            Ok(())
        }
    }

    fn cluster_mode_config() -> Config {
        let mut config = test_config();
        config.cluster.enabled = true;
        config.server.cluster_secret = "cluster-secret-for-test".to_string();
        config
    }

    async fn call_layer_with_strict_result(
        strict_result: StubStrictResult,
    ) -> http::Response<AxumBody> {
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);
        let rate_limiter = RateLimiter::from_backend(
            Arc::new(StubRateLimitBackend { strict_result }),
            "grpc-test:".to_string(),
        );
        let layer =
            GrpcRateLimitLayer::new(rate_limiter, Arc::new(cluster_mode_config()), jwt_validator);
        let mut svc = layer.layer(service_fn(|_req: http::Request<AxumBody>| async move {
            Ok::<_, Infallible>(
                tonic::Status::ok("ok")
                    .into_http::<tonic::body::Body>()
                    .map(AxumBody::new),
            )
        }));

        let request = http::Request::builder()
            .uri("/synctv.client.AuthService/Login")
            .body(AxumBody::empty())
            .unwrap();

        svc.call(request).await.unwrap()
    }

    #[tokio::test]
    async fn test_grpc_rate_limit_layer_maps_actual_limit_exceeded_to_resource_exhausted() {
        let response = call_layer_with_strict_result(StubStrictResult::RateLimited).await;

        assert_eq!(
            response
                .headers()
                .get("grpc-status")
                .and_then(|v| v.to_str().ok()),
            Some("8")
        );
    }

    #[tokio::test]
    async fn test_grpc_rate_limit_layer_maps_backend_unavailable_to_unavailable() {
        let response = call_layer_with_strict_result(StubStrictResult::BackendUnavailable).await;

        assert_eq!(
            response
                .headers()
                .get("grpc-status")
                .and_then(|v| v.to_str().ok()),
            Some("14")
        );
    }

    #[tokio::test]
    async fn test_grpc_rate_limit_layer_does_not_buffer_streaming_response_bodies() {
        use futures::stream;
        use http_body_util::StreamBody;
        use hyper::body::Frame;

        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);
        let layer = GrpcRateLimitLayer::new(
            RateLimiter::local_only("grpc-streaming-test:".to_string()),
            Arc::new(test_config()),
            jwt_validator,
        );

        let mut svc = layer.layer(service_fn(|_req: http::Request<AxumBody>| async move {
            let stream = stream::pending::<Result<Frame<bytes::Bytes>, Infallible>>();
            let body = AxumBody::new(StreamBody::new(stream));
            Ok::<_, Infallible>(
                http::Response::builder()
                    .status(http::StatusCode::OK)
                    .body(body)
                    .unwrap(),
            )
        }));

        let request = http::Request::builder()
            .uri("/synctv.client.RoomService/MessageStream")
            .body(AxumBody::empty())
            .unwrap();

        let result =
            tokio::time::timeout(std::time::Duration::from_millis(100), svc.call(request)).await;

        assert!(
            result.is_ok(),
            "rate limit middleware must not wait for streaming bodies to complete"
        );
    }

    // ============== Security-focused tests ==============

    #[test]
    fn test_security_multiple_tokens_cannot_bypass_rate_limit() {
        // SECURITY TEST: Verify that the same user with multiple tokens
        // cannot bypass rate limiting by rotating through tokens.
        //
        // This is the core fix for the "Rate Limit Key 可伪造漏洞" issue.
        // Before the fix, each token had its own rate limit quota.
        // After the fix, all tokens for the same user share one quota.

        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        // All tokens for the same user should produce the same key
        let user_id = "target_user";
        let same_user_tokens: Vec<String> = (0..5)
            .map(|_| create_test_token(&jwt_service, user_id))
            .collect();

        let keys: Vec<String> = same_user_tokens
            .iter()
            .map(|token| {
                let mut headers = http::HeaderMap::new();
                headers.insert(
                    http::header::AUTHORIZATION,
                    format!("Bearer {token}").parse().unwrap(),
                );
                extract_client_id(&request_with_headers(headers), &config, &jwt_validator)
            })
            .collect();

        // All keys should be identical
        let first_key = &keys[0];
        for key in &keys {
            assert_eq!(
                key, first_key,
                "All tokens for same user must produce same key"
            );
        }
        assert_eq!(first_key, "user:target_user");
    }

    #[test]
    fn test_security_cannot_spoof_user_id_in_token() {
        // SECURITY TEST: Verify that an attacker cannot craft a fake token
        // with a spoofed user_id to impersonate another user's rate limit quota.
        //
        // Since we validate the JWT signature before extracting user_id,
        // a fake token with a spoofed sub claim will fail validation and
        // fall back to IP-based rate limiting.

        let config = test_config();
        let jwt_service = create_test_jwt_service();
        let jwt_validator = create_test_jwt_validator(&jwt_service);

        // Create a valid token for the legitimate user
        let legitimate_token = create_test_token(&jwt_service, "legitimate_user");

        // Create a fake token that tries to spoof the same user_id
        // (This fake token doesn't have a valid signature)
        let spoofed_token = fake_token("legitimate_user");

        let mut legit_headers = http::HeaderMap::new();
        legit_headers.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {legitimate_token}").parse().unwrap(),
        );

        let mut spoofed_headers = http::HeaderMap::new();
        spoofed_headers.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {spoofed_token}").parse().unwrap(),
        );

        let legit_id = extract_client_id(
            &request_with_headers(legit_headers),
            &config,
            &jwt_validator,
        );
        let spoofed_id = extract_client_id(
            &request_with_headers(spoofed_headers),
            &config,
            &jwt_validator,
        );

        // Legitimate token gets user-based key
        assert_eq!(legit_id, "user:legitimate_user");
        // Spoofed token falls back to anonymous (since validation fails)
        assert_eq!(spoofed_id, "anon:unknown");
        // They should NOT be the same
        assert_ne!(
            legit_id, spoofed_id,
            "Spoofed token should not get legitimate user's key"
        );
    }
}
