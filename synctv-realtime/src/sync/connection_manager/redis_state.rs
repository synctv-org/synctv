use std::sync::LazyLock;

use redis::AsyncCommands;
use synctv_core::RedisConnectionRuntime;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// TTL for distributed connection counters in Redis (seconds).
///
/// Acts as a crash-safety mechanism: if a node crashes without decrementing,
/// the counter will expire after this duration. Set to 2x the TTL refresh
/// interval (60s) to balance crash recovery speed with tolerance for
/// transient network issues. This allows the counter to survive one missed
/// refresh while detecting crashes more quickly than the previous 3x multiplier.
pub(super) const DISTRIBUTED_COUNTER_TTL_SECONDS: i64 = 120;

/// Maximum number of keys to refresh in a single batch during TTL refresh.
///
/// This prevents memory and network pressure when there are many connections.
/// With 10,000 connections, we'll have ~30,000 keys (counter + metadata per connection),
/// which will be processed in ~30 batches of 1000 keys each.
pub(super) const TTL_REFRESH_BATCH_SIZE: usize = 1000;

/// TTL for distributed connection metadata and index keys in Redis (seconds).
///
/// These keys back cross-replica presence queries (`conn_mgr:conn:*`,
/// `conn_mgr:user:*`, `conn_mgr:room:*`). They must expire quickly after a pod
/// crash so dead connections do not remain visible for hours, but stay alive
/// through transient missed refreshes while a pod is healthy.
///
/// With a 60-second refresh interval, a 180-second TTL survives two missed
/// refreshes while still letting crashed-pod state drain within a few minutes.
pub(super) const CONNECTION_METADATA_TTL_SECONDS: i64 = 180;
pub(super) const USER_INDEX_DIRECTORY_KEY_SUFFIX: &str = "conn_mgr:user_indexes";
pub(super) const ROOM_INDEX_DIRECTORY_KEY_SUFFIX: &str = "conn_mgr:room_indexes";

pub(super) static UNREGISTER_CLEANUP_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local current_metadata = redis.call('GET', KEYS[5])
        local metadata_matches = false
        if current_metadata then
            local ok, obj = pcall(cjson.decode, current_metadata)
            if ok and obj and obj.registration_token == ARGV[4] then
                metadata_matches = true
            end
        end

        local first_cleanup = redis.call('SET', KEYS[1], '1', 'NX', 'EX', ARGV[1])
        if first_cleanup then
            local total = redis.call('DECR', KEYS[2])
            if total < 0 then redis.call('DEL', KEYS[2]) end

            local user_total = redis.call('DECR', KEYS[3])
            if user_total < 0 then redis.call('DEL', KEYS[3]) end

            if ARGV[3] == '1' then
                local room_total = redis.call('DECR', KEYS[4])
                if room_total < 0 then redis.call('DEL', KEYS[4]) end
            end
        end

        if metadata_matches then
            redis.call('DEL', KEYS[5])
            redis.call('SREM', KEYS[6], ARGV[2])
            if ARGV[3] == '1' then
                redis.call('SREM', KEYS[7], ARGV[2])
            end
        end

        if first_cleanup then
            return 1
        end
        return 0
        ",
    )
});

pub(super) static BATCH_REFRESH_TTLS_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r#"
        local counter_ttl = tonumber(ARGV[1])
        local metadata_ttl = tonumber(ARGV[2])
        local refreshed = 0

        -- Refresh counter keys (KEYS[1] to KEYS[N] where N = #counter_keys)
        local num_counter_keys = tonumber(ARGV[3])
        for i = 1, num_counter_keys do
            local key = KEYS[i]
            if redis.call("EXISTS", key) == 1 then
                redis.call("EXPIRE", key, counter_ttl)
                refreshed = refreshed + 1
            end
        end

        -- Refresh metadata keys
        local num_metadata_keys = tonumber(ARGV[4])
        for i = 1, num_metadata_keys do
            local key = KEYS[num_counter_keys + i]
            if redis.call("EXISTS", key) == 1 then
                redis.call("EXPIRE", key, metadata_ttl)
                refreshed = refreshed + 1
            end
        end

        return refreshed
        "#,
    )
});

