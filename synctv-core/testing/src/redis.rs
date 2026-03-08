use std::process::Command;
use std::sync::Arc;

use redis::AsyncCommands;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::redis::Redis;
use tokio::sync::RwLock;

use crate::postgres::docker_startup_timeout;

pub type RedisConnectionManager = redis::aio::ConnectionManager;
pub type RedisConnectionHandle = Arc<RwLock<redis::aio::ConnectionManager>>;

fn sanitize_container_name(raw: &str) -> String {
    let mut name: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    name.truncate(48);
    while name.ends_with('-') {
        name.pop();
    }
    if name.is_empty() {
        "redis-test".to_string()
    } else {
        name
    }
}

fn current_test_label() -> String {
    std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::thread::current().name().map(str::to_owned))
        .map(|value| sanitize_container_name(&value))
        .unwrap_or_else(|| "unknown-test".to_string())
}

fn redis_container_name(label: &str) -> String {
    format!(
        "synctv-redis-{}-{}-{}",
        current_test_label(),
        sanitize_container_name(label),
        nanoid::nanoid!(6).to_lowercase()
    )
}

fn named_redis_request(container_name: &str) -> testcontainers::ContainerRequest<Redis> {
    Redis::default().with_container_name(container_name.to_string())
}

pub struct RedisContainer {
    inner: Option<ContainerAsync<Redis>>,
    name: String,
}

impl RedisContainer {
    fn new(inner: ContainerAsync<Redis>, name: String) -> Self {
        Self {
            inner: Some(inner),
            name,
        }
    }

    pub async fn cleanup(mut self) {
        if let Some(container) = self.inner.take() {
            let _ = container.rm().await;
        }
    }

    pub fn raw(&self) -> &ContainerAsync<Redis> {
        self.inner
            .as_ref()
            .expect("redis container should still be present")
    }
}

impl std::ops::Deref for RedisContainer {
    type Target = ContainerAsync<Redis>;

    fn deref(&self) -> &Self::Target {
        self.raw()
    }
}

impl Drop for RedisContainer {
    fn drop(&mut self) {
        if let Some(container) = self.inner.take() {
            drop(container);
        }
        let _ = Command::new("docker")
            .args(["rm", "-f", self.name.as_str()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

async fn start_redis_inner(label: &str) -> (RedisContainer, String, redis::Client) {
    let container_name = redis_container_name(label);
    let container = tokio::time::timeout(
        docker_startup_timeout(),
        named_redis_request(&container_name).start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Redis");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get port");
    let redis_url = format!("redis://127.0.0.1:{port}");
    let client = redis::Client::open(redis_url.clone()).expect("Failed to create Redis client");
    wait_for_redis_ready(&client).await;
    (
        RedisContainer::new(container, container_name),
        redis_url,
        client,
    )
}

pub async fn start_redis_with_client() -> (RedisContainer, redis::Client) {
    let (container, _redis_url, client) = start_redis_inner("client").await;
    (container, client)
}

pub async fn start_redis_url_with_label(label: &str) -> (RedisContainer, String) {
    let (container, redis_url, _client) = start_redis_inner(label).await;
    (container, redis_url)
}

pub async fn start_redis() -> (RedisContainer, RedisConnectionManager) {
    let (container, client) = start_redis_with_client().await;
    let manager = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create Redis connection manager");
    (container, manager)
}

pub async fn start_redis_handle() -> (RedisContainer, RedisConnectionHandle) {
    let (container, manager) = start_redis().await;
    (container, Arc::new(RwLock::new(manager)))
}

pub async fn start_redis_url() -> (RedisContainer, String) {
    let (container, redis_url, _client) = start_redis_inner("url").await;
    (container, redis_url)
}

pub async fn wait_for_redis_ready(client: &redis::Client) {
    for _ in 0..120 {
        if let Ok(mut conn) = client.get_multiplexed_async_connection().await {
            let ping_result: redis::RedisResult<String> =
                redis::cmd("PING").query_async(&mut conn).await;
            let set_result: redis::RedisResult<()> =
                conn.set_ex("synctv:test:ping", "pong", 5).await;
            let get_result: redis::RedisResult<String> = conn.get("synctv:test:ping").await;
            if ping_result.is_ok() && set_result.is_ok() && get_result.as_deref() == Ok("pong") {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("Redis container did not become ready in time");
}
