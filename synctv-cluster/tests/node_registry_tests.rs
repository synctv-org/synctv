//! `NodeRegistry` integration tests (no Redis required)
//!
//! Tests for fencing token behavior and `NodeInfo` construction.

#![allow(clippy::unwrap_used)]
use synctv_cluster::discovery::node_registry::{FencingToken, NodeInfo};

// ============================================================================
// Test 1: fencing_token from different node IDs is NOT "newer"
// ============================================================================

#[test]
fn test_fencing_token_different_node_ids_not_newer() {
    let token_a = FencingToken::new("node-a".to_string(), 10);
    let token_b = FencingToken::new("node-b".to_string(), 20);

    // Different node_ids: is_newer_than should always return false,
    // regardless of epoch ordering
    assert!(
        !token_b.is_newer_than(&token_a),
        "Different node IDs should not be comparable"
    );
    assert!(
        !token_a.is_newer_than(&token_b),
        "Different node IDs should not be comparable (reverse)"
    );
}

// ============================================================================
// Test 2: same node_id, higher epoch IS newer
// ============================================================================

#[test]
fn test_fencing_token_same_node_higher_epoch_is_newer() {
    let old = FencingToken::new("node-1".to_string(), 5);
    let new = FencingToken::new("node-1".to_string(), 6);

    assert!(new.is_newer_than(&old));
    assert!(!old.is_newer_than(&new));
    assert!(!old.is_newer_than(&old)); // equal is not newer
}

// ============================================================================
// Test 3: NodeInfo fencing_token returns correct values
// ============================================================================

#[test]
fn test_node_info_fencing_token_values() {
    let node = NodeInfo::new(
        "my-node".to_string(),
        "10.0.0.1:50051".to_string(),
        "10.0.0.1:8080".to_string(),
    )
    .with_epoch(42);

    let token = node.fencing_token();
    assert_eq!(token.node_id, "my-node");
    assert_eq!(token.epoch, 42);
}

// ============================================================================
// Test 4: NodeInfo default epoch is 1
// ============================================================================

#[test]
fn test_node_info_default_epoch() {
    let node = NodeInfo::new(
        "n1".to_string(),
        "localhost:50051".to_string(),
        "localhost:8080".to_string(),
    );
    assert_eq!(node.epoch, 1, "New NodeInfo should have epoch 1");
}

// ============================================================================
// Test 5: FencingToken serialization round-trip
// ============================================================================

#[test]
fn test_fencing_token_serde_roundtrip() {
    let token = FencingToken::new("serde-node".to_string(), 99);
    let json = serde_json::to_string(&token).unwrap();
    let deserialized: FencingToken = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, token);
}

// ============================================================================
// Test 6: NodeInfo is_stale behavior
// ============================================================================

#[test]
fn test_node_info_is_stale() {
    let mut node = NodeInfo::new(
        "n1".to_string(),
        "localhost:50051".to_string(),
        "localhost:8080".to_string(),
    );
    // Fresh node should not be stale
    assert!(!node.is_stale(30));

    // Set heartbeat to 60 seconds ago
    node.last_heartbeat = chrono::Utc::now() - chrono::Duration::seconds(60);
    assert!(
        node.is_stale(30),
        "Node should be stale with 60s-old heartbeat and 30s timeout"
    );
    assert!(
        !node.is_stale(120),
        "Node should NOT be stale with 60s-old heartbeat and 120s timeout"
    );
}
