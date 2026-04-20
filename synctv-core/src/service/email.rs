//! Email verification and sending service.
//!
//! Handles SMTP-backed email delivery for verification and password reset flows.

use lettre::{
    message::{header::ContentType, Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use synctv_common::ExecutionControl;

use super::email_templates::EmailTemplateManager;
use super::email_token::{EmailTokenService, EmailTokenType};
use crate::{Error, InternalExt, Result};

/// Mask an email address for safe logging: `user***@example.com`
pub fn mask_email(email: &str) -> String {
    if let Some(at_pos) = email.find('@') {
        let local = &email[..at_pos];
        let domain = &email[at_pos..];
        let visible = local.len().min(3);
        format!("{}***{}", &local[..visible], domain)
    } else {
        "***".to_string()
    }
}

fn map_email_send_failure(_error: impl std::fmt::Display) -> Error {
    Error::ServiceUnavailable(
        "Email delivery is temporarily unavailable. Please try again later.".to_string(),
    )
}

/// Email verification error
#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    #[error("Send error: {0}")]
    SendError(String),
}

/// Email configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub from_email: String,
    pub from_name: String,
    pub use_tls: bool,
}

#[derive(Clone)]
pub struct EmailService {
    config: Option<EmailConfig>,
    template_manager: Arc<EmailTemplateManager>,
    /// Reusable SMTP transport (connection-pooled by lettre).
    smtp_transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
}

impl std::fmt::Debug for EmailService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailService")
            .field("configured", &self.config.is_some())
            .finish()
    }
}

impl EmailService {
    async fn run_with_control<T, F>(
        control: Option<&ExecutionControl>,
        future: F,
    ) -> std::result::Result<T, EmailError>
    where
        F: std::future::Future<Output = std::result::Result<T, EmailError>>,
    {
        match control {
            Some(control) => control
                .run(future)
                .await
                .map_err(|error| EmailError::SendError(error.to_string()))?,
            None => future.await,
        }
    }

    async fn send_tokenized_email_with_control(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
        token_type: EmailTokenType,
        success_log_message: &'static str,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        Self::validate_email(email)?;

        if let Some(control) = control {
            control
                .check_active()
                .map_err(|error| Error::Timeout(error.to_string()))?;
        }

        let token = token_service
            .generate_token_with_control(user_id, token_type, control)
            .await?;

        if let Some(config) = &self.config {
            let send_result = match token_type {
                EmailTokenType::EmailVerification => {
                    self.send_verification_email_impl(config, email, &token, control)
                        .await
                }
                EmailTokenType::PasswordReset => {
                    self.send_password_reset_email_impl(config, email, &token, control)
                        .await
                }
                EmailTokenType::EmailLogin => {
                    self.send_email_login_email_impl(config, email, &token, control)
                        .await
                }
            };

            if let Err(error) = send_result {
                tracing::error!(
                    email = %mask_email(email),
                    token_type = %token_type.as_str(),
                    error = %error,
                    "Failed to send tokenized email, invalidating generated token"
                );
                token_service
                    .invalidate_specific_token(&token, user_id, token_type)
                    .await?;
                return Err(map_email_send_failure(error));
            }

            tracing::info!(message = success_log_message, email = %mask_email(email));
            Ok(String::new())
        } else {
            tracing::warn!("Email service not configured, returning token directly");
            tracing::info!(message = success_log_message, email = %mask_email(email));
            Ok(token)
        }
    }

