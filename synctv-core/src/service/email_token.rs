//! Email token service for email verification and password reset
//!
//! Manages generation, validation, and cleanup of email tokens.

use chrono::{Duration, Utc};
use nanoid::nanoid;
use sqlx::PgPool;
use tracing::{debug, info};

use crate::{models::UserId, repository::EmailTokenRepository, Error, Result};

/// Token type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailTokenType {
    EmailVerification,
    PasswordReset,
}

impl EmailTokenType {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::EmailVerification => "email_verification",
            Self::PasswordReset => "password_reset",
        }
    }

    #[must_use]
    pub const fn expiration_duration(&self) -> Duration {
        match self {
            Self::EmailVerification => Duration::hours(24), // 24 hours
            Self::PasswordReset => Duration::hours(1),      // 1 hour
        }
    }
}

/// Email token service
#[derive(Clone)]
pub struct EmailTokenService {
    repository: EmailTokenRepository,
}

impl EmailTokenService {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            repository: EmailTokenRepository::new(pool),
        }
    }

    /// Generate a new email token
    ///
    /// Invalidates any existing tokens of the same type for this user before
    /// creating a new one. This ensures only one valid token per user per
    /// purpose at any time, preventing token accumulation attacks.
    pub async fn generate_token(
        &self,
        user_id: &UserId,
        token_type: EmailTokenType,
    ) -> Result<String> {
        // Invalidate any existing tokens of this type for the user first
        // This ensures only one valid token per user per purpose
        self.repository
            .delete_user_tokens(user_id, token_type)
            .await?;

        // Generate random token
        let token = nanoid!(64);

        let expires_at = Utc::now() + token_type.expiration_duration();

        self.repository
            .create(&token, user_id, token_type, expires_at)
            .await?;

        debug!(
            "Generated {} token for user {}",
            token_type.as_str(),
            user_id.as_str()
        );

        Ok(token)
    }

    /// Validate and consume an email token atomically
    ///
    /// Returns the `user_id` if token is valid.
    /// Uses a single UPDATE with WHERE conditions to atomically check validity
    /// and mark as used, preventing concurrent token reuse.
    pub async fn validate_token(&self, token: &str, token_type: EmailTokenType) -> Result<UserId> {
        let token_record = self
            .repository
            .validate_and_consume(token, token_type)
            .await?
            .ok_or_else(|| Error::InvalidInput("Invalid or expired token".to_string()))?;

        info!(
            "Validated {} token for user {}",
            token_type.as_str(),
            token_record.user_id.as_str()
        );

        Ok(token_record.user_id)
    }

    /// Invalidate all tokens of a specific type for a user
    pub async fn invalidate_user_tokens(
        &self,
        user_id: &UserId,
        token_type: EmailTokenType,
    ) -> Result<()> {
        self.repository
            .delete_user_tokens(user_id, token_type)
            .await?;

        debug!(
            "Invalidated all {} tokens for user {}",
            token_type.as_str(),
            user_id.as_str()
        );

        Ok(())
    }

    /// Cleanup expired tokens
    pub async fn cleanup_expired(&self) -> Result<usize> {
        let count = self.repository.cleanup_expired().await?;
        if count > 0 {
            info!("Cleaned up {} expired email tokens", count);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_type_expiration() {
        let email_verify = EmailTokenType::EmailVerification;
        let password_reset = EmailTokenType::PasswordReset;

        assert_eq!(email_verify.as_str(), "email_verification");
        assert_eq!(password_reset.as_str(), "password_reset");

        // Email verification: 24 hours
        assert_eq!(email_verify.expiration_duration(), Duration::hours(24));

        // Password reset: 1 hour
        assert_eq!(password_reset.expiration_duration(), Duration::hours(1));
    }
}
