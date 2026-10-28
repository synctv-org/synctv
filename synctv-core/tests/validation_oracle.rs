//! Equivalence oracle for `synctv_core::validation`.
//!
//! Contains a verbatim snapshot of the pre-optimization implementation
//! (frozen at the baseline commit) and compares it against the live
//! implementation on a deterministic corpus of valid, invalid, worst-case,
//! and adversarial inputs. Optimizations to `validation.rs` must keep this
//! oracle green: identical Ok values and identical error `Display` strings.

#![allow(clippy::unwrap_used)]

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Frozen snapshot of the original implementation (do not optimize this copy;
// it exists purely as the behavioral reference).
// ---------------------------------------------------------------------------

mod reference {
    use super::*;

    #[derive(Debug, Clone, thiserror::Error)]
    pub enum ValidationError {
        #[error("Invalid {field}: {message}")]
        Field { field: String, message: String },

        #[error("Potential security issue detected in input")]
        SecurityRisk,

        #[error("Multiple validation errors: {0}")]
        Multiple(String),
    }

    pub const USERNAME_MIN: usize = 3;
    pub const USERNAME_MAX: usize = 50;
    pub const PASSWORD_MIN: usize = 8;
    pub const PASSWORD_MAX: usize = 128;
    pub const ROOM_NAME_MAX: usize = 100;
    pub const ROOM_DESCRIPTION_MAX: usize = 500;
    pub const MEDIA_NAME_MAX: usize = 500;
    pub const ROOM_PASSWORD_MIN: usize = 4;
    pub const ROOM_PASSWORD_MAX: usize = 128;

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

    static PLAIN_TEXT_CLEANER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
        let mut cleaner = ammonia::Builder::default();
        cleaner
            .tags(HashSet::new())
            .tag_attributes(HashMap::new())
            .generic_attributes(HashSet::new())
            .url_relative(ammonia::UrlRelative::Deny);
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
        matches!(
            ch,
            '\u{0000}'..='\u{0008}'
                | '\u{000B}'
                | '\u{000C}'
                | '\u{000E}'..='\u{001F}'
                | '\u{007F}'
        )
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

    pub fn validate_room_name_input(name: &str) -> Result<String, ValidationError> {
        let sanitized = sanitize_string(name);
        RoomNameValidator::new()
            .validate(&sanitized)
            .map_err(|error| input_field_error("room_name", &error))?;

        if contains_html_markup(&sanitized) {
            return Err(ValidationError::SecurityRisk);
        }

        Ok(sanitized.into_owned())
    }

    pub fn validate_room_description_input(description: &str) -> Result<String, ValidationError> {
        let sanitized = sanitize_string(description);
        validate_room_description(&sanitized)
            .map_err(|error| input_field_error("room_description", &error))?;

        if contains_html_markup(&sanitized) {
            return Err(ValidationError::SecurityRisk);
        }

        Ok(sanitized.into_owned())
    }

    pub fn validate_media_name_input(name: &str) -> Result<String, ValidationError> {
        let sanitized = sanitize_string(name);
        validate_media_name(&sanitized).map_err(|error| input_field_error("media_name", &error))?;

        if contains_html_markup(&sanitized) {
            return Err(ValidationError::SecurityRisk);
        }

        Ok(sanitized.into_owned())
    }

    pub fn validate_media_name(name: &str) -> Result<(), ValidationError> {
        let char_count = name.chars().count();
        if char_count > MEDIA_NAME_MAX {
            return Err(ValidationError::Field {
                field: "media_name".to_string(),
                message: format!("must be at most {MEDIA_NAME_MAX} characters"),
            });
        }

        Ok(())
    }

    pub fn validate_room_description(description: &str) -> Result<(), ValidationError> {
        let char_count = description.chars().count();
        if char_count > ROOM_DESCRIPTION_MAX {
            return Err(ValidationError::Field {
                field: "room_description".to_string(),
                message: format!("must be at most {ROOM_DESCRIPTION_MAX} characters"),
            });
        }

        Ok(())
    }

    pub fn validate_room_password_for_set(password: &str) -> Result<(), ValidationError> {
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

        pub fn validate(&self, username: &str) -> Result<(), ValidationError> {
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

            if !username
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                return Err(ValidationError::Field {
                    field: "username".to_string(),
                    message: "can only contain letters, numbers, underscores, and hyphens"
                        .to_string(),
                });
            }

            if let Some(first_char) = username.chars().next() {
                if first_char == '_' || first_char == '-' {
                    return Err(ValidationError::Field {
                        field: "username".to_string(),
                        message: "cannot start with underscore or hyphen".to_string(),
                    });
                }
            }

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

    pub struct PasswordValidator {
        min_length: usize,
        require_uppercase: bool,
        require_lowercase: bool,
        require_digit: bool,
        require_special_char: bool,
        max_repeated_chars: usize,
        zxcvbn_enabled: bool,
        zxcvbn_min_score: u8,
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

        pub fn validate(&self, password: &str) -> Result<(), ValidationError> {
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

            if self.require_uppercase && !password.chars().any(char::is_uppercase) {
                return Err(ValidationError::Field {
                    field: "password".to_string(),
                    message: "must contain at least one uppercase letter".to_string(),
                });
            }

            if self.require_lowercase && !password.chars().any(char::is_lowercase) {
                return Err(ValidationError::Field {
                    field: "password".to_string(),
                    message: "must contain at least one lowercase letter".to_string(),
                });
            }

            if self.require_digit && !password.chars().any(|c| c.is_ascii_digit()) {
                return Err(ValidationError::Field {
                    field: "password".to_string(),
                    message: "must contain at least one digit".to_string(),
                });
            }

            if self.require_special_char && !password.chars().any(|c| !c.is_alphanumeric()) {
                return Err(ValidationError::Field {
                    field: "password".to_string(),
                    message: "must contain at least one special character".to_string(),
                });
            }

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

        const MAX_LENGTH: usize = PASSWORD_MAX;
    }

    static EMAIL_REGEX: LazyLock<Result<regex::Regex, regex::Error>> =
        LazyLock::new(|| regex::Regex::new(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$"));

    #[derive(Default)]
    pub struct EmailValidator {}

    impl EmailValidator {
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        pub fn validate(&self, email: &str) -> Result<(), ValidationError> {
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

        pub fn validate(&self, name: &str) -> Result<(), ValidationError> {
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

            if name.chars().any(char::is_control) {
                return Err(ValidationError::Field {
                    field: "room_name".to_string(),
                    message: "cannot contain control characters".to_string(),
                });
            }

            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Comparison harness
// ---------------------------------------------------------------------------

use reference as r;
use synctv_core::validation as l;

fn render<T: std::fmt::Debug>(result: Result<T, String>) -> String {
    match result {
        Ok(value) => format!("Ok({value:?})"),
        Err(err) => format!("Err({err})"),
    }
}

fn assert_equivalent(actual: Result<String, String>, expected: Result<String, String>, ctx: &str) {
    assert_eq!(
        render(actual),
        render(expected),
        "behavior divergence for input: {ctx}"
    );
}

fn err_string<E: std::fmt::Display>(res: Result<(), E>) -> Result<String, String> {
    res.map(|_| "ok".to_string()).map_err(|e| e.to_string())
}

#[test]
fn oracle_username_validator() {
    let cases: Vec<String> = vec![
        String::new(),
        "a".into(),
        "ab".into(),
        "abc".into(),
        "User-Name_123".into(),
        "alice".into(),
        "bob_123".into(),
        "charlie-test".into(),
        "_invalid".into(),
        "-invalid".into(),
        "invalid@name".into(),
        "with space".into(),
        "with.dot".into(),
        "josé".into(),
        "\u{4e00}\u{4e8c}\u{4e09}".into(),
        "RoOt".into(),
        "ADMIN".into(),
        "administrator".into(),
        "SyStEm".into(),
        "mod".into(),
        "MOD".into(),
        "team".into(),
        "\u{212a}ey_user".into(),       // Kelvin sign (lowercases to ASCII 'k')
        "\u{0130}admin".into(),         // uppercase dotted I (lowercases to 2 chars)
        "a".repeat(50),
        "a".repeat(51),
        "\u{4e00}".repeat(50),
        "\u{4e00}".repeat(51),
        format!("{}{}", "a".repeat(49), '@'),
        "trailing_".into(),
        "9start".into(),
        "Ünïcödé".into(),
    ];

    for case in &cases {
        assert_equivalent(
            err_string(l::UsernameValidator::new().validate(case)),
            err_string(r::UsernameValidator::new().validate(case)),
            &format!("username default {case:?}"),
        );
        assert_equivalent(
            err_string(
                l::UsernameValidator::new()
                    .with_length(2, 10)
                    .validate(case),
            ),
            err_string(
                r::UsernameValidator::new()
                    .with_length(2, 10)
                    .validate(case),
            ),
            &format!("username(2,10) {case:?}"),
        );
    }
}

#[test]
fn oracle_password_validator() {
    let cases: Vec<String> = vec![
        String::new(),
        "short".into(),
        "Password123".into(),
        "nouppercase123".into(),
        "NOLOWERCASE123".into(),
        "NoDigits".into(),
        "Paaass1w".into(),
        "Paaaass1".into(),
        "Paaaaaa1".into(),
        "Abcd1234".into(),
        "Ab1".repeat(42) + "Ab",
        "Ab1".repeat(43),
        "Abc".to_string() + &"x".repeat(125),
        format!("{}aaaa", "Ab1".repeat(41)),
        "\u{4e00}".repeat(125) + "Aa1",
        "日A1b".repeat(20),
        " padded123 ".into(),
        "Tabs\t123A".into(),
    ];

    for case in &cases {
        assert_equivalent(
            err_string(l::PasswordValidator::new().validate(case)),
            err_string(r::PasswordValidator::new().validate(case)),
            &format!("password default {case:?}"),
        );
        assert_equivalent(
            err_string(
                l::PasswordValidator::new()
                    .require_special_char(true)
                    .validate(case),
            ),
            err_string(
                r::PasswordValidator::new()
                    .require_special_char(true)
                    .validate(case),
            ),
            &format!("password special {case:?}"),
        );
        assert_equivalent(
            err_string(
                l::PasswordValidator::new()
                    .with_min_length(4)
                    .with_max_repeated_chars(0)
                    .validate(case),
            ),
            err_string(
                r::PasswordValidator::new()
                    .with_min_length(4)
                    .with_max_repeated_chars(0)
                    .validate(case),
            ),
            &format!("password relaxed {case:?}"),
        );
    }

    // zxcvbn opt-in path: slow, so exercise a small set only.
    for case in ["Password123", "CorrectHorseBatteryStaple123"] {
        assert_equivalent(
            err_string(
                l::PasswordValidator::new()
                    .with_zxcvbn(true, 3)
                    .validate(case),
            ),
            err_string(
                r::PasswordValidator::new()
                    .with_zxcvbn(true, 3)
                    .validate(case),
            ),
            &format!("password zxcvbn {case:?}"),
        );
    }
}

#[test]
fn oracle_email_validator() {
    let cases: Vec<String> = vec![
        String::new(),
        "a@b.co".into(),
        "user@example.com".into(),
        "user.name@example.co.uk".into(),
        "notanemail".into(),
        "@example.com".into(),
        "user@".into(),
        "user@.com".into(),
        "user@example".into(),
        "user@example.c".into(),
        "user@example.c0m".into(),
        "user+tag@example.com".into(),
        "user@sub.domain.example.com".into(),
        "a@b..cd".into(), // regex-legal: consecutive dots in domain
        "a@b.".into(),
        "a@.b.cd".into(),
        "a.b.c.d.e.f.g.h@i.j.k.l.mn.opq".into(),
        "user.example.dot.comuser.example.dot.comuser.example.dot.com".into(),
        "UPPER@EXAMPLE.COM".into(),
        "user@example.technology".into(),
        "us er@example.com".into(),
        "user@@example.com".into(),
        "user@exa_mple.com".into(),
        "us\x00er@example.com".into(),
        "usér@example.com".into(),
        "a@b.cd".into(),
        "a@b.c".into(),
        "a@-b.cd".into(),
        "a@b-.cd".into(),
        "a@b.c-".into(),
        "12345@6789.co".into(),
        "%@example.com".into(),
        "a@b.co\n".into(),
    ];

    for case in &cases {
        assert_equivalent(
            err_string(l::EmailValidator::new().validate(case)),
            err_string(r::EmailValidator::new().validate(case)),
            &format!("email {case:?}"),
        );
    }
}

#[test]
fn oracle_room_name_input_and_validator() {
    let cases: Vec<String> = vec![
        String::new(),
        " ".into(),
        "My Room".into(),
        "a".into(),
        "Room-123_Test".into(),
        "Room\x00Name".into(),
        "Room\nName".into(),
        "Room\rName".into(),
        "Room\x1fEnd".into(),
        "Room\x7fEnd".into(),
        "Room\u{85}End".into(),
        "\u{4e00}".repeat(100),
        "\u{4e00}".repeat(101),
        "a".repeat(100),
        "a".repeat(101),
        "  padded  ".into(),
        "ROSÉ & Bruno Mars - APT. (Official Music Video)".into(),
        "<b>x</b>".into(),
        "<script>alert(1)</script>".into(),
        "a < b".into(),
        "a > b".into(),
        ">".into(),
        "&bogus;".into(),
        "&lt;escaped&gt;".into(),
        "文字<b>混合</b>".into(),
        "<b>".repeat(12),
        format!("{}{}", "a".repeat(99), '\u{0007}'),
        "tab\tinside".into(),
        "a\u{000B}b".into(),
        "a\u{000C}b".into(),
    ];

    for case in &cases {
        assert_equivalent(
            l::validate_room_name_input(case)
                .map_err(|e| e.to_string()),
            r::validate_room_name_input(case)
                .map_err(|e| e.to_string()),
            &format!("room_name_input {case:?}"),
        );
        assert_equivalent(
            err_string(l::RoomNameValidator::new().validate(case)),
            err_string(r::RoomNameValidator::new().validate(case)),
            &format!("room_name {case:?}"),
        );
        assert_equivalent(
            err_string(
                l::RoomNameValidator::new()
                    .with_length(2, 10)
                    .validate(case),
            ),
            err_string(
                r::RoomNameValidator::new()
                    .with_length(2, 10)
                    .validate(case),
            ),
            &format!("room_name(2,10) {case:?}"),
        );
    }
}

#[test]
fn oracle_media_name_and_description_inputs() {
    let cases: Vec<String> = vec![
        String::new(),
        "Episode 01".into(),
        "a".repeat(500),
        "a".repeat(501),
        "\u{4e00}".repeat(500),
        "\u{4e00}".repeat(501),
        "<b>x</b>".repeat(100),
        "a".repeat(5000),
        "a".repeat(5001),
        "\u{4e00}".repeat(5000),
        "\u{4e00}".repeat(5001),
        "lorem ipsum dolor sit amet ".repeat(100),
        "  trimmed  ".into(),
        "a < b & c > d | ".repeat(6),
        "\u{0000}null".into(),
    ];

    for case in &cases {
        assert_equivalent(
            l::validate_media_name_input(case).map_err(|e| e.to_string()),
            r::validate_media_name_input(case).map_err(|e| e.to_string()),
            &format!("media_name_input {case:.40?}"),
        );
        assert_equivalent(
            err_string(l::validate_media_name(case)),
            err_string(r::validate_media_name(case)),
            &format!("media_name {case:.40?}"),
        );
    }

    for case in &cases {
        assert_equivalent(
            l::validate_room_description_input(case)
                .map_err(|e| e.to_string()),
            r::validate_room_description_input(case)
                .map_err(|e| e.to_string()),
            &format!("room_description_input {case:.40?}"),
        );
        assert_equivalent(
            err_string(l::validate_room_description(case)),
            err_string(r::validate_room_description(case)),
            &format!("room_description {case:.40?}"),
        );
    }
}

#[test]
fn oracle_room_password() {
    let cases: Vec<String> = vec![
        String::new(),
        " ".into(),
        " abc ".into(),
        " abcd ".into(),
        "abcd".into(),
        "a".repeat(128),
        "a".repeat(129),
        "\u{4e00}".repeat(128),
        "\u{4e00}".repeat(129),
        "  a".repeat(43),
    ];

    for case in &cases {
        assert_equivalent(
            err_string(l::validate_room_password_for_set(case)),
            err_string(r::validate_room_password_for_set(case)),
            &format!("room_password {case:.20?}"),
        );
    }
}