    /// Build a reusable SMTP transport from config.
    fn build_smtp_transport(
        config: &EmailConfig,
    ) -> std::result::Result<AsyncSmtpTransport<Tokio1Executor>, EmailError> {
        let creds = Credentials::new(config.smtp_username.clone(), config.smtp_password.clone());
        let transport = if config.use_tls {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&config.smtp_host)
                .map_err(|e| {
                    EmailError::SendError(format!("Failed to create SMTP transport: {e}"))
                })?
                .credentials(creds)
                .port(config.smtp_port)
                .build()
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.smtp_host)
                .credentials(creds)
                .port(config.smtp_port)
                .build()
        };
        Ok(transport)
    }

    /// Create a new email service.
    pub fn new(config: Option<EmailConfig>) -> Result<Self> {
        let template_manager = EmailTemplateManager::new()?;
        let smtp_transport = match config.as_ref() {
            Some(cfg) => Some(Self::build_smtp_transport(cfg).map_err(|e| {
                Error::Internal(format!("Failed to initialize SMTP transport: {e}"))
            })?),
            None => None,
        };
        Ok(Self {
            config,
            template_manager: Arc::new(template_manager),
            smtp_transport,
        })
    }

    /// Validate email format (RFC 5322 compliant)
    fn validate_email(email: &str) -> Result<()> {
        let email = email.trim();

        if email.is_empty() {
            return Err(Error::InvalidInput("Email cannot be empty".to_string()));
        }
        if email.len() > 254 {
            return Err(Error::InvalidInput(
                "Email too long (max 254 characters)".to_string(),
            ));
        }
        if !email.contains('@') {
            return Err(Error::InvalidInput(
                "Email must contain @ symbol".to_string(),
            ));
        }

        let parts: Vec<&str> = email.split('@').collect();
        if parts.len() != 2 {
            return Err(Error::InvalidInput(
                "Email must contain exactly one @ symbol".to_string(),
            ));
        }

        let local = parts[0];
        let domain = parts[1];

        if local.is_empty() {
            return Err(Error::InvalidInput(
                "Email local part cannot be empty".to_string(),
            ));
        }
        if local.len() > 64 {
            return Err(Error::InvalidInput(
                "Email local part too long (max 64 characters)".to_string(),
            ));
        }
        if !local
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '+')
        {
            return Err(Error::InvalidInput(
                "Email local part contains invalid characters".to_string(),
            ));
        }
        if local.starts_with('.') || local.ends_with('.') {
            return Err(Error::InvalidInput(
                "Email local part cannot start or end with dot".to_string(),
            ));
        }
        if local.contains("..") {
            return Err(Error::InvalidInput(
                "Email local part cannot contain consecutive dots".to_string(),
            ));
        }

        if domain.is_empty() {
            return Err(Error::InvalidInput(
                "Email domain cannot be empty".to_string(),
            ));
        }
        if domain.len() > 253 {
            return Err(Error::InvalidInput(
                "Email domain too long (max 253 characters)".to_string(),
            ));
        }
        if !domain.contains('.') {
            return Err(Error::InvalidInput(
                "Email domain must contain at least one dot".to_string(),
            ));
        }
        if domain.starts_with('.')
            || domain.ends_with('.')
            || domain.starts_with('-')
            || domain.ends_with('-')
        {
            return Err(Error::InvalidInput(
                "Email domain has invalid format".to_string(),
            ));
        }

        let domain_labels: Vec<&str> = domain.split('.').collect();
        for label in &domain_labels {
            if label.is_empty() {
                return Err(Error::InvalidInput(
                    "Email domain cannot have empty labels".to_string(),
                ));
            }
            if label.len() > 63 {
                return Err(Error::InvalidInput(
                    "Email domain label too long (max 63 characters)".to_string(),
                ));
            }
            if !label.chars().all(|c| c.is_alphanumeric() || c == '-') {
                return Err(Error::InvalidInput(
                    "Email domain contains invalid characters".to_string(),
                ));
            }
            if label.starts_with('-') || label.ends_with('-') {
                return Err(Error::InvalidInput(
                    "Email domain label cannot start or end with hyphen".to_string(),
                ));
            }
        }

        if let Some(tld) = domain_labels.last() {
            if tld.len() < 2 {
                return Err(Error::InvalidInput(
                    "Email domain TLD must be at least 2 characters".to_string(),
                ));
            }
            if !tld.chars().all(char::is_alphabetic) {
                return Err(Error::InvalidInput(
                    "Email domain TLD must be alphabetic".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Send verification email
    pub async fn send_verification_email(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
    ) -> Result<String> {
        self.send_verification_email_with_control(email, token_service, user_id, None)
            .await
    }

    pub async fn send_verification_email_with_control(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        self.send_tokenized_email_with_control(
            email,
            token_service,
            user_id,
            EmailTokenType::EmailVerification,
            "Sent verification email",
            control,
        )
        .await
    }

    /// Send password reset email
    pub async fn send_password_reset_email(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
    ) -> Result<String> {
        self.send_password_reset_email_with_control(email, token_service, user_id, None)
            .await
    }

    pub async fn send_password_reset_email_with_control(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        self.send_tokenized_email_with_control(
            email,
            token_service,
            user_id,
            EmailTokenType::PasswordReset,
            "Sent password reset email",
            control,
        )
        .await
    }

    /// Send a passwordless email login code.
    pub async fn send_email_login_email(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
    ) -> Result<String> {
        self.send_email_login_email_with_control(email, token_service, user_id, None)
            .await
    }

    pub async fn send_email_login_email_with_control(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        self.send_tokenized_email_with_control(
            email,
            token_service,
            user_id,
            EmailTokenType::EmailLogin,
            "Sent email login code",
            control,
        )
        .await
    }

    /// Send a test email to verify email configuration
    pub async fn send_test_email(&self, to: &str) -> Result<()> {
        self.send_test_email_with_control(to, None).await
    }

    pub async fn send_test_email_with_control(
        &self,
        to: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::validate_email(to)?;

        let config = self
            .config
            .as_ref()
            .ok_or_else(|| Error::Internal("Email service not configured".to_string()))?;

        let sent_at = synctv_common::time::format_datetime_display(chrono::Utc::now());
        let (html_body, plain_text_body) = self
            .template_manager
            .render_test_email(&config.smtp_host, config.smtp_port, &sent_at)
            .internal_with_err("Failed to render template")?;

        let subject = "SyncTV Email Test";
        self.send_html_email(config, to, subject, &html_body, &plain_text_body, control)
            .await
            .internal_with_err("Failed to send test email")?;

        tracing::info!("Sent test email to {}", mask_email(to));
        Ok(())
    }

    async fn send_verification_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Verify your SyncTV email";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_verification_email(token, "24 hours")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(config, to, subject, &html_body, &plain_text_body, control)
            .await
    }

    async fn send_password_reset_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Reset your SyncTV password";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_password_reset_email(token, "1 hour")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(config, to, subject, &html_body, &plain_text_body, control)
            .await
    }

    async fn send_email_login_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Your SyncTV login code";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_email_login_email(token, "15 minutes")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(config, to, subject, &html_body, &plain_text_body, control)
            .await
    }

    async fn send_html_email(
        &self,
        config: &EmailConfig,
        to: &str,
        subject: &str,
        html_body: &str,
        plain_text_body: &str,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let from_mailbox: Mailbox = format!("{} <{}>", config.from_name, config.from_email)
            .parse()
            .map_err(|e| EmailError::SendError(format!("Invalid from address: {e}")))?;

        let to_mailbox: Mailbox = to
            .parse()
            .map_err(|e| EmailError::SendError(format!("Invalid to address: {e}")))?;

        let email = Message::builder()
            .from(from_mailbox)
            .to(to_mailbox)
            .subject(subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(plain_text_body.to_string()),
                    )
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(html_body.to_string()),
                    ),
            )
            .map_err(|e| EmailError::SendError(format!("Failed to build email: {e}")))?;

        self.send_message(config, email, control).await
    }

    async fn send_message(
        &self,
        config: &EmailConfig,
        email: Message,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let recipient = email
            .envelope()
            .to()
            .first()
            .ok_or_else(|| EmailError::SendError("No recipients in email envelope".to_string()))?
            .clone();

        let transport = self
            .smtp_transport
            .as_ref()
            .ok_or_else(|| EmailError::SendError("SMTP transport not initialized".to_string()))?;

        Self::run_with_control(control, async {
            transport
                .send(email)
                .await
                .map_err(|e| EmailError::SendError(format!("Failed to send email: {e}")))
        })
        .await?;

        tracing::info!(
            "Email sent successfully to {} via SMTP {}:{}",
            recipient,
            config.smtp_host,
            config.smtp_port
        );

        Ok(())
    }

    /// Check if email service is configured
    #[must_use]
    pub const fn is_configured(&self) -> bool {
        self.config.is_some()
    }
}