pub(super) static SYNC_COUNTER_MIN_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"local current = redis.call('GET', KEYS[1])
          local current_num = 0
          if current ~= false then
            current_num = tonumber(current)
          end
          local expected_min = tonumber(ARGV[1])
          if current_num < expected_min then
            redis.call('SET', KEYS[1], ARGV[1])
            redis.call('EXPIRE', KEYS[1], ARGV[2])
            return {current_num, 1}
          end
          return {current_num, 0}",
    )
});

pub(super) static INCR_EXPIRE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        "local count = redis.call('INCR', KEYS[1]) \
         redis.call('EXPIRE', KEYS[1], ARGV[1]) \
         return count",
    )
});

pub(super) static DECR_DELETE_NEGATIVE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"local v = redis.call('DECR', KEYS[1])
          if v < 0 then
            redis.call('DEL', KEYS[1])
          end
          return v",
    )
});

/// A failed Redis counter operation that should be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PendingRedisOp {
    /// Decrement a counter key
    Decr(String),
    /// Idempotently clean up one unregistered connection's distributed state.
    UnregisterCleanup {
        cleanup_key: String,
        total_key: String,
        user_key: String,
        room_key: String,
        conn_key: String,
        user_index_key: String,
        room_index_key: String,
        connection_id: String,
        registration_token: String,
        has_room: bool,
    },
}

pub(super) struct UnregisterCleanupScriptArgs<'a> {
    pub(super) cleanup_key: &'a str,
    pub(super) total_key: &'a str,
    pub(super) user_key: &'a str,
    pub(super) room_key: &'a str,
    pub(super) conn_key: &'a str,
    pub(super) user_index_key: &'a str,
    pub(super) room_index_key: &'a str,
    pub(super) connection_id: &'a str,
    pub(super) registration_token: &'a str,
    pub(super) has_room: bool,
}

pub(super) fn spawn_pending_retries_task(
    redis_conn: std::sync::Arc<dyn RedisConnectionRuntime>,
    mut rx: mpsc::Receiver<PendingRedisOp>,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        const MAX_OP_RETRIES: u32 = 3;
        const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

        let mut pending: Vec<(PendingRedisOp, u32)> = Vec::new();
        let mut ticker = tokio::time::interval(RETRY_INTERVAL);
        ticker.tick().await;

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    info!("Pending Redis retries task shutting down");
                    return;
                }
                _ = ticker.tick() => {
                    while let Ok(op) = rx.try_recv() {
                        pending.push((op, 0));
                    }

                    if pending.is_empty() {
                        continue;
                    }

                    let mut still_pending = Vec::new();
                    let mut conn = match tokio::time::timeout(
                        redis_conn.operation_timeout(),
                        redis_conn.snapshot(),
                    )
                    .await
                    {
                        Ok(Ok(conn)) => conn,
                        Ok(Err(error)) => {
                            warn!(
                                error = %error,
                                pending_ops = pending.len(),
                                "Redis connection snapshot failed while retrying pending counter operations"
                            );
                            retry_or_drop_pending_after_snapshot_error(
                                &mut pending,
                                &mut still_pending,
                                MAX_OP_RETRIES,
                                "snapshot failure",
                            );
                            pending = still_pending;
                            continue;
                        }
                        Err(_) => {
                            warn!(
                                timeout_ms = redis_conn.operation_timeout().as_millis(),
                                pending_ops = pending.len(),
                                "Redis connection snapshot timed out while retrying pending counter operations"
                            );
                            retry_or_drop_pending_after_snapshot_error(
                                &mut pending,
                                &mut still_pending,
                                MAX_OP_RETRIES,
                                "snapshot timeout",
                            );
                            pending = still_pending;
                            continue;
                        }
                    };

                    for (op, attempts) in pending.drain(..) {
                        let result = run_pending_retry_operation(&redis_conn, &mut conn, &op).await;

                        match result {
                            Ok(_) => {
                                debug!(op = ?op, "Pending Redis retry succeeded");
                            }
                            Err(e) => {
                                let next_attempt = attempts + 1;
                                if next_attempt >= MAX_OP_RETRIES {
                                    tracing::error!(
                                        op = ?op,
                                        attempts = next_attempt,
                                        error = %e,
                                        "ALERT: Dropping failed Redis counter operation after max retries. \
                                         Distributed connection count may be inaccurate. \
                                         Counter will self-correct when TTL expires."
                                    );
                                } else {
                                    debug!(
                                        op = ?op,
                                        attempts = next_attempt,
                                        error = %e,
                                        "Redis retry failed, will retry again"
                                    );
                                    still_pending.push((op, next_attempt));
                                }
                            }
                        }
                    }

                    pending = still_pending;
                }
            }
        }
    })
}

