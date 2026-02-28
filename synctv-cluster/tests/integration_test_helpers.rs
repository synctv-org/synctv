//! Common helpers for cluster integration tests
//!
//! This module provides shared infrastructure for multi-replica cluster tests,
//! including Redis container management and ClusterManager creation.

use std::time::Duration;
use synctv_cluster::{ClusterConfig, ClusterManager};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::redis::Redis;

/// Default Redis version for test containers
pub const REDIS_VERSION: &str = "7-alpine";

/// Redis test infrastructure that manages a single Redis container.
/// The container is automatically stopped when this struct is dropped.
pub struct TestRedis {
    pub redis_url: String,
    pub _redis: ContainerAsync<Redis>,
}

impl TestRedis {
    /// Start a new Redis container for testing
    pub async fn start() -> Self {
        let redis_container = Redis::default()
            .start()
            .await
            .expect("Failed to start Redis container");

        let redis_host = redis_container
            .get_host()
            .await
            .expect("Failed to get Redis host");
        let redis_port = redis_container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Redis port");

        let redis_url = format!("redis://{}:{}", redis_host, redis_port);

        Self {
            redis_url,
            _redis: redis_container,
        }
    }

}

/// Helper: create a `ClusterManager` connected to the given Redis URL.
pub async fn create_node(redis_url: &str, node_id: &str) -> ClusterManager {
    let client = redis::Client::open(redis_url).expect("Failed to open Redis client");
    let conn = client.get_connection_manager().await.expect("Failed to get ConnectionManager");
    let config = ClusterConfig {
        redis_client: Some(client),
        redis_conn: Some(conn),
        node_id: node_id.to_string(),
        dedup_window: Duration::from_secs(10),
        cleanup_interval: Duration::from_secs(30),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
    };
    ClusterManager::new(config, None, None)
        .await
        .expect("Failed to create ClusterManager")
}

/// Helper: create a `ClusterManager` with custom configuration
pub async fn create_node_with_config(redis_url: &str, node_id: &str, mut config_modifier: impl FnMut(&mut ClusterConfig)) -> ClusterManager {
    let client = redis::Client::open(redis_url).expect("Failed to open Redis client");
    let conn = client.get_connection_manager().await.expect("Failed to get ConnectionManager");
    let mut config = ClusterConfig {
        redis_client: Some(client),
        redis_conn: Some(conn),
        node_id: node_id.to_string(),
        dedup_window: Duration::from_secs(10),
        cleanup_interval: Duration::from_secs(30),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
    };
    config_modifier(&mut config);
    ClusterManager::new(config, None, None)
        .await
        .expect("Failed to create ClusterManager")
}
