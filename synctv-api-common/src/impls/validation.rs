use ammonia::{Builder, UrlRelative};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

#[derive(Debug, Clone, thiserror::Error)]
pub enum InputValidationError {
    #[error("Invalid {field}: {message}")]
    Core {
        field: &'static str,
        message: String,
    },
    #[error("Potential security issue detected in input")]
    SecurityRisk,
}

pub type InputValidationResult<T> = Result<T, InputValidationError>;

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

fn map_core_validation_error(
    field: &'static str,
    error: &synctv_core::validation::ValidationError,
) -> InputValidationError {
    InputValidationError::Core {
        field,
        message: error.to_string(),
    }
}

pub fn validate_email(email: &str) -> InputValidationResult<String> {
    let sanitized = sanitize_string(email);
    synctv_core::validation::EmailValidator::new()
        .validate(&sanitized)
        .map_err(|error| map_core_validation_error("email", &error))?;
    Ok(sanitized.into_owned())
}

pub fn validate_username(username: &str) -> InputValidationResult<String> {
    let sanitized = sanitize_string(username);
    synctv_core::validation::UsernameValidator::new()
        .validate(&sanitized)
        .map_err(|error| map_core_validation_error("username", &error))?;
    Ok(sanitized.into_owned())
}

pub fn validate_login_username(username: &str) -> InputValidationResult<String> {
    let sanitized = sanitize_string(username);
    let char_count = sanitized.chars().count();
    if char_count < synctv_core::validation::USERNAME_MIN {
        return Err(InputValidationError::Core {
            field: "username",
            message: format!(
                "must be at least {} characters",
                synctv_core::validation::USERNAME_MIN
            ),
        });
    }
    if char_count > synctv_core::validation::USERNAME_MAX {
        return Err(InputValidationError::Core {
            field: "username",
            message: format!(
                "must be at most {} characters",
                synctv_core::validation::USERNAME_MAX
            ),
        });
    }
    if !sanitized
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(InputValidationError::Core {
            field: "username",
            message: "can only contain letters, numbers, underscores, and hyphens".to_string(),
        });
    }
    if matches!(sanitized.chars().next(), Some('_' | '-')) {
        return Err(InputValidationError::Core {
            field: "username",
            message: "cannot start with underscore or hyphen".to_string(),
        });
    }
    Ok(sanitized.into_owned())
}

#[cfg(test)]
pub fn validate_room_name(name: &str) -> InputValidationResult<String> {
    let sanitized = sanitize_string(name);
    synctv_core::validation::RoomNameValidator::new()
        .validate(&sanitized)
        .map_err(|error| map_core_validation_error("room_name", &error))?;

    if contains_html_markup(&sanitized) {
        return Err(InputValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

#[cfg(test)]
pub fn validate_room_description(description: &str) -> InputValidationResult<String> {
    let sanitized = sanitize_string(description);
    synctv_core::validation::validate_room_description(&sanitized)
        .map_err(|error| map_core_validation_error("room_description", &error))?;

    if contains_html_markup(&sanitized) {
        return Err(InputValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

pub fn validate_media_name(name: &str) -> InputValidationResult<String> {
    let sanitized = sanitize_string(name);
    synctv_core::validation::validate_media_name(&sanitized)
        .map_err(|error| map_core_validation_error("media_name", &error))?;

    if contains_html_markup(&sanitized) {
        return Err(InputValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

pub fn validate_playback_position(position: f64) -> InputValidationResult<f64> {
    if !position.is_finite() {
        return Err(InputValidationError::Core {
            field: "position",
            message: "must be a finite number".to_string(),
        });
    }
    if position < 0.0 {
        return Err(InputValidationError::Core {
            field: "position",
            message: "cannot be negative".to_string(),
        });
    }
    if position > 86_400.0 {
        return Err(InputValidationError::Core {
            field: "position",
            message: "must not exceed 24 hours".to_string(),
        });
    }
    Ok(position)
}

pub fn validate_playback_speed(speed: f64) -> InputValidationResult<f64> {
    if !speed.is_finite() {
        return Err(InputValidationError::Core {
            field: "speed",
            message: "must be a finite number".to_string(),
        });
    }
    if !(0.25..=4.0).contains(&speed) {
        return Err(InputValidationError::Core {
            field: "speed",
            message: "must be between 0.25 and 4.0".to_string(),
        });
    }
    Ok(speed)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn require_invalid<T>(
        result: Result<T, InputValidationError>,
        message: &'static str,
    ) -> TestResult<InputValidationError> {
        match result {
            Ok(_) => Err(test_error(message)),
            Err(error) => Ok(error),
        }
    }

    fn validation_ok<T>(result: Result<T, InputValidationError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    #[test]
    fn room_name_rejects_html_markup() -> TestResult {
        let error = require_invalid(
            validate_room_name("<b>watch party</b>"),
            "room name markup should fail validation",
        )?;
        assert!(matches!(error, InputValidationError::SecurityRisk));
        Ok(())
    }

    #[test]
    fn room_description_preserves_entity_encoded_text() -> TestResult {
        let description = validation_ok(validate_room_description(
            "&lt;script&gt;alert(1)&lt;/script&gt;",
        ))?;
        assert_eq!(description, "&lt;script&gt;alert(1)&lt;/script&gt;");
        Ok(())
    }

    #[test]
    fn media_name_rejects_markup_like_angle_brackets() -> TestResult {
        let error = require_invalid(
            validate_media_name("Episode < 10 > Preview"),
            "media name markup should fail validation",
        )?;
        assert!(matches!(error, InputValidationError::SecurityRisk));
        Ok(())
    }

    #[test]
    fn media_name_accepts_plain_text_ampersands() -> TestResult {
        let title = "ROSÉ & Bruno Mars - APT. (Official Music Video)";
        assert_eq!(validation_ok(validate_media_name(title))?, title);
        Ok(())
    }

    #[test]
    fn username_strips_disallowed_control_characters() -> TestResult {
        let username = validation_ok(validate_username("  alice\u{0007}  "))?;
        assert_eq!(username, "alice");
        Ok(())
    }
}
