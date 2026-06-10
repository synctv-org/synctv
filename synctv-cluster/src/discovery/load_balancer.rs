//! Load balancing for cluster requests
//!
//! Distributes requests across available cluster nodes.
//! Integrates with `HealthMonitor` to exclude unhealthy nodes.

use rand::prelude::IndexedRandom;
use std::sync::Arc;

use super::health_monitor::NodeHealth;
use super::node_registry::NodeInfo;
use super::runtime::{ClusterHealthRuntime, ClusterNodeDirectory};
use crate::error::{Error, Result};

/// Load balancing strategy
#[derive(Debug, Clone, Copy)]
pub enum LoadBalancingStrategy {
    /// Random selection
    Random,
    /// Round-robin
    RoundRobin,
    /// Least connections (select node with fewest active connections)
    /// Nodes must report connection count in `metadata["connections"]` via heartbeat.
    LeastConnections,
}

/// Load balancer for cluster node selection
pub struct LoadBalancer {
    node_registry: Arc<dyn ClusterNodeDirectory>,
    health_monitor: Option<Arc<dyn ClusterHealthRuntime>>,
    strategy: LoadBalancingStrategy,
    round_robin_index: std::sync::atomic::AtomicUsize,
}

fn metadata_i64(node: &NodeInfo, key: &'static str) -> Option<i64> {
    node.metadata.get(key).and_then(|value| {
        value
            .parse::<i64>()
            .map_err(|error| {
                tracing::warn!(
                    node_id = %node.node_id,
                    metadata_key = key,
                    metadata_value = %value,
                    error = %error,
                    "Ignoring invalid cluster node timestamp metadata"
                );
            })
            .ok()
    })
}

fn least_connections_score(node: &NodeInfo, now_timestamp: i64) -> usize {
    // Nodes report connection count in metadata["connections"] via heartbeat.
    // Nodes without valid connection metadata receive a warmup penalty so new or
    // malformed reporters are not immediately preferred by every caller.
    const WARMUP_PERIOD_SECS: i64 = 60;
    const WARMUP_PERIOD_SECS_USIZE: usize = 60;
    const WARMUP_PENALTY: usize = 1000;

    if let Some(raw_connections) = node.metadata.get("connections") {
        return raw_connections.parse::<usize>().unwrap_or_else(|error| {
            tracing::warn!(
                node_id = %node.node_id,
                metadata_key = "connections",
                metadata_value = %raw_connections,
                error = %error,
                effective_connections = WARMUP_PENALTY,
                "Cluster node reported invalid connection metadata; applying routing penalty"
            );
            WARMUP_PENALTY
        });
    }

    let registered_at = metadata_i64(node, "registered_at").unwrap_or_else(|| {
        tracing::debug!(
            node_id = %node.node_id,
            "Node has no valid registered_at metadata; applying full warmup penalty"
        );
        now_timestamp
    });

    let age_secs = now_timestamp.saturating_sub(registered_at);
    if age_secs < WARMUP_PERIOD_SECS {
        let age_secs = usize::try_from(age_secs).unwrap_or(usize::MAX);
        let remaining_warmup = WARMUP_PERIOD_SECS_USIZE.saturating_sub(age_secs);
        let penalty = WARMUP_PENALTY.saturating_mul(remaining_warmup) / WARMUP_PERIOD_SECS_USIZE;
        tracing::trace!(
            node_id = %node.node_id,
            age_secs = age_secs,
            effective_connections = penalty,
            "Node in warmup period"
        );
        penalty
    } else {
        tracing::debug!(
            node_id = %node.node_id,
            "Node has no connection metadata after warmup, treating as empty"
        );
        0
    }
}

impl LoadBalancer {
    /// Create a new load balancer
    #[must_use]
    pub fn new<N>(node_registry: Arc<N>, strategy: LoadBalancingStrategy) -> Self
    where
        N: ClusterNodeDirectory + 'static,
    {
        Self::from_runtime(node_registry, strategy)
    }

