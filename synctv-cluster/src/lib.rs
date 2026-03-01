#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod discovery;
pub mod error;
pub mod grpc;
pub mod leader;
pub mod sync;

#[cfg(feature = "k8s")]
pub use discovery::K8sDnsDiscovery;
pub use discovery::{
    ClusterMode, HealthMonitor, HeartbeatResult, LoadBalancer, LoadBalancingStrategy, NodeHealth,
    NodeInfo, NodeRegistry, StaticDiscovery, StaticDiscoveryConfig, StaticPeerConfig,
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
    BroadcastResult, ClusterConfig, ClusterManager, ClusterMetrics, ConnectionId,
    ConnectionManager, DedupKey, MessageDeduplicator, MessageSender as ClusterMessageSender,
    PublishRequest, RoomMessageHub, Subscriber,
};
