//! Input validation
//!
//! This module provides input validation for usernames, passwords, emails,
//! room names, and path traversal protection.

use ammonia::{Builder, UrlRelative};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

// Canonical validation limits — single source of truth for the entire codebase

/// Minimum username length
pub const USERNAME_MIN: usize = 3;
/// Maximum username length
pub const USERNAME_MAX: usize = 50;

/// Minimum user-account password length
pub const PASSWORD_MIN: usize = 8;
/// Maximum password length accepted by credential flows.
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

/// Maximum room description length enforced by the domain/service layer.
pub const ROOM_DESCRIPTION_MAX: usize = 500;

/// Maximum media display name length enforced by request/domain validation.
pub const MEDIA_NAME_MAX: usize = 500;
/// Maximum media description length enforced by the domain/service layer.
pub const MEDIA_DESCRIPTION_MAX: usize = 5000;
/// Maximum media items allowed at a single room root or playlist location.
pub const MEDIA_PLAYLIST_MAX_ITEMS: usize = 1000;

/// Maximum playlist name length enforced by the domain/service layer.
pub const PLAYLIST_NAME_MAX: usize = 255;
/// Maximum playlist description length enforced by the domain/service layer.
pub const PLAYLIST_DESCRIPTION_MAX: usize = 5000;

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

    #[error("Potential security issue detected in input")]
    SecurityRisk,

    #[error("Multiple validation errors: {0}")]
    Multiple(String),
}

/// Validation result
pub type ValidationResult<T> = Result<T, ValidationError>;

static PLAIN_TEXT_CLEANER: LazyLock<Builder<'static>> = LazyLock::new(|| {
    let mut cleaner = Builder::default();
    cleaner
        .tags(HashSet::new())
        .tag_attributes(HashMap::new())
        .generic_attributes(HashSet::new())
        .url_relative(UrlRelative::Deny);
    cleaner
});

fn sanitize_string(input: &str) -> Cow<'_, str> {
    let trimmed = input.trim();
    if !trimmed.chars().any(is_disallowed_control_char) {
        return Cow::Borrowed(trimmed);
    }

    Cow::Owned(
        trimmed
            .chars()
            .filter(|ch| !is_disallowed_control_char(*ch))
            .collect(),
    )
}

fn is_disallowed_control_char(ch: char) -> bool {
    matches!(ch, '\u{0000}'..='\u{0008}' | '\u{000B}' | '\u{000C}' | '\u{000E}'..='\u{001F}' | '\u{007F}')
}

fn contains_html_markup(input: &str) -> bool {
    if !input.contains(['<', '>']) {
        return false;
    }
    PLAIN_TEXT_CLEANER.clean(input).to_string() != input
}

fn input_field_error(field: &'static str, error: &ValidationError) -> ValidationError {
    ValidationError::Field {
        field: field.to_string(),
        message: error.to_string(),
    }
}

