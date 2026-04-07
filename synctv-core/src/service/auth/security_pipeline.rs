//! Shared security pipeline for HTTP and gRPC authentication.
//!
//! Both the HTTP `AuthUser` extractor and the gRPC `BlacklistCheckLayer` enforce
//! identical security checks in a fixed order:
//!
//! 1. **JWT verification** -- validate signature, expiration, and access token type
//! 2. **Password invalidation** -- reject tokens issued before a password change
//! 3. **User status** -- reject banned, pending, or soft-deleted users
//! 4. **Access token blacklist** -- reject revoked access tokens (e.g., after logout)
//!
//! This module provides [`SecurityPipeline`] so both transport layers can delegate
//! to a single implementation, preventing divergence.

use std::sync::Arc;

use crate::{
    cache::{KeyBuilder, UserCache},
    models::{UserId, UserStatus},
    service::UserService,
    Error, Result,
};

use super::{Claims, TokenBlacklistStore};

const AUTHENTICATION_FAILED_MESSAGE: &str = "Authentication failed";

/// Configuration for access token blacklist enforcement.
#[derive(Debug, Clone, Copy)]
pub struct BlacklistEnforcement {
    /// If true, the pipeline will reject requests when the blacklist store
    /// is not configured. This ensures that revoked access tokens cannot
    /// bypass the blacklist check.
    ///
    /// When false, a missing blacklist store is logged as a warning
    /// but the request is allowed to proceed (for development/testing only).
    ///
    /// Default is true for production safety. Set to false only in
    /// development environments where token blacklist is not needed.
    pub require_blacklist: bool,
}

impl BlacklistEnforcement {
    /// Create a new `BlacklistEnforcement` with default values.
    ///
    /// By default, `require_blacklist` is true to ensure logout tokens
    /// are properly invalidated in production deployments.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            require_blacklist: true,
        }
    }

    /// Create a `BlacklistEnforcement` that allows requests without blacklist store.
    ///
    /// Use this only for development/testing where token blacklist is not needed.
    #[must_use]
    pub const fn permissive() -> Self {
        Self {
            require_blacklist: false,
        }
    }
}

impl Default for BlacklistEnforcement {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of a successful security pipeline check.
#[derive(Debug, Clone)]
pub struct AuthenticatedToken {
    pub user_id: UserId,
    pub claims: Claims,
}

/// Shared security pipeline that performs the post-JWT security checks.
///
/// Step 1 (JWT verification) is intentionally left to the caller because
/// the HTTP and gRPC layers extract the raw token differently. Once the
/// caller has valid [`Claims`], it passes them here for steps 2-4.
///
/// When a [`UserCache`] is provided via [`with_user_cache`], the pipeline
/// consults the cache first to fast-reject obviously invalid requests and
/// still populates the cache after successful DB checks to reduce repeated
/// database reads. Successful authentication, however, is always confirmed
/// from the database because cross-replica cache invalidation is best-effort.
#[derive(Clone)]
pub struct SecurityPipeline {
    user_service: Arc<UserService>,
    /// Optional user cache for fast path lookups (avoids DB query on cache hit).
    user_cache: Option<Arc<UserCache>>,
    /// Optional access token blacklist store for checking revoked access tokens.
    token_blacklist: Option<Arc<dyn TokenBlacklistStore>>,
    /// Optional key builder for constructing blacklist keys.
    key_builder: Option<KeyBuilder>,
    /// Enforcement policy for access token blacklist checks.
    blacklist_enforcement: BlacklistEnforcement,
}

impl SecurityPipeline {
    #[must_use]
    pub const fn classify_auth_error(err: &Error) -> AuthErrorCategory {
        match err {
            Error::Authentication(_) => AuthErrorCategory::Authentication,
            Error::Authorization(_) | Error::EmailNotVerified => AuthErrorCategory::Authorization,
            Error::ServiceUnavailable(_)
            | Error::Database(_)
            | Error::Redis(_)
            | Error::Timeout(_) => AuthErrorCategory::Unavailable,
            _ => AuthErrorCategory::Internal,
        }
    }

    /// Create a new security pipeline.
    #[must_use]
    pub const fn new(user_service: Arc<UserService>) -> Self {
        Self {
            user_service,
            user_cache: None,
            token_blacklist: None,
            key_builder: None,
            blacklist_enforcement: BlacklistEnforcement::new(),
        }
    }

