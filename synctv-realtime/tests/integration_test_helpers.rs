//! Common helpers for cluster integration tests
//!
//! This module provides shared infrastructure for multi-replica cluster tests,
//! including Redis container management and `RealtimeManager` creation.

#![allow(clippy::unwrap_used)]
#![allow(dead_code)]
use std::sync::Arc;
use std::time::Duration;
use synctv_core::cache::InvalidationMessage;
use synctv_core::models::{RealtimeActor, UserId};
use synctv_core::{DirectRedisConnectionRuntime, RedisConnectionRuntime, SharedStateProfile};
use synctv_core_testing::redis::{
    redis_connection_manager, redis_multiplexed_connection, RedisContainer,
};
use synctv_core_testing::{start_redis_url_with_label, test_redis_key_prefix};
use synctv_realtime::sync::{
    build_room_message_runtime, ConnectionId, RealtimeConfig, RealtimeManager,
};

const ROUND_TIMEOUT: Duration = Duration::from_millis(750);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

pub fn user_actor(user_id: UserId) -> RealtimeActor {
    RealtimeActor::user(user_id, user_id.to_string())
}

/// Redis test infrastructure that manages a single Redis container.
/// The container is automatically stopped when this struct is dropped.
pub struct TestRedis {
    pub redis_url: String,
    pub key_prefix: String,
    pub redis_container: Option<RedisContainer>,
}

impl TestRedis {
    /// Start a new Redis container for testing.
    /// Waits until Redis is actually accepting connections before returning.
    /// Applies a bounded startup timeout so tests fail deterministically when
    /// Docker is unavailable, while still tolerating slower CI/container hosts.
    pub async fn start() -> Self {
        let (redis_container, redis_url) = start_redis_url_with_label("cluster-integration").await;
        Self::wait_until_ready(&redis_url).await;

        Self {
            redis_url,
            key_prefix: test_redis_key_prefix("cluster-integration"),
            redis_container: Some(redis_container),
        }
    }

    pub fn cleanup(mut self) {
        if let Some(redis) = self.redis_container.take() {
            redis.cleanup();
        }
    }

    /// Wait until both Redis connection paths used by production code are ready:
    /// `ConnectionManager` and `MultiplexedConnection`.
    ///
    /// `NodeRegistry` uses `get_multiplexed_async_connection()`, so checking only
    /// `ConnectionManager` can still let tests proceed into a transient startup
    /// window where `register()` times out even though the container process has
    /// already started.
    pub async fn wait_until_ready(redis_url: &str) {
        let client = redis::Client::open(redis_url)
            .expect("Failed to create Redis client for readiness check");
        let _manager = redis_connection_manager(&client).await;
        let _multiplexed = redis_multiplexed_connection(&client).await;
    }
}

/// Helper: create a `RealtimeManager` connected to the given Redis URL.
pub async fn create_node(redis_url: &str, node_id: &str) -> RealtimeManager {
    create_node_with_prefix(redis_url, node_id, test_redis_key_prefix("cluster-node")).await
}

pub async fn create_node_with_prefix(
    redis_url: &str,
    node_id: &str,
    key_prefix: String,
) -> RealtimeManager {
    let client = redis::Client::open(redis_url).expect("Failed to open Redis client");
    let conn = redis_connection_manager(&client).await;
    let shared_runtime: Arc<dyn RedisConnectionRuntime> =
        Arc::new(DirectRedisConnectionRuntime::new(conn.clone()));
    let realtime_profile =
        SharedStateProfile::for_cluster_runtime(Some(shared_runtime), &key_prefix, true);
    let config = RealtimeConfig {
        distributed_transport_factory: Some(Arc::new(
            synctv_realtime::sync::RedisRealtimeMessageTransportFactory::new(
                synctv_core::coordination_runtime_from_client(client),
            ),
        )),
        message_runtime: build_room_message_runtime(&realtime_profile)
            .expect("shared message runtime should initialize"),
        distributed_enabled: true,
        node_id: node_id.to_string(),
        dedup_window: Duration::from_secs(10),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix,
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };
    RealtimeManager::new(config)
        .await
        .expect("Failed to create RealtimeManager")
}

/// Broadcasts until every target client has actually received the expected
/// chat message. This avoids brittle fixed sleeps when Redis Pub/Sub room
/// subscriptions are still propagating across replicas.
pub async fn broadcast_until_all_clients_receive(
    manager: &RealtimeManager,
    clients: &mut [(
        tokio::sync::mpsc::Receiver<synctv_realtime::sync::SharedRealtimeEvent>,
        ConnectionId,
    )],
    expected_message: &str,
    mut make_event: impl FnMut() -> synctv_realtime::sync::RealtimeEvent,
    label: &str,
) {
    broadcast_until_all_clients_receive_with(
        || {
            let _ = manager.broadcast(make_event());
        },
        clients,
        expected_message,
        label,
    )
    .await;
}

