use std::sync::Arc;
use sha2::{Sha256, Digest};
use subtle::ConstantTimeEq;
use synctv_core::service::auth::{JwtService, JwtValidator};
use tonic::{Request, Status};
use tracing::warn;
use std::fmt::Debug;

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// User context - contains `user_id` and `iat` extracted from JWT
/// Used by `UserService` and `AdminService` methods
#[derive(Debug, Clone)]
pub struct UserContext {
    pub user_id: String,
    /// Token issued-at timestamp (Unix seconds), used for password-change invalidation
    pub iat: i64,
    /// Raw bearer token, used for blacklist checking at the service layer
    /// (interceptors are sync and cannot perform async Redis lookups)
    pub raw_token: String,
}

/// Room context - contains `UserContext` and `room_id`
/// Used by `RoomService` and `MediaService` methods
#[derive(Debug, Clone)]
pub struct RoomContext {
    #[allow(dead_code)] // Nested for future use when both user and room info needed
    pub user_ctx: UserContext,
    pub room_id: String,
}

/// Simple JWT auth interceptor (synchronous, compatible with `tonic::service::Interceptor`)
/// Only validates JWT and extracts `user_id` into `AuthContext`
/// Service methods should call helper functions to load entities from database
#[derive(Clone)]
pub struct AuthInterceptor {
    jwt_validator: Arc<JwtValidator>,
}

impl AuthInterceptor {
    #[must_use] 
    pub fn new(jwt_service: JwtService) -> Self {
        Self {
            jwt_validator: Arc::new(JwtValidator::new(Arc::new(jwt_service))),
        }
    }

    /// Inject `UserContext` - validates JWT and extracts `user_id` + `iat`
    /// Used for `UserService` and `AdminService`
    #[allow(clippy::result_large_err)]
    pub fn inject_user<T>(&self, mut request: Request<T>) -> Result<Request<T>, Status> {
        // Extract raw token before validation (for blacklist checking at service layer)
        let raw_token = Self::extract_raw_token(request.metadata())?;

        // Use unified validator for gRPC validation
        let claims = self
            .jwt_validator
            .validate_grpc_as_status(request.metadata())?;

        // Inject UserContext with user_id, iat, and raw token
        let user_context = UserContext {
            user_id: claims.sub,
            iat: claims.iat,
            raw_token,
        };
        request.extensions_mut().insert(user_context);

        Ok(request)
    }

    /// Inject `RoomContext` - validates JWT, extracts `user_id` and `room_id` from x-room-id header
    /// Used for `RoomService` and `MediaService`
    #[allow(clippy::result_large_err)]
    pub fn inject_room<T>(&self, mut request: Request<T>) -> Result<Request<T>, Status> {
        // Extract raw token before validation (for blacklist checking at service layer)
        let raw_token = Self::extract_raw_token(request.metadata())?;

        // Use unified validator for gRPC validation
        let claims = self
            .jwt_validator
            .validate_grpc_as_status(request.metadata())?;

        // Extract room_id from x-room-id header
        let room_id = request
            .metadata()
            .get("x-room-id")
            .ok_or_else(|| Status::invalid_argument("Missing x-room-id header"))?
            .to_str()
            .map_err(|_| Status::invalid_argument("Invalid x-room-id header"))?
            .to_string();

        // Inject UserContext (for nested structure)
        let user_context = UserContext {
            user_id: claims.sub.clone(),
            iat: claims.iat,
            raw_token: raw_token.clone(),
        };
        request.extensions_mut().insert(user_context);

        // Inject RoomContext
        let room_context = RoomContext {
            user_ctx: UserContext {
                user_id: claims.sub,
                iat: claims.iat,
                raw_token,
            },
            room_id,
        };
        request.extensions_mut().insert(room_context);

        Ok(request)
    }

    /// Extract the raw bearer token from gRPC metadata.
    ///
    /// Used to capture the token for async blacklist checking at the service layer,
    /// since interceptors are synchronous and cannot call Redis.
    #[allow(clippy::result_large_err)]
    fn extract_raw_token(metadata: &tonic::metadata::MetadataMap) -> Result<String, Status> {
        let auth_header = metadata
            .get("authorization")
            .ok_or_else(|| Status::unauthenticated("Missing authorization header"))?
            .to_str()
            .map_err(|_| Status::unauthenticated("Invalid authorization header format"))?;

        JwtValidator::extract_bearer_token(auth_header)
            .map_err(|e| Status::unauthenticated(format!("Token extraction failed: {e}")))
    }
}

impl std::fmt::Debug for AuthInterceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthInterceptor").finish()
    }
}

/// Logging interceptor for gRPC requests
///
/// Logs incoming requests with method name, timing, and status.
#[derive(Clone)]
pub struct LoggingInterceptor;

impl LoggingInterceptor {
    #[must_use] 
    pub const fn new() -> Self {
        Self
    }

    /// Log request with method name and timing
    pub fn log<T>(&self, method: &'static str, request: Request<T>) -> Request<T> {
        let metadata = request.metadata();
        let user_agent = metadata
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown");

        tracing::debug!(
            method = method,
            user_agent = user_agent,
            "Incoming gRPC request"
        );

        request
    }
}

