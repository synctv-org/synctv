//! Input validation utilities for HTTP endpoints
//!
//! This module provides validation functions for common input types to ensure
//! data integrity and prevent security issues like injection attacks.

use axum::{
    extract::{rejection::QueryRejection, FromRequestParts, Query},
    http::request::Parts,
};
use prost_reflect::ReflectMessage;
use regex::Regex;
use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::sync::LazyLock;

/// Maximum lengths for various input types
pub mod limits {
    // Core limits imported from the single source of truth in synctv-core
    pub use synctv_core::validation::{
        PASSWORD_MAX, PASSWORD_MIN, ROOM_NAME_MAX, ROOM_NAME_MIN, USERNAME_MAX, USERNAME_MIN,
    };

    /// Maximum room description length
    pub const ROOM_DESCRIPTION_MAX: usize = 500;
    /// Maximum media title length
    pub const MEDIA_TITLE_MAX: usize = 500;
    /// Maximum chat message length
    pub const CHAT_MESSAGE_MAX: usize = 5000;
    /// Maximum URL length
    pub const URL_MAX: usize = 2048;
    /// Maximum email length
    pub const EMAIL_MAX: usize = 254;
    /// Maximum generic public ID length (`user_id`, `media_id`, playlist refs, etc.)
    pub const ID_MAX: usize = 64;
    /// Maximum `OAuth2` redirect URL length
    pub const OAUTH2_REDIRECT_URL_MAX: usize = 2048;
    /// Maximum `OAuth2` provider user ID length
    pub const OAUTH2_PROVIDER_USER_ID_MAX: usize = 256;
    /// `OAuth2` state token length (shared base62 generator emits 32 chars)
    pub const OAUTH2_STATE_LENGTH: usize = 32;
    /// `OAuth2` authorization code max length
    pub const OAUTH2_CODE_MAX: usize = 256;
}

/// Regex patterns for validation
mod patterns {
    use super::{LazyLock, Regex};

    /// Valid username: alphanumeric, underscores, hyphens, and CJK characters
    pub static USERNAME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}_-]+$").expect("Invalid username regex"));

    /// URL format (http/https only)
    pub static URL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^https?://[^\s]+$").expect("Invalid URL regex"));

    /// HTML/script tag detection for XSS prevention
    pub static HTML_TAGS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<[^>]+>").expect("Invalid HTML regex"));

    /// Control characters that should be stripped
    pub static CONTROL_CHARS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]").expect("Invalid control char regex")
    });

    /// Public ID syntax: typed prefix plus runtime-configured encoded body.
    pub static PUBLIC_ID: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(usr|room|med|pl|rev|ban)_[A-Za-z0-9]+$").expect("Invalid public ID regex")
    });
}

/// Common weak passwords rejected on exact match (case-insensitive).
///
/// Sourced from the intersection of NCSC top-100k, HIBP Pwned Passwords
/// top-1000, and `SplashData` annual worst-passwords lists. Kept small enough
/// for an O(n) linear scan (sub-microsecond for ~40 entries).
const COMMON_PASSWORDS: &[&str] = &[
    // Top-10 most breached
    "password",
    "123456",
    "12345678",
    "123456789",
    "1234567890",
    "qwerty",
    "abc123",
    "111111",
    "password1",
    "iloveyou",
    // Common words / names
    "admin",
    "letmein",
    "welcome",
    "monkey",
    "dragon",
    "master",
    "login",
    "princess",
    "football",
    "shadow",
    "sunshine",
    "trustno1",
    "baseball",
    "superman",
    "michael",
    "access",
    "mustang",
    "batman",
    "passw0rd",
    // Keyboard walks
    "qwerty123",
    "qwertyuiop",
    "1q2w3e4r",
    "zxcvbnm",
    "asdfghjkl",
    "1qaz2wsx",
    // Numeric sequences
    "12345678901",
    "00000000",
    "11111111",
    // Year-based
    "password123",
];

/// Validation error type
#[derive(Debug, Clone, thiserror::Error)]
pub enum ValidationError {
    #[error("Input too long: {field} exceeds {max} characters (got {actual})")]
    TooLong {
        field: &'static str,
        max: usize,
        actual: usize,
    },
    #[error("Input too short: {field} must be at least {min} characters (got {actual})")]
    TooShort {
        field: &'static str,
        min: usize,
        actual: usize,
    },
    #[error("Invalid format: {field} contains invalid characters")]
    InvalidFormat { field: &'static str },
    #[error("Invalid value: {0}")]
    InvalidValue(&'static str),
    #[error("Field is required: {0}")]
    Required(&'static str),
    #[error("Potential security issue detected in input")]
    SecurityRisk,
}

/// Result type for validation operations
pub type ValidationResult<T> = std::result::Result<T, ValidationError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedQuery<T>(pub T);

impl<T> std::ops::Deref for ValidatedQuery<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for ValidatedQuery<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictQuery<T>(pub T);

impl<T> std::ops::Deref for StrictQuery<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for StrictQuery<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn map_query_rejection(rejection: &QueryRejection) -> super::AppError {
    super::AppError::new(rejection.status(), rejection.body_text())
}

impl<S, T> FromRequestParts<S> for ValidatedQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + ReflectMessage + Send,
{
    type Rejection = super::AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(value) = Query::<T>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| map_query_rejection(&rejection))?;
        crate::impls::validate_proto_request(&value).map_err(super::error::map_api_error)?;
        Ok(Self(value))
    }
}

impl<S, T> FromRequestParts<S> for StrictQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = super::AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(value) = Query::<T>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| map_query_rejection(&rejection))?;
        Ok(Self(value))
    }
}

pub fn garde_error(message: impl Into<String>) -> garde::Error {
    garde::Error::new(message.into())
}