async fn broadcast_until_all_clients_receive_with(
    mut broadcast: impl FnMut(),
    clients: &mut [(
        tokio::sync::mpsc::Receiver<synctv_realtime::sync::SharedRealtimeEvent>,
        ConnectionId,
    )],
    expected_message: &str,
    label: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut pending = vec![true; clients.len()];
    while pending.iter().any(|is_pending| *is_pending) {
        broadcast();

        let round_deadline = tokio::time::Instant::now() + ROUND_TIMEOUT;
        loop {
            let mut made_progress = false;

            for (index, (rx, _conn_id)) in clients.iter_mut().enumerate() {
                if !pending[index] {
                    continue;
                }

                match rx.try_recv() {
                    Ok(event)
                        if matches!(
                            event.as_ref(),
                            synctv_realtime::sync::RealtimeEvent::ChatMessage { message, .. }
                                if message == expected_message
                        ) =>
                    {
                        pending[index] = false;
                        made_progress = true;
                    }
                    Ok(_) => {
                        made_progress = true;
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        panic!("{label} channel closed unexpectedly");
                    }
                }
            }

            if !pending.iter().any(|is_pending| *is_pending) {
                return;
            }

            let now = tokio::time::Instant::now();
            if now >= deadline || now >= round_deadline {
                break;
            }

            if !made_progress {
                tokio::time::sleep(POLL_INTERVAL).await;
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

pub async fn broadcast_until_room_event(
    manager: &RealtimeManager,
    room_rx: &mut tokio::sync::mpsc::Receiver<synctv_realtime::sync::SharedRealtimeEvent>,
    mut make_event: impl FnMut() -> synctv_realtime::sync::RealtimeEvent,
    mut matches: impl FnMut(&synctv_realtime::sync::RealtimeEvent) -> bool,
    label: &str,
) -> synctv_realtime::sync::SharedRealtimeEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);

    loop {
        manager.broadcast(make_event());

        match tokio::time::timeout(Duration::from_millis(750), room_rx.recv()).await {
            Ok(Some(event)) if matches(&event) => return event,
            Ok(None) => panic!("{label} channel closed unexpectedly"),
            Ok(Some(_)) | Err(_) => {}
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {label}"
        );
    }
}

pub async fn broadcast_until_admin_event(
    manager: &RealtimeManager,
    admin_rx: &mut tokio::sync::broadcast::Receiver<synctv_realtime::sync::RealtimeEvent>,
    mut make_event: impl FnMut() -> synctv_realtime::sync::RealtimeEvent,
    mut matches: impl FnMut(&synctv_realtime::sync::RealtimeEvent) -> bool,
    label: &str,
) -> synctv_realtime::sync::RealtimeEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);

    loop {
        manager.broadcast(make_event());

        if let Ok(result) = tokio::time::timeout(Duration::from_millis(750), admin_rx.recv()).await
        {
            match result {
                Ok(event) if matches(&event) => return event,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("{label} channel closed unexpectedly");
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            }
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {label}"
        );
    }
}

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

pub async fn wait_until_async<F, Fut>(label: &str, timeout: Duration, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        if condition().await {
            return;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {label}"
        );

        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn broadcast_until_cache_invalidation(
    manager: &RealtimeManager,
    rx: &mut tokio::sync::broadcast::Receiver<InvalidationMessage>,
    mut make_event: impl FnMut() -> synctv_realtime::sync::RealtimeEvent,
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
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
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

#[cfg(test)]
mod tests {
    #[tokio::test(start_paused = true)]
    async fn broadcast_until_all_clients_receive_respects_global_deadline() {
        let mut clients = Vec::new();
        for index in 0..5 {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            clients.push((
                rx,
                synctv_realtime::sync::ConnectionId::new(format!("conn-{index}")),
            ));
        }

        let task = tokio::spawn(async move {
            super::broadcast_until_all_clients_receive_with(
                || {},
                &mut clients,
                "never-delivered",
                "helper test",
            )
            .await;
        });

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(15_100)).await;
        tokio::task::yield_now().await;

        assert!(
            task.is_finished(),
            "helper timeout should honor the global deadline instead of multiplying by client count"
        );

        let join_result = task.await;
        assert!(
            join_result.is_err(),
            "timeout path should panic the helper task"
        );
    }
}
