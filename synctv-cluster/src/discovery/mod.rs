//! Cluster node discovery and health monitoring

pub mod health_monitor;
#[cfg(feature = "k8s")]
pub mod k8s_dns;
pub mod load_balancer;
pub mod node_registry;
pub mod static_discovery;

pub use health_monitor::{HealthMonitor, NodeHealth};
#[cfg(feature = "k8s")]
pub use k8s_dns::K8sDnsDiscovery;
pub use load_balancer::{LoadBalancer, LoadBalancingStrategy};
pub use node_registry::{ClusterMode, HeartbeatResult, NodeInfo, NodeRegistry};
pub use static_discovery::{StaticDiscovery, StaticDiscoveryConfig, StaticPeerConfig};