impl Default for LoggingInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for LoggingInterceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggingInterceptor").finish()
    }
}

/// Request validation interceptor
///
/// Validates common request constraints like size limits.
#[derive(Clone)]
pub struct ValidationInterceptor {
    max_request_size_mb: usize,
}

impl ValidationInterceptor {
    #[must_use] 
    pub const fn new(max_request_size_mb: usize) -> Self {
        Self {
            max_request_size_mb,
        }
    }

    /// Validate request size
    #[allow(clippy::result_large_err)]
    pub fn validate<T>(&self, method: &'static str, request: &Request<T>) -> Result<(), Status> {
        // Get content-length if available
        if let Some(content_length) = request.metadata().get("content-length") {
            let length_str = content_length
                .to_str()
                .map_err(|_| Status::invalid_argument("Invalid content-length header"))?;

            if let Ok(size_bytes) = length_str.parse::<usize>() {
                let max_bytes = self.max_request_size_mb * 1024 * 1024;
                if size_bytes > max_bytes {
                    warn!(
                        method = method,
                        size_bytes = size_bytes,
                        max_bytes = max_bytes,
                        "Request too large"
                    );
                    return Err(Status::resource_exhausted(format!(
                        "Request too large: {} bytes (max {} MB)",
                        size_bytes, self.max_request_size_mb
                    )));
                }
            }
        }

        Ok(())
    }
}

impl Debug for ValidationInterceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidationInterceptor")
            .field("max_request_size_mb", &self.max_request_size_mb)
            .finish()
    }
}

/// Shared-secret interceptor for cluster gRPC endpoints.
///
/// Validates that incoming inter-node requests carry the correct shared secret
/// in the `x-cluster-secret` metadata header.
#[derive(Clone)]
pub struct ClusterAuthInterceptor {
    secret: Arc<String>,
}

impl ClusterAuthInterceptor {
    #[must_use]
    pub fn new(secret: String) -> Self {
        Self {
            secret: Arc::new(secret),
        }
    }

    /// Validate the shared secret from request metadata
    #[allow(clippy::result_large_err)]
    pub fn validate<T>(&self, request: Request<T>) -> Result<Request<T>, Status> {
        let token = request
            .metadata()
            .get("x-cluster-secret")
            .ok_or_else(|| Status::unauthenticated("Missing x-cluster-secret header"))?
            .to_str()
            .map_err(|_| Status::unauthenticated("Invalid x-cluster-secret header"))?;

        if !constant_time_eq(token.as_bytes(), self.secret.as_bytes()) {
            warn!("Cluster gRPC auth failed: invalid secret");
            return Err(Status::unauthenticated("Invalid cluster secret"));
        }

        Ok(request)
    }
}

impl Debug for ClusterAuthInterceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterAuthInterceptor").finish()
    }
}

/// Rate limit tier for gRPC services, aligned with HTTP middleware tiers.
///
/// Each tier defines a maximum number of requests per window (default 60s).
/// This prevents attackers from bypassing HTTP rate limits via the gRPC API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrpcRateLimitTier {
    /// Authentication endpoints (Login, Register, RefreshToken): 5 req/min
    Auth,
    /// Media mutation endpoints (AddMedia, RemoveMedia, BatchAdd): 20 req/min
    Media,
    /// Write endpoints (CreateRoom, UpdateRoom, JoinRoom, SendChat): 30 req/min
    Write,
    /// Read endpoints (GetRoom, ListRooms, GetUser, GetPlaylist): 100 req/min
    Read,
    /// Admin endpoints: 30 req/min
    Admin,
    /// Email endpoints (SendVerification, PasswordReset): 5 req/min
    Email,
}

impl GrpcRateLimitTier {
    /// Maximum requests per window for this tier
    const fn max_requests(self) -> u32 {
        match self {
            Self::Auth => 5,
            Self::Email => 5,
            Self::Media => 20,
            Self::Write => 30,
            Self::Admin => 30,
            Self::Read => 100,
        }
    }

    /// Rate limit key suffix for bucketing per tier
    const fn key_suffix(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Email => "email",
            Self::Media => "media",
            Self::Write => "write",
            Self::Admin => "admin",
            Self::Read => "read",
        }
    }
}

/// gRPC rate limit interceptor using the in-memory governor limiter.
///
/// Applies per-client, per-tier rate limiting at the transport level, matching
/// the HTTP middleware rate limiting tiers. Uses the synchronous
/// in-memory rate limiter since tonic interceptors cannot be async.
///
/// Each gRPC service is registered with a specific `GrpcRateLimitTier`,
/// ensuring that auth endpoints (5/min) cannot be abused at the rate of
/// read endpoints (100/min).
///
/// Rate limit tiers (aligned with HTTP):
/// - Auth endpoints: 5 req/min
/// - Email endpoints: 5 req/min
/// - Media endpoints: 20 req/min
/// - Write endpoints: 30 req/min
/// - Admin endpoints: 30 req/min
/// - Read endpoints: 100 req/min
#[derive(Clone)]
pub struct GrpcRateLimitInterceptor {
    rate_limiter: Arc<synctv_core::service::RateLimiter>,
    /// Rate limit tier for this interceptor instance
    tier: GrpcRateLimitTier,
    /// Window in seconds
    window_seconds: u64,
}

