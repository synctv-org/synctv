//! CL8: `LoadBalancer` unhealthy exclusion
//!
//! - 3 nodes: 2 Unhealthy + 1 Healthy -> only healthy selected (10 calls)
//! - All unhealthy -> fail closed (returns error)
//! - Degraded nodes are still selectable

#![allow(clippy::unwrap_used)]
use std::collections::HashSet;
use std::sync::Arc;

use synctv_cluster::discovery::health_monitor::HealthMonitor;
use synctv_cluster::discovery::load_balancer::{LoadBalancer, LoadBalancingStrategy};
use synctv_cluster::discovery::node_registry::{NodeInfo, NodeRegistry};
use synctv_cluster::NodeHealth;

/// Helper: create a local-only `NodeRegistry` and populate it with nodes.
async fn setup_registry(node_ids: &[&str]) -> Arc<NodeRegistry> {
    let registry = Arc::new(NodeRegistry::new_local_only("self".to_string(), 30, "cl8test:").unwrap());

    for id in node_ids {
        let node = NodeInfo::new(id.to_string(), format!("{id}:50051"), format!("{id}:8080"));
        registry.test_insert_local(node).await;
    }

    registry
}

/// 2 Unhealthy + 1 Healthy -> only the healthy node is selected (10 calls).
#[tokio::test]
async fn test_only_healthy_node_selected() {
    let registry = setup_registry(&["node-a", "node-b", "node-c"]).await;
    let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));

    // Mark 2 nodes as Unhealthy, 1 as Healthy
    {
        let mut status = monitor.health_status.write().await;
        status.insert("node-a".to_string(), NodeHealth::Unhealthy);
        status.insert("node-b".to_string(), NodeHealth::Unhealthy);
        status.insert("node-c".to_string(), NodeHealth::Healthy);
    }

    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random)
        .with_health_monitor(Arc::clone(&monitor));

    // Select 10 times - should always get node-c
    for i in 0..10 {
        let selected = lb.select_node().await.unwrap();
        assert_eq!(
            selected, "node-c",
            "Only healthy node-c should be selected, got {selected} on iteration {i}"
        );
    }
}

/// All unhealthy -> `LoadBalancer` fails closed instead of routing to known-bad nodes.
#[tokio::test]
async fn test_all_unhealthy_returns_error() {
    let registry = setup_registry(&["node-a", "node-b", "node-c"]).await;
    let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));

    // Mark all nodes as Unhealthy
    {
        let mut status = monitor.health_status.write().await;
        status.insert("node-a".to_string(), NodeHealth::Unhealthy);
        status.insert("node-b".to_string(), NodeHealth::Unhealthy);
        status.insert("node-c".to_string(), NodeHealth::Unhealthy);
    }

    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random)
        .with_health_monitor(monitor);

    let selected = lb.select_node().await;
    assert!(
        selected.is_err(),
        "Should fail closed when all nodes are unhealthy"
    );
}

/// Degraded nodes should still be selectable (not excluded like Unhealthy).
#[tokio::test]
async fn test_degraded_nodes_selectable() {
    let registry = setup_registry(&["node-a", "node-b", "node-c"]).await;
    let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));

    // Mark all nodes as Degraded
    {
        let mut status = monitor.health_status.write().await;
        status.insert("node-a".to_string(), NodeHealth::Degraded);
        status.insert("node-b".to_string(), NodeHealth::Degraded);
        status.insert("node-c".to_string(), NodeHealth::Degraded);
    }

    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::RoundRobin)
        .with_health_monitor(monitor);

    let mut selected_ids = HashSet::new();
    for _ in 0..20 {
        let node = lb.select_node().await.unwrap();
        selected_ids.insert(node);
    }

    // All degraded nodes should be selectable
    assert!(
        selected_ids.len() >= 2,
        "Degraded nodes should be selectable, got {selected_ids:?}"
    );
}

/// Round-robin strategy should cycle through healthy nodes only.
#[tokio::test]
async fn test_round_robin_cycles_through_healthy() {
    let registry = setup_registry(&["node-a", "node-b", "node-c"]).await;
    let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));

    // Mark node-a as unhealthy
    {
        let mut status = monitor.health_status.write().await;
        status.insert("node-a".to_string(), NodeHealth::Unhealthy);
        status.insert("node-b".to_string(), NodeHealth::Healthy);
        status.insert("node-c".to_string(), NodeHealth::Healthy);
    }

    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::RoundRobin)
        .with_health_monitor(monitor);

    let mut seen_b = false;
    let mut seen_c = false;
    for _ in 0..10 {
        let selected = lb.select_node().await.unwrap();
        assert_ne!(
            selected, "node-a",
            "Unhealthy node-a should not be selected"
        );
        match selected.as_str() {
            "node-b" => seen_b = true,
            "node-c" => seen_c = true,
            _ => {}
        }
    }
    assert!(
        seen_b && seen_c,
        "Round-robin should cycle through both healthy nodes"
    );
}

/// Empty registry -> error.
#[tokio::test]
async fn test_empty_registry_returns_error() {
    let registry = Arc::new(NodeRegistry::new_local_only(
        "self".to_string(),
        30,
        "cl8empty:",
    ).unwrap());

    let lb = LoadBalancer::new(registry, LoadBalancingStrategy::Random);
    let selected = lb.select_node().await;
    assert!(selected.is_err(), "Should return error for empty registry");
}

/// Nodes with no health status entry should be treated as healthy (new nodes).
#[tokio::test]
async fn test_unknown_health_treated_as_healthy() {
    let registry = setup_registry(&["node-a", "node-b", "node-c"]).await;
    let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));

    // Only mark node-a as Unhealthy; node-b and node-c have no status entry
    {
        let mut status = monitor.health_status.write().await;
        status.insert("node-a".to_string(), NodeHealth::Unhealthy);
        // node-b and node-c: no entry -> should be treated as healthy
    }

    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random)
        .with_health_monitor(monitor);

    let mut selected_ids = HashSet::new();
    for _ in 0..30 {
        let selected = lb.select_node().await.unwrap();
        selected_ids.insert(selected);
    }

    assert!(
        !selected_ids.contains("node-a"),
        "Unhealthy node-a should not be selected"
    );
    assert!(
        selected_ids.contains("node-b") || selected_ids.contains("node-c"),
        "Nodes without health status should be selectable, got {selected_ids:?}"
    );
}
