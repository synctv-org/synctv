//! Common helpers for cluster integration tests
//!
//! This module provides shared infrastructure for multi-replica cluster tests,
//! including Redis container management and `ClusterManager` creation.

#![allow(clippy::unwrap_used)]
use std::time::Duration;
use synctv_cluster::{ClusterConfig, ClusterManager};
use synctv_core::cache::InvalidationMessage;
use synctv_core_testing::redis::RedisContainer;
use synctv_core_testing::start_redis_url_with_label;

/// Default Redis version for test containers
#[allow(dead_code)]
pub const REDIS_VERSION: &str = "7-alpine";

#[allow(dead_code)]
const DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 120;
#[allow(dead_code)]
const MIN_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 30;
#[allow(dead_code)]
const DOCKER_STARTUP_TIMEOUT_ENV: &str = "SYNCTV_TEST_DOCKER_STARTUP_TIMEOUT_SECS";

#[allow(dead_code)]
fn docker_startup_timeout() -> Duration {
    std::env::var(DOCKER_STARTUP_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| secs.max(MIN_DOCKER_STARTUP_TIMEOUT_SECS))
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS))
}

/// Redis test infrastructure that manages a single Redis container.
/// The container is automatically stopped when this struct is dropped.
#[allow(dead_code)]
pub struct TestRedis {
    pub redis_url: String,
    pub redis_container: Option<RedisContainer>,
}

impl TestRedis {
    /// Start a new Redis container for testing.
    /// Waits until Redis is actually accepting connections before returning.
    /// Applies a bounded startup timeout so tests fail deterministically when
    /// Docker is unavailable, while still tolerating slower CI/container hosts.
    #[allow(dead_code)]
    pub async fn start() -> Self {
        let (redis_container, redis_url) = start_redis_url_with_label("cluster-integration").await;
        Self::wait_until_ready(&redis_url).await;

        Self {
            redis_url,
            redis_container: Some(redis_container),
        }
    }

    #[allow(dead_code)]
    pub async fn cleanup(mut self) {
        if let Some(redis) = self.redis_container.take() {
            redis.cleanup().await;
        }
    }

    #[allow(dead_code)]
    pub async fn terminate_container(&mut self) {
        if let Some(redis) = self.redis_container.take() {
            redis.cleanup().await;
        }
    }

