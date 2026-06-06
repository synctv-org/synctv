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

    #[test]
    fn test_source_config_large_rejection() {
        let large_string = "x".repeat(2 * 1024 * 1024);
        let large_config = serde_json::json!({
            "data": large_string
        });

        let err = validate_source_config_size(&large_config).unwrap_err();

        match err {
            Error::InvalidInput(message) => {
                assert!(message.contains("source_config too large"));
                assert!(message.contains("1048576"));
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn test_source_config_exactly_1mb_accepted() {
        // JSON overhead: {"data":"..."} = 11 bytes when serialized compactly.
        let data_size = MAX_SOURCE_CONFIG_SIZE - 11;
        let exact_config = serde_json::json!({
            "data": "x".repeat(data_size)
        });

        validate_source_config_size(&exact_config).unwrap();
    }

    #[test]
    fn test_source_config_1mb_plus_one_rejected() {
        let data_size = MAX_SOURCE_CONFIG_SIZE - 10;
        let over_config = serde_json::json!({
            "data": "x".repeat(data_size)
        });

        let err = validate_source_config_size(&over_config).unwrap_err();

        match err {
            Error::InvalidInput(message) => assert!(message.contains("source_config too large")),
            other => panic!("expected InvalidInput, got {other:?}"),
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

        validate_source_config_size(&nested_config).unwrap();
    }
}
