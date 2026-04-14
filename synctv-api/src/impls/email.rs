//! Shared Email Verification and Password Reset Implementation
//!
//! Used by both HTTP and gRPC handlers to avoid duplicating email logic.

use std::sync::Arc;
use synctv_core::service::{
    rate_limit::RateLimitError, EmailService, EmailTokenService, EmailTokenType, RateLimiter,
    UserService,
};

use crate::impls::ApiError;

const GENERIC_VERIFICATION_MESSAGE: &str =
    "If an account exists with this email, a verification code will be sent.";
const GENERIC_PASSWORD_RESET_MESSAGE: &str =
    "If an account exists with this email, a password reset code will be sent.";
const GENERIC_EMAIL_LOGIN_MESSAGE: &str =
    "If an active account exists with this email, a login code will be sent.";
const EMAIL_ADDR_MAX_REQUESTS: u32 = 3;
const EMAIL_ADDR_WINDOW_SECONDS: u64 = 3600;

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
    rate_limiter: Arc<RateLimiter>,
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

/// Request email login result
pub struct RequestEmailLoginResult {
    pub message: String,
}

/// Confirm email login result
pub struct ConfirmEmailLoginResult {
    pub user: synctv_core::models::User,
    pub user_id: String,
    pub access_token: String,
    pub refresh_token: String,
}

#[must_use]
pub fn build_shared_email_api(
    user_service: Arc<UserService>,
    email_service: Option<Arc<EmailService>>,
    email_token_service: Option<Arc<EmailTokenService>>,
    rate_limiter: RateLimiter,
) -> Option<Arc<EmailApiImpl>> {
    match (email_service, email_token_service) {
        (Some(email_service), Some(email_token_service)) => Some(Arc::new(EmailApiImpl::new(
            user_service,
            email_service,
            email_token_service,
            rate_limiter,
        ))),
        _ => None,
    }
}

impl EmailApiImpl {
    fn normalize_rate_limited_email(email: &str) -> String {
        email.trim().to_ascii_lowercase()
    }

