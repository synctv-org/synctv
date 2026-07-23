use super::metrics::ShutdownTaskOutcome;
use super::model::system_time_to_unix_secs;
use super::redis_state::DISTRIBUTED_COUNTER_TTL_SECONDS;
use super::*;
use std::time::UNIX_EPOCH;
use synctv_core_testing::{start_redis_url_with_label, RedisContainer};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = std::result::Result<(), BoxError>;

fn missing(message: &'static str) -> BoxError {
    anyhow::anyhow!(message).into()
}

fn require_error<T>(
    result: std::result::Result<T, String>,
    message: &'static str,
) -> std::result::Result<String, BoxError> {
    match result {
        Ok(_) => Err(missing(message)),
        Err(error) => Ok(error),
    }
}

impl ConnectionManager {
    fn users_online_metric_delta_for_test(&self) -> isize {
        self.users_online_metric_increments
            .load(Ordering::Relaxed)
            .saturating_sub(self.users_online_metric_decrements.load(Ordering::Relaxed))
            .cast_signed()
    }

    fn drain_pending_retries_for_test(&self) -> std::result::Result<Vec<PendingRedisOp>, BoxError> {
        let mut guard = self.pending_retries_rx.try_lock()?;
        let rx = guard.as_mut().ok_or_else(|| {
            missing("pending retries receiver is only available before with_redis()")
        })?;

        let mut ops = Vec::new();
        while let Ok(op) = rx.try_recv() {
            ops.push(op);
        }
        Ok(ops)
    }

    fn enqueue_pending_retry_for_test(
        &self,
        op: PendingRedisOp,
    ) -> std::result::Result<(), BoxError> {
        self.pending_retries_tx.try_send(op)?;
        Ok(())
    }

    fn test_set_ttl_refresh_handle(&self, handle: tokio::task::JoinHandle<()>) {
        *self.ttl_refresh_handle.lock() = Some(handle);
    }
}

#[tokio::test]
async fn test_register_connection() {
    let manager = ConnectionManager::default();
    let user_id = UserId::expect_positive(10_000_010);

    let result = manager.register("conn1".to_string(), user_id).await;
    assert!(result.is_ok());
    assert_eq!(manager.connection_count(), 1);
    assert_eq!(manager.user_connection_count(&user_id), 1);
}

#[tokio::test]
async fn test_register_duplicate_connection_id_is_rejected_without_double_counting() -> TestResult {
    let manager = ConnectionManager::default();
    let user_id = UserId::expect_positive(10_000_110);

    manager.register("dup-conn".to_string(), user_id).await?;

    let duplicate = manager.register("dup-conn".to_string(), user_id).await;
    let duplicate_err = require_error(
        duplicate,
        "duplicate connection_id must be rejected deterministically",
    )?;
    assert!(
        duplicate_err.contains("already registered"),
        "duplicate register should report an already-registered error"
    );

    assert_eq!(manager.connection_count(), 1);
    assert_eq!(manager.user_connection_count(&user_id), 1);

    let conn = manager
        .get_connection("dup-conn")
        .ok_or_else(|| missing("original connection should remain intact"))?;
    assert_eq!(conn.user_id, user_id);
    Ok(())
}

#[tokio::test]
async fn test_duplicate_register_fails_fast_while_first_attempt_holds_lifecycle_lock() -> TestResult
{
    let first_entered = Arc::new(tokio::sync::Notify::new());
    let release_first = Arc::new(tokio::sync::Notify::new());
    let manager = Arc::new(
        ConnectionManager::default().with_register_after_lifecycle_lock_hook({
            let first_entered = Arc::clone(&first_entered);
            let release_first = Arc::clone(&release_first);
            Arc::new(move || {
                let first_entered = Arc::clone(&first_entered);
                let release_first = Arc::clone(&release_first);
                Box::pin(async move {
                    first_entered.notify_waiters();
                    release_first.notified().await;
                })
            })
        }),
    );
    let user_id = UserId::expect_positive(10_000_111);

    let first = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.register("dup-fast".to_string(), user_id).await })
    };

    first_entered.notified().await;

    let duplicate = tokio::time::timeout(
        Duration::from_millis(100),
        manager.register("dup-fast".to_string(), user_id),
    )
    .await?;
    let duplicate_err = require_error(duplicate, "duplicate registration must be rejected")?;
    assert!(
        duplicate_err.contains("already registered"),
        "duplicate registration should surface the existing claim error: {duplicate_err}"
    );

    release_first.notify_waiters();
    first.await??;
    assert_eq!(manager.connection_count(), 1);
    assert_eq!(manager.user_connection_count(&user_id), 1);
    Ok(())
}

#[tokio::test]
async fn test_connection_id_claim_rejects_concurrent_duplicate_attempts() -> TestResult {
    let manager = Arc::new(ConnectionManager::default());
    let claimed = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());

    let first = {
        let manager = Arc::clone(&manager);
        let claimed = Arc::clone(&claimed);
        let release = Arc::clone(&release);
        tokio::spawn(async move {
            let claim = manager.try_claim_connection_id("local-claim-race")?;
            claimed.notify_one();
            release.notified().await;
            drop(claim);
            Ok::<_, String>(())
        })
    };

    claimed.notified().await;

    let duplicate = manager.try_claim_connection_id("local-claim-race");
    assert!(
        duplicate.is_err(),
        "concurrent duplicate claim must fail while the first registration is in flight"
    );

    release.notify_one();
    first.await??;

    let retry = manager.try_claim_connection_id("local-claim-race");
    assert!(
        retry.is_ok(),
        "connection_id claim should be released after the in-flight registration finishes"
    );
    Ok(())
}

#[tokio::test]
async fn test_failed_rollback_enqueues_retry_operation() -> TestResult {
    let manager = ConnectionManager::default();

    manager
        .rollback_distributed_counter("rollback:test:key".to_string())
        .await;

    assert_eq!(
        manager.drain_pending_retries_for_test()?,
        vec![PendingRedisOp::Decr("rollback:test:key".to_string())],
        "failed rollback must enqueue a retry instead of silently dropping the counter repair"
    );
    Ok(())
}

#[test]
fn test_pending_retry_queue_preserves_metadata_cleanup_operations() -> TestResult {
    let manager = ConnectionManager::default();
    let cleanup_op = manager.unregister_cleanup_op(
        "conn-123",
        "token-123",
        UserId::expect_positive(40_123_001),
        Some(RoomId::expect_positive(40_123_002)),
    );

    manager.enqueue_pending_retry_for_test(cleanup_op.clone())?;

    assert_eq!(
        manager.drain_pending_retries_for_test()?,
        vec![cleanup_op],
        "metadata, index, and counter cleanup retries must be retained as one idempotent operation"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_register_same_connection_id_concurrently_with_redis_rejects_one_attempt() -> TestResult
{
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("dup-race:").await?;
    let manager =
        Arc::new(ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix));
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let user1 = UserId::expect_positive(10_000_112);
    let user2 = UserId::expect_positive(10_000_113);

    let task1 = {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            manager.register("dup-race-conn".to_string(), user1).await
        })
    };
    let task2 = {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            manager.register("dup-race-conn".to_string(), user2).await
        })
    };

    barrier.wait().await;

    let result1 = task1.await?;
    let result2 = task2.await?;
    let success_count = usize::from(result1.is_ok()) + usize::from(result2.is_ok());

    assert_eq!(
        success_count, 1,
        "only one concurrent register should succeed for the same connection_id"
    );
    assert_eq!(
        manager.connection_count(),
        1,
        "duplicate concurrent register must not double-count local connections"
    );
    assert_eq!(
        manager.user_connection_count(&user1) + manager.user_connection_count(&user2),
        1,
        "duplicate concurrent register must not corrupt per-user indexes"
    );

    let registered = manager
        .get_connection("dup-race-conn")
        .ok_or_else(|| missing("winning registration should remain present"))?;
    assert!(
        registered.user_id == user1 || registered.user_id == user2,
        "the surviving connection must belong to exactly one of the contenders"
    );

    let mut redis_conn = redis::aio::ConnectionManager::new(client.clone()).await?;
    let total_count: i64 = redis_conn
        .get(format!("{prefix}connections:total"))
        .await
        .unwrap_or(0);
    assert_eq!(
        total_count, 1,
        "duplicate concurrent register must not over-increment distributed total count"
    );

    manager.unregister("dup-race-conn").await;
    Ok(())
}

