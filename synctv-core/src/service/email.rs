//! Email sending service.
//!
//! Handles SMTP-backed email delivery for email bind, login, and password reset flows.

use lettre::{
    message::{header::ContentType, Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use synctv_common::ExecutionControl;
use tokio::sync::broadcast;

use super::email_templates::EmailTemplateManager;
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

/// Email delivery error
#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    #[error("Send error: {0}")]
    SendError(String),
}

/// Email configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub from_email: String,
    pub from_name: String,
    pub use_tls: bool,
}

impl EmailConfig {
    pub fn validate(&self) -> Result<()> {
        if self.smtp_host.is_empty() {
            return Err(Error::InvalidInput(
                "email.smtp_host is required when email.enabled is true".to_string(),
            ));
        }
        if self.smtp_port == 0 {
            return Err(Error::InvalidInput(
                "email.smtp_port must be between 1 and 65535".to_string(),
            ));
        }
        if self.from_email.is_empty() {
            return Err(Error::InvalidInput(
                "email.from_email is required when email.enabled is true".to_string(),
            ));
        }
        EmailService::validate_email(&self.from_email)?;
        Ok(())
    }
}

pub trait EmailConfigProvider: Send + Sync {
    fn current_config(&self) -> Result<Option<EmailConfig>>;

    fn subscribe_changes(&self) -> Option<broadcast::Receiver<()>> {
        None
    }
}

#[derive(Clone)]
pub struct EmailService {
    config_provider: Arc<dyn EmailConfigProvider>,
    template_manager: Arc<EmailTemplateManager>,
    /// Reusable SMTP transport for the current runtime config snapshot.
    smtp_transport: Arc<RwLock<Option<CachedSmtpTransport>>>,
}

struct CachedSmtpTransport {
    config: EmailConfig,
    transport: std::result::Result<AsyncSmtpTransport<Tokio1Executor>, String>,
}

