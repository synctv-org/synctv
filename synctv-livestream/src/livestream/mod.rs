// Livestream server orchestration
// Main application layer that coordinates all protocols and libraries.
// Follows xiu's application/xiu architecture.

pub(crate) mod external_publish_manager;
pub(crate) mod external_puller;
pub(crate) mod managed_stream;
pub(crate) mod pull_manager;
pub(crate) mod pull_stream;
pub(crate) mod server;

pub(crate) use synctv_xiu::hls::segment_manager::{CleanupConfig, SegmentManager};