#[tokio::test]
async fn test_per_user_limit() {
    let limits = ConnectionLimits {
        max_per_user: 2,
        ..Default::default()
    };
    let manager = ConnectionManager::new(limits);
    let user_id = UserId::expect_positive(10_000_010);

    // First two should succeed
    assert!(manager.register("conn1".to_string(), user_id).await.is_ok());
    assert!(manager.register("conn2".to_string(), user_id).await.is_ok());

    // Third should fail
    let result = manager.register("conn3".to_string(), user_id).await;
    assert!(result.is_err());
    assert_eq!(manager.connection_count(), 2);
}

#[tokio::test]
async fn test_per_user_limit_holds_under_concurrent_registers_without_redis() -> TestResult {
    let limits = ConnectionLimits {
        max_per_user: 1,
        max_total: 10,
        ..Default::default()
    };
    let manager = Arc::new(ConnectionManager::new(limits));
    let user_id = UserId::expect_positive(10_000_114);
    let barrier = Arc::new(tokio::sync::Barrier::new(3));

    let task1 = {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            manager.register("conn-race-1".to_string(), user_id).await
        })
    };
    let task2 = {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            manager.register("conn-race-2".to_string(), user_id).await
        })
    };

    barrier.wait().await;

    let result1 = task1.await?;
    let result2 = task2.await?;
    let success_count = usize::from(result1.is_ok()) + usize::from(result2.is_ok());

    assert_eq!(
        success_count, 1,
        "only one concurrent register should succeed when max_per_user=1"
    );
    assert_eq!(
        manager.user_connection_count(&user_id),
        1,
        "local user index must not oversubscribe the per-user limit"
    );
    assert_eq!(manager.connection_count(), 1);
    Ok(())
}

#[tokio::test]
async fn test_join_room() -> TestResult {
    let manager = ConnectionManager::default();
    let user_id = UserId::expect_positive(10_000_010);
    let room_id = RoomId::expect_positive(10_000_092);

    manager.register("conn1".to_string(), user_id).await?;

    let result = manager.join_room("conn1", room_id).await;
    assert!(result.is_ok());
    assert_eq!(manager.room_connection_count(&room_id), 1);

    let conn = manager
        .get_connection("conn1")
        .ok_or_else(|| missing("connection should exist after joining room"))?;
    assert_eq!(conn.room_id.as_ref(), Some(&room_id));
    Ok(())
}

#[tokio::test]
async fn test_per_room_limit() -> TestResult {
    let limits = ConnectionLimits {
        max_per_room: 2,
        ..Default::default()
    };
    let manager = ConnectionManager::new(limits);
    let room_id = RoomId::expect_positive(10_000_092);

    // Register two connections and join room
    let user1 = UserId::expect_positive(10_000_010);
    let user2 = UserId::expect_positive(10_000_095);
    let user3 = UserId::expect_positive(10_000_115);

    manager.register("conn1".to_string(), user1).await?;
    manager.register("conn2".to_string(), user2).await?;
    manager.register("conn3".to_string(), user3).await?;

    assert!(manager.join_room("conn1", room_id).await.is_ok());
    assert!(manager.join_room("conn2", room_id).await.is_ok());

    // Third should fail
    let result = manager.join_room("conn3", room_id).await;
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn test_per_room_limit_holds_under_concurrent_join_without_redis() -> TestResult {
    let limits = ConnectionLimits {
        max_per_room: 1,
        max_total: 10,
        max_per_user: 10,
        ..Default::default()
    };
    let manager = Arc::new(ConnectionManager::new(limits));
    let room_id = RoomId::expect_positive(10_000_116);

    manager
        .register(
            "conn-room-race-1".to_string(),
            UserId::expect_positive(10_000_117),
        )
        .await?;
    manager
        .register(
            "conn-room-race-2".to_string(),
            UserId::expect_positive(10_000_118),
        )
        .await?;

    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let join1 = {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            manager.join_room("conn-room-race-1", room_id).await
        })
    };
    let join2 = {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            manager.join_room("conn-room-race-2", room_id).await
        })
    };

    barrier.wait().await;

    let result1 = join1.await?;
    let result2 = join2.await?;
    let success_count = usize::from(result1.is_ok()) + usize::from(result2.is_ok());

    assert_eq!(
        success_count, 1,
        "only one concurrent room join should succeed when max_per_room=1"
    );
    assert_eq!(manager.room_connection_count(&room_id), 1);
    Ok(())
}

#[tokio::test]
async fn test_concurrent_room_switch_for_same_connection_keeps_single_room_membership() -> TestResult
{
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let manager = Arc::new(
        ConnectionManager::default().with_join_room_before_commit_hook({
            let barrier = Arc::clone(&barrier);
            Arc::new(move || {
                let barrier = Arc::clone(&barrier);
                Box::pin(async move {
                    barrier.wait().await;
                })
            })
        }),
    );
    let user_id = UserId::expect_positive(10_000_119);
    let room_a = RoomId::expect_positive(10_000_120);
    let room_b = RoomId::expect_positive(10_000_121);

    manager.register("conn-switch".to_string(), user_id).await?;

    let join_a = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.join_room("conn-switch", room_a).await })
    };
    let join_b = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.join_room("conn-switch", room_b).await })
    };

    barrier.wait().await;

    join_a.await??;
    join_b.await??;

    let conn = manager
        .get_connection("conn-switch")
        .ok_or_else(|| missing("connection should exist after room switch race"))?;
    let final_room = conn
        .room_id
        .ok_or_else(|| missing("connection should belong to one room"))?;

    let room_a_connections = manager.get_room_connections(&room_a);
    let room_b_connections = manager.get_room_connections(&room_b);
    let rooms_with_connection = usize::from(
        room_a_connections
            .iter()
            .any(|info| info.connection_id == "conn-switch"),
    ) + usize::from(
        room_b_connections
            .iter()
            .any(|info| info.connection_id == "conn-switch"),
    );

    assert_eq!(
        rooms_with_connection, 1,
        "same connection_id must not remain indexed in multiple rooms after concurrent switches"
    );

    if final_room == room_a {
        assert_eq!(
            room_a_connections.len(),
            1,
            "final room must retain the connection exactly once"
        );
        assert!(
            room_b_connections.is_empty(),
            "non-final room must not retain a stale connection index"
        );
    } else {
        assert_eq!(
            final_room, room_b,
            "final room must be one of the two concurrently requested rooms"
        );
        assert_eq!(
            room_b_connections.len(),
            1,
            "final room must retain the connection exactly once"
        );
        assert!(
            room_a_connections.is_empty(),
            "non-final room must not retain a stale connection index"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_unregister_is_not_blocked_by_join_room_waiting_on_capacity_check() -> TestResult {
    let join_entered = Arc::new(tokio::sync::Notify::new());
    let release_join = Arc::new(tokio::sync::Notify::new());
    let manager = Arc::new(
        ConnectionManager::default().with_join_room_before_capacity_check_hook({
            let join_entered = Arc::clone(&join_entered);
            let release_join = Arc::clone(&release_join);
            Arc::new(move || {
                let join_entered = Arc::clone(&join_entered);
                let release_join = Arc::clone(&release_join);
                Box::pin(async move {
                    join_entered.notify_waiters();
                    release_join.notified().await;
                })
            })
        }),
    );
    let user_id = UserId::expect_positive(10_000_122);
    let room_id = RoomId::expect_positive(10_000_123);

    manager
        .register("conn-unregister-race".to_string(), user_id)
        .await?;

    let join_task = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.join_room("conn-unregister-race", room_id).await })
    };

    join_entered.notified().await;

    tokio::time::timeout(
        Duration::from_millis(100),
        manager.unregister("conn-unregister-race"),
    )
    .await?;

    assert!(
        manager.get_connection("conn-unregister-race").is_none(),
        "unregister should remove the connection immediately"
    );
    assert_eq!(
        manager.user_connection_count(&user_id),
        0,
        "unregister should free the per-user slot immediately"
    );

    release_join.notify_waiters();
    let join_err = require_error(
        join_task.await?,
        "join_room should observe that the connection was unregistered",
    )?;
    assert_eq!(join_err, "Connection not found");
    assert_eq!(manager.room_connection_count(&room_id), 0);
    Ok(())
}

