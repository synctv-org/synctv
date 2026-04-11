//! Email token service for email verification and password reset
//!
//! Manages generation, validation, and cleanup of email tokens.
//!
//! # Rate Limiting
//!
//! Token generation is rate-limited to prevent abuse:
//! - Per-user limit: 5 tokens per hour per token type
//! - This prevents email flooding and database bloat attacks

use chrono::{Duration, Utc};
use sqlx::PgPool;
use tracing::{debug, info, warn};

use crate::{
    models::UserId,
    repository::EmailTokenRepository,
    service::rate_limit::{RateLimitError, RateLimiter},
    Error, Result,
};

/// Token type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailTokenType {
    EmailVerification,
    PasswordReset,
    EmailLogin,
}

impl EmailTokenType {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::EmailVerification => "email_verification",
            Self::PasswordReset => "password_reset",
            Self::EmailLogin => "email_login",
        }
    }

    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::EmailVerification => 1,
            Self::PasswordReset => 2,
            Self::EmailLogin => 3,
        }
    }

    #[must_use]
    pub const fn expiration_duration(&self) -> Duration {
        match self {
            Self::EmailVerification => Duration::hours(24), // 24 hours
            Self::PasswordReset => Duration::hours(1),      // 1 hour
            Self::EmailLogin => Duration::minutes(15),      // 15 minutes
        }
    }

    #[must_use]
    pub const fn keeps_multiple_unused_tokens(self) -> bool {
        matches!(self, Self::EmailLogin)
    }
}

/// Rate limit configuration for email token generation
#[derive(Debug, Clone)]
pub struct EmailTokenRateLimitConfig {
    /// Maximum tokens per user per window
    pub max_tokens_per_user: u32,
    /// Window size in seconds
    pub window_seconds: u64,
}

impl Default for EmailTokenRateLimitConfig {
    fn default() -> Self {
        Self {
            // 5 tokens per hour per user - reasonable for email verification/password reset
            max_tokens_per_user: 5,
            window_seconds: 3600, // 1 hour
        }
    }
}

/// Email token service
#[derive(Clone)]
pub struct EmailTokenService {
    repository: EmailTokenRepository,
    rate_limiter: Option<RateLimiter>,
    rate_limit_config: EmailTokenRateLimitConfig,
}

impl std::fmt::Debug for EmailTokenService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailTokenService")
            .field("rate_limiter", &self.rate_limiter.is_some())
            .field("rate_limit_config", &self.rate_limit_config)
            .finish()
    }
}

