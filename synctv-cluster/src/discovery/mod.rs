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

use std::time::Duration;
use tonic::transport::Endpoint;

/// Probe a node's gRPC service by calling `GetNodes`.
///
/// Validates that the application-layer gRPC service is responsive, not just
/// that the port is open. Used by both `HealthMonitor` and `StaticDiscovery`.
///
/// Returns `true` if the node responds successfully to the `GetNodes` RPC.
pub async fn probe_node_grpc(grpc_address: &str, timeout_secs: u64, cluster_secret: &str) -> bool {
    let uri = if grpc_address.starts_with("http://") || grpc_address.starts_with("https://") {
        grpc_address.to_string()
    } else {
        format!("http://{grpc_address}")
    };

    let connect_timeout = Duration::from_secs(timeout_secs);
    let endpoint = match Endpoint::from_shared(uri) {
        Ok(ep) => ep.connect_timeout(connect_timeout).timeout(connect_timeout),
        Err(e) => {
            tracing::warn!(peer = %grpc_address, error = %e, "Invalid peer address");
            return false;
        }
    };

    let channel = match endpoint.connect().await {
        Ok(ch) => ch,
        Err(_) => return false,
    };

    use crate::grpc::synctv::cluster::cluster_service_client::ClusterServiceClient;
    use crate::grpc::synctv::cluster::GetNodesRequest;
    let mut client = ClusterServiceClient::new(channel);
    let mut request = tonic::Request::new(GetNodesRequest { status_filter: 0 });

    if !cluster_secret.is_empty() {
        match cluster_secret.parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>() {
            Ok(val) => {
                request.metadata_mut().insert("x-cluster-secret", val);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "cluster_secret contains invalid characters for gRPC metadata, skipping probe"
                );
                return false;
            }
        }
    }

    match client.get_nodes(request).await {
        Ok(_) => true,
        Err(e) => {
            tracing::debug!(peer = %grpc_address, error = %e, "Peer health check failed");
            false
        }
    }
}
