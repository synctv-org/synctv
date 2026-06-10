//! Tests for fail-closed distributed admission in `ConnectionManager`.

#![allow(clippy::unwrap_used)]

mod integration_test_helpers;

use std::time::Duration;

use async_trait::async_trait;
use integration_test_helpers::TestRedis;
use redis::aio::{ConnectionManager as RedisConnectionManager, ConnectionManagerConfig};
use synctv_core::models::id::{RoomId, UserId};
use synctv_core::{RedisConnectionRuntime, SharedStateProfile};
use synctv_core_testing::test_redis_key_prefix;
use synctv_realtime::sync::{build_connection_manager, ConnectionLimits, ConnectionManager};

fn uid(s: &str) -> UserId {
    UserId::expect_positive(stable_id(s))
}

fn rid(s: &str) -> RoomId {
    RoomId::expect_positive(stable_id(s))
}

fn stable_id(s: &str) -> i64 {
    i64::from(
        s.bytes()
            .fold(1u16, |acc, byte| acc.wrapping_add(u16::from(byte))),
    )
}

async fn redis_connection(redis_url: &str) -> redis::aio::ConnectionManager {
    let client = redis::Client::open(redis_url).expect("Failed to open Redis client");
    let config = ConnectionManagerConfig::new()
        .set_number_of_retries(1)
        .set_connection_timeout(Some(Duration::from_secs(2)))
        .set_response_timeout(Some(Duration::from_secs(2)))
        .set_min_delay(Duration::from_millis(50))
        .set_max_delay(Duration::from_millis(50));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut last_error = None;

    loop {
        match RedisConnectionManager::new_with_config(client.clone(), config.clone()).await {
            Ok(conn) => return conn,
            Err(error) if tokio::time::Instant::now() < deadline => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => {
                panic!(
                    "Failed to create Redis ConnectionManager after readiness wait: {}",
                    last_error.unwrap_or(error)
                );
            }
        }
    }
}

fn distributed_manager(
    limits: ConnectionLimits,
    conn: redis::aio::ConnectionManager,
    key_prefix: &str,
) -> ConnectionManager {
    build_connection_manager(
        limits,
        &SharedStateProfile::for_cluster_runtime(
            Some(synctv_core::direct_runtime(conn)),
            key_prefix,
            true,
        ),
    )
    .expect("shared realtime connection runtime should initialize")
}

struct HangingRedisRuntime;

#[async_trait]
impl RedisConnectionRuntime for HangingRedisRuntime {
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        std::future::pending().await
    }

    fn operation_timeout(&self) -> Duration {
        Duration::from_millis(10)
    }
}

fn manager_with_unavailable_redis(key_prefix: &str) -> ConnectionManager {
    build_connection_manager(
        ConnectionLimits::default(),
        &SharedStateProfile::for_cluster_runtime(
            Some(std::sync::Arc::new(HangingRedisRuntime)),
            key_prefix,
            true,
        ),
    )
    .expect("shared realtime connection runtime should initialize")
}

#[tokio::test]
async fn test_distributed_count_queries_fail_closed_when_redis_snapshot_times_out() {
    let manager = manager_with_unavailable_redis("fail-closed-counts:");

    let total_error = manager
        .connection_count_distributed()
        .await
        .expect_err("Redis-backed total count must not fall back to local-only state");
    assert!(total_error.contains("Distributed total connection count unavailable"));

    let room_error = manager
        .room_connection_count_distributed(&rid("room_a"))
        .await
        .expect_err("Redis-backed room count must not fall back to local-only state");
    assert!(room_error.contains("Distributed room connection count unavailable"));

    let batch_error = manager
        .room_connection_count_distributed_batch(&[&rid("room_a")])
        .await
        .expect_err("Redis-backed room count batch must not fall back to local-only state");
    assert!(batch_error.contains("Distributed room connection counts unavailable"));
}

