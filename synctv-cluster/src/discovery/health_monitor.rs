//! Health monitoring for cluster nodes
//!
//! Tracks node health via periodic heartbeats and active TCP health probes.
//!
//! **Reconnection resilience**: When the node registry (backed by Redis) becomes
//! unreachable, the monitoring loop applies exponential backoff (2x, 4x, up to 8x
//! the base check interval) to reduce log noise and wasted Redis round-trips.
//! Active TCP probes are also skipped while the registry is unreachable. Once the
//! registry recovers, the monitor resumes normal-interval checks.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

use super::node_registry::NodeRegistry;
#[allow(unused_imports)]
use futures::future::join_all;
use crate::error::Result;

/// Health status of a node
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeHealth {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Configuration for active health probes
#[derive(Debug, Clone)]
pub struct HealthProbeConfig {
    /// TCP connection timeout for health probes
    pub probe_timeout_secs: u64,
    /// Number of consecutive probe failures before marking unhealthy
    pub failure_threshold: u32,
    /// Number of consecutive probe successes before marking healthy
    pub success_threshold: u32,
    /// Interval between active probes (separate from heartbeat checks)
    pub probe_interval_secs: u64,
}

impl Default for HealthProbeConfig {
    fn default() -> Self {
        Self {
            probe_timeout_secs: 3,
            failure_threshold: 2,
            success_threshold: 1,
            probe_interval_secs: 15,
        }
    }
}

/// Probe health state for a single node
#[derive(Debug, Default)]
struct ProbeState {
    /// Consecutive successful probes
    success_count: AtomicU32,
    /// Consecutive failed probes
    failure_count: AtomicU32,
}

/// Health monitor for cluster nodes
///
/// Periodically checks node health via:
/// 1. Passive heartbeat monitoring (based on last_heartbeat timestamp)
/// 2. Active TCP health probes (connect to gRPC port)
pub struct HealthMonitor {
    node_registry: Arc<NodeRegistry>,
    check_interval_secs: u64,
    pub(crate) health_status: Arc<RwLock<std::collections::HashMap<String, NodeHealth>>>,
    cancel_token: CancellationToken,
    /// Active probe configuration
    probe_config: HealthProbeConfig,
    /// Probe state per node
    probe_states: Arc<RwLock<std::collections::HashMap<String, ProbeState>>>,
    /// JoinHandle for the monitoring task, stored so it can be awaited during shutdown
    join_handle: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl HealthMonitor {
    /// Create a new health monitor with default probe config
    #[must_use]
    pub fn new(node_registry: Arc<NodeRegistry>, check_interval_secs: u64) -> Self {
        Self {
            node_registry,
            check_interval_secs,
            health_status: Arc::new(RwLock::new(std::collections::HashMap::new())),
            cancel_token: CancellationToken::new(),
            probe_config: HealthProbeConfig::default(),
            probe_states: Arc::new(RwLock::new(std::collections::HashMap::new())),
            join_handle: tokio::sync::Mutex::new(None),
        }
    }

    /// Create a new health monitor with custom probe configuration
    #[must_use]
    pub fn with_probe_config(node_registry: Arc<NodeRegistry>, check_interval_secs: u64, probe_config: HealthProbeConfig) -> Self {
        Self {
            node_registry,
            check_interval_secs,
            health_status: Arc::new(RwLock::new(std::collections::HashMap::new())),
            cancel_token: CancellationToken::new(),
            probe_config,
            probe_states: Arc::new(RwLock::new(std::collections::HashMap::new())),
            join_handle: tokio::sync::Mutex::new(None),
        }
    }

    /// Store the JoinHandle from `start()` so it can be awaited during shutdown.
    pub fn set_join_handle(&self, handle: tokio::task::JoinHandle<()>) {
        // Use try_lock since this is called once during startup (no contention)
        if let Ok(mut guard) = self.join_handle.try_lock() {
            *guard = Some(handle);
        }
    }

    /// Start health monitoring loop
    ///
    /// Returns the `JoinHandle` so the caller can detect panics or task completion.
    /// Use `shutdown()` to gracefully stop the monitoring loop.
    pub async fn start(&self) -> Result<tokio::task::JoinHandle<()>> {
        let registry = self.node_registry.clone();
        let health_status = self.health_status.clone();
        let timeout_secs = registry.heartbeat_timeout_secs;
        let cancel_token = self.cancel_token.clone();
        let probe_config = self.probe_config.clone();
        let probe_states = self.probe_states.clone();
        let check_interval_secs = self.check_interval_secs;

        let mut probe_timer = interval(Duration::from_secs(probe_config.probe_interval_secs));

        let handle = tokio::spawn(async move {
            /// Maximum backoff multiplier when registry is unreachable.
            /// Caps at 8x the base check interval to avoid excessively long gaps.
            const MAX_BACKOFF_MULTIPLIER: u64 = 8;

            let mut consecutive_registry_failures: u32 = 0;

            loop {
                // Calculate backoff delay for heartbeat checks when registry is unreachable.
                // On consecutive failures, wait longer (exponential backoff capped at 8x)
                // to reduce log noise and wasted Redis round-trips.
                let backoff_multiplier = if consecutive_registry_failures > 0 {
                    (1u64 << consecutive_registry_failures.min(3)).min(MAX_BACKOFF_MULTIPLIER)
                } else {
                    1
                };
                let effective_check_interval = Duration::from_secs(
                    check_interval_secs * backoff_multiplier,
                );

                tokio::select! {
                    () = cancel_token.cancelled() => {
                        tracing::info!("Health monitor shutting down");
                        return;
                    }
                    _ = tokio::time::sleep(effective_check_interval) => {
                        // Passive heartbeat check with backoff on registry failures
                        match registry.get_all_nodes().await {
                            Ok(nodes) => {
                                if consecutive_registry_failures > 0 {
                                    tracing::info!(
                                        previous_failures = consecutive_registry_failures,
                                        "Health monitor reconnected to registry"
                                    );
                                }
                                consecutive_registry_failures = 0;
                                Self::process_heartbeats(&health_status, &nodes, timeout_secs).await;
                            }
                            Err(e) => {
                                consecutive_registry_failures = consecutive_registry_failures.saturating_add(1);
                                if consecutive_registry_failures <= 3 {
                                    tracing::error!(
                                        consecutive_failures = consecutive_registry_failures,
                                        error = %e,
                                        "Failed to get nodes for health check"
                                    );
                                } else {
                                    tracing::debug!(
                                        consecutive_failures = consecutive_registry_failures,
                                        error = %e,
                                        "Failed to get nodes for health check (backoff active)"
                                    );
                                }
                            }
                        }
                    }
                    _ = probe_timer.tick() => {
                        // Active TCP probe -- skip if registry is unreachable
                        if consecutive_registry_failures == 0 {
                            Self::probe_nodes(&registry, &health_status, &probe_config, &probe_states).await;
                        }
                    }
                }
            }
        });

        Ok(handle)
    }

    /// Process heartbeat status for a set of nodes (passive check).
    ///
    /// The caller is responsible for fetching nodes from the registry and
    /// handling errors (with backoff). This function only processes results.
    async fn process_heartbeats(
        health_status: &Arc<RwLock<std::collections::HashMap<String, NodeHealth>>>,
        nodes: &[super::node_registry::NodeInfo],
        timeout_secs: i64,
    ) {
        let mut status = health_status.write().await;
        let node_ids: std::collections::HashSet<String> = nodes.iter().map(|n| n.node_id.clone()).collect();

        for node in nodes {
            // Only update to unhealthy based on heartbeat; probes may override
            let is_alive = !node.is_stale(timeout_secs);
            if !is_alive {
                // Heartbeat expired - mark unhealthy immediately
                let old_status = status.get(&node.node_id);
                if old_status != Some(&NodeHealth::Unhealthy) {
                    tracing::warn!(
                        node_id = %node.node_id,
                        last_heartbeat = ?node.last_heartbeat,
                        "Node marked unhealthy: heartbeat expired"
                    );
                }
                status.insert(node.node_id.clone(), NodeHealth::Unhealthy);
            }
            // If heartbeat is alive, don't override - let probes decide
        }

        // Remove nodes that are no longer in registry
        status.retain(|node_id, _| node_ids.contains(node_id));
    }

    /// Perform active TCP probes on all nodes concurrently
    async fn probe_nodes(
        registry: &Arc<NodeRegistry>,
        health_status: &Arc<RwLock<std::collections::HashMap<String, NodeHealth>>>,
        probe_config: &HealthProbeConfig,
        probe_states: &Arc<RwLock<std::collections::HashMap<String, ProbeState>>>,
    ) {
        let nodes = match registry.get_all_nodes().await {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("Failed to get nodes for probe: {}", e);
                return;
            }
        };

        // Filter nodes to probe (skip those unhealthy with stale heartbeats)
        let heartbeat_timeout = registry.heartbeat_timeout_secs;
        let nodes_to_probe: Vec<_> = {
            let hs = health_status.read().await;
            nodes.into_iter().filter(|node| {
                let current_status = hs.get(&node.node_id).copied();
                if current_status == Some(NodeHealth::Unhealthy) {
                    // Only probe if heartbeat has recovered
                    !node.is_stale(heartbeat_timeout)
                } else {
                    true
                }
            }).collect()
        };

        // Probe all nodes concurrently
        let probe_timeout = probe_config.probe_timeout_secs;
        let probe_results: Vec<_> = futures::future::join_all(
            nodes_to_probe.iter().map(|node| {
                let addr = node.grpc_address.clone();
                async move {
                    Self::probe_node_static(&addr, probe_timeout).await
                }
            })
        ).await;

        // Process results
        let mut states = probe_states.write().await;
        let mut hs = health_status.write().await;
        let active_node_ids: std::collections::HashSet<String> =
            nodes_to_probe.iter().map(|n| n.node_id.clone()).collect();

        for (node, probe_success) in nodes_to_probe.iter().zip(probe_results) {
            let state = states.entry(node.node_id.clone()).or_default();

            let new_status = if probe_success {
                state.failure_count.store(0, Ordering::Relaxed);
                let successes = state.success_count.fetch_add(1, Ordering::Relaxed) + 1;

                if successes >= probe_config.success_threshold {
                    Some(NodeHealth::Healthy)
                } else {
                    None
                }
            } else {
                state.success_count.store(0, Ordering::Relaxed);
                let failures = state.failure_count.fetch_add(1, Ordering::Relaxed) + 1;

                if failures >= probe_config.failure_threshold {
                    if failures == probe_config.failure_threshold {
                        tracing::warn!(
                            node_id = %node.node_id,
                            consecutive_failures = failures,
                            "Node marked as unhealthy after consecutive probe failures"
                        );
                    }
                    Some(NodeHealth::Unhealthy)
                } else {
                    Some(NodeHealth::Degraded)
                }
            };

            if let Some(status) = new_status {
                let old_status = hs.get(&node.node_id);

                if old_status != Some(&status) {
                    match status {
                        NodeHealth::Healthy => {
                            tracing::info!(node_id = %node.node_id, "Node is healthy (probe)");
                        }
                        NodeHealth::Degraded => {
                            tracing::warn!(node_id = %node.node_id, "Node is degraded (probe)");
                        }
                        NodeHealth::Unhealthy => {
                            tracing::warn!(node_id = %node.node_id, "Node is unhealthy (probe)");
                        }
                    }
                }

                hs.insert(node.node_id.clone(), status);
            }
        }

        // Prune probe_states for nodes no longer in the registry
        states.retain(|node_id, _| active_node_ids.contains(node_id));
    }

    /// Static probe function for use in async context.
    /// Supports both IPv4 ("host:port") and IPv6 ("[::1]:port") addresses.
    async fn probe_node_static(grpc_address: &str, timeout_secs: u64) -> bool {
        // Try parsing as a SocketAddr first (handles IPv6 like [::1]:50051)
        // then fall back to rsplit_once for "host:port" format
        let addr = if grpc_address.parse::<std::net::SocketAddr>().is_ok() {
            grpc_address.to_string()
        } else if let Some((host, port_str)) = grpc_address.rsplit_once(':') {
            if port_str.parse::<u16>().is_err() {
                return false;
            }
            format!("{host}:{port_str}")
        } else {
            return false;
        };

        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            tokio::net::TcpStream::connect(&addr),
        )
        .await;

        match result {
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
        }
    }

    /// Gracefully shut down the health monitoring loop.
    ///
    /// Cancels the monitoring task and awaits its completion (with a timeout).
    pub async fn shutdown(&self) {
        self.cancel_token.cancel();
        let handle = self.join_handle.lock().await.take();
        if let Some(handle) = handle {
            match tokio::time::timeout(Duration::from_secs(5), handle).await {
                Ok(Ok(())) => tracing::info!("Health monitor task completed"),
                Ok(Err(e)) => tracing::warn!("Health monitor task panicked: {}", e),
                Err(_) => tracing::warn!("Health monitor task did not finish within 5s timeout"),
            }
        }
    }

    /// Get health status of all nodes
    pub async fn get_all_status(&self) -> std::collections::HashMap<String, NodeHealth> {
        let status = self.health_status.read().await;
        status.clone()
    }

    /// Get health status of a specific node
    pub async fn get_node_status(&self, node_id: &str) -> Option<NodeHealth> {
        let status = self.health_status.read().await;
        status.get(node_id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Helper: create a NodeRegistry in local-only mode (no Redis)
    fn make_registry() -> Arc<NodeRegistry> {
        Arc::new(NodeRegistry::new(None, "self".to_string(), 30, "test:").unwrap())
    }

    // --- HealthProbeConfig tests ---

    #[test]
    fn test_health_probe_config_defaults() {
        let config = HealthProbeConfig::default();
        assert_eq!(config.probe_timeout_secs, 3);
        assert_eq!(config.failure_threshold, 2);
        assert_eq!(config.success_threshold, 1);
        assert_eq!(config.probe_interval_secs, 15);
    }

    // --- NodeHealth enum tests ---

    #[test]
    fn test_node_health_equality() {
        assert_eq!(NodeHealth::Healthy, NodeHealth::Healthy);
        assert_eq!(NodeHealth::Degraded, NodeHealth::Degraded);
        assert_eq!(NodeHealth::Unhealthy, NodeHealth::Unhealthy);
        assert_ne!(NodeHealth::Healthy, NodeHealth::Unhealthy);
        assert_ne!(NodeHealth::Healthy, NodeHealth::Degraded);
    }

    // --- HealthMonitor construction ---

    #[tokio::test]
    async fn test_health_monitor_new() {
        let registry = make_registry();
        let monitor = HealthMonitor::new(registry, 10);
        let status = monitor.get_all_status().await;
        assert!(status.is_empty(), "New monitor should have no statuses");
    }

    #[tokio::test]
    async fn test_health_monitor_with_probe_config() {
        let registry = make_registry();
        let config = HealthProbeConfig {
            probe_timeout_secs: 1,
            failure_threshold: 5,
            success_threshold: 3,
            probe_interval_secs: 30,
        };
        let monitor = HealthMonitor::with_probe_config(registry, 10, config);
        assert_eq!(monitor.probe_config.failure_threshold, 5);
        assert_eq!(monitor.probe_config.success_threshold, 3);
    }

    // --- get_node_status ---

    #[tokio::test]
    async fn test_get_node_status_returns_none_for_unknown() {
        let registry = make_registry();
        let monitor = HealthMonitor::new(registry, 10);
        assert_eq!(monitor.get_node_status("nonexistent").await, None);
    }

    // --- process_heartbeats (passive check) ---

    #[tokio::test]
    async fn test_process_heartbeats_does_not_mark_fresh_nodes() {
        let registry = make_registry();
        registry.register("localhost:50051".to_string(), "localhost:8080".to_string()).await.unwrap();

        let health_status = Arc::new(RwLock::new(HashMap::new()));
        let nodes = registry.get_all_nodes().await.unwrap();
        HealthMonitor::process_heartbeats(&health_status, &nodes, 30).await;

        let status = health_status.read().await;
        // Fresh node should not be marked unhealthy
        let self_status = status.get("self");
        assert!(self_status.is_none() || *self_status.unwrap() != NodeHealth::Unhealthy);
    }

    #[tokio::test]
    async fn test_process_heartbeats_prunes_removed_nodes() {
        let registry = make_registry();
        registry.register("localhost:50051".to_string(), "localhost:8080".to_string()).await.unwrap();

        let health_status = Arc::new(RwLock::new(HashMap::new()));
        // Pre-populate with a node that no longer exists
        {
            let mut status = health_status.write().await;
            status.insert("ghost-node".to_string(), NodeHealth::Healthy);
        }

        let nodes = registry.get_all_nodes().await.unwrap();
        HealthMonitor::process_heartbeats(&health_status, &nodes, 30).await;

        let status = health_status.read().await;
        assert!(!status.contains_key("ghost-node"), "Removed node should be pruned");
    }

    // --- probe_node_static ---

    #[tokio::test]
    async fn test_probe_node_static_invalid_address() {
        // Invalid address should return false
        assert!(!HealthMonitor::probe_node_static("not-an-address", 1).await);
    }

    #[tokio::test]
    #[ignore] // Flaky: network-dependent test, may fail in some environments
    async fn test_probe_node_static_unreachable() {
        // Unreachable address should return false (timeout)
        assert!(!HealthMonitor::probe_node_static("192.0.2.1:12345", 1).await);
    }

    #[tokio::test]
    async fn test_probe_node_static_no_port() {
        assert!(!HealthMonitor::probe_node_static("localhost", 1).await);
    }

    #[tokio::test]
    async fn test_probe_node_static_invalid_port() {
        assert!(!HealthMonitor::probe_node_static("localhost:abc", 1).await);
    }

    // --- ProbeState ---

    #[test]
    fn test_probe_state_default() {
        let state = ProbeState::default();
        assert_eq!(state.success_count.load(Ordering::Relaxed), 0);
        assert_eq!(state.failure_count.load(Ordering::Relaxed), 0);
    }

    // --- start / shutdown lifecycle ---

    #[tokio::test]
    async fn test_health_monitor_start_and_shutdown() {
        let registry = make_registry();
        let monitor = HealthMonitor::new(registry, 60);
        let handle = monitor.start().await.unwrap();
        monitor.set_join_handle(handle);

        // Let it run briefly
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Shutdown should complete without error
        monitor.shutdown().await;
    }

    // --- probe_nodes with threshold logic ---

    #[tokio::test]
    async fn test_probe_failure_threshold() {
        // Test that probe_states track consecutive failures
        let state = ProbeState::default();

        // Simulate 3 failures
        for _ in 0..3 {
            state.success_count.store(0, Ordering::Relaxed);
            let failures = state.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
            assert!(failures <= 3);
        }

        assert_eq!(state.failure_count.load(Ordering::Relaxed), 3);
        assert_eq!(state.success_count.load(Ordering::Relaxed), 0);

        // Simulate one success resetting failures
        state.failure_count.store(0, Ordering::Relaxed);
        state.success_count.fetch_add(1, Ordering::Relaxed);

        assert_eq!(state.failure_count.load(Ordering::Relaxed), 0);
        assert_eq!(state.success_count.load(Ordering::Relaxed), 1);
    }
}
