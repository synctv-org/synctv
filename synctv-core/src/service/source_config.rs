use crate::{Error, Result};
use serde_json::Value as JsonValue;

/// Limit provider-owned source_config storage to prevent unbounded JSONB growth.
pub(crate) const MAX_SOURCE_CONFIG_SIZE: usize = 1024 * 1024;

pub(crate) fn validate_source_config_size(source_config: &JsonValue) -> Result<()> {
    let config_size = serde_json::to_vec(source_config)?.len();
    if config_size > MAX_SOURCE_CONFIG_SIZE {
        return Err(Error::InvalidInput(format!(
            "source_config too large: {config_size} bytes (max {MAX_SOURCE_CONFIG_SIZE} bytes / 1MB)"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_config_error(source_config: &JsonValue) -> Error {
        match validate_source_config_size(source_config) {
            Ok(()) => std::panic::panic_any("source_config validation should fail"),
            Err(error) => error,
        }
    }

    fn assert_source_config_valid(source_config: &JsonValue) {
        if let Err(error) = validate_source_config_size(source_config) {
            std::panic::panic_any(format!("source_config validation should pass: {error}"));
        }
    }

    fn assert_source_config_too_large(error: Error) {
        match error {
            Error::InvalidInput(message) => {
                assert!(message.contains("source_config too large"));
                assert!(message.contains(&MAX_SOURCE_CONFIG_SIZE.to_string()));
            }
            other => std::panic::panic_any(format!("expected InvalidInput, got {other:?}")),
        }
    }

    #[test]
    fn test_source_config_large_rejection() {
        let large_string = "x".repeat(2 * 1024 * 1024);
        let large_config = serde_json::json!({
            "data": large_string
        });

        assert_source_config_too_large(source_config_error(&large_config));
    }

    #[test]
    fn test_source_config_exactly_1mb_accepted() {
        let empty_config_size = serialized_size(&serde_json::json!({ "data": "" }));
        let data_size = MAX_SOURCE_CONFIG_SIZE - empty_config_size;
        let exact_config = serde_json::json!({
            "data": "x".repeat(data_size)
        });

        assert_source_config_valid(&exact_config);
    }

    #[test]
    fn test_source_config_1mb_plus_one_rejected() {
        let empty_config_size = serialized_size(&serde_json::json!({ "data": "" }));
        let data_size = MAX_SOURCE_CONFIG_SIZE - empty_config_size + 1;
        let over_config = serde_json::json!({
            "data": "x".repeat(data_size)
        });

        assert_source_config_too_large(source_config_error(&over_config));
    }

    fn serialized_size(source_config: &JsonValue) -> usize {
        match serde_json::to_vec(source_config) {
            Ok(bytes) => bytes.len(),
            Err(error) => std::panic::panic_any(format!("source_config should serialize: {error}")),
        }
    }

    #[test]
    fn test_source_config_nested_structure_size() {
        let nested_config = serde_json::json!({
            "playback_infos": {
                "1080p": {
                    "urls": ["https://example.com/video1.mp4", "https://example.com/video2.mp4"],
                    "headers": {
                        "Referer": "https://example.com",
                        "User-Agent": "Mozilla/5.0"
                    }
                },
                "720p": {
                    "urls": ["https://example.com/video1-720.mp4"],
                    "headers": {}
                }
            },
            "default_mode": "1080p",
            "metadata": {
                "title": "Test Video",
                "duration": 3600
            }
        });

        assert_source_config_valid(&nested_config);
    }
}
