use async_trait::async_trait;
use std::sync::{Arc, LazyLock};

use crate::{RedisConnectionRuntime, Result};
use synctv_common::ExecutionControl;

use super::{
    extract_rate_limit_tier, millis_to_i64, parse_quota_count_result, parse_sliding_window_result,
    retry_after_seconds_from_oldest, timestamp_millis, window_expire_seconds, window_millis,
    InMemoryGovernorLimiter, RateLimitBackend, RateLimitError,
};

static REDIS_SLIDING_WINDOW_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        redis.call('ZREMRANGEBYSCORE', KEYS[1], 0, ARGV[1])
        local count = redis.call('ZCARD', KEYS[1])
        local oldest = 0
        if count >= tonumber(ARGV[4]) then
            local entries = redis.call('ZRANGE', KEYS[1], 0, 0, 'WITHSCORES')
            if #entries >= 2 then
                oldest = tonumber(entries[2]) or 0
            end
            return {count + 1, oldest}
        end
        local seq = redis.call('INCR', KEYS[1] .. ':seq')
        local member = ARGV[2] .. ':' .. seq
        redis.call('ZADD', KEYS[1], ARGV[2], member)
        count = count + 1
        redis.call('EXPIRE', KEYS[1], ARGV[3])
        redis.call('EXPIRE', KEYS[1] .. ':seq', ARGV[3])
        return {count, oldest}
        ",
    )
});

/// Redis-backed rate limiter using sorted-set sliding window.
///
/// Falls back to in-memory governor on Redis errors (graceful degradation).
/// Accepts the shared `Arc<RwLock<ConnectionManager>>` to follow Sentinel failover.
pub(super) struct RedisRateLimitBackend {
    pub(super) conn: Arc<dyn RedisConnectionRuntime>,
    key_prefix: String,
    /// In-memory fallback for when Redis is temporarily unavailable.
    fallback: InMemoryGovernorLimiter,
}

impl RedisRateLimitBackend {
    #[must_use]
    pub(super) fn from_runtime(conn: Arc<dyn RedisConnectionRuntime>, key_prefix: String) -> Self {
        Self {
            conn,
            key_prefix,
            fallback: InMemoryGovernorLimiter::new(),
        }
    }

    async fn with_redis_conn<T, F, Fut>(
        &self,
        operation: &'static str,
        f: F,
    ) -> std::result::Result<T, RateLimitError>
    where
        F: FnOnce(redis::aio::ConnectionManager) -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, RateLimitError>>,
    {
        match tokio::time::timeout(self.conn.operation_timeout(), async {
            let conn = self.conn.snapshot().await.map_err(|error| {
                RateLimitError::BackendUnavailable(format!(
                    "Redis rate limiter {operation} connection failed: {error}"
                ))
            })?;
            f(conn).await
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(RateLimitError::BackendUnavailable(format!(
                "Redis rate limiter {operation} timed out after {}ms",
                self.conn.operation_timeout().as_millis()
            ))),
        }
    }

    async fn run_with_control<T, F>(
        control: Option<&ExecutionControl>,
        operation: F,
    ) -> std::result::Result<T, RateLimitError>
    where
        F: std::future::Future<Output = std::result::Result<T, RateLimitError>>,
    {
        match control {
            Some(control) => control.run(operation).await?,
            None => operation.await,
        }
    }
}

#[async_trait]
impl RateLimitBackend for RedisRateLimitBackend {
    async fn check(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        self.check_with_control(key, max_requests, window_seconds, None)
            .await
    }

    async fn check_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        let redis_key = format!("{}{}", self.key_prefix, key);
        let now = timestamp_millis()?;
        let window_ms = window_millis(window_seconds)?;
        let window_start = now.saturating_sub(window_ms);
        let expire_seconds = window_expire_seconds(window_seconds)?;
        let window_start = millis_to_i64(window_start)?;
        let now_arg = millis_to_i64(now)?;

        let result: Vec<i64> = match Self::run_with_control(control, async {
            self.with_redis_conn("sliding-window check", |mut conn| async move {
                REDIS_SLIDING_WINDOW_SCRIPT
                    .key(&redis_key)
                    .arg(window_start)
                    .arg(now_arg)
                    .arg(expire_seconds)
                    .arg(max_requests)
                    .invoke_async(&mut conn)
                    .await
                    .map_err(RateLimitError::from)
            })
            .await
        })
        .await
        {
            Ok(vals) => vals,
            Err(RateLimitError::Control(error)) => return Err(RateLimitError::Control(error)),
            Err(e) => {
                tracing::warn!(
                    "Redis rate limiter unavailable, falling back to in-memory: {}",
                    e
                );
                let tier_label = extract_rate_limit_tier(key);
                crate::metrics::rate_limit::RATE_LIMIT_REDIS_FALLBACKS_TOTAL
                    .with_label_values(&[tier_label])
                    .inc();
                let mem_key = format!("{}{}", self.key_prefix, key);
                return self
                    .fallback
                    .check(&mem_key, max_requests, window_seconds)
                    .map_err(|retry_after_seconds| RateLimitError::RateLimitExceeded {
                        retry_after_seconds,
                    });
            }
        };

