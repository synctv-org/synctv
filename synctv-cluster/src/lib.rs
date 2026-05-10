#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive — use only one");

pub mod discovery;
pub mod error;
pub mod grpc;
pub mod leader;
pub mod sync;

#[cfg(feature = "k8s")]
pub use discovery::K8sDnsDiscovery;
pub use discovery::{
    build_cluster_node_directory_factory, build_local_cluster_node_directory_factory,
    ClusterHealthRuntime, ClusterMode, ClusterNodeDirectory, ClusterNodeDirectoryFactory,
    HealthMonitor, HeartbeatResult, LoadBalancer, LoadBalancingStrategy,
    LocalClusterNodeDirectoryFactory, NodeHealth, NodeInfo, NodeRegistry, NodeViewMode,
    RedisClusterNodeDirectoryFactory, StaticDiscovery, StaticDiscoveryConfig, StaticPeerConfig,
};
pub use error::{Error, Result};
pub use grpc::{
    ClusterAuthInterceptor, ClusterClient, ClusterClientConfig, ClusterServer,
    ClusterServiceServer, ClusterSliceCachePurgeResult, ClusterSliceCacheStats, FanOutResult,
    SliceCacheRuntime,
};
#[cfg(feature = "k8s")]
pub use leader::{K8sLeaderElector, K8sLeaderElectorConfig};
pub use leader::{LeaderElector, LeaderElectorConfig};
pub use sync::{
    build_cluster_message_transport_factory, build_connection_manager, build_connection_runtime,
    build_room_message_runtime, BroadcastResult, ClusterConfig, ClusterManager,
    ClusterMessageTransport, ClusterMessageTransportConfig, ClusterMessageTransportFactory,
    ClusterMessageTransportRuntime, ClusterMetrics, ConnectionId, ConnectionManager,
    ConnectionRuntime, DedupKey, MessageDeduplicator, MessageSender as ClusterMessageSender,
    PublishRequest, RedisClusterMessageTransportFactory, RoomMessageHub, RoomMessageRuntime,
    Subscriber,
};