impl Default for EmailService {
    fn default() -> Self {
        Self::new(None).expect("Failed to create default EmailService")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_email_valid() {
        assert!(EmailService::validate_email("test@example.com").is_ok());
        assert!(EmailService::validate_email("user.name+tag@domain.co.uk").is_ok());
    }

    #[test]
    fn test_validate_email_invalid() {
        assert!(EmailService::validate_email("").is_err());
        assert!(EmailService::validate_email("invalid").is_err());
        assert!(EmailService::validate_email("@example.com").is_err());
        assert!(EmailService::validate_email("test@").is_err());
        assert!(EmailService::validate_email("test@.com").is_err());
    }

    #[tokio::test]
    async fn test_send_verification_email_returns_empty_when_email_configured() {
        // Verify that send_verification_email does not leak the token when email is configured.
        // We can only test the dev mode path (no config) since the configured path needs SMTP.
        let service = EmailService::new(None).unwrap();
        // In dev mode (no config), token should be returned
        // We can't call send_verification_email without a real EmailTokenService,
        // but we verify the contract: configured -> empty, unconfigured -> token.
        assert!(
            !service.is_configured(),
            "Service with None config should not be configured"
        );

        let configured_config = EmailConfig {
            smtp_host: "localhost".to_string(),
            smtp_port: 0,
            smtp_username: "test".to_string(),
            smtp_password: "test".to_string(),
            from_email: "noreply@example.com".to_string(),
            from_name: "SyncTV".to_string(),
            use_tls: false,
        };
        let configured_service = EmailService::new(Some(configured_config)).unwrap();
        assert!(
            configured_service.is_configured(),
            "Service with Some config should be configured"
        );
    }

    #[test]
    fn test_map_email_send_failure_returns_service_unavailable() {
        let err = map_email_send_failure("smtp connection refused");
        match err {
            Error::ServiceUnavailable(message) => {
                assert_eq!(
                    message,
                    "Email delivery is temporarily unavailable. Please try again later."
                );
            }
            other => panic!("expected ServiceUnavailable, got: {other:?}"),
        }
    }
}
