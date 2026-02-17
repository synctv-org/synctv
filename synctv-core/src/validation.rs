//! Input validation using mature crates
//!
//! This module provides production-grade input validation using the `validator` crate.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

// ============================================================================
// Canonical validation limits — single source of truth for the entire codebase
// ============================================================================

/// Minimum username length
pub const USERNAME_MIN: usize = 3;
/// Maximum username length
pub const USERNAME_MAX: usize = 50;

/// Minimum user-account password length
pub const PASSWORD_MIN: usize = 8;
/// Maximum password length (prevent bcrypt DoS; bcrypt input limit is 72 bytes,
/// but we allow up to 128 for pre-hashing schemes)
pub const PASSWORD_MAX: usize = 128;

/// Minimum room password length (shorter than user password because room
/// passwords are shared secrets with lower entropy requirements)
pub const ROOM_PASSWORD_MIN: usize = 4;
/// Maximum room password length (same cap as user password)
pub const ROOM_PASSWORD_MAX: usize = 128;

/// Minimum room name length
pub const ROOM_NAME_MIN: usize = 1;
/// Maximum room name length
pub const ROOM_NAME_MAX: usize = 100;

/// Maximum room description length (must match DB constraint rooms_description_length_check)
pub const ROOM_DESCRIPTION_MAX: usize = 500;

/// Validation error
#[derive(Debug, Clone, thiserror::Error)]
pub enum ValidationError {
    #[error("Invalid {field}: {message}")]
    Field { field: String, message: String },

    #[error("Multiple validation errors: {0}")]
    Multiple(String),

    #[error("SSRF protection: {0}")]
    SSRF(String),
}

/// Validation result
pub type ValidationResult<T> = Result<T, ValidationError>;

/// Username validator
pub struct UsernameValidator {
    min_length: usize,
    max_length: usize,
}

impl Default for UsernameValidator {
    fn default() -> Self {
        Self {
            min_length: USERNAME_MIN,
            max_length: USERNAME_MAX,
        }
    }
}

impl UsernameValidator {
    #[must_use] 
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use] 
    pub const fn with_length(mut self, min: usize, max: usize) -> Self {
        self.min_length = min;
        self.max_length = max;
        self
    }

    pub fn validate(&self, username: &str) -> ValidationResult<()> {
        // Check length (use char count for Unicode safety)
        let char_count = username.chars().count();
        if char_count < self.min_length {
            return Err(ValidationError::Field {
                field: "username".to_string(),
                message: format!("must be at least {} characters", self.min_length),
            });
        }

        if char_count > self.max_length {
            return Err(ValidationError::Field {
                field: "username".to_string(),
                message: format!("must be at most {} characters", self.max_length),
            });
        }

        // Check characters (alphanumeric, underscore, hyphen)
        if !username.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
            return Err(ValidationError::Field {
                field: "username".to_string(),
                message: "can only contain letters, numbers, underscores, and hyphens".to_string(),
            });
        }

        // Cannot start with special character
        if let Some(first_char) = username.chars().next() {
            if first_char == '_' || first_char == '-' {
                return Err(ValidationError::Field {
                    field: "username".to_string(),
                    message: "cannot start with underscore or hyphen".to_string(),
                });
            }
        }

        Ok(())
    }
}

/// Password validator
pub struct PasswordValidator {
    min_length: usize,
    require_uppercase: bool,
    require_lowercase: bool,
    require_digit: bool,
    require_special_char: bool,
    /// Maximum consecutive repeated characters allowed (0 = disabled)
    max_repeated_chars: usize,
}

impl Default for PasswordValidator {
    fn default() -> Self {
        Self {
            min_length: PASSWORD_MIN,
            require_uppercase: true,
            require_lowercase: true,
            require_digit: true,
            require_special_char: false,
            max_repeated_chars: 3,
        }
    }
}

