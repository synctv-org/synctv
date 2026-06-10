#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub mod discovery;
pub mod error;
pub mod grpc;
pub mod leader;

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
    ClusterAuthInterceptor, ClusterClient, ClusterClientConfig, ClusterServer, ClusterServiceServer,
};
#[cfg(feature = "k8s")]
pub use leader::{K8sLeaderElector, K8sLeaderElectorConfig};
pub use leader::{LeaderElector, LeaderElectorConfig};
