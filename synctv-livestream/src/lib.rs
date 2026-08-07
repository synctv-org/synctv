#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub(crate) mod api;
pub(crate) mod error;
pub(crate) mod grpc;
pub(crate) mod livestream;
pub(crate) mod relay;
pub(crate) mod util;

// Re-exports for convenience
pub use api::livestream::{FlvStreamingApi, HlsStreamingApi, LiveStreamingInfrastructure};
pub use api::tracker::StreamTracker;
pub use error::StreamError;
pub use grpc::{StreamRelayServiceImpl, StreamRelayServiceServer};
pub use livestream::server::{
    HlsS3Options, HlsStorageBackend, LivestreamConfig, LivestreamHandle, LivestreamServer,
};
pub use relay::{
    local_stream_registry, shared_stream_registry, ActiveStreamGeneration, LeaseRefreshOutcome,
    PublisherControlHandle, PublisherStopOutcome, PublisherStopRequest, RegistryConnectionRuntime,
    StreamGeneration, StreamLifecycleEvent, StreamRegistryTrait, PUBLISHER_TTL_SECS,
};
