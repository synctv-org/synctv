#![allow(clippy::unwrap_used)]

use synctv_cluster::NodeRegistry;
use synctv_core_testing::{start_redis_client_url_with_label, RedisContainer};

async fn setup_redis() -> (RedisContainer, redis::Client) {
    let (redis_container, redis_client, _redis_url) =
        start_redis_client_url_with_label("node-registry-unregister-remote").await;
    (redis_container, redis_client)
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_unregister_remote_rejects_missing_epoch() {
    let (_redis_container, redis_client) = setup_redis().await;
    let registry = NodeRegistry::new(
        synctv_core::coordination_runtime_from_client(redis_client),
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
        synctv_core::coordination_runtime_from_client(redis_client.clone()),
        "self-node".to_string(),
        30,
        "unregister-stale-epoch:",
    )
    .unwrap();

    let key = "unregister-stale-epoch:cluster:nodes:peer-node";

    registry
        .register_remote(
            synctv_cluster::NodeInfo::new("peer-node".to_string(), "10.0.0.2:8080".to_string())
                .with_epoch(5),
        )
        .await
        .expect("initial remote registration should succeed");

    registry
        .register_remote(
            synctv_cluster::NodeInfo::new("peer-node".to_string(), "10.0.0.3:8080".to_string())
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
        node["api_address"].as_str(),
        Some("10.0.0.3:8080"),
        "newer registration must remain intact"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_remote_writes_invalidate_cached_node_view_immediately() {
    let (_redis_container, redis_client) = setup_redis().await;
    let registry = NodeRegistry::new(
        synctv_core::coordination_runtime_from_client(redis_client),
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

    let remote =
        synctv_cluster::NodeInfo::new("peer-node".to_string(), "10.0.0.9:8080".to_string())
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
