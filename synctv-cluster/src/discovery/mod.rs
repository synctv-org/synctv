//! Cluster node discovery and health monitoring

pub mod node_registry;
pub mod health_monitor;
pub mod load_balancer;
pub mod static_discovery;
#[cfg(feature = "k8s")]
pub mod k8s_dns;

pub use node_registry::{ClusterMode, HeartbeatResult, NodeInfo, NodeRegistry};
pub use health_monitor::{HealthMonitor, NodeHealth};
pub use load_balancer::{LoadBalancer, LoadBalancingStrategy};
pub use static_discovery::{StaticDiscovery, StaticDiscoveryConfig, StaticPeerConfig};
#[cfg(feature = "k8s")]
pub use k8s_dns::K8sDnsDiscovery;
