use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use super::server_state::ServerStateClusterTarget;

const SLICE_CACHE_FAN_OUT_CONCURRENCY: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum SliceCacheManagementError {
    #[error("nodeId and allNodes are mutually exclusive")]
    InvalidSelection,
    #[error("cluster client is unavailable; cannot manage slice cache for node '{0}'")]
    ClusterUnavailable(String),
    #[error("{0}")]
    Cluster(String),
    #[error("cluster secret is required for remote slice cache operations")]
    MissingClusterSecret,
    #[error("invalid cluster secret configuration")]
    InvalidClusterSecret,
    #[error("failed to request slice cache from node '{node_id}': {error}")]
    RemoteRequest { node_id: String, error: String },
}

pub type SliceCacheManagementResult<T> = Result<T, SliceCacheManagementError>;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCacheSelection {
    pub node_id: Option<String>,
    pub all_nodes: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCacheConfigInfo {
    pub engine_enabled: bool,
    pub backend: String,
    pub file_cache_dir: String,
    pub slice_size: u64,
    pub max_cache_size: u64,
    pub segment_ttl_secs: u64,
    pub stale_max_age_secs: u64,
    pub stale_while_revalidate: bool,
    pub eviction_interval_secs: u64,
    pub watermark_ratio: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCacheStats {
    pub config: SliceCacheConfigInfo,
    pub current_size_bytes: u64,
    pub entry_count: u64,
    pub metadata_entries: u64,
    pub updating_entries: u64,
    pub lock_count: u64,
    pub usage_ratio: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCacheStatsNode {
    pub node_id: String,
    pub config: SliceCacheConfigInfo,
    pub current_size_bytes: u64,
    pub entry_count: u64,
    pub metadata_entries: u64,
    pub updating_entries: u64,
    pub lock_count: u64,
    pub usage_ratio: f64,
}

impl SliceCacheStats {
    #[must_use]
    pub fn with_node_id(self, node_id: String) -> SliceCacheStatsNode {
        SliceCacheStatsNode {
            node_id,
            config: self.config,
            current_size_bytes: self.current_size_bytes,
            entry_count: self.entry_count,
            metadata_entries: self.metadata_entries,
            updating_entries: self.updating_entries,
            lock_count: self.lock_count,
            usage_ratio: self.usage_ratio,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCacheNodeFailure {
    pub node_id: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCacheStatsResponse {
    pub nodes: Vec<SliceCacheStatsNode>,
    pub failures: Vec<SliceCacheNodeFailure>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCachePurgeResult {
    pub removed_entries: u64,
    pub freed_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCachePurgeNodeResult {
    pub node_id: String,
    pub success: bool,
    pub removed_entries: u64,
    pub freed_bytes: u64,
    pub stats: Option<SliceCacheStatsNode>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCachePurgeResponse {
    pub success: bool,
    pub removed_entries: u64,
    pub freed_bytes: u64,
    pub stats: Option<SliceCacheStatsNode>,
    pub nodes: Vec<SliceCachePurgeNodeResult>,
    pub failures: Vec<SliceCacheNodeFailure>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCacheEvictExpiredNodeResult {
    pub node_id: String,
    pub success: bool,
    pub removed_expired_entries: u64,
    pub stats: Option<SliceCacheStatsNode>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceCacheEvictExpiredResponse {
    pub success: bool,
    pub removed_expired_entries: u64,
    pub stats: Option<SliceCacheStatsNode>,
    pub nodes: Vec<SliceCacheEvictExpiredNodeResult>,
    pub failures: Vec<SliceCacheNodeFailure>,
}

#[async_trait]
pub trait SliceCacheManagementLocalRuntime: Send + Sync {
    fn stats(&self) -> SliceCacheStats;

    async fn purge_all(&self) -> SliceCachePurgeResult;

    async fn evict_expired_entries(&self) -> u64;
}

#[async_trait]
pub trait SliceCacheManagementClusterRuntime: Send + Sync {
    async fn resolve_routable_node(
        &self,
        target_node_id: &str,
    ) -> SliceCacheManagementResult<ServerStateClusterTarget>;

    async fn remote_routable_nodes(
        &self,
    ) -> SliceCacheManagementResult<Vec<ServerStateClusterTarget>>;
}

#[async_trait]
pub trait SliceCacheManagementRemoteClient: Send + Sync {
    async fn remote_stats(
        &self,
        node: &ServerStateClusterTarget,
    ) -> SliceCacheManagementResult<SliceCacheStatsNode>;

    async fn remote_purge(
        &self,
        node: &ServerStateClusterTarget,
    ) -> SliceCacheManagementResult<SliceCachePurgeNodeResult>;

    async fn remote_evict_expired(
        &self,
        node: &ServerStateClusterTarget,
    ) -> SliceCacheManagementResult<SliceCacheEvictExpiredNodeResult>;
}

pub struct SliceCacheManagementServiceDependencies {
    pub node_id: String,
    pub local_runtime: Arc<dyn SliceCacheManagementLocalRuntime>,
    pub cluster_runtime: Option<Arc<dyn SliceCacheManagementClusterRuntime>>,
    pub remote_client: Option<Arc<dyn SliceCacheManagementRemoteClient>>,
}

#[derive(Clone)]
pub struct SliceCacheManagementService {
    node_id: String,
    local_runtime: Arc<dyn SliceCacheManagementLocalRuntime>,
    cluster_runtime: Option<Arc<dyn SliceCacheManagementClusterRuntime>>,
    remote_client: Option<Arc<dyn SliceCacheManagementRemoteClient>>,
}

impl SliceCacheManagementService {
    #[must_use]
    pub fn new(deps: SliceCacheManagementServiceDependencies) -> Self {
        Self {
            node_id: deps.node_id,
            local_runtime: deps.local_runtime,
            cluster_runtime: deps.cluster_runtime,
            remote_client: deps.remote_client,
        }
    }

    pub async fn get_stats(
        &self,
        selection: SliceCacheSelection,
    ) -> SliceCacheManagementResult<SliceCacheStatsResponse> {
        let target_node_id =
            validate_slice_cache_selection(selection.node_id.as_deref(), selection.all_nodes)?;
        if selection.all_nodes {
            let local = self.local_stats();
            let (nodes, failures) = self
                .fan_out_all(local, |service, node| async move {
                    service.remote_client()?.remote_stats(&node).await
                })
                .await?;
            return Ok(SliceCacheStatsResponse { nodes, failures });
        }

        let node = match target_node_id {
            Some(node_id) => self.stats_for_target(&node_id).await?,
            None => self.local_stats(),
        };
        Ok(SliceCacheStatsResponse {
            nodes: vec![node],
            failures: Vec::new(),
        })
    }

    pub async fn purge(
        &self,
        selection: SliceCacheSelection,
    ) -> SliceCacheManagementResult<SliceCachePurgeResponse> {
        let target_node_id =
            validate_slice_cache_selection(selection.node_id.as_deref(), selection.all_nodes)?;
        if selection.all_nodes {
            let local = self.local_purge().await;
            let (nodes, failures) = self
                .fan_out_all(local, |service, node| async move {
                    service.remote_client()?.remote_purge(&node).await
                })
                .await?;
            return Ok(purge_response_from_nodes(nodes, failures));
        }

        let node = match target_node_id {
            Some(node_id) if node_id != self.node_id => {
                let node = self.resolve_routable_node(&node_id).await?;
                self.remote_client()?.remote_purge(&node).await?
            }
            Some(_) | None => self.local_purge().await,
        };
        Ok(purge_response_from_nodes(vec![node], Vec::new()))
    }

    pub async fn evict_expired(
        &self,
        selection: SliceCacheSelection,
    ) -> SliceCacheManagementResult<SliceCacheEvictExpiredResponse> {
        let target_node_id =
            validate_slice_cache_selection(selection.node_id.as_deref(), selection.all_nodes)?;
        if selection.all_nodes {
            let local = self.local_evict_expired().await;
            let (nodes, failures) = self
                .fan_out_all(local, |service, node| async move {
                    service.remote_client()?.remote_evict_expired(&node).await
                })
                .await?;
            return Ok(evict_expired_response_from_nodes(nodes, failures));
        }

        let node = match target_node_id {
            Some(node_id) if node_id != self.node_id => {
                let node = self.resolve_routable_node(&node_id).await?;
                self.remote_client()?.remote_evict_expired(&node).await?
            }
            Some(_) | None => self.local_evict_expired().await,
        };
        Ok(evict_expired_response_from_nodes(vec![node], Vec::new()))
    }

    fn local_stats(&self) -> SliceCacheStatsNode {
        self.local_runtime
            .stats()
            .with_node_id(self.node_id.clone())
    }

    async fn stats_for_target(
        &self,
        target_node_id: &str,
    ) -> SliceCacheManagementResult<SliceCacheStatsNode> {
        if target_node_id == self.node_id {
            return Ok(self.local_stats());
        }
        let node = self.resolve_routable_node(target_node_id).await?;
        self.remote_client()?.remote_stats(&node).await
    }

    async fn local_purge(&self) -> SliceCachePurgeNodeResult {
        let result = self.local_runtime.purge_all().await;
        SliceCachePurgeNodeResult {
            node_id: self.node_id.clone(),
            success: true,
            removed_entries: result.removed_entries,
            freed_bytes: result.freed_bytes,
            stats: Some(self.local_stats()),
        }
    }

    async fn local_evict_expired(&self) -> SliceCacheEvictExpiredNodeResult {
        let removed_expired_entries = self.local_runtime.evict_expired_entries().await;
        SliceCacheEvictExpiredNodeResult {
            node_id: self.node_id.clone(),
            success: true,
            removed_expired_entries,
            stats: Some(self.local_stats()),
        }
    }

    async fn resolve_routable_node(
        &self,
        target_node_id: &str,
    ) -> SliceCacheManagementResult<ServerStateClusterTarget> {
        self.cluster_runtime(target_node_id)?
            .resolve_routable_node(target_node_id)
            .await
    }

    fn cluster_runtime(
        &self,
        target_node_id: &str,
    ) -> SliceCacheManagementResult<&Arc<dyn SliceCacheManagementClusterRuntime>> {
        self.cluster_runtime.as_ref().ok_or_else(|| {
            SliceCacheManagementError::ClusterUnavailable(target_node_id.to_string())
        })
    }

    fn remote_client(
        &self,
    ) -> SliceCacheManagementResult<&Arc<dyn SliceCacheManagementRemoteClient>> {
        self.remote_client
            .as_ref()
            .ok_or_else(|| SliceCacheManagementError::ClusterUnavailable(self.node_id.clone()))
    }

    async fn fan_out_all<T, F, Fut>(
        &self,
        local_result: T,
        remote_call: F,
    ) -> SliceCacheManagementResult<(Vec<T>, Vec<SliceCacheNodeFailure>)>
    where
        F: Fn(Self, ServerStateClusterTarget) -> Fut + Clone,
        Fut: std::future::Future<Output = SliceCacheManagementResult<T>>,
    {
        let mut results = vec![local_result];
        let mut failures = Vec::new();

        if let Some(cluster_runtime) = &self.cluster_runtime {
            let remote_nodes = cluster_runtime.remote_routable_nodes().await?;
            let mut futures = futures::stream::iter(remote_nodes)
                .map(|node| {
                    let service = self.clone();
                    let call = remote_call.clone();
                    async move {
                        let node_id = node.node_id.clone();
                        call(service, node)
                            .await
                            .map_err(|error| SliceCacheNodeFailure {
                                node_id,
                                error: error.to_string(),
                            })
                    }
                })
                .buffer_unordered(SLICE_CACHE_FAN_OUT_CONCURRENCY);
            while let Some(result) = futures.next().await {
                match result {
                    Ok(response) => results.push(response),
                    Err(failure) => failures.push(failure),
                }
            }
        }

        Ok((results, failures))
    }
}

pub fn validate_slice_cache_selection(
    node_id: Option<&str>,
    all_nodes: bool,
) -> SliceCacheManagementResult<Option<String>> {
    let node_id = node_id.unwrap_or_default().trim();
    if all_nodes && !node_id.is_empty() {
        return Err(SliceCacheManagementError::InvalidSelection);
    }
    Ok((!node_id.is_empty()).then(|| node_id.to_string()))
}

#[must_use]
pub fn purge_response_from_nodes(
    nodes: Vec<SliceCachePurgeNodeResult>,
    failures: Vec<SliceCacheNodeFailure>,
) -> SliceCachePurgeResponse {
    let removed_entries = nodes.iter().map(|node| node.removed_entries).sum();
    let freed_bytes = nodes.iter().map(|node| node.freed_bytes).sum();
    let stats = (nodes.len() == 1).then(|| nodes[0].stats.clone()).flatten();
    SliceCachePurgeResponse {
        success: failures.is_empty() && nodes.iter().all(|node| node.success),
        removed_entries,
        freed_bytes,
        stats,
        nodes,
        failures,
    }
}

#[must_use]
pub fn evict_expired_response_from_nodes(
    nodes: Vec<SliceCacheEvictExpiredNodeResult>,
    failures: Vec<SliceCacheNodeFailure>,
) -> SliceCacheEvictExpiredResponse {
    let removed_expired_entries = nodes.iter().map(|node| node.removed_expired_entries).sum();
    let stats = (nodes.len() == 1).then(|| nodes[0].stats.clone()).flatten();
    SliceCacheEvictExpiredResponse {
        success: failures.is_empty() && nodes.iter().all(|node| node.success),
        removed_expired_entries,
        stats,
        nodes,
        failures,
    }
}
