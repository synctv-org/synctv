//! Multi-replica cluster integration tests
//!
//! These tests verify cross-node coordination by starting multiple
//! `RealtimeManager` instances that share a single Redis container
//! (via testcontainers). Each "node" has its own `node_id` but connects
//! to the same Redis, simulating a multi-replica deployment.

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use chrono::Utc;
use synctv_core::models::id::{RoomId, UserId};
use synctv_core_testing::redis_connection_manager;
use synctv_realtime::sync::events::RealtimeEvent;
mod integration_test_helpers;
use integration_test_helpers::{
    broadcast_until_all_clients_receive, create_node, wait_until, wait_until_async, TestRedis,
};

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_three_node_cluster() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;
    let node_c = create_node(&redis.redis_url, "node_c").await;

    let room_id = RoomId::expect_positive(10_000_054);

    // Subscribe on node A and node C
    let (rx_a, conn_a) = node_a
        .subscribe(room_id, UserId::expect_positive(10_000_003))
        .await
        .expect("subscribe should succeed");
    let (rx_c, conn_c) = node_c
        .subscribe(room_id, UserId::expect_positive(10_000_055))
        .await
        .expect("subscribe should succeed");

    let message_from_b = "Hello from B";
    let mut clients_a = vec![(rx_a, conn_a.clone())];
    let mut clients_c = vec![(rx_c, conn_c.clone())];

    broadcast_until_all_clients_receive(
        &node_b,
        &mut clients_a,
        message_from_b,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_004),
            username: "user_b".to_string(),
            message: message_from_b.to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        },
        "node A receiving node B broadcast",
    )
    .await;

    broadcast_until_all_clients_receive(
        &node_b,
        &mut clients_c,
        message_from_b,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_004),
            username: "user_b".to_string(),
            message: message_from_b.to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        },
        "node C receiving node B broadcast",
    )
    .await;

    node_a.unsubscribe(&conn_a);
    node_c.unsubscribe(&conn_c);
    node_a.shutdown().await;
    node_b.shutdown().await;
    node_c.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_node_discovery_three_nodes() {
    use synctv_cluster::NodeRegistry;

    let redis = TestRedis::start().await;

    let redis_client_a =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to create Redis client A");
    let redis_client_b =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to create Redis client B");
    let redis_client_c =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to create Redis client C");

    let registry_a = NodeRegistry::new(
        synctv_core::coordination_runtime_from_client(redis_client_a),
        "node_a".to_string(),
        30,
        &redis.key_prefix,
    )
    .expect("Failed to create registry A");

    let registry_b = NodeRegistry::new(
        synctv_core::coordination_runtime_from_client(redis_client_b),
        "node_b".to_string(),
        30,
        &redis.key_prefix,
    )
    .expect("Failed to create registry B");

    let registry_c = NodeRegistry::new(
        synctv_core::coordination_runtime_from_client(redis_client_c),
        "node_c".to_string(),
        30,
        &redis.key_prefix,
    )
    .expect("Failed to create registry C");

    // Register all three nodes
    registry_a
        .register("node_a:8080".to_string())
        .await
        .expect("Failed to register node A");

    registry_b
        .register("node_b:8080".to_string())
        .await
        .expect("Failed to register node B");

    registry_c
        .register("node_c:8080".to_string())
        .await
        .expect("Failed to register node C");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Each registry should see all 3 nodes
    let nodes = registry_a
        .get_all_nodes()
        .await
        .expect("Failed to get all nodes from A");

    let node_ids: Vec<String> = nodes.iter().map(|n| n.node_id.clone()).collect();
    assert!(
        node_ids.contains(&"node_a".to_string()),
        "Should contain node_a: {node_ids:?}"
    );
    assert!(
        node_ids.contains(&"node_b".to_string()),
        "Should contain node_b: {node_ids:?}"
    );
    assert!(
        node_ids.contains(&"node_c".to_string()),
        "Should contain node_c: {node_ids:?}"
    );
    assert_eq!(nodes.len(), 3, "Should have exactly 3 nodes");

    // Verify individual node lookup
    let node_b_info = registry_a
        .get_node("node_b")
        .await
        .expect("Failed to get node B")
        .expect("Node B not found");

    assert_eq!(node_b_info.node_id, "node_b");
    assert_eq!(node_b_info.api_address, "node_b:8080");
    assert!(node_b_info.epoch >= 1, "Epoch should be at least 1");

    // Heartbeat should work
    let heartbeat_result = registry_a.heartbeat().await.expect("Heartbeat failed");
    assert_eq!(
        heartbeat_result,
        synctv_cluster::HeartbeatResult::Ok,
        "Heartbeat should succeed"
    );

    // Unregister node C
    registry_c
        .unregister()
        .await
        .expect("Failed to unregister C");

    // After unregister + cache expiry, only 2 nodes should remain.
    wait_until_async(
        "node C removal visibility",
        Duration::from_secs(8),
        || async {
            registry_a
                .get_all_nodes()
                .await
                .is_ok_and(|nodes| nodes.iter().all(|node| node.node_id != "node_c"))
        },
    )
    .await;

    let nodes_after = registry_a
        .get_all_nodes()
        .await
        .expect("Failed to get nodes after unregister");

    let remaining_ids: Vec<String> = nodes_after.iter().map(|n| n.node_id.clone()).collect();
    assert!(
        !remaining_ids.contains(&"node_c".to_string()),
        "Node C should be unregistered: {remaining_ids:?}"
    );
    assert_eq!(
        nodes_after.len(),
        2,
        "Should have 2 remaining nodes: {remaining_ids:?}"
    );
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_node_epoch_fencing() {
    use synctv_cluster::NodeRegistry;

    let redis = TestRedis::start().await;

    let redis_client =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to create Redis client");

    let registry = NodeRegistry::new(
        synctv_core::coordination_runtime_from_client(redis_client),
        "fencing_node".to_string(),
        30,
        &redis.key_prefix,
    )
    .expect("Failed to create registry");

    // First registration
    registry
        .register("host:8080".to_string())
        .await
        .expect("First register failed");

    let token1 = registry.current_fencing_token();
    assert!(token1.epoch >= 1, "First epoch should be >= 1");

    // Re-registration should increment epoch
    registry
        .register("host:8080".to_string())
        .await
        .expect("Second register failed");

    let token2 = registry.current_fencing_token();
    assert!(
        token2.epoch > token1.epoch,
        "Re-registration should increment epoch: {} -> {}",
        token1.epoch,
        token2.epoch
    );

    // The newer token should report as newer
    assert!(
        token2.is_newer_than(&token1),
        "Second token should be newer than first"
    );
    assert!(
        !token1.is_newer_than(&token2),
        "First token should not be newer than second"
    );
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_testredis_wait_until_ready_supports_multiplexed_connections() {
    let redis = TestRedis::start().await;

    TestRedis::wait_until_ready(&redis.redis_url).await;

    let client =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to create Redis client");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("Multiplexed connection should be ready after helper returns");

    let pong: String = redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .expect("PING should succeed on multiplexed connection");
    assert_eq!(pong, "PONG");
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_leader_election_single_leader() {
    use synctv_cluster::leader::{LeaderElector, LeaderElectorConfig};
    use tokio_util::sync::CancellationToken;

    let redis = TestRedis::start().await;

    let client =
        redis::Client::open(redis.redis_url.as_str()).expect("Failed to create Redis client");

    let conn_a = redis_connection_manager(&client).await;
    let conn_b = redis_connection_manager(&client).await;
    let conn_c = redis_connection_manager(&client).await;

    let config_a = LeaderElectorConfig {
        lease_duration_secs: 5,
        renew_interval_secs: 1,
    };
    let config_b = LeaderElectorConfig {
        lease_duration_secs: 5,
        renew_interval_secs: 1,
    };
    let config_c = LeaderElectorConfig {
        lease_duration_secs: 5,
        renew_interval_secs: 1,
    };

    let elector_a = LeaderElector::with_config(
        conn_a,
        "node_a".to_string(),
        &config_a,
        &redis.key_prefix,
        false,
    );
    let elector_b = LeaderElector::with_config(
        conn_b,
        "node_b".to_string(),
        &config_b,
        &redis.key_prefix,
        false,
    );
    let elector_c = LeaderElector::with_config(
        conn_c,
        "node_c".to_string(),
        &config_c,
        &redis.key_prefix,
        false,
    );

    let cancel_a = CancellationToken::new();
    let cancel_b = CancellationToken::new();
    let cancel_c = CancellationToken::new();

    let _handle_a = elector_a.start(cancel_a.clone());
    let _handle_b = elector_b.start(cancel_b.clone());
    let _handle_c = elector_c.start(cancel_c.clone());

    wait_until("initial leader election", Duration::from_secs(5), || {
        let leader_count = [
            elector_a.is_leader(),
            elector_b.is_leader(),
            elector_c.is_leader(),
        ]
        .iter()
        .filter(|&&v| v)
        .count();
        leader_count == 1
    })
    .await;

    // Count leaders
    let leader_count = [
        elector_a.is_leader(),
        elector_b.is_leader(),
        elector_c.is_leader(),
    ]
    .iter()
    .filter(|&&v| v)
    .count();

    assert_eq!(
        leader_count,
        1,
        "Exactly one node should be leader, got {}: A={}, B={}, C={}",
        leader_count,
        elector_a.is_leader(),
        elector_b.is_leader(),
        elector_c.is_leader()
    );

    // Identify the leader
    let leader_id = if elector_a.is_leader() {
        "A"
    } else if elector_b.is_leader() {
        "B"
    } else {
        "C"
    };

    // Cancel the leader to simulate crash
    match leader_id {
        "A" => cancel_a.cancel(),
        "B" => cancel_b.cancel(),
        "C" => cancel_c.cancel(),
        _ => unreachable!(),
    }

    wait_until("leader failover", Duration::from_secs(10), || {
        [
            (!cancel_a.is_cancelled(), elector_a.is_leader()),
            (!cancel_b.is_cancelled(), elector_b.is_leader()),
            (!cancel_c.is_cancelled(), elector_c.is_leader()),
        ]
        .iter()
        .filter(|(active, is_leader)| *active && *is_leader)
        .count()
            == 1
    })
    .await;

    // A new leader should have been elected among the remaining two
    let remaining_leaders: Vec<&str> = [
        (!cancel_a.is_cancelled(), elector_a.is_leader(), "A"),
        (!cancel_b.is_cancelled(), elector_b.is_leader(), "B"),
        (!cancel_c.is_cancelled(), elector_c.is_leader(), "C"),
    ]
    .iter()
    .filter(|(active, is_leader, _)| *active && *is_leader)
    .map(|(_, _, name)| *name)
    .collect();

    assert_eq!(
        remaining_leaders.len(),
        1,
        "Exactly one remaining node should be leader after failover, got: {remaining_leaders:?}"
    );

    // Cleanup
    cancel_a.cancel();
    cancel_b.cancel();
    cancel_c.cancel();

    // Give tasks time to shut down
    tokio::time::sleep(Duration::from_millis(200)).await;
}
