#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub mod error;
pub mod grpc;
pub mod sync;

pub use error::{Error, Result};
pub use sync::{
    build_connection_manager, build_connection_runtime, build_realtime_message_transport_factory,
    build_room_message_runtime, BroadcastResult, CacheTarget, ConnectionId, ConnectionInfo,
    ConnectionLimits, ConnectionManager, ConnectionMetrics, ConnectionRuntime, DedupKey,
    DisconnectSignal, DisconnectSignalMetrics, MessageDeduplicator, NotificationLevel,
    PublishRequest, RealtimeConfig, RealtimeEvent, RealtimeEventHandler, RealtimeManager,
    RealtimeMessageTransport, RealtimeMessageTransportConfig, RealtimeMessageTransportFactory,
    RealtimeMessageTransportRuntime, RealtimeMetrics, RedisPubSub,
    RedisRealtimeMessageTransportFactory, RoomLifecycleEvent, RoomMessageHub, RoomMessageRuntime,
    Subscriber, WebRTCSignalKind,
};
