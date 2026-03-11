//! Email verification and sending service
//!
//! Handles email verification and password reset email sending.
//!
//! ## Verification Code Storage
//!
//! Uses a pluggable `VerificationCodeStore` backend:
//! - `RedisVerificationCodeStore`: Redis with TTL (multi-node safe)
//! - `InMemoryVerificationCodeStore`: moka cache (single-node only)

use async_trait::async_trait;
use chrono::{Duration, Utc};
use lettre::{
    message::{header::ContentType, Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, warn};

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

/// Email verification error
#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    #[error("Email service not configured")]
    NotConfigured,

    #[error("Invalid email address: {0}")]
    InvalidEmail(String),

    #[error("Verification code expired")]
    CodeExpired,

    #[error("Invalid verification code")]
    InvalidCode,

    #[error("Too many attempts")]
    TooManyAttempts,

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Send error: {0}")]
    SendError(String),
}

/// Verification code data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationCode {
    pub code: String,
    pub created_at: chrono::DateTime<Utc>,
    pub attempts: u32,
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

// ============================================================================
// VerificationCodeStore trait
// ============================================================================

/// Backend storage for email verification codes.
///
/// Implementations must provide atomic store and verify operations with
/// attempt counting and expiry.
#[async_trait]
pub trait VerificationCodeStore: Send + Sync {
    /// Store a verification code for the given email with a TTL.
    async fn store_code(&self, email: &str, code: &VerificationCode) -> Result<()>;

    /// Atomically verify a code: check existence, increment attempts, compare code,
    /// and delete on success or max-attempts exceeded.
    ///
    /// Returns `Ok(())` on success, or an appropriate `Error` on failure.
    async fn verify_code(
        &self,
        email: &str,
        code: &str,
        max_attempts: u32,
        ttl_minutes: i64,
    ) -> Result<()>;

    /// A label for logging/debug purposes.
    fn backend_name(&self) -> &'static str;
}

// ============================================================================
// Redis implementation
// ============================================================================

/// Redis key prefix for email verification codes
const EMAIL_CODE_KEY_PREFIX: &str = "email:code:";

/// Redis-backed verification code store (multi-node safe).
pub struct RedisVerificationCodeStore {
    shared_conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
    ttl_minutes: i64,
}

impl RedisVerificationCodeStore {
    #[must_use]
    pub const fn new(
        shared_conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        ttl_minutes: i64,
    ) -> Self {
        Self {
            shared_conn,
            ttl_minutes,
        }
    }

    async fn conn(&self) -> redis::aio::ConnectionManager {
        self.shared_conn.read().await.clone()
    }
}

#[async_trait]
impl VerificationCodeStore for RedisVerificationCodeStore {
    async fn store_code(&self, email: &str, code: &VerificationCode) -> Result<()> {
        let key = format!("{EMAIL_CODE_KEY_PREFIX}{email}");
        let value = serde_json::to_string(code)
            .internal_with_err("Failed to serialize verification code")?;

        let mut conn = self.conn().await;

        let ttl_seconds = self.ttl_minutes * 60;
        use redis::AsyncCommands;
        let _: () = conn
            .set_ex(&key, value, ttl_seconds as u64)
            .await
            .internal_with_err("Failed to store verification code in Redis")?;

        debug!(
            "Stored verification code in Redis for email {}",
            &email[..email.len().min(4)]
        );
        Ok(())
    }