pub fn validate_room_name_input(name: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(name);
    RoomNameValidator::new()
        .validate(&sanitized)
        .map_err(|error| input_field_error("room_name", &error))?;

    if contains_html_markup(&sanitized) {
        return Err(ValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

pub fn validate_room_description_input(description: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(description);
    validate_room_description(&sanitized)
        .map_err(|error| input_field_error("room_description", &error))?;

    if contains_html_markup(&sanitized) {
        return Err(ValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

pub fn validate_media_name_input(name: &str) -> ValidationResult<String> {
    let sanitized = sanitize_string(name);
    validate_media_name(&sanitized).map_err(|error| input_field_error("media_name", &error))?;

    if contains_html_markup(&sanitized) {
        return Err(ValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

pub fn validate_media_name(name: &str) -> ValidationResult<()> {
    let char_count = name.chars().count();
    if char_count > MEDIA_NAME_MAX {
        return Err(ValidationError::Field {
            field: "media_name".to_string(),
            message: format!("must be at most {MEDIA_NAME_MAX} characters"),
        });
    }

    Ok(())
}

pub fn validate_room_description(description: &str) -> ValidationResult<()> {
    let char_count = description.chars().count();
    if char_count > ROOM_DESCRIPTION_MAX {
        return Err(ValidationError::Field {
            field: "room_description".to_string(),
            message: format!("must be at most {ROOM_DESCRIPTION_MAX} characters"),
        });
    }

    Ok(())
}

pub fn validate_room_password_for_set(password: &str) -> ValidationResult<()> {
    let char_count = password.trim().chars().count();
    if char_count < ROOM_PASSWORD_MIN {
        return Err(ValidationError::Field {
            field: "room_password".to_string(),
            message: format!("must be at least {ROOM_PASSWORD_MIN} characters"),
        });
    }
    if char_count > ROOM_PASSWORD_MAX {
        return Err(ValidationError::Field {
            field: "room_password".to_string(),
            message: format!("must not exceed {ROOM_PASSWORD_MAX} characters"),
        });
    }

    Ok(())
}

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
    zxcvbn_enabled: bool,
    zxcvbn_min_score: u8,
}

#[derive(Debug, Clone)]
pub struct PasswordComplexityOptions {
    pub min_length: usize,
    pub require_uppercase: bool,
    pub require_lowercase: bool,
    pub require_digit: bool,
    pub require_special: bool,
    pub max_repeated_chars: usize,
    pub zxcvbn_enabled: bool,
    pub zxcvbn_min_score: u8,
}

impl Default for PasswordComplexityOptions {
    fn default() -> Self {
        Self {
            min_length: 8,
            require_uppercase: true,
            require_lowercase: true,
            require_digit: true,
            require_special: false,
            max_repeated_chars: 3,
            zxcvbn_enabled: false,
            zxcvbn_min_score: 3,
        }
    }
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
            zxcvbn_enabled: false,
            zxcvbn_min_score: 3,
        }
    }
}

impl PasswordValidator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a `PasswordValidator` from explicit password complexity options.
    #[must_use]
    pub const fn from_options(options: &PasswordComplexityOptions) -> Self {
        Self {
            min_length: options.min_length,
            require_uppercase: options.require_uppercase,
            require_lowercase: options.require_lowercase,
            require_digit: options.require_digit,
            require_special_char: options.require_special,
            max_repeated_chars: options.max_repeated_chars,
            zxcvbn_enabled: options.zxcvbn_enabled,
            zxcvbn_min_score: options.zxcvbn_min_score,
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

    #[must_use]
    pub const fn with_zxcvbn(mut self, enabled: bool, min_score: u8) -> Self {
        self.zxcvbn_enabled = enabled;
        self.zxcvbn_min_score = min_score;
        self
    }

    /// Maximum password length accepted by credential flows.
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

        if self.zxcvbn_enabled {
            let entropy = zxcvbn::zxcvbn(password, &[]);
            let score = u8::from(entropy.score());
            if score < self.zxcvbn_min_score {
                return Err(ValidationError::Field {
                    field: "password".to_string(),
                    message: format!(
                        "is too weak according to zxcvbn score {score}; minimum required score is {}",
                        self.zxcvbn_min_score
                    ),
                });
            }
        }

        Ok(())
    }
}

/// Pre-compiled email validation regex
static EMAIL_REGEX: LazyLock<Result<regex::Regex, regex::Error>> =
    LazyLock::new(|| regex::Regex::new(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$"));

/// Email validator
#[derive(Default)]
pub struct EmailValidator {}

impl EmailValidator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn validate(&self, email: &str) -> ValidationResult<()> {
        let regex = EMAIL_REGEX
            .as_ref()
            .map_err(|error| ValidationError::Field {
                field: "email".to_string(),
                message: format!("email validator is not available: {error}"),
            })?;
        if !regex.is_match(email) {
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

    fn result_err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => std::panic::panic_any(context.to_string()),
            Err(error) => error,
        }
    }

    #[test]
    fn test_media_name_validation_uses_character_count() {
        assert!(validate_media_name(&"a".repeat(MEDIA_NAME_MAX)).is_ok());
        assert!(validate_media_name(&"\u{4e00}".repeat(MEDIA_NAME_MAX)).is_ok());

        let result = validate_media_name(&"a".repeat(MEDIA_NAME_MAX + 1));
        assert!(matches!(
            result,
            Err(ValidationError::Field { ref field, .. }) if field == "media_name"
        ));
    }

    #[test]
    fn test_media_name_input_accepts_plain_text_ampersands() {
        let title = "ROSÉ & Bruno Mars - APT. (Official Music Video)";
        assert!(matches!(
            validate_media_name_input(title),
            Ok(ref validated) if validated == title
        ));
    }

    #[test]
    fn test_room_description_validation_uses_character_count() {
        assert!(validate_room_description(&"a".repeat(ROOM_DESCRIPTION_MAX)).is_ok());
        assert!(validate_room_description(&"\u{4e00}".repeat(ROOM_DESCRIPTION_MAX)).is_ok());

        let result = validate_room_description(&"a".repeat(ROOM_DESCRIPTION_MAX + 1));
        assert!(matches!(
            result,
            Err(ValidationError::Field { ref field, .. }) if field == "room_description"
        ));
    }

    #[test]
    fn test_room_password_set_validation_counts_trimmed_length() {
        assert!(validate_room_password_for_set(" abcd ").is_ok());
        assert!(validate_room_password_for_set(&"a".repeat(ROOM_PASSWORD_MAX)).is_ok());

        let too_short = validate_room_password_for_set(" abc ");
        assert!(matches!(
            too_short,
            Err(ValidationError::Field { ref field, .. }) if field == "room_password"
        ));

        let too_long = validate_room_password_for_set(&"a".repeat(ROOM_PASSWORD_MAX + 1));
        assert!(matches!(
            too_long,
            Err(ValidationError::Field { ref field, .. }) if field == "room_password"
        ));
    }

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
        assert!(validator.validate("RoOt").is_err()); // Reserved names are case-insensitive
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
    fn test_password_from_options() {
        let options = PasswordComplexityOptions {
            min_length: 10,
            require_uppercase: false,
            require_lowercase: true,
            require_digit: true,
            require_special: true,
            max_repeated_chars: 2,
            zxcvbn_enabled: false,
            zxcvbn_min_score: 3,
        };
        let validator = PasswordValidator::from_options(&options);

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
    fn test_password_zxcvbn_is_opt_in() {
        let default_validator = PasswordValidator::new();
        assert!(default_validator.validate("Password123").is_ok());

        let zxcvbn_validator = PasswordValidator::new().with_zxcvbn(true, 3);
        assert!(zxcvbn_validator.validate("Password123").is_err());
        assert!(zxcvbn_validator
            .validate("CorrectHorseBatteryStaple123")
            .is_ok());
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
        let err = result_err(validator.validate("ab"), "short username should fail");
        assert!(err.to_string().contains("username"));
        assert!(err.to_string().contains('3'));

        let validator = PasswordValidator::new();
        let err = result_err(validator.validate("weak"), "weak password should fail");
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
            _ => std::panic::panic_any("Expected Multiple errors"),
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
            _ => std::panic::panic_any("Expected Field error for single validation failure"),
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
        assert!(validate_path_for_traversal("unicode-file/résumé.txt").is_ok());
        // Unicode
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
