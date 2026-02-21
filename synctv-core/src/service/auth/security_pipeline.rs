//! Shared security pipeline for HTTP and gRPC authentication.
//!
//! Both the HTTP `AuthUser` extractor and the gRPC `BlacklistCheckLayer` enforce
//! identical security checks in a fixed order:
//!
//! 1. **JWT verification** -- validate signature, expiration, and access token type
//! 2. **Password invalidation** -- reject tokens issued before a password change (database-based)
//! 3. **User status** -- reject banned, pending, or soft-deleted users
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
/// caller has valid [`Claims`], it passes them here for steps 2-3.
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
    /// This is a no-op if no [`TokenBlacklistStore`] has been configured.
    async fn check_access_token_blacklist(&self, claims: &Claims) -> Result<()> {
        if let (Some(store), Some(kb)) = (&self.token_blacklist, &self.key_builder) {
            if !claims.jti.is_empty() {
                let key = kb.access_token_blacklist(&claims.jti);
                if store.is_blacklisted(&key).await {
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for SecurityPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityPipeline")
            .finish()
    }
}