impl PasswordValidator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a `PasswordValidator` from a `PasswordComplexityConfig`.
    #[must_use]
    pub fn from_config(config: &crate::config::PasswordComplexityConfig) -> Self {
        Self {
            min_length: config.min_length,
            require_uppercase: config.require_uppercase,
            require_lowercase: config.require_lowercase,
            require_digit: config.require_digit,
            require_special_char: config.require_special,
            max_repeated_chars: config.max_repeated_chars,
        }
    }

    #[must_use]
    pub const fn with_min_length(mut self, length: usize) -> Self {
        self.min_length = length;
        self
    }

    #[must_use]
    pub const fn require_special_char(mut self, required: bool) -> Self {
        self.require_special_char = required;
        self
    }

    #[must_use]
    pub const fn with_max_repeated_chars(mut self, max: usize) -> Self {
        self.max_repeated_chars = max;
        self
    }

    /// Maximum password length to prevent bcrypt `DoS` (bcrypt input limit is 72 bytes)
    const MAX_LENGTH: usize = PASSWORD_MAX;

    pub fn validate(&self, password: &str) -> ValidationResult<()> {
        // Check length (use char count for Unicode safety)
        let char_count = password.chars().count();
        if char_count < self.min_length {
            return Err(ValidationError::Field {
                field: "password".to_string(),
                message: format!("must be at least {} characters", self.min_length),
            });
        }

        if char_count > Self::MAX_LENGTH {
            return Err(ValidationError::Field {
                field: "password".to_string(),
                message: format!("must not exceed {} characters", Self::MAX_LENGTH),
            });
        }

        // Check for uppercase
        if self.require_uppercase && !password.chars().any(char::is_uppercase) {
            return Err(ValidationError::Field {
                field: "password".to_string(),
                message: "must contain at least one uppercase letter".to_string(),
            });
        }

        // Check for lowercase
        if self.require_lowercase && !password.chars().any(char::is_lowercase) {
            return Err(ValidationError::Field {
                field: "password".to_string(),
                message: "must contain at least one lowercase letter".to_string(),
            });
        }

        // Check for digit
        if self.require_digit && !password.chars().any(|c| c.is_ascii_digit()) {
            return Err(ValidationError::Field {
                field: "password".to_string(),
                message: "must contain at least one digit".to_string(),
            });
        }

        // Check for special character
        if self.require_special_char && !password.chars().any(|c| {
            !c.is_alphanumeric()
        }) {
            return Err(ValidationError::Field {
                field: "password".to_string(),
                message: "must contain at least one special character".to_string(),
            });
        }

        // Check for consecutive repeated characters
        if self.max_repeated_chars > 0 {
            let mut prev = None;
            let mut count = 1usize;
            for ch in password.chars() {
                if prev == Some(ch) {
                    count += 1;
                    if count > self.max_repeated_chars {
                        return Err(ValidationError::Field {
                            field: "password".to_string(),
                            message: format!(
                                "must not contain more than {} consecutive repeated characters",
                                self.max_repeated_chars
                            ),
                        });
                    }
                } else {
                    count = 1;
                }
                prev = Some(ch);
            }
        }

        Ok(())
    }
}

/// Pre-compiled email validation regex
static EMAIL_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    // SAFETY: This is a compile-time constant regex literal that is known to be valid.
    regex::Regex::new(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$")
        .expect("email validation regex is a compile-time constant and always valid")
});

/// Email validator
#[derive(Default)]
pub struct EmailValidator {}


impl EmailValidator {
    #[must_use] 
    pub fn new() -> Self {
        Self::default()
    }

    pub fn validate(&self, email: &str) -> ValidationResult<()> {
        if !EMAIL_REGEX.is_match(email) {
            return Err(ValidationError::Field {
                field: "email".to_string(),
                message: "must be a valid email address".to_string(),
            });
        }

        Ok(())
    }
}

/// URL validator
#[derive(Default)]
pub struct UrlValidator {
    allow_https_only: bool,
    allowed_domains: Option<Vec<String>>,
}


impl UrlValidator {
    #[must_use] 
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use] 
    pub const fn https_only(mut self) -> Self {
        self.allow_https_only = true;
        self
    }

    #[must_use] 
    pub fn with_allowed_domains(mut self, domains: Vec<String>) -> Self {
        self.allowed_domains = Some(domains);
        self
    }

    pub fn validate(&self, url: &str) -> ValidationResult<()> {
        match url::Url::parse(url) {
            Ok(parsed) => {
                // Check HTTPS requirement
                if self.allow_https_only && parsed.scheme() != "https" {
                    return Err(ValidationError::Field {
                        field: "url".to_string(),
                        message: "must use HTTPS".to_string(),
                    });
                }

                // Check allowed domains
                if let Some(ref domains) = self.allowed_domains {
                    if let Some(host) = parsed.host_str() {
                        if !domains.iter().any(|d| host == d.as_str() || host.ends_with(&format!(".{d}"))) {
                            return Err(ValidationError::Field {
                                field: "url".to_string(),
                                message: format!("domain not in allowed list: {domains:?}"),
                            });
                        }
                    }
                }

                Ok(())
            }
            Err(_) => Err(ValidationError::Field {
                field: "url".to_string(),
                message: "must be a valid URL".to_string(),
            }),
        }
    }
}

/// Room name validator
pub struct RoomNameValidator {
    min_length: usize,
    max_length: usize,
}

impl Default for RoomNameValidator {
    fn default() -> Self {
        Self {
            min_length: 1,
            max_length: ROOM_NAME_MAX,
        }
    }
}