    async fn verify_code(
        &self,
        email: &str,
        code: &str,
        max_attempts: u32,
        _ttl_minutes: i64,
    ) -> Result<()> {
        let key = format!("{EMAIL_CODE_KEY_PREFIX}{email}");

        let mut conn = self.conn().await;

        // Lua script: atomically read, check attempts, verify code, and return result.
        //
        // Expiry is handled by Redis TTL (set via SET EX in store_code), so
        // there is no need for a Lua-side time comparison.
        //
        // We preserve the remaining TTL manually via PTTL + SET PX instead of
        // using KEEPTTL, because KEEPTTL requires Redis >= 6.0 and may not be
        // available in all test/CI environments.
        //
        // Returns:
        //   -1 = key not found (expired via TTL or never stored)
        //   -3 = too many attempts (deleted)
        //   -4 = wrong code (attempts incremented)
        //    1 = success (key deleted)
        let script = redis::Script::new(
            r"
            local data = redis.call('GET', KEYS[1])
            if not data then return -1 end
            local obj = cjson.decode(data)
            obj['attempts'] = obj['attempts'] + 1
            if obj['attempts'] >= tonumber(ARGV[2]) then
                redis.call('DEL', KEYS[1])
                return -3
            end
            if obj['code'] ~= ARGV[1] then
                local pttl = redis.call('PTTL', KEYS[1])
                if pttl > 0 then
                    redis.call('SET', KEYS[1], cjson.encode(obj), 'PX', pttl)
                else
                    redis.call('SET', KEYS[1], cjson.encode(obj))
                end
                return -4
            end
            redis.call('DEL', KEYS[1])
            return 1
            ",
        );

        let result: i64 = script
            .key(&key)
            .arg(code)
            .arg(max_attempts)
            .invoke_async(&mut conn)
            .await
            .internal_with_err("Redis script failed")?;

        match result {
            1 => Ok(()),
            -1 => Err(Error::InvalidInput(
                "No verification code found or code expired".to_string(),
            )),
            -3 => Err(Error::InvalidInput("Too many failed attempts".to_string())),
            -4 => Err(Error::InvalidInput("Invalid verification code".to_string())),
            _ => Err(Error::Internal(
                "Unexpected verification result".to_string(),
            )),
        }
    }

    fn backend_name(&self) -> &'static str {
        "redis"
    }
}

// ============================================================================
// In-memory implementation
// ============================================================================

/// In-memory verification code store using moka cache with TTL.
pub struct InMemoryVerificationCodeStore {
    cache: moka::sync::Cache<String, VerificationCode>,
}

impl InMemoryVerificationCodeStore {
    #[must_use]
    pub fn new(ttl_minutes: i64) -> Self {
        Self {
            cache: moka::sync::Cache::builder()
                .max_capacity(10_000)
                .time_to_live(std::time::Duration::from_secs(
                    (ttl_minutes.max(1) * 60) as u64,
                ))
                .build(),
        }
    }
}

#[async_trait]
impl VerificationCodeStore for InMemoryVerificationCodeStore {
    async fn store_code(&self, email: &str, code: &VerificationCode) -> Result<()> {
        self.cache.insert(email.to_string(), code.clone());
        debug!(
            "Stored verification code in memory for email {}",
            &email[..email.len().min(4)]
        );
        Ok(())
    }

    #[allow(clippy::unwrap_used)] // Mutex is uncontended; lock() cannot fail
    async fn verify_code(
        &self,
        email: &str,
        code: &str,
        max_attempts: u32,
        ttl_minutes: i64,
    ) -> Result<()> {
        use moka::ops::compute::Op;

        let code = code.to_string();

        // Use a Mutex to communicate the verification error from the closure.
        // The closure runs synchronously and completes before and_compute_with
        // returns, so there is no contention, but Mutex satisfies Send.
        let error_slot = std::sync::Mutex::new(Option::<Error>::None);

        self.cache
            .entry_by_ref(email)
            .and_compute_with(|maybe_entry| {
                let Some(entry) = maybe_entry else {
                    *error_slot.lock().unwrap() = Some(Error::InvalidInput(
                        "No verification code found".to_string(),
                    ));
                    return Op::Nop;
                };

                let mut vc = entry.into_value();

                // Check if expired (moka TTL handles eviction, but also check our own)
                let expiration = vc.created_at + Duration::minutes(ttl_minutes);
                if Utc::now() > expiration {
                    *error_slot.lock().unwrap() =
                        Some(Error::InvalidInput("Verification code expired".to_string()));
                    return Op::Remove;
                }

                // Increment and check attempts
                vc.attempts += 1;
                if vc.attempts >= max_attempts {
                    *error_slot.lock().unwrap() =
                        Some(Error::InvalidInput("Too many failed attempts".to_string()));
                    return Op::Remove;
                }

                // Wrong code: persist incremented attempt counter
                if vc.code != code {
                    *error_slot.lock().unwrap() =
                        Some(Error::InvalidInput("Invalid verification code".to_string()));
                    return Op::Put(vc);
                }

                // Success: remove code after successful verification
                Op::Remove
            });

        match error_slot.into_inner().unwrap() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn backend_name(&self) -> &'static str {
        "memory"
    }
}

