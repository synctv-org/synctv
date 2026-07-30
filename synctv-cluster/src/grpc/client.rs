//! Cluster gRPC/topology client.
//!
//! The cluster client exposes node discovery only. Business fan-out clients
//! live in the owning crates and use this topology data to call each node's
//! internal service on the shared API tonic server.

use std::sync::Arc;

use crate::discovery::{ClusterNodeDirectory, NodeInfo};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct ClusterClientConfig {
    pub self_node_id: String,
}

pub struct ClusterClient {
    node_registry: Arc<dyn ClusterNodeDirectory>,
    config: ClusterClientConfig,
}

impl ClusterClient {
    pub fn new<N>(node_registry: Arc<N>, config: ClusterClientConfig) -> Self
    where
        N: ClusterNodeDirectory + 'static,
    {
        Self::from_runtime(node_registry, config)
    }

    pub const fn from_runtime(
        node_registry: Arc<dyn ClusterNodeDirectory>,
        config: ClusterClientConfig,
    ) -> Self {
        Self {
            node_registry,
            config,
        }
    }

    async fn find_routable_node(&self, target_node_id: &str) -> Result<NodeInfo> {
        let (nodes, _view_mode) = self.node_registry.get_routable_nodes().await?;
        nodes
            .into_iter()
            .find(|node| node.node_id == target_node_id)
            .ok_or_else(|| Error::Rpc(format!("cluster node '{target_node_id}' is not routable")))
    }

    /// Resolve a routable cluster node by ID.
    ///
    /// This keeps cluster's responsibility limited to topology discovery. Callers
    /// use the returned `cluster_address` to invoke their own internal gRPC services.
    pub async fn resolve_routable_node(&self, target_node_id: &str) -> Result<NodeInfo> {
        self.find_routable_node(target_node_id).await
    }

    /// Return all routable remote nodes, excluding this client node.
    pub async fn remote_routable_nodes(&self) -> Result<Vec<NodeInfo>> {
        let (nodes, _view_mode) = self.node_registry.get_routable_nodes().await?;
        Ok(nodes
            .into_iter()
            .filter(|node| node.node_id != self.config.self_node_id)
            .collect())
    }

    #[must_use]
    pub const fn config(&self) -> &ClusterClientConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::ClusterClientConfig;

    #[test]
    fn default_config_has_no_local_node_filter() {
        let config = ClusterClientConfig::default();
        assert!(config.self_node_id.is_empty());
    }
}
