mod backpressure;
mod connection_manager;
mod dedup;
mod events;
mod realtime_manager;
mod redis_pubsub;
mod room_hub;
mod runtime;
pub(crate) mod stream_id;
mod transport;

use std::sync::Arc;

use synctv_core::RedisCoordinationRuntime;

pub use backpressure::{BufferPressure, PublishBackpressure};
pub use connection_manager::{
    ConnectionInfo, ConnectionLimits, ConnectionLimitsOptions, ConnectionManager,
    ConnectionMetrics, ConnectionReservationError, DisconnectSignal, RoomDisconnectReason,
    VoiceRtcJoinOutcome,
};
pub use dedup::{DedupKey, MessageDeduplicator};
pub use realtime_manager::{
    BroadcastResult, RealtimeConfig, RealtimeManager, RealtimeManagerRuntime, RealtimeMetrics,
};
pub use redis_pubsub::{
    is_sentinel_failover_error, PublishRequest, RedisPubSub, RedisPubSubConfig,
    RedisRealtimeMessageTransportFactory,
};
pub use room_hub::{ConnectionId, RoomLifecycleEvent, RoomMessageHub, Subscriber};
pub use runtime::{
    build_connection_manager, build_connection_runtime, build_room_message_runtime,
    ConnectionRuntime, RoomMessageRuntime,
};
pub use synctv_core::models::{
    CacheTarget, NotificationLevel, RealtimeDeliveryRoute, RealtimeEvent, WebRTCSignalKind,
};
pub use transport::{
    RealtimeEventHandler, RealtimeMessageTransport, RealtimeMessageTransportConfig,
    RealtimeMessageTransportFactory, RealtimeMessageTransportRuntime,
};

pub type SharedRealtimeEvent = Arc<RealtimeEvent>;

#[must_use]
pub fn build_realtime_message_transport_factory(
    runtime: Arc<dyn RedisCoordinationRuntime>,
) -> Arc<dyn RealtimeMessageTransportFactory> {
    Arc::new(redis_pubsub::RedisRealtimeMessageTransportFactory::new(
        runtime,
    ))
}