    /// Attach a [`UserCache`] to this pipeline.
    ///
    /// When set, [`check`] will consult the cache before hitting the database
    /// so it can fast-reject stale or invalid sessions. The cache is also
    /// populated after successful DB confirmation.
    #[must_use]
    pub fn with_user_cache(mut self, user_cache: Arc<UserCache>) -> Self {
        self.user_cache = Some(user_cache);
        self
    }

    /// Attach a [`TokenBlacklistStore`] and [`KeyBuilder`] to this pipeline.
    ///
    /// When set, [`check`] will verify that the access token's JTI has not
    /// been blacklisted (e.g. via logout) before allowing the request.
    #[must_use]
    pub fn with_token_blacklist(
        mut self,
        store: Arc<dyn TokenBlacklistStore>,
        key_builder: KeyBuilder,
    ) -> Self {
        self.token_blacklist = Some(store);
        self.key_builder = Some(key_builder);
        self
    }

    /// Configure blacklist enforcement policy.
    ///
    /// When `require_blacklist` is true, the pipeline will reject requests if
    /// the blacklist store is not configured. This prevents revoked access
    /// tokens from bypassing the blacklist check.
    #[must_use]
    pub const fn with_blacklist_enforcement(mut self, enforcement: BlacklistEnforcement) -> Self {
        self.blacklist_enforcement = enforcement;
        self
    }

    /// Check if the blacklist store is configured.
    #[must_use]
    pub fn has_blacklist_store(&self) -> bool {
        self.token_blacklist.is_some() && self.key_builder.is_some()
    }

    /// Check if the fast-path [`UserCache`] is configured.
    #[must_use]
    pub const fn has_user_cache(&self) -> bool {
        self.user_cache.is_some()
    }

    /// Run post-JWT security checks (steps 2-3).
    ///
    /// The caller is responsible for step 1 (JWT verification) and must
    /// provide the validated [`Claims`].
    ///
    /// ## Cache behaviour
    ///
    /// The cache is used as a negative cache:
    /// - Cache hit → reject obviously invalid `password_version` or user status.
    /// - Any allow decision → confirm current state from DB, then populate the cache.
    ///
    /// # Arguments
    /// * `claims` -- the already-verified JWT claims
    ///
    /// # Returns
    /// [`AuthenticatedToken`] on success, or an [`Error::Authentication`] on failure.
    pub async fn check(&self, claims: &Claims) -> Result<AuthenticatedToken> {
        let user_id = claims.user_id();

        if let Some(cache) = &self.user_cache {
            if let Ok(Some(cached)) = cache.get(&user_id).await {
                if cached.status() == UserStatus::Banned
                    || cached.status() == UserStatus::Pending
                    || cached.status() == UserStatus::Rejected
                    || cached.is_deleted()
                    || claims.pv < cached.password_version()
                {
                    return Err(if claims.pv < cached.password_version() {
                        Error::Authentication(
                            "Token invalidated due to password change. Please log in again."
                                .to_string(),
                        )
                    } else {
                        Error::Authentication("Authentication failed".to_string())
                    });
                }
            }
        }

        // Always confirm security-sensitive user state from the database.
        // Cache invalidation is best-effort across replicas, so a cache hit
        // cannot be treated as authoritative for password version, status, or
        // soft-deletion checks.
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|e| match &e {
                Error::NotFound(_) => {
                    Error::Authentication(AUTHENTICATION_FAILED_MESSAGE.to_string())
                }
                _ => e,
            })?;

        // Step 2: Password version check
        if claims.pv < user.password_version {
            return Err(Error::Authentication(
                "Token invalidated due to password change. Please log in again.".to_string(),
            ));
        }