#[tokio::test]
async fn test_record_message() -> TestResult {
    let manager = ConnectionManager::default();
    let user_id = UserId::expect_positive(10_000_010);

    manager.register("conn1".to_string(), user_id).await?;

    manager.record_message("conn1");
    manager.record_message("conn1");

    let conn = manager
        .get_connection("conn1")
        .ok_or_else(|| missing("connection should exist after recording message"))?;
    assert_eq!(conn.message_count, 2);
    assert_eq!(manager.total_messages(), 2);
    Ok(())
}

#[tokio::test]
async fn test_get_user_connections_distributed_without_redis_uses_local_state() -> TestResult {
    let manager = ConnectionManager::default();
    let user_id = UserId::expect_positive(10_000_010);

    manager.register("conn1".to_string(), user_id).await?;

    let conn_ids = manager.get_user_connections_distributed(&user_id).await?;

    assert_eq!(conn_ids, vec!["conn1".to_string()]);
    Ok(())
}

#[tokio::test]
async fn test_user_connection_count_in_room_distributed_counts_all_connections() -> TestResult {
    let manager = ConnectionManager::default();
    let user_id = UserId::expect_positive(10_000_125);
    let room_id = RoomId::expect_positive(10_000_126);
    let other_room_id = RoomId::expect_positive(10_000_127);

    manager
        .register("room-count-1".to_string(), user_id)
        .await?;
    manager
        .register("room-count-2".to_string(), user_id)
        .await?;
    manager
        .register("room-count-3".to_string(), user_id)
        .await?;
    manager.join_room("room-count-1", room_id).await?;
    manager.join_room("room-count-2", room_id).await?;
    manager.join_room("room-count-3", other_room_id).await?;

    let count = manager
        .user_connection_count_in_room_distributed(&user_id, &room_id)
        .await?;
    assert_eq!(count, 2);
    Ok(())
}

#[tokio::test]
async fn test_unregister() -> TestResult {
    let manager = ConnectionManager::default();
    let user_id = UserId::expect_positive(10_000_010);
    let room_id = RoomId::expect_positive(10_000_092);

    manager.register("conn1".to_string(), user_id).await?;
    manager.join_room("conn1", room_id).await?;

    assert_eq!(manager.connection_count(), 1);
    assert_eq!(manager.user_connection_count(&user_id), 1);
    assert_eq!(manager.room_connection_count(&room_id), 1);

    manager.unregister("conn1").await;

    assert_eq!(manager.connection_count(), 0);
    assert_eq!(manager.user_connection_count(&user_id), 0);
    assert_eq!(manager.room_connection_count(&room_id), 0);
    Ok(())
}

#[tokio::test]
async fn test_users_online_metric_deduplicates_multiple_connections_per_user() -> TestResult {
    let manager = ConnectionManager::default();
    let user_id = UserId::expect_positive(10_000_128);

    manager
        .register("metric-conn-1".to_string(), user_id)
        .await?;
    assert_eq!(
        manager.users_online_metric_delta_for_test(),
        1,
        "first connection for a user should increase online user count"
    );

    manager
        .register("metric-conn-2".to_string(), user_id)
        .await?;
    assert_eq!(
        manager.users_online_metric_delta_for_test(),
        1,
        "second connection for the same user must not double-count online users"
    );

    manager.unregister("metric-conn-1").await;
    manager.unregister("metric-conn-2").await;
    assert_eq!(manager.users_online_metric_delta_for_test(), 0);
    Ok(())
}

#[tokio::test]
async fn test_users_online_metric_decrements_only_after_last_connection_leaves() -> TestResult {
    let manager = ConnectionManager::default();
    let user_id = UserId::expect_positive(10_000_129);

    manager
        .register("metric-last-1".to_string(), user_id)
        .await?;
    manager
        .register("metric-last-2".to_string(), user_id)
        .await?;

    manager.unregister("metric-last-1").await;
    assert_eq!(
        manager.users_online_metric_delta_for_test(),
        1,
        "user should remain online while another connection is still active"
    );

    manager.unregister("metric-last-2").await;
    assert_eq!(
        manager.users_online_metric_delta_for_test(),
        0,
        "online user count should drop only after the final connection closes"
    );
    Ok(())
}

#[tokio::test]
async fn test_metrics() -> TestResult {
    let manager = ConnectionManager::default();
    let user1 = UserId::expect_positive(10_000_010);
    let user2 = UserId::expect_positive(10_000_095);

    manager.register("conn1".to_string(), user1).await?;
    manager.register("conn2".to_string(), user2).await?;

    manager.record_message("conn1");
    manager.record_message("conn2");

    let metrics = manager.metrics();
    assert_eq!(metrics.active_connections, 2);
    assert_eq!(metrics.total_connections_ever, 2);
    assert_eq!(metrics.total_messages, 2);
    assert_eq!(metrics.active_users, 2);
    Ok(())
}