impl std::fmt::Debug for EmailService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailService")
            .field("configured", &self.is_configured())
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

    /// Create a new email service backed by a runtime config provider.
    pub fn new(config_provider: Arc<dyn EmailConfigProvider>) -> Result<Self> {
        let template_manager = EmailTemplateManager::new()?;
        let config_changes = config_provider.subscribe_changes();
        let service = Self {
            config_provider,
            template_manager: Arc::new(template_manager),
            smtp_transport: Arc::new(RwLock::new(None)),
        };
        service.current_config()?;
        service.start_config_change_listener(config_changes);
        Ok(service)
    }

    fn current_config(&self) -> Result<Option<EmailConfig>> {
        let config = self.config_provider.current_config()?;
        if let Some(config) = &config {
            config.validate()?;
        }
        Ok(config)
    }

    fn start_config_change_listener(&self, receiver: Option<broadcast::Receiver<()>>) {
        let Some(mut receiver) = receiver else {
            return;
        };

        let smtp_transport = Arc::clone(&self.smtp_transport);
        crate::spawn::spawn_monitored("email_config_change_listener", async move {
            loop {
                match receiver.recv().await {
                    Ok(()) => {
                        *smtp_transport.write() = None;
                        tracing::debug!(
                            "SMTP transport cache invalidated by email settings change"
                        );
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        *smtp_transport.write() = None;
                        tracing::warn!(
                            count,
                            "Email settings change listener lagged; SMTP transport cache invalidated"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
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

    pub async fn send_email_bind_token_email_with_control(
        &self,
        email: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::validate_email(email)?;

        if let Some(control) = control {
            control
                .check_active()
                .map_err(|error| Error::Timeout(error.to_string()))?;
        }

        let config = self.current_config()?.ok_or_else(|| {
            Error::ServiceUnavailable(
                "Email delivery is not configured on this server.".to_string(),
            )
        })?;

        self.send_email_bind_email_impl(&config, email, token, control)
            .await
            .map_err(map_email_send_failure)?;

        tracing::info!("Sent email bind email to {}", mask_email(email));
        Ok(())
    }

    pub async fn send_password_reset_token_email_with_control(
        &self,
        email: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::validate_email(email)?;

        if let Some(control) = control {
            control
                .check_active()
                .map_err(|error| Error::Timeout(error.to_string()))?;
        }

        let config = self.current_config()?.ok_or_else(|| {
            Error::ServiceUnavailable(
                "Email delivery is not configured on this server.".to_string(),
            )
        })?;

        self.send_password_reset_email_impl(&config, email, token, control)
            .await
            .map_err(map_email_send_failure)?;

        tracing::info!("Sent password reset email to {}", mask_email(email));
        Ok(())
    }

    pub async fn send_email_login_token_email_with_control(
        &self,
        email: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::validate_email(email)?;

        if let Some(control) = control {
            control
                .check_active()
                .map_err(|error| Error::Timeout(error.to_string()))?;
        }

        let config = self.current_config()?.ok_or_else(|| {
            Error::ServiceUnavailable(
                "Email delivery is not configured on this server.".to_string(),
            )
        })?;

        self.send_email_login_email_impl(&config, email, token, control)
            .await
            .map_err(map_email_send_failure)?;

        tracing::info!("Sent email login code to {}", mask_email(email));
        Ok(())
    }

    pub async fn send_email_registration_token_email_with_control(
        &self,
        email: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::validate_email(email)?;

        if let Some(control) = control {
            control
                .check_active()
                .map_err(|error| Error::Timeout(error.to_string()))?;
        }

        let config = self.current_config()?.ok_or_else(|| {
            Error::ServiceUnavailable(
                "Email delivery is not configured on this server.".to_string(),
            )
        })?;

        self.send_email_registration_email_impl(&config, email, token, control)
            .await
            .map_err(map_email_send_failure)?;

        tracing::info!("Sent email registration code to {}", mask_email(email));
        Ok(())
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

        let config = self.current_config()?.ok_or_else(|| {
            Error::ServiceUnavailable(
                "Email delivery is not configured on this server.".to_string(),
            )
        })?;

        let sent_at = synctv_common::time::format_datetime_display(chrono::Utc::now());
        let (html_body, plain_text_body) = self
            .template_manager
            .render_test_email(&config.smtp_host, config.smtp_port, &sent_at)
            .internal_with_err("Failed to render template")?;

        let subject = "SyncTV Email Test";
        self.send_html_email(&config, to, subject, &html_body, &plain_text_body, control)
            .await
            .internal_with_err("Failed to send test email")?;

        tracing::info!("Sent test email to {}", mask_email(to));
        Ok(())
    }

    async fn send_email_bind_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Confirm your SyncTV email";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_email_bind_email(token, "24 hours")
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

    async fn send_email_registration_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Your SyncTV registration code";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_email_registration_email(token, "15 minutes")
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

        let transport = self.smtp_transport_for_config(config)?;

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

    fn smtp_transport_for_config(
        &self,
        config: &EmailConfig,
    ) -> std::result::Result<AsyncSmtpTransport<Tokio1Executor>, EmailError> {
        let mut cached = self.smtp_transport.write();
        let should_rebuild = cached
            .as_ref()
            .is_none_or(|cached| cached.config != *config);

        if should_rebuild {
            let transport = Self::build_smtp_transport(config).map_err(|error| error.to_string());
            *cached = Some(CachedSmtpTransport {
                config: config.clone(),
                transport,
            });
        }

        if let Some(cached) = cached.as_ref() {
            return cached.transport.clone().map_err(EmailError::SendError);
        }

        Err(EmailError::SendError(
            "SMTP transport cache was unavailable after rebuild".to_string(),
        ))
    }

    /// Check if email service is configured
    #[must_use]
    pub fn is_configured(&self) -> bool {
        matches!(self.current_config(), Ok(Some(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => std::panic::panic_any(context.to_string()),
        }
    }

    #[derive(Clone)]
    struct TestEmailConfigProvider(Option<EmailConfig>);

    impl EmailConfigProvider for TestEmailConfigProvider {
        fn current_config(&self) -> Result<Option<EmailConfig>> {
            Ok(self.0.clone())
        }
    }

    fn test_service(config: Option<EmailConfig>) -> EmailService {
        ok(
            EmailService::new(Arc::new(TestEmailConfigProvider(config))),
            "email service should build",
        )
    }

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

    #[test]
    fn test_configured_state_uses_config_provider() {
        let service = test_service(None);
        assert!(
            !service.is_configured(),
            "Service without runtime config should not be configured"
        );

        let configured_config = EmailConfig {
            smtp_host: "localhost".to_string(),
            smtp_port: 25,
            smtp_username: "test".to_string(),
            smtp_password: "test".to_string(),
            from_email: "noreply@example.com".to_string(),
            from_name: "SyncTV".to_string(),
            use_tls: false,
        };
        let configured_service = test_service(Some(configured_config));
        assert!(
            configured_service.is_configured(),
            "Service with runtime config should be configured"
        );
    }

    #[test]
    fn test_configured_service_initializes_without_building_transport() {
        let configured_service = test_service(Some(EmailConfig {
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_username: "user".to_string(),
            smtp_password: "password".to_string(),
            from_email: "noreply@example.com".to_string(),
            from_name: "SyncTV".to_string(),
            use_tls: true,
        }));

        assert!(configured_service.is_configured());
        assert!(
            configured_service.smtp_transport.read().is_none(),
            "SMTP transport should be created lazily on first send"
        );
    }

    #[tokio::test]
    async fn test_smtp_transport_cache_rebuilds_when_config_changes() {
        let service = test_service(Some(EmailConfig {
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_username: "user".to_string(),
            smtp_password: "password".to_string(),
            from_email: "noreply@example.com".to_string(),
            from_name: "SyncTV".to_string(),
            use_tls: false,
        }));

        let first = EmailConfig {
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_username: "user".to_string(),
            smtp_password: "password".to_string(),
            from_email: "noreply@example.com".to_string(),
            from_name: "SyncTV".to_string(),
            use_tls: false,
        };
        ok(
            service.smtp_transport_for_config(&first),
            "first SMTP transport should build",
        );
        assert_eq!(
            some(
                service
                    .smtp_transport
                    .read()
                    .as_ref()
                    .map(|cached| cached.config.clone()),
                "transport should be cached",
            ),
            first
        );

        let second = EmailConfig {
            smtp_host: "smtp2.example.com".to_string(),
            smtp_port: 465,
            smtp_username: "next-user".to_string(),
            smtp_password: "next-password".to_string(),
            from_email: "mailer@example.com".to_string(),
            from_name: "SyncTV Mail".to_string(),
            use_tls: false,
        };
        ok(
            service.smtp_transport_for_config(&second),
            "second SMTP transport should build",
        );
        assert_eq!(
            some(
                service
                    .smtp_transport
                    .read()
                    .as_ref()
                    .map(|cached| cached.config.clone()),
                "transport should stay cached",
            ),
            second
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
            other => std::panic::panic_any(format!("expected ServiceUnavailable, got: {other:?}")),
        }
    }
}
