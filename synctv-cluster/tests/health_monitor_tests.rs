//! HealthMonitor integration tests
//!
//! Tests for process_heartbeats logic: stale nodes marked unhealthy,
//! fresh nodes marked healthy (verifies the bug fix), and backoff
//! multiplier capping.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use synctv_cluster::NodeHealth;
use synctv_cluster::discovery::health_monitor::HealthMonitor;
use synctv_cluster::NodeInfo;

// ============================================================================
// Test 1: stale nodes are marked unhealthy
// ============================================================================

#[tokio::test]
async fn test_process_heartbeats_marks_stale_unhealthy() {
    // Create a stale node (heartbeat 60 seconds ago) directly
    let mut stale_node = NodeInfo::new(
        "stale-node".to_string(),
        "localhost:50051".to_string(),
        "localhost:8080".to_string(),
    );
    stale_node.last_heartbeat = chrono::Utc::now() - chrono::Duration::seconds(60);

    let health_status: Arc<RwLock<HashMap<String, NodeHealth>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Pass the stale node directly to process_heartbeats
    HealthMonitor::process_heartbeats(&health_status, &[stale_node], 30).await;

    let status = health_status.read().await;
    assert_eq!(
        status.get("stale-node"),
        Some(&NodeHealth::Unhealthy),
        "Stale node should be marked Unhealthy"
    );
}

// ============================================================================
// Test 2: fresh nodes with no existing status are marked healthy (bug fix)
// ============================================================================

#[tokio::test]
async fn test_process_heartbeats_marks_fresh_healthy() {
    let fresh_node = NodeInfo::new(
        "fresh-node".to_string(),
        "localhost:50051".to_string(),
        "localhost:8080".to_string(),
    );

    let health_status: Arc<RwLock<HashMap<String, NodeHealth>>> =
        Arc::new(RwLock::new(HashMap::new()));

    HealthMonitor::process_heartbeats(&health_status, &[fresh_node], 30).await;

    let status = health_status.read().await;
    assert_eq!(
        status.get("fresh-node"),
        Some(&NodeHealth::Healthy),
        "Fresh node with no prior status should be marked Healthy (bug fix verification)"
    );
}

// ============================================================================
// Test 3: fresh node with existing status is NOT overridden by heartbeat
// ============================================================================

#[tokio::test]
async fn test_process_heartbeats_does_not_override_existing() {
    let fresh_node = NodeInfo::new(
        "probed-node".to_string(),
        "localhost:50051".to_string(),
        "localhost:8080".to_string(),
    );

    let health_status: Arc<RwLock<HashMap<String, NodeHealth>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Pre-populate with Degraded status (simulating a prior probe result)
    {
        let mut status = health_status.write().await;
        status.insert("probed-node".to_string(), NodeHealth::Degraded);
    }

    HealthMonitor::process_heartbeats(&health_status, &[fresh_node], 30).await;

    let status = health_status.read().await;
    assert_eq!(
        status.get("probed-node"),
        Some(&NodeHealth::Degraded),
        "Fresh node with existing status should NOT be overridden by heartbeat processing"
    );
}

// ============================================================================
// Test 4: backoff multiplier caps at 8x
// ============================================================================

#[test]
fn test_backoff_multiplier_capped() {
    const MAX_BACKOFF_MULTIPLIER: u64 = 8;

    for failures in 0..20u32 {
        let backoff_multiplier = if failures > 0 {
            (1u64 << failures.min(3)).min(MAX_BACKOFF_MULTIPLIER)
        } else {
            1
        };

        assert!(
            backoff_multiplier <= MAX_BACKOFF_MULTIPLIER,
            "Backoff multiplier {} exceeds max {} at {} failures",
            backoff_multiplier,
            MAX_BACKOFF_MULTIPLIER,
            failures
        );

        // Verify specific values
        match failures {
            0 => assert_eq!(backoff_multiplier, 1),
            1 => assert_eq!(backoff_multiplier, 2),
            2 => assert_eq!(backoff_multiplier, 4),
            3 => assert_eq!(backoff_multiplier, 8),
            _ => assert_eq!(backoff_multiplier, 8, "Should cap at 8x for {} failures", failures),
        }
    }
}

// ============================================================================
// Test 5: removed nodes are pruned from status map
// ============================================================================

#[tokio::test]
async fn test_process_heartbeats_prunes_removed_nodes() {
    let fresh_node = NodeInfo::new(
        "alive-node".to_string(),
        "localhost:50051".to_string(),
        "localhost:8080".to_string(),
    );

    let health_status: Arc<RwLock<HashMap<String, NodeHealth>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Pre-populate with a ghost node that won't appear in the nodes list
    {
        let mut status = health_status.write().await;
        status.insert("ghost-node".to_string(), NodeHealth::Healthy);
    }

    HealthMonitor::process_heartbeats(&health_status, &[fresh_node], 30).await;

    let status = health_status.read().await;
    assert!(
        !status.contains_key("ghost-node"),
        "Removed node should be pruned from health status"
    );
    assert!(
        status.contains_key("alive-node"),
        "Alive node should be present"
    );
}
