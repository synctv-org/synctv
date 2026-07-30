//! Cluster node discovery and health monitoring

pub mod health_monitor;
#[cfg(feature = "k8s")]
pub mod k8s_dns;
pub mod load_balancer;
pub mod node_registry;
pub mod runtime;
pub mod static_discovery;

use std::sync::Arc;
use synctv_core::RedisCoordinationRuntime;

pub use health_monitor::{HealthMonitor, NodeHealth};
#[cfg(feature = "k8s")]
pub use k8s_dns::{K8sDnsDiscovery, K8sDnsDiscoveryOptions};
pub use load_balancer::{LoadBalancer, LoadBalancingStrategy};
pub use node_registry::{
    ClusterMode, HeartbeatResult, LocalClusterNodeDirectoryFactory, NodeInfo, NodeRegistry,
    NodeViewMode, RedisClusterNodeDirectoryFactory,
};
pub use runtime::{ClusterHealthRuntime, ClusterNodeDirectory, ClusterNodeDirectoryFactory};
pub use static_discovery::{
    normalize_static_peer_address, StaticDiscovery, StaticDiscoveryConfig, StaticPeerConfig,
};

use std::time::Duration;
use tonic::transport::Endpoint;

use crate::grpc::synctv::cluster::cluster_service_client::ClusterServiceClient;
use crate::grpc::synctv::cluster::{self, GetNodesRequest};

#[must_use]
pub fn build_cluster_node_directory_factory(
    runtime: Arc<dyn RedisCoordinationRuntime>,
) -> Arc<dyn ClusterNodeDirectoryFactory> {
    Arc::new(RedisClusterNodeDirectoryFactory::new(runtime))
}

#[must_use]
pub fn build_local_cluster_node_directory_factory() -> Arc<dyn ClusterNodeDirectoryFactory> {
    Arc::new(LocalClusterNodeDirectoryFactory)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedNodeIdentity {
    pub node_id: String,
    pub cluster_address: String,
    pub epoch: u64,
}

/// Probe a node's internal gRPC listener by calling `GetNodes`.
///
/// Validates that the application-layer gRPC service is responsive, not just
/// that the port is open. Used by both `HealthMonitor` and `StaticDiscovery`.
///
/// Returns `true` if the node responds successfully to the `GetNodes` RPC.
pub async fn probe_cluster_node(
    cluster_address: &str,
    timeout_secs: u64,
    cluster_secret: &str,
) -> bool {
    probe_node_identity(cluster_address, timeout_secs, cluster_secret)
        .await
        .is_some()
}

/// Probe a node and return the peer-reported node identity.
///
/// This is used by static discovery so registrations use the remote node's
/// actual `node_id` instead of a synthetic ID derived from the address.
pub async fn probe_node_identity(
    cluster_address: &str,
    timeout_secs: u64,
    cluster_secret: &str,
) -> Option<ProbedNodeIdentity> {
    let uri = if cluster_address.starts_with("http://") || cluster_address.starts_with("https://") {
        cluster_address.to_string()
    } else {
        format!("http://{cluster_address}")
    };

    let connect_timeout = Duration::from_secs(timeout_secs);
    let endpoint = match Endpoint::from_shared(uri) {
        Ok(ep) => ep.connect_timeout(connect_timeout).timeout(connect_timeout),
        Err(e) => {
            tracing::warn!(peer = %cluster_address, error = %e, "Invalid peer address");
            return None;
        }
    };

    let Ok(channel) = endpoint.connect().await else {
        return None;
    };
    let mut client = ClusterServiceClient::new(channel);
    let mut request = tonic::Request::new(GetNodesRequest {});

    if !cluster_secret.is_empty() {
        match crate::grpc::attach_cluster_secret(&mut request, cluster_secret) {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "cluster_secret contains invalid characters for gRPC metadata, skipping probe"
                );
                return None;
            }
        }
    }

    match client.get_nodes(request).await {
        Ok(response) => extract_probed_node_identity(response.into_inner(), cluster_address),
        Err(e) => {
            tracing::debug!(peer = %cluster_address, error = %e, "Peer health check failed");
            None
        }
    }
}

fn extract_probed_node_identity(
    response: cluster::GetNodesResponse,
    probed_address: &str,
) -> Option<ProbedNodeIdentity> {
    response.nodes.into_iter().find_map(|node| {
        normalize_cluster_node_address(&node.address).and_then(|node_address| {
            if node.node_id.is_empty() {
                return None;
            }
            if node_address == normalize_cluster_node_address(probed_address)? {
                Some(ProbedNodeIdentity {
                    node_id: node.node_id,
                    cluster_address: node_address,
                    epoch: node.epoch,
                })
            } else {
                None
            }
        })
    })
}

fn normalize_cluster_node_address(address: &str) -> Option<String> {
    if let Some(rest) = address.strip_prefix("http://") {
        return Some(rest.to_string());
    }
    if let Some(rest) = address.strip_prefix("https://") {
        return Some(rest.to_string());
    }
    if address.is_empty() {
        return None;
    }
    Some(address.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_probed_node_identity_preserves_peer_epoch() -> Result<(), &'static str> {
        let response = cluster::GetNodesResponse {
            nodes: vec![cluster::NodeInfo {
                node_id: "peer-node-1".to_string(),
                address: "10.0.0.5:50051".to_string(),
                last_heartbeat: 100,
                epoch: 7,
            }],
        };

        let identity = extract_probed_node_identity(response, "10.0.0.5:50051")
            .ok_or("matching peer should be extracted")?;

        assert_eq!(identity.node_id, "peer-node-1");
        assert_eq!(identity.cluster_address, "10.0.0.5:50051");
        assert_eq!(identity.epoch, 7);
        Ok(())
    }
}
