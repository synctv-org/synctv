//! Shared Email Verification and Password Reset Implementation
//!
//! Used by both HTTP and gRPC handlers to avoid duplicating email logic.

use std::sync::Arc;
use synctv_core::service::{EmailService, EmailTokenService, EmailTokenType, UserService};

use crate::impls::ApiError;

const GENERIC_VERIFICATION_MESSAGE: &str =
    "If an account exists with this email, a verification code will be sent.";
const GENERIC_PASSWORD_RESET_MESSAGE: &str =
    "If an account exists with this email, a password reset code will be sent.";

fn map_email_user_lookup_error(err: synctv_core::Error) -> ApiError {
    ApiError::from(err)
}

fn map_email_mutation_error(err: synctv_core::Error) -> ApiError {
    ApiError::from(err)
}

/// Shared email operations implementation.
pub struct EmailApiImpl {
    pub user_service: Arc<UserService>,
    pub email_service: Arc<EmailService>,
    pub email_token_service: Arc<EmailTokenService>,
}

/// Send verification email result
pub struct SendVerificationResult {
    pub message: String,
}

/// Confirm email result
pub struct ConfirmEmailResult {
    pub message: String,
    pub user_id: String,
}

/// Request password reset result
pub struct RequestPasswordResetResult {
    pub message: String,
}

/// Confirm password reset result
pub struct ConfirmPasswordResetResult {
    pub message: String,
    pub user_id: String,
}

impl EmailApiImpl {
    #[must_use]
    pub const fn new(
        user_service: Arc<UserService>,
        email_service: Arc<EmailService>,
        email_token_service: Arc<EmailTokenService>,
    ) -> Self {
        Self {
            user_service,
            email_service,
            email_token_service,
        }
    }

    /// Send a verification email.
    /// Returns generic message regardless of whether user exists (anti-enumeration).
    pub async fn send_verification_email(
        &self,
        email: &str,
    ) -> Result<SendVerificationResult, ApiError> {
        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(map_email_user_lookup_error)?;

        let user = if let Some(u) = user {
            u
        } else {
            // Add random delay to prevent timing side-channel that leaks
            // whether an account exists based on response time differences.
            let delay_ms = rand::random_range(100u64..500u64);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            return Ok(SendVerificationResult {
                message: GENERIC_VERIFICATION_MESSAGE.to_string(),
            });
        };

        let _token = self
            .email_service
            .send_verification_email(email, &self.email_token_service, &user.id)
            .await
            .map_err(ApiError::from)?;

        tracing::info!(
            "Sent verification email to {}",
            synctv_core::service::mask_email(email)
        );

        Ok(SendVerificationResult {
            message: GENERIC_VERIFICATION_MESSAGE.to_string(),
        })
    }

    /// Confirm an email verification token.
    pub async fn confirm_email(
        &self,
        email: &str,
        token: &str,
    ) -> Result<ConfirmEmailResult, ApiError> {
        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(map_email_user_lookup_error)?
            .ok_or_else(|| {
                ApiError::InvalidInput("Invalid or expired verification token".to_string())
            })?;

        let validated_user_id = self
            .email_token_service
            .validate_token_for_user(token, EmailTokenType::EmailVerification, &user.id)
            .await
            .map_err(|_| {
                ApiError::InvalidInput("Invalid or expired verification token".to_string())
            })?;

        if validated_user_id != user.id {
            return Err(ApiError::InvalidInput(
                "Invalid or expired verification token".to_string(),
            ));
        }

        // Reject banned or soft-deleted users
        if user.is_deleted() || user.status == synctv_core::models::UserStatus::Banned {
            return Err(ApiError::InvalidInput(
                "Invalid or expired verification token".to_string(),
            ));
        }

        self.user_service
            .set_email_verified(&user.id, true)
            .await
            .map_err(map_email_mutation_error)?;

        // Invalidate all remaining email verification tokens for this user
        self.email_token_service
            .invalidate_user_tokens(&user.id, EmailTokenType::EmailVerification)
            .await
            .map_err(map_email_mutation_error)?;

        tracing::info!("Email verified for user {}", user.id.as_str());

        Ok(ConfirmEmailResult {
            message: "Email verified successfully".to_string(),
            user_id: user.id.to_string(),
        })
    }

    /// Request a password reset email.
    /// Returns generic message regardless of whether user exists (anti-enumeration).
    pub async fn request_password_reset(
        &self,
        email: &str,
    ) -> Result<RequestPasswordResetResult, ApiError> {
        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(map_email_user_lookup_error)?;

        let Some(user) = user else {
            // Add random delay to prevent timing side-channel that leaks
            // whether an account exists based on response time differences.
            let delay_ms = rand::random_range(100u64..500u64);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            return Ok(RequestPasswordResetResult {
                message: GENERIC_PASSWORD_RESET_MESSAGE.to_string(),
            });
        };

        let _token = self
            .email_service
            .send_password_reset_email(email, &self.email_token_service, &user.id)
            .await
            .map_err(ApiError::from)?;

        tracing::info!("Password reset requested for user {}", user.id.as_str());

        Ok(RequestPasswordResetResult {
            message: GENERIC_PASSWORD_RESET_MESSAGE.to_string(),
        })
    }

