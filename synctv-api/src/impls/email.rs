//! Shared Email Verification and Password Reset Implementation
//!
//! Used by both HTTP and gRPC handlers to avoid duplicating email logic.

use std::sync::Arc;
use synctv_core::models::EmailTokenType;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::{
    rate_limit::RateLimitError, AuthFactorMethod, AuthenticatedLogin, EmailService,
    EmailTokenService, RequestRateLimiterService, UserService,
};
use synctv_proto::client::{
    ConfirmPasswordResetResponse, FinishOpaquePasswordResetRequest, RequestMfaEmailCodeRequest,
    RequestMfaEmailCodeResponse, RequestPasswordResetRequest, RequestPasswordResetResponse,
    StartOpaquePasswordResetRequest, StartOpaquePasswordResetResponse, VerifyMfaEmailCodeRequest,
};

use crate::impls::ApiError;

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

#[async_trait::async_trait]
pub trait EmailDeliveryService: Send + Sync {
    async fn send_email_bind_token_email_with_control(
        &self,
        email: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> synctv_core::Result<()>;

    async fn send_password_reset_email_with_control(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &synctv_core::models::UserId,
        control: Option<&ExecutionControl>,
    ) -> synctv_core::Result<String>;

    async fn send_email_login_email_with_control(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &synctv_core::models::UserId,
        control: Option<&ExecutionControl>,
    ) -> synctv_core::Result<String>;
}

#[async_trait::async_trait]
impl EmailDeliveryService for EmailService {
    async fn send_email_bind_token_email_with_control(
        &self,
        email: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> synctv_core::Result<()> {
        EmailService::send_email_bind_token_email_with_control(self, email, token, control).await
    }

    async fn send_password_reset_email_with_control(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &synctv_core::models::UserId,
        control: Option<&ExecutionControl>,
    ) -> synctv_core::Result<String> {
        EmailService::send_password_reset_email_with_control(
            self,
            email,
            token_service,
            user_id,
            control,
        )
        .await
    }

    async fn send_email_login_email_with_control(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &synctv_core::models::UserId,
        control: Option<&ExecutionControl>,
    ) -> synctv_core::Result<String> {
        EmailService::send_email_login_email_with_control(
            self,
            email,
            token_service,
            user_id,
            control,
        )
        .await
    }
}

/// Shared email operations implementation.
pub struct EmailApiImpl {
    pub user_service: Arc<UserService>,
    pub email_service: Arc<dyn EmailDeliveryService>,
    pub email_token_service: Arc<EmailTokenService>,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
    public_id_codec: Arc<crate::PublicIdCodec>,
}

/// Request password reset result
pub struct RequestPasswordResetResult {
    pub message: String,
}

pub struct StartOpaquePasswordResetResult {
    pub session_id: String,
    pub registration_response: Vec<u8>,
}

/// Finish password reset result
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
    pub user_id: String,
    pub login: AuthenticatedLogin,
}

pub struct RequestMfaEmailCodeResult {
    pub message: String,
    pub masked_email: String,
}

#[must_use]
pub fn build_shared_email_api(
    user_service: Arc<UserService>,
    email_service: Option<Arc<EmailService>>,
    email_token_service: Option<Arc<EmailTokenService>>,
    rate_limiter: impl RequestRateLimiterService + 'static,
    public_id_codec: Arc<crate::PublicIdCodec>,
) -> Option<Arc<EmailApiImpl>> {
    match (email_service, email_token_service) {
        (Some(email_service), Some(email_token_service)) => Some(Arc::new(EmailApiImpl::new(
            user_service,
            email_service,
            email_token_service,
            rate_limiter,
            public_id_codec,
        ))),
        _ => None,
    }
}

impl EmailApiImpl {
    async fn sleep_with_control(
        delay: std::time::Duration,
        control: Option<&ExecutionControl>,
    ) -> Result<(), ApiError> {
        match control {
            Some(control) => control
                .run(tokio::time::sleep(delay))
                .await
                .map_err(|error| ApiError::from(synctv_core::Error::from(error)))?,
            None => tokio::time::sleep(delay).await,
        }

        Ok(())
    }

    fn normalize_rate_limited_email(email: &str) -> String {
        email.trim().to_ascii_lowercase()
    }

