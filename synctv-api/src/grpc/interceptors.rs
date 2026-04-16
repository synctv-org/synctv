use sha2::{Digest, Sha256};
use std::fmt::Debug;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use synctv_core::service::{auth::JwtService, AuthenticatedToken};
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

fn authorization_metadata_looks_like_bearer<T: Debug>(request: &Request<T>) -> bool {
    request
        .metadata()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim_start()
                .get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer "))
        })
}

/// User context - contains `user_id` and `iat` extracted from JWT
/// Used by `UserService` and `AdminService` methods
#[derive(Debug, Clone)]
pub struct UserContext {
    pub user_id: String,
    /// Token issued-at timestamp (Unix seconds)
    pub iat: i64,
    /// Password version from JWT claims, used for password-change invalidation
    pub pv: i32,
}

/// Room context for room-scoped gRPC operations.
#[derive(Debug, Clone)]
pub struct RoomContext {
    pub room_id: String,
}

/// gRPC auth interceptor that consumes the authenticated identity produced by
/// `BlacklistCheckLayer` and exposes transport-agnostic request context.
#[derive(Clone)]
pub struct AuthInterceptor;

impl AuthInterceptor {
    #[must_use]
    pub fn new(_jwt_service: JwtService) -> Self {
        Self
    }

    /// Inject `UserContext` using the authenticated token produced by `BlacklistCheckLayer`.
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
    pub fn inject_user<T: std::fmt::Debug>(
        &self,
        mut request: Request<T>,
    ) -> Result<Request<T>, Status> {
        let authenticated_token = Self::require_authenticated_token(&request)?;
        let claims = &authenticated_token.claims;

        // Inject UserContext with user_id, iat, pv
        let user_context = UserContext {
            user_id: authenticated_token.user_id.as_str().to_string(),
            iat: claims.iat,
            pv: claims.pv,
        };
        request.extensions_mut().insert(user_context);

        Ok(request)
    }

    /// Inject `RoomContext` using the authenticated token and `x-room-id` metadata.
    /// Used for room-scoped gRPC operations.
    #[allow(clippy::result_large_err)]
    pub fn inject_room<T: std::fmt::Debug>(
        &self,
        mut request: Request<T>,
    ) -> Result<Request<T>, Status> {
        let authenticated_token = Self::require_authenticated_token(&request)?;
        let claims = &authenticated_token.claims;
        let room_id_str = request
            .metadata()
            .get("x-room-id")
            .ok_or_else(|| Status::invalid_argument("Missing x-room-id header"))?
            .to_str()
            .map_err(|_| Status::invalid_argument("Invalid x-room-id header"))?;
        let room_id = crate::room_id_validation::parse_room_id(room_id_str)
            .map_err(|e| Status::invalid_argument(format!("Invalid room_id: {e}")))?;

        let user_context = UserContext {
            user_id: authenticated_token.user_id.as_str().to_string(),
            iat: claims.iat,
            pv: claims.pv,
        };
        request.extensions_mut().insert(user_context);
        request
            .extensions_mut()
            .insert(RoomContext { room_id: room_id.0 });

        Ok(request)
    }

    /// Read the authenticated token that must be produced by `BlacklistCheckLayer`
    /// before the interceptor runs.
    #[allow(clippy::result_large_err)]
    fn require_authenticated_token<T: std::fmt::Debug>(
        request: &Request<T>,
    ) -> Result<AuthenticatedToken, Status> {
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

        if let Some(authenticated_token) = request.extensions().get::<AuthenticatedToken>() {
            return Ok(authenticated_token.clone());
        }

        if authorization_metadata_looks_like_bearer(request) {
            tracing::error!(
                "AuthInterceptor called with authorization metadata but without AuthenticatedToken. \
                 BlacklistCheckLayer must propagate authenticated identity before AuthInterceptor."
            );
            return Err(Status::internal(
                "Server misconfiguration: authenticated token missing",
            ));
        }

        Err(Status::unauthenticated("Missing authorization header"))
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
    use synctv_core::models::UserId;
    use synctv_core::service::{Claims, TokenType};

    fn test_authenticated_token(user_id: &UserId) -> AuthenticatedToken {
        AuthenticatedToken {
            user_id: user_id.clone(),
            claims: Claims {
                sub: user_id.as_str().to_string(),
                typ: "access".to_string(),
                jti: "test-jti".to_string(),
                iat: 1_700_000_000,
                exp: 1_700_003_600,
                pv: 7,
                iss: None,
                aud: None,
            },
        }
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
            iat: 1_234_567_890,
            pv: 3,
        };
        assert_eq!(ctx.user_id, "user1");
        assert_eq!(ctx.iat, 1_234_567_890);
        assert_eq!(ctx.pv, 3);
    }

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
        let user_id = synctv_core::models::UserId::new();
        request
            .extensions_mut()
            .insert(test_authenticated_token(&user_id));

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
    fn test_inject_user_rejects_when_authenticated_token_missing_after_security_layer() {
        let jwt_service =
            synctv_core::service::auth::JwtService::new("test-secret-key-for-testing-1234567890")
                .expect("Should create JwtService");

        let mut request = tonic::Request::new(());
        request.extensions_mut().insert(SecurityCheckPassed);

        let user_id = synctv_core::models::UserId::new();
        let token = jwt_service
            .sign_token(&user_id, TokenType::Access, 0)
            .expect("Should sign token");
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());

        let interceptor = AuthInterceptor::new(jwt_service);
        let result = interceptor.inject_user(request);
        assert!(result.is_err(), "Missing authenticated token should fail");
        let err = result.unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
        assert!(err.message().contains("authenticated token missing"));
    }

    #[test]
    fn test_inject_user_treats_non_bearer_authorization_as_missing_auth() {
        let jwt_service =
            synctv_core::service::auth::JwtService::new("test-secret-key-for-testing-1234567890")
                .expect("Should create JwtService");

        let mut request = tonic::Request::new(());
        request.extensions_mut().insert(SecurityCheckPassed);
        request
            .metadata_mut()
            .insert("authorization", "Basic dXNlcjpwYXNz".parse().unwrap());

        let interceptor = AuthInterceptor::new(jwt_service);
        let result = interceptor.inject_user(request);
        assert!(
            result.is_err(),
            "Non-bearer authorization should not look authenticated"
        );
        let err = result.unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
        assert_eq!(err.message(), "Missing authorization header");
    }

    #[test]
    fn test_inject_user_uses_authenticated_token_extension_without_revalidating_metadata() {
        let jwt_service =
            synctv_core::service::auth::JwtService::new("test-secret-key-for-testing-1234567890")
                .expect("Should create JwtService");
        let interceptor = AuthInterceptor::new(jwt_service);

        let user_id = synctv_core::models::UserId::new();
        let expected = test_authenticated_token(&user_id);

        let mut request = tonic::Request::new(());
        request.extensions_mut().insert(SecurityCheckPassed);
        request.extensions_mut().insert(expected.clone());
        request
            .metadata_mut()
            .insert("authorization", "Bearer invalid.jwt.value".parse().unwrap());

        let request = interceptor
            .inject_user(request)
            .expect("Existing authenticated token should be reused");
        let ctx = request
            .extensions()
            .get::<UserContext>()
            .expect("UserContext should be injected");

        assert_eq!(ctx.user_id, user_id.as_str());
        assert_eq!(ctx.iat, expected.claims.iat);
        assert_eq!(ctx.pv, expected.claims.pv);
    }
}
