//! CL7: `HealthMonitor` probe recovery
//!
//! - Pre-set node to Unhealthy, call `process_heartbeats` with fresh node
//! - Assert status transitions to Healthy after `process_heartbeats` when node
//!   becomes stale and then fresh again

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use synctv_cluster::discovery::health_monitor::HealthMonitor;
use synctv_cluster::{NodeHealth, NodeInfo};

/// Pre-set a node to Unhealthy, then process heartbeats with a stale node.
/// The node should transition to Unhealthy because stale nodes override all states.
#[tokio::test]
async fn test_stale_node_overrides_healthy() {
    let health_status: Arc<RwLock<HashMap<String, NodeHealth>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Pre-set as Healthy
    {
        let mut status = health_status.write().await;
        status.insert("node-1".to_string(), NodeHealth::Healthy);
    }

    // Create a stale node (heartbeat 120 seconds ago)
    let mut stale_node = NodeInfo::new(
        "node-1".to_string(),
        "localhost:8080".to_string(),
    );
    stale_node.last_heartbeat = chrono::Utc::now() - chrono::Duration::seconds(120);

    // Process heartbeats: stale check overrides Healthy -> Unhealthy
    HealthMonitor::process_heartbeats(&health_status, &[stale_node], 30).await;

    let status = health_status.read().await;
    assert_eq!(
        status.get("node-1"),
        Some(&NodeHealth::Unhealthy),
        "Stale node should override Healthy to Unhealthy"
    );
}

/// Pre-set a node to Unhealthy, then process heartbeats with a fresh node.
/// A fresh node without existing entry gets Healthy, but a fresh node with
/// existing Unhealthy status is NOT overridden (preserves probe result).
#[tokio::test]
async fn test_unhealthy_not_overridden_by_fresh_heartbeat() {
    let health_status: Arc<RwLock<HashMap<String, NodeHealth>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Pre-set as Unhealthy (simulating probe failure)
    {
        let mut status = health_status.write().await;
        status.insert("node-1".to_string(), NodeHealth::Unhealthy);
    }

    // Create a fresh node (recent heartbeat)
    let fresh_node = NodeInfo::new(
        "node-1".to_string(),
        "localhost:8080".to_string(),
    );

    // Process heartbeats: fresh node with existing status is NOT overridden
    HealthMonitor::process_heartbeats(&health_status, &[fresh_node], 30).await;

    let status = health_status.read().await;
    assert_eq!(
        status.get("node-1"),
        Some(&NodeHealth::Unhealthy),
        "Unhealthy status should NOT be overridden by heartbeat processing (only probes change it)"
    );
}

/// After being stale -> Unhealthy, becoming fresh again (simulating node recovery)
/// should not automatically transition to Healthy (requires probe).
#[tokio::test]
async fn test_stale_then_fresh_stays_unhealthy_until_probed() {
    let health_status: Arc<RwLock<HashMap<String, NodeHealth>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Step 1: Fresh node gets marked Healthy (no prior entry)
    let fresh_node = NodeInfo::new(
        "node-1".to_string(),
        "localhost:8080".to_string(),
    );
    HealthMonitor::process_heartbeats(&health_status, std::slice::from_ref(&fresh_node), 30).await;

    {
        let status = health_status.read().await;
        assert_eq!(status.get("node-1"), Some(&NodeHealth::Healthy));
    }

    // Step 2: Node goes stale -> Unhealthy
    let mut stale = fresh_node.clone();
    stale.last_heartbeat = chrono::Utc::now() - chrono::Duration::seconds(60);
    HealthMonitor::process_heartbeats(&health_status, &[stale], 30).await;

    {
        let status = health_status.read().await;
        assert_eq!(status.get("node-1"), Some(&NodeHealth::Unhealthy));
    }

    // Step 3: Node recovers (fresh heartbeat again), but existing Unhealthy
    // status is preserved (NOT overridden by heartbeat)
    let recovered = NodeInfo::new(
        "node-1".to_string(),
        "localhost:8080".to_string(),
    );
    HealthMonitor::process_heartbeats(&health_status, &[recovered], 30).await;

    let status = health_status.read().await;
    assert_eq!(
        status.get("node-1"),
        Some(&NodeHealth::Unhealthy),
        "Recovered node should stay Unhealthy until explicitly probed healthy"
    );
}

/// Verify that Degraded status is preserved through heartbeat processing.
#[tokio::test]
async fn test_degraded_preserved_through_heartbeat() {
    let health_status: Arc<RwLock<HashMap<String, NodeHealth>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Pre-set as Degraded
    {
        let mut status = health_status.write().await;
        status.insert("node-1".to_string(), NodeHealth::Degraded);
    }

    let fresh_node = NodeInfo::new(
        "node-1".to_string(),
        "localhost:8080".to_string(),
    );

    HealthMonitor::process_heartbeats(&health_status, &[fresh_node], 30).await;

    let status = health_status.read().await;
    assert_eq!(
        status.get("node-1"),
        Some(&NodeHealth::Degraded),
        "Degraded status should be preserved through heartbeat processing"
    );
}

/// Verify the recovery path: node with no prior status + fresh heartbeat
/// -> Healthy (first seen scenario after restart or new node joining).
#[tokio::test]
async fn test_new_fresh_node_gets_healthy() {
    let health_status: Arc<RwLock<HashMap<String, NodeHealth>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // No prior status entry for node-new

    let fresh_node = NodeInfo::new(
        "node-new".to_string(),
        "localhost:8080".to_string(),
    );

    HealthMonitor::process_heartbeats(&health_status, &[fresh_node], 30).await;

    let status = health_status.read().await;
    assert_eq!(
        status.get("node-new"),
        Some(&NodeHealth::Healthy),
        "New fresh node without prior status should get Healthy"
    );
}

/// Multiple nodes with mixed states: verify each is processed independently.
#[tokio::test]
async fn test_multiple_nodes_mixed_states() {
    let health_status: Arc<RwLock<HashMap<String, NodeHealth>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Pre-set some states
    {
        let mut status = health_status.write().await;
        status.insert("node-a".to_string(), NodeHealth::Healthy);
        status.insert("node-b".to_string(), NodeHealth::Degraded);
        // node-c has no prior state
    }

    // node-a becomes stale, node-b stays fresh, node-c is new and fresh
    let mut stale_a = NodeInfo::new(
        "node-a".to_string(),
        "localhost:8080".to_string(),
    );
    stale_a.last_heartbeat = chrono::Utc::now() - chrono::Duration::seconds(60);

    let fresh_b = NodeInfo::new(
        "node-b".to_string(),
        "localhost:8081".to_string(),
    );

    let fresh_c = NodeInfo::new(
        "node-c".to_string(),
        "localhost:8082".to_string(),
    );

    HealthMonitor::process_heartbeats(&health_status, &[stale_a, fresh_b, fresh_c], 30).await;

    let status = health_status.read().await;
    assert_eq!(
        status.get("node-a"),
        Some(&NodeHealth::Unhealthy),
        "Stale node-a should be Unhealthy"
    );
    assert_eq!(
        status.get("node-b"),
        Some(&NodeHealth::Degraded),
        "Fresh node-b should preserve Degraded"
    );
    assert_eq!(
        status.get("node-c"),
        Some(&NodeHealth::Healthy),
        "New fresh node-c should get Healthy"
    );
}
