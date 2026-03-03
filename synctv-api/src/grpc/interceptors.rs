use sha2::{Digest, Sha256};
use std::fmt::Debug;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use synctv_core::service::auth::{JwtService, JwtValidator};
use tonic::{Request, Status};
use tracing::warn;

/// Marker type injected by `BlacklistCheckLayer` after security checks pass.
///
/// This marker is used to enforce layer ordering at runtime. `AuthInterceptor`
/// checks for this marker and rejects requests if it's missing, ensuring that
/// the security pipeline (JWT verification, blacklist check, banned user check)
/// has run before the interceptor extracts user context.
///
/// # Security
///
/// If `BlacklistCheckLayer` does not run before `AuthInterceptor`, revoked tokens
/// or banned users could bypass security checks. This marker prevents that by
/// failing fast with an internal error when the layer ordering is incorrect.
#[derive(Debug, Clone, Copy)]
pub struct SecurityCheckPassed;

/// Constant-time secret comparison to prevent timing attacks.
///
/// Both inputs are hashed to fixed-length SHA-256 digests before comparison,
/// so the execution time is independent of input lengths. This prevents
/// leaking whether the attacker's input matches the secret's length.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let hash_a = Sha256::digest(a);
    let hash_b = Sha256::digest(b);
    hash_a.ct_eq(&hash_b).into()
}

/// User context - contains `user_id` and `iat` extracted from JWT
/// Used by `UserService` and `AdminService` methods
#[derive(Debug, Clone)]
pub struct UserContext {
    pub user_id: String,
    /// Token issued-at timestamp (Unix seconds), used for password-change invalidation (legacy fallback)
    pub iat: i64,
    /// Password version from JWT claims, used for password-change invalidation
    pub pv: Option<i32>,
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
    ///
    /// # Layer Ordering (RUNTIME CHECK)
    ///
    /// This method requires `SecurityCheckPassed` marker in request extensions,
    /// which is injected by `BlacklistCheckLayer`. If the marker is missing,
    /// this method returns an internal error to indicate misconfigured layer ordering.
    ///
    /// The security checks performed by `BlacklistCheckLayer` include:
    /// 1. JWT verification (signature, expiration, access token type)
    /// 2. Password invalidation check (tokens issued before password change)
    /// 3. Banned/deleted user check
    #[allow(clippy::result_large_err)]
    pub fn inject_user<T>(&self, mut request: Request<T>) -> Result<Request<T>, Status> {
        // RUNTIME CHECK: Verify BlacklistCheckLayer has run before this interceptor.
        // This prevents security bypass if layer ordering is misconfigured.
        if request.extensions().get::<SecurityCheckPassed>().is_none() {
            tracing::error!(
                "AuthInterceptor called without SecurityCheckPassed marker. \
                 BlacklistCheckLayer must run before AuthInterceptor. \
                 Check gRPC server layer ordering in grpc/mod.rs."
            );
            return Err(Status::internal(
                "Server misconfiguration: security layer ordering error",
            ));
        }

        // Extract and validate the bearer token
        let raw_token = Self::extract_raw_token(request.metadata())?;

        let claims = self
            .jwt_validator
            .validate_token(&raw_token)
            .map_err(|e| Status::unauthenticated(format!("Token verification failed: {e}")))?;

        // Inject UserContext with user_id, iat, pv
        let user_context = UserContext {
            user_id: claims.sub,
            iat: claims.iat,
            pv: claims.pv,
        };
        request.extensions_mut().insert(user_context);

        Ok(request)
    }

    /// Inject `RoomContext` - validates JWT, extracts `user_id` and `room_id` from x-room-id header
    /// Used for `RoomService` and `MediaService`
    ///
    /// # Layer Ordering (RUNTIME CHECK)
    ///
    /// This method requires `SecurityCheckPassed` marker in request extensions,
    /// which is injected by `BlacklistCheckLayer`. If the marker is missing,
    /// this method returns an internal error to indicate misconfigured layer ordering.
    ///
    /// The room_id is validated against the same rules as HTTP endpoints:
    /// - Must not be empty
    /// - Must not exceed 64 characters (ID_MAX limit)
    /// - Must contain only alphanumeric characters, underscores, and hyphens
    #[allow(clippy::result_large_err)]
    pub fn inject_room<T>(&self, mut request: Request<T>) -> Result<Request<T>, Status> {
        // RUNTIME CHECK: Verify BlacklistCheckLayer has run before this interceptor.
        // This prevents security bypass if layer ordering is misconfigured.
        if request.extensions().get::<SecurityCheckPassed>().is_none() {
            tracing::error!(
                "AuthInterceptor called without SecurityCheckPassed marker. \
                 BlacklistCheckLayer must run before AuthInterceptor. \
                 Check gRPC server layer ordering in grpc/mod.rs."
            );
            return Err(Status::internal(
                "Server misconfiguration: security layer ordering error",
            ));
        }

        // Extract and validate the bearer token
        let raw_token = Self::extract_raw_token(request.metadata())?;

        let claims = self
            .jwt_validator
            .validate_token(&raw_token)
            .map_err(|e| Status::unauthenticated(format!("Token verification failed: {e}")))?;

        // Extract room_id from x-room-id header
        let room_id_str = request
            .metadata()
            .get("x-room-id")
            .ok_or_else(|| Status::invalid_argument("Missing x-room-id header"))?
            .to_str()
            .map_err(|_| Status::invalid_argument("Invalid x-room-id header"))?;

        // Validate room_id format (same rules as HTTP layer)
        let room_id = crate::room_id_validation::parse_room_id(room_id_str)
            .map_err(|e| Status::invalid_argument(format!("Invalid room_id: {e}")))?;

        // Inject UserContext (for nested structure)
        let user_context = UserContext {
            user_id: claims.sub.clone(),
            iat: claims.iat,
            pv: claims.pv,
        };
        request.extensions_mut().insert(user_context);

        // Inject RoomContext
        let room_context = RoomContext {
            user_ctx: UserContext {
                user_id: claims.sub,
                iat: claims.iat,
                pv: claims.pv,
            },
            room_id: room_id.0,
        };
        request.extensions_mut().insert(room_context);

        Ok(request)
    }

