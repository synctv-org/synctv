use crate::{Error, Result};
use serde::Serialize;

/// Limit provider-owned source_config storage to prevent unbounded JSONB growth.
pub(crate) const MAX_SOURCE_CONFIG_SIZE: usize = 1024 * 1024;

pub(crate) fn validate_source_config_size<T: Serialize + ?Sized>(source_config: &T) -> Result<()> {
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
    use serde::Serialize;

    #[derive(Serialize)]
    struct SizeProbe {
        data: String,
    }

    #[derive(Serialize)]
    struct NestedProbe {
        medias: Vec<NestedMediaProbe>,
        default_media_index: usize,
        metadata: NestedMetadataProbe,
    }

    #[derive(Serialize)]
    struct NestedMediaProbe {
        name: &'static str,
        url: &'static str,
        headers: Vec<(&'static str, &'static str)>,
    }

    #[derive(Serialize)]
    struct NestedMetadataProbe {
        title: &'static str,
        duration: u64,
    }

    fn source_config_error<T: Serialize + ?Sized>(source_config: &T) -> Error {
        match validate_source_config_size(source_config) {
            Ok(()) => std::panic::panic_any("source_config validation should fail"),
            Err(error) => error,
        }
    }

    fn assert_source_config_valid<T: Serialize + ?Sized>(source_config: &T) {
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
        let large_config = SizeProbe {
            data: "x".repeat(2 * 1024 * 1024),
        };

        assert_source_config_too_large(source_config_error(&large_config));
    }

    #[test]
    fn test_source_config_exactly_1mb_accepted() {
        let empty_config_size = serialized_size(&SizeProbe {
            data: String::new(),
        });
        let data_size = MAX_SOURCE_CONFIG_SIZE - empty_config_size;
        let exact_config = SizeProbe {
            data: "x".repeat(data_size),
        };

        assert_source_config_valid(&exact_config);
    }

    #[test]
    fn test_source_config_1mb_plus_one_rejected() {
        let empty_config_size = serialized_size(&SizeProbe {
            data: String::new(),
        });
        let data_size = MAX_SOURCE_CONFIG_SIZE - empty_config_size + 1;
        let over_config = SizeProbe {
            data: "x".repeat(data_size),
        };

        assert_source_config_too_large(source_config_error(&over_config));
    }

    fn serialized_size<T: Serialize + ?Sized>(source_config: &T) -> usize {
        match serde_json::to_vec(source_config) {
            Ok(bytes) => bytes.len(),
            Err(error) => std::panic::panic_any(format!("source_config should serialize: {error}")),
        }
    }

    #[test]
    fn test_source_config_nested_structure_size() {
        let nested_config = NestedProbe {
            medias: vec![
                NestedMediaProbe {
                    name: "1080p",
                    url: "https://example.com/video1.mp4",
                    headers: vec![
                        ("Referer", "https://example.com"),
                        ("User-Agent", "Mozilla/5.0"),
                    ],
                },
                NestedMediaProbe {
                    name: "720p",
                    url: "https://example.com/video1-720.mp4",
                    headers: Vec::new(),
                },
            ],
            default_media_index: 0,
            metadata: NestedMetadataProbe {
                title: "Test Video",
                duration: 3600,
            },
        };

        assert_source_config_valid(&nested_config);
    }
}