        if user.is_deleted()
            || user.status == UserStatus::Banned
            || user.status == UserStatus::Pending
            || user.status == UserStatus::Rejected
        {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        // Check access token JTI blacklist (e.g. logout)
        self.check_access_token_blacklist(claims).await?;

        // Populate the cache after a successful DB lookup so future requests
        // for this user are served from the cache.
        if let Some(cache) = &self.user_cache {
            let cached_user = crate::cache::user_cache::CachedUser::with_updated_at(
                user.id.as_str().to_string(),
                user.username.clone(),
                user.role,
                user.status,
                user.created_at,
                user.updated_at,
                user.password_version,
                user.is_deleted(),
            );
            // Best-effort: log but do not fail the request if the cache write errors.
            if let Err(e) = cache.set(&user_id, cached_user).await {
                tracing::warn!(
                    error = %e,
                    user_id = %user_id.as_str(),
                    "Failed to populate user cache after DB lookup in SecurityPipeline"
                );
            }
        }

        Ok(AuthenticatedToken {
            user_id,
            claims: claims.clone(),
        })
    }

    /// Check if the access token's JTI has been blacklisted (e.g. via logout).
    ///
    /// ## Behavior
    ///
    /// - If both `token_blacklist` and `key_builder` are configured, the method
    ///   checks if the token's JTI is blacklisted and returns an error if so.
    ///   If the blacklist store encounters a storage error (e.g., database/Redis
    ///   unavailable), the method **fails closed** -- the request is rejected to
    ///   prevent blacklisted tokens from bypassing the check during outages.
    /// - If the blacklist store is not configured and `require_blacklist` is true,
    ///   the method returns an error to prevent bypassing the blacklist check.
    /// - If the blacklist store is not configured and `require_blacklist` is false,
    ///   a warning is logged but the request is allowed to proceed
    ///   (for development/testing only).
    async fn check_access_token_blacklist(&self, claims: &Claims) -> Result<()> {
        // Skip check if JTI is empty (shouldn't happen for valid tokens)
        if claims.jti.is_empty() {
            return Ok(());
        }

        match (&self.token_blacklist, &self.key_builder) {
            (Some(store), Some(kb)) => {
                let key = kb.access_token_blacklist(&claims.jti);
                // Use is_blacklisted_checked to propagate storage errors.
                // On error, fail-closed: treat as blacklisted to prevent bypass
                // during storage outages.
                match store.is_blacklisted_checked(&key).await {
                    Ok(true) => Err(Error::Authentication("Authentication failed".to_string())),
                    Ok(false) => Ok(()),
                    Err(e) => {
                        tracing::error!(
                            user_id = %claims.sub,
                            jti = %claims.jti,
                            error = %e,
                            "Access token blacklist check failed due to storage error (fail-closed)"
                        );
                        Err(Error::ServiceUnavailable(
                            "Authentication service temporarily unavailable".to_string(),
                        ))
                    }
                }
            }
            _ => {
                // Blacklist store not configured
                if self.blacklist_enforcement.require_blacklist {
                    // Fail-closed: reject the request to prevent bypassing blacklist
                    tracing::error!(
                        user_id = %claims.sub,
                        jti = %claims.jti,
                        "Access token blacklist check required but blacklist store not configured"
                    );
                    Err(Error::ServiceUnavailable(
                        "Authentication service misconfigured".to_string(),
                    ))
                } else {
                    // Fail-open: log warning but allow (development/testing only)
                    tracing::warn!(
                        user_id = %claims.sub,
                        jti = %claims.jti,
                        "Access token blacklist check skipped: blacklist store not configured. \
                         Consider enabling require_blacklist for production deployments."
                    );
                    Ok(())
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthErrorCategory {
    Authentication,
    Authorization,
    Unavailable,
    Internal,
}

impl std::fmt::Debug for SecurityPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityPipeline").finish()
    }
}

/// Builder for creating a [`SecurityPipeline`] with validation.
///
/// This builder ensures that the pipeline is properly configured before use.
/// When `require_blacklist` is enabled, the builder will reject configurations
/// that don't include a blacklist store.
///
/// # Example
///
/// ```ignore
/// // This example is ignored because SecurityPipelineBuilder requires multiple dependencies.
/// // In practice, use your dependency injection framework to construct the pipeline.
/// use std::sync::Arc;
/// use synctv_core::service::auth::{SecurityPipelineBuilder, BlacklistEnforcement};
///
/// // Assuming you have:
/// // - user_service: Arc<UserService>
/// // - blacklist_store: Arc<dyn TokenBlacklistStore>
/// // - key_builder: KeyBuilder
///
/// // let pipeline = SecurityPipelineBuilder::new(user_service)
/// //     .with_token_blacklist(blacklist_store, key_builder)
/// //     .with_blacklist_enforcement(BlacklistEnforcement { require_blacklist: true })
/// //     .build()?;
/// ```
pub struct SecurityPipelineBuilder {
    user_service: Arc<UserService>,
    user_cache: Option<Arc<UserCache>>,
    token_blacklist: Option<Arc<dyn TokenBlacklistStore>>,
    key_builder: Option<KeyBuilder>,
    blacklist_enforcement: BlacklistEnforcement,
}

/// Error returned when [`SecurityPipelineBuilder::build`] fails validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityPipelineBuildError {
    /// Human-readable description of what's missing.
    pub message: String,
}

impl std::fmt::Display for SecurityPipelineBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecurityPipeline build error: {}", self.message)
    }
}

impl std::error::Error for SecurityPipelineBuildError {}

// Manual Debug impl for SecurityPipelineBuilder since KeyBuilder and dyn TokenBlacklistStore don't impl Debug
impl std::fmt::Debug for SecurityPipelineBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityPipelineBuilder")
            .field("user_cache", &self.user_cache.is_some())
            .field("token_blacklist", &self.token_blacklist.is_some())
            .field("key_builder", &self.key_builder.is_some())
            .field("blacklist_enforcement", &self.blacklist_enforcement)
            .finish()
    }
}

