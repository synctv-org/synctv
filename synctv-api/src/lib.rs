#![recursion_limit = "256"]

// SyncTV API Library
// Provides gRPC and HTTP API services for SyncTV

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub mod api_error_model;
pub(crate) mod chat_event_dispatcher;
pub(crate) mod client_ip;
pub mod config_adapters;
pub(crate) mod fanout;
pub mod grpc;
pub mod grpc_support;
pub mod http;
pub mod impls;
mod media_fanout;
mod membership_event_fanout;
pub(crate) mod observability;
#[cfg(feature = "openapi")]
pub mod openapi;
mod playback_fanout;
mod playlist_fanout;
pub mod realtime_fanout;
mod realtime_lifecycle;
mod resource_change;
mod room_cache_fanout;
mod room_lifecycle_fanout;
pub mod runtime;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod webrtc_status;
