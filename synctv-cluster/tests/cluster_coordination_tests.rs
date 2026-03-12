//! Integration tests for cluster coordination using actual implementations
//!
//! Tests use local-mode `NodeRegistry` (no Redis) to validate cluster coordination
//! logic without external dependencies.
//!
//! Run with: cargo test --test `cluster_coordination_tests`

#![allow(clippy::unwrap_used)]
use std::collections::HashSet;
use std::sync::Arc;
use synctv_cluster::discovery::{
    HealthMonitor, LoadBalancer, LoadBalancingStrategy, NodeInfo, NodeRegistry,
};

/// Helper: create a local-only `NodeRegistry` for coordination logic tests.
fn make_registry(node_id: &str) -> Arc<NodeRegistry> {
    Arc::new(NodeRegistry::new_local_only(node_id.to_string(), 30, "test:").unwrap())
}

// =====================================================================
// NodeInfo tests
// =====================================================================

#[tokio::test]
async fn test_node_info_serialization() {
    let node = NodeInfo {
        node_id: "node-123".to_string(),
        grpc_address: "127.0.0.1:50051".to_string(),
        http_address: "127.0.0.1:8080".to_string(),
        epoch: 0,
        last_heartbeat: chrono::Utc::now(),
        metadata: Default::default(),
    };

    let json = serde_json::to_string(&node).expect("Failed to serialize");
    let deserialized: NodeInfo = serde_json::from_str(&json).expect("Failed to deserialize");

    assert_eq!(node.node_id, deserialized.node_id);
    assert_eq!(node.grpc_address, deserialized.grpc_address);
    assert_eq!(node.http_address, deserialized.http_address);
    assert_eq!(node.epoch, deserialized.epoch);
}

#[tokio::test]
async fn test_node_info_is_stale() {
    let mut node = NodeInfo::new(
        "node-1".to_string(),
        "localhost:50051".to_string(),
        "localhost:8080".to_string(),
    );

    // Fresh node should not be stale
    assert!(!node.is_stale(30));

    // Make heartbeat old
    node.last_heartbeat = chrono::Utc::now() - chrono::Duration::seconds(60);
    assert!(node.is_stale(30));
}

// =====================================================================
// NodeRegistry tests (actual implementation, local mode)
// =====================================================================

#[tokio::test]
async fn test_node_registry_register_and_get() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    let node = registry.test_get_local("self").await;
    assert!(node.is_some());
    let node = node.unwrap();
    assert_eq!(node.node_id, "self");
    assert_eq!(node.grpc_address, "localhost:50051");
}

#[tokio::test]
async fn test_node_registry_concurrent_registration() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    // Register 9 additional remote nodes concurrently
    let mut handles = vec![];
    for i in 1..10 {
        let reg = registry.clone();
        let handle = tokio::spawn(async move {
            let node = NodeInfo::new(
                format!("node-{i}"),
                format!("localhost:{}", 50051 + i),
                format!("localhost:{}", 8080 + i),
            );
            reg.test_insert_local(node).await;
        });
        handles.push(handle);
    }

    futures::future::join_all(handles).await;

    let all_nodes = registry.test_get_all_local().await;
    assert_eq!(all_nodes.len(), 10);
}

#[tokio::test]
async fn test_node_registry_unregister() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    let remote = NodeInfo::new(
        "remote-1".to_string(),
        "localhost:50052".to_string(),
        "localhost:8081".to_string(),
    );
    registry.test_insert_local(remote).await;

    assert_eq!(registry.test_get_all_local().await.len(), 2);

    // Unregister the remote node
    registry.test_remove_local("remote-1").await;
    assert_eq!(registry.test_get_all_local().await.len(), 1);
}

#[tokio::test]
async fn test_node_registry_get_nonexistent() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    let node = registry.test_get_local("nonexistent").await;
    assert!(node.is_none());
}

// =====================================================================
// HealthMonitor tests (actual implementation)
// =====================================================================

#[tokio::test]
async fn test_health_monitor_initial_state() {
    let registry = make_registry("self");
    let monitor = HealthMonitor::new(registry, 60);

    let status = monitor.get_all_status().await;
    assert!(status.is_empty());
    assert_eq!(monitor.get_node_status("self").await, None);
}

#[tokio::test]
async fn test_health_monitor_start_and_shutdown() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    let monitor = HealthMonitor::new(registry, 60);
    let handle = monitor.start().await.unwrap();
    monitor.set_join_handle(handle);

    // Let it run briefly
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Shutdown should complete cleanly
    monitor.shutdown().await;
}

