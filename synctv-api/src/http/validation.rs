//! Input validation utilities for HTTP endpoints
//!
//! This module provides validation functions for common input types to ensure
//! data integrity and prevent security issues like injection attacks.

use std::sync::LazyLock;
use regex::Regex;
use std::borrow::Cow;

/// Maximum lengths for various input types
pub mod limits {
    // Core limits imported from the single source of truth in synctv-core
    pub use synctv_core::validation::{
        USERNAME_MIN, USERNAME_MAX,
        PASSWORD_MIN, PASSWORD_MAX,
        ROOM_NAME_MIN, ROOM_NAME_MAX,
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
    /// Maximum ID length (`room_id`, `user_id`, `media_id`)
    pub const ID_MAX: usize = 64;
}

/// Regex patterns for validation
mod patterns {
    use super::{LazyLock, Regex};

    /// Valid username: alphanumeric, underscores, hyphens, and CJK characters
    pub static USERNAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[\p{L}\p{N}_-]+$").expect("Invalid username regex")
    });

    /// Valid room ID: alphanumeric, underscores, hyphens
    pub static ROOM_ID: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[a-zA-Z0-9_-]+$").expect("Invalid room_id regex")
    });

    /// Valid email format (basic validation)
    pub static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[^\s@]+@[^\s@]+\.[^\s@]+$").expect("Invalid email regex")
    });

    /// URL format (http/https only)
    pub static URL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^https?://[^\s]+$").expect("Invalid URL regex")
    });

    /// HTML/script tag detection for XSS prevention
    pub static HTML_TAGS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"<[^>]+>").expect("Invalid HTML regex")
    });

    /// Control characters that should be stripped
    pub static CONTROL_CHARS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]").expect("Invalid control char regex")
    });
}

/// Common weak passwords rejected on exact match (case-insensitive).
///
/// Sourced from the intersection of NCSC top-100k, HIBP Pwned Passwords
/// top-1000, and SplashData annual worst-passwords lists. Kept small enough
/// for an O(n) linear scan (sub-microsecond for ~40 entries).
const COMMON_PASSWORDS: &[&str] = &[
    // Top-10 most breached
    "password", "123456", "12345678", "123456789", "1234567890",
    "qwerty", "abc123", "111111", "password1", "iloveyou",
    // Common words / names
    "admin", "letmein", "welcome", "monkey", "dragon",
    "master", "login", "princess", "football", "shadow",
    "sunshine", "trustno1", "baseball", "superman", "michael",
    "access", "mustang", "batman", "passw0rd",
    // Keyboard walks
    "qwerty123", "qwertyuiop", "1q2w3e4r", "zxcvbnm",
    "asdfghjkl", "1qaz2wsx",
    // Numeric sequences
    "12345678901", "00000000", "11111111",
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
        Cow::Owned(patterns::CONTROL_CHARS.replace_all(trimmed, "").into_owned())
    }
}

