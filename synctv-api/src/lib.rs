// SyncTV API Library
//
// Provides gRPC and HTTP API services for SyncTV
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod client_ip;
pub mod cluster_fanout;
pub mod fanout;
pub mod grpc;
pub mod http;
pub mod impls;
pub mod observability;
#[cfg(feature = "openapi")]
pub mod openapi;
mod member_fanout;
mod membership_event_fanout;
mod media_fanout;
mod playlist_fanout;
mod realtime_lifecycle;
mod room_cache_fanout;
mod room_lifecycle_fanout;
pub mod proto;
pub mod runtime;

// Shared validation utilities
pub mod room_id_validation;

// Re-export commonly used types
pub use http::AppState;

/// Shared Redis connection handle that supports Sentinel failover.
///
/// After a Redis Sentinel failover, the background health check replaces the
/// `ConnectionManager` inside the `RwLock` with one connected to the new master.
/// Components holding this type can obtain a fresh `ConnectionManager` via
/// `.read().await.clone()` on each operation, ensuring they always talk to the
/// current master.
pub type SharedRedisConn = std::sync::Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>;
