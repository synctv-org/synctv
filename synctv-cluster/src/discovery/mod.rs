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

use crate::grpc::synctv::cluster::cluster_service_client::ClusterServiceClient;
use crate::grpc::synctv::cluster::{self, GetNodesRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedNodeIdentity {
    pub node_id: String,
    pub api_address: String,
    pub epoch: u64,
}

/// Probe a node's gRPC service on the shared API address by calling `GetNodes`.
///
/// Validates that the application-layer gRPC service is responsive, not just
/// that the port is open. Used by both `HealthMonitor` and `StaticDiscovery`.
///
/// Returns `true` if the node responds successfully to the `GetNodes` RPC.
pub async fn probe_node_api(api_address: &str, timeout_secs: u64, cluster_secret: &str) -> bool {
    probe_node_identity(api_address, timeout_secs, cluster_secret)
        .await
        .is_some()
}

/// Probe a node and return the peer-reported node identity.
///
/// This is used by static discovery so registrations use the remote node's
/// actual `node_id` instead of a synthetic ID derived from the address.
pub async fn probe_node_identity(
    api_address: &str,
    timeout_secs: u64,
    cluster_secret: &str,
) -> Option<ProbedNodeIdentity> {
    let uri = if api_address.starts_with("http://") || api_address.starts_with("https://") {
        api_address.to_string()
    } else {
        format!("http://{api_address}")
    };

    let connect_timeout = Duration::from_secs(timeout_secs);
    let endpoint = match Endpoint::from_shared(uri) {
        Ok(ep) => ep.connect_timeout(connect_timeout).timeout(connect_timeout),
        Err(e) => {
            tracing::warn!(peer = %api_address, error = %e, "Invalid peer address");
            return None;
        }
    };

    let Ok(channel) = endpoint.connect().await else {
        return None;
    };
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
                return None;
            }
        }
    }

    match client.get_nodes(request).await {
        Ok(response) => extract_probed_node_identity(response.into_inner(), api_address),
        Err(e) => {
            tracing::debug!(peer = %api_address, error = %e, "Peer health check failed");
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
                    api_address: node_address,
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
    fn test_extract_probed_node_identity_preserves_peer_epoch() {
        let response = cluster::GetNodesResponse {
            nodes: vec![cluster::NodeInfo {
                node_id: "peer-node-1".to_string(),
                address: "10.0.0.5:50051".to_string(),
                region: String::new(),
                status: cluster::NodeStatus::Active as i32,
                registered_at: 100,
                last_heartbeat: 100,
                metrics: None,
                epoch: 7,
            }],
        };

        let identity = extract_probed_node_identity(response, "10.0.0.5:50051")
            .expect("matching peer should be extracted");

        assert_eq!(identity.node_id, "peer-node-1");
        assert_eq!(identity.api_address, "10.0.0.5:50051");
        assert_eq!(identity.epoch, 7);
    }
}