    #[must_use]
    pub fn from_runtime(
        node_registry: Arc<dyn ClusterNodeDirectory>,
        strategy: LoadBalancingStrategy,
    ) -> Self {
        Self {
            node_registry,
            health_monitor: None,
            strategy,
            round_robin_index: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Attach a health monitor to filter out unhealthy nodes
    #[must_use]
    pub fn with_health_monitor<H>(self, monitor: Arc<H>) -> Self
    where
        H: ClusterHealthRuntime + 'static,
    {
        self.with_health_runtime(monitor)
    }

    #[must_use]
    pub fn with_health_runtime(mut self, monitor: Arc<dyn ClusterHealthRuntime>) -> Self {
        self.health_monitor = Some(monitor);
        self
    }

    /// Get healthy nodes, filtering by health monitor if available
    async fn get_healthy_nodes(&self) -> Result<Vec<NodeInfo>> {
        let (nodes, _view_mode) = self.node_registry.get_routable_nodes().await?;

        // If no health monitor, return all nodes (stale nodes already filtered by registry)
        let Some(ref monitor) = self.health_monitor else {
            return Ok(nodes);
        };

        if monitor.is_snapshot_stale() {
            return Err(Error::NotFound(
                "Cluster health snapshot is stale; refusing to route with frozen health data"
                    .to_string(),
            ));
        }

        let statuses = monitor.get_all_status().await;

        let healthy: Vec<NodeInfo> = nodes
            .into_iter()
            .filter(|n| {
                statuses
                    .get(&n.node_id)
                    .is_none_or(|s| *s != NodeHealth::Unhealthy)
            })
            .collect();

        Ok(healthy)
    }

    /// Select a node for the next request.
    pub async fn select_node(&self) -> Result<String> {
        let nodes = self.get_healthy_nodes().await?;

        if nodes.is_empty() {
            return Err(Error::NotFound(
                "No healthy nodes available in the cluster".to_string(),
            ));
        }

        let selected_node = match self.strategy {
            LoadBalancingStrategy::Random => nodes
                .choose(&mut rand::rng())
                .ok_or_else(|| Error::NotFound("No nodes available".to_string()))?
                .node_id
                .clone(),
            LoadBalancingStrategy::RoundRobin => {
                // Sort by node_id for stable ordering across calls
                let mut sorted = nodes;
                sorted.sort_by(|a, b| a.node_id.cmp(&b.node_id));
                let index = self
                    .round_robin_index
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                    % sorted.len();
                sorted[index].node_id.clone()
            }
            LoadBalancingStrategy::LeastConnections => {
                let now_timestamp = chrono::Utc::now().timestamp();
                nodes
                    .iter()
                    .min_by_key(|node| least_connections_score(node, now_timestamp))
                    .ok_or_else(|| Error::NotFound("No nodes available".to_string()))?
                    .node_id
                    .clone()
            }
        };

        Ok(selected_node)
    }

    /// Select a specific node by ID (returns error if node not available)
    pub async fn select_node_by_id(&self, node_id: &str) -> Result<String> {
        self.get_healthy_nodes()
            .await?
            .into_iter()
            .find(|node| node.node_id == node_id)
            .map(|node| node.node_id)
            .ok_or_else(|| Error::NotFound(format!("Node {node_id} not available")))
    }

    /// Get all available healthy nodes
    pub async fn get_available_nodes(&self) -> Result<Vec<String>> {
        let nodes = self.get_healthy_nodes().await?;
        Ok(nodes.into_iter().map(|n| n.node_id).collect())
    }

    /// Get count of available healthy nodes
    pub async fn available_count(&self) -> Result<usize> {
        let nodes = self.get_healthy_nodes().await?;
        Ok(nodes.len())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{HealthMonitor, NodeRegistry};
    use super::*;
    use std::collections::HashSet;

    fn make_registry() -> Result<Arc<NodeRegistry>> {
        Ok(Arc::new(NodeRegistry::new_local_only(
            "self".to_string(),
            30,
            "test:",
        )?))
    }

    fn make_redis_registry(prefix: &str) -> Result<Arc<NodeRegistry>> {
        let client = redis::Client::open("redis://127.0.0.1:1")
            .map_err(|error| Error::Redis(error.to_string()))?;
        Ok(Arc::new(NodeRegistry::new(
            synctv_core::coordination_runtime_from_client(client),
            "self".to_string(),
            30,
            prefix,
        )?))
    }

    fn node_mut<'a>(
        nodes: &'a mut std::collections::HashMap<String, NodeInfo>,
        node_id: &str,
    ) -> Result<&'a mut NodeInfo> {
        nodes
            .get_mut(node_id)
            .ok_or_else(|| Error::NotFound(format!("test node {node_id} not registered")))
    }

    fn unix_now_secs_for_test() -> Result<u64> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .map_err(|error| {
                Error::Internal(anyhow::anyhow!(
                    "system clock is before UNIX_EPOCH: {error}"
                ))
            })
    }

    /// Helper: populate N nodes directly into the local cache (no Redis required)
    async fn register_nodes(registry: &NodeRegistry, count: usize) {
        let mut nodes = registry.local_nodes.write().await;
        // Register "self" first
        nodes.insert(
            "self".to_string(),
            NodeInfo::new("self".to_string(), "localhost:8080".to_string()),
        );

        // Add remote nodes
        for i in 1..count {
            nodes.insert(
                format!("node-{i}"),
                NodeInfo::new(format!("node-{i}"), format!("localhost:{}", 8080 + i)),
            );
        }
    }

