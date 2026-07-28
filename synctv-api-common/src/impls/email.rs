//! Shared Email Verification and Password Reset Implementation
//!
//! Used by both HTTP and gRPC handlers to avoid duplicating email logic.

use std::sync::Arc;
use synctv_core::models::EmailTokenType;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::{
    AuthFactorMethod, AuthenticatedLogin, EmailOutboxService, EmailTokenService, RateLimitError,
    RequestRateLimiterService, UserService,
};
use synctv_proto::client::{
    ConfirmPasswordResetResponse, FinishOpaquePasswordResetRequest,
    RequestEmailRegistrationRequest, RequestMfaEmailCodeRequest, RequestMfaEmailCodeResponse,
    RequestPasswordResetRequest, RequestPasswordResetResponse, StartOpaquePasswordResetRequest,
    StartOpaquePasswordResetResponse, VerifyMfaEmailCodeRequest,
};

use crate::impls::ApiError;

const GENERIC_PASSWORD_RESET_MESSAGE: &str =
    "If an account exists with this email, a password reset code will be sent.";
const GENERIC_EMAIL_LOGIN_MESSAGE: &str =
    "If an active account exists with this email, a login code will be sent.";
const GENERIC_EMAIL_REGISTRATION_MESSAGE: &str =
    "If registration is available for this email, a registration code will be sent.";
const EMAIL_ADDR_MAX_REQUESTS: u32 = 3;
const EMAIL_ADDR_WINDOW_SECONDS: u64 = 3600;
const EMAIL_LOGIN_MIN_RESPONSE_TIME: std::time::Duration = std::time::Duration::from_millis(500);
const PASSWORD_RESET_MIN_RESPONSE_TIME: std::time::Duration = std::time::Duration::from_millis(500);

fn map_email_user_lookup_error(err: synctv_core::Error) -> ApiError {
    ApiError::from(err)
}

fn map_email_mutation_error(err: synctv_core::Error) -> ApiError {
    ApiError::from(err)
}

/// Shared email operations implementation.
#[derive(Clone)]
pub struct EmailApiImpl {
    pub user_service: Arc<UserService>,
    pub email_token_service: Arc<EmailTokenService>,
    email_outbox_service: Arc<EmailOutboxService>,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
    public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
}

/// Request password reset result
pub struct RequestPasswordResetResult {
    pub message: String,
}

pub struct StartOpaquePasswordResetResult {
    pub session_id: String,
    pub registration_response: bytes::Bytes,
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

pub struct RequestEmailRegistrationResult {
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

impl EmailApiImpl {
    pub(crate) async fn complete_email_login_timing_with_control(
        started_at: std::time::Instant,
        control: Option<&ExecutionControl>,
    ) -> Result<(), ApiError> {
        Self::sleep_with_control(
            EMAIL_LOGIN_MIN_RESPONSE_TIME.saturating_sub(started_at.elapsed()),
            control,
        )
        .await
    }

    pub async fn request_decoy_email_login_with_control(
        &self,
        rate_limit_key: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<RequestEmailLoginResult, ApiError> {
        let started_at = std::time::Instant::now();
        let result = self
            .check_email_rate_limit_key(rate_limit_key, control)
            .await
            .map(|()| RequestEmailLoginResult {
                message: GENERIC_EMAIL_LOGIN_MESSAGE.to_string(),
            });
        Self::complete_email_login_timing_with_control(started_at, control).await?;
        result
    }

    pub fn reject_decoy_email_login(&self) -> Result<(), ApiError> {
        Err(ApiError::Authentication(
            "Invalid or expired login code".to_string(),
        ))
    }

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
        self.check_email_rate_limit_key(&key, control).await
    }

