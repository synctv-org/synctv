// Livestream server orchestration
// Main application layer that coordinates all protocols and libraries.
// Follows xiu's application/xiu architecture.

pub mod external_publish_manager;
pub mod external_puller;
pub mod managed_stream;
pub mod pull_manager;
pub mod pull_stream;
pub mod server;

pub use external_publish_manager::ExternalPublishManager;
pub use pull_manager::PullStreamManager;
pub use server::{LivestreamConfig, LivestreamHandle, LivestreamServer};
pub use synctv_xiu::hls::segment_manager::{CleanupConfig, SegmentManager};

// Re-export from protocols
pub use crate::protocols::httpflv::HttpFlvSession;
