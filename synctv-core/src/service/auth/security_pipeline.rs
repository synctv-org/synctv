//! Shared security pipeline for authenticated requests.
//!
//! Request entrypoints enforce identical security checks in a fixed order:
//!
//! 1. **JWT verification** -- validate signature, expiration, and access token type
//! 2. **Password invalidation** -- reject tokens issued before a password change
//! 3. **User status** -- reject banned, pending, or soft-deleted users
//! 4. **2FA context** -- reject local single-factor tokens while 2FA is enabled
//! 5. **Access token blacklist** -- reject revoked access tokens (e.g., after logout)
//!
//! This module provides [`SecurityPipeline`] so all request execution paths can
//! delegate to a single implementation, preventing divergence.

use std::sync::Arc;

use crate::{
    cache::{user_cache::CachedUserSnapshot, KeyBuilder, UserCache},
    models::{UserId, UserStatus},
    service::UserService,
    Error, Result,
};

use super::{Claims, TokenBlacklistStore};

const AUTHENTICATION_FAILED_MESSAGE: &str = "Authentication failed";
const TWO_FACTOR_REQUIRED_MESSAGE: &str =
    "Two-factor authentication is required before tokens can be used";

/// Outcome of a successful security pipeline check.
#[derive(Debug, Clone)]
pub struct AuthenticatedToken {
    pub user_id: UserId,
    pub claims: Claims,
}

/// Shared security pipeline that performs the post-JWT security checks.
///
/// Step 1 (JWT verification) is intentionally left to the caller because
/// each entrypoint extracts the raw token from its own request envelope.
/// Once the caller has valid [`Claims`], it passes them here for steps 2-4.
///
/// When a [`UserCache`] is provided via [`SecurityPipelineRuntime`], the pipeline
/// consults the cache first to fast-reject obviously invalid requests and
/// still populates the cache after successful DB checks to reduce repeated
/// database reads. Successful authentication, however, is always confirmed
/// from the database because cross-replica cache invalidation is best-effort.
#[derive(Clone)]
pub struct SecurityPipeline {
    user_service: Arc<UserService>,
    /// Optional user cache for fast path lookups (avoids DB query on cache hit).
    user_cache: Option<Arc<UserCache>>,
    /// Access token blacklist store for checking revoked access tokens.
    token_blacklist: Arc<dyn TokenBlacklistStore>,
    /// Key builder for constructing blacklist keys.
    key_builder: KeyBuilder,
}

#[derive(Clone)]
pub struct SecurityPipelineRuntime {
    pub user_cache: Option<Arc<UserCache>>,
    pub token_blacklist: Arc<dyn TokenBlacklistStore>,
    pub key_builder: KeyBuilder,
}

impl SecurityPipeline {
    #[must_use]
    pub const fn classify_auth_error(err: &Error) -> AuthErrorCategory {
        match err {
            Error::Authentication(_) => AuthErrorCategory::Authentication,
            Error::Authorization(_) | Error::KickCooldownDenied => AuthErrorCategory::Authorization,
            Error::ServiceUnavailable(_)
            | Error::Database(_)
            | Error::Redis(_)
            | Error::Timeout(_) => AuthErrorCategory::Unavailable,
            _ => AuthErrorCategory::Internal,
        }
    }

    /// Create a new security pipeline.
    #[must_use]
    pub fn new(user_service: &Arc<UserService>) -> Self {
        Self::new_with_runtime(
            user_service.clone(),
            SecurityPipelineRuntime {
                user_cache: None,
                token_blacklist: user_service.token_blacklist_store(),
                key_builder: user_service.key_builder().clone(),
            },
        )
    }

