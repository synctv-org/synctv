//! Email verification and sending service
//!
//! Handles email verification and password reset email sending.
//!
//! ## Verification Code Storage
//! Verification codes are stored in Redis when available (for multi-node deployments).
//! Falls back to in-memory storage when Redis is not configured.

use chrono::{Duration, Utc};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{header::ContentType, Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
};
use tracing::{debug, warn};

use crate::{Error, Result, InternalExt};
use super::email_token::{EmailTokenService, EmailTokenType};
use super::email_templates::EmailTemplateManager;

/// Mask an email address for safe logging: `user***@example.com`
fn mask_email(email: &str) -> String {
    if let Some(at_pos) = email.find('@') {
        let local = &email[..at_pos];
        let domain = &email[at_pos..];
        let visible = local.len().min(3);
        format!("{}***{}", &local[..visible], domain)
    } else {
        "***".to_string()
    }
}

/// Redis key prefix for email verification codes
const EMAIL_CODE_KEY_PREFIX: &str = "email:code:";

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
struct VerificationCode {
    code: String,
    created_at: chrono::DateTime<Utc>,
    attempts: u32,
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

/// Email service for sending verification codes
///
/// Storage:
/// - When Redis is available: codes are stored in Redis with TTL (multi-node safe)
/// - When Redis is not available: codes are stored in memory (single-node only)
#[derive(Clone)]
pub struct EmailService {
    config: Option<EmailConfig>,
    /// In-memory code storage with TTL (fallback when Redis is not available)
    local_codes: Arc<moka::sync::Cache<String, VerificationCode>>,
    code_ttl_minutes: i64,
    max_attempts: u32,
    template_manager: Arc<EmailTemplateManager>,
    /// Optional Redis client for distributed code storage
    redis: Option<Arc<redis::Client>>,
}

impl std::fmt::Debug for EmailService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailService")
            .field("configured", &self.config.is_some())
            .field("code_ttl_minutes", &self.code_ttl_minutes)
            .field("max_attempts", &self.max_attempts)
            .finish()
    }
}

impl EmailService {
    /// Build a moka cache for local verification codes with the given TTL
    fn build_local_codes_cache(ttl_minutes: i64) -> moka::sync::Cache<String, VerificationCode> {
        moka::sync::Cache::builder()
            .max_capacity(10_000)
            .time_to_live(std::time::Duration::from_secs((ttl_minutes.max(1) * 60) as u64))
            .build()
    }

    /// Create a new email service (without Redis - single node only)
    pub fn new(config: Option<EmailConfig>) -> Result<Self> {
        let template_manager = EmailTemplateManager::new()?;
        Ok(Self {
            config,
            local_codes: Arc::new(Self::build_local_codes_cache(10)),
            code_ttl_minutes: 10, // 10 minutes default
            max_attempts: 3,
            template_manager: Arc::new(template_manager),
            redis: None,
        })
    }

    /// Create with custom TTL (without Redis - single node only)
    pub fn with_ttl(config: Option<EmailConfig>, code_ttl_minutes: i64) -> Result<Self> {
        let template_manager = EmailTemplateManager::new()?;
        Ok(Self {
            config,
            local_codes: Arc::new(Self::build_local_codes_cache(code_ttl_minutes)),
            code_ttl_minutes,
            max_attempts: 3,
            template_manager: Arc::new(template_manager),
            redis: None,
        })
    }

    /// Create a new email service with Redis support (multi-node safe)
    pub fn with_redis(config: Option<EmailConfig>, redis: Arc<redis::Client>) -> Result<Self> {
        let template_manager = EmailTemplateManager::new()?;
        Ok(Self {
            config,
            local_codes: Arc::new(Self::build_local_codes_cache(10)),
            code_ttl_minutes: 10,
            max_attempts: 3,
            template_manager: Arc::new(template_manager),
            redis: Some(redis),
        })
    }

    /// Store verification code (Redis if available, otherwise local memory)
    async fn store_code(&self, email: &str, code: &VerificationCode) -> Result<()> {
        if let Some(ref redis) = self.redis {
            let key = format!("{EMAIL_CODE_KEY_PREFIX}{email}");
            let value = serde_json::to_string(code)
                .internal_with_err("Failed to serialize verification code")?;

            let mut conn = redis
                .get_multiplexed_async_connection()
                .await
                .internal_with_err("Redis connection failed")?;

            let ttl_seconds = self.code_ttl_minutes * 60;
            use redis::AsyncCommands;
            let _: () = conn
                .set_ex(&key, value, ttl_seconds as u64)
                .await
                .internal_with_err("Failed to store verification code in Redis")?;

            debug!("Stored verification code in Redis for email {}", &email[..email.len().min(4)]);
        } else {
            self.local_codes.insert(email.to_string(), code.clone());
            debug!("Stored verification code in memory for email {}", &email[..email.len().min(4)]);
        }
        Ok(())
    }