#[tokio::test]
async fn test_distributed_presence_queries_fail_closed_when_redis_snapshot_times_out() {
    let manager = manager_with_unavailable_redis("fail-closed-presence:");

    let user_error = manager
        .get_user_connections_distributed(&uid("user1"))
        .await
        .expect_err("Redis-backed user connection lookup must not fall back locally");
    assert!(user_error.contains("Distributed user connection lookup unavailable"));

    let room_error = manager
        .get_room_connections_distributed(&rid("room_a"))
        .await
        .expect_err("Redis-backed room connection lookup must not fall back locally");
    assert!(room_error.contains("Distributed room connection lookup unavailable"));

    let online_error = manager
        .room_online_user_count_distributed(&rid("room_a"))
        .await
        .expect_err("Redis-backed online user count must not fall back locally");
    assert!(online_error.contains("Distributed online user counts unavailable"));

    let hot_error = manager
        .hot_room_online_user_counts_distributed()
        .await
        .expect_err("Redis-backed hot-room query must not fall back locally");
    assert!(hot_error.contains("Distributed hot-room online user counts unavailable"));

    let same_user_error = manager
        .has_other_connection_for_user_in_room_distributed(&uid("user1"), &rid("room_a"), "conn1")
        .await
        .expect_err("Redis-backed same-user presence lookup must not fall back locally");
    assert!(same_user_error.contains("Distributed same-user room presence lookup unavailable"));
}

#[tokio::test]
async fn test_register_fails_closed_when_distributed_limit_state_is_unavailable() {
    let manager = manager_with_unavailable_redis("fail_closed_register:");

    let result = tokio::time::timeout(
        Duration::from_secs(3),
        manager.register("conn1".to_string(), uid("user1")),
    )
    .await
    .expect("register should not hang when Redis disappears");

    let err = result.expect_err("register should fail closed when Redis is unavailable");
    assert!(
        err.contains("Distributed")
            && err.contains("connection check unavailable")
            && err.contains("cluster Redis is degraded"),
        "unexpected error: {err}"
    );
    assert_eq!(manager.connection_count(), 0);
}

#[tokio::test]
async fn test_join_room_rejects_when_distributed_room_limit_state_unavailable() {
    let manager = manager_with_unavailable_redis("fail_closed_join:");

    let target_room = rid("room_b");
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        manager.join_room("conn1", target_room),
    )
    .await
    .expect("join_room should not hang when Redis disappears");

    let err = result.expect_err("join_room should fail closed when Redis is unavailable");
    assert!(err.contains("Distributed room capacity check unavailable"));
    assert_eq!(manager.room_connection_count(&target_room), 0);
}

