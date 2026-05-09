//! CL2: `NodeRegistry` heartbeat TTL (testcontainer Redis)
//!
//! - Register node, DEL Redis key manually, call `heartbeat()`, assert re-registered via `get_all_nodes()`
//! - Force epoch mismatch by writing modified epoch to Redis, verify auto-retry

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use synctv_core_testing::{start_redis_client_url_with_label, RedisContainer};

use synctv_cluster::discovery::node_registry::NodeRegistry;
use synctv_cluster::HeartbeatResult;

/// Helper to create a Redis container and client.
async fn setup_redis() -> (RedisContainer, redis::Client, String) {
    start_redis_client_url_with_label("node-registry-heartbeat").await
}

/// Register node, DEL Redis key manually, call `heartbeat()`, assert re-registered.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_heartbeat_reregisters_after_key_deletion() {
    let (_redis_container, redis_client, _url) = setup_redis().await;

    let registry = Arc::new(
        NodeRegistry::new(
            synctv_core::coordination_runtime_from_client(redis_client.clone()),
            "test-node".to_string(),
            30,
            "cl2test:",
        )
        .unwrap(),
    );

    // Register the node
    registry
        .register("localhost:8080".to_string())
        .await
        .expect("register should succeed");

    // Verify node is visible
    let nodes = registry.get_all_nodes().await.expect("get_all_nodes");
    assert!(
        nodes.iter().any(|n| n.node_id == "test-node"),
        "Node should be visible after registration"
    );

    // Manually DEL the Redis key to simulate TTL expiry
    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let key = "cl2test:cluster:nodes:test-node";
    let deleted: i64 = redis::cmd("DEL")
        .arg(key)
        .query_async(&mut conn)
        .await
        .expect("DEL should succeed");
    assert_eq!(deleted, 1, "Should have deleted exactly 1 key");

    // Verify key is gone from Redis
    let exists: bool = redis::cmd("EXISTS")
        .arg(key)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert!(!exists, "Key should not exist after DEL");

    // Call heartbeat() - should auto-re-register
    let result = registry
        .heartbeat()
        .await
        .expect("heartbeat should succeed");

    // heartbeat() auto-re-registers and returns Ok (not NeedReregistration)
    assert_eq!(
        result,
        HeartbeatResult::Ok,
        "heartbeat should auto-re-register and return Ok"
    );

    // Verify node is visible again via get_all_nodes (Redis)
    // Invalidate the moka cache by waiting for its TTL (2s) to expire
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let nodes = registry.get_all_nodes().await.expect("get_all_nodes");
    assert!(
        nodes.iter().any(|n| n.node_id == "test-node"),
        "Node should be visible again after heartbeat auto-re-registration"
    );
}

/// Force epoch mismatch by writing modified epoch to Redis, verify auto-retry.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_heartbeat_auto_retries_on_epoch_mismatch() {
    let (_redis_container, redis_client, _url) = setup_redis().await;

    let registry = Arc::new(
        NodeRegistry::new(
            synctv_core::coordination_runtime_from_client(redis_client.clone()),
            "epoch-node".to_string(),
            30,
            "cl2epoch:",
        )
        .unwrap(),
    );

    // Register the node (gets epoch from atomic Lua script)
    registry
        .register("localhost:8080".to_string())
        .await
        .expect("register should succeed");

    // Read current state from Redis
    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let key = "cl2epoch:cluster:nodes:epoch-node";
    let json_str: String = redis::cmd("GET")
        .arg(key)
        .query_async(&mut conn)
        .await
        .expect("GET should succeed");

    // Parse, modify epoch to a different value, write back
    let mut node_info: serde_json::Value = serde_json::from_str(&json_str).expect("should parse");
    let original_epoch = node_info["epoch"].as_u64().unwrap();
    let modified_epoch = original_epoch + 100;
    node_info["epoch"] = serde_json::Value::Number(serde_json::Number::from(modified_epoch));

    let modified_json = serde_json::to_string(&node_info).unwrap();
    let _: () = redis::cmd("SET")
        .arg(key)
        .arg(&modified_json)
        .query_async(&mut conn)
        .await
        .expect("SET should succeed");

    // Preserve TTL
    let ttl = 60i64;
    let _: () = redis::cmd("EXPIRE")
        .arg(key)
        .arg(ttl)
        .query_async(&mut conn)
        .await
        .expect("EXPIRE should succeed");

    // Call heartbeat() - should detect epoch mismatch and auto-re-register
    let result = registry
        .heartbeat()
        .await
        .expect("heartbeat should succeed");

    // heartbeat() auto-re-registers on epoch mismatch and returns Ok
    assert_eq!(
        result,
        HeartbeatResult::Ok,
        "heartbeat should auto-re-register on epoch mismatch and return Ok"
    );

    // Verify the new epoch is higher than the modified one
    let new_json_str: String = redis::cmd("GET")
        .arg(key)
        .query_async(&mut conn)
        .await
        .expect("GET should succeed after re-registration");
    let new_node_info: serde_json::Value =
        serde_json::from_str(&new_json_str).expect("should parse");
    let new_epoch = new_node_info["epoch"].as_u64().unwrap();

    assert!(
        new_epoch > modified_epoch,
        "New epoch ({new_epoch}) should be greater than modified epoch ({modified_epoch})"
    );
}
