//! Shared Email Verification and Password Reset Implementation
//!
//! Used by both HTTP and gRPC handlers to avoid duplicating email logic.

use std::sync::Arc;
use synctv_core::service::{EmailService, EmailTokenService, EmailTokenType, UserService};

use crate::impls::ApiError;

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
        let generic_message =
            "If an account exists with this email, a verification code will be sent.".to_string();

        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {e}")))?;

        let user = if let Some(u) = user {
            u
        } else {
            // Add random delay to prevent timing side-channel that leaks
            // whether an account exists based on response time differences.
            let delay_ms = rand::random_range(100u64..500u64);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            return Ok(SendVerificationResult {
                message: generic_message,
            });
        };

        let _token = self
            .email_service
            .send_verification_email(email, &self.email_token_service, &user.id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to send email: {e}")))?;

        tracing::info!("Sent verification email to {email}");

        Ok(SendVerificationResult {
            message: generic_message,
        })
    }

    /// Confirm an email verification token.
    pub async fn confirm_email(
        &self,
        email: &str,
        token: &str,
    ) -> Result<ConfirmEmailResult, ApiError> {
        let validated_user_id = self
            .email_token_service
            .validate_token(token, EmailTokenType::EmailVerification)
            .await
            .map_err(|_| {
                ApiError::InvalidInput("Invalid or expired verification token".to_string())
            })?;

        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {e}")))?
            .ok_or_else(|| {
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
            .map_err(|e| ApiError::Internal(format!("Failed to update email verification: {e}")))?;

        // Invalidate all remaining email verification tokens for this user
        self.email_token_service
            .invalidate_user_tokens(&user.id, EmailTokenType::EmailVerification)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to invalidate tokens: {e}")))?;

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
            .map_err(|e| ApiError::Internal(format!("Database error: {e}")))?;

        let Some(user) = user else {
            // Add random delay to prevent timing side-channel that leaks
            // whether an account exists based on response time differences.
            let delay_ms = rand::random_range(100u64..500u64);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            return Ok(RequestPasswordResetResult {
                message:
                    "If an account exists with this email, a password reset code will be sent."
                        .to_string(),
            });
        };

        let _token = self
            .email_service
            .send_password_reset_email(email, &self.email_token_service, &user.id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to send email: {e}")))?;

        tracing::info!("Password reset requested for user {}", user.id.as_str());

        Ok(RequestPasswordResetResult {
            message: "Password reset code sent to your email".to_string(),
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

        let validated_user_id = self
            .email_token_service
            .validate_token(token, EmailTokenType::PasswordReset)
            .await
            .map_err(|_| ApiError::InvalidInput("Invalid or expired reset token".to_string()))?;

        let user = self
            .user_service
            .get_by_email(email)
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {e}")))?
            .ok_or_else(|| ApiError::InvalidInput("Invalid or expired reset token".to_string()))?;

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
            .map_err(|e| ApiError::Internal(format!("Failed to update password: {e}")))?;

        // Invalidate all remaining password reset tokens for this user
        self.email_token_service
            .invalidate_user_tokens(&user.id, EmailTokenType::PasswordReset)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to invalidate tokens: {e}")))?;

        tracing::info!("Password reset completed for user {}", user.id.as_str());

        Ok(ConfirmPasswordResetResult {
            message: "Password reset successfully".to_string(),
            user_id: user.id.to_string(),
        })
    }
}
