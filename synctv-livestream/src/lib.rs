#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive — use only one");

// synctv-livestream - Live streaming infrastructure for SyncTV
// Architecture (following xiu's modular design):
// - protocols/   - Protocol implementations (RTMP, HTTP-FLV, HLS)
// - libraries/    - Shared components (GOP cache, storage, etc.)
// - api/         - Public API for synctv-api
// - relay/       - Multi-node streaming (Publisher/Puller)
// - src/         - Server orchestration (application layer)
// All streams are scoped to room_id:media_id (media-level streaming).

/// Encoded file descriptor set for livestream gRPC proto definitions.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("descriptor.bin");

pub mod api;
pub mod error;
pub mod grpc;
pub mod libraries;
pub mod livestream;
pub mod protocols;
pub mod relay;
pub mod util;

// Re-exports for convenience
pub use api::{FlvStreamingApi, HlsStreamingApi, LiveStreamingInfrastructure};
pub use libraries::storage::HlsStorage;
pub use livestream::{
    LivestreamConfig, LivestreamHandle, LivestreamServer, PullStreamManager, SegmentManager,
};
pub use protocols::hls::{CustomHlsRemuxer, StreamRegistry};
pub use protocols::httpflv::HttpFlvSession;
pub use synctv_xiu::rtmp::auth::{AuthCallback, AuthPublishRewrite};

/// Re-export auth types for use in downstream crates (e.g., `synctv/src/rtmp_auth`)
pub mod rtmp_auth {
    pub use synctv_xiu::rtmp::auth::AuthPublishRewrite;
}
