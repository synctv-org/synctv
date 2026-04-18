//! Input validation
//!
//! This module provides input validation for usernames, passwords, emails,
//! room names, and path traversal protection.

use std::sync::LazyLock;

// Canonical validation limits — single source of truth for the entire codebase

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

// Reserved usernames — prevent phishing/impersonation attacks

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

// Path Traversal Validation

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
    synctv_common::validation::validate_path_for_traversal(path).map_err(|e| {
        ValidationError::Field {
            field: "path".to_string(),
            message: e.reason,
        }
    })
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
        assert!(validator.validate("josé").is_ok());

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
        assert!(validate_path_for_traversal("unicode-file/résumé.txt").is_ok()); // Unicode
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
