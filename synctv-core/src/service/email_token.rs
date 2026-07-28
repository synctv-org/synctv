//! Email token service for email bind, login, and password reset
//!
//! Manages generation, validation, and cleanup of email tokens.
//!
//! # Rate Limiting
//!
//! Token generation is rate-limited to prevent abuse:
//! - Per-user limit: 5 tokens per hour per token type
//! - This prevents email flooding and database bloat attacks

use sqlx::PgPool;
use std::future::Future;
use std::sync::Arc;
use synctv_common::ExecutionControl;
use tracing::{debug, info, warn};

use crate::{
    models::UserId,
    repository::EmailTokenRepository,
    service::{EmailOutboxService, RateLimitError, RequestRateLimiterService},
    Clock, Error, Result, SystemClock,
};

use crate::models::EmailTokenType;

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
            // 5 tokens per hour per user is reasonable for email bind and password reset flows.
            max_tokens_per_user: 5,
            window_seconds: 3600, // 1 hour
        }
    }
}

/// Email token service
#[derive(Clone)]
pub struct EmailTokenService {
    clock: Arc<dyn Clock>,
    repository: EmailTokenRepository,
    rate_limiter: Option<Arc<dyn RequestRateLimiterService>>,
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
    async fn run_with_control<T, F>(control: Option<&ExecutionControl>, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        match control {
            Some(control) => control.run(future).await.map_err(Error::from)?,
            None => future.await,
        }
    }

