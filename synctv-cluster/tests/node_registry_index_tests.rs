#![allow(clippy::unwrap_used)]

use redis::AsyncCommands;
use synctv_cluster::{HeartbeatResult, NodeRegistry};
use synctv_core_testing::{start_redis_client_url_with_label, RedisContainer};

async fn setup_redis(label: &str) -> (RedisContainer, redis::Client) {
    let (redis_container, redis_client, _redis_url) =
        start_redis_client_url_with_label(label).await;
    (redis_container, redis_client)
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_get_all_nodes_prunes_missing_members_from_node_index() {
    let (_redis_container, redis_client) = setup_redis("node-registry-index-prune").await;
    let prefix = "node-index-prune:";
    let registry = NodeRegistry::new(
        synctv_core::coordination_runtime_from_client(redis_client.clone()),
        "self-node".to_string(),
        30,
        prefix,
    )
    .unwrap();

    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection");
    let index_key = format!("{prefix}cluster:nodes:index");

    let _: () = conn
        .sadd(&index_key, "missing-node")
        .await
        .expect("should seed node index");

    let nodes = registry
        .get_all_nodes()
        .await
        .expect("get_all_nodes should succeed");
    assert!(
        nodes.is_empty(),
        "missing node index members must not materialize as live nodes"
    );

    let remaining_members: Vec<String> = conn
        .smembers(&index_key)
        .await
        .expect("should read node index members");
    assert!(
        remaining_members.is_empty(),
        "stale node index members should be pruned during discovery, got {remaining_members:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_heartbeat_restores_missing_node_index_membership() {
    let (_redis_container, redis_client) = setup_redis("node-registry-index-heal").await;
    let prefix = "node-index-heal:";
    let registry = NodeRegistry::new(
        synctv_core::coordination_runtime_from_client(redis_client.clone()),
        "self-node".to_string(),
        30,
        prefix,
    )
    .unwrap();

    registry
        .register("127.0.0.1:50051".to_string())
        .await
        .expect("register should succeed");

    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection");
    let index_key = format!("{prefix}cluster:nodes:index");
    let removed: i64 = conn
        .srem(&index_key, "self-node")
        .await
        .expect("should remove node from index");
    assert_eq!(
        removed, 1,
        "registered node should exist in index before repair"
    );

    let heartbeat = registry
        .heartbeat()
        .await
        .expect("heartbeat should succeed");
    assert_eq!(
        heartbeat,
        HeartbeatResult::Ok,
        "heartbeat should repair missing node index membership"
    );

    let indexed: bool = conn
        .sismember(&index_key, "self-node")
        .await
        .expect("should inspect node index");
    assert!(indexed, "heartbeat must restore the node index membership");

    let nodes = registry
        .get_all_nodes()
        .await
        .expect("get_all_nodes should succeed after repair");
    assert!(
        nodes.iter().any(|node| node.node_id == "self-node"),
        "node should remain discoverable after index repair"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_node_index_uses_crash_safety_ttl() {
    let (_redis_container, redis_client) = setup_redis("node-registry-index-ttl").await;
    let prefix = "node-index-ttl:";
    let registry = NodeRegistry::new(
        synctv_core::coordination_runtime_from_client(redis_client.clone()),
        "self-node".to_string(),
        30,
        prefix,
    )
    .unwrap();

    registry
        .register("127.0.0.1:50051".to_string())
        .await
        .expect("register should succeed");

    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection");
    let index_key = format!("{prefix}cluster:nodes:index");
    let ttl: i64 = conn
        .ttl(&index_key)
        .await
        .expect("should read node index TTL");

    assert!(
        (55..=60).contains(&ttl),
        "node index should use the same crash-safety TTL window as node entries, got {ttl}s"
    );
}
