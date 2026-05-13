pub mod connection_manager;
pub mod dedup;
pub mod events;
pub mod realtime_manager;
pub mod redis_pubsub;
pub mod room_hub;
pub mod runtime;
pub mod transport;

use std::sync::Arc;

use synctv_core::RedisCoordinationRuntime;

pub use connection_manager::{
    ConnectionInfo, ConnectionLimits, ConnectionManager, ConnectionMetrics, DisconnectSignal,
    DisconnectSignalMetrics,
};
pub use dedup::{DedupKey, MessageDeduplicator};
pub use events::{
    CacheTarget, NotificationLevel, RealtimeDeliveryRoute, RealtimeEvent, WebRTCSignalKind,
};
pub use realtime_manager::{BroadcastResult, RealtimeConfig, RealtimeManager, RealtimeMetrics};
pub use redis_pubsub::{PublishRequest, RedisPubSub, RedisRealtimeMessageTransportFactory};
pub use room_hub::{ConnectionId, MessageSender, RoomLifecycleEvent, RoomMessageHub, Subscriber};
pub use runtime::{
    build_connection_manager, build_connection_runtime, build_room_message_runtime,
    ConnectionRuntime, RoomMessageRuntime,
};
pub use transport::{
    RealtimeEventHandler, RealtimeMessageTransport, RealtimeMessageTransportConfig,
    RealtimeMessageTransportFactory, RealtimeMessageTransportRuntime,
};

#[must_use]
pub fn build_realtime_message_transport_factory(
    runtime: Arc<dyn RedisCoordinationRuntime>,
) -> Arc<dyn RealtimeMessageTransportFactory> {
    Arc::new(redis_pubsub::RedisRealtimeMessageTransportFactory::new(
        runtime,
    ))
}