    /// Create a new email token service without rate limiting
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            clock: Arc::new(SystemClock),
            repository: EmailTokenRepository::new(pool),
            rate_limiter: None,
            rate_limit_config: EmailTokenRateLimitConfig::default(),
        }
    }

    /// Create a new email token service with explicit runtime dependencies.
    #[must_use]
    pub fn new_with_runtime(
        pool: PgPool,
        rate_limiter: Arc<dyn RequestRateLimiterService>,
        rate_limit_config: Option<EmailTokenRateLimitConfig>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            clock,
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

    async fn check_generation_rate_limit(
        &self,
        user_id: &UserId,
        token_type: EmailTokenType,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::check_generation_rate_limit_for(
            self.rate_limiter.as_deref(),
            &self.rate_limit_config,
            user_id,
            token_type,
            control,
        )
        .await
    }

    async fn check_generation_rate_limit_for(
        limiter: Option<&dyn RequestRateLimiterService>,
        config: &EmailTokenRateLimitConfig,
        user_id: &UserId,
        token_type: EmailTokenType,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        let Some(limiter) = limiter else {
            return Ok(());
        };
        let rate_limit_key = format!("email:{token_type}:{user_id}");

        match limiter
            .check_rate_limit_with_control(
                &rate_limit_key,
                config.max_tokens_per_user,
                config.window_seconds,
                control,
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(RateLimitError::RateLimitExceeded {
                retry_after_seconds,
            }) => {
                warn!(
                    user_id = %user_id,
                    token_type = %token_type.as_str(),
                    retry_after_seconds,
                    "Email token rate limit exceeded"
                );
                Err(Error::RateLimited(format!(
                    "Too many {} requests. Please try again in {} seconds.",
                    token_type.as_str(),
                    retry_after_seconds
                )))
            }
            Err(RateLimitError::Control(error)) => Err(Error::Timeout(error.to_string())),
            Err(RateLimitError::BackendUnavailable(message)) => {
                Err(Error::ServiceUnavailable(message))
            }
            Err(RateLimitError::RedisError(error)) => Err(Error::ServiceUnavailable(format!(
                "Email token rate limit service unavailable: {error}"
            ))),
        }
    }

    pub async fn check_generate_token_rate_limit(
        &self,
        user_id: &UserId,
        token_type: EmailTokenType,
    ) -> Result<()> {
        self.check_generate_token_rate_limit_with_control(user_id, token_type, None)
            .await
    }

    pub async fn check_generate_token_rate_limit_with_control(
        &self,
        user_id: &UserId,
        token_type: EmailTokenType,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.check_generation_rate_limit(user_id, token_type, control)
            .await
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
        self.generate_token_with_control(user_id, token_type, None)
            .await
    }

    pub async fn generate_token_with_control(
        &self,
        user_id: &UserId,
        token_type: EmailTokenType,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        self.check_generation_rate_limit(user_id, token_type, control)
            .await?;

        // Generate random token
        let token = synctv_common::snanoid!(64);

        let expires_at = self.clock.now() + token_type.expiration_duration();

        if token_type.keeps_multiple_unused_tokens() {
            self.repository
                .create(&token, user_id, token_type, expires_at)
                .await?;
        } else {
            self.repository
                .create_or_replace_unused(&token, user_id, token_type, expires_at)
                .await?;
        }

        debug!("Generated {} token for user {}", token_type, user_id);

        Ok(token)
    }

    pub async fn generate_token_and_enqueue_with_control(
        &self,
        outbox: &EmailOutboxService,
        recipient: &str,
        user_id: &UserId,
        token_type: EmailTokenType,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        self.check_generation_rate_limit(user_id, token_type, control)
            .await?;

        let token = synctv_common::snanoid!(64);
        let expires_at = self.clock.now() + token_type.expiration_duration();
        let job = outbox.prepare_token(recipient, &token, user_id, token_type, expires_at)?;

        Self::run_with_control(control, async {
            let mut tx = self.repository.pool().begin().await?;
            if token_type.keeps_multiple_unused_tokens() {
                self.repository
                    .create_with_executor(&token, user_id, token_type, expires_at, &mut tx)
                    .await?;
            } else {
                self.repository
                    .create_or_replace_unused_with_executor(
                        &token, user_id, token_type, expires_at, &mut tx,
                    )
                    .await?;
            }
            if !outbox
                .repository()
                .insert_with_executor(&job, &mut tx)
                .await?
            {
                return Err(Error::Internal(
                    "Email outbox job was unexpectedly deduplicated".to_string(),
                ));
            }
            tx.commit().await?;
            Ok(())
        })
        .await?;

        debug!(
            user_id = %user_id,
            token_type = %token_type,
            "Generated email token and persisted delivery job"
        );
        Ok(token)
    }

    /// Validate and consume an email token atomically
    ///
    /// Returns the `user_id` if token is valid.
    /// Uses a single UPDATE with WHERE conditions to atomically check validity
    /// and mark as used, preventing concurrent token reuse.
    pub async fn validate_token(&self, token: &str, token_type: EmailTokenType) -> Result<UserId> {
        self.validate_token_with_control(token, token_type, None)
            .await
    }

    pub async fn validate_token_with_control(
        &self,
        token: &str,
        token_type: EmailTokenType,
        control: Option<&ExecutionControl>,
    ) -> Result<UserId> {
        let now = self.clock.now();
        let token_record = Self::run_with_control(
            control,
            self.repository.validate_and_consume(token, token_type, now),
        )
        .await?
        .ok_or_else(|| {
            Error::InvalidInput(synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string())
        })?;

        info!(
            "Validated {} token for user {}",
            token_type, token_record.user_id
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
        self.validate_token_for_user_with_control(token, token_type, expected_user_id, None)
            .await
    }

    pub async fn validate_token_for_user_with_control(
        &self,
        token: &str,
        token_type: EmailTokenType,
        expected_user_id: &UserId,
        control: Option<&ExecutionControl>,
    ) -> Result<UserId> {
        let now = self.clock.now();
        let token_record = Self::run_with_control(
            control,
            self.repository
                .validate_and_consume_for_user(token, token_type, expected_user_id, now),
        )
        .await?
        .ok_or_else(|| {
            Error::InvalidInput(synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string())
        })?;

        info!(
            "Validated {} token for expected user {}",
            token_type, expected_user_id
        );

        Ok(token_record.user_id)
    }

    /// Invalidate all tokens of a specific type for a user
    pub async fn invalidate_user_tokens(
        &self,
        user_id: &UserId,
        token_type: EmailTokenType,
    ) -> Result<()> {
        self.invalidate_user_tokens_with_control(user_id, token_type, None)
            .await
    }

    pub async fn invalidate_user_tokens_with_control(
        &self,
        user_id: &UserId,
        token_type: EmailTokenType,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::run_with_control(
            control,
            self.repository.delete_user_tokens(user_id, token_type),
        )
        .await?;

        debug!("Invalidated all {} tokens for user {}", token_type, user_id);

        Ok(())
    }

    /// Invalidate a specific unused token without touching newer replacements.
    pub async fn invalidate_specific_token(
        &self,
        token: &str,
        user_id: &UserId,
        token_type: EmailTokenType,
    ) -> Result<()> {
        self.invalidate_specific_token_with_control(token, user_id, token_type, None)
            .await
    }

    pub async fn invalidate_specific_token_with_control(
        &self,
        token: &str,
        user_id: &UserId,
        token_type: EmailTokenType,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::run_with_control(
            control,
            self.repository
                .delete_unused_token(token, user_id, token_type),
        )
        .await?;

        debug!(
            "Invalidated specific {} token for user {}",
            token_type, user_id
        );

        Ok(())
    }

    /// Cleanup expired tokens
    pub async fn cleanup_expired(&self) -> Result<usize> {
        self.cleanup_expired_with_control(None).await
    }

    pub async fn cleanup_expired_with_control(
        &self,
        control: Option<&ExecutionControl>,
    ) -> Result<usize> {
        let now = self.clock.now();
        let count = Self::run_with_control(control, self.repository.cleanup_expired(now)).await?;
        if count > 0 {
            info!("Cleaned up {} expired email tokens", count);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn test_token_type_expiration() {
        let email_verify = EmailTokenType::EmailBind;
        let password_reset = EmailTokenType::PasswordReset;
        let email_login = EmailTokenType::EmailLogin;

        assert_eq!(email_verify.as_str(), "email_bind");
        assert_eq!(password_reset.as_str(), "password_reset");
        assert_eq!(email_login.as_str(), "email_login");
        assert_eq!(i16::from(email_verify), 1);
        assert_eq!(i16::from(password_reset), 2);
        assert_eq!(i16::from(email_login), 3);

        // Email bind: 24 hours
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
        // Create rate limiter with in-memory backend
        let limiter = synctv_core_testing::create_test_request_rate_limiter("email_token_test:");

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
        let key = format!("email:{}:{}", "email_bind", "user123");
        assert!(key.contains("email"));
        assert!(key.contains("email_bind"));
        assert!(key.contains("user123"));

        ok(
            limiter.check_rate_limit(&key, 2, 60).await,
            "first rate limit check should pass",
        );
        ok(
            limiter.check_rate_limit(&key, 2, 60).await,
            "second rate limit check should pass",
        );
        let result = limiter.check_rate_limit(&key, 2, 60).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_check_generate_token_rate_limit_blocks_without_database_write() {
        let limiter = Arc::new(crate::service::RateLimiter::local_only(
            "email_token_check:".to_string(),
        ));
        let config = EmailTokenRateLimitConfig {
            max_tokens_per_user: 2,
            window_seconds: 60,
        };
        let user_id = UserId::new();

        ok(
            EmailTokenService::check_generation_rate_limit_for(
                Some(limiter.as_ref()),
                &config,
                &user_id,
                EmailTokenType::EmailBind,
                None,
            )
            .await
            .map_err(|error| error.to_string()),
            "first generation rate limit check should pass",
        );
        ok(
            EmailTokenService::check_generation_rate_limit_for(
                Some(limiter.as_ref()),
                &config,
                &user_id,
                EmailTokenType::EmailBind,
                None,
            )
            .await
            .map_err(|error| error.to_string()),
            "second generation rate limit check should pass",
        );

        let result = EmailTokenService::check_generation_rate_limit_for(
            Some(limiter.as_ref()),
            &config,
            &user_id,
            EmailTokenType::EmailBind,
            None,
        )
        .await;
        assert!(matches!(result, Err(Error::RateLimited(_))));
    }
}