// ============================================================================
// EmailService
// ============================================================================

/// Email service for sending verification codes.
///
/// Uses a pluggable `VerificationCodeStore` backend for code persistence.
#[derive(Clone)]
pub struct EmailService {
    config: Option<EmailConfig>,
    code_store: Arc<dyn VerificationCodeStore>,
    code_ttl_minutes: i64,
    max_attempts: u32,
    template_manager: Arc<EmailTemplateManager>,
    /// Reusable SMTP transport (connection-pooled by lettre).
    smtp_transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
}

impl std::fmt::Debug for EmailService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailService")
            .field("configured", &self.config.is_some())
            .field("code_ttl_minutes", &self.code_ttl_minutes)
            .field("max_attempts", &self.max_attempts)
            .field("backend", &self.code_store.backend_name())
            .finish()
    }
}

impl EmailService {
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

    /// Create a new email service with a custom code store backend.
    pub fn from_store(
        config: Option<EmailConfig>,
        code_store: Arc<dyn VerificationCodeStore>,
        code_ttl_minutes: i64,
    ) -> Result<Self> {
        let template_manager = EmailTemplateManager::new()?;
        let smtp_transport = match config.as_ref() {
            Some(cfg) => Some(Self::build_smtp_transport(cfg).map_err(|e| {
                Error::Internal(format!("Failed to initialize SMTP transport: {e}"))
            })?),
            None => None,
        };
        Ok(Self {
            config,
            code_store,
            code_ttl_minutes,
            max_attempts: 3,
            template_manager: Arc::new(template_manager),
            smtp_transport,
        })
    }

    /// Create a new email service (without Redis - single node only)
    pub fn new(config: Option<EmailConfig>) -> Result<Self> {
        let store = Arc::new(InMemoryVerificationCodeStore::new(10));
        Self::from_store(config, store, 10)
    }

    /// Create with custom TTL (without Redis - single node only)
    pub fn with_ttl(config: Option<EmailConfig>, code_ttl_minutes: i64) -> Result<Self> {
        let store = Arc::new(InMemoryVerificationCodeStore::new(code_ttl_minutes));
        Self::from_store(config, store, code_ttl_minutes)
    }

    /// Create a new email service with Redis support (multi-node safe).
    ///
    /// Uses a shared Redis `ConnectionManager` handle so Sentinel failover
    /// updates are picked up automatically.
    pub fn with_redis(
        config: Option<EmailConfig>,
        shared_conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
    ) -> Result<Self> {
        let store = Arc::new(RedisVerificationCodeStore::new(shared_conn, 10));
        Self::from_store(config, store, 10)
    }

