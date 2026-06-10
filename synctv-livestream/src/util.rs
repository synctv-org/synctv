//! Shared utilities for the livestream crate.

use rand::RngExt;
use std::future::Future;
use tokio::task::JoinHandle;

const MAX_STREAM_ID_COMPONENT_LEN: usize = 128;
const MAX_HLS_SEGMENT_NAME_LEN: usize = 256;
const MAX_HLS_SEGMENT_URL_PART_LEN: usize = 2048;

/// Validate a stream identifier component before it is used in internal
/// registry/cache keys.
pub(crate) fn validate_stream_id_component(component: &str, field: &str) -> anyhow::Result<()> {
    if component.is_empty() {
        return Err(anyhow::anyhow!("{field} must not be empty"));
    }
    if component.len() > MAX_STREAM_ID_COMPONENT_LEN {
        return Err(anyhow::anyhow!(
            "{field} exceeds maximum length of {MAX_STREAM_ID_COMPONENT_LEN} bytes"
        ));
    }
    if component.contains(':') {
        return Err(anyhow::anyhow!(
            "{field} must not contain ':' because stream keys use ':' as an internal delimiter"
        ));
    }
    if component.contains('/') || component.contains('\\') {
        return Err(anyhow::anyhow!("{field} must not contain path separators"));
    }
    if component.chars().any(char::is_control) {
        return Err(anyhow::anyhow!(
            "{field} must not contain control characters"
        ));
    }
    synctv_common::validation::validate_path_for_traversal(component)
        .map_err(|error| anyhow::anyhow!("invalid {field}: {error}"))?;
    Ok(())
}

/// Validate canonical room/media identifiers used by livestream internals.
pub(crate) fn validate_stream_ids(room_id: &str, media_id: &str) -> anyhow::Result<()> {
    validate_stream_id_component(room_id, "room_id")?;
    validate_stream_id_component(media_id, "media_id")?;
    Ok(())
}

/// Validate a HLS segment name before it is sent over gRPC or used in cache/storage keys.
pub fn validate_hls_segment_name(segment_name: &str) -> anyhow::Result<()> {
    if segment_name.is_empty() {
        return Err(anyhow::anyhow!("segment_name must not be empty"));
    }
    if segment_name.len() > MAX_HLS_SEGMENT_NAME_LEN {
        return Err(anyhow::anyhow!(
            "segment_name exceeds maximum length of {MAX_HLS_SEGMENT_NAME_LEN} bytes"
        ));
    }
    if segment_name.contains(':') {
        return Err(anyhow::anyhow!("segment_name must not contain ':'"));
    }
    if segment_name.contains('/') || segment_name.contains('\\') {
        return Err(anyhow::anyhow!(
            "segment_name must not contain path separators"
        ));
    }
    if segment_name.chars().any(char::is_control) {
        return Err(anyhow::anyhow!(
            "segment_name must not contain control characters"
        ));
    }
    synctv_common::validation::validate_path_for_traversal(segment_name)
        .map_err(|error| anyhow::anyhow!("invalid segment_name: {error}"))?;
    Ok(())
}

/// Validate the segment URL prefix embedded into remote HLS playlists.
pub(crate) fn validate_hls_segment_url_base(segment_url_base: &str) -> anyhow::Result<()> {
    validate_hls_segment_url_part(segment_url_base, "segment_url_base")
}

/// Validate the segment URL suffix embedded into remote HLS playlists.
pub(crate) fn validate_hls_segment_url_suffix(segment_url_suffix: &str) -> anyhow::Result<()> {
    validate_hls_segment_url_part(segment_url_suffix, "segment_url_suffix")
}

fn validate_hls_segment_url_part(value: &str, field: &str) -> anyhow::Result<()> {
    if value.len() > MAX_HLS_SEGMENT_URL_PART_LEN {
        return Err(anyhow::anyhow!(
            "{field} exceeds maximum length of {MAX_HLS_SEGMENT_URL_PART_LEN} bytes"
        ));
    }
    if value.contains('\r') || value.contains('\n') {
        return Err(anyhow::anyhow!("{field} must not contain CR/LF characters"));
    }
    if value.contains('\0') {
        return Err(anyhow::anyhow!("{field} must not contain null bytes"));
    }
    Ok(())
}

/// Exponential backoff with jitter.
///
/// Delays for `initial_ms * 2^(attempt-1)` capped at `max_ms`, with +/- 25% jitter
/// to prevent thundering herd on retry storms.
pub(crate) async fn backoff(attempt: u32, initial_ms: u64, max_ms: u64) {
    let base = initial_ms.saturating_mul(1u64 << attempt.min(16));
    let capped = base.min(max_ms);
    // Add jitter: +/- 25% using proper RNG
    let jitter_range = capped / 4;
    let random_offset = if jitter_range > 0 {
        rand::rng().random_range(0..=(jitter_range * 2))
    } else {
        0
    };
    let delay = (capped.saturating_sub(jitter_range) + random_offset).min(max_ms);
    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
}

/// Best-effort spawn that does nothing when no Tokio runtime is available.
///
/// This is intended for `Drop` paths and other fire-and-forget cleanup where
/// panicking during runtime teardown would be worse than skipping async cleanup.
pub(crate) fn try_spawn<F>(future: F) -> Option<JoinHandle<F::Output>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::runtime::Handle::try_current()
        .ok()
        .map(|handle| handle.spawn(future))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    #[test]
    fn try_spawn_returns_none_without_runtime() {
        let result = try_spawn(async {});
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn try_spawn_spawns_when_runtime_exists() -> TestResult {
        let Some(handle) = try_spawn(async { 42 }) else {
            return Err(test_error("runtime should be available"));
        };
        assert_eq!(handle.await?, 42);
        Ok(())
    }

    #[test]
    fn validate_stream_ids_rejects_ambiguous_delimiters_and_paths() {
        assert!(validate_stream_ids("room1", "media-1").is_ok());
        assert!(validate_stream_ids("room:1", "media").is_err());
        assert!(validate_stream_ids("room", "media:1").is_err());
        assert!(validate_stream_ids("room/1", "media").is_err());
        assert!(validate_stream_ids("room", "../media").is_err());
    }

    #[test]
    fn validate_hls_segment_inputs_reject_playlist_or_path_injection() {
        assert!(validate_hls_segment_name("segment_001").is_ok());
        assert!(validate_hls_segment_name("segment_001.ts").is_ok());
        assert!(validate_hls_segment_name("../secret").is_err());
        assert!(validate_hls_segment_name("seg:001").is_err());
        assert!(validate_hls_segment_url_base("/api/live/segment/").is_ok());
        assert!(validate_hls_segment_url_base("/api/live/\n#EXT-X-ENDLIST").is_err());
        assert!(validate_hls_segment_url_suffix(".png?sig=abc").is_ok());
        assert!(validate_hls_segment_url_suffix(".ts\r\n#EXT-X-ENDLIST").is_err());
    }
}