    async fn check_email_rate_limit(
        &self,
        email: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<(), ApiError> {
        let normalized = Self::normalize_rate_limited_email(email);
        let key = format!("email:addr:{normalized}");
        match self
            .rate_limiter
            .check_rate_limit_with_control(
                &key,
                EMAIL_ADDR_MAX_REQUESTS,
                EMAIL_ADDR_WINDOW_SECONDS,
                control,
            )
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

    pub async fn check_email_delivery_rate_limits(
        &self,
        email: &str,
        user_id: &synctv_core::models::UserId,
        token_type: EmailTokenType,
        control: Option<&ExecutionControl>,
    ) -> Result<(), ApiError> {
        self.check_email_rate_limit(email, control).await?;
        self.email_token_service
            .check_generate_token_rate_limit_with_control(user_id, token_type, control)
            .await
            .map_err(ApiError::from)
    }

    #[must_use]
    pub fn new(
        user_service: Arc<UserService>,
        email_service: Arc<dyn EmailDeliveryService>,
        email_token_service: Arc<EmailTokenService>,
        rate_limiter: impl RequestRateLimiterService + 'static,
        public_id_codec: Arc<crate::PublicIdCodec>,
    ) -> Self {
        Self {
            user_service,
            email_service,
            email_token_service,
            rate_limiter: Arc::new(rate_limiter),
            public_id_codec,
        }
    }

    fn public_user_id(&self, user_id: synctv_core::models::UserId) -> String {
        self.public_id_codec
            .encode_user_id(user_id)
            .expect("positive user ID must encode")
    }

    /// Request a password reset email.
    /// Returns generic message regardless of whether user exists (anti-enumeration).
    pub async fn request_password_reset(
        &self,
        email: &str,
    ) -> Result<RequestPasswordResetResult, ApiError> {
        self.request_password_reset_with_control(email, None).await
    }

    pub async fn request_password_reset_with_control(
        &self,
        email: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<RequestPasswordResetResult, ApiError> {
        self.check_email_rate_limit(email, control).await?;

        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(map_email_user_lookup_error)?;

        let Some(user) = user else {
            // Add random delay to prevent timing side-channel that leaks
            // whether an account exists based on response time differences.
            let delay_ms = rand::random_range(100u64..500u64);
            Self::sleep_with_control(std::time::Duration::from_millis(delay_ms), control).await?;
            return Ok(RequestPasswordResetResult {
                message: GENERIC_PASSWORD_RESET_MESSAGE.to_string(),
            });
        };

        let _token = self
            .email_service
            .send_password_reset_email_with_control(
                email,
                &self.email_token_service,
                &user.id,
                control,
            )
            .await
            .map_err(ApiError::from)?;

        tracing::info!("Password reset requested for user {}", user.id);

        Ok(RequestPasswordResetResult {
            message: GENERIC_PASSWORD_RESET_MESSAGE.to_string(),
        })
    }

    pub async fn request_password_reset_response_with_control(
        &self,
        req: RequestPasswordResetRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<RequestPasswordResetResponse, ApiError> {
        let result = self
            .request_password_reset_with_control(&req.email, control)
            .await?;
        Ok(RequestPasswordResetResponse {
            message: result.message,
        })
    }

    /// Request an email login code.
    pub async fn request_email_login(
        &self,
        email: &str,
    ) -> Result<RequestEmailLoginResult, ApiError> {
        self.request_email_login_with_control(email, None).await
    }

    pub async fn request_email_login_with_control(
        &self,
        email: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<RequestEmailLoginResult, ApiError> {
        self.check_email_rate_limit(email, control).await?;

        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(map_email_user_lookup_error)?;

        let Some(user) = user else {
            let delay_ms = rand::random_range(100u64..500u64);
            Self::sleep_with_control(std::time::Duration::from_millis(delay_ms), control).await?;
            return Ok(RequestEmailLoginResult {
                message: GENERIC_EMAIL_LOGIN_MESSAGE.to_string(),
            });
        };

        if user.is_deleted() || matches!(user.status, synctv_core::models::UserStatus::Banned) {
            let delay_ms = rand::random_range(100u64..500u64);
            Self::sleep_with_control(std::time::Duration::from_millis(delay_ms), control).await?;
            return Ok(RequestEmailLoginResult {
                message: GENERIC_EMAIL_LOGIN_MESSAGE.to_string(),
            });
        }

        let _token = self
            .email_service
            .send_email_login_email_with_control(
                email,
                &self.email_token_service,
                &user.id,
                control,
            )
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

    /// Start a password reset by consuming the email token and creating an
    /// OPAQUE registration session for the replacement password.
    pub async fn start_opaque_password_reset(
        &self,
        email: &str,
        token: &str,
        registration_request: Vec<u8>,
    ) -> Result<StartOpaquePasswordResetResult, ApiError> {
        self.start_opaque_password_reset_with_control(email, token, registration_request, None)
            .await
    }

    pub async fn start_opaque_password_reset_with_control(
        &self,
        email: &str,
        token: &str,
        registration_request: Vec<u8>,
        control: Option<&ExecutionControl>,
    ) -> Result<StartOpaquePasswordResetResult, ApiError> {
        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(map_email_user_lookup_error)?
            .ok_or_else(|| ApiError::InvalidInput("Invalid or expired reset token".to_string()))?;

        let validated_user_id = self
            .email_token_service
            .validate_token_for_user_with_control(
                token,
                EmailTokenType::PasswordReset,
                &user.id,
                control,
            )
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

        self.email_token_service
            .invalidate_user_tokens_with_control(&user.id, EmailTokenType::PasswordReset, control)
            .await
            .map_err(map_email_mutation_error)?;

        let challenge = self
            .user_service
            .start_opaque_password_reset_after_external_verification(&user.id, registration_request)
            .await
            .map_err(map_email_mutation_error)?;

        Ok(StartOpaquePasswordResetResult {
            session_id: challenge.session_id,
            registration_response: challenge.registration_response,
        })
    }

    pub async fn start_opaque_password_reset_response_with_control(
        &self,
        req: StartOpaquePasswordResetRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<StartOpaquePasswordResetResponse, ApiError> {
        let result = self
            .start_opaque_password_reset_with_control(
                &req.email,
                &req.token,
                req.registration_request,
                control,
            )
            .await?;
        Ok(StartOpaquePasswordResetResponse {
            session_id: result.session_id,
            registration_response: result.registration_response,
        })
    }

    pub async fn finish_opaque_password_reset(
        &self,
        session_id: &str,
        registration_upload: Vec<u8>,
    ) -> Result<ConfirmPasswordResetResult, ApiError> {
        self.finish_opaque_password_reset_with_control(session_id, registration_upload, None)
            .await
    }

    pub async fn finish_opaque_password_reset_with_control(
        &self,
        session_id: &str,
        registration_upload: Vec<u8>,
        _control: Option<&ExecutionControl>,
    ) -> Result<ConfirmPasswordResetResult, ApiError> {
        let user = self
            .user_service
            .finish_opaque_password_reset_after_external_verification(
                session_id,
                registration_upload,
            )
            .await
            .map_err(map_email_mutation_error)?;

        tracing::info!("Password reset completed for user {}", user.id);

        Ok(ConfirmPasswordResetResult {
            message: "Password reset successfully".to_string(),
            user_id: self.public_user_id(user.id),
        })
    }

    pub async fn finish_opaque_password_reset_response_with_control(
        &self,
        req: FinishOpaquePasswordResetRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<ConfirmPasswordResetResponse, ApiError> {
        let result = self
            .finish_opaque_password_reset_with_control(
                &req.session_id,
                req.registration_upload,
                control,
            )
            .await?;
        Ok(ConfirmPasswordResetResponse {
            message: result.message,
            user_id: result.user_id,
        })
    }

    /// Confirm an email login token and issue an authenticated session.
    pub async fn confirm_email_login(
        &self,
        email: &str,
        token: &str,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<ConfirmEmailLoginResult, ApiError> {
        self.confirm_email_login_with_control(email, token, client_ip, None)
            .await
    }

    pub async fn confirm_email_login_with_control(
        &self,
        email: &str,
        token: &str,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<ConfirmEmailLoginResult, ApiError> {
        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(map_email_user_lookup_error)?
            .ok_or_else(|| ApiError::Authentication("Invalid or expired login code".to_string()))?;

        let validated_user_id = self
            .email_token_service
            .validate_token_for_user_with_control(
                token,
                EmailTokenType::EmailLogin,
                &user.id,
                control,
            )
            .await
            .map_err(|_| ApiError::Authentication("Invalid or expired login code".to_string()))?;

        if validated_user_id != user.id {
            return Err(ApiError::Authentication(
                "Invalid or expired login code".to_string(),
            ));
        }

        if user.is_deleted() || matches!(user.status, synctv_core::models::UserStatus::Banned) {
            return Err(ApiError::Authentication(
                "Authentication failed".to_string(),
            ));
        }

        let login_key = format!("email:{}", Self::normalize_rate_limited_email(email));
        let login = self
            .user_service
            .login_with_verified_email_with_control(&user.id, &login_key, client_ip, control)
            .await
            .map_err(ApiError::from)?;

        Ok(ConfirmEmailLoginResult {
            user_id: self.public_user_id(user.id),
            login,
        })
    }

    pub async fn request_mfa_email_code_with_control(
        &self,
        mfa_session_id: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<RequestMfaEmailCodeResult, ApiError> {
        let user = self
            .user_service
            .get_mfa_session_user_for_method(mfa_session_id, AuthFactorMethod::Email)
            .await
            .map_err(ApiError::from)?;
        let email = user
            .email
            .as_deref()
            .ok_or_else(|| ApiError::Authentication("Authentication failed".to_string()))?;
        self.check_email_rate_limit(email, control).await?;
        let _token = self
            .email_service
            .send_email_login_email_with_control(
                email,
                &self.email_token_service,
                &user.id,
                control,
            )
            .await
            .map_err(ApiError::from)?;
        Ok(RequestMfaEmailCodeResult {
            message: GENERIC_EMAIL_LOGIN_MESSAGE.to_string(),
            masked_email: synctv_core::service::mask_email(email),
        })
    }

    pub async fn request_mfa_email_code_response_with_control(
        &self,
        req: RequestMfaEmailCodeRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<RequestMfaEmailCodeResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let result = self
            .request_mfa_email_code_with_control(&req.mfa_session_id, control)
            .await?;

        Ok(RequestMfaEmailCodeResponse {
            message: result.message,
            masked_email: result.masked_email,
        })
    }

    pub async fn verify_mfa_email_code_with_control(
        &self,
        mfa_session_id: &str,
        token: &str,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin, ApiError> {
        let user = self
            .user_service
            .get_mfa_session_user_for_method(mfa_session_id, AuthFactorMethod::Email)
            .await
            .map_err(ApiError::from)?;
        self.email_token_service
            .validate_token_for_user_with_control(
                token,
                EmailTokenType::EmailLogin,
                &user.id,
                control,
            )
            .await
            .map_err(|_| ApiError::Authentication("Invalid or expired login code".to_string()))?;
        self.user_service
            .complete_mfa_session_with_control(
                mfa_session_id,
                AuthFactorMethod::Email,
                client_ip,
                control,
            )
            .await
            .map_err(ApiError::from)
    }

    pub async fn verify_mfa_email_code_request_with_control(
        &self,
        req: VerifyMfaEmailCodeRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.verify_mfa_email_code_with_control(
            &req.mfa_session_id,
            &req.email_token,
            client_ip,
            control,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        map_email_mutation_error, map_email_user_lookup_error, EmailApiImpl, EmailDeliveryService,
        GENERIC_EMAIL_LOGIN_MESSAGE, GENERIC_PASSWORD_RESET_MESSAGE,
    };
    use std::sync::Arc;
    use synctv_core::cache::{KeyBuilder, UsernameCache};
    use synctv_core::models::{EmailTokenType, SignupMethod, User, UserId, UserRole, UserStatus};
    use synctv_core::service::AuthenticatedLogin;
    use synctv_core::service::{
        auth::BruteForceProtection, EmailTokenService, InMemoryTokenBlacklistStore, JwtService,
        RateLimiter, UserService,
    };

    #[derive(Clone)]
    struct TestEmailDeliveryService;

    #[async_trait::async_trait]
    impl EmailDeliveryService for TestEmailDeliveryService {
        async fn send_email_bind_token_email_with_control(
            &self,
            _email: &str,
            _token: &str,
            _control: Option<&synctv_core::provider::ExecutionControl>,
        ) -> synctv_core::Result<()> {
            Ok(())
        }

        async fn send_password_reset_email_with_control(
            &self,
            _email: &str,
            token_service: &EmailTokenService,
            user_id: &UserId,
            _control: Option<&synctv_core::provider::ExecutionControl>,
        ) -> synctv_core::Result<String> {
            token_service
                .generate_token(user_id, EmailTokenType::PasswordReset)
                .await
        }

        async fn send_email_login_email_with_control(
            &self,
            _email: &str,
            token_service: &EmailTokenService,
            user_id: &UserId,
            _control: Option<&synctv_core::provider::ExecutionControl>,
        ) -> synctv_core::Result<String> {
            token_service
                .generate_token(user_id, EmailTokenType::EmailLogin)
                .await
        }
    }

    fn make_user(username: &str) -> User {
        let now = chrono::Utc::now();
        User {
            id: UserId::new(),
            username: username.to_string(),
            email: Some(format!("{username}@example.com")),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
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
        let mut user_service = UserService::new(
            &pool,
            jwt_service,
            username_cache,
            synctv_core::config::PasswordComplexityConfig::default(),
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
        );
        user_service.enable_password_registration_for_tests();
        user_service.enable_legacy_password_login_for_tests();
        user_service.enable_legacy_password_registration_for_tests();
        let user_service = Arc::new(user_service);

        EmailApiImpl::new(
            user_service,
            Arc::new(TestEmailDeliveryService),
            Arc::new(EmailTokenService::new(pool)),
            RateLimiter::local_only("email-api-tests:".to_string()),
            Arc::new(crate::PublicIdCodec::default_for_tests()),
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
    async fn confirm_email_login_accepts_multiple_outstanding_codes_for_same_user() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool.clone());
        let repo = synctv_core::repository::UserRepository::new(pool.clone());

        let user = make_user("email_login_multi");
        let email = user.email.clone().unwrap();
        let created = repo.create(&user).await.unwrap();

        let first = api
            .email_token_service
            .generate_token(&created.id, synctv_core::models::EmailTokenType::EmailLogin)
            .await
            .unwrap();
        let second = api
            .email_token_service
            .generate_token(&created.id, synctv_core::models::EmailTokenType::EmailLogin)
            .await
            .unwrap();

        let first_login = api.confirm_email_login(&email, &first, None).await.unwrap();
        assert_eq!(
            first_login.user_id,
            api.public_user_id(created.id),
            "first login code should authenticate the target user"
        );
        assert!(
            matches!(
                first_login.login,
                AuthenticatedLogin::Complete {
                    access_token,
                    refresh_token,
                    ..
                } if !access_token.is_empty() && !refresh_token.is_empty()
            ),
            "first login code should issue tokens"
        );

        let second_login = api
            .confirm_email_login(&email, &second, None)
            .await
            .unwrap();
        assert_eq!(
            second_login.user_id,
            api.public_user_id(created.id),
            "second outstanding login code should remain usable"
        );
        assert!(
            matches!(
                second_login.login,
                AuthenticatedLogin::Complete {
                    access_token,
                    refresh_token,
                    ..
                } if !access_token.is_empty() && !refresh_token.is_empty()
            ),
            "second login code should issue tokens"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn mfa_email_code_flow_completes_password_first_factor_login() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool);
        let email = format!("mfa_{}@example.com", synctv_common::snanoid!(8));
        let user = api
            .user_service
            .create_user_with_role(
                "mfa_email_user".to_string(),
                Some(email.clone()),
                "StrongPass1".to_string(),
                None,
            )
            .await
            .unwrap();
        api.user_service
            .set_two_factor_enabled(&user.id, true)
            .await
            .unwrap();

        let first_factor = api
            .user_service
            .login(
                "mfa_email_user".to_string(),
                "StrongPass1".to_string(),
                None,
            )
            .await
            .unwrap();
        let AuthenticatedLogin::MfaRequired { challenge, .. } = first_factor else {
            panic!("password first factor should require MFA");
        };
        assert!(challenge
            .available_methods
            .contains(&synctv_core::service::AuthFactorMethod::Email));

        let request = api
            .request_mfa_email_code_with_control(&challenge.session_id, None)
            .await
            .unwrap();
        assert_eq!(
            request.masked_email,
            synctv_core::service::mask_email(&email)
        );

        let token = api
            .email_token_service
            .generate_token(&user.id, synctv_core::models::EmailTokenType::EmailLogin)
            .await
            .unwrap();
        let completed = api
            .verify_mfa_email_code_with_control(&challenge.session_id, &token, None, None)
            .await
            .unwrap();
        assert!(
            matches!(
                completed,
                AuthenticatedLogin::Complete {
                    access_token,
                    refresh_token,
                    ..
                } if !access_token.is_empty() && !refresh_token.is_empty()
            ),
            "email MFA verification should issue tokens"
        );
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