    /// Get verification code (Redis if available, otherwise local memory)
    #[allow(dead_code)]
    async fn get_code(&self, email: &str) -> Result<Option<VerificationCode>> {
        if let Some(ref redis) = self.redis {
            let key = format!("{EMAIL_CODE_KEY_PREFIX}{email}");

            let mut conn = redis
                .get_multiplexed_async_connection()
                .await
                .internal_with_err("Redis connection failed")?;

            use redis::AsyncCommands;
            let value: Option<String> = conn
                .get(&key)
                .await
                .internal_with_err("Failed to get verification code from Redis")?;

            match value {
                Some(json) => {
                    let code: VerificationCode = serde_json::from_str(&json)
                        .internal_with_err("Failed to deserialize verification code")?;
                    Ok(Some(code))
                }
                None => Ok(None),
            }
        } else {
            Ok(self.local_codes.get(&email.to_string()))
        }
    }

    /// Update verification code (Redis if available, otherwise local memory)
    #[allow(dead_code)]
    async fn update_code(&self, email: &str, code: &VerificationCode) -> Result<()> {
        if self.redis.is_some() {
            // For Redis, we just store the updated code (TTL refresh is implicit)
            self.store_code(email, code).await
        } else {
            self.local_codes.insert(email.to_string(), code.clone());
            Ok(())
        }
    }

    /// Remove verification code (Redis if available, otherwise local memory)
    #[allow(dead_code)]
    async fn remove_code(&self, email: &str) -> Result<()> {
        if let Some(ref redis) = self.redis {
            let key = format!("{EMAIL_CODE_KEY_PREFIX}{email}");

            let mut conn = redis
                .get_multiplexed_async_connection()
                .await
                .internal_with_err("Redis connection failed")?;

            use redis::AsyncCommands;
            let _: () = conn
                .del(&key)
                .await
                .internal_with_err("Failed to remove verification code from Redis")?;

            debug!("Removed verification code from Redis for email {}", &email[..email.len().min(4)]);
        } else {
            self.local_codes.invalidate(&email.to_string());
        }
        Ok(())
    }

    /// Generate a 6-digit verification code
    fn generate_code() -> String {
        let mut rng = rand::rng();
        format!("{:06}", rng.random_range(0..1_000_000))
    }

    /// Validate email format (RFC 5322 compliant)
    fn validate_email(email: &str) -> Result<()> {
        let email = email.trim();

        // Check length constraints
        if email.is_empty() {
            return Err(Error::InvalidInput("Email cannot be empty".to_string()));
        }
        if email.len() > 254 {
            // RFC 5321 maximum email length
            return Err(Error::InvalidInput("Email too long (max 254 characters)".to_string()));
        }

        // Check for @ symbol
        if !email.contains('@') {
            return Err(Error::InvalidInput("Email must contain @ symbol".to_string()));
        }

        // Split and check parts
        let parts: Vec<&str> = email.split('@').collect();
        if parts.len() != 2 {
            return Err(Error::InvalidInput("Email must contain exactly one @ symbol".to_string()));
        }

        let local = parts[0];
        let domain = parts[1];

        // Validate local part (before @)
        if local.is_empty() {
            return Err(Error::InvalidInput("Email local part cannot be empty".to_string()));
        }
        if local.len() > 64 {
            // RFC 5321 local part max length
            return Err(Error::InvalidInput("Email local part too long (max 64 characters)".to_string()));
        }

        // Check for invalid characters in local part
        // Allow alphanumeric, dot, hyphen, underscore, plus
        if !local.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '+') {
            return Err(Error::InvalidInput("Email local part contains invalid characters".to_string()));
        }

        // Local part cannot start or end with dot
        if local.starts_with('.') || local.ends_with('.') {
            return Err(Error::InvalidInput("Email local part cannot start or end with dot".to_string()));
        }

        // Cannot have consecutive dots
        if local.contains("..") {
            return Err(Error::InvalidInput("Email local part cannot contain consecutive dots".to_string()));
        }

        // Validate domain part (after @)
        if domain.is_empty() {
            return Err(Error::InvalidInput("Email domain cannot be empty".to_string()));
        }
        if domain.len() > 253 {
            // RFC 1035 domain name max length
            return Err(Error::InvalidInput("Email domain too long (max 253 characters)".to_string()));
        }