    /// Create a security pipeline with explicit runtime dependencies.
    #[must_use]
    pub fn new_with_runtime(
        user_service: Arc<UserService>,
        runtime: SecurityPipelineRuntime,
    ) -> Self {
        Self {
            user_service,
            user_cache: runtime.user_cache,
            token_blacklist: runtime.token_blacklist,
            key_builder: runtime.key_builder,
        }
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
        let user_id = claims.user_id()?;

        if let Some(cache) = &self.user_cache {
            match cache.get(&user_id).await {
                Ok(Some(cached)) => {
                    if cached.is_banned()
                        || cached.status() == UserStatus::Banned
                        || cached.is_deleted()
                    {
                        return Err(Error::Authentication("Authentication failed".to_string()));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        user_id = %user_id,
                        "Failed to read user cache in SecurityPipeline"
                    );
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

        let password_version = self
            .user_service
            .get_password_credential_state(&user_id)
            .await?
            .version;

        if claims.pv < password_version {
            return Err(Error::Authentication(
                "Token invalidated due to password change. Please log in again.".to_string(),
            ));
        }

        let is_banned = self.user_service.is_user_banned(&user_id).await?;
        if user.is_deleted() || is_banned {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        if self.user_service.is_two_factor_enabled(&user_id).await?
            && !claims.satisfies_two_factor_requirement()
        {
            return Err(Error::Authentication(
                TWO_FACTOR_REQUIRED_MESSAGE.to_string(),
            ));
        }

        // Check access token JTI blacklist (e.g. logout)
        self.check_access_token_blacklist(claims).await?;

        // Populate the cache after a successful DB lookup so future requests
        // for this user are served from the cache.
        if let Some(cache) = &self.user_cache {
            let cached_user =
                crate::cache::user_cache::CachedUser::from_snapshot(CachedUserSnapshot {
                    id: user.id,
                    username: user.username.clone(),
                    role: user.role,
                    status: UserStatus::Active,
                    created_at: user.created_at,
                    updated_at: user.updated_at,
                    is_banned,
                    is_deleted: user.is_deleted(),
                });
            // Best-effort: log but do not fail the request if the cache write errors.
            if let Err(e) = cache.set(&user_id, cached_user).await {
                tracing::warn!(
                    error = %e,
                    user_id = %user_id,
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
    /// Checks whether the token's JTI is blacklisted and fails closed on
    /// storage errors so revoked tokens cannot bypass the check during outages.
    async fn check_access_token_blacklist(&self, claims: &Claims) -> Result<()> {
        Self::check_access_token_blacklist_with(
            self.token_blacklist.as_ref(),
            &self.key_builder,
            claims,
        )
        .await
    }

    async fn check_access_token_blacklist_with(
        token_blacklist: &dyn TokenBlacklistStore,
        key_builder: &KeyBuilder,
        claims: &Claims,
    ) -> Result<()> {
        // Skip check if JTI is empty (shouldn't happen for valid tokens)
        if claims.jti.is_empty() {
            return Ok(());
        }

        let key = key_builder.access_token_blacklist(&claims.jti);
        match token_blacklist.is_blacklisted_checked(&key).await {
            Ok(true) => Err(Error::Authentication("Authentication failed".to_string())),
            Ok(false) => Ok(()),
            Err(e) => {
                tracing::error!(
                    user_id = %claims.sub,
                    jti = %claims.jti,
                    error = %e,
                    "Access token blacklist check failed due to storage error"
                );
                Err(Error::ServiceUnavailable(
                    "Authentication service temporarily unavailable".to_string(),
                ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::cache::KeyBuilder;
    use async_trait::async_trait;

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

        async fn blacklist_if_not_exists(&self, _key: &str, _ttl_secs: u64) -> Result<bool> {
            Err(Error::Redis(redis::RedisError::from(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "blacklist backend unavailable",
            ))))
        }

        async fn get_family_revoked_at_checked(&self, _key: &str) -> Result<Option<i64>> {
            Ok(None)
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

    fn make_claims(user_id: &str, pv: i32) -> Claims {
        let now = crate::SystemClock.now();
        Claims {
            sub: user_id.to_string(),
            typ: "access".to_string(),
            jti: "test-jti".to_string(),
            iat: now.timestamp(),
            exp: (now + chrono::Duration::hours(1)).timestamp(),
            pv,
            sid: None,
            amr: None,
            cbm: None,
            opi: None,
            ops: None,
            eml: None,
            wcid: None,
            iss: None,
            aud: None,
        }
    }

    #[tokio::test]
    async fn blacklist_storage_error_is_service_unavailable() {
        let blacklist = Arc::new(FailingBlacklistStore);
        let key_builder = KeyBuilder::new("test");
        let err = SecurityPipeline::check_access_token_blacklist_with(
            blacklist.as_ref(),
            &key_builder,
            &make_claims("user-1", 0),
        )
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
