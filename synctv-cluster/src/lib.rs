#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod discovery;
pub mod error;
pub mod grpc;
pub mod leader;
pub mod sync;

#[cfg(feature = "k8s")]
pub use discovery::K8sDnsDiscovery;
pub use discovery::{
    ClusterHealthRuntime, ClusterMode, ClusterNodeDirectory, ClusterNodeDirectoryFactory,
    HealthMonitor, HeartbeatResult, LoadBalancer, LoadBalancingStrategy,
    LocalClusterNodeDirectoryFactory, NodeHealth, NodeInfo, NodeRegistry, NodeViewMode,
    RedisClusterNodeDirectoryFactory, StaticDiscovery, StaticDiscoveryConfig, StaticPeerConfig,
};
pub use error::{Error, Result};
pub use grpc::{
    ClusterAuthInterceptor, ClusterClient, ClusterClientConfig, ClusterServer,
    ClusterServiceServer, FanOutResult,
};
#[cfg(feature = "k8s")]
pub use leader::{K8sLeaderElector, K8sLeaderElectorConfig};
pub use leader::{LeaderElector, LeaderElectorConfig};
pub use sync::{
    build_connection_manager, build_connection_runtime, build_room_message_runtime,
    BroadcastResult, ClusterConfig, ClusterManager, ClusterMessageTransport,
    ClusterMessageTransportConfig, ClusterMessageTransportFactory, ClusterMessageTransportRuntime,
    ClusterMetrics, ConnectionId, ConnectionManager, ConnectionRuntime, DedupKey,
    MessageDeduplicator, MessageSender as ClusterMessageSender, PublishRequest,
    RedisClusterMessageTransportFactory, RoomMessageHub, RoomMessageRuntime, Subscriber,
};