        let (current_count, oldest_score) = match parse_sliding_window_result(&result) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Redis rate limiter returned malformed script output, falling back to in-memory"
                );
                let tier_label = extract_rate_limit_tier(key);
                crate::metrics::rate_limit::RATE_LIMIT_REDIS_FALLBACKS_TOTAL
                    .with_label_values(&[tier_label])
                    .inc();
                let mem_key = format!("{}{}", self.key_prefix, key);
                return self
                    .fallback
                    .check(&mem_key, max_requests, window_seconds)
                    .map_err(|retry_after_seconds| RateLimitError::RateLimitExceeded {
                        retry_after_seconds,
                    });
            }
        };

        if current_count > max_requests {
            return Err(RateLimitError::RateLimitExceeded {
                retry_after_seconds: retry_after_seconds_from_oldest(
                    now,
                    oldest_score,
                    window_seconds,
                ),
            });
        }

        Ok(())
    }

    async fn check_strict(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        self.check_strict_with_control(key, max_requests, window_seconds, None)
            .await
    }

    async fn check_strict_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        let redis_key = format!("{}{}", self.key_prefix, key);
        let now = timestamp_millis()?;
        let window_ms = window_millis(window_seconds)?;
        let window_start = now.saturating_sub(window_ms);
        let expire_seconds = window_expire_seconds(window_seconds)?;
        let window_start = millis_to_i64(window_start)?;
        let now_arg = millis_to_i64(now)?;

        let result: Vec<i64> = match Self::run_with_control(control, async {
            self.with_redis_conn("strict sliding-window check", |mut conn| async move {
                REDIS_SLIDING_WINDOW_SCRIPT
                    .key(&redis_key)
                    .arg(window_start)
                    .arg(now_arg)
                    .arg(expire_seconds)
                    .arg(max_requests)
                    .invoke_async(&mut conn)
                    .await
                    .map_err(RateLimitError::from)
            })
            .await
        })
        .await
        {
            Ok(count) => count,
            Err(RateLimitError::Control(error)) => return Err(RateLimitError::Control(error)),
            Err(e) => {
                tracing::error!(
                    "Redis unreachable during distributed rate limit check, denying request (fail closed): {e}"
                );
                return Err(RateLimitError::BackendUnavailable(
                    "Distributed rate limit backend unavailable".to_string(),
                ));
            }
        };
        let (current_count, oldest_score) = parse_sliding_window_result(&result)?;

        if current_count > max_requests {
            return Err(RateLimitError::RateLimitExceeded {
                retry_after_seconds: retry_after_seconds_from_oldest(
                    now,
                    oldest_score,
                    window_seconds,
                ),
            });
        }

        Ok(())
    }

    async fn get_quota(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> Result<(u32, u64)> {
        use redis::AsyncCommands;

        let redis_key = format!("{}{}", self.key_prefix, key);
        let now = timestamp_millis()?;
        let window_ms = window_millis(window_seconds)?;
        let window_start = now.saturating_sub(window_ms);
        let window_start = millis_to_i64(window_start)?;

        let results: Vec<u32> = self
            .with_redis_conn("quota query", |mut conn| {
                let redis_key = redis_key.clone();
                async move {
                    let mut pipe = redis::pipe();
                    pipe.atomic()
                        .zrembyscore(&redis_key, 0, window_start)
                        .ignore()
                        .zcard(&redis_key);

                    pipe.query_async(&mut conn)
                        .await
                        .map_err(RateLimitError::from)
                }
            })
            .await?;
        let current_count = parse_quota_count_result(&results)?;
        let remaining = max_requests.saturating_sub(current_count);

        let oldest: Option<u64> = self
            .with_redis_conn("quota oldest-score query", |mut conn| {
                let redis_key = redis_key.clone();
                async move {
                    let entries: Vec<(String, u64)> = conn
                        .zrange_withscores(&redis_key, 0, 0)
                        .await
                        .map_err(RateLimitError::from)?;
                    Ok(entries.first().map(|(_, ts)| *ts))
                }
            })
            .await?;

        let reset_seconds = if let Some(oldest_ts) = oldest {
            let time_since_oldest = now.saturating_sub(oldest_ts);
            let remaining_window = window_ms.saturating_sub(time_since_oldest);
            let reset_seconds = remaining_window.div_ceil(1000);
            if remaining == 0 {
                reset_seconds.max(1)
            } else {
                reset_seconds
            }
        } else {
            0
        };

        Ok((remaining, reset_seconds))
    }

    async fn reset(&self, key: &str) -> Result<()> {
        let full_key = format!("{}{}", self.key_prefix, key);
        let seq_key = format!("{full_key}:seq");
        let _: () = self
            .with_redis_conn("reset", |mut conn| async move {
                redis::cmd("DEL")
                    .arg(&full_key)
                    .arg(&seq_key)
                    .query_async(&mut conn)
                    .await
                    .map_err(RateLimitError::from)
            })
            .await?;
        Ok(())
    }

    async fn health_check(&self) -> std::result::Result<(), String> {
        self.with_redis_conn("health check", |mut conn| async move {
            redis::cmd("PING")
                .query_async::<String>(&mut conn)
                .await
                .map(|_| ())
                .map_err(RateLimitError::from)
        })
        .await
        .map_err(|e| format!("Redis ping failed: {e}"))
    }
}