        // Domain must contain at least one dot
        if !domain.contains('.') {
            return Err(Error::InvalidInput("Email domain must contain at least one dot".to_string()));
        }

        // Domain cannot start or end with dot or hyphen
        if domain.starts_with('.') || domain.ends_with('.') || domain.starts_with('-') || domain.ends_with('-') {
            return Err(Error::InvalidInput("Email domain has invalid format".to_string()));
        }

        // Validate domain labels (parts separated by dots)
        let domain_labels: Vec<&str> = domain.split('.').collect();
        for label in &domain_labels {
            if label.is_empty() {
                return Err(Error::InvalidInput("Email domain cannot have empty labels".to_string()));
            }
            if label.len() > 63 {
                // RFC 1035 label max length
                return Err(Error::InvalidInput("Email domain label too long (max 63 characters)".to_string()));
            }
            // Labels can only contain alphanumeric and hyphens (but not start/end with hyphen)
            if !label.chars().all(|c| c.is_alphanumeric() || c == '-') {
                return Err(Error::InvalidInput("Email domain contains invalid characters".to_string()));
            }
            if label.starts_with('-') || label.ends_with('-') {
                return Err(Error::InvalidInput("Email domain label cannot start or end with hyphen".to_string()));
            }
        }

        // Check TLD is at least 2 characters and alphabetic
        if let Some(tld) = domain_labels.last() {
            if tld.len() < 2 {
                return Err(Error::InvalidInput("Email domain TLD must be at least 2 characters".to_string()));
            }
            if !tld.chars().all(char::is_alphabetic) {
                return Err(Error::InvalidInput("Email domain TLD must be alphabetic".to_string()));
            }
        }