    /// Confirm a password reset with a token and new password.
    pub async fn confirm_password_reset(
        &self,
        email: &str,
        token: &str,
        new_password: &str,
    ) -> Result<ConfirmPasswordResetResult, ApiError> {
        // Password validation (complexity, length) is handled by
        // UserService::set_password() which uses the full PasswordValidator.
        // No redundant length-only check here.

        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(map_email_user_lookup_error)?
            .ok_or_else(|| ApiError::InvalidInput("Invalid or expired reset token".to_string()))?;

        let validated_user_id = self
            .email_token_service
            .validate_token_for_user(token, EmailTokenType::PasswordReset, &user.id)
            .await
            .map_err(|_| ApiError::InvalidInput("Invalid or expired reset token".to_string()))?;

        if validated_user_id != user.id {
            return Err(ApiError::InvalidInput(
                "Invalid or expired reset token".to_string(),
            ));
        }

        // Check if user is banned or soft-deleted
        if user.is_deleted() || user.status == synctv_core::models::UserStatus::Banned {
            return Err(ApiError::InvalidInput(
                "Invalid or expired reset token".to_string(),
            ));
        }

        self.user_service
            .set_password(&user.id, new_password)
            .await
            .map_err(map_email_mutation_error)?;

        // Invalidate all remaining password reset tokens for this user
        self.email_token_service
            .invalidate_user_tokens(&user.id, EmailTokenType::PasswordReset)
            .await
            .map_err(map_email_mutation_error)?;

        tracing::info!("Password reset completed for user {}", user.id.as_str());

        Ok(ConfirmPasswordResetResult {
            message: "Password reset successfully".to_string(),
            user_id: user.id.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        map_email_mutation_error, map_email_user_lookup_error, EmailApiImpl,
        GENERIC_PASSWORD_RESET_MESSAGE,
    };
    use std::sync::Arc;
    use synctv_core::cache::{KeyBuilder, NoopCacheL2, UsernameCache};
    use synctv_core::models::{SignupMethod, User, UserId, UserRole, UserStatus};
    use synctv_core::service::{
        auth::BruteForceProtection, EmailService, EmailTokenService, InMemoryTokenBlacklistStore,
        JwtService, UserService,
    };

    fn make_user(username: &str) -> User {
        let now = chrono::Utc::now();
        User {
            id: UserId::new(),
            username: username.to_string(),
            email: Some(format!("{username}@example.com")),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            email_verified: true,
            signup_method: SignupMethod::Email,
            created_at: now,
            updated_at: now,
            password_changed_at: now,
            password_version: 0,
            version: 0,
            deleted_at: None,
        }
    }

    fn build_email_api(pool: sqlx::PgPool) -> EmailApiImpl {
        let username_cache =
            UsernameCache::new(Arc::new(NoopCacheL2), "test:username:".to_string(), 128, 60);
        let jwt_service =
            JwtService::new("test-secret-key-for-email-api-tests-minimum-32-chars").unwrap();
        let user_service = Arc::new(UserService::new(
            pool.clone(),
            jwt_service,
            username_cache,
            synctv_core::config::PasswordComplexityConfig::default(),
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
        ));

        EmailApiImpl::new(
            user_service,
            Arc::new(EmailService::new(None).unwrap()),
            Arc::new(EmailTokenService::new(pool)),
        )
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn request_password_reset_returns_same_message_for_existing_and_missing_users() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_email_api(pool.clone());
        let repo = synctv_core::repository::UserRepository::new(pool);
        let existing_user = make_user("email_api_existing");
        let existing_email = existing_user.email.clone().unwrap();
        repo.create(&existing_user).await.unwrap();

        let existing = api.request_password_reset(&existing_email).await.unwrap();
        let missing = api
            .request_password_reset("missing-email@example.com")
            .await
            .unwrap();

        assert_eq!(existing.message, GENERIC_PASSWORD_RESET_MESSAGE);
        assert_eq!(missing.message, GENERIC_PASSWORD_RESET_MESSAGE);
    }

    #[test]
    fn email_user_lookup_backend_outage_maps_to_service_unavailable() {
        let mapped =
            map_email_user_lookup_error(synctv_core::Error::Database(sqlx::Error::PoolTimedOut));

        assert!(
            matches!(mapped, crate::impls::ApiError::ServiceUnavailable(ref msg) if msg == "Service temporarily unavailable. Please try again later."),
            "email user lookup backend failures must remain service unavailable, got: {mapped:?}"
        );
    }

    #[test]
    fn email_mutation_backend_outage_maps_to_service_unavailable() {
        let mapped =
            map_email_mutation_error(synctv_core::Error::Database(sqlx::Error::PoolTimedOut));

        assert!(
            matches!(mapped, crate::impls::ApiError::ServiceUnavailable(ref msg) if msg == "Service temporarily unavailable. Please try again later."),
            "email mutation backend failures must remain service unavailable, got: {mapped:?}"
        );
    }
}