impl RoomNameValidator {
    #[must_use] 
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use] 
    pub const fn with_length(mut self, min: usize, max: usize) -> Self {
        self.min_length = min;
        self.max_length = max;
        self
    }

    pub fn validate(&self, name: &str) -> ValidationResult<()> {
        // Check length (use char count for Unicode safety)
        let char_count = name.chars().count();
        if char_count < self.min_length {
            return Err(ValidationError::Field {
                field: "room_name".to_string(),
                message: format!("must be at least {} characters", self.min_length),
            });
        }

        if char_count > self.max_length {
            return Err(ValidationError::Field {
                field: "room_name".to_string(),
                message: format!("must be at most {} characters", self.max_length),
            });
        }

        // Check for control characters
        if name.chars().any(char::is_control) {
            return Err(ValidationError::Field {
                field: "room_name".to_string(),
                message: "cannot contain control characters".to_string(),
            });
        }

        Ok(())
    }
}

/// Batch validator for multiple fields
pub struct Validator {
    errors: Vec<ValidationError>,
}

impl Validator {
    #[must_use] 
    pub const fn new() -> Self {
        Self {
            errors: Vec::new(),
        }
    }

    pub fn validate_field<F>(&mut self, _field: &str, result: ValidationResult<F>) -> &mut Self {
        if let Err(e) = result {
            self.errors.push(e);
        }
        self
    }

    #[must_use] 
    pub const fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn into_result(self) -> ValidationResult<()> {
        let mut errors = self.errors;
        match errors.len() {
            0 => Ok(()),
            1 => {
                // Vec has exactly 1 element so pop() always returns Some
                if let Some(err) = errors.pop() {
                    Err(err)
                } else {
                    Ok(())
                }
            }
            _ => {
                let messages: Vec<String> = errors.iter()
                    .map(std::string::ToString::to_string)
                    .collect();
                Err(ValidationError::Multiple(messages.join("; ")))
            }
        }
    }
}

impl Default for Validator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// SSRF Protection
// ============================================================================

// Core IP/hostname primitives live in `synctv_media_providers::ssrf` (single
// source of truth). This module provides the higher-level `SSRFValidator` with
// configurable blocklists, custom blocked IPs, and async DNS resolution.
use synctv_media_providers::ssrf as ssrf_primitives;

/// SSRF (Server-Side Request Forgery) protection validator
///
/// Validates URLs to prevent requests to internal/private networks.
/// This is critical for Provider URLs that are fetched server-side.
///
/// Core IP/hostname checking is delegated to `synctv_media_providers::ssrf`.
#[derive(Debug, Clone)]
pub struct SSRFValidator {
    /// Additional IP addresses to block (e.g., cloud metadata endpoints)
    blocked_ips: Vec<IpAddr>,
    /// Whether to block link-local addresses
    block_link_local: bool,
    /// Whether to block localhost/loopback
    block_localhost: bool,
}

impl Default for SSRFValidator {
    fn default() -> Self {
        Self {
            blocked_ips: vec![
                // AWS metadata endpoint
                IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
                // Google Cloud metadata endpoint
                IpAddr::V4(Ipv4Addr::new(169, 254, 169, 253)),
            ],
            block_link_local: true,
            block_localhost: true,
        }
    }
}

impl SSRFValidator {
    /// Create a new SSRF validator with default settings
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an IP address to the blocklist
    #[must_use]
    pub fn with_blocked_ip(mut self, ip: IpAddr) -> Self {
        self.blocked_ips.push(ip);
        self
    }

    /// Set whether to block localhost/loopback addresses
    #[must_use]
    pub const fn block_localhost(mut self, block: bool) -> Self {
        self.block_localhost = block;
        self
    }

    /// Set whether to block link-local addresses
    #[must_use]
    pub const fn block_link_local(mut self, block: bool) -> Self {
        self.block_link_local = block;
        self
    }

    /// Validate a URL for SSRF protection
    ///
    /// Returns Ok(()) if the URL is safe to fetch, Err otherwise.
    /// This method:
    /// 1. Parses the URL
    /// 2. Resolves the hostname to IP addresses
    /// 3. Checks each IP against blocklists
    ///
    /// # Note
    ///
    /// This performs DNS resolution synchronously. For async use,
    /// use `validate_url_async` instead.
    pub fn validate_url(&self, url: &str) -> ValidationResult<()> {
        let parsed = url::Url::parse(url).map_err(|e| ValidationError::SSRF(
            format!("Invalid URL: {e}")
        ))?;

        let host = parsed.host_str().ok_or_else(|| ValidationError::SSRF(
            "URL has no host".to_string()
        ))?;

        // Handle IPv6 addresses which come with brackets from url parser
        let host = if host.starts_with('[') && host.ends_with(']') {
            &host[1..host.len()-1]
        } else {
            host
        };

        // First check if host is an IP address directly
        if let Ok(ip) = host.parse::<IpAddr>() {
            self.validate_ip(&ip)?;
            return Ok(());
        }

        // For hostnames, validate against suspicious patterns
        self.validate_hostname(host)?;

        Ok(())
    }

