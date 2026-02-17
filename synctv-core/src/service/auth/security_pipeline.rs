//! Shared 4-step security pipeline for HTTP and gRPC authentication.
//!
//! Both the HTTP `AuthUser` extractor and the gRPC `BlacklistCheckLayer` enforce
//! identical security checks in a fixed order:
//!
//! 1. **JWT verification** -- validate signature, expiration, and access token type
//! 2. **Token blacklist** -- reject explicitly revoked tokens (e.g. after logout)
//! 3. **Password invalidation** -- reject tokens issued before a password change
//! 4. **User status** -- reject banned, pending, or soft-deleted users
//!
//! This module provides [`SecurityPipeline`] so both transport layers can delegate
//! to a single implementation, preventing divergence.

use std::sync::Arc;

use crate::{
    models::{UserId, UserStatus},
    service::{TokenBlacklistService, UserService},
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
    blacklist_service: Arc<TokenBlacklistService>,
    user_service: Arc<UserService>,
}

impl SecurityPipeline {
    /// Create a new security pipeline.
    pub fn new(
        blacklist_service: Arc<TokenBlacklistService>,
        user_service: Arc<UserService>,
    ) -> Self {
        Self {
            blacklist_service,
            user_service,
        }
    }

    /// Run post-JWT security checks (steps 2-4).
    ///
    /// The caller is responsible for step 1 (JWT verification) and must
    /// provide the validated [`Claims`] and the raw token string.
    ///
    /// # Arguments
    /// * `raw_token` -- the raw JWT string (needed for blacklist lookup)
    /// * `claims` -- the already-verified JWT claims
    ///
    /// # Returns
    /// [`AuthenticatedToken`] on success, or an [`Error::Authentication`] on failure.
    pub async fn check(&self, raw_token: &str, claims: &Claims) -> Result<AuthenticatedToken> {
        let user_id = claims.user_id();

        // Step 2: Token blacklist check (explicit revocation via logout)
        if self
            .blacklist_service
            .is_blacklisted(raw_token)
            .await
            .unwrap_or(true) // Fail closed
        {
            return Err(Error::Authentication(
                "Token has been revoked".to_string(),
            ));
        }

        // Step 3: Password invalidation check
        if self
            .user_service
            .is_token_invalidated_by_password_change(&user_id, claims.iat)
            .await
            .unwrap_or(true) // Fail closed
        {
            return Err(Error::Authentication(
                "Token invalidated due to password change. Please log in again.".to_string(),
            ));
        }

        // Step 4: User status check (banned / pending / deleted)
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|_| Error::Authentication("User not found".to_string()))?;

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
        f.debug_struct("SecurityPipeline").finish()
    }
}