    async fn check_email_rate_limit(&self, email: &str) -> Result<(), ApiError> {
        let normalized = Self::normalize_rate_limited_email(email);
        let key = format!("email:addr:{normalized}");
        match self
            .rate_limiter
            .check_rate_limit(&key, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
            .await
        {
            Ok(()) => Ok(()),
            Err(RateLimitError::RateLimitExceeded {
                retry_after_seconds,
            }) => Err(ApiError::RateLimitedWithRetry {
                message: format!(
                    "Too many requests. Please try again in {retry_after_seconds} seconds."
                ),
                retry_after_seconds,
            }),
            Err(_) => Err(ApiError::RateLimitedWithRetry {
                message: "Too many requests. Please try again in 1 seconds.".to_string(),
                retry_after_seconds: 1,
            }),
        }
    }

    #[must_use]
    pub fn new(
        user_service: Arc<UserService>,
        email_service: Arc<EmailService>,
        email_token_service: Arc<EmailTokenService>,
        rate_limiter: RateLimiter,
    ) -> Self {
        Self {
            user_service,
            email_service,
            email_token_service,
            rate_limiter: Arc::new(rate_limiter),
        }
    }

    /// Send a verification email.
    /// Returns generic message regardless of whether user exists (anti-enumeration).
    pub async fn send_verification_email(
        &self,
        email: &str,
    ) -> Result<SendVerificationResult, ApiError> {
        self.check_email_rate_limit(email).await?;

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
        self.check_email_rate_limit(email).await?;

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

    /// Request an email login code.
    pub async fn request_email_login(
        &self,
        email: &str,
    ) -> Result<RequestEmailLoginResult, ApiError> {
        self.check_email_rate_limit(email).await?;

        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(map_email_user_lookup_error)?;

        let Some(user) = user else {
            let delay_ms = rand::random_range(100u64..500u64);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            return Ok(RequestEmailLoginResult {
                message: GENERIC_EMAIL_LOGIN_MESSAGE.to_string(),
            });
        };

        if user.is_deleted()
            || matches!(
                user.status,
                synctv_core::models::UserStatus::Banned
                    | synctv_core::models::UserStatus::Pending
                    | synctv_core::models::UserStatus::Rejected
            )
        {
            let delay_ms = rand::random_range(100u64..500u64);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            return Ok(RequestEmailLoginResult {
                message: GENERIC_EMAIL_LOGIN_MESSAGE.to_string(),
            });
        }

        let _token = self
            .email_service
            .send_email_login_email(email, &self.email_token_service, &user.id)
            .await
            .map_err(ApiError::from)?;

        tracing::info!(
            "Sent email login code to {}",
            synctv_core::service::mask_email(email)
        );

        Ok(RequestEmailLoginResult {
            message: GENERIC_EMAIL_LOGIN_MESSAGE.to_string(),
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

    /// Confirm an email login token and issue an authenticated session.
    pub async fn confirm_email_login(
        &self,
        email: &str,
        token: &str,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<ConfirmEmailLoginResult, ApiError> {
        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(map_email_user_lookup_error)?
            .ok_or_else(|| ApiError::Authentication("Invalid or expired login code".to_string()))?;

        let validated_user_id = self
            .email_token_service
            .validate_token_for_user(token, EmailTokenType::EmailLogin, &user.id)
            .await
            .map_err(|_| ApiError::Authentication("Invalid or expired login code".to_string()))?;

        if validated_user_id != user.id {
            return Err(ApiError::Authentication(
                "Invalid or expired login code".to_string(),
            ));
        }

        if user.is_deleted()
            || matches!(
                user.status,
                synctv_core::models::UserStatus::Banned
                    | synctv_core::models::UserStatus::Pending
                    | synctv_core::models::UserStatus::Rejected
            )
        {
            return Err(ApiError::Authentication(
                "Authentication failed".to_string(),
            ));
        }

        if !user.email_verified {
            self.user_service
                .set_email_verified(&user.id, true)
                .await
                .map_err(map_email_mutation_error)?;
        }

        let login_key = format!("email:{}", Self::normalize_rate_limited_email(email));
        let (user, access_token, refresh_token) = self
            .user_service
            .login_with_verified_email(&user.id, &login_key, client_ip)
            .await
            .map_err(ApiError::from)?;

        Ok(ConfirmEmailLoginResult {
            user_id: user.id.to_string(),
            user,
            access_token,
            refresh_token,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        map_email_mutation_error, map_email_user_lookup_error, EmailApiImpl,
        GENERIC_EMAIL_LOGIN_MESSAGE, GENERIC_PASSWORD_RESET_MESSAGE,
    };
    use std::sync::Arc;
    use synctv_core::cache::{KeyBuilder, UsernameCache};
    use synctv_core::models::{SignupMethod, User, UserId, UserRole, UserStatus};
    use synctv_core::service::{
        auth::BruteForceProtection, EmailService, EmailTokenService, InMemoryTokenBlacklistStore,
        JwtService, RateLimiter, UserService,
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

    fn build_test_email_api(pool: sqlx::PgPool) -> EmailApiImpl {
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
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
            RateLimiter::local_only("email-api-tests:".to_string()),
        )
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn request_password_reset_returns_same_message_for_existing_and_missing_users() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool.clone());
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

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn send_verification_email_rate_limits_per_normalized_email_across_calls() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool);

        for email in [
            "Target@example.com",
            " target@example.com ",
            "TARGET@example.com",
        ] {
            let result = api.send_verification_email(email).await.unwrap();
            assert_eq!(result.message, super::GENERIC_VERIFICATION_MESSAGE);
        }

        let err = match api.send_verification_email("target@example.com").await {
            Ok(result) => panic!("expected rate limiting, got success: {}", result.message),
            Err(err) => err,
        };
        assert!(
            matches!(
                err,
                crate::impls::ApiError::RateLimitedWithRetry {
                    message: ref msg,
                    retry_after_seconds,
                } if msg.contains("Please try again in") && retry_after_seconds > 0
            ),
            "expected rate limited error, got: {err:?}"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn confirm_email_login_accepts_multiple_outstanding_codes_for_same_user() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool.clone());
        let repo = synctv_core::repository::UserRepository::new(pool.clone());

        let user = make_user("email_login_multi");
        let email = user.email.clone().unwrap();
        let created = repo.create(&user).await.unwrap();

        let first = api
            .email_token_service
            .generate_token(
                &created.id,
                synctv_core::service::EmailTokenType::EmailLogin,
            )
            .await
            .unwrap();
        let second = api
            .email_token_service
            .generate_token(
                &created.id,
                synctv_core::service::EmailTokenType::EmailLogin,
            )
            .await
            .unwrap();

        let first_login = api.confirm_email_login(&email, &first, None).await.unwrap();
        assert_eq!(
            first_login.user_id,
            created.id.to_string(),
            "first login code should authenticate the target user"
        );
        assert!(!first_login.access_token.is_empty());
        assert!(!first_login.refresh_token.is_empty());

        let second_login = api
            .confirm_email_login(&email, &second, None)
            .await
            .unwrap();
        assert_eq!(
            second_login.user_id,
            created.id.to_string(),
            "second outstanding login code should remain usable"
        );
        assert!(!second_login.access_token.is_empty());
        assert!(!second_login.refresh_token.is_empty());
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn request_email_login_returns_same_message_for_existing_and_missing_users() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool.clone());
        let repo = synctv_core::repository::UserRepository::new(pool);
        let existing_user = make_user("email_login_existing");
        let existing_email = existing_user.email.clone().unwrap();
        repo.create(&existing_user).await.unwrap();

        let existing = api.request_email_login(&existing_email).await.unwrap();
        let missing = api
            .request_email_login("missing-login@example.com")
            .await
            .unwrap();

        assert_eq!(existing.message, GENERIC_EMAIL_LOGIN_MESSAGE);
        assert_eq!(missing.message, GENERIC_EMAIL_LOGIN_MESSAGE);
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