    /// Validate a URL asynchronously with DNS resolution
    ///
    /// This method resolves the hostname and checks all resolved IPs.
    pub async fn validate_url_async(&self, url: &str) -> ValidationResult<()> {
        let parsed = url::Url::parse(url).map_err(|e| ValidationError::SSRF(
            format!("Invalid URL: {e}")
        ))?;

        let host = parsed.host_str().ok_or_else(|| ValidationError::SSRF(
            "URL has no host".to_string()
        ))?;

        // Check if host is an IP address directly
        if let Ok(ip) = host.parse::<IpAddr>() {
            self.validate_ip(&ip)?;
            return Ok(());
        }

        // Resolve hostname to IPs
        use tokio::net::lookup_host;
        let port = parsed.port().unwrap_or_else(|| {
            match parsed.scheme() {
                "http" => 80,
                "https" => 443,
                "rtmp" => 1935,
                _ => 443,
            }
        });

        let addr_str = format!("{host}:{port}");
        let addrs = lookup_host(&addr_str).await.map_err(|e| ValidationError::SSRF(
            format!("DNS resolution failed for {host}: {e}")
        ))?;

        for socket_addr in addrs {
            self.validate_ip(&socket_addr.ip())?;
        }

        Ok(())
    }

    /// Validate an IP address against blocklists.
    ///
    /// Checks the custom blocklist first, then delegates to shared primitives.
    /// Respects the `block_localhost` and `block_link_local` settings.
    pub fn validate_ip(&self, ip: &IpAddr) -> ValidationResult<()> {
        // Check explicit blocklist
        if self.blocked_ips.contains(ip) {
            return Err(ValidationError::SSRF(
                format!("IP {ip} is in blocklist (cloud metadata endpoint)")
            ));
        }

        match ip {
            IpAddr::V4(ipv4) => {
                let octets = ipv4.octets();
                // Respect configurable localhost blocking
                if !self.block_localhost && octets[0] == 127 {
                    return Ok(());
                }
                // Respect configurable link-local blocking
                if !self.block_link_local && octets[0] == 169 && octets[1] == 254 {
                    return Ok(());
                }
                if ssrf_primitives::is_blocked_ipv4(ipv4) {
                    return Err(ValidationError::SSRF(
                        format!("IP {ip} is a private/reserved address")
                    ));
                }
            }
            IpAddr::V6(ipv6) => {
                // Respect configurable localhost blocking
                if !self.block_localhost && *ipv6 == Ipv6Addr::LOCALHOST {
                    return Ok(());
                }
                // Respect configurable link-local blocking
                if !self.block_link_local && (ipv6.segments()[0] & 0xffc0) == 0xfe80 {
                    return Ok(());
                }
                if ssrf_primitives::is_blocked_ipv6(ipv6) {
                    return Err(ValidationError::SSRF(
                        format!("IP {ip} is a private/reserved address")
                    ));
                }
            }
        }

        Ok(())
    }

    /// Validate a hostname (without DNS resolution)
    ///
    /// Delegates to shared SSRF hostname checking in `synctv_media_providers::ssrf`.
    fn validate_hostname(&self, host: &str) -> ValidationResult<()> {
        // Respect configurable localhost blocking
        if !self.block_localhost {
            let lower = host.to_lowercase();
            if lower == "localhost" || lower == "localhost.localdomain" {
                return Ok(());
            }
        }

        match ssrf_primitives::check_hostname(host) {
            ssrf_primitives::SsrfCheckResult::Ok => Ok(()),
            ssrf_primitives::SsrfCheckResult::Blocked(reason) => {
                Err(ValidationError::SSRF(reason))
            }
        }
    }
}

/// Check if an IP address is private/internal (helper function)
#[must_use]
pub fn is_private_ip(ip: &IpAddr) -> bool {
    ssrf_primitives::is_blocked_ip(*ip)
}