pub fn map_garde_report(report: &garde::Report) -> super::AppError {
    super::AppError::bad_request(report.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtoEnumOption {
    pub raw: i32,
    pub label: &'static str,
}

pub const fn proto_enum_option(raw: i32, label: &'static str) -> ProtoEnumOption {
    ProtoEnumOption { raw, label }
}

fn format_proto_enum_options(expected: &[ProtoEnumOption]) -> String {
    expected
        .iter()
        .map(|option| format!("{} ({})", option.raw, option.label))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn parse_proto_enum_value(
    field: &'static str,
    value: Option<i32>,
    default_raw: i32,
    expected: &[ProtoEnumOption],
) -> Result<i32, super::AppError> {
    let resolved = value.unwrap_or(default_raw);

    if expected.iter().any(|option| option.raw == resolved) {
        Ok(resolved)
    } else {
        Err(super::AppError::bad_request(format!(
            "Invalid {field} '{resolved}'. Expected one of: {}",
            format_proto_enum_options(expected)
        )))
    }
}

pub fn parse_proto_enum_filter(
    field: &'static str,
    value: Option<i32>,
    unspecified_raw: i32,
    expected: &[ProtoEnumOption],
) -> Result<Option<i32>, super::AppError> {
    let resolved = parse_proto_enum_value(field, value, unspecified_raw, expected)?;
    Ok((resolved != unspecified_raw).then_some(resolved))
}

/// Sanitize a string by trimming whitespace and removing control characters
pub fn sanitize_string(input: &str) -> Cow<'_, str> {
    let trimmed = input.trim();

    // Check if we need to do any sanitization
    let has_control = patterns::CONTROL_CHARS.is_match(trimmed);

    if !has_control && trimmed.len() == input.len() {
        Cow::Borrowed(input)
    } else if !has_control {
        Cow::Owned(trimmed.to_string())
    } else {
        // Remove control characters
        Cow::Owned(
            patterns::CONTROL_CHARS
                .replace_all(trimmed, "")
                .into_owned(),
        )
    }
}

/// Validate username format and length
pub fn validate_username(username: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(username);

    let len = sanitized.chars().count();
    if len < limits::USERNAME_MIN {
        return Err(ValidationError::TooShort {
            field: "username",
            min: limits::USERNAME_MIN,
            actual: len,
        });
    }
    if len > limits::USERNAME_MAX {
        return Err(ValidationError::TooLong {
            field: "username",
            max: limits::USERNAME_MAX,
            actual: len,
        });
    }

    if !patterns::USERNAME.is_match(&sanitized) {
        return Err(ValidationError::InvalidFormat { field: "username" });
    }

    Ok(sanitized.into_owned())
}

/// Validate password strength
pub fn validate_password(password: &str) -> ValidationResult<()> {
    let len = password.chars().count();

    if len < limits::PASSWORD_MIN {
        return Err(ValidationError::TooShort {
            field: "password",
            min: limits::PASSWORD_MIN,
            actual: len,
        });
    }

    if len > limits::PASSWORD_MAX {
        return Err(ValidationError::TooLong {
            field: "password",
            max: limits::PASSWORD_MAX,
            actual: len,
        });
    }

    // Reject common weak passwords (exact match, case-insensitive).
    // Based on top entries from public breach datasets (NCSC top-100k, HIBP Pwned
    // Passwords top-1000). For full HIBP k-anonymity API integration, configure an
    // external password check service per NIST SP 800-63B guidance.
    let lowercase = password.to_lowercase();
    if COMMON_PASSWORDS.contains(&lowercase.as_str()) {
        return Err(ValidationError::InvalidValue(
            "Password is too common. Please choose a stronger password.",
        ));
    }

    Ok(())
}

/// Validate a login identifier that may be either a username or an email address.
pub fn validate_login_identifier(identifier: &str) -> ValidationResult<String> {
    let trimmed = identifier.trim();
    if trimmed.contains('@') {
        validate_email(trimmed)
    } else {
        validate_username(trimmed)
    }
}

/// Validate room name
pub fn validate_room_name(name: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(name);

    let len = sanitized.len();
    if len < limits::ROOM_NAME_MIN {
        return Err(ValidationError::TooShort {
            field: "room_name",
            min: limits::ROOM_NAME_MIN,
            actual: len,
        });
    }
    if len > limits::ROOM_NAME_MAX {
        return Err(ValidationError::TooLong {
            field: "room_name",
            max: limits::ROOM_NAME_MAX,
            actual: len,
        });
    }

    // Check for HTML/script injection
    if patterns::HTML_TAGS.is_match(&sanitized) {
        return Err(ValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

/// Validate room description
pub fn validate_room_description(description: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(description);

    let len = sanitized.len();
    if len > limits::ROOM_DESCRIPTION_MAX {
        return Err(ValidationError::TooLong {
            field: "room_description",
            max: limits::ROOM_DESCRIPTION_MAX,
            actual: len,
        });
    }

    // Check for HTML/script injection
    if patterns::HTML_TAGS.is_match(&sanitized) {
        return Err(ValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

/// Validate room ID format
pub fn validate_room_id(id: &str) -> ValidationResult<String> {
    validate_public_id_with_prefix(id, "room_id", "room_")
}

/// Validate externally visible entity IDs.
///
/// Public IDs carry a type prefix (`room_`, `usr_`, `med_`, etc.). Decoding
/// depends on runtime configuration, so this format check only validates the
/// shared wire syntax before the impl layer decodes with `PublicIdCodec`.
pub fn validate_nanoid_id(id: &str, field_name: &'static str) -> ValidationResult<String> {
    validate_public_id(id, field_name)
}

pub fn validate_public_id(id: &str, field_name: &'static str) -> ValidationResult<String> {
    let sanitized = sanitize_string(id);

    let len = sanitized.len();
    if len == 0 {
        return Err(ValidationError::Required(field_name));
    }
    if len > limits::ID_MAX {
        return Err(ValidationError::TooLong {
            field: field_name,
            max: limits::ID_MAX,
            actual: len,
        });
    }
    if !patterns::PUBLIC_ID.is_match(&sanitized) {
        return Err(ValidationError::InvalidFormat { field: field_name });
    }

    Ok(sanitized.into_owned())
}

fn validate_public_id_with_prefix(
    id: &str,
    field_name: &'static str,
    prefix: &'static str,
) -> ValidationResult<String> {
    let sanitized = validate_public_id(id, field_name)?;
    if !sanitized.starts_with(prefix) {
        return Err(ValidationError::InvalidFormat { field: field_name });
    }
    Ok(sanitized)
}

/// Validate media title
pub fn validate_media_title(title: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(title);

    let len = sanitized.len();
    if len > limits::MEDIA_TITLE_MAX {
        return Err(ValidationError::TooLong {
            field: "media_title",
            max: limits::MEDIA_TITLE_MAX,
            actual: len,
        });
    }

    // Check for HTML/script injection
    if patterns::HTML_TAGS.is_match(&sanitized) {
        return Err(ValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

/// Validate chat message
pub fn validate_chat_message(message: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(message);

    let len = sanitized.len();
    if len == 0 {
        return Err(ValidationError::Required("message"));
    }
    if len > limits::CHAT_MESSAGE_MAX {
        return Err(ValidationError::TooLong {
            field: "message",
            max: limits::CHAT_MESSAGE_MAX,
            actual: len,
        });
    }

    // Sanitize HTML using ammonia (allows safe subset like <b>, <i>, <em>, <strong>)
    let cleaned = ammonia::clean(&sanitized);
    if cleaned.is_empty() {
        return Err(ValidationError::Required("message"));
    }

    Ok(cleaned)
}

/// Validate URL format
pub fn validate_url(url: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(url);

    let len = sanitized.len();
    if len == 0 {
        return Err(ValidationError::Required("url"));
    }
    if len > limits::URL_MAX {
        return Err(ValidationError::TooLong {
            field: "url",
            max: limits::URL_MAX,
            actual: len,
        });
    }

    // Only allow http/https URLs to prevent javascript: and data: attacks
    if !sanitized.starts_with("http://") && !sanitized.starts_with("https://") {
        return Err(ValidationError::InvalidFormat { field: "url" });
    }

    if !patterns::URL.is_match(&sanitized) {
        return Err(ValidationError::InvalidFormat { field: "url" });
    }

    Ok(sanitized.into_owned())
}

/// Validate URL for server-side requests with option to allow private IPs
///
/// This is useful for development environments where internal services
/// may need to be accessed.
pub fn validate_url_with_options(url: &str, allow_private_ips: bool) -> ValidationResult<String> {
    if allow_private_ips {
        let sanitized = sanitize_string(url);
        let len = sanitized.len();

        if len == 0 {
            return Err(ValidationError::Required("url"));
        }
        if len > limits::URL_MAX {
            return Err(ValidationError::TooLong {
                field: "url",
                max: limits::URL_MAX,
                actual: len,
            });
        }

        if !sanitized.starts_with("http://") && !sanitized.starts_with("https://") {
            return Err(ValidationError::InvalidFormat { field: "url" });
        }

        if !patterns::URL.is_match(&sanitized) {
            return Err(ValidationError::InvalidFormat { field: "url" });
        }

        Ok(sanitized.into_owned())
    } else {
        validate_url(url)
    }
}

/// Validate email format
///
/// Performs structural validation following RFC 5321 constraints:
/// - Total length max 254 characters
/// - Local part max 64 characters
/// - Local part: alphanumeric, dots, hyphens, underscores, plus signs
/// - No leading/trailing dots or consecutive dots in local part
/// - Domain must contain at least one dot
/// - Domain labels: alphanumeric and hyphens, no leading/trailing hyphens
/// - TLD must be at least 2 characters and alphabetic
pub fn validate_email(email: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(email);

    let len = sanitized.len();
    if len == 0 {
        return Err(ValidationError::Required("email"));
    }
    if len > limits::EMAIL_MAX {
        return Err(ValidationError::TooLong {
            field: "email",
            max: limits::EMAIL_MAX,
            actual: len,
        });
    }

    // Split on '@' -- must have exactly one
    let parts: Vec<&str> = sanitized.splitn(3, '@').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(ValidationError::InvalidFormat { field: "email" });
    }

    let local = parts[0];
    let domain = parts[1];

    // RFC 5321: local part max 64 characters
    if local.len() > 64 {
        return Err(ValidationError::TooLong {
            field: "email local part",
            max: 64,
            actual: local.len(),
        });
    }

    // Local part character validation: alphanumeric, dots, hyphens, underscores, plus
    if !local
        .chars()
        .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '+')
    {
        return Err(ValidationError::InvalidFormat { field: "email" });
    }

    // No leading/trailing dots in local part
    if local.starts_with('.') || local.ends_with('.') {
        return Err(ValidationError::InvalidFormat { field: "email" });
    }

    // No consecutive dots in local part
    if local.contains("..") {
        return Err(ValidationError::InvalidFormat { field: "email" });
    }

    // Domain must contain at least one dot
    if !domain.contains('.') {
        return Err(ValidationError::InvalidFormat { field: "email" });
    }

    // Domain must not start/end with dot or hyphen
    if domain.starts_with('.')
        || domain.ends_with('.')
        || domain.starts_with('-')
        || domain.ends_with('-')
    {
        return Err(ValidationError::InvalidFormat { field: "email" });
    }

    // Validate each domain label
    let labels: Vec<&str> = domain.split('.').collect();
    for label in &labels {
        if label.is_empty() {
            return Err(ValidationError::InvalidFormat { field: "email" });
        }
        if label.len() > 63 {
            return Err(ValidationError::InvalidFormat { field: "email" });
        }
        if !label.chars().all(|c| c.is_alphanumeric() || c == '-') {
            return Err(ValidationError::InvalidFormat { field: "email" });
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(ValidationError::InvalidFormat { field: "email" });
        }
    }

    // TLD must be at least 2 characters and alphabetic
    if let Some(tld) = labels.last() {
        if tld.len() < 2 {
            return Err(ValidationError::InvalidFormat { field: "email" });
        }
        if !tld.chars().all(char::is_alphabetic) {
            return Err(ValidationError::InvalidFormat { field: "email" });
        }
    }

    Ok(sanitized.to_lowercase())
}

/// Validate playback position (in seconds)
pub fn validate_playback_position(position: f64) -> ValidationResult<f64> {
    if position.is_nan() || position.is_infinite() {
        return Err(ValidationError::InvalidValue(
            "Position must be a finite number",
        ));
    }
    if position < 0.0 {
        return Err(ValidationError::InvalidValue("Position cannot be negative"));
    }
    // Max 24 hours in seconds - reasonable upper limit
    if position > 86400.0 {
        return Err(ValidationError::InvalidValue(
            "Position exceeds maximum (24 hours)",
        ));
    }
    Ok(position)
}

/// Validate playback speed
pub fn validate_playback_speed(speed: f64) -> ValidationResult<f64> {
    if speed.is_nan() || speed.is_infinite() {
        return Err(ValidationError::InvalidValue(
            "Speed must be a finite number",
        ));
    }
    // Reasonable range: 0.25x to 4x
    if !(0.25..=4.0).contains(&speed) {
        return Err(ValidationError::InvalidValue(
            "Speed must be between 0.25 and 4.0",
        ));
    }
    Ok(speed)
}

// Pagination Validation

/// Default page number when not specified
pub const DEFAULT_PAGE: i32 = 1;

/// Default page size when not specified
pub const DEFAULT_PAGE_SIZE: i32 = 20;

/// Maximum allowed page size across all endpoints
///
/// This prevents excessive memory usage and database load from large queries.
/// Set to 200 as a reasonable upper bound for list endpoints.
pub const MAX_PAGE_SIZE: i32 = 200;

/// Maximum allowed page number to prevent deep pagination `DoS`
///
/// Beyond this limit, the database must scan too many rows, causing performance issues.
/// Most users don't navigate beyond page 1000 legitimately.
pub const MAX_PAGE: i32 = 10000;

/// Validate and normalize a page number
///
/// - Converts `None` to `DEFAULT_PAGE`
/// - Clamps values to the range `1..=MAX_PAGE`
/// - Ensures page numbers are positive
///
/// # Examples
/// ```
/// use synctv_api::http::validation::{validate_page, MAX_PAGE};
///
/// assert_eq!(validate_page(None), 1);
/// assert_eq!(validate_page(Some(0)), 1); // Minimum is 1
/// assert_eq!(validate_page(Some(5)), 5);
/// assert_eq!(validate_page(Some(100000)), MAX_PAGE); // Clamped to max
/// ```
#[must_use]
pub fn validate_page(page: Option<i32>) -> i32 {
    page.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE)
}

/// Validate and normalize a page size
///
/// - Converts `None` to `DEFAULT_PAGE_SIZE`
/// - Clamps values to the range `1..=MAX_PAGE_SIZE`
/// - Ensures page sizes are positive
///
/// # Examples
/// ```
/// use synctv_api::http::validation::{validate_page_size, MAX_PAGE_SIZE};
///
/// assert_eq!(validate_page_size(None), 20);
/// assert_eq!(validate_page_size(Some(0)), 1); // Minimum is 1
/// assert_eq!(validate_page_size(Some(50)), 50);
/// assert_eq!(validate_page_size(Some(1000)), MAX_PAGE_SIZE); // Clamped to max
/// ```
#[must_use]
pub fn validate_page_size(page_size: Option<i32>) -> i32 {
    page_size
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE)
}

/// Validate both page and `page_size`, returning normalized values
///
/// This is a convenience function for endpoints that need both values validated.
///
/// # Examples
/// ```
/// use synctv_api::http::validation::validate_pagination;
///
/// let (page, page_size) = validate_pagination(Some(2), Some(50));
/// assert_eq!(page, 2);
/// assert_eq!(page_size, 50);
///
/// let (page, page_size) = validate_pagination(None, None);
/// assert_eq!(page, 1); // DEFAULT_PAGE
/// assert_eq!(page_size, 20); // DEFAULT_PAGE_SIZE
/// ```
#[must_use]
pub fn validate_pagination(page: Option<i32>, page_size: Option<i32>) -> (i32, i32) {
    (validate_page(page), validate_page_size(page_size))
}

/// Validate generic ID (`user_id`, `media_id`, etc.)
pub fn validate_id(id: &str, field_name: &'static str) -> ValidationResult<String> {
    let sanitized = sanitize_string(id);

    let len = sanitized.len();
    if len == 0 {
        return Err(ValidationError::Required(field_name));
    }
    if len > limits::ID_MAX {
        return Err(ValidationError::TooLong {
            field: field_name,
            max: limits::ID_MAX,
            actual: len,
        });
    }

    if !sanitized
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ValidationError::InvalidFormat { field: field_name });
    }

    Ok(sanitized.into_owned())
}

/// Validate `OAuth2` redirect URL (optional field)
///
/// Supports:
/// - HTTP/HTTPS URLs for web applications (e.g., `https://example.com/callback`)
/// - Custom schemes for native/mobile apps (e.g., `mysynctv://oauth2/callback`)
///
/// This is a lenient validation that only checks length and basic format.
/// The actual URL format validation is done by the `OAuth2` provider.
pub fn validate_oauth2_redirect_url(url: Option<&str>) -> ValidationResult<Option<String>> {
    let Some(url) = url else {
        return Ok(None);
    };

    let sanitized = sanitize_string(url);

    // Empty string is treated as None
    if sanitized.is_empty() {
        return Ok(None);
    }

    let len = sanitized.len();
    if len > limits::OAUTH2_REDIRECT_URL_MAX {
        return Err(ValidationError::TooLong {
            field: "redirect_url",
            max: limits::OAUTH2_REDIRECT_URL_MAX,
            actual: len,
        });
    }

    // Reject dangerous protocols first (javascript:, data:, ftp:, file:, etc.)
    // These should never be accepted as redirect_uri
    let lower = sanitized.to_lowercase();
    if lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("vbscript:")
        || lower.starts_with("file:")
        || lower.starts_with("ftp:")
        || lower.starts_with("mailto:")
    {
        return Err(ValidationError::InvalidFormat {
            field: "redirect_url (dangerous protocol not allowed)",
        });
    }

    // Allow http/https URLs for web applications
    if sanitized.starts_with("http://") || sanitized.starts_with("https://") {
        return Ok(Some(sanitized.into_owned()));
    }

    // Allow custom schemes for native/mobile apps (e.g., mysynctv://callback)
    // Format: scheme://path (must contain ://)
    // Scheme requirements:
    // - Starts with a letter or digit
    // - Contains only letters, digits, hyphens, plus signs, or dots
    // - At least 2 characters
    if let Some(pos) = sanitized.find("://") {
        let scheme = &sanitized[..pos];

        // Validate scheme format
        if scheme.len() < 2 {
            return Err(ValidationError::InvalidFormat {
                field: "redirect_url (custom scheme too short)",
            });
        }

        // Scheme must start with a letter (not digit) and contain only safe characters
        if !scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        {
            return Err(ValidationError::InvalidFormat {
                field: "redirect_url (custom scheme must start with a letter)",
            });
        }

        if !scheme
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '+' || c == '.')
        {
            return Err(ValidationError::InvalidFormat {
                field: "redirect_url (invalid characters in custom scheme)",
            });
        }

        // Path after :// must exist
        if pos + 3 >= sanitized.len() {
            return Err(ValidationError::InvalidFormat {
                field: "redirect_url (custom scheme missing path)",
            });
        }

        return Ok(Some(sanitized.into_owned()));
    }

    // Reject anything else
    Err(ValidationError::InvalidFormat {
        field: "redirect_url",
    })
}

/// Validate `OAuth2` provider user ID (optional field)
///
/// This is a lenient validation that only checks length if the ID is provided.
pub fn validate_oauth2_provider_user_id(id: Option<&str>) -> ValidationResult<Option<String>> {
    let Some(id) = id else {
        return Ok(None);
    };

    let sanitized = sanitize_string(id);

    // Empty string is treated as None
    if sanitized.is_empty() {
        return Ok(None);
    }

    let len = sanitized.len();
    if len > limits::OAUTH2_PROVIDER_USER_ID_MAX {
        return Err(ValidationError::TooLong {
            field: "provider_user_id",
            max: limits::OAUTH2_PROVIDER_USER_ID_MAX,
            actual: len,
        });
    }

    Ok(Some(sanitized.into_owned()))
}

/// Validate `OAuth2` state token (required for CSRF protection)
///
/// State tokens are generated using the shared ID generator with 32 characters from the
/// shared base62 alphabet: A-Za-z0-9
///
/// This validation ensures:
/// - State is not empty
/// - State has exactly the expected length
/// - State only contains valid characters
pub fn validate_oauth2_state(state: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(state);

    if sanitized.is_empty() {
        return Err(ValidationError::Required("state"));
    }

    if !synctv_common::id::is_valid_with_len(&sanitized, limits::OAUTH2_STATE_LENGTH) {
        return Err(ValidationError::InvalidFormat { field: "state" });
    }

    Ok(sanitized.into_owned())
}

/// Validate `OAuth2` authorization code (required)
///
/// Authorization codes are provider-specific but should be reasonably
/// sized alphanumeric strings.
pub fn validate_oauth2_code(code: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(code);

    if sanitized.is_empty() {
        return Err(ValidationError::Required("code"));
    }

    let len = sanitized.len();
    if len > limits::OAUTH2_CODE_MAX {
        return Err(ValidationError::TooLong {
            field: "code",
            max: limits::OAUTH2_CODE_MAX,
            actual: len,
        });
    }

    // Authorization codes should be alphanumeric (may include - and _)
    if !sanitized
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '+')
    {
        return Err(ValidationError::InvalidFormat { field: "code" });
    }

    Ok(sanitized.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_string() {
        assert_eq!(sanitize_string("hello"), "hello");
        assert_eq!(sanitize_string("  hello  "), "hello");
        assert_eq!(sanitize_string("hello\x00world"), "helloworld");
        assert_eq!(sanitize_string("hello\x1Bworld"), "helloworld");
    }

    #[test]
    fn test_parse_proto_enum_value_accepts_known_values_and_defaults() {
        let expected = [
            proto_enum_option(0, "unspecified"),
            proto_enum_option(1, "asc"),
            proto_enum_option(2, "desc"),
        ];

        assert_eq!(
            parse_proto_enum_value("sort_direction", Some(1), 2, &expected).unwrap(),
            1
        );
        assert_eq!(
            parse_proto_enum_value("sort_direction", None, 2, &expected).unwrap(),
            2
        );
    }

    #[test]
    fn test_parse_proto_enum_value_rejects_unknown_values_with_readable_error() {
        let expected = [
            proto_enum_option(0, "unspecified"),
            proto_enum_option(1, "asc"),
            proto_enum_option(2, "desc"),
        ];

        let err = parse_proto_enum_value("sort_direction", Some(99), 2, &expected).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            err.message,
            "Invalid sort_direction '99'. Expected one of: 0 (unspecified), 1 (asc), 2 (desc)"
        );
    }

    #[test]
    fn test_parse_proto_enum_filter_maps_unspecified_to_none() {
        let expected = [
            proto_enum_option(0, "unspecified"),
            proto_enum_option(1, "active"),
            proto_enum_option(2, "closed"),
        ];

        assert_eq!(
            parse_proto_enum_filter("status filter", Some(0), 0, &expected).unwrap(),
            None
        );
        assert_eq!(
            parse_proto_enum_filter("status filter", Some(2), 0, &expected).unwrap(),
            Some(2)
        );
        assert_eq!(
            parse_proto_enum_filter("status filter", None, 0, &expected).unwrap(),
            None
        );
    }

    #[test]
    fn test_validate_username() {
        assert!(validate_username("user123").is_ok());
        assert!(validate_username("user_name").is_ok());
        assert!(validate_username("user-name").is_ok());
        assert!(validate_username("josé").is_ok()); // Unicode letters
        assert!(validate_username("a").is_err()); // Too short
        assert!(validate_username("ab").is_err()); // Still too short (min=3)
        assert!(validate_username(&"a".repeat(limits::USERNAME_MAX + 1)).is_err()); // Too long
        assert!(validate_username("user@name").is_err()); // Invalid character
    }

    #[test]
    fn test_validate_password() {
        assert!(validate_password("MySecure123!").is_ok()); // Good password
        assert!(validate_password("qwerty12345").is_ok()); // Not exact match to any common password
        assert!(validate_password("short").is_err()); // Too short
        assert!(validate_password(&"a".repeat(limits::PASSWORD_MAX + 1)).is_err()); // Too long
                                                                                    // Exact matches to common weak passwords should be rejected
        assert!(validate_password("password").is_err());
        assert!(validate_password("12345678").is_err()); // In expanded common password list
        assert!(validate_password("password123").is_err()); // In expanded common password list
        assert!(validate_password("admin123").is_ok()); // Not exact match to any entry
        assert!(validate_password("passw0rd").is_err()); // Leet-speak variant in list
        assert!(validate_password("Passw0rd").is_err()); // Case-insensitive match
    }

    #[test]
    fn test_validate_login_identifier() {
        assert_eq!(
            validate_login_identifier(" user@example.com ").unwrap(),
            "user@example.com"
        );
        assert_eq!(validate_login_identifier("user_name").unwrap(), "user_name");
        assert!(validate_login_identifier("bad email@").is_err());
    }

    #[test]
    fn test_validate_room_name() {
        assert!(validate_room_name("My Room").is_ok());
        assert!(validate_room_name("").is_err()); // Too short (empty after trim)
        assert!(validate_room_name("   ").is_err()); // Too short (whitespace-only)
        assert!(validate_room_name(&"a".repeat(limits::ROOM_NAME_MAX + 1)).is_err()); // Too long
        assert!(validate_room_name("<script>alert('xss')</script>").is_err()); // XSS attempt
    }

    #[test]
    fn test_validate_room_id() {
        assert!(validate_room_id("room_1").is_ok());
        assert!(validate_room_id("room_123").is_ok());
        assert!(validate_room_id("room_AbC123xYz890").is_ok());
        assert!(validate_room_id("AbC123xYz890").is_err());
        assert!(validate_room_id("room1234_abx").is_err());
        assert!(validate_room_id("room@123").is_err()); // Invalid character
        assert!(validate_room_id("").is_err()); // Empty
    }

    #[test]
    fn test_validate_chat_message() {
        assert!(validate_chat_message("Hello world").is_ok());
        assert!(validate_chat_message("").is_err()); // Empty
        assert!(validate_chat_message(&"a".repeat(5001)).is_err()); // Too long
                                                                    // Safe HTML (like <b>) is preserved by ammonia
        let result = validate_chat_message("<b>Hello</b>").unwrap();
        assert_eq!(result, "<b>Hello</b>");
        // Dangerous HTML (like <script>) is stripped
        let result = validate_chat_message("<script>alert('xss')</script>Hello").unwrap();
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_validate_url() {
        assert!(validate_url("https://example.com").is_ok());
        assert!(validate_url("http://example.com/path").is_ok());
        assert!(validate_url("ftp://example.com").is_err()); // Not http/https
        assert!(validate_url("javascript:alert(1)").is_err()); // Security risk
        assert!(validate_url("").is_err()); // Empty
    }

    #[test]
    fn test_validate_url_format_only() {
        // SSRF protection is now at the DNS resolver level, not in validate_url.
        // validate_url only checks URL format (scheme, structure).

        // Valid HTTP/HTTPS URLs pass format validation
        assert!(validate_url("https://example.com/api").is_ok());
        assert!(validate_url("https://api.github.com/users").is_ok());
        assert!(validate_url("http://localhost/admin").is_ok());
        assert!(validate_url("http://127.0.0.1/admin").is_ok());
        assert!(validate_url("http://192.168.1.1/internal").is_ok());

        // Non-HTTP schemes are still rejected at format level
        assert!(validate_url("ftp://example.com").is_err());
        assert!(validate_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn test_validate_url_with_options() {
        // Both modes should validate URL format
        assert!(validate_url_with_options("http://localhost/admin", true).is_ok());
        assert!(validate_url_with_options("http://192.168.1.1/internal", true).is_ok());
        assert!(validate_url_with_options("http://localhost/admin", false).is_ok());

        // Both modes should still validate URL format
        assert!(validate_url_with_options("not-a-url", true).is_err());
        assert!(validate_url_with_options("ftp://example.com", true).is_err());
    }

    #[test]
    fn test_validate_email() {
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("USER@EXAMPLE.COM").is_ok()); // Should be lowercased
        assert_eq!(
            validate_email("USER@EXAMPLE.COM").unwrap(),
            "user@example.com"
        );
        assert!(validate_email("invalid-email").is_err());
        assert!(validate_email("").is_err());
    }

    #[test]
    fn test_validate_email_rejects_single_char_tld() {
        // TLD must be at least 2 characters
        assert!(validate_email("a@b.c").is_err());
        assert!(validate_email("user@domain.x").is_err());
    }

    #[test]
    fn test_validate_email_rejects_long_emails() {
        // RFC 5321: total email length max 254 chars
        let long_local = "a".repeat(64);
        let long_email = format!("{}@{}.com", long_local, "b".repeat(254 - 64 - 5));
        assert!(validate_email(&long_email).is_err());
    }

    #[test]
    fn test_validate_email_rejects_long_local_part() {
        // RFC 5321: local part max 64 chars
        let long_local = "a".repeat(65);
        let email = format!("{long_local}@example.com");
        assert!(validate_email(&email).is_err());
    }

    #[test]
    fn test_validate_email_accepts_valid_emails() {
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("user.name@example.com").is_ok());
        assert!(validate_email("user+tag@example.co.uk").is_ok());
        assert!(validate_email("user123@sub.domain.com").is_ok());
        assert!(validate_email("a@example.com").is_ok()); // Single char local is fine
    }

    #[test]
    fn test_validate_email_rejects_invalid_patterns() {
        assert!(validate_email("@example.com").is_err()); // No local part
        assert!(validate_email("user@").is_err()); // No domain
        assert!(validate_email("user@.com").is_err()); // Domain starts with dot
        assert!(validate_email("user@domain").is_err()); // No TLD dot
        assert!(validate_email("user@@example.com").is_err()); // Double @
        assert!(validate_email("user@exam ple.com").is_err()); // Space in domain
        assert!(validate_email("us er@example.com").is_err()); // Space in local
    }

    #[test]
    fn test_validate_email_tld_must_be_alphabetic() {
        // TLD should be alphabetic, not numeric
        assert!(validate_email("user@example.123").is_err());
        assert!(validate_email("user@example.c0m").is_err());
    }

    #[test]
    fn test_validate_email_rejects_consecutive_dots() {
        assert!(validate_email("user..name@example.com").is_err());
        assert!(validate_email("user@example..com").is_err());
    }

    #[test]
    fn test_validate_email_rejects_leading_trailing_dots() {
        assert!(validate_email(".user@example.com").is_err());
        assert!(validate_email("user.@example.com").is_err());
    }

    #[test]
    fn test_validate_playback_position() {
        assert!(validate_playback_position(0.0).is_ok());
        assert!(validate_playback_position(100.5).is_ok());
        assert!(validate_playback_position(-1.0).is_err()); // Negative
        assert!(validate_playback_position(f64::NAN).is_err()); // NaN
        assert!(validate_playback_position(f64::INFINITY).is_err()); // Infinity
        assert!(validate_playback_position(100_000.0).is_err()); // Too large (> 24h)
    }

    #[test]
    fn test_validate_playback_speed() {
        assert!(validate_playback_speed(1.0).is_ok());
        assert!(validate_playback_speed(0.25).is_ok()); // Min
        assert!(validate_playback_speed(4.0).is_ok()); // Max
        assert!(validate_playback_speed(0.1).is_err()); // Too slow
        assert!(validate_playback_speed(5.0).is_err()); // Too fast
    }

    #[test]
    fn test_validate_page_with_none() {
        // None should return default page
        assert_eq!(validate_page(None), DEFAULT_PAGE);
        assert_eq!(validate_page(None), 1);
    }

    #[test]
    fn test_validate_page_with_zero() {
        // Zero should be clamped to minimum (1)
        assert_eq!(validate_page(Some(0)), 1);
    }

    #[test]
    fn test_validate_page_with_negative() {
        // Negative should be clamped to minimum (1)
        assert_eq!(validate_page(Some(-1)), 1);
        assert_eq!(validate_page(Some(-100)), 1);
    }

    #[test]
    fn test_validate_page_with_valid_values() {
        // Valid values should pass through
        assert_eq!(validate_page(Some(1)), 1);
        assert_eq!(validate_page(Some(5)), 5);
        assert_eq!(validate_page(Some(100)), 100);
        assert_eq!(validate_page(Some(1000)), 1000);
    }

    #[test]
    fn test_validate_page_with_excessive_values() {
        // Values over MAX_PAGE should be clamped
        assert_eq!(validate_page(Some(MAX_PAGE + 1)), MAX_PAGE);
        assert_eq!(validate_page(Some(100_000)), MAX_PAGE);
        assert_eq!(validate_page(Some(i32::MAX)), MAX_PAGE);
    }

    #[test]
    fn test_validate_page_size_with_none() {
        // None should return default page size
        assert_eq!(validate_page_size(None), DEFAULT_PAGE_SIZE);
        assert_eq!(validate_page_size(None), 20);
    }

    #[test]
    fn test_validate_page_size_with_zero() {
        // Zero should be clamped to minimum (1)
        assert_eq!(validate_page_size(Some(0)), 1);
    }

    #[test]
    fn test_validate_page_size_with_negative() {
        // Negative should be clamped to minimum (1)
        assert_eq!(validate_page_size(Some(-1)), 1);
        assert_eq!(validate_page_size(Some(-100)), 1);
    }

    #[test]
    fn test_validate_page_size_with_valid_values() {
        // Valid values should pass through
        assert_eq!(validate_page_size(Some(1)), 1);
        assert_eq!(validate_page_size(Some(10)), 10);
        assert_eq!(validate_page_size(Some(50)), 50);
        assert_eq!(validate_page_size(Some(100)), 100);
        assert_eq!(validate_page_size(Some(200)), 200);
    }

    #[test]
    fn test_validate_page_size_with_excessive_values() {
        // Values over MAX_PAGE_SIZE should be clamped
        assert_eq!(validate_page_size(Some(MAX_PAGE_SIZE + 1)), MAX_PAGE_SIZE);
        assert_eq!(validate_page_size(Some(500)), MAX_PAGE_SIZE);
        assert_eq!(validate_page_size(Some(1000)), MAX_PAGE_SIZE);
        assert_eq!(validate_page_size(Some(i32::MAX)), MAX_PAGE_SIZE);
    }

    #[test]
    fn test_validate_pagination_with_both_none() {
        // Both None should return defaults
        let (page, page_size) = validate_pagination(None, None);
        assert_eq!(page, DEFAULT_PAGE);
        assert_eq!(page_size, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn test_validate_pagination_with_valid_values() {
        // Valid values should pass through
        let (page, page_size) = validate_pagination(Some(5), Some(50));
        assert_eq!(page, 5);
        assert_eq!(page_size, 50);
    }

    #[test]
    fn test_validate_pagination_with_edge_cases() {
        // Zero values should be clamped to minimum
        let (page, page_size) = validate_pagination(Some(0), Some(0));
        assert_eq!(page, 1);
        assert_eq!(page_size, 1);

        // Excessive values should be clamped to maximum
        let (page, page_size) = validate_pagination(Some(100_000), Some(1000));
        assert_eq!(page, MAX_PAGE);
        assert_eq!(page_size, MAX_PAGE_SIZE);
    }

    #[test]
    fn test_validate_pagination_boundary_values() {
        // Test exact boundary values
        assert_eq!(validate_page(Some(MAX_PAGE)), MAX_PAGE);
        assert_eq!(validate_page(Some(1)), 1);

        assert_eq!(validate_page_size(Some(MAX_PAGE_SIZE)), MAX_PAGE_SIZE);
        assert_eq!(validate_page_size(Some(1)), 1);

        // Test just beyond boundaries
        assert_eq!(validate_page(Some(MAX_PAGE + 1)), MAX_PAGE);
        assert_eq!(validate_page_size(Some(MAX_PAGE_SIZE + 1)), MAX_PAGE_SIZE);
    }

    #[test]
    fn test_validate_id() {
        assert!(validate_id("provider-alpha_01", "name").is_ok());
        assert!(validate_id("provider@alpha", "name").is_err());
        assert!(validate_id("", "user_id").is_err()); // Empty
    }

    #[test]
    fn test_validate_nanoid_id() {
        assert!(validate_nanoid_id("usr_1", "user_id").is_ok());
        assert!(validate_nanoid_id("room_AbC123xYz890", "room_id").is_ok());
        assert!(validate_nanoid_id("med_A", "media_id").is_ok());
        assert!(validate_nanoid_id("AbC123xYz890", "user_id").is_err());
        assert!(validate_nanoid_id(&"A".repeat(limits::ID_MAX + 1), "user_id").is_err());
        assert!(validate_nanoid_id("usr_AbC123xYz8_0", "user_id").is_err()); // Invalid character
        assert!(validate_nanoid_id("", "user_id").is_err()); // Empty
    }

    #[test]
    fn test_validate_oauth2_redirect_url() {
        // None is valid
        assert!(validate_oauth2_redirect_url(None).is_ok());
        assert_eq!(validate_oauth2_redirect_url(None).unwrap(), None);

        // Empty string is treated as None
        assert!(validate_oauth2_redirect_url(Some("")).is_ok());
        assert_eq!(validate_oauth2_redirect_url(Some("")).unwrap(), None);

        // Whitespace-only is treated as None
        assert!(validate_oauth2_redirect_url(Some("   ")).is_ok());
        assert_eq!(validate_oauth2_redirect_url(Some("   ")).unwrap(), None);

        // Valid HTTP URL
        let result = validate_oauth2_redirect_url(Some("http://example.com/callback"));
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            Some("http://example.com/callback".to_string())
        );

        // Valid HTTPS URL
        let result = validate_oauth2_redirect_url(Some("https://example.com/callback?state=abc"));
        assert!(result.is_ok());

        // Invalid: not http/https
        assert!(validate_oauth2_redirect_url(Some("ftp://example.com")).is_err());
        assert!(validate_oauth2_redirect_url(Some("javascript:alert(1)")).is_err());
        assert!(validate_oauth2_redirect_url(Some("data:text/html,<script>")).is_err());

        // Invalid: too long
        let long_url =
            "https://example.com/".to_string() + &"a".repeat(limits::OAUTH2_REDIRECT_URL_MAX);
        assert!(validate_oauth2_redirect_url(Some(&long_url)).is_err());

        // Valid: exactly at max length
        let exact_url =
            "https://example.com/".to_string() + &"a".repeat(limits::OAUTH2_REDIRECT_URL_MAX - 20);
        assert!(validate_oauth2_redirect_url(Some(&exact_url)).is_ok());

        // Valid custom scheme for mobile app
        let result = validate_oauth2_redirect_url(Some("mysynctv://oauth2/callback"));
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            Some("mysynctv://oauth2/callback".to_string())
        );

        // Valid custom scheme with query parameters
        let result = validate_oauth2_redirect_url(Some("com.example.app://auth?param=value"));
        assert!(result.is_ok());

        // Valid custom scheme with path
        let result = validate_oauth2_redirect_url(Some("io.github.synctv://oauth2/callback"));
        assert!(result.is_ok());

        // Valid reverse-domain notation
        let result = validate_oauth2_redirect_url(Some("com.google.android.gms://oauth2/callback"));
        assert!(result.is_ok());

        // Valid custom scheme with hyphen
        let result = validate_oauth2_redirect_url(Some("my-custom-app://callback"));
        assert!(result.is_ok());

        // Valid custom scheme with plus
        let result = validate_oauth2_redirect_url(Some("app+custom://callback"));
        assert!(result.is_ok());

        // Valid custom scheme with dot
        let result = validate_oauth2_redirect_url(Some("app.custom://callback"));
        assert!(result.is_ok());

        // Invalid: custom scheme too short (1 char)
        assert!(validate_oauth2_redirect_url(Some("a://callback")).is_err());

        // Invalid: custom scheme starts with number
        assert!(validate_oauth2_redirect_url(Some("1app://callback")).is_err());

        // Invalid: custom scheme starts with special character
        assert!(validate_oauth2_redirect_url(Some("-app://callback")).is_err());
        assert!(validate_oauth2_redirect_url(Some("_app://callback")).is_err());

        // Invalid: custom scheme contains invalid characters
        assert!(validate_oauth2_redirect_url(Some("app@name://callback")).is_err());
        assert!(validate_oauth2_redirect_url(Some("app name://callback")).is_err());

        // Invalid: custom scheme missing path after ://
        assert!(validate_oauth2_redirect_url(Some("mysynctv://")).is_err());

        // Invalid: missing :// separator
        assert!(validate_oauth2_redirect_url(Some("mysynctv/callback")).is_err());

        // Invalid: still rejected dangerous protocols even with custom scheme
        assert!(validate_oauth2_redirect_url(Some("javascript://callback")).is_err());
        assert!(validate_oauth2_redirect_url(Some("data://callback")).is_err());
    }

    #[test]
    fn test_validate_oauth2_provider_user_id() {
        // None is valid
        assert!(validate_oauth2_provider_user_id(None).is_ok());
        assert_eq!(validate_oauth2_provider_user_id(None).unwrap(), None);

        // Empty string is treated as None
        assert!(validate_oauth2_provider_user_id(Some("")).is_ok());
        assert_eq!(validate_oauth2_provider_user_id(Some("")).unwrap(), None);

        // Whitespace-only is treated as None
        assert!(validate_oauth2_provider_user_id(Some("   ")).is_ok());
        assert_eq!(validate_oauth2_provider_user_id(Some("   ")).unwrap(), None);

        // Valid provider user IDs
        assert!(validate_oauth2_provider_user_id(Some("12345")).is_ok());
        assert!(validate_oauth2_provider_user_id(Some("user@example.com")).is_ok());
        assert!(validate_oauth2_provider_user_id(Some("github-user-123")).is_ok());

        // Invalid: too long
        let long_id = "a".repeat(limits::OAUTH2_PROVIDER_USER_ID_MAX + 1);
        assert!(validate_oauth2_provider_user_id(Some(&long_id)).is_err());

        // Valid: exactly at max length
        let exact_id = "a".repeat(limits::OAUTH2_PROVIDER_USER_ID_MAX);
        assert!(validate_oauth2_provider_user_id(Some(&exact_id)).is_ok());
    }

    #[test]
    fn test_validate_oauth2_state() {
        // Valid state: 32 chars, base62 alphabet
        let valid_state = "AbCdEfGh1234567890ZaBcDeFgHiJkLQ";
        assert_eq!(valid_state.len(), limits::OAUTH2_STATE_LENGTH);
        assert!(validate_oauth2_state(valid_state).is_ok());

        // Another valid state with different characters
        let valid_state2 = "abcdefghijklmnopqrstuvwxyz123456";
        assert_eq!(valid_state2.len(), limits::OAUTH2_STATE_LENGTH);
        assert!(validate_oauth2_state(valid_state2).is_ok());

        // Empty state is invalid
        assert!(validate_oauth2_state("").is_err());

        // Whitespace-only is invalid after trim
        assert!(validate_oauth2_state("   ").is_err());

        // Too short
        assert!(validate_oauth2_state("abc123").is_err());

        // Too long
        let long_state = "a".repeat(limits::OAUTH2_STATE_LENGTH + 1);
        assert!(validate_oauth2_state(&long_state).is_err());

        // Invalid characters
        assert!(validate_oauth2_state("AbCdEfGh1234567890aBcDeFgHiJkLm!@").is_err()); // Special chars
        assert!(validate_oauth2_state("AbCdEfGh1234567890 aBcDeFgHiJkLmN").is_err()); // Space
        assert!(validate_oauth2_state("AbCdEfGh1234567890-aBcDeFgHiJkLmN").is_err()); // Hyphen
        assert!(validate_oauth2_state("AbCdEfGh1234567890_aBcDeFgHiJkLmN").is_err()); // Underscore

        // State with control characters should be sanitized and fail
        assert!(validate_oauth2_state("AbCdEfGh1234567890\x00aBcDeFgHiJ").is_err());
    }

    #[test]
    fn test_validate_oauth2_state_csrf_protection() {
        // This test documents the CSRF protection requirements for OAuth2 state

        // 1. State must be exactly 32 characters (prevents length manipulation)
        let short_state = "abc";
        assert!(validate_oauth2_state(short_state).is_err());

        let exact_state = "abcdefghijklmnopqrstuvwxyz123456";
        assert_eq!(exact_state.len(), 32);
        assert!(validate_oauth2_state(exact_state).is_ok());

        // 2. State must only contain base62 characters (prevents injection)
        let injection_attempt = "abc;DROP TABLE users;123456789012345678";
        assert!(validate_oauth2_state(injection_attempt).is_err());

        // 3. State must not contain unicode (prevents encoding attacks)
        let unicode_attempt = "abcRésumé12345678901234567890ab";
        assert!(validate_oauth2_state(unicode_attempt).is_err());
    }

    #[test]
    fn test_validate_oauth2_code() {
        // Valid authorization codes
        assert!(validate_oauth2_code("abc123").is_ok());
        assert!(validate_oauth2_code("AbCdEfGh12345678").is_ok());
        assert!(validate_oauth2_code("code_with_underscores").is_ok());
        assert!(validate_oauth2_code("code-with-hyphens-123").is_ok());
        assert!(validate_oauth2_code("code.with.dots").is_ok());
        assert!(validate_oauth2_code("code+with+plus").is_ok());

        // Empty code is invalid
        assert!(validate_oauth2_code("").is_err());

        // Whitespace-only is invalid after trim
        assert!(validate_oauth2_code("   ").is_err());

        // Too long
        let long_code = "a".repeat(limits::OAUTH2_CODE_MAX + 1);
        assert!(validate_oauth2_code(&long_code).is_err());

        // Invalid characters
        assert!(validate_oauth2_code("code with spaces").is_err());
        assert!(validate_oauth2_code("code!special").is_err());
        assert!(validate_oauth2_code("code@symbol").is_err());

        // Exactly at max length is valid
        let exact_code = "a".repeat(limits::OAUTH2_CODE_MAX);
        assert!(validate_oauth2_code(&exact_code).is_ok());
    }

    #[test]
    fn test_validate_media_title() {
        // Valid titles
        assert!(validate_media_title("My Video").is_ok());
        assert!(validate_media_title("Movie 2024").is_ok());
        assert!(validate_media_title("Café Video").is_ok()); // Unicode characters

        // Empty title is allowed (defaults will be used)
        assert!(validate_media_title("").is_ok());

        // Exactly at max length (500) should succeed
        let exact_title = "a".repeat(limits::MEDIA_TITLE_MAX);
        assert!(validate_media_title(&exact_title).is_ok());

        // Over max length (501) should fail
        let long_title = "a".repeat(limits::MEDIA_TITLE_MAX + 1);
        let result = validate_media_title(&long_title);
        assert!(result.is_err());
        match result {
            Err(ValidationError::TooLong { field, max, actual }) => {
                assert_eq!(field, "media_title");
                assert_eq!(max, limits::MEDIA_TITLE_MAX);
                assert_eq!(actual, limits::MEDIA_TITLE_MAX + 1);
            }
            _ => panic!("Expected TooLong error"),
        }

        // XSS attempt should fail
        let xss_result = validate_media_title("<script>alert('xss')</script>");
        assert!(xss_result.is_err());
        match xss_result {
            Err(ValidationError::SecurityRisk) => {}
            _ => panic!("Expected SecurityRisk error for HTML tags"),
        }

        // Control characters should be stripped
        let sanitized = validate_media_title("hello\x00world").unwrap();
        assert_eq!(sanitized, "helloworld");
    }
}
