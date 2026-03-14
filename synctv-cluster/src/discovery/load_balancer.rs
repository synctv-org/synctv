//! Load balancing for cluster requests
//!
//! Distributes requests across available cluster nodes.
//! Integrates with `HealthMonitor` to exclude unhealthy nodes.

use rand::prelude::IndexedRandom;
use std::sync::Arc;

use super::health_monitor::{HealthMonitor, NodeHealth};
use super::node_registry::{NodeInfo, NodeRegistry};
use crate::error::{Error, Result};

/// Load balancing strategy
#[derive(Debug, Clone, Copy)]
pub enum LoadBalancingStrategy {
    /// Random selection
    Random,
    /// Round-robin
    RoundRobin,
    /// Least connections (select node with fewest active connections)
    /// Nodes must report connection count in metadata["connections"] via heartbeat.
    LeastConnections,
}

/// Load balancer for cluster node selection
pub struct LoadBalancer {
    node_registry: Arc<NodeRegistry>,
    health_monitor: Option<Arc<HealthMonitor>>,
    strategy: LoadBalancingStrategy,
    round_robin_index: std::sync::atomic::AtomicUsize,
}

impl LoadBalancer {
    /// Create a new load balancer
    #[must_use]
    pub const fn new(node_registry: Arc<NodeRegistry>, strategy: LoadBalancingStrategy) -> Self {
        Self {
            node_registry,
            health_monitor: None,
            strategy,
            round_robin_index: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Attach a health monitor to filter out unhealthy nodes
    #[must_use]
    pub fn with_health_monitor(mut self, monitor: Arc<HealthMonitor>) -> Self {
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
                // Select node with fewest connections based on metadata.
                // Nodes report connection count in metadata["connections"] via heartbeat.
                //
                // For nodes without connection metadata (newly joined), we use a
                // warmup penalty to avoid immediately routing traffic to them.
                // The penalty is based on registered_at timestamp - nodes registered
                // within the last 60 seconds get a higher effective connection count
                // to reduce the "thundering herd" problem on new nodes.
                const WARMUP_PERIOD_SECS: i64 = 60;
                const WARMUP_PENALTY: usize = 1000; // High value to deprioritize new nodes

                let now = chrono::Utc::now();

                nodes
                    .iter()
                    .min_by_key(|n| {
                        let connections = n.metadata
                            .get("connections")
                            .and_then(|v| v.parse::<usize>().ok());

                        if let Some(conn) = connections { conn } else {
                            // Node hasn't reported connections - check if in warmup
                            let registered_at = n.metadata
                                .get("registered_at")
                                .and_then(|v| v.parse::<i64>().ok())
                                .unwrap_or(0);

                            let age_secs = now.timestamp() - registered_at;
                            if age_secs < WARMUP_PERIOD_SECS {
                                // In warmup period - apply penalty that decreases over time
                                let warmup_progress = age_secs as f64 / WARMUP_PERIOD_SECS as f64;
                                let penalty = (WARMUP_PENALTY as f64 * (1.0 - warmup_progress)) as usize;
                                tracing::trace!(
                                    node_id = %n.node_id,
                                    age_secs = age_secs,
                                    effective_connections = penalty,
                                    "Node in warmup period"
                                );
                                penalty
                            } else {
                                // Past warmup with no connection data - treat as empty
                                tracing::debug!(
                                    node_id = %n.node_id,
                                    "Node has no connection metadata after warmup, treating as empty"
                                );
                                0
                            }
                        }
                    })
                    .ok_or_else(|| Error::NotFound("No nodes available".to_string()))?
                    .node_id
                    .clone()
            }
        };

        Ok(selected_node)
    }

