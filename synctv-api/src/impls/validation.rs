use regex::Regex;
use std::borrow::Cow;
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

static CONTROL_CHARS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]").expect("Invalid control char regex")
});

static HTML_TAGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").expect("Invalid HTML regex"));

fn sanitize_string(input: &str) -> Cow<'_, str> {
    let trimmed = input.trim();
    CONTROL_CHARS.replace_all(trimmed, "")
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

pub fn validate_room_name(name: &str) -> InputValidationResult<String> {
    let sanitized = sanitize_string(name);
    synctv_core::validation::RoomNameValidator::new()
        .validate(&sanitized)
        .map_err(|error| map_core_validation_error("room_name", &error))?;

    if HTML_TAGS.is_match(&sanitized) {
        return Err(InputValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

pub fn validate_room_description(description: &str) -> InputValidationResult<String> {
    let sanitized = sanitize_string(description);
    let len = sanitized.chars().count();
    if len > synctv_core::validation::ROOM_DESCRIPTION_MAX {
        return Err(InputValidationError::Core {
            field: "room_description",
            message: format!(
                "must be at most {} characters",
                synctv_core::validation::ROOM_DESCRIPTION_MAX
            ),
        });
    }

    if HTML_TAGS.is_match(&sanitized) {
        return Err(InputValidationError::SecurityRisk);
    }

    Ok(sanitized.into_owned())
}

pub fn validate_media_name(name: &str) -> InputValidationResult<String> {
    let sanitized = sanitize_string(name);
    synctv_core::validation::validate_media_name(&sanitized)
        .map_err(|error| map_core_validation_error("media_name", &error))?;

    if HTML_TAGS.is_match(&sanitized) {
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

pub fn validate_websocket_connect_request(
    request: &synctv_proto::client::WebSocketConnectRequest,
) -> Result<(), crate::impls::ApiError> {
    crate::impls::validate_proto_request(request)
}
