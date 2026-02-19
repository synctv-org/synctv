pub mod remuxer;
pub mod segment_manager;

pub use remuxer::{ActivePublishersSource, CustomHlsRemuxer, PublisherActivityCallback, StreamRegistry, StreamProcessorState, SegmentInfo, HlsRemuxerError};
pub use segment_manager::{SegmentManager, CleanupConfig};

/// Generate a canonical HLS segment storage key from components.
///
/// Uses `/` as the separator between all parts, replacing any `:` in
/// `stream_name` with `/` to ensure a consistent hierarchical key
/// regardless of whether `stream_name` is passed as a single composite
/// value (e.g., `"room_id:media_id"`) or as a bare `media_id`.
///
/// Both the remuxer (write side) and the API (read side) **must** use
/// this function so that keys always match.
#[inline]
pub fn hls_segment_storage_key(app_name: &str, stream_name: &str, segment_name: &str) -> String {
    format!(
        "{}/{}/{}",
        app_name,
        stream_name.replace(':', "/"),
        segment_name,
    )
}

/// Generate a canonical HLS stream storage prefix from components.
///
/// Same normalization as [`hls_segment_storage_key`] but without the
/// segment name, ending with `/`. Used for prefix-based cleanup.
#[inline]
pub fn hls_stream_storage_prefix(app_name: &str, stream_name: &str) -> String {
    format!("{}/{}/", app_name, stream_name.replace(':', "/"))
}