    /// Attempt to extract and validate a user ID from gRPC metadata without requiring auth.
    ///
    /// Returns `Some(UserId)` if a valid Bearer token is present in the `authorization`
    /// metadata header. Returns `None` if no header is present or if the token is invalid.
    ///
    /// This is used by endpoints that support optional authentication (e.g. `OAuth2` exchange
    /// for bind flows — login flows need no auth, but bind flows require the caller to prove
    /// their identity).
    #[must_use]
    pub fn try_extract_user_id(
        &self,
        metadata: &tonic::metadata::MetadataMap,
    ) -> Option<synctv_core::models::UserId> {
        self.jwt_validator
            .validate_grpc_extract_user_id(metadata)
            .ok()
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

/// Extract `user_id` from `UserContext` (injected by `inject_user` interceptor).
///
/// Shared helper to avoid duplicating this pattern across gRPC service files.
#[allow(clippy::result_large_err)]
pub fn extract_user_id<T: std::fmt::Debug>(
    request: &Request<T>,
) -> Result<synctv_core::models::UserId, Status> {
    let user_context = request
        .extensions()
        .get::<UserContext>()
        .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
    Ok(synctv_core::models::UserId::from_string(
        user_context.user_id.clone(),
    ))
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
    /// Media mutation endpoints (`AddMedia`, `RemoveMedia`, BatchAdd): 20 req/min
    Media,
    /// Write endpoints (`CreateRoom`, `UpdateRoom`, `JoinRoom`, SendChat): 30 req/min
    Write,
    /// Read endpoints (`GetRoom`, `ListRooms`, `GetUser`, GetPlaylist): 100 req/min
    Read,
    /// Admin endpoints: 30 req/min
    Admin,
    /// Email endpoints (`SendVerification`, PasswordReset): 5 req/min
    Email,
}

impl GrpcRateLimitTier {
    /// Maximum requests per window for this tier, read from config.
    ///
    /// Configurable via `grpc_rate_limits` section in the config file or
    /// `SYNCTV_GRPC_RATE_LIMITS_*` environment variables.
    ///
    /// The async `GrpcRateLimitLayer` (tower middleware) uses a Redis-backed
    /// distributed limiter that shares a single counter across all replicas,
    /// so the configured value IS the global limit.
    pub(crate) const fn max_requests(self, config: &synctv_core::GrpcRateLimitConfig) -> u32 {
        match self {
            Self::Auth => config.auth_max_requests,
            Self::Email => config.email_max_requests,
            Self::Media => config.media_max_requests,
            Self::Write => config.write_max_requests,
            Self::Admin => config.admin_max_requests,
            Self::Read => config.read_max_requests,
        }
    }

    /// Window duration in seconds for this tier, read from config.
    pub(crate) const fn window_seconds(self, config: &synctv_core::GrpcRateLimitConfig) -> u64 {
        match self {
            Self::Auth => config.auth_window_seconds,
            Self::Email => config.email_window_seconds,
            Self::Media => config.media_window_seconds,
            Self::Write => config.write_window_seconds,
            Self::Admin => config.admin_window_seconds,
            Self::Read => config.read_window_seconds,
        }
    }

    /// Rate limit key suffix for bucketing per tier
    pub(crate) const fn key_suffix(self) -> &'static str {
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
    fn test_constant_time_eq_empty_inputs() {
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"", b"notempty"));
        assert!(!constant_time_eq(b"notempty", b""));
    }

    #[test]
    fn test_user_context_fields() {
        let ctx = UserContext {
            user_id: "user1".to_string(),
            iat: 1234567890,
            pv: Some(3),
        };
        assert_eq!(ctx.user_id, "user1");
        assert_eq!(ctx.iat, 1234567890);
        assert_eq!(ctx.pv, Some(3));
    }

    // ========== SecurityCheckPassed Marker Tests ==========