    async fn check_email_rate_limit_key(
        &self,
        key: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<(), ApiError> {
        match self
            .rate_limiter
            .check_rate_limit_with_control(
                key,
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

    pub async fn check_email_address_rate_limit(
        &self,
        email: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<(), ApiError> {
        self.check_email_rate_limit(email, control).await
    }

    #[must_use]
    pub fn new(
        user_service: Arc<UserService>,
        email_token_service: Arc<EmailTokenService>,
        email_outbox_service: Arc<EmailOutboxService>,
        rate_limiter: Arc<dyn RequestRateLimiterService>,
        public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    ) -> Self {
        Self {
            user_service,
            email_token_service,
            email_outbox_service,
            rate_limiter,
            public_id_codec,
        }
    }

    fn public_user_id(&self, user_id: synctv_core::models::UserId) -> Result<String, ApiError> {
        self.public_id_codec
            .encode_user_id(user_id)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode user public id: {error}"))
            })
    }

    pub async fn enqueue_tokenized_email_with_control(
        &self,
        email: &str,
        user_id: &synctv_core::models::UserId,
        token_type: EmailTokenType,
        control: Option<&ExecutionControl>,
    ) -> Result<String, ApiError> {
        self.email_token_service
            .generate_token_and_enqueue_with_control(
                &self.email_outbox_service,
                email,
                user_id,
                token_type,
                control,
            )
            .await
            .map_err(ApiError::from)?;

        Ok(String::new())
    }

    pub async fn start_email_bind_and_enqueue(
        &self,
        email: &str,
        user_id: &synctv_core::models::UserId,
    ) -> Result<(), ApiError> {
        self.user_service
            .start_email_bind_and_enqueue(&self.email_outbox_service, user_id, email)
            .await
            .map_err(ApiError::from)
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
        let started_at = std::time::Instant::now();
        let result = async {
            self.check_email_rate_limit(email, control).await?;
            if let Some(user) = self
                .user_service
                .get_by_email(email)
                .await
                .map_err(ApiError::from)?
            {
                self.email_token_service
                    .generate_token_and_enqueue_with_control(
                        &self.email_outbox_service,
                        email,
                        &user.id,
                        EmailTokenType::PasswordReset,
                        control,
                    )
                    .await
                    .map_err(ApiError::from)?;
            }
            Ok::<_, ApiError>(RequestPasswordResetResult {
                message: GENERIC_PASSWORD_RESET_MESSAGE.to_string(),
            })
        }
        .await;
        Self::sleep_with_control(
            PASSWORD_RESET_MIN_RESPONSE_TIME.saturating_sub(started_at.elapsed()),
            control,
        )
        .await?;
        result
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
        user_id: &synctv_core::models::UserId,
        email: &str,
        rate_limit_key: &str,
    ) -> Result<RequestEmailLoginResult, ApiError> {
        self.request_email_login_with_control(user_id, email, rate_limit_key, None)
            .await
    }

    pub async fn request_email_login_with_control(
        &self,
        user_id: &synctv_core::models::UserId,
        email: &str,
        rate_limit_key: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<RequestEmailLoginResult, ApiError> {
        let started_at = std::time::Instant::now();
        let result = async {
            self.check_email_rate_limit_key(rate_limit_key, control)
                .await?;
            let active_binding = match self.user_service.get_user_with_email(user_id).await {
                Ok(current) => {
                    !current.user.is_deleted()
                        && !matches!(current.user.status, synctv_core::models::UserStatus::Banned)
                        && current
                            .email
                            .as_deref()
                            .is_some_and(|current| current.eq_ignore_ascii_case(email))
                }
                Err(synctv_core::Error::NotFound(_)) => false,
                Err(error) => return Err(ApiError::from(error)),
            };
            if active_binding {
                self.email_token_service
                    .generate_token_and_enqueue_with_control(
                        &self.email_outbox_service,
                        email,
                        user_id,
                        EmailTokenType::EmailLogin,
                        control,
                    )
                    .await
                    .map_err(ApiError::from)?;
            }
            Ok::<(), ApiError>(())
        }
        .await;
        Self::complete_email_login_timing_with_control(started_at, control).await?;
        result.map(|()| RequestEmailLoginResult {
            message: GENERIC_EMAIL_LOGIN_MESSAGE.to_string(),
        })
    }

    pub async fn request_email_registration_with_control(
        &self,
        req: RequestEmailRegistrationRequest,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<RequestEmailRegistrationResult, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let username = crate::impls::validation::validate_username(&req.username)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        let email = crate::impls::validation::validate_email(&req.email)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;

        self.check_email_rate_limit(&email, control).await?;
        self.user_service
            .create_email_registration_and_enqueue_with_control(
                &self.email_outbox_service,
                username,
                email,
                client_ip,
                control,
            )
            .await
            .map_err(ApiError::from)?;

        Ok(RequestEmailRegistrationResult {
            message: GENERIC_EMAIL_REGISTRATION_MESSAGE.to_string(),
        })
    }

    /// Start a password reset by consuming the email token and creating an
    /// OPAQUE registration session for the replacement password.
    pub async fn start_opaque_password_reset(
        &self,
        email: &str,
        token: &str,
        registration_request: bytes::Bytes,
    ) -> Result<StartOpaquePasswordResetResult, ApiError> {
        self.start_opaque_password_reset_with_control(email, token, registration_request, None)
            .await
    }

    pub async fn start_opaque_password_reset_with_control(
        &self,
        email: &str,
        token: &str,
        registration_request: bytes::Bytes,
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
        registration_upload: bytes::Bytes,
    ) -> Result<ConfirmPasswordResetResult, ApiError> {
        self.finish_opaque_password_reset_with_control(session_id, registration_upload, None)
            .await
    }

    pub async fn finish_opaque_password_reset_with_control(
        &self,
        session_id: &str,
        registration_upload: bytes::Bytes,
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
            user_id: self.public_user_id(user.id)?,
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
        user_id: &synctv_core::models::UserId,
        email: &str,
        token: &str,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<ConfirmEmailLoginResult, ApiError> {
        self.confirm_email_login_with_control(user_id, email, token, client_ip, None)
            .await
    }

    pub async fn confirm_email_login_with_control(
        &self,
        user_id: &synctv_core::models::UserId,
        email: &str,
        token: &str,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<ConfirmEmailLoginResult, ApiError> {
        self.validate_email_login_token_with_control(user_id, token, control)
            .await?;
        self.complete_verified_email_login_with_control(user_id, email, client_ip, control)
            .await
    }

    pub async fn validate_email_login_token_with_control(
        &self,
        user_id: &synctv_core::models::UserId,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<(), ApiError> {
        let validated_user_id = self
            .email_token_service
            .validate_token_for_user_with_control(
                token,
                EmailTokenType::EmailLogin,
                user_id,
                control,
            )
            .await
            .map_err(|_| ApiError::Authentication("Invalid or expired login code".to_string()))?;

        if validated_user_id != *user_id {
            return Err(ApiError::Authentication(
                "Invalid or expired login code".to_string(),
            ));
        }

        Ok(())
    }

    pub async fn complete_verified_email_login_with_control(
        &self,
        user_id: &synctv_core::models::UserId,
        email: &str,
        client_ip: Option<std::net::IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<ConfirmEmailLoginResult, ApiError> {
        let login_key = format!("email:{}", Self::normalize_rate_limited_email(email));
        let login = self
            .user_service
            .login_with_verified_email_with_control(user_id, email, &login_key, client_ip, control)
            .await
            .map_err(ApiError::from)?;

        Ok(ConfirmEmailLoginResult {
            user_id: self.public_user_id(*user_id)?,
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
        let email = self
            .user_service
            .get_email(&user.id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::Authentication("Authentication failed".to_string()))?;
        let email = email.as_str();
        self.check_email_rate_limit(email, control).await?;
        let _token = self
            .enqueue_tokenized_email_with_control(
                email,
                &user.id,
                EmailTokenType::EmailLogin,
                control,
            )
            .await?;
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
        map_email_mutation_error, map_email_user_lookup_error, EmailApiImpl,
        GENERIC_EMAIL_LOGIN_MESSAGE, GENERIC_PASSWORD_RESET_MESSAGE,
    };
    use std::sync::Arc;
    use synctv_core::cache::{KeyBuilder, UsernameCache};
    use synctv_core::models::{SignupMethod, User, UserId, UserRole, UserStatus};
    use synctv_core::service::AuthenticatedLogin;
    use synctv_core::service::{
        BruteForceProtection, EmailOutboxService, EmailTokenService, InMemoryTokenBlacklistStore,
        JwtService, RateLimiter, UserService, UserServiceRuntimeOptions,
    };

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn api_ok<T>(result: Result<T, crate::impls::ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    fn make_user(username: &str) -> User {
        let now = synctv_core::SystemClock.now();
        User {
            id: UserId::new(),
            username: username.to_string(),
            role: UserRole::User,
            avatar_file_reference_id: None,
            status: UserStatus::Active,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
            signup_method: SignupMethod::Email,
            created_at: now,
            updated_at: now,
            version: 0,
            deleted_at: None,
        }
    }

    fn build_test_email_api(pool: sqlx::PgPool) -> TestResult<EmailApiImpl> {
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
        let jwt_service = JwtService::new("test-secret-key-for-email-api-tests-minimum-32-chars")?;
        let user_service = UserService::new_with_runtime(
            &pool,
            jwt_service,
            username_cache,
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
            UserServiceRuntimeOptions {
                password_registration_policy_override: Some(
                    synctv_core::service::RegistrationPolicy {
                        enabled: true,
                        need_review: false,
                    },
                ),
                ..synctv_core::service::UserServiceRuntimeOptions::test_defaults()
            },
        );
        let user_service = Arc::new(user_service);
        let outbox = Arc::new(EmailOutboxService::new(
            pool.clone(),
            b"email-api-test-outbox-secret",
        )?);

        Ok(EmailApiImpl::new(
            user_service,
            Arc::new(EmailTokenService::new(pool)),
            outbox,
            Arc::new(RateLimiter::local_only("email-api-tests:".to_string())),
            Arc::new(synctv_adapter::PublicIdCodec::plain()),
        ))
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn request_password_reset_returns_same_message_for_existing_and_missing_users(
    ) -> TestResult {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool.clone())?;
        let repo = synctv_core::repository::UserRepository::new(pool.clone());
        let email_repo = synctv_core::repository::UserEmailRepository::new(pool.clone());
        let existing_user = make_user("email_api_existing");
        let existing_email = "email_api_existing@example.com".to_string();
        let created = repo.create(&existing_user).await?;
        core_ok(
            email_repo
                .create_for_user_with_executor(&created, Some(&existing_email), repo.pool())
                .await,
        )?;

        let existing_started = std::time::Instant::now();
        let existing = api_ok(api.request_password_reset(&existing_email).await)?;
        let existing_elapsed = existing_started.elapsed();
        let missing_started = std::time::Instant::now();
        let missing = api_ok(
            api.request_password_reset("missing-email@example.com")
                .await,
        )?;
        let missing_elapsed = missing_started.elapsed();

        assert_eq!(existing.message, GENERIC_PASSWORD_RESET_MESSAGE);
        assert_eq!(missing.message, GENERIC_PASSWORD_RESET_MESSAGE);
        assert!(existing_elapsed >= std::time::Duration::from_millis(490));
        assert!(missing_elapsed >= std::time::Duration::from_millis(490));
        let token_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM auth_email_tokens WHERE user_id = $1 AND token_type = $2",
        )
        .bind(created.id)
        .bind(i16::from(
            synctv_core::models::EmailTokenType::PasswordReset,
        ))
        .fetch_one(&pool)
        .await?;
        let outbox_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM email_outbox WHERE kind = $1 AND status = 1",
        )
        .bind(synctv_core::repository::EmailOutboxKind::PasswordReset.as_i16())
        .fetch_one(&pool)
        .await?;
        assert_eq!(token_count, 1);
        assert_eq!(outbox_count, 1);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn confirm_email_login_accepts_multiple_outstanding_codes_for_same_user() -> TestResult {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool.clone())?;
        let repo = synctv_core::repository::UserRepository::new(pool.clone());
        let email_repo = synctv_core::repository::UserEmailRepository::new(pool.clone());

        let user = make_user("email_login_multi");
        let email = "email_login_multi@example.com".to_string();
        let created = repo.create(&user).await?;
        core_ok(
            email_repo
                .create_for_user_with_executor(&created, Some(&email), repo.pool())
                .await,
        )?;

        let first = core_ok(
            api.email_token_service
                .generate_token(&created.id, synctv_core::models::EmailTokenType::EmailLogin)
                .await,
        )?;
        let second = core_ok(
            api.email_token_service
                .generate_token(&created.id, synctv_core::models::EmailTokenType::EmailLogin)
                .await,
        )?;

        let first_login = api_ok(
            api.confirm_email_login(&created.id, &email, &first, None)
                .await,
        )?;
        assert_eq!(
            first_login.user_id,
            api_ok(api.public_user_id(created.id))?,
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

        let second_login = api_ok(
            api.confirm_email_login(&created.id, &email, &second, None)
                .await,
        )?;
        assert_eq!(
            second_login.user_id,
            api_ok(api.public_user_id(created.id))?,
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
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn confirm_email_login_rejects_email_transferred_after_session_start() -> TestResult {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool.clone())?;
        let repo = synctv_core::repository::UserRepository::new(pool.clone());
        let email_repo = synctv_core::repository::UserEmailRepository::new(pool.clone());
        let email = "email_login_transferred@example.com";
        let original_owner = repo
            .create(&make_user("email_login_original_owner"))
            .await?;
        let new_owner = repo.create(&make_user("email_login_new_owner")).await?;
        email_repo
            .create_for_user_with_executor(&original_owner, Some(email), repo.pool())
            .await?;
        let token = api
            .email_token_service
            .generate_token(
                &original_owner.id,
                synctv_core::models::EmailTokenType::EmailLogin,
            )
            .await?;

        let mut tx = pool.begin().await?;
        email_repo
            .delete_with_executor(&original_owner.id, synctv_core::SystemClock.now(), &mut *tx)
            .await?;
        email_repo
            .upsert_with_executor(
                &new_owner.id,
                email,
                synctv_core::SystemClock.now(),
                &mut *tx,
            )
            .await?;
        tx.commit().await?;

        let result = api
            .confirm_email_login(&original_owner.id, email, &token, None)
            .await;
        assert!(matches!(
            result,
            Err(crate::impls::ApiError::Authentication(_))
        ));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn mfa_email_code_flow_completes_password_first_factor_login() -> TestResult {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool)?;
        let email = format!("mfa_{}@example.com", synctv_common::snanoid!(8));
        let (user, _, _) = core_ok(
            synctv_core_testing::opaque_register_user(
                api.user_service.as_ref(),
                "mfa_email_user",
                Some(email.clone()),
                "StrongPass1",
            )
            .await,
        )?;
        core_ok(
            api.user_service
                .set_two_factor_enabled(&user.id, true)
                .await,
        )?;

        let first_factor = core_ok(
            synctv_core_testing::opaque_login_user_with_challenge(
                api.user_service.as_ref(),
                "mfa_email_user",
                "StrongPass1",
            )
            .await,
        )?;
        let AuthenticatedLogin::MfaRequired { challenge, .. } = first_factor else {
            return Err(test_error("password first factor should require MFA"));
        };
        assert!(challenge
            .available_methods
            .contains(&synctv_core::service::AuthFactorMethod::Email));

        let request = api_ok(
            api.request_mfa_email_code_with_control(&challenge.session_id, None)
                .await,
        )?;
        assert_eq!(
            request.masked_email,
            synctv_core::service::mask_email(&email)
        );

        let token = core_ok(
            api.email_token_service
                .generate_token(&user.id, synctv_core::models::EmailTokenType::EmailLogin)
                .await,
        )?;
        let completed = api_ok(
            api.verify_mfa_email_code_with_control(&challenge.session_id, &token, None, None)
                .await,
        )?;
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
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn request_email_login_returns_same_message_for_existing_and_missing_users() -> TestResult
    {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool.clone())?;
        let repo = synctv_core::repository::UserRepository::new(pool.clone());
        let email_repo = synctv_core::repository::UserEmailRepository::new(pool);
        let existing_user = make_user("email_login_existing");
        let existing_email = "email_login_existing@example.com".to_string();
        let created = repo.create(&existing_user).await?;
        core_ok(
            email_repo
                .create_for_user_with_executor(&created, Some(&existing_email), repo.pool())
                .await,
        )?;

        let existing = api_ok(
            api.request_email_login(&created.id, &existing_email, "test:existing-email-login")
                .await,
        )?;
        let missing = api_ok(
            api.request_decoy_email_login_with_control("test:missing-email-login", None)
                .await,
        )?;

        assert_eq!(existing.message, GENERIC_EMAIL_LOGIN_MESSAGE);
        assert_eq!(missing.message, GENERIC_EMAIL_LOGIN_MESSAGE);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn email_login_request_persists_before_returning_without_waiting_for_smtp() -> TestResult
    {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool.clone())?;
        let repo = synctv_core::repository::UserRepository::new(pool.clone());
        let email_repo = synctv_core::repository::UserEmailRepository::new(pool.clone());
        let email = "email_login_delivery_failure@example.com";
        let user = repo
            .create(&make_user("email_login_delivery_failure"))
            .await?;
        email_repo
            .create_for_user_with_executor(&user, Some(email), repo.pool())
            .await?;

        let real = api_ok(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                api.request_email_login(&user.id, email, "test:delivery-failure"),
            )
            .await
            .map_err(|_| test_error("public email login response waited for SMTP delivery"))?,
        )?;
        let decoy = api_ok(
            api.request_decoy_email_login_with_control("test:delivery-failure-decoy", None)
                .await,
        )?;
        assert_eq!(real.message, GENERIC_EMAIL_LOGIN_MESSAGE);
        assert_eq!(decoy.message, GENERIC_EMAIL_LOGIN_MESSAGE);
        let outbox_jobs = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM email_outbox WHERE recipient = $1 AND status = 1",
        )
        .bind(email)
        .fetch_one(&pool)
        .await?;
        assert_eq!(outbox_jobs, 1);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn real_and_decoy_email_login_requests_share_the_same_limit_shape() -> TestResult {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let api = build_test_email_api(pool.clone())?;
        let repo = synctv_core::repository::UserRepository::new(pool.clone());
        let email_repo = synctv_core::repository::UserEmailRepository::new(pool);
        let user = repo.create(&make_user("email_login_rate_limit")).await?;
        let email = "email_login_rate_limit@example.com";
        email_repo
            .create_for_user_with_executor(&user, Some(email), repo.pool())
            .await?;

        for _ in 0..3 {
            api_ok(
                api.request_email_login(&user.id, email, "test:real-email-login-limit")
                    .await,
            )?;
            api_ok(
                api.request_decoy_email_login_with_control("test:decoy-email-login-limit", None)
                    .await,
            )?;
        }

        let real = api
            .request_email_login(&user.id, email, "test:real-email-login-limit")
            .await;
        let decoy = api
            .request_decoy_email_login_with_control("test:decoy-email-login-limit", None)
            .await;
        assert!(matches!(
            real,
            Err(crate::impls::ApiError::RateLimitedWithRetry { .. })
        ));
        assert!(matches!(
            decoy,
            Err(crate::impls::ApiError::RateLimitedWithRetry { .. })
        ));
        Ok(())
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
