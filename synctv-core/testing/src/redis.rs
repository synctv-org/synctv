use std::sync::Arc;

use redis::AsyncCommands;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::redis::Redis;
use tokio::sync::RwLock;

use crate::postgres::docker_startup_timeout;

pub type RedisContainer = ContainerAsync<Redis>;
pub type RedisConnectionManager = redis::aio::ConnectionManager;
pub type SharedRedisConnectionManager = Arc<RwLock<redis::aio::ConnectionManager>>;

async fn start_redis_inner() -> (RedisContainer, String, redis::Client) {
    let container = tokio::time::timeout(docker_startup_timeout(), Redis::default().start())
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
    (container, redis_url, client)
}

pub async fn start_redis_with_client() -> (RedisContainer, redis::Client) {
    let (container, _redis_url, client) = start_redis_inner().await;
    (container, client)
}

pub async fn start_redis() -> (RedisContainer, RedisConnectionManager) {
    let (container, client) = start_redis_with_client().await;
    let manager = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create Redis connection manager");
    (container, manager)
}

pub async fn start_redis_shared() -> (RedisContainer, SharedRedisConnectionManager) {
    let (container, manager) = start_redis().await;
    (container, Arc::new(RwLock::new(manager)))
}

pub async fn start_redis_url() -> (RedisContainer, String) {
    let (container, redis_url, _client) = start_redis_inner().await;
    (container, redis_url)
}

pub async fn wait_for_redis_ready(client: &redis::Client) {
    for _ in 0..120 {
        if let Ok(mut conn) = client.get_multiplexed_async_connection().await {
            let ping_result: redis::RedisResult<String> = redis::cmd("PING").query_async(&mut conn).await;
            let set_result: redis::RedisResult<()> = conn.set_ex("synctv:test:ping", "pong", 5).await;
            let get_result: redis::RedisResult<String> = conn.get("synctv:test:ping").await;
            if ping_result.is_ok() && set_result.is_ok() && get_result.as_deref() == Ok("pong") {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("Redis container did not become ready in time");
}