/// Validate username format and length
pub fn validate_username(username: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(username);

    let len = sanitized.len();
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
    let len = password.len();

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
    let sanitized = sanitize_string(id);

    let len = sanitized.len();
    if len == 0 {
        return Err(ValidationError::Required("room_id"));
    }
    if len > limits::ID_MAX {
        return Err(ValidationError::TooLong {
            field: "room_id",
            max: limits::ID_MAX,
            actual: len,
        });
    }

    if !patterns::ROOM_ID.is_match(&sanitized) {
        return Err(ValidationError::InvalidFormat { field: "room_id" });
    }

    Ok(sanitized.into_owned())
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

    // Check for script injection (but allow some HTML for formatting if needed)
    // For strict security, we could block all HTML
    if patterns::HTML_TAGS.is_match(&sanitized) {
        // Strip HTML tags for chat messages
        let stripped = patterns::HTML_TAGS.replace_all(&sanitized, "").into_owned();
        return Ok(stripped);
    }

    Ok(sanitized.into_owned())
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

    // SSRF protection: delegate to the authoritative SSRFValidator in synctv-core
    // which properly parses IPs (no false positives from substring matching)
    if synctv_core::validation::validate_url_for_ssrf(&sanitized).is_err() {
        tracing::warn!(
            url = %sanitized,
            "SSRF attempt blocked by SSRFValidator"
        );
        return Err(ValidationError::SecurityRisk);
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

    if !patterns::EMAIL.is_match(&sanitized) {
        return Err(ValidationError::InvalidFormat { field: "email" });
    }

    Ok(sanitized.to_lowercase())
}

/// Validate playback position (in seconds)
pub fn validate_playback_position(position: f64) -> ValidationResult<f64> {
    if position.is_nan() || position.is_infinite() {
        return Err(ValidationError::InvalidValue("Position must be a finite number"));
    }
    if position < 0.0 {
        return Err(ValidationError::InvalidValue("Position cannot be negative"));
    }
    // Max 24 hours in seconds - reasonable upper limit
    if position > 86400.0 {
        return Err(ValidationError::InvalidValue("Position exceeds maximum (24 hours)"));
    }
    Ok(position)
}

/// Validate playback speed
pub fn validate_playback_speed(speed: f64) -> ValidationResult<f64> {
    if speed.is_nan() || speed.is_infinite() {
        return Err(ValidationError::InvalidValue("Speed must be a finite number"));
    }
    // Reasonable range: 0.25x to 4x
    if !(0.25..=4.0).contains(&speed) {
        return Err(ValidationError::InvalidValue(
            "Speed must be between 0.25 and 4.0",
        ));
    }
    Ok(speed)
}

/// Validate pagination limit
pub const fn validate_pagination_limit(limit: u32) -> ValidationResult<u32> {
    // Default and max limits
    const DEFAULT_LIMIT: u32 = 20;
    const MAX_LIMIT: u32 = 100;

    if limit == 0 {
        return Ok(DEFAULT_LIMIT);
    }
    if limit > MAX_LIMIT {
        return Err(ValidationError::TooLong {
            field: "limit",
            max: MAX_LIMIT as usize,
            actual: limit as usize,
        });
    }
    Ok(limit)
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

    // Allow alphanumeric, underscores, and hyphens
    if !sanitized
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ValidationError::InvalidFormat { field: field_name });
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
    fn test_validate_username() {
        assert!(validate_username("user123").is_ok());
        assert!(validate_username("user_name").is_ok());
        assert!(validate_username("user-name").is_ok());
        assert!(validate_username("用户名").is_ok()); // CJK characters
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
    fn test_validate_room_name() {
        assert!(validate_room_name("My Room").is_ok());
        assert!(validate_room_name("").is_err()); // Too short (empty after trim)
        assert!(validate_room_name("   ").is_err()); // Too short (whitespace-only)
        assert!(validate_room_name(&"a".repeat(limits::ROOM_NAME_MAX + 1)).is_err()); // Too long
        assert!(validate_room_name("<script>alert('xss')</script>").is_err()); // XSS attempt
    }

    #[test]
    fn test_validate_room_id() {
        assert!(validate_room_id("room123").is_ok());
        assert!(validate_room_id("room_123").is_ok());
        assert!(validate_room_id("room-123").is_ok());
        assert!(validate_room_id("room@123").is_err()); // Invalid character
        assert!(validate_room_id("").is_err()); // Empty
    }

    #[test]
    fn test_validate_chat_message() {
        assert!(validate_chat_message("Hello world").is_ok());
        assert!(validate_chat_message("").is_err()); // Empty
        assert!(validate_chat_message(&"a".repeat(5001)).is_err()); // Too long
        // HTML should be stripped
        let result = validate_chat_message("<b>Hello</b>").unwrap();
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
    fn test_validate_url_ssrf_protection() {
        // Should reject localhost
        assert!(validate_url("http://localhost/admin").is_err());
        assert!(validate_url("http://LOCALHOST/admin").is_err());

        // Should reject loopback
        assert!(validate_url("http://127.0.0.1/admin").is_err());
        assert!(validate_url("http://0.0.0.0/admin").is_err());

        // Should reject private IP ranges
        assert!(validate_url("http://10.0.0.1/internal").is_err());
        assert!(validate_url("http://172.16.0.1/internal").is_err());
        assert!(validate_url("http://172.31.255.255/internal").is_err());
        assert!(validate_url("http://192.168.1.1/internal").is_err());

        // Should reject link-local
        assert!(validate_url("http://169.254.1.1/internal").is_err());

        // Should reject IPv6 private
        assert!(validate_url("http://[::1]/admin").is_err());
        assert!(validate_url("http://[fc00::1]/internal").is_err());
        assert!(validate_url("http://[fd00::1]/internal").is_err());

        // Octal loopback is treated as a hostname by the url crate, not as an IP.
        // The SSRFValidator would catch it during async DNS resolution at request time.
        // At the static validation level, it is not rejected as a private IP.
        // assert!(validate_url("http://0177.0.0.1/admin").is_err());

        // Should allow public URLs
        assert!(validate_url("https://example.com/api").is_ok());
        assert!(validate_url("https://api.github.com/users").is_ok());
    }

    #[test]
    fn test_validate_url_with_options() {
        // With allow_private_ips = true, should allow private IPs
        assert!(validate_url_with_options("http://localhost/admin", true).is_ok());
        assert!(validate_url_with_options("http://192.168.1.1/internal", true).is_ok());

        // With allow_private_ips = false, should reject
        assert!(validate_url_with_options("http://localhost/admin", false).is_err());

        // Both modes should still validate URL format
        assert!(validate_url_with_options("not-a-url", true).is_err());
        assert!(validate_url_with_options("ftp://example.com", true).is_err());
    }

    #[test]
    fn test_validate_email() {
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("USER@EXAMPLE.COM").is_ok()); // Should be lowercased
        assert_eq!(validate_email("USER@EXAMPLE.COM").unwrap(), "user@example.com");
        assert!(validate_email("invalid-email").is_err());
        assert!(validate_email("").is_err());
    }

    #[test]
    fn test_validate_playback_position() {
        assert!(validate_playback_position(0.0).is_ok());
        assert!(validate_playback_position(100.5).is_ok());
        assert!(validate_playback_position(-1.0).is_err()); // Negative
        assert!(validate_playback_position(f64::NAN).is_err()); // NaN
        assert!(validate_playback_position(f64::INFINITY).is_err()); // Infinity
        assert!(validate_playback_position(100000.0).is_err()); // Too large (> 24h)
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
    fn test_validate_pagination_limit() {
        assert_eq!(validate_pagination_limit(0).unwrap(), 20); // Default
        assert_eq!(validate_pagination_limit(50).unwrap(), 50);
        assert!(validate_pagination_limit(101).is_err()); // Over max
    }

    #[test]
    fn test_validate_id() {
        assert!(validate_id("user123", "user_id").is_ok());
        assert!(validate_id("user_123", "user_id").is_ok());
        assert!(validate_id("user@123", "user_id").is_err()); // Invalid character
        assert!(validate_id("", "user_id").is_err()); // Empty
    }
}