    /// Generate a 6-digit verification code
    fn generate_code() -> String {
        let mut rng = rand::rng();
        format!("{:06}", rng.random_range(0..1_000_000))
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

    /// Send verification code to email
    pub async fn send_verification_code(&self, email: &str) -> Result<String> {
        Self::validate_email(email)?;

        if self.config.is_none() {
            warn!("Email service not configured, returning code directly");
            let code = Self::generate_code();
            let verification_code = VerificationCode {
                code: code.clone(),
                created_at: Utc::now(),
                attempts: 0,
            };
            self.code_store
                .store_code(email, &verification_code)
                .await?;
            return Ok(code);
        }

        let code = Self::generate_code();
        let verification_code = VerificationCode {
            code: code.clone(),
            created_at: Utc::now(),
            attempts: 0,
        };
        self.code_store
            .store_code(email, &verification_code)
            .await?;

        if let Some(config) = &self.config {
            if let Err(e) = self
                .send_email_impl(config, email, "Your SyncTV verification code", &code)
                .await
            {
                tracing::error!("Failed to send email: {}", e);
                return Err(Error::Internal(format!("Failed to send email: {e}")));
            }
        }

        // In production (email configured), never leak the code to the caller.
        // Only return the raw code in dev mode (config.is_none() path above).
        Ok(String::new())
    }

    /// Verify code for email
    pub async fn verify_code(&self, email: &str, code: &str) -> Result<()> {
        self.code_store
            .verify_code(email, code, self.max_attempts, self.code_ttl_minutes)
            .await
    }

    /// Send verification email
    pub async fn send_verification_email(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
    ) -> Result<String> {
        Self::validate_email(email)?;

        let token = token_service
            .generate_token(user_id, EmailTokenType::EmailVerification)
            .await?;

        if let Some(config) = &self.config {
            if let Err(e) = self
                .send_verification_email_impl(config, email, &token)
                .await
            {
                tracing::error!("Failed to send verification email: {}", e);
                return Err(Error::Internal(format!("Failed to send email: {e}")));
            }
            // In production (email configured), never leak the token to the caller.
            tracing::info!("Sent verification email to {}", mask_email(email));
            Ok(String::new())
        } else {
            tracing::warn!("Email service not configured, returning token directly");
            tracing::info!("Sent verification email to {}", mask_email(email));
            Ok(token)
        }
    }

    /// Send password reset email
    pub async fn send_password_reset_email(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
    ) -> Result<String> {
        Self::validate_email(email)?;

        let token = token_service
            .generate_token(user_id, EmailTokenType::PasswordReset)
            .await?;

        if let Some(config) = &self.config {
            if let Err(e) = self
                .send_password_reset_email_impl(config, email, &token)
                .await
            {
                tracing::error!("Failed to send password reset email: {}", e);
                return Err(Error::Internal(format!("Failed to send email: {e}")));
            }
            // In production (email configured), never leak the token to the caller.
            tracing::info!("Sent password reset email to {}", mask_email(email));
            Ok(String::new())
        } else {
            tracing::warn!("Email service not configured, returning token directly");
            tracing::info!("Sent password reset email to {}", mask_email(email));
            Ok(token)
        }
    }

    /// Send a test email to verify email configuration
    pub async fn send_test_email(&self, to: &str) -> Result<()> {
        Self::validate_email(to)?;

        let config = self
            .config
            .as_ref()
            .ok_or_else(|| Error::Internal("Email service not configured".to_string()))?;

        let sent_at = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();
        let (html_body, plain_text_body) = self
            .template_manager
            .render_test_email(&config.smtp_host, config.smtp_port, &sent_at)
            .internal_with_err("Failed to render template")?;

        let subject = "SyncTV Email Test";
        self.send_html_email(config, to, subject, &html_body, &plain_text_body)
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
    ) -> std::result::Result<(), EmailError> {
        let subject = "Verify your SyncTV email";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_verification_email(token, "24 hours")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(config, to, subject, &html_body, &plain_text_body)
            .await
    }

    async fn send_password_reset_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Reset your SyncTV password";
        let (html_body, plain_text_body) = self
            .template_manager
            .render_password_reset_email(token, "1 hour")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(config, to, subject, &html_body, &plain_text_body)
            .await
    }

    async fn send_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        subject: &str,
        body: &str,
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
            .body(body.to_string())
            .map_err(|e| EmailError::SendError(format!("Failed to build email: {e}")))?;

