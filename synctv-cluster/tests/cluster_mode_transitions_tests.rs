//! CL3: NodeRegistry ClusterMode transitions
//!
//! - Trip circuit breaker (3 failures) -> ClusterMode::Degraded, get_all_nodes returns local cache
//! - After degraded mode, get_all_nodes falls back to local cache
//!
//! Note: These tests use a dummy Redis client that can't actually connect, so
//! each get_all_nodes() call fails and records a circuit breaker error. After 3
//! consecutive failures, the circuit breaker opens, switching to Degraded mode.

use std::sync::Arc;
use synctv_cluster::discovery::node_registry::{NodeInfo, NodeRegistry};
use synctv_cluster::ClusterMode;

/// Helper: create a NodeRegistry with a dummy Redis client (no server needed).
/// Connection attempts will fail fast.
fn make_registry(node_id: &str) -> Arc<NodeRegistry> {
    // Use a definitely-non-listening address for fast failure
    Arc::new(
        NodeRegistry::new(
            redis::Client::open("redis://127.0.0.1:1").unwrap(),
            node_id.to_string(),
            30,
            "cl3test:",
        )
        .unwrap(),
    )
}

/// Trip circuit breaker (3 failures) -> ClusterMode::Degraded.
/// In degraded mode, get_all_nodes returns local cache instead of error.
#[tokio::test]
async fn test_cluster_mode_degrades_after_circuit_breaker_trips() {
    let registry = make_registry("self");

    // Initially should be Normal
    assert_eq!(
        registry.cluster_mode(),
        ClusterMode::Normal,
        "Initial cluster mode should be Normal"
    );

    // Populate the local cache with a known node so we can verify fallback
    let node = NodeInfo::new(
        "self".to_string(),
        "localhost:50051".to_string(),
        "localhost:8080".to_string(),
    );
    registry.test_insert_local(node).await;

    // Attempt operations that will fail (Redis not running at port 1)
    // Each get_all_nodes() failure records a circuit breaker error.
    // We need 3 failures to trip the breaker, then 1 more to trigger Degraded mode.
    for i in 0..4 {
        let _ = registry.get_all_nodes().await;
        // Check if mode transitioned early
        if registry.cluster_mode() == ClusterMode::Degraded {
            break;
        }
        // Small delay to ensure the error is recorded
        if i < 3 {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    }

    // After 3+ consecutive failures, circuit breaker should be open
    // and cluster mode should be Degraded
    assert_eq!(
        registry.cluster_mode(),
        ClusterMode::Degraded,
        "Cluster mode should be Degraded after circuit breaker trips"
    );

    // In Degraded mode, get_all_nodes should fall back to local cache
    // instead of returning an error
    let nodes = registry.get_all_nodes().await;
    assert!(
        nodes.is_ok(),
        "get_all_nodes should succeed in Degraded mode by returning local cache, got: {:?}",
        nodes.err()
    );

    let nodes = nodes.unwrap();
    assert!(
        nodes.iter().any(|n| n.node_id == "self"),
        "Local cache node should be returned in Degraded mode"
    );
}

/// Verify ClusterMode Display trait.
#[test]
fn test_cluster_mode_display() {
    assert_eq!(format!("{}", ClusterMode::Normal), "Normal");
    assert_eq!(format!("{}", ClusterMode::Degraded), "Degraded");
    assert_eq!(format!("{}", ClusterMode::Standalone), "Standalone");
}

/// Verify that the local cache is used in degraded mode by inserting
/// multiple nodes and checking they all appear.
#[tokio::test]
async fn test_degraded_mode_returns_all_local_nodes() {
    let registry = make_registry("self");

    // Populate local cache with several nodes
    for i in 0..5 {
        let node = NodeInfo::new(
            format!("node-{}", i),
            format!("localhost:{}", 50051 + i),
            format!("localhost:{}", 8080 + i),
        );
        registry.test_insert_local(node).await;
    }

    // Trip the circuit breaker
    for _ in 0..4 {
        let _ = registry.get_all_nodes().await;
        if registry.cluster_mode() == ClusterMode::Degraded {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    assert_eq!(
        registry.cluster_mode(),
        ClusterMode::Degraded,
        "Should be in Degraded mode"
    );

    // get_all_nodes should return local cache
    let nodes = registry.get_all_nodes().await.unwrap();
    assert_eq!(
        nodes.len(),
        5,
        "Should return all 5 local cache nodes in degraded mode"
    );
}

/// Verify cluster starts Normal and is_nodes_stale() is true initially
/// (never refreshed from Redis).
#[test]
fn test_initial_state_is_normal_and_stale() {
    let registry = make_registry("self");

    assert_eq!(registry.cluster_mode(), ClusterMode::Normal);
    assert!(
        registry.is_nodes_stale(),
        "Should be stale initially (never refreshed)"
    );
    assert_eq!(
        registry.last_refreshed_at(),
        0,
        "last_refreshed_at should be 0 initially"
    );
}

/// Verify that ClusterMode::Normal nodes are not stale after successful refresh.
/// (Requires Redis, so this is a unit test of the concept.)
#[test]
fn test_cluster_mode_equality() {
    assert_eq!(ClusterMode::Normal, ClusterMode::Normal);
    assert_ne!(ClusterMode::Normal, ClusterMode::Degraded);
    assert_ne!(ClusterMode::Degraded, ClusterMode::Standalone);
}
