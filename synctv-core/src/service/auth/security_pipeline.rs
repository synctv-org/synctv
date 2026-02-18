//! Shared 4-step security pipeline for HTTP and gRPC authentication.
//!
//! Both the HTTP `AuthUser` extractor and the gRPC `BlacklistCheckLayer` enforce
//! identical security checks in a fixed order:
//!
//! 1. **JWT verification** -- validate signature, expiration, and access token type
//! 2. **Token blacklist** -- reject tokens that have been explicitly revoked (e.g., logout)
//! 3. **Password invalidation** -- reject tokens issued before a password change (database-based)
//! 4. **User status** -- reject banned, pending, or soft-deleted users
//!
//! This module provides [`SecurityPipeline`] so both transport layers can delegate
//! to a single implementation, preventing divergence.

use std::sync::Arc;

use crate::{
    cache::KeyBuilder,
    models::{UserId, UserStatus},
    service::UserService,
    Error, Result,
};

use super::Claims;

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
#[derive(Clone)]
pub struct SecurityPipeline {
    user_service: Arc<UserService>,
    redis_conn: Option<redis::aio::ConnectionManager>,
    key_builder: KeyBuilder,
}

impl SecurityPipeline {
    /// Create a new security pipeline (without Redis -- token blacklist is disabled).
    #[must_use] 
    pub fn new(user_service: Arc<UserService>) -> Self {
        Self {
            user_service,
            redis_conn: None,
            key_builder: KeyBuilder::default(),
        }
    }

    /// Create a new security pipeline with Redis for token blacklisting.
    #[must_use] 
    pub const fn with_redis(
        user_service: Arc<UserService>,
        redis_conn: redis::aio::ConnectionManager,
        key_builder: KeyBuilder,
    ) -> Self {
        Self {
            user_service,
            redis_conn: Some(redis_conn),
            key_builder,
        }
    }

    /// Blacklist a token by its JTI (JWT ID). The token will be rejected by
    /// the pipeline until the TTL expires.
    ///
    /// `ttl_secs` should be set to the remaining lifetime of the token so the
    /// blacklist entry automatically expires when the token would have expired.
    ///
    /// Returns `Ok(true)` if the token was blacklisted, `Ok(false)` if Redis
    /// is not configured (blacklist disabled).
    pub async fn blacklist_token(&self, jti: &str, ttl_secs: u64) -> Result<bool> {
        let Some(ref conn) = self.redis_conn else {
            tracing::debug!("Token blacklist skipped (Redis not configured)");
            return Ok(false);
        };

        if jti.is_empty() {
            return Ok(false);
        }

        let key = self.key_builder.token_blacklist(jti);
        let mut conn = conn.clone();

        redis::cmd("SET")
            .arg(&key)
            .arg("1")
            .arg("EX")
            .arg(ttl_secs.max(1))
            .arg("NX")
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| Error::Internal(format!("Redis token blacklist SET failed: {e}")))?;

        tracing::debug!(jti = jti, ttl_secs = ttl_secs, "Token blacklisted");
        Ok(true)
    }

    /// Check if a token's JTI is blacklisted.
    ///
    /// Returns `true` if the token is blacklisted and should be rejected.
    /// Returns `false` if Redis is not configured or the token is not found.
    pub async fn is_token_blacklisted(&self, jti: &str) -> Result<bool> {
        let Some(ref conn) = self.redis_conn else {
            return Ok(false);
        };

        if jti.is_empty() {
            return Ok(false);
        }

        let key = self.key_builder.token_blacklist(jti);
        let mut conn = conn.clone();

        let exists: bool = redis::cmd("EXISTS")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .map_err(|e| {
                tracing::warn!("Redis token blacklist EXISTS failed: {e}");
                Error::Internal(format!("Redis token blacklist check failed: {e}"))
            })?;

        Ok(exists)
    }

    /// Run post-JWT security checks (steps 2-4).
    ///
    /// The caller is responsible for step 1 (JWT verification) and must
    /// provide the validated [`Claims`].
    ///
    /// # Arguments
    /// * `claims` -- the already-verified JWT claims
    ///
    /// # Returns
    /// [`AuthenticatedToken`] on success, or an [`Error::Authentication`] on failure.
    pub async fn check(&self, claims: &Claims) -> Result<AuthenticatedToken> {
        let user_id = claims.user_id();

        // Step 2: Token blacklist check (Redis-based)
        if self.is_token_blacklisted(&claims.jti).await.unwrap_or(false) {
            return Err(Error::Authentication(
                "Token has been revoked. Please log in again.".to_string(),
            ));
        }

        // Step 3 + 4: Fetch user and check password version + status
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|_| Error::Authentication("User not found".to_string()))?;

        // Step 3: Password version check
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

        Ok(AuthenticatedToken {
            user_id,
            claims: claims.clone(),
        })
    }
}

impl std::fmt::Debug for SecurityPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityPipeline")
            .field("has_redis", &self.redis_conn.is_some())
            .finish()
    }
}