/// Validate a URL for SSRF protection (convenience function)
pub fn validate_url_for_ssrf(url: &str) -> ValidationResult<()> {
    SSRFValidator::new().validate_url(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_username_validation() {
        let validator = UsernameValidator::new();

        // Valid usernames
        assert!(validator.validate("alice").is_ok());
        assert!(validator.validate("bob_123").is_ok());
        assert!(validator.validate("charlie-test").is_ok());

        // Invalid usernames
        assert!(validator.validate("ab").is_err()); // Too short
        assert!(validator.validate("_invalid").is_err()); // Starts with underscore
        assert!(validator.validate("invalid@name").is_err()); // Invalid character
    }

    #[test]
    fn test_password_validation() {
        let validator = PasswordValidator::new();

        // Valid passwords
        assert!(validator.validate("Password123").is_ok());

        // Invalid passwords
        assert!(validator.validate("short").is_err()); // Too short
        assert!(validator.validate("nouppercase123").is_err()); // No uppercase
        assert!(validator.validate("NOLOWERCASE123").is_err()); // No lowercase
        assert!(validator.validate("NoDigits").is_err()); // No digit
    }

    #[test]
    fn test_email_validation() {
        let validator = EmailValidator::new();

        // Valid emails
        assert!(validator.validate("user@example.com").is_ok());
        assert!(validator.validate("user.name@example.co.uk").is_ok());

        // Invalid emails
        assert!(validator.validate("notanemail").is_err());
        assert!(validator.validate("@example.com").is_err());
        assert!(validator.validate("user@").is_err());
    }

    #[test]
    fn test_url_validation() {
        let validator = UrlValidator::new().https_only();

        // Valid HTTPS URLs
        assert!(validator.validate("https://example.com").is_ok());
        assert!(validator.validate("https://example.com/path").is_ok());

        // Invalid URLs
        assert!(validator.validate("http://example.com").is_err()); // Not HTTPS
        assert!(validator.validate("not-a-url").is_err());
    }

    #[test]
    fn test_batch_validation() {
        let mut validator = Validator::new();

        validator
            .validate_field("username", UsernameValidator::new().validate("valid_user"))
            .validate_field("email", EmailValidator::new().validate("invalid-email"))
            .validate_field("password", PasswordValidator::new().validate("short"));

        assert!(!validator.is_valid());
        assert!(validator.into_result().is_err());
    }

    #[test]
    fn test_username_edge_cases() {
        let validator = UsernameValidator::new();

        // Empty username
        assert!(validator.validate("").is_err());

        // Exactly minimum length
        assert!(validator.validate("abc").is_ok());

        // Exactly maximum length (50 chars)
        let max_length = "a".repeat(50);
        assert!(validator.validate(&max_length).is_ok());

        // Over maximum length
        let too_long = "a".repeat(51);
        assert!(validator.validate(&too_long).is_err());

        // Starts with hyphen
        assert!(validator.validate("-invalid").is_err());

        // Unicode characters (should be valid as they are alphanumeric)
        assert!(validator.validate("用户名").is_ok());

        // Mixed valid characters
        assert!(validator.validate("User-Name_123").is_ok());
    }

    #[test]
    fn test_password_edge_cases() {
        let validator = PasswordValidator::new();

        // Empty password
        assert!(validator.validate("").is_err());

        // Exactly minimum length
        assert!(validator.validate("Abcd1234").is_ok());

        // Maximum length (128 chars) without long consecutive repeats
        let max_password = "Ab1".repeat(42) + "Ab";
        assert_eq!(max_password.len(), 128);
        assert!(validator.validate(&max_password).is_ok());

        // Over maximum length
        let too_long = "Ab1".repeat(43);
        assert_eq!(too_long.len(), 129);
        assert!(validator.validate(&too_long).is_err());

        // With special characters
        let validator_with_special = PasswordValidator::new().require_special_char(true);
        assert!(validator_with_special.validate("Password123!").is_ok());
        assert!(validator_with_special.validate("Password123").is_err());

        // Relaxed requirements
        let relaxed_validator = PasswordValidator::new()
            .with_min_length(4)
            .require_special_char(false);
        assert!(relaxed_validator.validate("Abc1").is_ok());
    }

    #[test]
    fn test_password_max_repeated_chars() {
        let validator = PasswordValidator::new(); // default max_repeated_chars = 3

        // 3 consecutive 'a' is OK
        assert!(validator.validate("Paaass1w").is_ok());
        // 4 consecutive 'a' is NOT OK
        assert!(validator.validate("Paaaass1").is_err());

        // Disabled check (0 means no limit)
        let no_limit = PasswordValidator::new().with_max_repeated_chars(0);
        assert!(no_limit.validate("Paaaaaa1").is_ok());
    }

    #[test]
    fn test_password_from_config() {
        use crate::config::PasswordComplexityConfig;

        let config = PasswordComplexityConfig {
            min_length: 10,
            require_uppercase: false,
            require_lowercase: true,
            require_digit: true,
            require_special: true,
            max_repeated_chars: 2,
        };
        let validator = PasswordValidator::from_config(&config);

        // 9 chars = too short (min 10)
        assert!(validator.validate("abcde1!fg").is_err());
        // 10 chars, no uppercase needed, has special
        assert!(validator.validate("abcde1!fgh").is_ok());
        // No special char
        assert!(validator.validate("abcde1fghi").is_err());
        // 3 consecutive chars with max_repeated_chars=2
        assert!(validator.validate("aaabcde1!f").is_err());
    }

    #[test]
    fn test_room_name_validation() {
        let validator = RoomNameValidator::new();

        // Valid room names
        assert!(validator.validate("My Room").is_ok());
        assert!(validator.validate("a").is_ok());
        assert!(validator.validate("Room-123_Test").is_ok());

        // Invalid room names
        assert!(validator.validate("").is_err()); // Empty

        // Control characters
        assert!(validator.validate("Room\x00Name").is_err());
        assert!(validator.validate("Room\nName").is_err());

        // Too long
        let too_long = "a".repeat(101);
        assert!(validator.validate(&too_long).is_err());
    }

    #[test]
    fn test_email_edge_cases() {
        let validator = EmailValidator::new();

        // Valid edge cases
        assert!(validator.validate("a@b.co").is_ok());
        assert!(validator.validate("user+tag@example.com").is_ok());
        assert!(validator.validate("user@sub.domain.example.com").is_ok());

        // Invalid edge cases
        assert!(validator.validate("").is_err());
        assert!(validator.validate("user@.com").is_err());
        assert!(validator.validate("user@example").is_err()); // No TLD
        assert!(validator.validate("user@example.c").is_err()); // TLD too short
    }

    #[test]
    fn test_url_edge_cases() {
        let validator = UrlValidator::new();

        // Both HTTP and HTTPS allowed
        assert!(validator.validate("http://example.com").is_ok());
        assert!(validator.validate("https://example.com").is_ok());

        // HTTPS only
        let https_only = UrlValidator::new().https_only();
        assert!(https_only.validate("https://example.com").is_ok());
        assert!(https_only.validate("http://example.com").is_err());

        // Domain whitelist
        let domain_validator = UrlValidator::new()
            .with_allowed_domains(vec!["example.com".to_string(), "trusted.org".to_string()]);
        assert!(domain_validator.validate("https://example.com/path").is_ok());
        assert!(domain_validator.validate("https://sub.example.com/path").is_ok());
        assert!(domain_validator.validate("https://other.com").is_err());

        // Invalid URLs
        assert!(validator.validate("").is_err());
        assert!(validator.validate("not-a-url").is_err());
        assert!(validator.validate("ftp://example.com").is_ok()); // ftp is a valid scheme
    }

    #[test]
    fn test_validation_error_messages() {
        let validator = UsernameValidator::new();
        let err = validator.validate("ab").unwrap_err();
        assert!(err.to_string().contains("username"));
        assert!(err.to_string().contains("3"));

        let validator = PasswordValidator::new();
        let err = validator.validate("weak").unwrap_err();
        assert!(err.to_string().contains("password"));
        assert!(err.to_string().contains("8"));
    }

    #[test]
    fn test_batch_validation_multiple_errors() {
        let mut validator = Validator::new();

        validator
            .validate_field("username", UsernameValidator::new().validate("ab")) // Too short
            .validate_field("email", EmailValidator::new().validate("invalid")) // Invalid email
            .validate_field("password", PasswordValidator::new().validate("weak")); // Too short

        let result = validator.into_result();
        assert!(result.is_err());

        match result {
            Err(ValidationError::Multiple(msgs)) => {
                assert!(msgs.contains("username"));
                assert!(msgs.contains("email"));
                assert!(msgs.contains("password"));
            }
            _ => panic!("Expected Multiple errors"),
        }
    }

    #[test]
    fn test_ssrf_ipv4_private_addresses() {
        let validator = SSRFValidator::new();

        // Private networks should be blocked
        assert!(validator.validate_url("http://10.0.0.1/path").is_err());
        assert!(validator.validate_url("http://10.255.255.255/path").is_err());
        assert!(validator.validate_url("http://172.16.0.1/path").is_err());
        assert!(validator.validate_url("http://172.31.255.255/path").is_err());
        assert!(validator.validate_url("http://192.168.0.1/path").is_err());
        assert!(validator.validate_url("http://192.168.255.255/path").is_err());

        // Loopback should be blocked
        assert!(validator.validate_url("http://127.0.0.1/path").is_err());
        assert!(validator.validate_url("http://127.255.255.255/path").is_err());

        // Link-local should be blocked
        assert!(validator.validate_url("http://169.254.0.1/path").is_err());
        assert!(validator.validate_url("http://169.254.169.254/path").is_err()); // AWS metadata

        // Current network
        assert!(validator.validate_url("http://0.0.0.0/path").is_err());
        assert!(validator.validate_url("http://0.255.255.255/path").is_err());

        // Multicast
        assert!(validator.validate_url("http://224.0.0.1/path").is_err());
        assert!(validator.validate_url("http://239.255.255.255/path").is_err());

        // Reserved
        assert!(validator.validate_url("http://240.0.0.1/path").is_err());
        assert!(validator.validate_url("http://255.255.255.255/path").is_err());
    }

    #[test]
    fn test_ssrf_ipv6_addresses() {
        let validator = SSRFValidator::new();

        // Loopback
        assert!(validator.validate_url("http://[::1]/path").is_err());

        // Link-local
        assert!(validator.validate_url("http://[fe80::1]/path").is_err());

        // Unique local
        assert!(validator.validate_url("http://[fc00::1]/path").is_err());
        assert!(validator.validate_url("http://[fd00::1]/path").is_err());

        // IPv4-mapped IPv6 addresses
        assert!(validator.validate_url("http://[::ffff:192.168.0.1]/path").is_err());
        assert!(validator.validate_url("http://[::ffff:127.0.0.1]/path").is_err());
    }

    #[test]
    fn test_ssrf_valid_public_addresses() {
        let validator = SSRFValidator::new();

        // Public IP addresses should be allowed
        // Note: These are real public IPs, but we're just testing the validation
        assert!(validator.validate_url("http://8.8.8.8/path").is_ok()); // Google DNS
        assert!(validator.validate_url("http://1.1.1.1/path").is_ok()); // Cloudflare DNS
        assert!(validator.validate_url("http://93.184.216.34/path").is_ok()); // example.com

        // Public hostnames should be allowed (without DNS resolution)
        assert!(validator.validate_url("https://example.com/path").is_ok());
        assert!(validator.validate_url("https://google.com/path").is_ok());
        assert!(validator.validate_url("https://github.com/path").is_ok());
    }

    #[test]
    fn test_ssrf_suspicious_hostnames() {
        let validator = SSRFValidator::new();

        // Localhost variations
        assert!(validator.validate_url("http://localhost/path").is_err());
        assert!(validator.validate_url("http://localhost.localdomain/path").is_err());

        // Internal TLDs
        assert!(validator.validate_url("http://myserver.local/path").is_err());
        assert!(validator.validate_url("http://myserver.internal/path").is_err());

        // Cloud metadata hostnames
        assert!(validator.validate_url("http://metadata.google.internal/path").is_err());
        assert!(validator.validate_url("http://metadata.internal/path").is_err());

        // Container/Kubernetes hostnames
        assert!(validator.validate_url("http://kubernetes.default/path").is_err());
        assert!(validator.validate_url("http://k8s.api/path").is_err());
        assert!(validator.validate_url("http://docker.local/path").is_err());
        assert!(validator.validate_url("http://container.internal/path").is_err());
    }

    #[test]
    fn test_ssrf_localhost_disabled() {
        // Validator with localhost checking disabled
        let validator = SSRFValidator::new().block_localhost(false);

        // Localhost should now be allowed (but still not recommended)
        assert!(validator.validate_url("http://127.0.0.1/path").is_ok());
        assert!(validator.validate_url("http://[::1]/path").is_ok());

        // Private networks should still be blocked
        assert!(validator.validate_url("http://192.168.0.1/path").is_err());
    }

    #[test]
    fn test_ssrf_custom_blocklist() {
        let validator = SSRFValidator::new()
            .with_blocked_ip("1.2.3.4".parse().unwrap());

        // Custom blocked IP
        assert!(validator.validate_url("http://1.2.3.4/path").is_err());

        // Other public IPs should still work
        assert!(validator.validate_url("http://8.8.8.8/path").is_ok());
    }

    #[test]
    fn test_is_private_ip_helper() {
        assert!(is_private_ip(&"192.168.0.1".parse().unwrap()));
        assert!(is_private_ip(&"10.0.0.1".parse().unwrap()));
        assert!(is_private_ip(&"127.0.0.1".parse().unwrap()));
        assert!(!is_private_ip(&"8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip(&"1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn test_validate_url_for_ssrf_helper() {
        assert!(validate_url_for_ssrf("http://192.168.0.1/path").is_err());
        assert!(validate_url_for_ssrf("https://example.com/path").is_ok());
    }

    // ========== SSRF: IPv4-Mapped IPv6 Edge Cases ==========

    #[test]
    fn test_ssrf_ipv4_mapped_ipv6_private_ranges() {
        let validator = SSRFValidator::new();

        // IPv4-mapped IPv6 forms of private addresses must be blocked
        assert!(validator.validate_url("http://[::ffff:10.0.0.1]/path").is_err());
        assert!(validator.validate_url("http://[::ffff:172.16.0.1]/path").is_err());
        assert!(validator.validate_url("http://[::ffff:192.168.1.1]/path").is_err());
        assert!(validator.validate_url("http://[::ffff:127.0.0.1]/path").is_err());
        assert!(validator.validate_url("http://[::ffff:169.254.169.254]/path").is_err());
    }

    #[test]
    fn test_ssrf_ipv4_mapped_ipv6_public_address() {
        let validator = SSRFValidator::new();

        // IPv4-mapped IPv6 form of public addresses should be allowed
        assert!(validator.validate_url("http://[::ffff:8.8.8.8]/path").is_ok());
        assert!(validator.validate_url("http://[::ffff:1.1.1.1]/path").is_ok());
    }

    // ========== SSRF: CGNAT Range (100.64.0.0/10) ==========

    #[test]
    fn test_ssrf_cgnat_range_boundaries() {
        let validator = SSRFValidator::new();

        // CGNAT range: 100.64.0.0 - 100.127.255.255
        assert!(validator.validate_url("http://100.64.0.0/path").is_err());
        assert!(validator.validate_url("http://100.64.0.1/path").is_err());
        assert!(validator.validate_url("http://100.100.100.100/path").is_err());
        assert!(validator.validate_url("http://100.127.255.255/path").is_err());

        // Just outside CGNAT range
        assert!(validator.validate_url("http://100.63.255.255/path").is_ok());
        assert!(validator.validate_url("http://100.128.0.0/path").is_ok());
    }

    // ========== SSRF: IPv6 Unique Local (fc00::/7) ==========

    #[test]
    fn test_ssrf_ipv6_unique_local_both_prefixes() {
        let validator = SSRFValidator::new();

        // fc00::/7 covers both fc00::/8 and fd00::/8
        assert!(validator.validate_url("http://[fc00::1]/path").is_err());
        assert!(validator.validate_url("http://[fd00::1]/path").is_err());
        assert!(validator.validate_url("http://[fdff::1]/path").is_err());
    }

    // ========== SSRF: Validate IP Directly ==========

    #[test]
    fn test_ssrf_validate_ip_directly() {
        let validator = SSRFValidator::new();

        // Test validate_ip directly for edge cases
        assert!(validator.validate_ip(&"0.0.0.0".parse().unwrap()).is_err());
        assert!(validator.validate_ip(&"0.0.0.1".parse().unwrap()).is_err());
        assert!(validator.validate_ip(&"224.0.0.1".parse().unwrap()).is_err());
        assert!(validator.validate_ip(&"255.255.255.255".parse().unwrap()).is_err());
        assert!(validator.validate_ip(&"240.0.0.1".parse().unwrap()).is_err());
        assert!(validator.validate_ip(&"8.8.8.8".parse().unwrap()).is_ok());
    }

    // ========== SSRF: URL Parsing Edge Cases ==========

    #[test]
    fn test_ssrf_invalid_url() {
        let validator = SSRFValidator::new();
        assert!(validator.validate_url("not-a-url").is_err());
        assert!(validator.validate_url("").is_err());
    }

    #[test]
    fn test_ssrf_url_without_host() {
        let validator = SSRFValidator::new();
        // A URL like "file:///etc/passwd" has no host
        assert!(validator.validate_url("file:///etc/passwd").is_err());
    }

    // ========== SSRF: Hostname Edge Cases ==========

    #[test]
    fn test_ssrf_instance_data_hostname() {
        let validator = SSRFValidator::new();
        assert!(validator.validate_url("http://instance-data/latest/meta-data").is_err());
    }

    #[test]
    fn test_ssrf_metadata_azure_hostname() {
        let validator = SSRFValidator::new();
        assert!(validator.validate_url("http://metadata.azure/metadata/instance").is_err());
    }

    // ========== SSRF: Link-Local Disabled ==========

    #[test]
    fn test_ssrf_link_local_disabled() {
        let validator = SSRFValidator::new().block_link_local(false);

        // Link-local should be allowed when disabled
        assert!(validator.validate_url("http://169.254.0.1/path").is_ok());
        // But metadata endpoint is still blocked via explicit blocklist
        assert!(validator.validate_url("http://169.254.169.254/path").is_err());
    }

    // ========== Validation: Password Max Length ==========

    #[test]
    fn test_password_exactly_at_max_length() {
        let validator = PasswordValidator::new();
        // Exactly 128 chars should be OK (no long consecutive repeats)
        let pwd = "Ab1".repeat(42) + "Ab";
        assert_eq!(pwd.len(), 128);
        assert!(validator.validate(&pwd).is_ok());
    }

    #[test]
    fn test_password_one_over_max_length() {
        let validator = PasswordValidator::new();
        let pwd = "Ab1".repeat(43);
        assert_eq!(pwd.len(), 129);
        assert!(validator.validate(&pwd).is_err());
    }

    // ========== Validation: Batch Validator Single Error ==========

    #[test]
    fn test_batch_validation_single_error_returns_field_not_multiple() {
        let mut validator = Validator::new();
        validator.validate_field("username", UsernameValidator::new().validate("ab"));
        let result = validator.into_result();
        assert!(result.is_err());
        // Single error should be Field variant, not Multiple
        match result {
            Err(ValidationError::Field { field, .. }) => assert_eq!(field, "username"),
            _ => panic!("Expected Field error for single validation failure"),
        }
    }

    #[test]
    fn test_batch_validation_no_errors_is_ok() {
        let mut validator = Validator::new();
        validator
            .validate_field("username", UsernameValidator::new().validate("validuser"))
            .validate_field("email", EmailValidator::new().validate("user@example.com"));
        assert!(validator.is_valid());
        assert!(validator.into_result().is_ok());
    }
}