    /// Select a specific node by ID (returns error if node not available)
    pub async fn select_node_by_id(&self, node_id: &str) -> Result<String> {
        let node = self
            .node_registry
            .get_node_local(node_id)
            .await
            .ok_or_else(|| Error::NotFound(format!("Node {node_id} not found")))?;

        Ok(node.node_id)
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
    use super::*;
    use std::collections::HashSet;

    /// Helper: create a NodeRegistry (redis::Client::open succeeds without a running server)
    fn make_registry() -> Arc<NodeRegistry> {
        Arc::new(NodeRegistry::new_local_only("self".to_string(), 30, "test:").unwrap())
    }

    /// Helper: populate N nodes directly into the local cache (no Redis required)
    async fn register_nodes(registry: &NodeRegistry, count: usize) {
        let mut nodes = registry.local_nodes.write().await;
        // Register "self" first
        nodes.insert(
            "self".to_string(),
            NodeInfo::new(
                "self".to_string(),
                "localhost:50051".to_string(),
                "localhost:8080".to_string(),
            ),
        );

        // Add remote nodes
        for i in 1..count {
            nodes.insert(
                format!("node-{i}"),
                NodeInfo::new(
                    format!("node-{i}"),
                    format!("localhost:{}", 50051 + i),
                    format!("localhost:{}", 8080 + i),
                ),
            );
        }
    }

    // --- Construction ---

    #[tokio::test]
    async fn test_load_balancer_new() {
        let registry = make_registry();
        let _lb = LoadBalancer::new(registry, LoadBalancingStrategy::Random);
    }

    #[tokio::test]
    async fn test_load_balancer_with_health_monitor() {
        let registry = make_registry();
        let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));
        let _lb =
            LoadBalancer::new(registry, LoadBalancingStrategy::Random).with_health_monitor(monitor);
    }

    // --- select_node: empty cluster ---

    #[tokio::test]
    async fn test_select_node_empty_cluster() {
        let registry = make_registry();
        let lb = LoadBalancer::new(registry, LoadBalancingStrategy::Random);
        let result = lb.select_node().await;
        assert!(result.is_err(), "Empty cluster should return error");
    }

    // --- select_node: Random strategy ---

    #[tokio::test]
    async fn test_select_node_random_single() {
        let registry = make_registry();
        register_nodes(&registry, 1).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        let node = lb.select_node().await.unwrap();
        assert_eq!(node, "self");
    }

    #[tokio::test]
    async fn test_select_node_random_multiple() {
        let registry = make_registry();
        register_nodes(&registry, 5).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        // Run many selections and verify we get a reasonable distribution
        let mut selected: HashSet<String> = HashSet::new();
        for _ in 0..100 {
            let node = lb.select_node().await.unwrap();
            selected.insert(node);
        }

        // With 5 nodes and 100 selections, we should see at least 2 different nodes
        assert!(
            selected.len() >= 2,
            "Random selection should hit multiple nodes, got: {selected:?}"
        );
    }

    // --- select_node: RoundRobin strategy ---

    #[tokio::test]
    async fn test_select_node_round_robin() {
        let registry = make_registry();
        register_nodes(&registry, 3).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::RoundRobin);

        // Collect one full cycle
        let mut cycle = Vec::new();
        for _ in 0..3 {
            cycle.push(lb.select_node().await.unwrap());
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
            second_cycle.push(lb.select_node().await.unwrap());
        }
        assert_eq!(cycle, second_cycle, "Round-robin should be deterministic");
    }

    // --- select_node: LeastConnections strategy ---

    #[tokio::test]
    async fn test_select_node_least_connections() {
        let registry = make_registry();
        register_nodes(&registry, 3).await;

        // Set connection counts via metadata
        {
            let mut nodes = registry.local_nodes.write().await;
            // "self" = 10 connections
            nodes
                .get_mut("self")
                .unwrap()
                .metadata
                .insert("connections".to_string(), "10".to_string());
            // Ensure past warmup by setting registered_at far in the past
            nodes
                .get_mut("self")
                .unwrap()
                .metadata
                .insert("registered_at".to_string(), "0".to_string());
            // "node-1" = 5 connections (fewest)
            nodes
                .get_mut("node-1")
                .unwrap()
                .metadata
                .insert("connections".to_string(), "5".to_string());
            nodes
                .get_mut("node-1")
                .unwrap()
                .metadata
                .insert("registered_at".to_string(), "0".to_string());
            // "node-2" = 20 connections
            nodes
                .get_mut("node-2")
                .unwrap()
                .metadata
                .insert("connections".to_string(), "20".to_string());
            nodes
                .get_mut("node-2")
                .unwrap()
                .metadata
                .insert("registered_at".to_string(), "0".to_string());
        }

        let lb = LoadBalancer::new(
            Arc::clone(&registry),
            LoadBalancingStrategy::LeastConnections,
        );
        let node = lb.select_node().await.unwrap();
        assert_eq!(node, "node-1", "Should select node with fewest connections");
    }

    #[tokio::test]
    async fn test_select_node_least_connections_warmup_penalty() {
        let registry = make_registry();
        register_nodes(&registry, 2).await;

        // "self" has 5 connections, established node
        // "node-1" has no connection metadata and was just registered
        {
            let mut nodes = registry.local_nodes.write().await;
            nodes
                .get_mut("self")
                .unwrap()
                .metadata
                .insert("connections".to_string(), "5".to_string());
            nodes
                .get_mut("self")
                .unwrap()
                .metadata
                .insert("registered_at".to_string(), "0".to_string());
            // node-1: recently registered (current time), no connections reported
            let now = chrono::Utc::now().timestamp().to_string();
            nodes
                .get_mut("node-1")
                .unwrap()
                .metadata
                .insert("registered_at".to_string(), now);
        }

        let lb = LoadBalancer::new(
            Arc::clone(&registry),
            LoadBalancingStrategy::LeastConnections,
        );
        let node = lb.select_node().await.unwrap();
        // node-1 is in warmup period with penalty > 5, so "self" should be selected
        assert_eq!(node, "self", "Warmup node should be penalized");
    }

    // --- select_node_by_id ---

    #[tokio::test]
    async fn test_select_node_by_id_found() {
        let registry = make_registry();
        register_nodes(&registry, 3).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        let node = lb.select_node_by_id("node-1").await.unwrap();
        assert_eq!(node, "node-1");
    }

    #[tokio::test]
    async fn test_select_node_by_id_not_found() {
        let registry = make_registry();
        register_nodes(&registry, 1).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        let result = lb.select_node_by_id("nonexistent").await;
        assert!(result.is_err());
    }

    // --- get_available_nodes ---

    #[tokio::test]
    async fn test_get_available_nodes() {
        let registry = make_registry();
        register_nodes(&registry, 3).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        let available = lb.get_available_nodes().await.unwrap();
        assert_eq!(available.len(), 3);
    }

    #[tokio::test]
    async fn test_available_count() {
        let registry = make_registry();
        register_nodes(&registry, 4).await;
        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

        let count = lb.available_count().await.unwrap();
        assert_eq!(count, 4);
    }

    #[tokio::test]
    async fn test_select_node_uses_routable_nodes_in_degraded_mode_when_fresh_enough() {
        let registry = make_registry();
        register_nodes(&registry, 2).await;
        registry.test_set_cluster_mode(super::super::node_registry::ClusterMode::Degraded);
        registry.test_set_last_refreshed_at(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );

        let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);
        let selected = lb.select_node().await.unwrap();
        assert!(
            selected == "self" || selected == "node-1",
            "degraded mode should still route while the cached topology is fresh enough"
        );
    }

    #[tokio::test]
    async fn test_select_node_fails_closed_when_degraded_cache_is_stale() {
        let registry = Arc::new(
            NodeRegistry::new(
                redis::Client::open("redis://127.0.0.1:1").unwrap(),
                "self".to_string(),
                30,
                "test-stale:",
            )
            .unwrap(),
        );
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
    }

    // --- Health filtering ---

    #[tokio::test]
    async fn test_select_node_filters_unhealthy() {
        let registry = make_registry();
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
            selected.insert(lb.select_node().await.unwrap());
        }

        assert!(
            !selected.contains("node-1"),
            "Unhealthy node should be excluded"
        );
        assert!(selected.contains("self") || selected.contains("node-2"));
    }

    #[tokio::test]
    async fn test_select_node_returns_error_when_all_unhealthy() {
        let registry = make_registry();
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
    }

    #[tokio::test]
    async fn test_degraded_nodes_are_not_excluded() {
        let registry = make_registry();
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
            selected.insert(lb.select_node().await.unwrap());
        }

        assert!(
            selected.contains("node-1"),
            "Degraded nodes should still be included"
        );
    }
}
