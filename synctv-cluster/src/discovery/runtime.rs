use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::health_monitor::NodeHealth;
use super::node_registry::{
    ClusterMode, HeartbeatResult, NodeDiscoverySource, NodeInfo, NodeViewMode,
};
use crate::error::Result;

#[async_trait]
pub trait ClusterNodeDirectory: Send + Sync {
    async fn register(&self, cluster_address: String) -> Result<()>;
    async fn heartbeat(&self) -> Result<HeartbeatResult>;
    async fn unregister(&self) -> Result<()>;
    async fn register_remote(&self, node_info: NodeInfo) -> Result<()>;
    async fn unregister_remote(&self, node_id: &str, expected_epoch: Option<u64>) -> Result<()>;
    async fn get_all_nodes(&self) -> Result<Vec<NodeInfo>>;
    async fn get_routable_nodes(&self) -> Result<(Vec<NodeInfo>, NodeViewMode)>;
    async fn update_local_metadata(&self, key: &str, value: String);
    async fn upsert_discovered_local_node(
        &self,
        node_info: NodeInfo,
        discovery_source: NodeDiscoverySource,
    );
    async fn remove_discovered_local_node(
        &self,
        node_id: &str,
        discovery_source: NodeDiscoverySource,
    ) -> bool;
    fn heartbeat_timeout_secs(&self) -> i64;
    fn cluster_mode(&self) -> ClusterMode;
    fn cancel_token(&self) -> CancellationToken;
    fn is_nodes_stale(&self) -> bool;
}

pub trait ClusterNodeDirectoryFactory: Send + Sync {
    fn build(
        &self,
        node_id: String,
        heartbeat_timeout_secs: i64,
        key_prefix: &str,
    ) -> Result<Arc<dyn ClusterNodeDirectory>>;
}

#[async_trait]
pub trait ClusterHealthRuntime: Send + Sync {
    fn start(&self) -> Result<()>;
    async fn shutdown(&self);
    async fn get_all_status(&self) -> HashMap<String, NodeHealth>;
    async fn get_node_status(&self, node_id: &str) -> Option<NodeHealth>;
    fn is_snapshot_stale(&self) -> bool;
}