        Ok(())
    }

    /// Send verification code to email
    ///
    /// # Arguments
    /// * `email` - Email address to send code to
    ///
    /// # Returns
    /// The verification code (for testing purposes)
    pub async fn send_verification_code(&self, email: &str) -> Result<String> {
        // Validate email
        Self::validate_email(email)?;

        // Check if service is configured
        if self.config.is_none() {
            warn!("Email service not configured, returning code directly");
            let code = Self::generate_code();
            // Store code anyway
            let verification_code = VerificationCode {
                code: code.clone(),
                created_at: Utc::now(),
                attempts: 0,
            };
            self.store_code(email, &verification_code).await?;
            return Ok(code);
        }

        // Generate code
        let code = Self::generate_code();

        // Store code
        let verification_code = VerificationCode {
            code: code.clone(),
            created_at: Utc::now(),
            attempts: 0,
        };
        self.store_code(email, &verification_code).await?;

        // Send email
        if let Some(config) = &self.config {
            if let Err(e) = self.send_email(config, email, &code).await {
                tracing::error!("Failed to send email: {}", e);
                return Err(Error::Internal(format!("Failed to send email: {e}")));
            }
        }

        Ok(code)
    }

    /// Verify code for email
    ///
    /// Uses atomic check-and-update to prevent concurrent verification attempts
    /// from bypassing the `max_attempts` check.
    pub async fn verify_code(&self, email: &str, code: &str) -> Result<()> {
        if let Some(ref redis) = self.redis {
            // Redis path: use Lua script for atomic read-check-increment-return
            self.verify_code_redis(redis, email, code).await
        } else {
            // Memory path: hold write lock across the entire read-modify-write
            self.verify_code_memory(email, code).await
        }
    }

    /// Atomic verify for Redis using a Lua script
    async fn verify_code_redis(
        &self,
        redis: &redis::Client,
        email: &str,
        code: &str,
    ) -> Result<()> {
        let key = format!("{EMAIL_CODE_KEY_PREFIX}{email}");

        let mut conn = redis
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| Error::Internal(format!("Redis connection failed: {e}")))?;

        // Lua script: atomically read, check attempts, verify code, and return result.
        //
        // Expiry is handled by Redis TTL (set via SET EX in store_code), so
        // there is no need for a Lua-side time comparison. Previously the script
        // tried `tonumber(obj['created_at'])` which returned nil because chrono
        // serialises DateTime as an ISO-8601 string, not numeric millis.
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
            if obj['attempts'] > tonumber(ARGV[2]) then
                redis.call('DEL', KEYS[1])
                return -3
            end
            if obj['code'] ~= ARGV[1] then
                redis.call('SET', KEYS[1], cjson.encode(obj), 'KEEPTTL')
                return -4
            end
            redis.call('DEL', KEYS[1])
            return 1
            ",
        );

        let result: i64 = script
            .key(&key)
            .arg(code)
            .arg(self.max_attempts)
            .invoke_async(&mut conn)
            .await
            .internal_with_err("Redis script failed")?;

        match result {
            1 => Ok(()),
            -1 => Err(Error::InvalidInput("No verification code found or code expired".to_string())),
            -3 => Err(Error::InvalidInput("Too many failed attempts".to_string())),
            -4 => Err(Error::InvalidInput("Invalid verification code".to_string())),
            _ => Err(Error::Internal("Unexpected verification result".to_string())),
        }
    }

    /// Atomic verify for in-memory storage using moka cache.
    ///
    /// Uses `entry_by_ref().and_compute_with()` for atomic read-modify-write to
    /// prevent TOCTOU races where concurrent calls could both read the same
    /// attempt count and bypass `max_attempts`. The closure runs under moka's
    /// per-key lock, so concurrent verify calls for the same email are serialized.
    async fn verify_code_memory(&self, email: &str, code: &str) -> Result<()> {
        use moka::ops::compute::Op;

        let max_attempts = self.max_attempts;
        let ttl_minutes = self.code_ttl_minutes;
        let code = code.to_string();

        // Use a Mutex to communicate the verification error from the closure.
        // The closure runs synchronously and completes before and_compute_with
        // returns, so there is no contention, but Mutex satisfies Send.
        let error_slot = std::sync::Mutex::new(Option::<Error>::None);

        self.local_codes.entry_by_ref(email).and_compute_with(|maybe_entry| {
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
                *error_slot.lock().unwrap() = Some(Error::InvalidInput(
                    "Verification code expired".to_string(),
                ));
                return Op::Remove;
            }

            // Increment and check attempts
            vc.attempts += 1;
            if vc.attempts > max_attempts {
                *error_slot.lock().unwrap() = Some(Error::InvalidInput(
                    "Too many failed attempts".to_string(),
                ));
                return Op::Remove;
            }

            // Wrong code: persist incremented attempt counter
            if vc.code != code {
                *error_slot.lock().unwrap() = Some(Error::InvalidInput(
                    "Invalid verification code".to_string(),
                ));
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

    /// Send email using SMTP (deprecated - use `send_email_impl` instead)
    #[allow(dead_code)]
    async fn send_email(&self, config: &EmailConfig, to: &str, code: &str) -> std::result::Result<(), EmailError> {
        let subject = "SyncTV - Verification Code";
        let body = format!(
            "SyncTV Email Verification\n\nYour verification code is: {}\n\nThis code will expire in {} minutes.\nIf you didn't request this code, please ignore this email.",
            code, self.code_ttl_minutes
        );

        self.send_email_impl(config, to, subject, &body).await
    }

    /// Send verification email
    ///
    /// Generates a token and sends verification email
    /// Returns the token (for testing)
    pub async fn send_verification_email(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
    ) -> Result<String> {
        // Validate email
        Self::validate_email(email)?;

        // Generate token
        let token = token_service
            .generate_token(user_id, EmailTokenType::EmailVerification)
            .await?;

        // Send email
        if let Some(config) = &self.config {
            if let Err(e) = self
                .send_verification_email_impl(config, email, &token)
                .await
            {
                tracing::error!("Failed to send verification email: {}", e);
                return Err(Error::Internal(format!("Failed to send email: {e}")));
            }
        } else {
            tracing::warn!("Email service not configured, returning token directly");
        }

        tracing::info!("Sent verification email to {}", mask_email(email));
        Ok(token)
    }

    /// Send password reset email
    ///
    /// Generates a token and sends password reset email
    /// Returns the token (for testing)
    pub async fn send_password_reset_email(
        &self,
        email: &str,
        token_service: &EmailTokenService,
        user_id: &crate::models::UserId,
    ) -> Result<String> {
        // Validate email
        Self::validate_email(email)?;

        // Generate token
        let token = token_service
            .generate_token(user_id, EmailTokenType::PasswordReset)
            .await?;

        // Send email
        if let Some(config) = &self.config {
            if let Err(e) = self
                .send_password_reset_email_impl(config, email, &token)
                .await
            {
                tracing::error!("Failed to send password reset email: {}", e);
                return Err(Error::Internal(format!("Failed to send email: {e}")));
            }
        } else {
            tracing::warn!("Email service not configured, returning token directly");
        }

        tracing::info!("Sent password reset email to {}", mask_email(email));
        Ok(token)
    }

    /// Send a test email to verify email configuration
    ///
    /// This is used by admins to test email settings
    pub async fn send_test_email(&self, to: &str) -> Result<()> {
        // Validate email
        Self::validate_email(to)?;

        // Check if service is configured
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| Error::Internal("Email service not configured".to_string()))?;

        // Render template
        let sent_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
        let (html_body, plain_text_body) = self.template_manager
            .render_test_email(&config.smtp_host, config.smtp_port, &sent_at)
            .internal_with_err("Failed to render template")?;

        // Send test email
        let subject = "SyncTV Email Test";
        self.send_html_email(config, to, subject, &html_body, &plain_text_body)
            .await
            .internal_with_err("Failed to send test email")?;

        tracing::info!("Sent test email to {}", mask_email(to));
        Ok(())
    }

    /// Send verification email implementation
    async fn send_verification_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Verify your SyncTV email";
        let (html_body, plain_text_body) = self.template_manager
            .render_verification_email(token, "24 hours")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(config, to, subject, &html_body, &plain_text_body).await
    }

    /// Send password reset email implementation
    async fn send_password_reset_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        token: &str,
    ) -> std::result::Result<(), EmailError> {
        let subject = "Reset your SyncTV password";
        let (html_body, plain_text_body) = self.template_manager
            .render_password_reset_email(token, "1 hour")
            .map_err(|e| EmailError::SendError(format!("Failed to render template: {e}")))?;

        self.send_html_email(config, to, subject, &html_body, &plain_text_body).await
    }

    /// Send plain text email using SMTP
    async fn send_email_impl(
        &self,
        config: &EmailConfig,
        to: &str,
        subject: &str,
        body: &str,
    ) -> std::result::Result<(), EmailError> {
        // Parse email addresses
        let from_mailbox: Mailbox = format!("{} <{}>", config.from_name, config.from_email)
            .parse()
            .map_err(|e| EmailError::SendError(format!("Invalid from address: {e}")))?;

        let to_mailbox: Mailbox = to
            .parse()
            .map_err(|e| EmailError::SendError(format!("Invalid to address: {e}")))?;

        // Build message
        let email = Message::builder()
            .from(from_mailbox)
            .to(to_mailbox)
            .subject(subject)
            .body(body.to_string())
            .map_err(|e| EmailError::SendError(format!("Failed to build email: {e}")))?;

        // Send email
        self.send_message(config, email).await
    }

    /// Send HTML email with plain text fallback
    async fn send_html_email(
        &self,
        config: &EmailConfig,
        to: &str,
        subject: &str,
        html_body: &str,
        plain_text_body: &str,
    ) -> std::result::Result<(), EmailError> {
        // Parse email addresses
        let from_mailbox: Mailbox = format!("{} <{}>", config.from_name, config.from_email)
            .parse()
            .map_err(|e| EmailError::SendError(format!("Invalid from address: {e}")))?;

        let to_mailbox: Mailbox = to
            .parse()
            .map_err(|e| EmailError::SendError(format!("Invalid to address: {e}")))?;

        // Build multipart message (HTML + plain text fallback)
        let email = Message::builder()
            .from(from_mailbox)
            .to(to_mailbox)
            .subject(subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(plain_text_body.to_string())
                    )
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(html_body.to_string())
                    )
            )
            .map_err(|e| EmailError::SendError(format!("Failed to build email: {e}")))?;

        // Send email
        self.send_message(config, email).await
    }

    /// Send email message via SMTP
    async fn send_message(
        &self,
        config: &EmailConfig,
        email: Message,
    ) -> std::result::Result<(), EmailError> {
        // Get recipient before consuming email
        let recipient = email.envelope().to().first()
            .ok_or_else(|| EmailError::SendError("No recipients in email envelope".to_string()))?
            .clone();

        // Create SMTP credentials
        let creds = Credentials::new(
            config.smtp_username.clone(),
            config.smtp_password.clone(),
        );

        // Create SMTP transport
        let transport = if config.use_tls {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&config.smtp_host)
                .map_err(|e| EmailError::SendError(format!("Failed to create SMTP transport: {e}")))?
                .credentials(creds)
                .port(config.smtp_port)
                .build()
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.smtp_host)
                .credentials(creds)
                .port(config.smtp_port)
                .build()
        };

        // Send email
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
    ///
    /// Note: moka cache handles TTL-based expiration automatically.
    /// This method triggers a manual sync of pending evictions.
    pub async fn cleanup_expired_codes(&self) {
        // moka handles TTL expiration automatically; run_pending_tasks flushes evictions
        self.local_codes.run_pending_tasks();
    }

    /// Check if email service is configured
    #[must_use]
    pub const fn is_configured(&self) -> bool {
        self.config.is_some()
    }

    /// Check if Redis is being used for code storage
    #[must_use]
    pub const fn uses_redis(&self) -> bool {
        self.redis.is_some()
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

        // Wait a bit to ensure expiration
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
}
