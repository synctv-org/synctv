#![allow(clippy::unwrap_used)]

use std::time::Duration;

use redis::AsyncCommands;
use synctv_cluster::{HeartbeatResult, NodeRegistry};
use synctv_core_testing::{start_redis_url_with_label, RedisContainer};

fn docker_startup_timeout() -> Duration {
    std::env::var("SYNCTV_TEST_DOCKER_STARTUP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| secs.max(30))
        .map_or_else(|| Duration::from_mins(2), Duration::from_secs)
}

async fn setup_redis(label: &str) -> (RedisContainer, redis::Client) {
    let (redis_container, redis_url) =
        tokio::time::timeout(docker_startup_timeout(), start_redis_url_with_label(label))
            .await
            .expect("Docker container startup timed out (is Docker running?)");

    let redis_client =
        redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

    let mut conn = {
        let mut retries = 0;
        loop {
            match redis_client.get_multiplexed_async_connection().await {
                Ok(conn) => break conn,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
                Err(error) => panic!("Redis connection failed after {retries} retries: {error}"),
            }
        }
    };

    let _: () = redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .expect("Redis PING failed");

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
        .register("127.0.0.1:8080".to_string())
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
        .register("127.0.0.1:8080".to_string())
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
