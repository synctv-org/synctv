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
/// caller has valid [`Claims`], it passes them here for steps 2-3.
#[derive(Clone)]
pub struct SecurityPipeline {
    user_service: Arc<UserService>,
}

impl SecurityPipeline {
    /// Create a new security pipeline.
    #[must_use]
    pub fn new(user_service: Arc<UserService>) -> Self {
        Self { user_service }
    }

    /// Run post-JWT security checks (steps 2-3).
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

        // Step 2 + 3: Fetch user and check password version + status
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|_| Error::Authentication("User not found".to_string()))?;

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

        Ok(AuthenticatedToken {
            user_id,
            claims: claims.clone(),
        })
    }
}

impl std::fmt::Debug for SecurityPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityPipeline")
            .finish()
    }
}
