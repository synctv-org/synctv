//! Input validation using mature crates
//!
//! This module provides production-grade input validation using the `validator` crate.
//!
//! # SSRF Protection
//!
//! SSRF (Server-Side Request Forgery) protection is provided by the `url_jail` crate,
//! which offers:
//! - DNS rebinding protection (validates after DNS resolution)
//! - IP encoding attack detection (hex, octal, decimal, short-form)
//! - Cloud metadata endpoint blocking (AWS, GCP, Azure, Alibaba)
//! - Private IP range blocking
//! - Custom blocklist support

use std::net::{IpAddr, Ipv6Addr};
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
/// Maximum password length (prevent bcrypt `DoS`; bcrypt input limit is 72 bytes,
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

/// Maximum room description length (must match DB constraint `rooms_description_length_check`)
pub const ROOM_DESCRIPTION_MAX: usize = 500;

// ============================================================================
// Reserved usernames — prevent phishing/impersonation attacks
// ============================================================================

/// Reserved usernames that cannot be used to prevent phishing/impersonation.
/// Case-insensitive matching is applied.
///
/// These usernames are commonly used by system administrators and support staff,
/// so allowing regular users to claim them could enable social engineering attacks.
pub const RESERVED_USERNAMES: &[&str] = &[
    "admin",
    "administrator",
    "system",
    "root",
    "moderator",
    "mod",
    "support",
    "help",
    "official",
    "staff",
    "owner",
    "bot",
    "service",
    "sysop",
    "operator",
    "dev",
    "developer",
    "security",
    "team",
];

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
        if !username
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
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

        // Check reserved usernames (case-insensitive)
        let lower_username = username.to_lowercase();
        for reserved in RESERVED_USERNAMES {
            if lower_username == *reserved {
                return Err(ValidationError::Field {
                    field: "username".to_string(),
                    message: format!("'{username}' is reserved and cannot be used"),
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
    pub const fn from_config(config: &crate::config::PasswordComplexityConfig) -> Self {
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
        if self.require_special_char && !password.chars().any(|c| !c.is_alphanumeric()) {
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
                        if !domains
                            .iter()
                            .any(|d| host == d.as_str() || host.ends_with(&format!(".{d}")))
                        {
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
        Self { errors: Vec::new() }
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
                let messages: Vec<String> = errors
                    .iter()
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
// SSRF Protection (using url_jail)
// ============================================================================

// Re-export url_jail types for convenience
pub use url_jail::{Policy, PolicyBuilder, Validated};

/// SSRF (Server-Side Request Forgery) protection validator
///
/// Validates URLs to prevent requests to internal/private networks.
/// Uses the `url_jail` crate for production-grade SSRF protection including:
/// - DNS rebinding protection
/// - IP encoding attack detection (hex, octal, decimal, short-form)
/// - Cloud metadata endpoint blocking (AWS, GCP, Azure, Alibaba)
/// - Private IP range blocking
///
/// # Example
///
/// ```
/// use synctv_core::validation::{SSRFValidator, Policy};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Default validator with PublicOnly policy
/// let validator = SSRFValidator::new();
/// validator.validate_url("https://example.com/api")?;
///
/// // Allow private IPs (for internal services)
/// let internal_validator = SSRFValidator::with_policy(Policy::AllowPrivate);
/// internal_validator.validate_url("http://192.168.1.1/internal")?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SSRFValidator {
    /// The `url_jail` policy to use for validation
    policy: Policy,
    /// Additional blocked IPs (for custom blocklists)
    blocked_ips: Vec<IpAddr>,
    /// Blocked hostnames (for internal hostnames like localhost.localdomain)
    blocked_hostnames: Vec<String>,
}

impl Default for SSRFValidator {
    fn default() -> Self {
        // No additional blocked IPs by default (url_jail handles standard private ranges)
        // CGNAT (100.64.0.0/10, RFC 6598) is blocked in the pre-url_jail IP check
        let blocked_ips = vec![];

        // Blocked hostnames (internal/private hostnames)
        let blocked_hostnames = vec![
            "localhost.localdomain".to_string(),
            "metadata.google.internal".to_string(),
            "myserver.local".to_string(),
            "myserver.internal".to_string(),
            "kubernetes.default".to_string(),
            "k8s.api".to_string(),
            "docker.local".to_string(),
            "container.internal".to_string(),
            "instance-data".to_string(),
            "metadata.azure".to_string(),
        ];

        Self {
            policy: Policy::PublicOnly,
            blocked_ips,
            blocked_hostnames,
        }
    }
}

impl SSRFValidator {
    /// Create a new SSRF validator with default `PublicOnly` policy.
    ///
    /// Blocks: private IPs, loopback, link-local, cloud metadata endpoints.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a validator with a custom policy.
    #[must_use]
    pub const fn with_policy(policy: Policy) -> Self {
        Self {
            policy,
            blocked_ips: Vec::new(),
            blocked_hostnames: Vec::new(),
        }
    }

    /// Create a validator that allows private IPs.
    ///
    /// Use this for internal services that need to access private networks.
    /// Still blocks loopback and cloud metadata endpoints.
    #[must_use]
    pub const fn allow_private() -> Self {
        Self::with_policy(Policy::AllowPrivate)
    }

    /// Add an IP address to the blocklist.
    #[must_use]
    pub fn with_blocked_ip(mut self, ip: IpAddr) -> Self {
        self.blocked_ips.push(ip);
        self
    }

    /// Validate a URL for SSRF protection (synchronous).
    ///
    /// Returns Ok(()) if the URL is safe to fetch, Err otherwise.
    pub fn validate_url(&self, url: &str) -> ValidationResult<()> {
        // First check custom blocklist at URL level
        if let Ok(parsed) = url::Url::parse(url) {
            if let Some(host) = parsed.host_str() {
                // Handle IPv6 addresses with brackets
                let host_str = if host.starts_with('[') && host.ends_with(']') {
                    &host[1..host.len() - 1]
                } else {
                    host
                };

                // Check if host is a blocked hostname (before IP parsing)
                let host_lower = host_str.to_lowercase();
                for blocked in &self.blocked_hostnames {
                    if host_lower == *blocked || host_lower.starts_with(&format!("{blocked}.")) {
                        return Err(ValidationError::SSRF(format!(
                            "hostname '{host_str}' is blocked"
                        )));
                    }
                }

                // Check if host is a blocked IP
                if let Ok(ip) = host_str.parse::<IpAddr>() {
                    if self.blocked_ips.contains(&ip) {
                        return Err(ValidationError::SSRF(format!(
                            "IP {ip} is in custom blocklist"
                        )));
                    }

                    // Check for additional blocked IP ranges that url_jail doesn't block
                    // These include CGNAT, multicast, reserved, and current network ranges
                    if let IpAddr::V4(ipv4) = ip {
                        let octets = ipv4.octets();

                        // CGNAT / Shared Address Space: 100.64.0.0/10 (RFC 6598)
                        if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                            return Err(ValidationError::SSRF(format!("CGNAT IP {ip} is blocked")));
                        }

                        // Current network: 0.0.0.0/8
                        if octets[0] == 0 {
                            return Err(ValidationError::SSRF(format!(
                                "Current network IP {ip} is blocked"
                            )));
                        }

                        // Multicast: 224.0.0.0/4
                        if (224..=239).contains(&octets[0]) {
                            return Err(ValidationError::SSRF(format!(
                                "Multicast IP {ip} is blocked"
                            )));
                        }

                        // Reserved/Broadcast: 240.0.0.0/4
                        if octets[0] >= 240 {
                            return Err(ValidationError::SSRF(format!(
                                "Reserved IP {ip} is blocked"
                            )));
                        }
                    }
                }
            }
        }

        // Use url_jail for validation
        match url_jail::validate_sync(url, self.policy) {
            Ok(_) => Ok(()),
            Err(e) => {
                let reason = if e.is_blocked() {
                    format!("SSRF blocked: {e}")
                } else if e.is_retriable() {
                    format!("Temporary error: {e}")
                } else {
                    format!("Validation error: {e}")
                };
                Err(ValidationError::SSRF(reason))
            }
        }
    }

    /// Validate a URL asynchronously with DNS resolution.
    ///
    /// This method resolves the hostname and checks all resolved IPs.
    pub async fn validate_url_async(&self, url: &str) -> ValidationResult<()> {
        // First check custom blocklist at URL level
        if let Ok(parsed) = url::Url::parse(url) {
            if let Some(host) = parsed.host_str() {
                let host_str = if host.starts_with('[') && host.ends_with(']') {
                    &host[1..host.len() - 1]
                } else {
                    host
                };

                if let Ok(ip) = host_str.parse::<IpAddr>() {
                    if self.blocked_ips.contains(&ip) {
                        return Err(ValidationError::SSRF(format!(
                            "IP {ip} is in custom blocklist"
                        )));
                    }

                    // Check for additional blocked IP ranges that url_jail doesn't block
                    if let IpAddr::V4(ipv4) = ip {
                        let octets = ipv4.octets();

                        // CGNAT / Shared Address Space: 100.64.0.0/10 (RFC 6598)
                        if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                            return Err(ValidationError::SSRF(format!("CGNAT IP {ip} is blocked")));
                        }

                        // Current network: 0.0.0.0/8
                        if octets[0] == 0 {
                            return Err(ValidationError::SSRF(format!(
                                "Current network IP {ip} is blocked"
                            )));
                        }

                        // Multicast: 224.0.0.0/4
                        if (224..=239).contains(&octets[0]) {
                            return Err(ValidationError::SSRF(format!(
                                "Multicast IP {ip} is blocked"
                            )));
                        }

                        // Reserved/Broadcast: 240.0.0.0/4
                        if octets[0] >= 240 {
                            return Err(ValidationError::SSRF(format!(
                                "Reserved IP {ip} is blocked"
                            )));
                        }
                    }
                }
            }
        }

        // Use url_jail for async validation with DNS resolution
        match url_jail::validate(url, self.policy).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let reason = if e.is_blocked() {
                    format!("SSRF blocked: {e}")
                } else if e.is_retriable() {
                    format!("Temporary error: {e}")
                } else {
                    format!("Validation error: {e}")
                };
                Err(ValidationError::SSRF(reason))
            }
        }
    }

    /// Validate an IP address against blocklists.
    ///
    /// Checks the custom blocklist first, then validates against the policy.
    pub fn validate_ip(&self, ip: &IpAddr) -> ValidationResult<()> {
        // Check custom blocklist
        if self.blocked_ips.contains(ip) {
            return Err(ValidationError::SSRF(format!(
                "IP {ip} is in custom blocklist"
            )));
        }

        // Use shared IP validation from synctv_media_providers
        use synctv_media_providers::ssrf::is_blocked_ip;

        // For PublicOnly policy, check if IP is blocked
        if matches!(self.policy, Policy::PublicOnly) && is_blocked_ip(*ip) {
            return Err(ValidationError::SSRF(format!(
                "IP {ip} is a private/reserved address"
            )));
        }

        // For AllowPrivate, still block loopback and link-local
        if matches!(self.policy, Policy::AllowPrivate) {
            match ip {
                IpAddr::V4(v4) => {
                    let o = v4.octets();
                    if o[0] == 127 {
                        return Err(ValidationError::SSRF(
                            "Loopback address not allowed".to_string(),
                        ));
                    }
                    if o[0] == 169 && o[1] == 254 {
                        return Err(ValidationError::SSRF(
                            "Link-local address not allowed".to_string(),
                        ));
                    }
                }
                IpAddr::V6(v6) => {
                    if *v6 == Ipv6Addr::LOCALHOST {
                        return Err(ValidationError::SSRF(
                            "Loopback address not allowed".to_string(),
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    /// Get the current policy.
    #[must_use]
    pub const fn policy(&self) -> &Policy {
        &self.policy
    }
}

/// Check if an IP address is private/internal (helper function).
#[must_use]
pub fn is_private_ip(ip: &IpAddr) -> bool {
    synctv_media_providers::ssrf::is_blocked_ip(*ip)
}

/// Validate a URL for SSRF protection with default policy.
///
/// Convenience function that uses `SSRFValidator::new()`.
pub fn validate_url_for_ssrf(url: &str) -> ValidationResult<()> {
    SSRFValidator::new().validate_url(url)
}

/// Validate a URL for SSRF protection with a custom policy.
pub fn validate_url_with_policy(url: &str, policy: Policy) -> ValidationResult<()> {
    SSRFValidator::with_policy(policy).validate_url(url)
}

/// Validate an RTMP/RTMPS URL for SSRF protection (synchronous, no DNS resolution).
///
/// Since `url::Url` cannot parse `rtmp://` URLs, this function extracts the host
/// manually from the `rtmp://host[:port]/app/stream` format and checks it against
/// the SSRF blocklists. This is a synchronous check that does NOT perform DNS
/// resolution - use `validate_rtmp_url_host_with_dns()` for DNS rebinding protection.
///
/// # Arguments
///
/// * `raw` - The RTMP/RTMPS URL to validate (e.g., "<rtmp://example.com/live/stream>")
///
/// # Returns
///
/// `Ok(())` if the URL's host passes SSRF checks, `Err(ValidationError)` otherwise.
///
/// # Errors
///
/// Returns `ValidationError::SSRF` if:
/// - The URL scheme is not `rtmp://` or `rtmps://`
/// - The host is a private/internal IP address
/// - The host is a blocked hostname (localhost, metadata endpoints, etc.)
pub fn validate_rtmp_url_for_ssrf(raw: &str) -> ValidationResult<()> {
    let rest = raw
        .strip_prefix("rtmp://")
        .or_else(|| raw.strip_prefix("rtmps://"))
        .ok_or_else(|| ValidationError::SSRF("Expected rtmp:// or rtmps:// scheme".to_string()))?;

    let authority = rest.split('/').next().unwrap_or(rest);

    // Handle IPv6 addresses in brackets: [::1]:port or [::1]
    let host_str = if authority.starts_with('[') {
        // IPv6 address in brackets
        if let Some(end) = authority.find(']') {
            &authority[1..end]
        } else {
            return Err(ValidationError::SSRF(
                "Malformed IPv6 address in URL".to_string(),
            ));
        }
    } else if let Some((host, _port_str)) = authority.rsplit_once(':') {
        // IPv4 or hostname with port
        host
    } else {
        // IPv4 or hostname without port
        authority
    };

    // Check if host is a literal IP address
    if let Ok(ip) = host_str.parse::<IpAddr>() {
        if is_private_ip(&ip) {
            return Err(ValidationError::SSRF(
                "URL targets a private IP address".to_string(),
            ));
        }
        return Ok(());
    }

    // Check hostname against shared blocklist (localhost, metadata endpoints, etc.)
    use synctv_media_providers::ssrf::{check_hostname, SsrfCheckResult};
    match check_hostname(host_str) {
        SsrfCheckResult::Ok => Ok(()),
        SsrfCheckResult::Blocked(reason) => Err(ValidationError::SSRF(reason)),
    }
}

/// Validate an RTMP/RTMPS URL's host with async DNS resolution for DNS rebinding protection.
///
/// This is an async version of `validate_rtmp_url_for_ssrf` that additionally performs
/// DNS resolution to check if the hostname resolves to a private IP address. This
/// prevents DNS rebinding attacks where a domain passes static hostname checks but
/// resolves to a private/internal IP address at query time.
///
/// # Arguments
///
/// * `raw` - The RTMP/RTMPS URL to validate
///
/// # Returns
///
/// `Ok(())` if the URL passes all SSRF checks, `Err(ValidationError)` otherwise.
///
/// # Errors
///
/// Returns `ValidationError::SSRF` if:
/// - The URL scheme is not `rtmp://` or `rtmps://`
/// - The host is a private/internal IP address
/// - The host is a blocked hostname
/// - DNS resolution fails
/// - The hostname resolves to a private IP address
pub async fn validate_rtmp_url_host_with_dns(raw: &str) -> ValidationResult<()> {
    // First do synchronous validation
    validate_rtmp_url_for_ssrf(raw)?;

    // Extract host and port for DNS resolution
    let rest = raw
        .strip_prefix("rtmp://")
        .or_else(|| raw.strip_prefix("rtmps://"))
        .ok_or_else(|| ValidationError::SSRF("Expected rtmp:// or rtmps:// scheme".to_string()))?;

    let authority = rest.split('/').next().unwrap_or(rest);

    // Handle IPv6 addresses in brackets: [::1]:port or [::1]
    let (host_str, port) = if authority.starts_with('[') {
        if let Some(end) = authority.find(']') {
            let host = &authority[1..end];
            let remainder = &authority[end + 1..];
            let port = if let Some(stripped) = remainder.strip_prefix(':') {
                stripped.parse::<u16>().unwrap_or(1935)
            } else {
                1935
            };
            (host, port)
        } else {
            return Err(ValidationError::SSRF(
                "Malformed IPv6 address in URL".to_string(),
            ));
        }
    } else if let Some((host, port_str)) = authority.rsplit_once(':') {
        (host, port_str.parse::<u16>().unwrap_or(1935))
    } else {
        (authority, 1935u16)
    };

    // Skip DNS resolution for literal IP addresses (already validated above)
    if host_str.parse::<IpAddr>().is_ok() {
        return Ok(());
    }

    // Perform async DNS resolution
    let addrs = tokio::net::lookup_host((host_str, port))
        .await
        .map_err(|e| ValidationError::SSRF(format!("DNS lookup failed for {host_str}: {e}")))?;

    let mut found = false;
    for addr in addrs {
        if is_private_ip(&addr.ip()) {
            return Err(ValidationError::SSRF(format!(
                "Hostname {host_str} resolves to private/reserved IP {}",
                addr.ip()
            )));
        }
        found = true;
    }

    if !found {
        return Err(ValidationError::SSRF(format!(
            "Hostname {host_str} resolved to no addresses"
        )));
    }

    Ok(())
}

// ============================================================================
// Path Traversal Validation
// ============================================================================

/// Validate a path component for directory traversal attacks
///
/// This function checks for various path traversal patterns including:
/// - Literal `..` (double dot)
/// - URL-encoded variants (`%2e%2e`, `%2E%2E`)
/// - Double-encoded variants (`%252e%252e`)
/// - Partial encoding (`.%2e`, `%2e.`)
/// - Backslash traversal on Windows (`..\\`)
/// - Mixed dot sequences (`./..`, `../.`)
/// - Null bytes
///
/// # Arguments
/// * `path` - The path component to validate
///
/// # Returns
/// * `Ok(())` if the path is safe
/// * `Err(ValidationError)` if path traversal is detected
///
/// # Examples
/// ```ignore
/// use synctv_core::validation::validate_path_for_traversal;
///
/// // Safe paths
/// assert!(validate_path_for_traversal("media/movies").is_ok());
/// assert!(validate_path_for_traversal("/absolute/path").is_ok());
///
/// // Unsafe paths
/// assert!(validate_path_for_traversal("../../../etc/passwd").is_err());
/// assert!(validate_path_for_traversal("%2e%2e/secret").is_err());
/// ```
pub fn validate_path_for_traversal(path: &str) -> Result<(), ValidationError> {
    // Check 1: Literal .. (most basic)
    if path.contains("..") {
        return Err(ValidationError::Field {
            field: "path".to_string(),
            message: "must not contain '..' for path traversal".to_string(),
        });
    }

    // Check 2: Null bytes
    if path.contains('\0') {
        return Err(ValidationError::Field {
            field: "path".to_string(),
            message: "must not contain null bytes".to_string(),
        });
    }

    // Check 3: Backslash traversal (Windows-style)
    if path.contains("..\\") || path.contains("\\..") {
        return Err(ValidationError::Field {
            field: "path".to_string(),
            message: "must not contain backslash path traversal".to_string(),
        });
    }

    // Check 4: Mixed traversal (e.g., "./../")
    if path.contains("./.") {
        return Err(ValidationError::Field {
            field: "path".to_string(),
            message: "must not contain mixed dot sequences".to_string(),
        });
    }

    // Check 5: URL-encoded variants and complex attacks
    // We need to check for:
    // - %2e%2e or %2E%2E (single-encoded ..)
    // - %252e%252e (double-encoded ..)
    // - .%2e or %2e. (partial encoding)
    // - Any % followed by hex that decodes to .
    //
    // Strategy: Reject any path containing %2e or %2E (URL-encoded dot)
    // as it's too complex to correctly validate all encoding combinations.
    // Legitimate paths don't need URL-encoded dots.
    let bytes = path.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &path[i + 1..i + 3];
            if let Ok(byte_val) = u8::from_str_radix(hex, 16) {
                // Check if this decodes to a dot (0x2E)
                if byte_val == 0x2E {
                    return Err(ValidationError::Field {
                        field: "path".to_string(),
                        message: "must not contain URL-encoded dot character".to_string(),
                    });
                }

                // Check for double-encoded dot: %252e / %252E
                // %25 decodes to %, so %252e -> %2e -> .
                if byte_val == 0x25 && i + 4 < bytes.len() {
                    let inner_hex = &path[i + 3..i + 5];
                    if let Ok(inner_val) = u8::from_str_radix(inner_hex, 16) {
                        if inner_val == 0x2E {
                            return Err(ValidationError::Field {
                                field: "path".to_string(),
                                message: "must not contain double-encoded dot character"
                                    .to_string(),
                            });
                        }
                    }
                }

                // Also check for encoded / or \ after a dot
                if byte_val == 0x2F || byte_val == 0x5C {
                    // Check if we had .. before this
                    if i >= 2 && bytes[i - 2] == b'.' && bytes[i - 1] == b'.' {
                        return Err(ValidationError::Field {
                            field: "path".to_string(),
                            message: "must not contain '..' followed by encoded separator"
                                .to_string(),
                        });
                    }
                }
            }
        }
        i += 1;
    }

    Ok(())
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
        assert!(domain_validator
            .validate("https://example.com/path")
            .is_ok());
        assert!(domain_validator
            .validate("https://sub.example.com/path")
            .is_ok());
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
        assert!(err.to_string().contains('3'));

        let validator = PasswordValidator::new();
        let err = validator.validate("weak").unwrap_err();
        assert!(err.to_string().contains("password"));
        assert!(err.to_string().contains('8'));
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

        // Private networks should be blocked (RFC 1918)
        assert!(validator.validate_url("http://10.0.0.1/path").is_err());
        assert!(validator
            .validate_url("http://10.255.255.255/path")
            .is_err());
        assert!(validator.validate_url("http://172.16.0.1/path").is_err());
        assert!(validator
            .validate_url("http://172.31.255.255/path")
            .is_err());
        assert!(validator.validate_url("http://192.168.0.1/path").is_err());
        assert!(validator
            .validate_url("http://192.168.255.255/path")
            .is_err());

        // Loopback should be blocked
        assert!(validator.validate_url("http://127.0.0.1/path").is_err());
        assert!(validator
            .validate_url("http://127.255.255.255/path")
            .is_err());

        // Link-local should be blocked (includes cloud metadata)
        assert!(validator.validate_url("http://169.254.0.1/path").is_err());
        assert!(validator
            .validate_url("http://169.254.169.254/path")
            .is_err()); // AWS metadata

        // Current network (0.0.0.0/8) - url_jail blocks this
        assert!(validator.validate_url("http://0.0.0.0/path").is_err());
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
        assert!(validator
            .validate_url("http://[::ffff:192.168.0.1]/path")
            .is_err());
        assert!(validator
            .validate_url("http://[::ffff:127.0.0.1]/path")
            .is_err());
    }

    #[test]
    fn test_ssrf_valid_public_addresses() {
        let validator = SSRFValidator::new();

        // Public IP addresses should be allowed
        assert!(validator.validate_url("http://8.8.8.8/path").is_ok()); // Google DNS
        assert!(validator.validate_url("http://1.1.1.1/path").is_ok()); // Cloudflare DNS
        assert!(validator.validate_url("http://93.184.216.34/path").is_ok()); // example.com

        // Public hostnames should be allowed
        assert!(validator.validate_url("https://example.com/path").is_ok());
        assert!(validator.validate_url("https://google.com/path").is_ok());
        assert!(validator.validate_url("https://github.com/path").is_ok());
    }

    #[test]
    fn test_ssrf_suspicious_hostnames() {
        let validator = SSRFValidator::new();

        // Localhost - url_jail blocks this
        assert!(validator.validate_url("http://localhost/path").is_err());

        // Cloud metadata hostnames - url_jail blocks GCP metadata
        assert!(validator
            .validate_url("http://metadata.google.internal/path")
            .is_err());

        // Note: url_jail does NOT block these by default:
        // - .local suffix (e.g., myserver.local)
        // - .internal suffix (except specific cloud metadata like metadata.google.internal)
        // - kubernetes.default
        // - docker.local
        // Use a custom blocklist with SSRFValidator::with_blocked_hostname() if needed.
    }

    #[test]
    fn test_ssrf_allow_private_policy() {
        // AllowPrivate policy allows private IPs but still blocks loopback
        let validator = SSRFValidator::allow_private();

        // Private networks should be allowed
        assert!(validator.validate_url("http://192.168.0.1/path").is_ok());
        assert!(validator.validate_url("http://10.0.0.1/path").is_ok());

        // Loopback should still be blocked
        assert!(validator.validate_url("http://127.0.0.1/path").is_err());
        assert!(validator.validate_url("http://[::1]/path").is_err());
    }

    #[test]
    fn test_ssrf_custom_blocklist() {
        let validator = SSRFValidator::new().with_blocked_ip("1.2.3.4".parse().unwrap());

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
        assert!(validator
            .validate_url("http://[::ffff:10.0.0.1]/path")
            .is_err());
        assert!(validator
            .validate_url("http://[::ffff:172.16.0.1]/path")
            .is_err());
        assert!(validator
            .validate_url("http://[::ffff:192.168.1.1]/path")
            .is_err());
        assert!(validator
            .validate_url("http://[::ffff:127.0.0.1]/path")
            .is_err());
        assert!(validator
            .validate_url("http://[::ffff:169.254.169.254]/path")
            .is_err());
    }

    #[test]
    fn test_ssrf_ipv4_mapped_ipv6_public_address() {
        let validator = SSRFValidator::new();

        // IPv4-mapped IPv6 form of public addresses should be allowed
        assert!(validator
            .validate_url("http://[::ffff:8.8.8.8]/path")
            .is_ok());
        assert!(validator
            .validate_url("http://[::ffff:1.1.1.1]/path")
            .is_ok());
    }

    // ========== SSRF: CGNAT Range (100.64.0.0/10) ==========

    #[test]
    fn test_ssrf_cgnat_range_blocked() {
        // CGNAT / Shared Address Space (100.64.0.0/10, RFC 6598) is blocked
        let validator = SSRFValidator::new();

        assert!(validator.validate_url("http://100.64.0.0/path").is_err());
        assert!(validator
            .validate_url("http://100.100.100.100/path")
            .is_err());
        assert!(validator
            .validate_url("http://100.127.255.255/path")
            .is_err());

        // Just outside CGNAT range should be allowed
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
        assert!(validator
            .validate_ip(&"224.0.0.1".parse().unwrap())
            .is_err());
        assert!(validator
            .validate_ip(&"255.255.255.255".parse().unwrap())
            .is_err());
        assert!(validator
            .validate_ip(&"240.0.0.1".parse().unwrap())
            .is_err());
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
        assert!(validator
            .validate_url("http://instance-data/latest/meta-data")
            .is_err());
    }

    #[test]
    fn test_ssrf_metadata_azure_ip() {
        // Azure uses the link-local IP 169.254.169.254 for metadata
        // url_jail blocks this IP (link-local range)
        let validator = SSRFValidator::new();
        assert!(validator
            .validate_url("http://169.254.169.254/metadata/instance")
            .is_err());
    }

    // ========== SSRF: IP Encoding Attacks (url_jail handles these) ==========

    #[test]
    fn test_ssrf_ip_encoding_attacks() {
        let validator = SSRFValidator::new();

        // Decimal encoding of 127.0.0.1 = 2130706433
        assert!(validator.validate_url("http://2130706433/").is_err());

        // Hex encoding of 127.0.0.1 = 0x7f000001
        assert!(validator.validate_url("http://0x7f000001/").is_err());

        // Octal encoding of 127.0.0.1 = 0177.0.0.1
        assert!(validator.validate_url("http://0177.0.0.1/").is_err());

        // Short-form of 127.0.0.1 = 127.1
        assert!(validator.validate_url("http://127.1/").is_err());

        // IPv4-mapped IPv6
        assert!(validator
            .validate_url("http://[::ffff:127.0.0.1]/")
            .is_err());
    }

    // ========== SSRF: Policy Tests ==========

    #[test]
    fn test_ssrf_policy_public_only() {
        // Default policy is PublicOnly
        let validator = SSRFValidator::new();
        assert!(matches!(validator.policy(), &Policy::PublicOnly));

        // Should block private IPs
        assert!(validator.validate_url("http://192.168.1.1/").is_err());
    }

    #[test]
    fn test_ssrf_policy_allow_private() {
        let validator = SSRFValidator::with_policy(Policy::AllowPrivate);

        // Should allow private IPs
        assert!(validator.validate_url("http://192.168.1.1/").is_ok());

        // But still block loopback
        assert!(validator.validate_url("http://127.0.0.1/").is_err());
    }

    #[test]
    fn test_ssrf_with_policy_helper() {
        // PublicOnly should block private
        assert!(validate_url_with_policy("http://192.168.1.1/", Policy::PublicOnly).is_err());

        // AllowPrivate should allow private
        assert!(validate_url_with_policy("http://192.168.1.1/", Policy::AllowPrivate).is_ok());
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

    // ========== RTMP SSRF Validation ==========

    #[test]
    fn test_rtmp_ssrf_blocks_localhost() {
        assert!(validate_rtmp_url_for_ssrf("rtmp://localhost/live/stream").is_err());
        assert!(validate_rtmp_url_for_ssrf("rtmps://localhost/live/stream").is_err());
    }

    #[test]
    fn test_rtmp_ssrf_blocks_private_ipv4() {
        assert!(validate_rtmp_url_for_ssrf("rtmp://10.0.0.1/live/stream").is_err());
        assert!(validate_rtmp_url_for_ssrf("rtmp://192.168.1.1/live/stream").is_err());
        assert!(validate_rtmp_url_for_ssrf("rtmp://172.16.0.1/live/stream").is_err());
        assert!(validate_rtmp_url_for_ssrf("rtmp://127.0.0.1/live/stream").is_err());
        assert!(validate_rtmp_url_for_ssrf("rtmps://10.0.0.1:1935/live/stream").is_err());
        assert!(validate_rtmp_url_for_ssrf("rtmp://0.0.0.0/live/stream").is_err());
    }

    #[test]
    fn test_rtmp_ssrf_blocks_private_ipv6() {
        assert!(validate_rtmp_url_for_ssrf("rtmp://[::1]/live/stream").is_err());
        assert!(validate_rtmp_url_for_ssrf("rtmp://[fe80::1]/live/stream").is_err());
    }

    #[test]
    fn test_rtmp_ssrf_blocks_metadata_endpoints() {
        assert!(validate_rtmp_url_for_ssrf("rtmp://metadata.google.internal/live").is_err());
        assert!(validate_rtmp_url_for_ssrf("rtmp://instance-data/live").is_err());
    }

    #[test]
    fn test_rtmp_ssrf_allows_public_urls() {
        assert!(validate_rtmp_url_for_ssrf("rtmp://live.example.com/live/stream").is_ok());
        assert!(validate_rtmp_url_for_ssrf("rtmps://live.example.com/live/stream").is_ok());
        assert!(validate_rtmp_url_for_ssrf("rtmp://93.184.216.34:1935/live/stream").is_ok());
    }

    #[test]
    fn test_rtmp_ssrf_rejects_non_rtmp_scheme() {
        assert!(validate_rtmp_url_for_ssrf("http://example.com/stream").is_err());
        assert!(validate_rtmp_url_for_ssrf("https://example.com/stream").is_err());
    }

    // ========== Path Traversal Validation ==========

    #[test]
    fn test_validate_path_for_traversal_rejects_literal_double_dot() {
        assert!(validate_path_for_traversal("../../../etc/passwd").is_err());
        assert!(validate_path_for_traversal("../secret").is_err());
        assert!(validate_path_for_traversal("test/../etc").is_err());
        assert!(validate_path_for_traversal("/safe/../../etc").is_err());
    }

    #[test]
    fn test_validate_path_for_traversal_rejects_url_encoded_dot() {
        // URL-encoded . (2E in hex)
        assert!(validate_path_for_traversal("%2e%2e/etc/passwd").is_err());
        assert!(validate_path_for_traversal("%2E%2E/secret").is_err()); // uppercase
        assert!(validate_path_for_traversal("test/%2e%2e/config").is_err());
        assert!(validate_path_for_traversal("/%2e%2e/../etc").is_err());
    }

    #[test]
    fn test_validate_path_for_traversal_rejects_mixed_encoding() {
        // Mixed literal and encoded
        assert!(validate_path_for_traversal("..%2fetc/passwd").is_err());
        assert!(validate_path_for_traversal("..%2Fetc/passwd").is_err());
        assert!(validate_path_for_traversal("%2e%2e/secret").is_err());
        assert!(validate_path_for_traversal("test/..%5cwindows").is_err()); // backslash
    }

    #[test]
    fn test_validate_path_for_traversal_rejects_backslash_traversal() {
        assert!(validate_path_for_traversal("..\\..\\windows").is_err());
        assert!(validate_path_for_traversal("test\\..\\config").is_err());
        assert!(validate_path_for_traversal("..\\secret").is_err());
        assert!(validate_path_for_traversal("\\..\\windows").is_err());
    }

    #[test]
    fn test_validate_path_for_traversal_rejects_mixed_dot_sequences() {
        assert!(validate_path_for_traversal("./../etc").is_err());
        assert!(validate_path_for_traversal(".././secret").is_err());
        assert!(validate_path_for_traversal("././../config").is_err());
        assert!(validate_path_for_traversal("./.././etc").is_err());
    }

    #[test]
    fn test_validate_path_for_traversal_rejects_null_bytes() {
        assert!(validate_path_for_traversal("test\0../etc").is_err());
        assert!(validate_path_for_traversal("/etc/\0passwd").is_err());
        assert!(validate_path_for_traversal("test\0file").is_err());
    }

    #[test]
    fn test_validate_path_for_traversal_allows_valid_paths() {
        assert!(validate_path_for_traversal("media/movies").is_ok());
        assert!(validate_path_for_traversal("/absolute/path").is_ok());
        assert!(validate_path_for_traversal("folder with spaces/file.txt").is_ok());
        assert!(validate_path_for_traversal("file-with-dashes.txt").is_ok());
        assert!(validate_path_for_traversal("file_with_underscores.txt").is_ok());
        assert!(validate_path_for_traversal("single.dot").is_ok()); // single dot is ok
        assert!(validate_path_for_traversal("file.tar.gz").is_ok()); // dots in filename
        assert!(validate_path_for_traversal("/path/with.dots/in/middle").is_ok());
        assert!(validate_path_for_traversal("日本語/ファイル").is_ok()); // Unicode
    }

    #[test]
    fn test_validate_path_for_traversal_edge_cases() {
        // Empty path is technically safe (no traversal)
        assert!(validate_path_for_traversal("").is_ok());

        // Single slash
        assert!(validate_path_for_traversal("/").is_ok());

        // Multiple slashes (no traversal)
        assert!(validate_path_for_traversal("path//to//file").is_ok());

        // Trailing slash
        assert!(validate_path_for_traversal("path/to/file/").is_ok());

        // Leading slash
        assert!(validate_path_for_traversal("/leading/slash").is_ok());
    }

    #[test]
    fn test_validate_path_for_traversal_double_url_encoding() {
        // Double-encoded . (%252E = %2E = .)
        // Our simplified check rejects any URL-encoded dot, so these are caught
        assert!(validate_path_for_traversal("%252e%252e/secret").is_err());
        assert!(validate_path_for_traversal("%252E%252E/secret").is_err());
        assert!(validate_path_for_traversal("%252e%252e").is_err());
    }

    #[test]
    fn test_validate_path_for_traversal_mixed_case_encoding() {
        // Any case variation of %2e is rejected
        assert!(validate_path_for_traversal("%2e%2E/secret").is_err());
        assert!(validate_path_for_traversal("%2E%2e/secret").is_err());
        assert!(validate_path_for_traversal("%2E%2E/secret").is_err());
    }

    #[test]
    fn test_validate_path_for_traversal_partial_encoding() {
        // Partial encoding attempts - all rejected
        assert!(validate_path_for_traversal(".%2e/secret").is_err());
        assert!(validate_path_for_traversal("%2e./secret").is_err());
        assert!(validate_path_for_traversal(".%2E/secret").is_err());
    }

    #[test]
    fn test_validate_path_for_traversal_any_url_encoded_dot() {
        // Any URL-encoded dot is rejected as it could be part of an attack
        assert!(validate_path_for_traversal("file%2eext").is_err());
        assert!(validate_path_for_traversal("%2ext").is_err());
        assert!(validate_path_for_traversal("t%2ext").is_err());
    }
}
