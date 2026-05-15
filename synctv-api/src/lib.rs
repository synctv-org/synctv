// SyncTV API Library
// Provides gRPC and HTTP API services for SyncTV
#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub mod client_ip;
pub mod config_adapters;
pub mod fanout;
pub mod grpc;
pub mod grpc_support;
pub mod http;
pub mod impls;
mod media_fanout;
mod membership_event_fanout;
pub mod observability;
#[cfg(feature = "openapi")]
pub mod openapi;
mod playlist_fanout;
pub mod proto;
pub mod realtime_fanout;
mod realtime_lifecycle;
mod resource_change;
mod room_cache_fanout;
mod room_lifecycle_fanout;
pub mod runtime;
pub mod webrtc_status;

#[doc(hidden)]
pub mod test_support {
    pub use crate::realtime_fanout::channel_realtime_fanout_service;
}

// Re-export commonly used types
pub use http::AppState;
pub use synctv_core::PublicIdCodec;

/// Shared Redis connection handle that supports Sentinel failover.
///
/// After a Redis Sentinel failover, the background health check replaces the
/// `ConnectionManager` inside the `RwLock` with one connected to the new master.
/// Components holding this type can obtain a fresh `ConnectionManager` via
/// `.read().await.clone()` on each operation, ensuring they always talk to the
/// current master.
pub type SharedRedisConn = std::sync::Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>;