    #[test]
    fn test_security_check_passed_marker_exists() {
        // Verify the marker type exists and can be created
        let marker = SecurityCheckPassed;
        assert!(format!("{marker:?}").contains("SecurityCheckPassed"));
    }

    #[test]
    fn test_inject_user_rejects_without_security_check_marker() {
        // TDD test: inject_user MUST reject requests without SecurityCheckPassed marker
        // This ensures layer ordering is enforced at runtime
        let jwt_service =
            synctv_core::service::auth::JwtService::new("test-secret-key-for-testing-1234567890")
                .expect("Should create JwtService");

        let mut request = tonic::Request::new(());
        // Add a valid token (but no SecurityCheckPassed marker)
        let user_id = synctv_core::models::UserId::new();
        let token = jwt_service
            .sign_token(&user_id, synctv_core::service::auth::TokenType::Access, 0)
            .expect("Should sign token");
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());

        // Clone jwt_service before moving to AuthInterceptor
        let interceptor = AuthInterceptor::new(jwt_service);

        // Should fail with internal error because SecurityCheckPassed marker is missing
        let result = interceptor.inject_user(request);
        assert!(
            result.is_err(),
            "Should reject without SecurityCheckPassed marker"
        );
        let err = result.unwrap_err();
        assert_eq!(
            err.code(),
            tonic::Code::Internal,
            "Should return Internal status"
        );
        assert!(
            err.message().contains("misconfiguration"),
            "Error should mention misconfiguration"
        );
    }

    #[test]
    fn test_inject_user_accepts_with_security_check_marker() {
        // TDD test: inject_user MUST accept requests with SecurityCheckPassed marker
        let jwt_service =
            synctv_core::service::auth::JwtService::new("test-secret-key-for-testing-1234567890")
                .expect("Should create JwtService");

        let mut request = tonic::Request::new(());
        // Add the SecurityCheckPassed marker (simulating BlacklistCheckLayer)
        request.extensions_mut().insert(SecurityCheckPassed);
        // Add a valid token
        let user_id = synctv_core::models::UserId::new();
        let token = jwt_service
            .sign_token(&user_id, synctv_core::service::auth::TokenType::Access, 0)
            .expect("Should sign token");
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());

        // Clone jwt_service before moving to AuthInterceptor
        let interceptor = AuthInterceptor::new(jwt_service);

        // Should succeed because SecurityCheckPassed marker is present
        let result = interceptor.inject_user(request);
        assert!(
            result.is_ok(),
            "Should accept with SecurityCheckPassed marker"
        );
    }

    #[test]
    fn test_inject_room_rejects_without_security_check_marker() {
        // TDD test: inject_room MUST reject requests without SecurityCheckPassed marker
        let jwt_service =
            synctv_core::service::auth::JwtService::new("test-secret-key-for-testing-1234567890")
                .expect("Should create JwtService");

        let mut request = tonic::Request::new(());
        // Add a valid token and room_id (but no SecurityCheckPassed marker)
        let user_id = synctv_core::models::UserId::new();
        let token = jwt_service
            .sign_token(&user_id, synctv_core::service::auth::TokenType::Access, 0)
            .expect("Should sign token");
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        request
            .metadata_mut()
            .insert("x-room-id", "test-room-123".parse().unwrap());

        // Clone jwt_service before moving to AuthInterceptor
        let interceptor = AuthInterceptor::new(jwt_service);

        // Should fail with internal error because SecurityCheckPassed marker is missing
        let result = interceptor.inject_room(request);
        assert!(
            result.is_err(),
            "Should reject without SecurityCheckPassed marker"
        );
        let err = result.unwrap_err();
        assert_eq!(
            err.code(),
            tonic::Code::Internal,
            "Should return Internal status"
        );
        assert!(
            err.message().contains("misconfiguration"),
            "Error should mention misconfiguration"
        );
    }

    #[test]
    fn test_inject_room_accepts_with_security_check_marker() {
        // TDD test: inject_room MUST accept requests with SecurityCheckPassed marker
        let jwt_service =
            synctv_core::service::auth::JwtService::new("test-secret-key-for-testing-1234567890")
                .expect("Should create JwtService");

        let mut request = tonic::Request::new(());
        // Add the SecurityCheckPassed marker (simulating BlacklistCheckLayer)
        request.extensions_mut().insert(SecurityCheckPassed);
        // Add a valid token and room_id
        let user_id = synctv_core::models::UserId::new();
        let token = jwt_service
            .sign_token(&user_id, synctv_core::service::auth::TokenType::Access, 0)
            .expect("Should sign token");
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        request
            .metadata_mut()
            .insert("x-room-id", "test-room-123".parse().unwrap());

        // Clone jwt_service before moving to AuthInterceptor
        let interceptor = AuthInterceptor::new(jwt_service);

        // Should succeed because SecurityCheckPassed marker is present
        let result = interceptor.inject_room(request);
        assert!(
            result.is_ok(),
            "Should accept with SecurityCheckPassed marker"
        );
    }
}
