#![allow(clippy::unwrap_used)]

use std::time::Duration;

use synctv_cluster::NodeRegistry;
use synctv_core_testing::{start_redis_url_with_label, RedisContainer};

fn docker_startup_timeout() -> Duration {
    std::env::var("SYNCTV_TEST_DOCKER_STARTUP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| secs.max(30))
        .map_or_else(|| Duration::from_mins(2), Duration::from_secs)
}

async fn setup_redis() -> (RedisContainer, redis::Client) {
    let (redis_container, redis_url) = tokio::time::timeout(
        docker_startup_timeout(),
        start_redis_url_with_label("node-registry-unregister-remote"),
    )
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
                Err(e) => panic!("Redis connection failed after {retries} retries: {e}"),
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
async fn test_unregister_remote_rejects_missing_epoch() {
    let (_redis_container, redis_client) = setup_redis().await;
    let registry = NodeRegistry::new(
        redis_client,
        "self-node".to_string(),
        30,
        "unregister-missing-epoch:",
    )
    .unwrap();

    let err = registry
        .unregister_remote("peer-node", None)
        .await
        .expect_err("missing epoch should be rejected");

    assert!(
        err.to_string().contains("expected_epoch is required"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_unregister_remote_stale_epoch_does_not_delete_reregistered_node() {
    let (_redis_container, redis_client) = setup_redis().await;
    let registry = NodeRegistry::new(
        redis_client.clone(),
        "self-node".to_string(),
        30,
        "unregister-stale-epoch:",
    )
    .unwrap();

    let key = "unregister-stale-epoch:cluster:nodes:peer-node";

    registry
        .register_remote(
            synctv_cluster::NodeInfo::new(
                "peer-node".to_string(),
                "10.0.0.2:50051".to_string(),
                "10.0.0.2:8080".to_string(),
            )
            .with_epoch(5),
        )
        .await
        .expect("initial remote registration should succeed");

    registry
        .register_remote(
            synctv_cluster::NodeInfo::new(
                "peer-node".to_string(),
                "10.0.0.3:50051".to_string(),
                "10.0.0.3:8080".to_string(),
            )
            .with_epoch(6),
        )
        .await
        .expect("re-registration with higher epoch should succeed");

    registry
        .unregister_remote("peer-node", Some(5))
        .await
        .expect("stale unregister should not error");

    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection");

    let stored: String = redis::cmd("GET")
        .arg(key)
        .query_async(&mut conn)
        .await
        .expect("peer registration should still exist");

    let node: serde_json::Value =
        serde_json::from_str(&stored).expect("stored node info should deserialize");
    assert_eq!(
        node["epoch"].as_u64(),
        Some(6),
        "stale unregister must not delete newer registration"
    );
    assert_eq!(
        node["grpc_address"].as_str(),
        Some("10.0.0.3:50051"),
        "newer registration must remain intact"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_remote_writes_invalidate_cached_node_view_immediately() {
    let (_redis_container, redis_client) = setup_redis().await;
    let registry = NodeRegistry::new(
        redis_client,
        "self-node".to_string(),
        30,
        "remote-cache-invalidate:",
    )
    .unwrap();

    let initial = registry
        .get_all_nodes()
        .await
        .expect("initial get_all_nodes should succeed");
    assert!(initial.is_empty(), "registry should start empty");

    let remote = synctv_cluster::NodeInfo::new(
        "peer-node".to_string(),
        "10.0.0.9:50051".to_string(),
        "10.0.0.9:8080".to_string(),
    )
    .with_epoch(1);
    registry
        .register_remote(remote.clone())
        .await
        .expect("remote registration should succeed");

    let after_register = registry
        .get_all_nodes()
        .await
        .expect("remote registration should be immediately visible");
    assert_eq!(
        after_register.len(),
        1,
        "register_remote must invalidate cached node view"
    );
    assert_eq!(after_register[0].node_id, remote.node_id);

    registry
        .unregister_remote(&remote.node_id, Some(remote.epoch))
        .await
        .expect("remote unregister should succeed");

    let after_unregister = registry
        .get_all_nodes()
        .await
        .expect("remote unregister should be immediately visible");
    assert!(
        after_unregister.is_empty(),
        "unregister_remote must invalidate cached node view"
    );
}