    /// Wait until both Redis connection paths used by production code are ready:
    /// `ConnectionManager` and `MultiplexedConnection`.
    ///
    /// `NodeRegistry` uses `get_multiplexed_async_connection()`, so checking only
    /// `ConnectionManager` can still let tests proceed into a transient startup
    /// window where `register()` times out even though the container process has
    /// already started.
    #[allow(dead_code)]
    pub async fn wait_until_ready(redis_url: &str) {
        let client = redis::Client::open(redis_url)
            .expect("Failed to create Redis client for readiness check");
        let mut retries = 0;

        loop {
            let manager_ready = match redis::aio::ConnectionManager::new(client.clone()).await {
                Ok(mut conn) => redis::cmd("PING")
                    .query_async::<()>(&mut conn)
                    .await
                    .is_ok(),
                Err(_) => false,
            };

            let multiplexed_ready = match client.get_multiplexed_async_connection().await {
                Ok(mut conn) => redis::cmd("PING")
                    .query_async::<()>(&mut conn)
                    .await
                    .is_ok(),
                Err(_) => false,
            };

            if manager_ready && multiplexed_ready {
                return;
            }

            if retries >= 60 {
                panic!(
                    "Redis not ready after {retries} retries: manager_ready={manager_ready}, multiplexed_ready={multiplexed_ready}"
                );
            }

            retries += 1;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

/// Helper: create a `ClusterManager` connected to the given Redis URL.
#[allow(dead_code)]
pub async fn create_node(redis_url: &str, node_id: &str) -> ClusterManager {
    let client = redis::Client::open(redis_url).expect("Failed to open Redis client");
    let conn = client
        .get_connection_manager()
        .await
        .expect("Failed to get ConnectionManager");
    let config = ClusterConfig {
        redis_client: Some(client),
        redis_conn: Some(conn),
        cluster_enabled: true,
        node_id: node_id.to_string(),
        dedup_window: Duration::from_secs(10),
        cleanup_interval: Duration::from_secs(30),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };
    ClusterManager::new(config, None, None)
        .await
        .expect("Failed to create ClusterManager")
}

/// Helper: create a `ClusterManager` with custom configuration
#[allow(dead_code)]
pub async fn create_node_with_config(
    redis_url: &str,
    node_id: &str,
    mut config_modifier: impl FnMut(&mut ClusterConfig),
) -> ClusterManager {
    let client = redis::Client::open(redis_url).expect("Failed to open Redis client");
    let conn = client
        .get_connection_manager()
        .await
        .expect("Failed to get ConnectionManager");
    let mut config = ClusterConfig {
        redis_client: Some(client),
        redis_conn: Some(conn),
        cluster_enabled: true,
        node_id: node_id.to_string(),
        dedup_window: Duration::from_secs(10),
        cleanup_interval: Duration::from_secs(30),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        parent_cancel_token: None,
    };
    config_modifier(&mut config);
    ClusterManager::new(config, None, None)
        .await
        .expect("Failed to create ClusterManager")
}

#[allow(dead_code)]
/// Broadcasts until every target client has actually received the expected
/// chat message. This avoids brittle fixed sleeps when Redis Pub/Sub room
/// subscriptions are still propagating across replicas.
pub async fn broadcast_until_all_clients_receive(
    manager: &ClusterManager,
    clients: &mut [(
        tokio::sync::mpsc::Receiver<synctv_cluster::sync::events::ClusterEvent>,
        String,
    )],
    expected_message: &str,
    mut make_event: impl FnMut() -> synctv_cluster::sync::events::ClusterEvent,
    label: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut pending = vec![true; clients.len()];

    while pending.iter().any(|is_pending| *is_pending) {
        manager.broadcast(make_event());

        for (index, (rx, _conn_id)) in clients.iter_mut().enumerate() {
            if !pending[index] {
                continue;
            }

            match tokio::time::timeout(Duration::from_millis(750), rx.recv()).await {
                Ok(Some(synctv_cluster::sync::events::ClusterEvent::ChatMessage {
                    message,
                    ..
                })) if message == expected_message => {
                    pending[index] = false;
                }
                Ok(Some(_)) => {}
                Ok(None) => panic!("{label} channel closed unexpectedly"),
                Err(_) => {}
            }
        }

        if pending.iter().any(|is_pending| *is_pending) && tokio::time::Instant::now() >= deadline {
            let missing = pending.into_iter().filter(|is_pending| *is_pending).count();
            panic!(
                "timed out waiting for {label}; {missing} clients still missing expected message"
            );
        }
    }
}

#[allow(dead_code)]
pub async fn broadcast_until_room_event(
    manager: &ClusterManager,
    room_rx: &mut tokio::sync::mpsc::Receiver<synctv_cluster::sync::events::ClusterEvent>,
    mut make_event: impl FnMut() -> synctv_cluster::sync::events::ClusterEvent,
    mut matches: impl FnMut(&synctv_cluster::sync::events::ClusterEvent) -> bool,
    label: &str,
) -> synctv_cluster::sync::events::ClusterEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);

    loop {
        manager.broadcast(make_event());

        match tokio::time::timeout(Duration::from_millis(750), room_rx.recv()).await {
            Ok(Some(event)) if matches(&event) => return event,
            Ok(Some(_)) => {}
            Ok(None) => panic!("{label} channel closed unexpectedly"),
            Err(_) => {}
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {label}"
        );
    }
}

#[allow(dead_code)]
pub async fn broadcast_until_admin_event(
    manager: &ClusterManager,
    admin_rx: &mut tokio::sync::broadcast::Receiver<synctv_cluster::sync::events::ClusterEvent>,
    mut make_event: impl FnMut() -> synctv_cluster::sync::events::ClusterEvent,
    mut matches: impl FnMut(&synctv_cluster::sync::events::ClusterEvent) -> bool,
    label: &str,
) -> synctv_cluster::sync::events::ClusterEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);

    loop {
        manager.broadcast(make_event());

        match tokio::time::timeout(Duration::from_millis(750), admin_rx.recv()).await {
            Ok(Ok(event)) if matches(&event) => return event,
            Ok(Ok(_)) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                panic!("{label} channel closed unexpectedly");
            }
            Err(_) => {}
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {label}"
        );
    }
}

#[allow(dead_code)]
pub async fn wait_until(label: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        if condition() {
            return;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {label}"
        );

        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[allow(dead_code)]
pub async fn broadcast_until_cache_invalidation(
    manager: &ClusterManager,
    rx: &mut tokio::sync::broadcast::Receiver<InvalidationMessage>,
    mut make_event: impl FnMut() -> synctv_cluster::sync::events::ClusterEvent,
    mut consume: impl FnMut(InvalidationMessage) -> bool,
    label: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);

    loop {
        manager.broadcast(make_event());

        loop {
            match tokio::time::timeout(Duration::from_millis(250), rx.recv()).await {
                Ok(Ok(message)) => {
                    if consume(message) {
                        return;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                    panic!("{label} channel closed unexpectedly");
                }
                Err(_) => break,
            }
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {label}"
        );
    }
}
