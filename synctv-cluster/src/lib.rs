pub mod sync;
pub mod discovery;
pub mod grpc;
pub mod error;
pub mod leader;

pub use error::{Error, Result};
pub use discovery::{HeartbeatResult, NodeInfo, NodeRegistry, HealthMonitor, NodeHealth, LoadBalancer, LoadBalancingStrategy};
#[cfg(feature = "k8s")]
pub use discovery::K8sDnsDiscovery;
pub use sync::{
    ConnectionManager, PublishRequest, RoomMessageHub,
    ClusterManager, ClusterConfig, ClusterMetrics, BroadcastResult,
    MessageDeduplicator, DedupKey, ConnectionId, Subscriber,
    MessageSender as ClusterMessageSender,
};
pub use grpc::{ClusterClient, ClusterClientConfig, ClusterServer, ClusterServiceServer, ClusterAuthInterceptor, FanOutResult};
pub use leader::{LeaderElector, LeaderElectorConfig};
#[cfg(feature = "k8s")]
pub use leader::{K8sLeaderElector, K8sLeaderElectorConfig};