impl EmailTokenService {
    /// Create a new email token service without rate limiting
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            repository: EmailTokenRepository::new(pool),
            rate_limiter: None,
            rate_limit_config: EmailTokenRateLimitConfig::default(),
        }
    }

    /// Create a new email token service with rate limiting enabled
    #[must_use]
    pub fn with_rate_limiter(
        pool: PgPool,
        rate_limiter: RateLimiter,
        rate_limit_config: Option<EmailTokenRateLimitConfig>,
    ) -> Self {
        Self {
            repository: EmailTokenRepository::new(pool),
            rate_limiter: Some(rate_limiter),
            rate_limit_config: rate_limit_config.unwrap_or_default(),
        }
    }

    #[must_use]
    pub const fn has_rate_limiter(&self) -> bool {
        self.rate_limiter.is_some()
    }

    #[must_use]
    pub const fn rate_limit_config(&self) -> &EmailTokenRateLimitConfig {
        &self.rate_limit_config
    }

    /// Generate a new email token
    ///
    /// # Rate Limiting
    ///
    /// Token generation is rate-limited to prevent abuse:
    /// - Default: 5 tokens per hour per user per token type
    /// - Prevents email flooding and database bloat attacks
    ///
    /// # Token Invalidation
    ///
    /// Atomically replaces any existing unused token of the same type for this
    /// user while creating a new one. This ensures only one valid token per
    /// user per purpose at any time, without exposing callers to a delete/insert race.
    pub async fn generate_token(
        &self,
        user_id: &UserId,
        token_type: EmailTokenType,
    ) -> Result<String> {
        // Check rate limit if configured
        if let Some(ref limiter) = self.rate_limiter {
            let rate_limit_key = format!("email:{}:{}", token_type.as_str(), user_id.as_str());

            if let Err(RateLimitError::RateLimitExceeded {
                retry_after_seconds,
            }) = limiter
                .check_rate_limit(
                    &rate_limit_key,
                    self.rate_limit_config.max_tokens_per_user,
                    self.rate_limit_config.window_seconds,
                )
                .await
            {
                warn!(
                    user_id = %user_id.as_str(),
                    token_type = %token_type.as_str(),
                    retry_after_seconds,
                    "Email token rate limit exceeded"
                );
                return Err(Error::RateLimited(format!(
                    "Too many {} requests. Please try again in {} seconds.",
                    token_type.as_str(),
                    retry_after_seconds
                )));
            }
        }

        // Generate random token
        let token = synctv_common::snanoid!(64);

        let expires_at = Utc::now() + token_type.expiration_duration();

        if token_type.keeps_multiple_unused_tokens() {
            self.repository
                .create(&token, user_id, token_type, expires_at)
                .await?;
        } else {
            self.repository
                .create_or_replace_unused(&token, user_id, token_type, expires_at)
                .await?;
        }

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

    /// Validate and consume a token only if it belongs to the expected user.
    pub async fn validate_token_for_user(
        &self,
        token: &str,
        token_type: EmailTokenType,
        expected_user_id: &UserId,
    ) -> Result<UserId> {
        let token_record = self
            .repository
            .validate_and_consume_for_user(token, token_type, expected_user_id)
            .await?
            .ok_or_else(|| Error::InvalidInput("Invalid or expired token".to_string()))?;

        info!(
            "Validated {} token for expected user {}",
            token_type.as_str(),
            expected_user_id.as_str()
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

    /// Invalidate a specific unused token without touching newer replacements.
    pub async fn invalidate_specific_token(
        &self,
        token: &str,
        user_id: &UserId,
        token_type: EmailTokenType,
    ) -> Result<()> {
        self.repository
            .delete_unused_token(token, user_id, token_type)
            .await?;

        debug!(
            "Invalidated specific {} token for user {}",
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
        let email_login = EmailTokenType::EmailLogin;

        assert_eq!(email_verify.as_str(), "email_verification");
        assert_eq!(password_reset.as_str(), "password_reset");
        assert_eq!(email_login.as_str(), "email_login");
        assert_eq!(email_verify.as_i16(), 1);
        assert_eq!(password_reset.as_i16(), 2);
        assert_eq!(email_login.as_i16(), 3);

        // Email verification: 24 hours
        assert_eq!(email_verify.expiration_duration(), Duration::hours(24));

        // Password reset: 1 hour
        assert_eq!(password_reset.expiration_duration(), Duration::hours(1));

        // Email login: 15 minutes
        assert_eq!(email_login.expiration_duration(), Duration::minutes(15));
        assert!(email_login.keeps_multiple_unused_tokens());
        assert!(!email_verify.keeps_multiple_unused_tokens());
    }

    #[tokio::test]
    async fn test_email_token_rate_limiting() {
        use crate::service::rate_limit::RateLimiter;

        // Create rate limiter with in-memory backend
        let limiter = RateLimiter::in_memory_only("email_token_test:".to_string());

        // Create service with aggressive rate limiting for testing
        let config = EmailTokenRateLimitConfig {
            max_tokens_per_user: 2,
            window_seconds: 60,
        };
        assert_eq!(config.max_tokens_per_user, 2);
        assert_eq!(config.window_seconds, 60);

        // We can't test actual token generation without a database,
        // but we can verify the rate limit key format and limiter behavior

        // Test rate limit key format
        let key = format!("email:{}:{}", "email_verification", "user123");
        assert!(key.contains("email"));
        assert!(key.contains("email_verification"));
        assert!(key.contains("user123"));

        // Test that rate limiter blocks after limit
        limiter.check_rate_limit(&key, 2, 60).await.unwrap();
        limiter.check_rate_limit(&key, 2, 60).await.unwrap();
        let result = limiter.check_rate_limit(&key, 2, 60).await;
        assert!(result.is_err());
    }
}