impl SecurityPipelineBuilder {
    /// Create a new builder with the required user service.
    #[must_use]
    pub fn new(user_service: Arc<UserService>) -> Self {
        Self {
            user_service,
            user_cache: None,
            token_blacklist: None,
            key_builder: None,
            blacklist_enforcement: BlacklistEnforcement::default(),
        }
    }

    /// Attach a [`UserCache`] to the pipeline.
    #[must_use]
    pub fn with_user_cache(mut self, user_cache: Arc<UserCache>) -> Self {
        self.user_cache = Some(user_cache);
        self
    }

    /// Attach a [`TokenBlacklistStore`] and [`KeyBuilder`] to the pipeline.
    #[must_use]
    pub fn with_token_blacklist(
        mut self,
        store: Arc<dyn TokenBlacklistStore>,
        key_builder: KeyBuilder,
    ) -> Self {
        self.token_blacklist = Some(store);
        self.key_builder = Some(key_builder);
        self
    }

    /// Configure blacklist enforcement policy.
    #[must_use]
    pub const fn with_blacklist_enforcement(mut self, enforcement: BlacklistEnforcement) -> Self {
        self.blacklist_enforcement = enforcement;
        self
    }

    /// Build the [`SecurityPipeline`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] if:
    /// - `require_blacklist` is true but no blacklist store is configured
    /// - Only one of `token_blacklist` or `key_builder` is set (partial configuration)
    pub fn build(self) -> crate::Result<SecurityPipeline> {
        // Validate blacklist configuration consistency
        let has_blacklist = self.token_blacklist.is_some();
        let has_key_builder = self.key_builder.is_some();

        match (has_blacklist, has_key_builder) {
            (true, false) | (false, true) => {
                return Err(crate::Error::Internal(
                    "Incomplete blacklist configuration: both token_blacklist and key_builder must be set together".to_string(),
                ));
            }
            _ => {}
        }

        // Validate require_blacklist constraint
        if self.blacklist_enforcement.require_blacklist && !has_blacklist {
            return Err(crate::Error::Internal(
                "require_blacklist is enabled but no TokenBlacklistStore is configured. \
                          Either provide a blacklist store via with_token_blacklist() or \
                          disable require_blacklist."
                    .to_string(),
            ));
        }

        Ok(SecurityPipeline {
            user_service: self.user_service,
            user_cache: self.user_cache,
            token_blacklist: self.token_blacklist,
            key_builder: self.key_builder,
            blacklist_enforcement: self.blacklist_enforcement,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use async_trait::async_trait;
    use sqlx::PgPool;

    use crate::{
        cache::{KeyBuilder, NoopCacheL2, UsernameCache},
        config::PasswordComplexityConfig,
        service::{
            auth::{BruteForceProtection, JwtService},
            InMemoryTokenBlacklistStore, UserService,
        },
    };

    // ========================================================================
    // BlacklistEnforcement behavior tests
    // ========================================================================

    #[test]
    fn blacklist_enforcement_default_is_true() {
        // By default, require_blacklist should be true for production safety
        let enforcement = BlacklistEnforcement::default();
        assert!(enforcement.require_blacklist);
    }

    #[test]
    fn blacklist_enforcement_new_is_true() {
        let enforcement = BlacklistEnforcement::new();
        assert!(enforcement.require_blacklist);
    }

    #[test]
    fn blacklist_enforcement_permissive_is_false() {
        let enforcement = BlacklistEnforcement::permissive();
        assert!(!enforcement.require_blacklist);
    }

    struct FailingBlacklistStore;

    #[async_trait]
    impl TokenBlacklistStore for FailingBlacklistStore {
        async fn is_blacklisted_checked(&self, _key: &str) -> Result<bool> {
            Err(Error::Redis(redis::RedisError::from(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "blacklist backend unavailable",
            ))))
        }

        async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> Result<()> {
            Ok(())
        }

        async fn get_family_revoked_at(&self, _key: &str) -> Option<i64> {
            None
        }

        async fn set_family_revoked(
            &self,
            _key: &str,
            _timestamp: i64,
            _ttl_secs: u64,
        ) -> Result<()> {
            Ok(())
        }
    }

    fn create_user_service(pool: PgPool) -> Arc<UserService> {
        let jwt_service =
            JwtService::new("test-secret-key-for-security-pipeline-unit-tests-min-32")
                .expect("failed to create jwt service");
        let username_cache =
            UsernameCache::new(Arc::new(NoopCacheL2), "test:username:".to_string(), 100, 0);
        let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
        let key_builder = KeyBuilder::new("test");
        let brute_force = BruteForceProtection::in_memory("test".to_string());

        Arc::new(UserService::new(
            pool,
            jwt_service,
            username_cache,
            PasswordComplexityConfig::default(),
            token_blacklist,
            key_builder,
            brute_force,
        ))
    }

    fn make_claims(user_id: &str, pv: i32) -> Claims {
        let now = chrono::Utc::now();
        Claims {
            sub: user_id.to_string(),
            typ: "access".to_string(),
            jti: "test-jti".to_string(),
            iat: now.timestamp(),
            exp: (now + chrono::Duration::hours(1)).timestamp(),
            pv,
            iss: None,
            aud: None,
        }
    }

    #[tokio::test]
    async fn blacklist_storage_error_is_service_unavailable() {
        let pool = PgPool::connect_lazy("postgres://localhost/synctv")
            .expect("lazy pool should build without network");
        let pipeline = SecurityPipeline::new(create_user_service(pool))
            .with_token_blacklist(Arc::new(FailingBlacklistStore), KeyBuilder::new("test"))
            .with_blacklist_enforcement(BlacklistEnforcement::new());

        let err = pipeline
            .check_access_token_blacklist(&make_claims("user-1", 0))
            .await
            .expect_err("blacklist storage failures must fail closed");

        assert!(
            matches!(&err, Error::ServiceUnavailable(msg) if msg.contains("temporarily unavailable")),
            "expected ServiceUnavailable on blacklist storage failure, got: {err}"
        );
        assert_eq!(
            SecurityPipeline::classify_auth_error(&err),
            AuthErrorCategory::Unavailable
        );
    }

    #[test]
    fn classify_auth_error_preserves_transport_semantics() {
        assert_eq!(
            SecurityPipeline::classify_auth_error(&Error::Authentication(
                AUTHENTICATION_FAILED_MESSAGE.to_string()
            )),
            AuthErrorCategory::Authentication
        );
        assert_eq!(
            SecurityPipeline::classify_auth_error(&Error::Authorization("denied".to_string())),
            AuthErrorCategory::Authorization
        );
        assert_eq!(
            SecurityPipeline::classify_auth_error(&Error::ServiceUnavailable(
                "backend unavailable".to_string()
            )),
            AuthErrorCategory::Unavailable
        );
        assert_eq!(
            SecurityPipeline::classify_auth_error(&Error::Internal("boom".to_string())),
            AuthErrorCategory::Internal
        );
    }
}