// =====================================================================
// LoadBalancer tests (actual implementation, local mode)
// =====================================================================

#[tokio::test]
async fn test_load_balancer_empty_cluster_returns_error() {
    let registry = make_registry("self");
    let lb = LoadBalancer::new(registry, LoadBalancingStrategy::Random);
    assert!(lb.select_node().await.is_err());
}

#[tokio::test]
async fn test_load_balancer_single_node() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);
    let node = lb.select_node().await.unwrap();
    assert_eq!(node, "self");
}

#[tokio::test]
async fn test_load_balancer_round_robin_cycles() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    for i in 1..3 {
        let node = NodeInfo::new(
            format!("node-{i}"),
            format!("localhost:{}", 50051 + i),
            format!("localhost:{}", 8080 + i),
        );
        registry.test_insert_local(node).await;
    }

    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::RoundRobin);

    // Collect first cycle
    let mut first_cycle = Vec::new();
    for _ in 0..3 {
        first_cycle.push(lb.select_node().await.unwrap());
    }

    // Should get all 3 unique nodes
    let unique: HashSet<_> = first_cycle.iter().collect();
    assert_eq!(
        unique.len(),
        3,
        "Round-robin should visit all nodes in one cycle"
    );

    // Second cycle should match the first (deterministic)
    let mut second_cycle = Vec::new();
    for _ in 0..3 {
        second_cycle.push(lb.select_node().await.unwrap());
    }
    assert_eq!(first_cycle, second_cycle);
}

#[tokio::test]
async fn test_load_balancer_random_distributes() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    for i in 1..5 {
        let node = NodeInfo::new(
            format!("node-{i}"),
            format!("localhost:{}", 50051 + i),
            format!("localhost:{}", 8080 + i),
        );
        registry.test_insert_local(node).await;
    }

    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

    let mut selected = HashSet::new();
    for _ in 0..100 {
        selected.insert(lb.select_node().await.unwrap());
    }

    assert!(
        selected.len() >= 2,
        "Random should hit multiple nodes over 100 selections"
    );
}

#[tokio::test]
async fn test_load_balancer_with_health_monitor_no_status() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    let remote = NodeInfo::new(
        "node-1".to_string(),
        "localhost:50052".to_string(),
        "localhost:8081".to_string(),
    );
    registry.test_insert_local(remote).await;

    // Monitor has no statuses yet -- all nodes should be available
    let monitor = Arc::new(HealthMonitor::new(Arc::clone(&registry), 60));
    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::RoundRobin)
        .with_health_monitor(monitor);

    let available = lb.get_available_nodes().await.unwrap();
    assert_eq!(
        available.len(),
        2,
        "All nodes available when health monitor has no data"
    );
}

#[tokio::test]
async fn test_load_balancer_available_count() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    for i in 1..4 {
        let node = NodeInfo::new(
            format!("node-{i}"),
            format!("localhost:{}", 50051 + i),
            format!("localhost:{}", 8080 + i),
        );
        registry.test_insert_local(node).await;
    }

    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);
    assert_eq!(lb.available_count().await.unwrap(), 4);
}

#[tokio::test]
async fn test_load_balancer_select_by_id() {
    let registry = make_registry("self");
    registry
        .test_insert_local(NodeInfo::new(
            "self".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        ))
        .await;

    let lb = LoadBalancer::new(Arc::clone(&registry), LoadBalancingStrategy::Random);

    assert!(lb.select_node_by_id("self").await.is_ok());
    assert!(lb.select_node_by_id("nonexistent").await.is_err());
}

// =====================================================================
// Quorum logic (algorithmic test, no mock -- pure function)
// =====================================================================

#[tokio::test]
async fn test_quorum_validation() {
    fn has_quorum(alive_nodes: usize, total_nodes: usize) -> bool {
        alive_nodes > total_nodes / 2
    }

    // 5 node cluster
    assert!(has_quorum(3, 5), "3/5 nodes have quorum");
    assert!(has_quorum(4, 5), "4/5 nodes have quorum");
    assert!(has_quorum(5, 5), "5/5 nodes have quorum");
    assert!(!has_quorum(2, 5), "2/5 nodes do not have quorum");
    assert!(!has_quorum(1, 5), "1/5 nodes do not have quorum");

    // 3 node cluster
    assert!(has_quorum(2, 3), "2/3 nodes have quorum");
    assert!(!has_quorum(1, 3), "1/3 nodes do not have quorum");

    // Single node (always has quorum)
    assert!(has_quorum(1, 1), "Single node has quorum");
}
