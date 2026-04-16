pub mod cluster_manager;
pub mod connection_manager;
pub mod dedup;
pub mod events;
pub mod redis_pubsub;
pub mod room_hub;
pub mod runtime;
pub mod transport;

use std::sync::Arc;

use synctv_core::RedisCoordinationRuntime;

pub use cluster_manager::{BroadcastResult, ClusterConfig, ClusterManager, ClusterMetrics};
pub use connection_manager::{
    ConnectionInfo, ConnectionLimits, ConnectionManager, ConnectionMetrics, DisconnectSignal,
    DisconnectSignalMetrics,
};
pub use dedup::{DedupKey, MessageDeduplicator};
pub use events::{CacheTarget, ClusterEvent, NotificationLevel};
pub use redis_pubsub::{PublishRequest, RedisClusterMessageTransportFactory, RedisPubSub};
pub use room_hub::{ConnectionId, MessageSender, RoomLifecycleEvent, RoomMessageHub, Subscriber};
pub use runtime::{
    build_connection_manager, build_connection_runtime, build_room_message_runtime,
    ConnectionRuntime, RoomMessageRuntime,
};
pub use transport::{
    ClusterMessageTransport, ClusterMessageTransportConfig, ClusterMessageTransportFactory,
    ClusterMessageTransportRuntime,
};

#[must_use]
pub fn build_cluster_message_transport_factory(
    runtime: Arc<dyn RedisCoordinationRuntime>,
) -> Arc<dyn ClusterMessageTransportFactory> {
    Arc::new(redis_pubsub::RedisClusterMessageTransportFactory::new(
        runtime,
    ))
}
