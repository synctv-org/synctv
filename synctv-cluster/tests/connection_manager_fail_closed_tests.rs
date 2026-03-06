//! Tests for fail-closed distributed admission in `ConnectionManager`.

#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod integration_test_helpers;

use std::time::Duration;

use integration_test_helpers::TestRedis;
use redis::aio::{ConnectionManager as RedisConnectionManager, ConnectionManagerConfig};
use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::models::id::{RoomId, UserId};

fn uid(s: &str) -> UserId {
    UserId::from_string(s.to_string())
}

fn rid(s: &str) -> RoomId {
    RoomId::from_string(s.to_string())
}

async fn redis_connection(redis_url: &str) -> redis::aio::ConnectionManager {
    let client = redis::Client::open(redis_url).expect("Failed to open Redis client");
    let config = ConnectionManagerConfig::new()
        .set_number_of_retries(1)
        .set_connection_timeout(Some(Duration::from_millis(200)))
        .set_response_timeout(Some(Duration::from_millis(200)))
        .set_min_delay(Duration::from_millis(50))
        .set_max_delay(Duration::from_millis(50));

    RedisConnectionManager::new_with_config(client, config)
        .await
        .expect("Failed to create Redis ConnectionManager")
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_register_rejects_when_distributed_user_limit_state_unavailable() {
    let redis = TestRedis::start().await;
    let conn = redis_connection(&redis.redis_url).await;
    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_redis(conn, "fail_closed_register:");

    drop(redis._redis);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let result = tokio::time::timeout(
        Duration::from_secs(3),
        manager.register("conn1".to_string(), uid("user1")),
    )
    .await
    .expect("register should not hang when Redis disappears");

    let err = result.expect_err("register should fail closed when Redis is unavailable");
    assert!(err.contains("Distributed user connection check unavailable"));
    assert_eq!(manager.connection_count(), 0);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_join_room_rejects_when_distributed_room_limit_state_unavailable() {
    let redis = TestRedis::start().await;
    let conn = redis_connection(&redis.redis_url).await;
    let manager =
        ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, "fail_closed_join:");

    manager
        .register("conn1".to_string(), uid("user1"))
        .await
        .expect("initial registration should succeed while Redis is healthy");
    manager
        .join_room("conn1", rid("room_a"))
        .await
        .expect("initial room join should succeed while Redis is healthy");

    drop(redis._redis);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let target_room = rid("room_b");
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        manager.join_room("conn1", target_room.clone()),
    )
    .await
    .expect("join_room should not hang when Redis disappears");

    let err = result.expect_err("join_room should fail closed when Redis is unavailable");
    assert!(err.contains("Distributed room capacity check unavailable"));
    assert_eq!(manager.room_connection_count(&rid("room_a")), 1);
    assert_eq!(manager.room_connection_count(&target_room), 0);

    let conn = manager
        .get_connection("conn1")
        .expect("connection should remain tracked");
    assert_eq!(
        conn.room_id.as_ref().map(|room| room.as_str()),
        Some("room_a")
    );
}