impl GrpcRateLimitInterceptor {
    /// Create a new rate limit interceptor for a specific tier.
    ///
    /// Each gRPC service should use its own interceptor with the appropriate tier.
    #[must_use]
    pub fn new(
        rate_limiter: synctv_core::service::RateLimiter,
        tier: GrpcRateLimitTier,
        window_seconds: u64,
    ) -> Self {
        Self {
            rate_limiter: Arc::new(rate_limiter),
            tier,
            window_seconds,
        }
    }

    /// Create a new interceptor instance for a different tier, sharing the
    /// same underlying rate limiter.
    #[must_use]
    pub fn with_tier(&self, tier: GrpcRateLimitTier) -> Self {
        Self {
            rate_limiter: Arc::clone(&self.rate_limiter),
            tier,
            window_seconds: self.window_seconds,
        }
    }

    /// Extract a stable client identifier from the request.
    ///
    /// Priority:
    /// 1. SHA-256 hash of JWT bearer token (authenticated users)
    /// 2. Peer IP address (anonymous users)
    /// 3. "anonymous" fallback
    fn extract_client_id<T>(request: &Request<T>) -> String {
        request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
                if s.len() > 7 && (s.starts_with("Bearer ") || s.starts_with("bearer ")) {
                    // M-9: Use SHA-256 hash of full token for stable client identity.
                    let token = &s[7..];
                    let hash = Sha256::digest(token.as_bytes());
                    Some(format!("user:{:x}", hash))
                } else {
                    None
                }
            })
            .or_else(|| {
                // Use peer IP address for anonymous rate limiting instead of a shared bucket
                request.remote_addr().map(|addr| format!("anon:ip:{}", addr.ip()))
            })
            .unwrap_or_else(|| "anon:unknown".to_string())
    }

    /// Apply rate limiting to a gRPC request.
    ///
    /// Uses the tier configured for this interceptor instance to determine the
    /// rate limit. Each client gets independent buckets per tier so that, e.g.,
    /// auth requests (5/min) don't consume read quota (100/min).
    #[allow(clippy::result_large_err)]
    pub fn check<T>(&self, request: Request<T>) -> Result<Request<T>, Status> {
        let client_id = Self::extract_client_id(&request);

        // Include tier in the rate limit key so each tier has its own bucket
        let rate_key = format!("{}:{}", client_id, self.tier.key_suffix());

        if let Err(_e) = self.rate_limiter.check_rate_limit_sync(
            &rate_key,
            self.tier.max_requests(),
            self.window_seconds,
        ) {
            warn!(
                client_id = %client_id,
                tier = ?self.tier,
                max_requests = self.tier.max_requests(),
                "gRPC rate limit exceeded"
            );
            return Err(Status::resource_exhausted(
                "Rate limit exceeded. Please retry later.",
            ));
        }

        Ok(request)
    }
}

impl Debug for GrpcRateLimitInterceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcRateLimitInterceptor")
            .field("tier", &self.tier)
            .field("max_requests", &self.tier.max_requests())
            .field("window_seconds", &self.window_seconds)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_raw_token_valid() {
        let mut metadata = tonic::metadata::MetadataMap::new();
        metadata.insert("authorization", "Bearer my_jwt_token_here".parse().unwrap());
        let token = AuthInterceptor::extract_raw_token(&metadata).unwrap();
        assert_eq!(token, "my_jwt_token_here");
    }

    #[test]
    fn test_extract_raw_token_missing_header() {
        let metadata = tonic::metadata::MetadataMap::new();
        let result = AuthInterceptor::extract_raw_token(&metadata);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_extract_raw_token_no_bearer_prefix() {
        let mut metadata = tonic::metadata::MetadataMap::new();
        metadata.insert("authorization", "Basic dXNlcjpwYXNz".parse().unwrap());
        let result = AuthInterceptor::extract_raw_token(&metadata);
        assert!(result.is_err());
    }

    #[test]
    fn test_constant_time_eq_equal() {
        assert!(constant_time_eq(b"secret", b"secret"));
    }

    #[test]
    fn test_constant_time_eq_not_equal() {
        assert!(!constant_time_eq(b"secret", b"Secret"));
    }

    #[test]
    fn test_constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"short", b"longer_string"));
    }

    #[test]
    fn test_user_context_has_raw_token() {
        let ctx = UserContext {
            user_id: "user1".to_string(),
            iat: 1234567890,
            raw_token: "token123".to_string(),
        };
        assert_eq!(ctx.raw_token, "token123");
        assert_eq!(ctx.user_id, "user1");
        assert_eq!(ctx.iat, 1234567890);
    }
}