fn retry_or_drop_pending_after_snapshot_error(
    pending: &mut Vec<(PendingRedisOp, u32)>,
    still_pending: &mut Vec<(PendingRedisOp, u32)>,
    max_retries: u32,
    reason: &'static str,
) {
    for (op, attempts) in pending.drain(..) {
        let next_attempt = attempts + 1;
        if next_attempt >= max_retries {
            tracing::error!(
                op = ?op,
                attempts = next_attempt,
                "ALERT: Dropping pending Redis counter operation after {reason}. \
                 Distributed connection count may be inaccurate until TTL expiry."
            );
        } else {
            still_pending.push((op, next_attempt));
        }
    }
}

async fn run_pending_retry_operation(
    redis_conn: &std::sync::Arc<dyn RedisConnectionRuntime>,
    conn: &mut redis::aio::ConnectionManager,
    op: &PendingRedisOp,
) -> redis::RedisResult<i64> {
    match op {
        PendingRedisOp::Decr(key) => tokio::time::timeout(
            redis_conn.operation_timeout(),
            conn.decr::<_, _, i64>(key, 1i64),
        )
        .await
        .unwrap_or_else(|_| {
            Err(redis::RedisError::from((
                redis::ErrorKind::Io,
                "Redis timeout: retry distributed counter decrement",
            )))
        }),
        PendingRedisOp::UnregisterCleanup {
            cleanup_key,
            total_key,
            user_key,
            room_key,
            conn_key,
            user_index_key,
            room_index_key,
            connection_id,
            registration_token,
            has_room,
        } => tokio::time::timeout(
            redis_conn.operation_timeout(),
            run_unregister_cleanup_script(
                conn,
                UnregisterCleanupScriptArgs {
                    cleanup_key,
                    total_key,
                    user_key,
                    room_key,
                    conn_key,
                    user_index_key,
                    room_index_key,
                    connection_id,
                    registration_token,
                    has_room: *has_room,
                },
            ),
        )
        .await
        .unwrap_or_else(|_| {
            Err(redis::RedisError::from((
                redis::ErrorKind::Io,
                "Redis timeout: retry unregister cleanup",
            )))
        }),
    }
}

pub(super) async fn run_unregister_cleanup_script(
    conn: &mut redis::aio::ConnectionManager,
    args: UnregisterCleanupScriptArgs<'_>,
) -> redis::RedisResult<i64> {
    UNREGISTER_CLEANUP_SCRIPT
        .key(args.cleanup_key)
        .key(args.total_key)
        .key(args.user_key)
        .key(args.room_key)
        .key(args.conn_key)
        .key(args.user_index_key)
        .key(args.room_index_key)
        .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
        .arg(args.connection_id)
        .arg(if args.has_room { "1" } else { "0" })
        .arg(args.registration_token)
        .invoke_async(conn)
        .await
}