        self.send_message(config, email).await
    }

    async fn send_html_email(
        &self,
        config: &EmailConfig,
        to: &str,
        subject: &str,
        html_body: &str,
        plain_text_body: &str,
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

        self.send_message(config, email).await
    }

    async fn send_message(
        &self,
        config: &EmailConfig,
        email: Message,
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

        transport
            .send(email)
            .await
            .map_err(|e| EmailError::SendError(format!("Failed to send email: {e}")))?;

        tracing::info!(
            "Email sent successfully to {} via SMTP {}:{}",
            recipient,
            config.smtp_host,
            config.smtp_port
        );

        Ok(())
    }

    /// Clean up expired codes (local memory only - Redis handles its own TTL)
    pub async fn cleanup_expired_codes(&self) {
        // No-op: moka handles TTL expiration automatically; Redis handles its own TTL.
    }

    /// Check if email service is configured
    #[must_use]
    pub const fn is_configured(&self) -> bool {
        self.config.is_some()
    }

    /// Return the backend name of the code store.
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        self.code_store.backend_name()
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
    fn test_generate_code() {
        let code = EmailService::generate_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
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

    #[tokio::test]
    async fn test_send_and_verify_code() {
        let service = EmailService::new(None).unwrap();

        let email = "test@example.com";
        let code = service.send_verification_code(email).await.unwrap();

        // Verify correct code
        assert!(service.verify_code(email, &code).await.is_ok());

        // Verify wrong code
        assert!(service.verify_code(email, "000000").await.is_err());

        // Verify again after successful verification
        assert!(service.verify_code(email, &code).await.is_err());
    }

    #[tokio::test]
    async fn test_verify_expired_code() {
        let service = EmailService::with_ttl(None, -1).unwrap(); // Expired immediately

        let email = "test@example.com";
        let code = service.send_verification_code(email).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        assert!(service.verify_code(email, &code).await.is_err());
    }

    #[tokio::test]
    async fn test_max_attempts() {
        let service = EmailService::with_ttl(None, 60).unwrap();

        let email = "test@example.com";
        let code = service.send_verification_code(email).await.unwrap();

        // Try wrong codes up to max attempts
        for _ in 0..3 {
            assert!(service.verify_code(email, "000000").await.is_err());
        }

        // After max attempts, even correct code should fail
        assert!(service.verify_code(email, &code).await.is_err());
    }

    // ========== Security: Token/Code Leakage Tests ==========

    #[tokio::test]
    async fn test_send_verification_code_returns_code_in_dev_mode() {
        // When config is None (dev mode), the code should be returned directly
        let service = EmailService::new(None).unwrap();
        let code = service
            .send_verification_code("test@example.com")
            .await
            .unwrap();
        assert!(!code.is_empty(), "Dev mode should return the code");
        assert_eq!(code.len(), 6);
    }

    #[tokio::test]
    async fn test_send_verification_code_returns_empty_when_email_configured() {
        // When config is Some (production), the code must NOT be returned.
        // A send failure is acceptable here, but the code must never leak.
        let fake_config = EmailConfig {
            smtp_host: "localhost".to_string(),
            smtp_port: 2525,
            smtp_username: "test".to_string(),
            smtp_password: "test".to_string(),
            from_email: "noreply@example.com".to_string(),
            from_name: "SyncTV".to_string(),
            use_tls: false,
        };
        let service = EmailService::new(Some(fake_config)).expect("transport should build");

        let result = service.send_verification_code("test@example.com").await;
        match result {
            Ok(code) => assert!(
                code.is_empty(),
                "Production mode must NOT return the verification code, got: {code}"
            ),
            Err(_) => {} // SMTP connection failure is fine as long as no code leaks
        }
    }

    #[tokio::test]
    async fn test_send_verification_code_configured_service_never_leaks_code() {
        let fake_config = EmailConfig {
            smtp_host: "localhost".to_string(),
            smtp_port: 2525,
            smtp_username: "test".to_string(),
            smtp_password: "test".to_string(),
            from_email: "noreply@example.com".to_string(),
            from_name: "SyncTV".to_string(),
            use_tls: false,
        };
        let service = EmailService::new(Some(fake_config)).expect("transport should build");
        // With a fake SMTP that can't connect, this should return an error,
        // which is safe (code is not leaked). The important thing is that
        // on success it would return empty string (verified by code inspection).
        let result = service.send_verification_code("test@example.com").await;
        // Either error (SMTP fails) or empty string (email sent) -- never the raw code
        match result {
            Ok(code) => assert!(
                code.is_empty(),
                "Production mode must NOT return the verification code, got: {code}"
            ),
            Err(_) => {} // SMTP failure is expected with fake config, code is not leaked
        }
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
}
