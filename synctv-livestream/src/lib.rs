// synctv-livestream - Live streaming infrastructure for SyncTV
//
// Architecture (following xiu's modular design):
// - protocols/   - Protocol implementations (RTMP, HTTP-FLV, HLS)
// - libraries/    - Shared components (GOP cache, storage, etc.)
// - api/         - Public API for synctv-api
// - relay/       - Multi-node streaming (Publisher/Puller)
// - src/         - Server orchestration (application layer)
//
// All streams are scoped to room_id:media_id (media-level streaming).

pub mod grpc;
pub mod error;
pub mod libraries;
pub mod protocols;
pub mod relay;
pub mod api;
pub mod livestream;
pub mod util;

// Re-exports for convenience
pub use libraries::storage::HlsStorage;
pub use protocols::httpflv::HttpFlvSession;
pub use protocols::hls::{CustomHlsRemuxer, StreamRegistry};
pub use protocols::rtmp::RtmpAuthCallbackImpl;
pub use api::{LiveStreamingInfrastructure, FlvStreamingApi, HlsStreamingApi};
pub use livestream::{LivestreamServer, LivestreamConfig, LivestreamHandle, PullStreamManager, SegmentManager};
pub use synctv_xiu::rtmp::auth::{AuthCallback, AuthPublishRewrite};

/// Re-export auth types for use in downstream crates (e.g., synctv/src/rtmp_auth)
pub mod rtmp_auth {
    pub use synctv_xiu::rtmp::auth::AuthPublishRewrite;
}