    #[tokio::test]
    async fn test_load_balancer_new() -> Result<()> {
        let registry = make_registry()?;
        let _lb = LoadBalancer::new(registry, LoadBalancingStrategy::Random);
        Ok(())
    }

    #[tokio::test]
    async fn test_load_balancer_with_health_monitor() -> Result<()> {
        let registry = make_registry()?;
        let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));
        let _lb =
            LoadBalancer::new(registry, LoadBalancingStrategy::Random).with_health_monitor(monitor);
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_empty_cluster() -> Result<()> {
        let registry = make_registry()?;
        let lb = LoadBalancer::new(registry, LoadBalancingStrategy::Random);
        let result = lb.select_node().await;
        assert!(result.is_err(), "Empty cluster should return error");
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_random_single() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 1).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        let node = lb.select_node().await?;
        assert_eq!(node, "self");
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_random_multiple() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 5).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        // Run many selections and verify we get a reasonable distribution
        let mut selected: HashSet<String> = HashSet::new();
        for _ in 0..100 {
            let node = lb.select_node().await?;
            selected.insert(node);
        }

        // With 5 nodes and 100 selections, we should see at least 2 different nodes
        assert!(
            selected.len() >= 2,
            "Random selection should hit multiple nodes, got: {selected:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_round_robin() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 3).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::RoundRobin);

        // Collect one full cycle
        let mut cycle = Vec::new();
        for _ in 0..3 {
            cycle.push(lb.select_node().await?);
        }

        // Should get all 3 unique nodes in one cycle
        let unique: HashSet<_> = cycle.iter().collect();
        assert_eq!(
            unique.len(),
            3,
            "Round-robin should cycle through all nodes"
        );

        // Next cycle should repeat the same order (sorted by node_id)
        let mut second_cycle = Vec::new();
        for _ in 0..3 {
            second_cycle.push(lb.select_node().await?);
        }
        assert_eq!(cycle, second_cycle, "Round-robin should be deterministic");
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_least_connections() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 3).await;

        // Set connection counts via metadata
        {
            let mut nodes = registry.local_nodes.write().await;
            // "self" = 10 connections
            node_mut(&mut nodes, "self")?
                .metadata
                .insert("connections".to_string(), "10".to_string());
            // Ensure past warmup by setting registered_at far in the past
            node_mut(&mut nodes, "self")?
                .metadata
                .insert("registered_at".to_string(), "0".to_string());
            // "node-1" = 5 connections (fewest)
            node_mut(&mut nodes, "node-1")?
                .metadata
                .insert("connections".to_string(), "5".to_string());
            node_mut(&mut nodes, "node-1")?
                .metadata
                .insert("registered_at".to_string(), "0".to_string());
            // "node-2" = 20 connections
            node_mut(&mut nodes, "node-2")?
                .metadata
                .insert("connections".to_string(), "20".to_string());
            node_mut(&mut nodes, "node-2")?
                .metadata
                .insert("registered_at".to_string(), "0".to_string());
        }

        let lb = LoadBalancer::new(
            Arc::clone(&registry),
            LoadBalancingStrategy::LeastConnections,
        );
        let node = lb.select_node().await?;
        assert_eq!(node, "node-1", "Should select node with fewest connections");
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_least_connections_warmup_penalty() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 2).await;

        // "self" has 5 connections, established node
        // "node-1" has no connection metadata and was just registered
        {
            let mut nodes = registry.local_nodes.write().await;
            node_mut(&mut nodes, "self")?
                .metadata
                .insert("connections".to_string(), "5".to_string());
            node_mut(&mut nodes, "self")?
                .metadata
                .insert("registered_at".to_string(), "0".to_string());
            // node-1: recently registered (current time), no connections reported
            let now = chrono::Utc::now().timestamp().to_string();
            node_mut(&mut nodes, "node-1")?
                .metadata
                .insert("registered_at".to_string(), now);
        }

        let lb = LoadBalancer::new(
            Arc::clone(&registry),
            LoadBalancingStrategy::LeastConnections,
        );
        let node = lb.select_node().await?;
        // node-1 is in warmup period with penalty > 5, so "self" should be selected
        assert_eq!(node, "self", "Warmup node should be penalized");
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_by_id_found() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 3).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        let node = lb.select_node_by_id("node-1").await?;
        assert_eq!(node, "node-1");
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_by_id_not_found() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 1).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        let result = lb.select_node_by_id("nonexistent").await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_by_id_filters_unhealthy_nodes() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 3).await;

        let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));
        {
            let mut status = monitor.health_status.write().await;
            status.insert("node-1".to_string(), NodeHealth::Unhealthy);
        }

        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random)
            .with_health_monitor(monitor);

        let err = lb
            .select_node_by_id("node-1")
            .await
            .expect_err("explicit node selection must fail closed for unhealthy nodes");
        assert!(
            err.to_string().contains("not available"),
            "error should explain that unhealthy nodes are not routable: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_by_id_fails_closed_when_degraded_cache_is_stale() -> Result<()> {
        let registry = make_redis_registry("test-stale-select:")?;
        register_nodes(&registry, 2).await;
        registry.test_set_cluster_mode(super::super::node_registry::ClusterMode::Degraded);
        registry.test_set_last_refreshed_at(1);

        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);
        let err = lb
            .select_node_by_id("node-1")
            .await
            .expect_err("stale degraded cache must not be used for explicit routing either");
        assert!(
            err.to_string().contains("stale"),
            "error should preserve the stale-topology fail-closed reason: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_get_available_nodes() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 3).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        let available = lb.get_available_nodes().await?;
        assert_eq!(available.len(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn test_available_count() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 4).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        let count = lb.available_count().await?;
        assert_eq!(count, 4);
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_uses_routable_nodes_in_degraded_mode_when_fresh_enough() -> Result<()>
    {
        let registry = make_registry()?;
        register_nodes(&registry, 2).await;
        registry.test_set_cluster_mode(super::super::node_registry::ClusterMode::Degraded);
        registry.test_set_last_refreshed_at(unix_now_secs_for_test()?);

        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);
        let selected = lb.select_node().await?;
        assert!(
            selected == "self" || selected == "node-1",
            "degraded mode should still route while the cached topology is fresh enough"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_fails_closed_when_degraded_cache_is_stale() -> Result<()> {
        let registry = make_redis_registry("test-stale:")?;
        register_nodes(&registry, 2).await;
        registry.test_set_cluster_mode(super::super::node_registry::ClusterMode::Degraded);
        registry.test_set_last_refreshed_at(1);

        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);
        let err = lb
            .select_node()
            .await
            .expect_err("stale degraded cache must not be used for routing");
        assert!(
            err.to_string().contains("stale"),
            "error should explain the fail-closed stale topology guard: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_filters_unhealthy() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 3).await;

        let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));

        // Mark "node-1" as unhealthy
        {
            let mut status = monitor.health_status.write().await;
            status.insert("node-1".to_string(), NodeHealth::Unhealthy);
        }

        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::RoundRobin)
            .with_health_monitor(Arc::clone(&monitor));

        // Should never select node-1
        let mut selected: HashSet<String> = HashSet::new();
        for _ in 0..20 {
            selected.insert(lb.select_node().await?);
        }

        assert!(
            !selected.contains("node-1"),
            "Unhealthy node should be excluded"
        );
        assert!(selected.contains("self") || selected.contains("node-2"));
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_returns_error_when_all_unhealthy() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 2).await;

        let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));

        // Mark all nodes as unhealthy
        {
            let mut status = monitor.health_status.write().await;
            status.insert("self".to_string(), NodeHealth::Unhealthy);
            status.insert("node-1".to_string(), NodeHealth::Unhealthy);
        }

        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random)
            .with_health_monitor(monitor);

        let node = lb.select_node().await;
        assert!(
            node.is_err(),
            "Must fail closed when all nodes are unhealthy"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_fails_closed_when_health_snapshot_is_stale() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 2).await;

        let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 1));
        {
            let mut status = monitor.health_status.write().await;
            status.insert("self".to_string(), NodeHealth::Healthy);
            status.insert("node-1".to_string(), NodeHealth::Healthy);
        }
        monitor.test_set_last_successful_refresh_at(unix_now_secs_for_test()?.saturating_sub(2));

        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random)
            .with_health_monitor(monitor);

        let err = lb
            .select_node()
            .await
            .expect_err("stale health snapshot must not be used for routing");
        assert!(
            err.to_string().contains("stale"),
            "error should preserve the stale health snapshot reason: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_select_node_allows_routing_before_first_health_refresh() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 2).await;

        let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random)
            .with_health_monitor(monitor);

        let selected = lb.select_node().await?;
        assert!(
            selected == "self" || selected == "node-1",
            "selection should come from currently routable nodes: {selected}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_degraded_nodes_are_not_excluded() -> Result<()> {
        let registry = make_registry()?;
        register_nodes(&registry, 2).await;

        let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));

        // Mark node-1 as Degraded (not Unhealthy)
        {
            let mut status = monitor.health_status.write().await;
            status.insert("node-1".to_string(), NodeHealth::Degraded);
        }

        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::RoundRobin)
            .with_health_monitor(monitor);

        let mut selected: HashSet<String> = HashSet::new();
        for _ in 0..20 {
            selected.insert(lb.select_node().await?);
        }

        assert!(
            selected.contains("node-1"),
            "Degraded nodes should still be included"
        );
        Ok(())
    }
}
