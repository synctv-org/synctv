#![allow(clippy::unwrap_used)]

use synctv_cluster::{NodeInfo, NodeRegistry};
use synctv_core_testing::{start_redis_client_url_with_label, RedisContainer};

async fn setup_redis() -> (RedisContainer, redis::Client) {
    let (redis_container, redis_client, _redis_url) =
        start_redis_client_url_with_label("static-discovery-epoch").await;
    (redis_container, redis_client)
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_static_discovery_epoch_fencing_preserves_newer_registration_on_unreachable_peer() {
    let (_redis_container, redis_client) = setup_redis().await;
    let registry = NodeRegistry::new(
        synctv_core::coordination_runtime_from_client(redis_client.clone()),
        "self-node".to_string(),
        30,
        "static-discovery-epoch:",
    )
    .unwrap();

    let key = "static-discovery-epoch:cluster:nodes:peer-node";

    registry
        .register_remote(
            NodeInfo::new("peer-node".to_string(), "10.0.0.2:50051".to_string()).with_epoch(7),
        )
        .await
        .expect("registration with probed epoch should succeed");

    registry
        .register_remote(
            NodeInfo::new("peer-node".to_string(), "10.0.0.3:50051".to_string()).with_epoch(8),
        )
        .await
        .expect("newer registration should succeed");

    registry
        .unregister_remote("peer-node", Some(7))
        .await
        .expect("stale unregister should be ignored");

    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection");

    let stored: String = redis::cmd("GET")
        .arg(key)
        .query_async(&mut conn)
        .await
        .expect("newer registration should still exist");

    let node: serde_json::Value =
        serde_json::from_str(&stored).expect("stored node info should deserialize");
    assert_eq!(
        node["epoch"].as_u64(),
        Some(8),
        "stale unregister must not delete newer static-discovery registration"
    );
    assert_eq!(
        node["cluster_address"].as_str(),
        Some("10.0.0.3:50051"),
        "newer registration must remain intact after stale unregister"
    );
}
