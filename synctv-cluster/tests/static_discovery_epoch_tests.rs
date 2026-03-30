#![allow(clippy::unwrap_used)]

use std::time::Duration;

use synctv_cluster::{NodeInfo, NodeRegistry};
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
        start_redis_url_with_label("static-discovery-epoch"),
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
async fn test_static_discovery_epoch_fencing_preserves_newer_registration_on_unreachable_peer() {
    let (_redis_container, redis_client) = setup_redis().await;
    let registry = NodeRegistry::new(
        redis_client.clone(),
        "self-node".to_string(),
        30,
        "static-discovery-epoch:",
    )
    .unwrap();

    let key = "static-discovery-epoch:cluster:nodes:peer-node";

    registry
        .register_remote(
            NodeInfo::new("peer-node".to_string(), "10.0.0.2:8080".to_string()).with_epoch(7),
        )
        .await
        .expect("registration with probed epoch should succeed");

    registry
        .register_remote(
            NodeInfo::new("peer-node".to_string(), "10.0.0.3:8080".to_string()).with_epoch(8),
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
        node["api_address"].as_str(),
        Some("10.0.0.3:8080"),
        "newer registration must remain intact after stale unregister"
    );
}