#[tokio::test]
async fn test_idle_timeout() -> TestResult {
    let limits = ConnectionLimits {
        idle_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let manager = ConnectionManager::new(limits);
    let user_id = UserId::expect_positive(10_000_010);

    manager.register("conn1".to_string(), user_id).await?;

    // Wait for idle timeout
    tokio::time::sleep(Duration::from_millis(150)).await;

    let timeouts = manager.check_timeouts();
    assert_eq!(timeouts.len(), 1);
    assert_eq!(timeouts[0], "conn1");
    Ok(())
}

#[tokio::test]
async fn test_record_message_refreshes_idle_deadline() -> TestResult {
    let limits = ConnectionLimits {
        idle_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let manager = ConnectionManager::new(limits);
    let user_id = UserId::expect_positive(10_000_010);

    manager.register("conn1".to_string(), user_id).await?;

    tokio::time::sleep(Duration::from_millis(60)).await;
    manager.record_message("conn1");
    tokio::time::sleep(Duration::from_millis(60)).await;

    assert!(
        manager.check_timeouts().is_empty(),
        "fresh activity should postpone idle timeout"
    );

    tokio::time::sleep(Duration::from_millis(60)).await;
    let timeouts = manager.check_timeouts();
    assert_eq!(timeouts, vec!["conn1".to_string()]);
    Ok(())
}

#[tokio::test]
async fn test_rtc_timeout_marks_connection_left_before_disconnect() -> TestResult {
    let limits = ConnectionLimits {
        idle_timeout: Duration::from_secs(10),
        max_duration: Duration::from_secs(10),
        webrtc_session_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let manager = ConnectionManager::new(limits);
    let user_id = UserId::expect_positive(10_000_010);
    let room_id = RoomId::expect_positive(10_000_092);

    manager.register("conn1".to_string(), user_id).await?;
    manager.join_room("conn1", room_id).await?;
    manager.mark_voice_rtc_joined(&room_id, &user_id, "conn1", true);

    tokio::time::sleep(Duration::from_millis(150)).await;

    let timeouts = manager.check_timeouts();
    assert_eq!(timeouts, vec!["conn1".to_string()]);
    assert!(
        manager.get_voice_rtc_connections(&room_id).is_empty(),
        "RTC timeout should clear joined state before disconnect handling"
    );
    Ok(())
}

#[tokio::test]
async fn voice_rtc_capacity_is_atomic_and_leave_releases_a_slot() -> TestResult {
    let manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let room_id = RoomId::expect_positive(10_000_093);
    let participant_limit: usize = 3;
    let contender_count: usize = 10;
    for index in 0..contender_count {
        let connection_id = format!("voice-capacity-{index}");
        let user_id = UserId::expect_positive(
            10_001_000 + i64::try_from(index).expect("test index should fit i64"),
        );
        manager.register(connection_id.clone(), user_id).await?;
        manager.join_room(&connection_id, room_id).await?;
    }

    let barrier = Arc::new(tokio::sync::Barrier::new(contender_count));
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..contender_count {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            let connection_id = format!("voice-capacity-{index}");
            let user_id = UserId::expect_positive(
                10_001_000 + i64::try_from(index).expect("test index should fit i64"),
            );
            barrier.wait().await;
            manager
                .try_join_voice_rtc(&room_id, &user_id, &connection_id, participant_limit)
                .await
                .map(|outcome| (connection_id, user_id, outcome))
        });
    }

    let mut joined = Vec::new();
    let mut rejected = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let (connection_id, user_id, outcome) = result??;
        match outcome {
            VoiceRtcJoinOutcome::Joined => joined.push((connection_id, user_id)),
            VoiceRtcJoinOutcome::RoomAtCapacity => rejected.push((connection_id, user_id)),
            VoiceRtcJoinOutcome::AlreadyJoined => {
                return Err(missing("first join attempt cannot already be joined"));
            }
        }
    }

    assert_eq!(joined.len(), participant_limit);
    assert_eq!(rejected.len(), contender_count - participant_limit);
    assert_eq!(
        manager.get_voice_rtc_connections(&room_id).len(),
        participant_limit
    );

    let (leaving_connection, leaving_user) = joined
        .pop()
        .ok_or_else(|| missing("one joined participant is required"))?;
    assert!(
        manager
            .leave_voice_rtc(&room_id, &leaving_user, &leaving_connection)
            .await?
    );
    let (replacement_connection, replacement_user) = rejected
        .pop()
        .ok_or_else(|| missing("one rejected participant is required"))?;
    assert_eq!(
        manager
            .try_join_voice_rtc(
                &room_id,
                &replacement_user,
                &replacement_connection,
                participant_limit,
            )
            .await?,
        VoiceRtcJoinOutcome::Joined
    );
    assert_eq!(
        manager.get_voice_rtc_connections(&room_id).len(),
        participant_limit
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn voice_rtc_capacity_is_atomic_across_replicas() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn_a, prefix) =
        docker_redis_connection("voice-capacity-replicas:").await?;
    let conn_b = redis::aio::ConnectionManager::new(client.clone()).await?;
    let manager_a =
        Arc::new(ConnectionManager::new(ConnectionLimits::default()).with_redis(conn_a, &prefix));
    let manager_b =
        Arc::new(ConnectionManager::new(ConnectionLimits::default()).with_redis(conn_b, &prefix));
    let room_id = RoomId::expect_positive(10_000_094);
    let participant_limit: usize = 3;
    let contender_count: usize = 10;

    for index in 0..contender_count {
        let manager = if index % 2 == 0 {
            &manager_a
        } else {
            &manager_b
        };
        let connection_id = format!("voice-replica-capacity-{index}");
        let user_id = UserId::expect_positive(
            10_002_000 + i64::try_from(index).expect("test index should fit i64"),
        );
        manager.register(connection_id.clone(), user_id).await?;
        manager.join_room(&connection_id, room_id).await?;
    }

    let barrier = Arc::new(tokio::sync::Barrier::new(contender_count));
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..contender_count {
        let manager = if index % 2 == 0 {
            Arc::clone(&manager_a)
        } else {
            Arc::clone(&manager_b)
        };
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            let connection_id = format!("voice-replica-capacity-{index}");
            let user_id = UserId::expect_positive(
                10_002_000 + i64::try_from(index).expect("test index should fit i64"),
            );
            barrier.wait().await;
            manager
                .try_join_voice_rtc(&room_id, &user_id, &connection_id, participant_limit)
                .await
                .map(|outcome| (manager, connection_id, user_id, outcome))
        });
    }

    let mut joined = Vec::new();
    let mut rejected = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let (manager, connection_id, user_id, outcome) = result??;
        match outcome {
            VoiceRtcJoinOutcome::Joined => joined.push((manager, connection_id, user_id)),
            VoiceRtcJoinOutcome::RoomAtCapacity => {
                rejected.push((manager, connection_id, user_id));
            }
            VoiceRtcJoinOutcome::AlreadyJoined => {
                return Err(missing("first join attempt cannot already be joined"));
            }
        }
    }

    assert_eq!(joined.len(), participant_limit);
    assert_eq!(rejected.len(), contender_count - participant_limit);
    let mut redis = redis::aio::ConnectionManager::new(client).await?;
    let distributed_count: usize = redis.zcard(manager_a.voice_room_key(&room_id)).await?;
    assert_eq!(distributed_count, participant_limit);

    let (leaving_manager, leaving_connection, leaving_user) = joined
        .pop()
        .ok_or_else(|| missing("one joined participant is required"))?;
    assert!(
        leaving_manager
            .leave_voice_rtc(&room_id, &leaving_user, &leaving_connection)
            .await?
    );
    let (replacement_manager, replacement_connection, replacement_user) = rejected
        .pop()
        .ok_or_else(|| missing("one rejected participant is required"))?;
    assert_eq!(
        replacement_manager
            .try_join_voice_rtc(
                &room_id,
                &replacement_user,
                &replacement_connection,
                participant_limit,
            )
            .await?,
        VoiceRtcJoinOutcome::Joined
    );
    let distributed_count: usize = redis.zcard(manager_a.voice_room_key(&room_id)).await?;
    assert_eq!(distributed_count, participant_limit);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_redis_recovery_reconciles_connection_counts() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("test:").await?;
    let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

    let user_id = UserId::expect_positive(10_000_010);
    let room_id = RoomId::expect_positive(10_000_092);
    let user_key = format!("{prefix}connections:user:{user_id}");
    let room_key = format!("{prefix}connections:room:{room_id}");

    manager.register("conn1".to_string(), user_id).await?;
    manager.join_room("conn1", room_id).await?;

    let mut redis_conn = redis::aio::ConnectionManager::new(client.clone()).await?;
    let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);
    assert_eq!(user_count, 1);

    let _: () = redis_conn.del(&user_key).await?;
    let _: () = redis_conn.del(&room_key).await?;

    assert_eq!(manager.user_connection_count(&user_id), 1);

    manager.reconcile_with_redis().await;

    let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);
    assert_eq!(user_count, 1);

    manager.unregister("conn1").await;
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_redis_recovery_reconciles_stale_connections() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("test2:").await?;
    let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

    // Manually inject a stale connection id into the distributed user/room indexes
    // without creating a matching conn_mgr:conn:* metadata key.
    let mut redis_conn = redis::aio::ConnectionManager::new(client.clone()).await?;
    let stale_user_index = format!("{prefix}conn_mgr:user:user_stale");
    let stale_room_index = format!("{prefix}conn_mgr:room:room_stale");
    let stale_conn_key = format!("{prefix}conn_mgr:conn:stale_conn");
    let unrelated_conn_key = format!("{prefix}conn_mgr:conn:other_node_conn");
    let user_index_directory_key = format!("{prefix}{USER_INDEX_DIRECTORY_KEY_SUFFIX}");
    let room_index_directory_key = format!("{prefix}{ROOM_INDEX_DIRECTORY_KEY_SUFFIX}");

    let _: () = redis_conn.sadd(&stale_user_index, "stale_conn").await?;
    let _: () = redis_conn.sadd(&stale_room_index, "stale_conn").await?;
    let _: () = redis_conn
        .expire(&stale_user_index, CONNECTION_METADATA_TTL_SECONDS)
        .await?;
    let _: () = redis_conn
        .expire(&stale_room_index, CONNECTION_METADATA_TTL_SECONDS)
        .await?;
    let _: () = redis_conn
        .sadd(&user_index_directory_key, &stale_user_index)
        .await?;
    let _: () = redis_conn
        .sadd(&room_index_directory_key, &stale_room_index)
        .await?;

    // Also create a metadata key that belongs to another replica. Reconciliation
    // on this node must not delete it just because it is absent from local memory.
    let foreign_meta = ConnectionInfoPersistent {
        connection_id: "other_node_conn".to_string(),
        registration_token: "foreign-token".to_string(),
        user_id: UserId::expect_positive(20_000_201),
        actor_id: "usr_foreign".to_string(),
        room_id: Some(RoomId::expect_positive(20_000_202)),
        connected_at_unix: 0,
        last_activity_unix: 0,
        message_count: 0,
        voice_rtc_joined: false,
        voice_rtc_joined_at_unix: None,
    };
    let _: () = redis_conn
        .set(&unrelated_conn_key, serde_json::to_string(&foreign_meta)?)
        .await?;

    let stale_user_members: Vec<String> = redis_conn.smembers(&stale_user_index).await?;
    let stale_room_members: Vec<String> = redis_conn.smembers(&stale_room_index).await?;
    assert_eq!(stale_user_members, vec!["stale_conn".to_string()]);
    assert_eq!(stale_room_members, vec!["stale_conn".to_string()]);

    // Trigger reconciliation
    manager.reconcile_with_redis().await;

    // Stale index members should be cleaned up since the metadata key is missing.
    let stale_user_exists: bool = redis_conn.exists(&stale_user_index).await?;
    let stale_room_exists: bool = redis_conn.exists(&stale_room_index).await?;
    let stale_conn_exists: bool = redis_conn.exists(&stale_conn_key).await?;
    let unrelated_conn_exists: bool = redis_conn.exists(&unrelated_conn_key).await?;

    assert!(
        !stale_user_exists,
        "Empty stale user index should be removed during reconciliation"
    );
    assert!(
        !stale_room_exists,
        "Empty stale room index should be removed during reconciliation"
    );
    assert!(
        !stale_conn_exists,
        "Missing metadata key must remain absent"
    );
    assert!(
        unrelated_conn_exists,
        "Reconciliation must not delete connection metadata that may belong to another replica"
    );

    let user_directory_members: Vec<String> =
        redis_conn.smembers(&user_index_directory_key).await?;
    let room_directory_members: Vec<String> =
        redis_conn.smembers(&room_index_directory_key).await?;
    assert!(
        user_directory_members.is_empty(),
        "stale user index directory entry should be pruned during reconciliation"
    );
    assert!(
        room_directory_members.is_empty(),
        "stale room index directory entry should be pruned during reconciliation"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_redis_outage_during_register_eventually_consistent() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("test3:").await?;
    let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

    let user_id = UserId::expect_positive(10_000_010);
    let user_key = format!("{prefix}connections:user:{user_id}");

    manager.register("conn1".to_string(), user_id).await?;

    let mut redis_conn = redis::aio::ConnectionManager::new(client.clone()).await?;
    let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);
    assert_eq!(user_count, 1);

    let _: () = redis_conn.set(&user_key, 0).await?;

    assert_eq!(manager.user_connection_count(&user_id), 1);

    manager.reconcile_with_redis().await;

    let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);
    assert_eq!(user_count, 1);

    manager.unregister("conn1").await;
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_reconcile_with_redis_does_not_overwrite_other_replica_counters() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("test5:").await?;
    let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

    // Simulate another healthy replica already having active connections.
    let mut redis_conn = redis::aio::ConnectionManager::new(client.clone()).await?;
    let user_key = format!("{prefix}connections:user:20000101");
    let room_key = format!("{prefix}connections:room:20000102");
    let total_key = format!("{prefix}connections:total");

    let _: () = redis_conn.set(&user_key, 3).await?;
    let _: () = redis_conn
        .expire(&user_key, DISTRIBUTED_COUNTER_TTL_SECONDS)
        .await?;
    let _: () = redis_conn.set(&room_key, 4).await?;
    let _: () = redis_conn
        .expire(&room_key, DISTRIBUTED_COUNTER_TTL_SECONDS)
        .await?;
    let _: () = redis_conn.set(&total_key, 7).await?;
    let _: () = redis_conn
        .expire(&total_key, DISTRIBUTED_COUNTER_TTL_SECONDS)
        .await?;

    // This node has no local connections. Reconciliation must not zero out
    // counters that may belong to other replicas.
    manager.reconcile_with_redis().await;

    let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);
    let room_count: i64 = redis_conn.get(&room_key).await.unwrap_or(0);
    let total_count: i64 = redis_conn.get(&total_key).await.unwrap_or(0);

    assert_eq!(
        user_count, 3,
        "reconciliation must preserve user counters that may belong to other replicas"
    );
    assert_eq!(
        room_count, 4,
        "reconciliation must preserve room counters that may belong to other replicas"
    );
    assert_eq!(
        total_count, 7,
        "reconciliation must preserve total counters that may belong to other replicas"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_distributed_queries_prune_stale_index_members() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("test6:").await?;
    let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

    let user_a = UserId::expect_positive(10_000_130);
    let room_a = RoomId::expect_positive(10_000_120);
    let stale_missing = "conn-missing";
    let stale_mismatch = "conn-mismatch";
    let valid = "conn-valid";

    let mut redis_conn = redis::aio::ConnectionManager::new(client.clone()).await?;
    let user_index_key = format!("{prefix}conn_mgr:user:{user_a}");
    let room_index_key = format!("{prefix}conn_mgr:room:{room_a}");
    let mismatch_conn_key = format!("{prefix}conn_mgr:conn:{stale_mismatch}");
    let valid_conn_key = format!("{prefix}conn_mgr:conn:{valid}");

    let mismatch_metadata = ConnectionInfoPersistent {
        connection_id: stale_mismatch.to_string(),
        registration_token: "mismatch-token".to_string(),
        user_id: UserId::expect_positive(10_000_131),
        actor_id: "usr_mismatch".to_string(),
        room_id: Some(RoomId::expect_positive(10_000_121)),
        connected_at_unix: 0,
        last_activity_unix: 0,
        message_count: 0,
        voice_rtc_joined: false,
        voice_rtc_joined_at_unix: None,
    };
    let valid_metadata = ConnectionInfoPersistent {
        connection_id: valid.to_string(),
        registration_token: "valid-token".to_string(),
        user_id: user_a,
        actor_id: "usr_valid".to_string(),
        room_id: Some(room_a),
        connected_at_unix: 0,
        last_activity_unix: 0,
        message_count: 0,
        voice_rtc_joined: false,
        voice_rtc_joined_at_unix: None,
    };

    let _: () = redis_conn
        .set(
            &mismatch_conn_key,
            serde_json::to_string(&mismatch_metadata)?,
        )
        .await?;
    let _: () = redis_conn
        .set(&valid_conn_key, serde_json::to_string(&valid_metadata)?)
        .await?;

    for conn_id in [stale_missing, stale_mismatch, valid] {
        let _: () = redis_conn.sadd(&user_index_key, conn_id).await?;
        let _: () = redis_conn.sadd(&room_index_key, conn_id).await?;
    }
    let _: () = redis_conn
        .expire(&user_index_key, CONNECTION_METADATA_TTL_SECONDS)
        .await?;
    let _: () = redis_conn
        .expire(&room_index_key, CONNECTION_METADATA_TTL_SECONDS)
        .await?;
    let _: () = redis_conn
        .expire(&mismatch_conn_key, CONNECTION_METADATA_TTL_SECONDS)
        .await?;
    let _: () = redis_conn
        .expire(&valid_conn_key, CONNECTION_METADATA_TTL_SECONDS)
        .await?;

    let mut user_connections = manager.get_user_connections_distributed(&user_a).await?;
    let mut room_connections = manager.get_room_connections_distributed(&room_a).await?;
    user_connections.sort();
    room_connections.sort();

    assert_eq!(
        user_connections,
        vec![valid.to_string()],
        "distributed user lookup must prune missing and mismatched index members"
    );
    assert_eq!(
        room_connections,
        vec![valid.to_string()],
        "distributed room lookup must prune missing and mismatched index members"
    );

    let mut user_members: Vec<String> = redis_conn.smembers(&user_index_key).await?;
    let mut room_members: Vec<String> = redis_conn.smembers(&room_index_key).await?;
    user_members.sort();
    room_members.sort();
    assert_eq!(
        user_members,
        vec![valid.to_string()],
        "user index should retain only valid members after lazy pruning"
    );
    assert_eq!(
        room_members,
        vec![valid.to_string()],
        "room index should retain only valid members after lazy pruning"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_connection_metadata_ttl_uses_short_crash_safety_window() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("test7:").await?;
    let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

    let user_id = UserId::expect_positive(10_000_131);
    let room_id = RoomId::expect_positive(10_000_132);

    manager
        .register("conn-meta-ttl".to_string(), user_id)
        .await?;
    manager.join_room("conn-meta-ttl", room_id).await?;

    let mut redis_conn = redis::aio::ConnectionManager::new(client).await?;
    for key in [
        format!("{prefix}conn_mgr:conn:conn-meta-ttl"),
        format!("{prefix}conn_mgr:user:{user_id}"),
        format!("{prefix}conn_mgr:room:{room_id}"),
        format!("{prefix}{USER_INDEX_DIRECTORY_KEY_SUFFIX}"),
        format!("{prefix}{ROOM_INDEX_DIRECTORY_KEY_SUFFIX}"),
    ] {
        let ttl: i64 = redis_conn.ttl(&key).await?;
        assert!(
            (CONNECTION_METADATA_TTL_SECONDS - 5..=CONNECTION_METADATA_TTL_SECONDS).contains(&ttl),
            "metadata/index key {key} should use the short crash-safety TTL, got {ttl}s"
        );
    }

    manager.unregister("conn-meta-ttl").await;
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_reconcile_with_redis_repairs_missing_user_and_room_index_memberships() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("test8:").await?;
    let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

    let user_id = UserId::expect_positive(10_000_133);
    let room_id = RoomId::expect_positive(10_000_134);
    let user_index_key = format!("{prefix}conn_mgr:user:{user_id}");
    let room_index_key = format!("{prefix}conn_mgr:room:{room_id}");
    let user_index_directory_key = format!("{prefix}{USER_INDEX_DIRECTORY_KEY_SUFFIX}");
    let room_index_directory_key = format!("{prefix}{ROOM_INDEX_DIRECTORY_KEY_SUFFIX}");

    manager.register("conn-repair".to_string(), user_id).await?;
    manager.join_room("conn-repair", room_id).await?;

    let mut redis_conn = redis::aio::ConnectionManager::new(client).await?;
    let _: () = redis_conn.del(&user_index_key).await?;
    let _: () = redis_conn.del(&room_index_key).await?;
    let _: () = redis_conn
        .srem(&user_index_directory_key, &user_index_key)
        .await?;
    let _: () = redis_conn
        .srem(&room_index_directory_key, &room_index_key)
        .await?;

    manager.reconcile_with_redis().await;

    let user_connections = manager.get_user_connections_distributed(&user_id).await?;
    let room_connections = manager.get_room_connections_distributed(&room_id).await?;
    assert_eq!(
        user_connections,
        vec!["conn-repair".to_string()],
        "reconciliation should restore missing user index membership"
    );
    assert_eq!(
        room_connections,
        vec!["conn-repair".to_string()],
        "reconciliation should restore missing room index membership"
    );

    let user_directory_members: Vec<String> =
        redis_conn.smembers(&user_index_directory_key).await?;
    let room_directory_members: Vec<String> =
        redis_conn.smembers(&room_index_directory_key).await?;
    assert_eq!(
        user_directory_members,
        vec![user_index_key.clone()],
        "reconciliation should restore the user index directory entry"
    );
    assert_eq!(
        room_directory_members,
        vec![room_index_key.clone()],
        "reconciliation should restore the room index directory entry"
    );

    manager.unregister("conn-repair").await;
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_register_user_limit_rejection_rolls_back_distributed_total_counter() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("test4:").await?;

    let limits = ConnectionLimits {
        max_per_user: 1,
        ..ConnectionLimits::default()
    };
    let manager = ConnectionManager::new(limits).with_redis(conn, &prefix);
    let user_id = UserId::expect_positive(10_000_135);
    let user_key = format!("{prefix}connections:user:{user_id}");

    manager.register("conn1".to_string(), user_id).await?;

    let second = manager.register("conn2".to_string(), user_id).await;
    assert!(
        second.is_err(),
        "second connection should be rejected by distributed per-user limit"
    );

    let mut redis_conn = redis::aio::ConnectionManager::new(client.clone()).await?;
    let total_count: i64 = redis_conn
        .get(format!("{prefix}connections:total"))
        .await
        .unwrap_or(0);
    let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);

    assert_eq!(
        total_count, 1,
        "distributed total counter must be rolled back when register is rejected"
    );
    assert_eq!(
        user_count, 1,
        "distributed per-user counter should only reflect the accepted connection"
    );

    manager.unregister("conn1").await;
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_shared_redis_handle_observes_hot_swapped_connection() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("shared-test:").await?;
    let shared_conn = Arc::new(tokio::sync::RwLock::new(conn));
    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_shared_redis(shared_conn.clone(), &prefix);

    manager
        .register(
            "conn-shared".to_string(),
            UserId::expect_positive(10_000_136),
        )
        .await?;
    manager
        .join_room("conn-shared", RoomId::expect_positive(10_000_137))
        .await?;

    let initial_metadata_key = format!("{prefix}conn_mgr:conn:conn-shared");
    let initial_room_key = format!("{prefix}connections:room:10000137");
    let mut verify_conn = redis::aio::ConnectionManager::new(client.clone()).await?;
    let initial_metadata: Option<String> = verify_conn.get(&initial_metadata_key).await?;
    let initial_room_count: i64 = verify_conn.get(&initial_room_key).await.unwrap_or(0);
    assert!(
        initial_metadata.is_some(),
        "initial shared handle should write metadata"
    );
    assert_eq!(
        initial_room_count, 1,
        "initial shared handle should write room counter"
    );

    let replacement_conn = redis::aio::ConnectionManager::new(client.clone()).await?;
    *shared_conn.write().await = replacement_conn;

    let moved_room = RoomId::expect_positive(10_000_138);
    manager.join_room("conn-shared", moved_room).await?;

    let moved_room_key = format!("{prefix}connections:room:{moved_room}");
    let old_room_count: i64 = verify_conn.get(&initial_room_key).await.unwrap_or(0);
    let new_room_count: i64 = verify_conn.get(&moved_room_key).await.unwrap_or(0);
    let updated_metadata: String = verify_conn.get(&initial_metadata_key).await?;
    let updated_info: ConnectionInfoPersistent = serde_json::from_str(&updated_metadata)?;

    assert_eq!(
        old_room_count, 0,
        "old room counter should be decremented after move"
    );
    assert_eq!(
        new_room_count, 1,
        "new room counter should be incremented after move"
    );
    assert_eq!(
        updated_info.room_id,
        Some(moved_room),
        "post-swap operations must use the replacement shared Redis connection"
    );

    manager.unregister("conn-shared").await;
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_with_redis_runtime_accepts_trait_object_shared_runtime() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("shared-runtime:").await?;
    let shared_conn = Arc::new(tokio::sync::RwLock::new(conn));
    let runtime: Arc<dyn RedisConnectionRuntime> =
        Arc::new(SharedRedisConnectionRuntime::new(shared_conn.clone()));
    let manager =
        ConnectionManager::new_with_redis_runtime(ConnectionLimits::default(), runtime, &prefix);

    manager
        .register(
            "conn-runtime".to_string(),
            UserId::expect_positive(10_000_139),
        )
        .await?;

    let key = format!("{prefix}connections:user:10000139");
    let mut verify_conn = redis::aio::ConnectionManager::new(client).await?;
    let user_count: i64 = verify_conn.get(&key).await.unwrap_or(0);
    assert_eq!(user_count, 1);

    manager.unregister("conn-runtime").await;
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis"]
async fn test_pending_retries_cleanup_metadata_and_indexes_after_recovery() -> TestResult {
    use redis::AsyncCommands;

    let (_container, client, conn, prefix) = docker_redis_connection("shared-unregister:").await?;
    let shared_conn = Arc::new(tokio::sync::RwLock::new(conn));
    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_shared_redis(shared_conn.clone(), &prefix);

    let cleanup_op = manager.unregister_cleanup_op(
        "conn-recover",
        "token-recover",
        UserId::expect_positive(20_000_301),
        Some(RoomId::expect_positive(20_000_302)),
    );
    let PendingRedisOp::UnregisterCleanup {
        total_key,
        user_key,
        room_key,
        conn_key,
        user_index_key,
        room_index_key,
        ..
    } = cleanup_op.clone()
    else {
        return Err(missing(
            "unregister_cleanup_op must build an unregister cleanup operation",
        ));
    };

    let mut verify_conn = redis::aio::ConnectionManager::new(client.clone()).await?;
    let metadata = ConnectionInfoPersistent {
        connection_id: "conn-recover".to_string(),
        registration_token: "token-recover".to_string(),
        user_id: UserId::expect_positive(20_000_301),
        actor_id: "usr_recover".to_string(),
        room_id: Some(RoomId::expect_positive(20_000_302)),
        connected_at_unix: 0,
        last_activity_unix: 0,
        message_count: 0,
        voice_rtc_joined: false,
        voice_rtc_joined_at_unix: None,
    };
    let _: () = verify_conn
        .set(&conn_key, serde_json::to_string(&metadata)?)
        .await?;
    let _: () = verify_conn.set(&total_key, 1i64).await?;
    let _: () = verify_conn.set(&user_key, 1i64).await?;
    let _: () = verify_conn.set(&room_key, 1i64).await?;
    let _: () = verify_conn.sadd(&user_index_key, "conn-recover").await?;
    let _: () = verify_conn.sadd(&room_index_key, "conn-recover").await?;

    assert!(
        verify_conn.exists::<_, bool>(&conn_key).await?,
        "metadata should exist before retry processing"
    );
    manager.enqueue_pending_retry_for_test(cleanup_op)?;

    tokio::time::sleep(Duration::from_secs(6)).await;

    let metadata_exists: bool = verify_conn.exists(&conn_key).await?;
    let user_members: Vec<String> = verify_conn.smembers(&user_index_key).await?;
    let room_members: Vec<String> = verify_conn.smembers(&room_index_key).await?;
    let total_count: i64 = verify_conn.get(&total_key).await.unwrap_or(0);
    let user_count: i64 = verify_conn.get(&user_key).await.unwrap_or(0);
    let room_count: i64 = verify_conn.get(&room_key).await.unwrap_or(0);

    assert!(
        !metadata_exists,
        "pending retry processing must delete stale connection metadata"
    );
    assert!(
        user_members.is_empty(),
        "pending retry processing must remove stale user index members"
    );
    assert!(
        room_members.is_empty(),
        "pending retry processing must remove stale room index members"
    );
    assert_eq!(total_count, 0);
    assert_eq!(user_count, 0);
    assert_eq!(room_count, 0);

    manager.shutdown().await;
    Ok(())
}

#[test]
fn test_connection_info_persistent_serialization() -> TestResult {
    let persistent = ConnectionInfoPersistent {
        connection_id: "conn1".to_string(),
        registration_token: "token1".to_string(),
        user_id: UserId::expect_positive(20_000_401),
        actor_id: "usr_20000401".to_string(),
        room_id: Some(RoomId::expect_positive(20_000_402)),
        connected_at_unix: 1000,
        last_activity_unix: 2000,
        message_count: 5,
        voice_rtc_joined: true,
        voice_rtc_joined_at_unix: Some(1500),
    };

    let json = serde_json::to_string(&persistent)?;
    let deserialized: ConnectionInfoPersistent = serde_json::from_str(&json)?;

    assert_eq!(deserialized.connection_id, "conn1");
    assert_eq!(deserialized.registration_token, "token1");
    assert_eq!(deserialized.user_id, UserId::expect_positive(20_000_401));
    assert_eq!(
        deserialized.room_id,
        Some(RoomId::expect_positive(20_000_402))
    );
    assert_eq!(deserialized.message_count, 5);
    assert!(deserialized.voice_rtc_joined);
    Ok(())
}

#[test]
fn test_system_time_to_unix_secs_handles_pre_epoch_without_panicking() -> TestResult {
    let pre_epoch = UNIX_EPOCH
        .checked_sub(Duration::from_secs(1))
        .ok_or_else(|| missing("pre-epoch time should be constructible"))?;

    let result = std::panic::catch_unwind(|| system_time_to_unix_secs(pre_epoch));

    assert!(
        result.is_ok(),
        "cluster connection metadata conversion must not panic on clock rollback"
    );
    Ok(())
}

#[tokio::test]
async fn test_reserve_room_slot_enforces_limit() {
    let limits = ConnectionLimits {
        max_per_room: 3,
        ..ConnectionLimits::default()
    };
    let mgr = ConnectionManager::new(limits);
    let rid = RoomId::expect_positive(1);

    assert!(mgr.reserve_room_slot(&rid).is_ok());
    assert!(mgr.reserve_room_slot(&rid).is_ok());
    assert!(mgr.reserve_room_slot(&rid).is_ok());
    assert!(
        mgr.reserve_room_slot(&rid).is_err(),
        "Fourth reservation should fail (limit=3)"
    );
}

#[tokio::test]
async fn test_release_room_reservation_frees_slot() {
    let limits = ConnectionLimits {
        max_per_room: 1,
        ..ConnectionLimits::default()
    };
    let mgr = ConnectionManager::new(limits);
    let rid = RoomId::expect_positive(1);

    assert!(mgr.reserve_room_slot(&rid).is_ok());
    assert!(mgr.reserve_room_slot(&rid).is_err());

    mgr.release_room_reservation(&rid);
    assert!(
        mgr.reserve_room_slot(&rid).is_ok(),
        "Should succeed after releasing reservation"
    );
}

#[tokio::test]
async fn test_reserve_user_slot_enforces_limit() {
    let limits = ConnectionLimits {
        max_per_user: 2,
        ..ConnectionLimits::default()
    };
    let mgr = ConnectionManager::new(limits);
    let uid = UserId::expect_positive(1);

    assert!(mgr.reserve_user_slot(&uid).is_ok());
    assert!(mgr.reserve_user_slot(&uid).is_ok());
    assert!(
        mgr.reserve_user_slot(&uid).is_err(),
        "Third reservation should fail (limit=2)"
    );
}

#[tokio::test]
async fn test_release_user_reservation_frees_slot() {
    let limits = ConnectionLimits {
        max_per_user: 1,
        ..ConnectionLimits::default()
    };
    let mgr = ConnectionManager::new(limits);
    let uid = UserId::expect_positive(1);

    assert!(mgr.reserve_user_slot(&uid).is_ok());
    assert!(mgr.reserve_user_slot(&uid).is_err());

    mgr.release_user_reservation(&uid);
    assert!(
        mgr.reserve_user_slot(&uid).is_ok(),
        "Should succeed after releasing reservation"
    );
}

#[tokio::test]
async fn test_reserve_room_slot_independent_rooms() {
    let limits = ConnectionLimits {
        max_per_room: 1,
        ..ConnectionLimits::default()
    };
    let mgr = ConnectionManager::new(limits);
    let rid1 = RoomId::expect_positive(1);
    let rid2 = RoomId::expect_positive(2);

    assert!(mgr.reserve_room_slot(&rid1).is_ok());
    assert!(
        mgr.reserve_room_slot(&rid2).is_ok(),
        "Different rooms should have independent limits"
    );
    assert!(mgr.reserve_room_slot(&rid1).is_err());
    assert!(mgr.reserve_room_slot(&rid2).is_err());
}

#[tokio::test]
async fn test_reserve_release_idempotent() {
    let limits = ConnectionLimits {
        max_per_room: 2,
        ..ConnectionLimits::default()
    };
    let mgr = ConnectionManager::new(limits);
    let rid = RoomId::expect_positive(1);

    // Release without prior reservation should not panic
    mgr.release_room_reservation(&rid);

    // Normal reserve/release cycle
    assert!(mgr.reserve_room_slot(&rid).is_ok());
    mgr.release_room_reservation(&rid);

    // Should still be able to reserve up to the limit
    assert!(mgr.reserve_room_slot(&rid).is_ok());
    assert!(mgr.reserve_room_slot(&rid).is_ok());
    assert!(mgr.reserve_room_slot(&rid).is_err());
}

#[tokio::test]
async fn test_release_room_reservation_removes_zero_counter_entry() {
    let mgr = ConnectionManager::new(ConnectionLimits::default());
    let rid = RoomId::expect_positive(1);

    assert!(mgr.reserve_room_slot(&rid).is_ok());
    assert_eq!(mgr.pending_room_reservations.len(), 1);

    mgr.release_room_reservation(&rid);

    assert!(
        mgr.pending_room_reservations.get(&rid).is_none(),
        "room reservation entry should be removed after the count returns to zero"
    );
    assert_eq!(mgr.pending_room_reservations.len(), 0);
}

#[tokio::test]
async fn test_release_user_reservation_removes_zero_counter_entry() {
    let mgr = ConnectionManager::new(ConnectionLimits::default());
    let uid = UserId::expect_positive(1);

    assert!(mgr.reserve_user_slot(&uid).is_ok());
    assert_eq!(mgr.pending_user_reservations.len(), 1);

    mgr.release_user_reservation(&uid);

    assert!(
        mgr.pending_user_reservations.get(&uid).is_none(),
        "user reservation entry should be removed after the count returns to zero"
    );
    assert_eq!(mgr.pending_user_reservations.len(), 0);
}

#[tokio::test]
async fn test_shutdown_reports_background_task_panic() -> TestResult {
    let manager = ConnectionManager::new(ConnectionLimits::default());
    manager.test_set_ttl_refresh_handle(tokio::spawn(async {
        panic!("ttl refresh panic");
    }));

    let report = manager.shutdown().await;

    match report.ttl_refresh {
        Some(ShutdownTaskOutcome::Failed(message)) => {
            assert!(
                message.contains("panic"),
                "panic outcome should surface join error details: {message}"
            );
        }
        other => {
            return Err(anyhow::anyhow!("expected panic failure outcome, got {other:?}").into());
        }
    }
    Ok(())
}

#[tokio::test]
async fn test_shutdown_reports_cancelled_background_task() {
    let manager = ConnectionManager::new(ConnectionLimits::default());
    let handle = tokio::spawn(async {
        futures::future::pending::<()>().await;
    });
    handle.abort();
    manager.test_set_ttl_refresh_handle(handle);

    let report = manager.shutdown().await;

    assert_eq!(
        report.ttl_refresh,
        Some(ShutdownTaskOutcome::Cancelled),
        "aborted background tasks must not be silently swallowed during shutdown"
    );
}

#[tokio::test]
async fn test_shutdown_aborts_timed_out_background_task() -> TestResult {
    let manager = ConnectionManager::new(ConnectionLimits::default());
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        started_tx
            .send(())
            .expect("shutdown test should receive background task start signal");
        futures::future::pending::<()>().await;
    });
    manager.test_set_ttl_refresh_handle(handle);

    started_rx.await?;

    let report = manager.shutdown().await;

    assert_eq!(
        report.ttl_refresh,
        Some(ShutdownTaskOutcome::TimedOut),
        "shutdown should report timeout before forcing task abort"
    );
    assert!(
        manager.ttl_refresh_handle.lock().is_none(),
        "shutdown must drain the timed-out task handle after aborting it"
    );
    Ok(())
}

async fn docker_redis_connection(
    prefix: &str,
) -> std::result::Result<
    (
        RedisContainer,
        redis::Client,
        redis::aio::ConnectionManager,
        String,
    ),
    BoxError,
> {
    let sanitized_label = prefix.replace(':', "-");
    let (container, redis_url) = start_redis_url_with_label(&sanitized_label).await;
    let client = redis::Client::open(redis_url.as_str())?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match redis::aio::ConnectionManager::new(client.clone()).await {
            Ok(mut conn) => match redis::cmd("PING").query_async::<String>(&mut conn).await {
                Ok(_) => {
                    return Ok((container, client, conn, prefix.to_string()));
                }
                Err(error) => {
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "Redis test container did not become ready in time: {error}"
                    );
                }
            },
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "Failed to create Redis ConnectionManager: {error}"
                );
            }
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
