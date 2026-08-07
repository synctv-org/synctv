//! Email sending service.
//!
//! Handles SMTP-backed email delivery for email bind, login, and password reset flows.

use lettre::{
    message::{header::ContentType, Mailbox, MultiPart},
    transport::smtp::{
        authentication::{Credentials, DEFAULT_MECHANISMS},
        client::{AsyncSmtpConnection, AsyncTokioStream},
        extension::ClientId,
    },
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use parking_lot::RwLock;
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
use serde::{Deserialize, Serialize};
use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use synctv_common::ExecutionControl;
use tokio::sync::broadcast;
use tokio_rustls::TlsConnector;
use tokio_socks::tcp::Socks5Stream;
use url::Url;

use super::email_templates::EmailTemplateManager;
use crate::{repository::EmailOutboxKind, Error, InternalExt, Result};

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

struct RenderedEmail<'a> {
    subject: &'a str,
    html_body: &'a str,
    plain_text_body: &'a str,
    message_id: Option<&'a str>,
}

/// Email configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_credentials: Option<SmtpCredentials>,
    pub smtp_proxy: Option<SmtpProxyConfig>,
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
        if let Some(proxy) = &self.smtp_proxy {
            proxy.validate()?;
        }
        if let Some(credentials) = &self.smtp_credentials {
            credentials.validate()?;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmtpCredentials {
    pub username: String,
    pub password: String,
}

impl SmtpCredentials {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.username.trim().is_empty() || self.password.is_empty() {
            return Err(Error::InvalidInput(
                "email SMTP credentials require both username and password".to_string(),
            ));
        }
        Ok(())
    }

    fn lettre_credentials(&self) -> Credentials {
        Credentials::new(self.username.clone(), self.password.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmtpProxyConfig {
    pub url: String,
    pub credentials: Option<SmtpCredentials>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SmtpProxyEndpoint {
    host: String,
    port: u16,
}

impl SmtpProxyConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        self.endpoint()?;
        if let Some(credentials) = &self.credentials {
            credentials.validate()?;
            if credentials.username.len() > u8::MAX as usize
                || credentials.password.len() > u8::MAX as usize
            {
                return Err(Error::InvalidInput(
                    "email SMTP proxy credentials must be at most 255 bytes".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn endpoint(&self) -> Result<SmtpProxyEndpoint> {
        let url = Url::parse(self.url.trim()).map_err(|error| {
            Error::InvalidInput(format!("email.smtp_proxy must be a valid URL: {error}"))
        })?;
        if url.scheme() != "socks5" {
            return Err(Error::InvalidInput(
                "email.smtp_proxy currently supports only the socks5 scheme".to_string(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::InvalidInput(
                "email.smtp_proxy URL must not contain credentials".to_string(),
            ));
        }
        if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
            return Err(Error::InvalidInput(
                "email.smtp_proxy URL must contain only a SOCKS5 host and port".to_string(),
            ));
        }
        let host = url.host_str().ok_or_else(|| {
            Error::InvalidInput("email.smtp_proxy URL must include a host".to_string())
        })?;
        Ok(SmtpProxyEndpoint {
            host: host.to_string(),
            port: url.port().unwrap_or(1080),
        })
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
    transport: std::result::Result<SmtpTransport, String>,
}

#[derive(Clone)]
struct SmtpTransport {
    direct: AsyncSmtpTransport<Tokio1Executor>,
    proxy: Option<Socks5SmtpTransport>,
}

#[derive(Clone)]
struct Socks5SmtpTransport {
    smtp_host: String,
    smtp_port: u16,
    smtp_credentials: Option<SmtpCredentials>,
    use_tls: bool,
    proxy: SmtpProxyConfig,
    proxy_endpoint: SmtpProxyEndpoint,
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
    ) -> std::result::Result<SmtpTransport, EmailError> {
        let proxy = config
            .smtp_proxy
            .as_ref()
            .map(|proxy| {
                Ok(Socks5SmtpTransport {
                    smtp_host: config.smtp_host.clone(),
                    smtp_port: config.smtp_port,
                    smtp_credentials: config.smtp_credentials.clone(),
                    use_tls: config.use_tls,
                    proxy: proxy.clone(),
                    proxy_endpoint: proxy
                        .endpoint()
                        .map_err(|error| EmailError::SendError(error.to_string()))?,
                })
            })
            .transpose()?;

        let builder = if config.use_tls {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&config.smtp_host)
                .map_err(|e| {
                    EmailError::SendError(format!("Failed to create SMTP transport: {e}"))
                })?
                .port(config.smtp_port)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.smtp_host)
                .port(config.smtp_port)
        };
        let builder = match &config.smtp_credentials {
            Some(credentials) => builder.credentials(credentials.lettre_credentials()),
            None => builder,
        };
        let direct = builder.build();
        Ok(SmtpTransport { direct, proxy })
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
    pub(crate) fn validate_email(email: &str) -> Result<()> {
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

        self.send_email_bind_email_impl(&config, email, token, None, control)
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

        self.send_password_reset_email_impl(&config, email, token, None, control)
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

        self.send_email_login_email_impl(&config, email, token, None, control)
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

        self.send_email_registration_email_impl(&config, email, token, None, control)
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

        let sent_at = synctv_common::time::format_datetime_display(crate::SystemClock.now());
        let (html_body, plain_text_body) = self
            .template_manager
            .render_test_email(&config.smtp_host, config.smtp_port, &sent_at)
            .internal_with_err("Failed to render template")?;

        let subject = "SyncTV Email Test";
        self.send_html_email(
            &config,
            to,
            RenderedEmail {
                subject,
                html_body: &html_body,
                plain_text_body: &plain_text_body,
                message_id: None,
            },
            control,
        )
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
        message_id: Option<&str>,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Confirm your SyncTV email";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_email_bind_email(token, "24 hours")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(
            config,
            to,
            RenderedEmail {
                subject,
                html_body: &html_body,
                plain_text_body: &plain_text_body,
                message_id,
            },
            control,
        )
        .await
    }

    async fn send_password_reset_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
        message_id: Option<&str>,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Reset your SyncTV password";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_password_reset_email(token, "1 hour")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(
            config,
            to,
            RenderedEmail {
                subject,
                html_body: &html_body,
                plain_text_body: &plain_text_body,
                message_id,
            },
            control,
        )
        .await
    }

    async fn send_email_login_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
        message_id: Option<&str>,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Your SyncTV login code";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_email_login_email(token, "15 minutes")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(
            config,
            to,
            RenderedEmail {
                subject,
                html_body: &html_body,
                plain_text_body: &plain_text_body,
                message_id,
            },
            control,
        )
        .await
    }

    async fn send_email_registration_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
        message_id: Option<&str>,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Your SyncTV registration code";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_email_registration_email(token, "15 minutes")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(
            config,
            to,
            RenderedEmail {
                subject,
                html_body: &html_body,
                plain_text_body: &plain_text_body,
                message_id,
            },
            control,
        )
        .await
    }

    pub async fn send_outbox_email_with_control(
        &self,
        kind: EmailOutboxKind,
        email: &str,
        token: &str,
        message_id: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::validate_email(email)?;
        let config = self.current_config()?.ok_or_else(|| {
            Error::ServiceUnavailable(
                "Email delivery is not configured on this server.".to_string(),
            )
        })?;
        let result = match kind {
            EmailOutboxKind::EmailBind => {
                self.send_email_bind_email_impl(&config, email, token, Some(message_id), control)
                    .await
            }
            EmailOutboxKind::PasswordReset => {
                self.send_password_reset_email_impl(
                    &config,
                    email,
                    token,
                    Some(message_id),
                    control,
                )
                .await
            }
            EmailOutboxKind::EmailLogin => {
                self.send_email_login_email_impl(&config, email, token, Some(message_id), control)
                    .await
            }
            EmailOutboxKind::EmailRegistration => {
                self.send_email_registration_email_impl(
                    &config,
                    email,
                    token,
                    Some(message_id),
                    control,
                )
                .await
            }
        };
        result.map_err(map_email_send_failure)?;
        tracing::info!(
            kind = kind.as_str(),
            recipient = %mask_email(email),
            "Sent outbox email"
        );
        Ok(())
    }

    async fn send_html_email(
        &self,
        config: &EmailConfig,
        to: &str,
        content: RenderedEmail<'_>,
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
            .message_id(content.message_id.map(str::to_string))
            .subject(content.subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(content.plain_text_body.to_string()),
                    )
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(content.html_body.to_string()),
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

        let transport = self.smtp_transport_for_settings(config)?;

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

    fn smtp_transport_for_settings(
        &self,
        config: &EmailConfig,
    ) -> std::result::Result<SmtpTransport, EmailError> {
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

impl SmtpTransport {
    async fn send(&self, email: Message) -> std::result::Result<(), EmailError> {
        if let Some(proxy) = &self.proxy {
            return proxy.send(email).await;
        }
        self.direct
            .send(email)
            .await
            .map(|_| ())
            .map_err(|error| EmailError::SendError(error.to_string()))
    }
}

impl Socks5SmtpTransport {
    async fn send(&self, email: Message) -> std::result::Result<(), EmailError> {
        let proxy_address = (self.proxy_endpoint.host.as_str(), self.proxy_endpoint.port);
        let smtp_address = (self.smtp_host.as_str(), self.smtp_port);
        let socks = match &self.proxy.credentials {
            Some(credentials) => {
                Socks5Stream::connect_with_password(
                    proxy_address,
                    smtp_address,
                    &credentials.username,
                    &credentials.password,
                )
                .await
            }
            None => Socks5Stream::connect(proxy_address, smtp_address).await,
        }
        .map_err(|error| EmailError::SendError(format!("SOCKS5 connection failed: {error}")))?;

        let tcp_stream = socks.into_inner();
        let peer_addr = tcp_stream
            .peer_addr()
            .map_err(|error| EmailError::SendError(error.to_string()))?;
        let stream: Box<dyn AsyncTokioStream> = if self.use_tls {
            let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let tls_config = ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let server_name = ServerName::try_from(self.smtp_host.clone()).map_err(|_| {
                EmailError::SendError("SMTP host is not a valid TLS server name".to_string())
            })?;
            let stream = TlsConnector::from(Arc::new(tls_config))
                .connect(server_name, tcp_stream)
                .await
                .map_err(|error| {
                    EmailError::SendError(format!("SMTP TLS connection failed: {error}"))
                })?;
            Box::new(SmtpProxyIo { stream, peer_addr })
        } else {
            Box::new(SmtpProxyIo {
                stream: tcp_stream,
                peer_addr,
            })
        };

        let mut connection =
            AsyncSmtpConnection::connect_with_transport(stream, &ClientId::default())
                .await
                .map_err(|error| EmailError::SendError(error.to_string()))?;
        if let Some(credentials) = &self.smtp_credentials {
            connection
                .auth(DEFAULT_MECHANISMS, &credentials.lettre_credentials())
                .await
                .map_err(|error| EmailError::SendError(error.to_string()))?;
        }

        let envelope = email.envelope().clone();
        let formatted = email.formatted();
        connection
            .send(&envelope, &formatted)
            .await
            .map_err(|error| EmailError::SendError(error.to_string()))?;
        connection
            .quit()
            .await
            .map_err(|error| EmailError::SendError(error.to_string()))?;
        Ok(())
    }
}

#[derive(Debug)]
struct SmtpProxyIo<S> {
    stream: S,
    peer_addr: SocketAddr,
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for SmtpProxyIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(context, buffer)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for SmtpProxyIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

impl<S> AsyncTokioStream for SmtpProxyIo<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + std::fmt::Debug,
{
    fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.peer_addr)
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

    fn credentials(username: &str, password: &str) -> SmtpCredentials {
        SmtpCredentials {
            username: username.to_string(),
            password: password.to_string(),
        }
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
    fn test_smtp_proxy_config_validation() {
        let proxy = SmtpProxyConfig {
            url: "socks5://proxy.example.com:1081".to_string(),
            credentials: Some(credentials("proxy-user", "proxy-password")),
        };
        assert!(proxy.validate().is_ok());
        let endpoint = ok(proxy.endpoint(), "valid SOCKS5 endpoint");
        assert_eq!(endpoint.host, "proxy.example.com");
        assert_eq!(endpoint.port, 1081);

        for url in [
            "http://proxy.example.com:8080",
            "socks5://user:secret@proxy.example.com:1080",
        ] {
            assert!(SmtpProxyConfig {
                url: url.to_string(),
                credentials: None,
            }
            .validate()
            .is_err());
        }
        assert!(SmtpCredentials {
            username: "proxy-user".to_string(),
            password: String::new(),
        }
        .validate()
        .is_err());
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
            smtp_credentials: Some(credentials("test", "test")),
            smtp_proxy: None,
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
            smtp_credentials: Some(credentials("user", "password")),
            smtp_proxy: None,
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
            smtp_credentials: Some(credentials("user", "password")),
            smtp_proxy: None,
            from_email: "noreply@example.com".to_string(),
            from_name: "SyncTV".to_string(),
            use_tls: false,
        }));

        let first = EmailConfig {
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_credentials: Some(credentials("user", "password")),
            smtp_proxy: None,
            from_email: "noreply@example.com".to_string(),
            from_name: "SyncTV".to_string(),
            use_tls: false,
        };
        ok(
            service.smtp_transport_for_settings(&first),
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
            smtp_credentials: Some(credentials("next-user", "next-password")),
            smtp_proxy: Some(SmtpProxyConfig {
                url: "socks5://proxy.example.com:1080".to_string(),
                credentials: Some(credentials("proxy-user", "proxy-password")),
            }),
            from_email: "mailer@example.com".to_string(),
            from_name: "SyncTV Mail".to_string(),
            use_tls: false,
        };
        ok(
            service.smtp_transport_for_settings(&second),
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

    #[tokio::test]
    async fn test_sends_email_through_socks5_proxy() {
        use tokio::{
            io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
            net::{TcpListener, TcpStream},
        };

        let smtp_listener = ok(
            TcpListener::bind("127.0.0.1:0").await,
            "bind test SMTP listener",
        );
        let smtp_address = ok(smtp_listener.local_addr(), "SMTP listener address");
        let smtp_task = tokio::spawn(async move {
            let (stream, _) = smtp_listener.accept().await?;
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            writer.write_all(b"220 smtp.test ESMTP\r\n").await?;

            let mut line = String::new();
            reader.read_line(&mut line).await?;
            assert!(line.starts_with("EHLO "));
            writer
                .write_all(b"250-smtp.test\r\n250-AUTH PLAIN LOGIN\r\n250 OK\r\n")
                .await?;

            loop {
                line.clear();
                if reader.read_line(&mut line).await? == 0 {
                    break;
                }
                if line.starts_with("AUTH ") {
                    writer.write_all(b"235 authenticated\r\n").await?;
                } else if line.starts_with("MAIL FROM:") || line.starts_with("RCPT TO:") {
                    writer.write_all(b"250 OK\r\n").await?;
                } else if line == "DATA\r\n" {
                    writer.write_all(b"354 send message\r\n").await?;
                    loop {
                        line.clear();
                        reader.read_line(&mut line).await?;
                        if line == ".\r\n" {
                            break;
                        }
                    }
                    writer.write_all(b"250 queued\r\n").await?;
                } else if line == "QUIT\r\n" {
                    writer.write_all(b"221 bye\r\n").await?;
                    break;
                }
            }
            io::Result::Ok(())
        });

        let proxy_listener = ok(
            TcpListener::bind("127.0.0.1:0").await,
            "bind test SOCKS5 listener",
        );
        let proxy_address = ok(proxy_listener.local_addr(), "SOCKS5 listener address");
        let proxy_task = tokio::spawn(async move {
            let (mut client, _) = proxy_listener.accept().await?;
            let mut greeting = [0_u8; 2];
            client.read_exact(&mut greeting).await?;
            assert_eq!(greeting[0], 5);
            let mut methods = vec![0_u8; greeting[1] as usize];
            client.read_exact(&mut methods).await?;
            assert!(methods.contains(&0));
            client.write_all(&[5, 0]).await?;

            let mut request = [0_u8; 4];
            client.read_exact(&mut request).await?;
            assert_eq!(request, [5, 1, 0, 3]);
            let domain_len = client.read_u8().await? as usize;
            let mut domain = vec![0_u8; domain_len];
            client.read_exact(&mut domain).await?;
            assert_eq!(domain, b"smtp.internal.test");
            let requested_port = client.read_u16().await?;
            assert_eq!(requested_port, smtp_address.port());

            let mut upstream = TcpStream::connect(smtp_address).await?;
            client.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await?;
            tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
            io::Result::Ok(())
        });

        let service = test_service(Some(EmailConfig {
            smtp_host: "smtp.internal.test".to_string(),
            smtp_port: smtp_address.port(),
            smtp_credentials: Some(credentials("smtp-user", "smtp-password")),
            smtp_proxy: Some(SmtpProxyConfig {
                url: format!("socks5://{proxy_address}"),
                credentials: None,
            }),
            from_email: "noreply@example.com".to_string(),
            from_name: "SyncTV".to_string(),
            use_tls: false,
        }));

        ok(
            ok(
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    service.send_test_email("recipient@example.com"),
                )
                .await,
                "SOCKS5 SMTP send timeout",
            ),
            "send through SOCKS5 proxy",
        );
        ok(ok(proxy_task.await, "join SOCKS5 task"), "SOCKS5 task");
        ok(ok(smtp_task.await, "join SMTP task"), "SMTP task");
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
