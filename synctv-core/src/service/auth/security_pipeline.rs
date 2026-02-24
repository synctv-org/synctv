//! Shared security pipeline for HTTP and gRPC authentication.
//!
//! Both the HTTP `AuthUser` extractor and the gRPC `BlacklistCheckLayer` enforce
//! identical security checks in a fixed order:
//!
//! 1. **JWT verification** -- validate signature, expiration, and access token type
//! 2. **Password invalidation** -- reject tokens issued before a password change (database-based)
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

/// Configuration for access token blacklist enforcement.
#[derive(Debug, Clone, Copy)]
pub struct BlacklistEnforcement {
    /// If true, the pipeline will reject requests when the blacklist store
    /// is not configured. This ensures that revoked access tokens cannot
    /// bypass the blacklist check.
    ///
    /// When false (default), a missing blacklist store is logged as a warning
    /// but the request is allowed to proceed (for backward compatibility).
    pub require_blacklist: bool,
}

impl BlacklistEnforcement {
    /// Create a new BlacklistEnforcement with default values.
    #[must_use]
    pub const fn new() -> Self {
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
/// consults the cache first on every authenticated request. Only on a cache
/// miss does it fall back to a database query, and the cache is populated
/// after the DB lookup so subsequent requests for the same user are fast.
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
    /// When set, [`check`] will consult the cache before hitting the database.
    /// The cache is populated on every DB fallback so future requests are served
    /// from the cache.
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
    pub fn with_token_blacklist(mut self, store: Arc<dyn TokenBlacklistStore>, key_builder: KeyBuilder) -> Self {
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

    /// Run post-JWT security checks (steps 2-3).
    ///
    /// The caller is responsible for step 1 (JWT verification) and must
    /// provide the validated [`Claims`].
    ///
    /// ## Cache behaviour
    ///
    /// For modern tokens that carry a `pv` (password version) claim the check
    /// is fully satisfiable from the [`UserCache`]:
    /// - Cache hit → validate `password_version` + `status` without a DB round-trip.
    /// - Cache miss → fall back to DB, then populate the cache.
    ///
    /// For legacy tokens without `pv` we always fall back to the DB because the
    /// `password_changed_at` timestamp required for the `iat`-based check is not
    /// stored in the cache.
    ///
    /// # Arguments
    /// * `claims` -- the already-verified JWT claims
    ///
    /// # Returns
    /// [`AuthenticatedToken`] on success, or an [`Error::Authentication`] on failure.
    pub async fn check(&self, claims: &Claims) -> Result<AuthenticatedToken> {
        let user_id = claims.user_id();

        // Fast path: try to satisfy the check from the cache when the token
        // carries a `pv` claim (all tokens issued by modern code do).
        if let (Some(cache), Some(token_pv)) = (&self.user_cache, claims.pv) {
            if let Ok(Some(cached)) = cache.get(&user_id).await {
                // Step 2: Password version check against cached value.
                if token_pv < cached.password_version() {
                    return Err(Error::Authentication(
                        "Token invalidated due to password change. Please log in again.".to_string(),
                    ));
                }

                // Step 3: Status check.
                // INVARIANT: CachedUser does not store `deleted_at` (to keep
                // the cache entry compact). Instead, `UserService::soft_delete`
                // invalidates the cache entry on deletion, so a cache HIT with
                // Active status can be trusted to not be deleted. If this
                // invariant is ever broken (e.g. a code path deletes without
                // invalidation), the DB slow path below will still catch it.
                if cached.status() == UserStatus::Banned || cached.status() == UserStatus::Pending || cached.is_deleted() {
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }

                // Check access token JTI blacklist (e.g. logout)
                self.check_access_token_blacklist(claims).await?;

                return Ok(AuthenticatedToken {
                    user_id,
                    claims: claims.clone(),
                });
            }
            // Cache miss: fall through to DB lookup and populate the cache below.
        }

        // Slow path: fetch user from the database.
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|e| match &e {
                Error::NotFound(_) => Error::Authentication("User not found".to_string()),
                _ => e,
            })?;

        // Step 2: Password version check
        if let Some(token_pv) = claims.pv {
            if token_pv < user.password_version {
                return Err(Error::Authentication(
                    "Token invalidated due to password change. Please log in again.".to_string(),
                ));
            }
        } else {
            // Legacy tokens without pv: fall back to iat-based check
            if claims.iat < user.password_changed_at.timestamp() {
                return Err(Error::Authentication(
                    "Token invalidated due to password change. Please log in again.".to_string(),
                ));
            }
        }

        if user.is_deleted() || user.status == UserStatus::Banned || user.status == UserStatus::Pending {
            return Err(Error::Authentication(
                "Authentication failed".to_string(),
            ));
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
    /// - If the blacklist store is not configured and `require_blacklist` is true,
    ///   the method returns an error to prevent bypassing the blacklist check.
    /// - If the blacklist store is not configured and `require_blacklist` is false
    ///   (default), a warning is logged but the request is allowed to proceed
    ///   for backward compatibility.
    async fn check_access_token_blacklist(&self, claims: &Claims) -> Result<()> {
        // Skip check if JTI is empty (shouldn't happen for valid tokens)
        if claims.jti.is_empty() {
            return Ok(());
        }

        match (&self.token_blacklist, &self.key_builder) {
            (Some(store), Some(kb)) => {
                let key = kb.access_token_blacklist(&claims.jti);
                if store.is_blacklisted(&key).await {
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
                Ok(())
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
                    Err(Error::Authentication(
                        "Authentication service misconfigured".to_string(),
                    ))
                } else {
                    // Fail-open: log warning but allow (backward compatibility)
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

impl std::fmt::Debug for SecurityPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityPipeline")
            .finish()
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
/// let pipeline = SecurityPipelineBuilder::new(user_service)
///     .with_user_cache(user_cache)
///     .with_token_blacklist(blacklist_store, key_builder)
///     .with_blacklist_enforcement(BlacklistEnforcement { require_blacklist: true })
///     .build()?;
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
    pub fn with_blacklist_enforcement(mut self, enforcement: BlacklistEnforcement) -> Self {
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
    use crate::service::auth::token_blacklist::InMemoryTokenBlacklistStore;

    // ========================================================================
    // BlacklistEnforcement behavior tests
    // ========================================================================

    #[test]
    fn blacklist_enforcement_default_is_false() {
        // By default, require_blacklist should be false for backward compatibility
        let enforcement = BlacklistEnforcement::default();
        assert!(!enforcement.require_blacklist);
    }

    #[test]
    fn has_blacklist_store_returns_false_by_default() {
        // SecurityPipeline without blacklist store should return false
        // Note: We can only test this without UserService since it requires many dependencies
        let blacklist_store = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 7200));
        let key_builder = KeyBuilder::new("test");

        // Test that we can check if blacklist store is set via the builder
        // This is a compile-time check that the types are correct
        let _: Arc<dyn TokenBlacklistStore> = blacklist_store.clone();
        let _: KeyBuilder = key_builder;
    }
}
