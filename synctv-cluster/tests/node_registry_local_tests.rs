//! Tests for local-only `NodeRegistry` mode (no Redis required)
//!
//! These tests verify that `NodeRegistry` can function in a degraded/local mode
//! when Redis is unavailable, supporting the "non-cluster mode can work without Redis" architecture.

#![allow(clippy::unwrap_used)]
use synctv_cluster::{ClusterMode, NodeInfo, NodeRegistry};

/// Test that `NodeRegistry` can be created in local-only mode without Redis.
#[tokio::test]
async fn test_new_local_only_creates_registry() {
    // This should succeed without requiring a Redis connection
    let registry = NodeRegistry::new_local_only(
        "test-node-local".to_string(),
        30,
        "localtest:",
    );

    assert!(
        registry.is_ok(),
        "new_local_only should succeed without Redis"
    );
}

/// Test that local-only registry starts in Standalone mode.
#[tokio::test]
async fn test_local_only_starts_in_standalone_mode() {
    let registry = NodeRegistry::new_local_only(
        "test-node-standalone".to_string(),
        30,
        "standalone:",
    ).expect("new_local_only should succeed");

    // Should be in Standalone mode since there's no Redis
    assert_eq!(
        registry.cluster_mode(),
        ClusterMode::Standalone,
        "Local-only registry should start in Standalone mode"
    );
}

/// Test that local-only registry can discover nodes from local cache.
#[tokio::test]
async fn test_local_only_discover_nodes_from_local_cache() {
    let registry = NodeRegistry::new_local_only(
        "test-node-discover".to_string(),
        30,
        "discover:",
    ).expect("new_local_only should succeed");

    // Insert a node into local cache
    let node_info = NodeInfo::new(
        "peer-node".to_string(),
        "localhost:50052".to_string(),
        "localhost:8081".to_string(),
    );
    registry.test_insert_local(node_info).await;

    // get_all_nodes_local should return the cached node
    let nodes = registry.get_all_nodes_local().await;
    assert_eq!(nodes.len(), 1, "Should find one node in local cache");
    assert_eq!(nodes[0].node_id, "peer-node");

    // get_all_nodes should also work (returns local cache in Standalone mode)
    let all_nodes = registry.get_all_nodes().await.expect("get_all_nodes should succeed");
    assert_eq!(all_nodes.len(), 1, "Should find one node via get_all_nodes");
    assert_eq!(all_nodes[0].node_id, "peer-node");
}

/// Test that local-only registry can get a specific node from local cache.
#[tokio::test]
async fn test_local_only_get_node_from_local_cache() {
    let registry = NodeRegistry::new_local_only(
        "test-node-get".to_string(),
        30,
        "gettest:",
    ).expect("new_local_only should succeed");

    // Insert a node into local cache
    let node_info = NodeInfo::new(
        "specific-node".to_string(),
        "localhost:50053".to_string(),
        "localhost:8082".to_string(),
    );
    registry.test_insert_local(node_info.clone()).await;

    // get_node_local should find it
    let found = registry.get_node_local("specific-node").await;
    assert!(found.is_some(), "Should find the node");
    let found = found.unwrap();
    assert_eq!(found.grpc_address, "localhost:50053");
    assert_eq!(found.http_address, "localhost:8082");
}

/// Test that local-only registry handles empty cache gracefully.
#[tokio::test]
async fn test_local_only_empty_cache_returns_empty() {
    let registry = NodeRegistry::new_local_only(
        "test-node-empty".to_string(),
        30,
        "emptytest:",
    ).expect("new_local_only should succeed");

    // Empty cache should return empty vector
    let nodes = registry.get_all_nodes_local().await;
    assert!(nodes.is_empty(), "Empty cache should return empty vector");

    // get_all_nodes should also return empty (no Redis fallback needed)
    let all_nodes = registry.get_all_nodes().await.expect("get_all_nodes should succeed");
    assert!(all_nodes.is_empty(), "Should return empty for empty cache");
}

/// Test that `is_nodes_stale` returns true for local-only registry that never refreshed.
#[tokio::test]
async fn test_local_only_is_nodes_stale_when_never_refreshed() {
    let registry = NodeRegistry::new_local_only(
        "test-node-stale".to_string(),
        30,
        "staletest:",
    ).expect("new_local_only should succeed");

    // Since there's no Redis, is_nodes_stale should return true (never refreshed from Redis)
    assert!(
        registry.is_nodes_stale(),
        "Local-only registry should report stale (never refreshed from Redis)"
    );
}

/// Test that `merge_dns_peers` works in local-only mode.
#[tokio::test]
async fn test_local_only_merge_dns_peers() {
    let registry = NodeRegistry::new_local_only(
        "test-node-dns".to_string(),
        30,
        "dnstest:",
    ).expect("new_local_only should succeed");

    // Merge some DNS-discovered peers
    let peers = vec![
        NodeInfo::new("dns-peer-1".to_string(), "10.0.0.1:50051".to_string(), "10.0.0.1:8080".to_string()),
        NodeInfo::new("dns-peer-2".to_string(), "10.0.0.2:50051".to_string(), "10.0.0.2:8080".to_string()),
    ];
    registry.merge_dns_peers(peers).await;

    // Should find both peers in local cache
    let nodes = registry.get_all_nodes_local().await;
    assert_eq!(nodes.len(), 2, "Should have 2 peers from DNS merge");

    let node_ids: std::collections::HashSet<_> = nodes.iter().map(|n| n.node_id.as_str()).collect();
    assert!(node_ids.contains("dns-peer-1"));
    assert!(node_ids.contains("dns-peer-2"));
}

/// Test that local-only registry provides valid fencing token.
#[tokio::test]
async fn test_local_only_fencing_token() {
    let registry = NodeRegistry::new_local_only(
        "test-node-fencing".to_string(),
        30,
        "fencingtest:",
    ).expect("new_local_only should succeed");

    // Should be able to get a fencing token
    let token = registry.current_fencing_token();
    assert_eq!(token.node_id, "test-node-fencing");
    assert!(token.epoch >= 1, "Epoch should start at 1 or higher");
}