#[tokio::test]
async fn test_distributed_connection_queries_fail_closed_when_redis_is_unavailable() {
    let manager = manager_with_unavailable_redis("fail_closed_get:");

    let user_err = tokio::time::timeout(
        Duration::from_secs(3),
        manager.get_user_connections_distributed(&uid("user1")),
    )
    .await
    .expect("user distributed query should not hang when Redis disappears")
    .expect_err("user distributed query should fail closed when Redis is unavailable");
    assert!(user_err.contains("Distributed user connection lookup unavailable"));

    let room_err = tokio::time::timeout(
        Duration::from_secs(3),
        manager.get_room_connections_distributed(&rid("room_a")),
    )
    .await
    .expect("room distributed query should not hang when Redis disappears")
    .expect_err("room distributed query should fail closed when Redis is unavailable");
    assert!(room_err.contains("Distributed room connection lookup unavailable"));
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_register_rejects_when_distributed_total_limit_is_reached() {
    use redis::AsyncCommands;

    let redis = TestRedis::start().await;
    let conn = redis_connection(&redis.redis_url).await;
    let prefix = test_redis_key_prefix("fail-closed-total");
    let manager = distributed_manager(
        ConnectionLimits {
            max_total: 1,
            max_per_user: 10,
            ..ConnectionLimits::default()
        },
        conn,
        &prefix,
    );

    manager
        .register("conn1".to_string(), uid("user1"))
        .await
        .expect("first registration should succeed");

    let second = manager
        .register("conn2".to_string(), uid("user2"))
        .await
        .expect_err("second registration must fail once distributed total limit is reached");
    assert!(
        second.contains("across all replicas"),
        "unexpected error: {second}"
    );

    let mut verify_conn = redis_connection(&redis.redis_url).await;
    let total_count: i64 = verify_conn
        .get(format!("{prefix}connections:total"))
        .await
        .unwrap_or(0);
    let user2 = uid("user2");
    let user2_count: i64 = verify_conn
        .get(format!("{prefix}connections:user:{user2}"))
        .await
        .unwrap_or(0);

    assert_eq!(
        total_count, 1,
        "distributed total counter must roll back rejected registrations"
    );
    assert_eq!(
        user2_count, 0,
        "rejected registration must not leak per-user distributed counters"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_join_room_move_removes_old_room_from_distributed_index() {
    let redis = TestRedis::start().await;
    let conn = redis_connection(&redis.redis_url).await;
    let prefix = test_redis_key_prefix("move-room-idx");
    let manager = distributed_manager(ConnectionLimits::default(), conn, &prefix);

    let user = uid("user1");
    let room_a = rid("room_a");
    let room_b = rid("room_b");

    manager
        .register("conn1".to_string(), user)
        .await
        .expect("registration should succeed");
    manager
        .join_room("conn1", room_a)
        .await
        .expect("initial room join should succeed");
    manager
        .join_room("conn1", room_b)
        .await
        .expect("moving to another room should succeed");

    let old_room_connections = manager
        .get_room_connections_distributed(&room_a)
        .await
        .expect("old room distributed lookup should succeed");
    assert!(
        old_room_connections.is_empty(),
        "old room distributed index must not retain moved connection, got {old_room_connections:?}"
    );

    let new_room_connections = manager
        .get_room_connections_distributed(&room_b)
        .await
        .expect("new room distributed lookup should succeed");
    assert_eq!(new_room_connections, vec!["conn1".to_string()]);

    let online_counts = manager
        .room_online_user_count_distributed_batch(&[&room_a, &room_b])
        .await
        .expect("distributed online user counts should succeed");
    assert_eq!(online_counts, vec![0, 1]);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_join_room_rejection_rolls_back_distributed_room_counter() {
    use redis::AsyncCommands;

    let redis = TestRedis::start().await;
    let conn = redis_connection(&redis.redis_url).await;
    let prefix = test_redis_key_prefix("join-rollback");
    let manager = distributed_manager(
        ConnectionLimits {
            max_per_room: 1,
            ..ConnectionLimits::default()
        },
        conn,
        &prefix,
    );

    let room_a = rid("room_a");
    let room_b = rid("room_b");

    manager
        .register("conn_a".to_string(), uid("user_a"))
        .await
        .expect("conn_a registration should succeed");
    manager
        .register("conn_b".to_string(), uid("user_b"))
        .await
        .expect("conn_b registration should succeed");

    manager
        .join_room("conn_a", room_a)
        .await
        .expect("conn_a should join room_a");
    manager
        .join_room("conn_b", room_b)
        .await
        .expect("conn_b should join room_b");

    let err = manager
        .join_room("conn_a", room_b)
        .await
        .expect_err("room move must fail when target room is full");
    assert!(err.contains("Room at capacity"), "unexpected error: {err}");

    let mut verify_conn = redis_connection(&redis.redis_url).await;
    let room_b_count: i64 = verify_conn
        .get(format!("{prefix}connections:room:{room_b}"))
        .await
        .unwrap_or(0);

    assert_eq!(
        room_b_count, 1,
        "failed room move must roll back the distributed room counter"
    );
}
