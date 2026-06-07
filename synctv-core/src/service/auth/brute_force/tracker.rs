use async_trait::async_trait;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use synctv_common::ExecutionControl;

use crate::{Error, RedisConnectionRuntime, Result};

static RECORD_FAILURE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local raw = redis.call('GET', KEYS[1])
        local count = 0
        if raw then
            local ok, state = pcall(cjson.decode, raw)
            if ok and state and state.count then
                count = tonumber(state.count) or 0
            else
                return redis.error_reply('invalid brute-force attempt state')
            end
        end
        count = count + 1
        local new_state = cjson.encode({count = count, last_failure_at = tonumber(ARGV[1])})
        redis.call('SET', KEYS[1], new_state, 'EX', tonumber(ARGV[2]))
        return count
        ",
    )
});

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BruteForceState {
    count: u64,
    last_failure_at: i64,
}

fn u64_to_i64(value: u64, field: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::InvalidInput(format!("{field} exceeds i64::MAX")))
}

pub(super) fn parse_redis_attempt_state(key: &str, raw: &str) -> Result<(u64, i64)> {
    if let Ok(state) = serde_json::from_str::<BruteForceState>(raw) {
        return Ok((state.count, state.last_failure_at));
    }
    tracing::error!(
        key = %key,
        raw_len = raw.len(),
        "Invalid brute-force attempt state in Redis"
    );
    Err(Error::ServiceUnavailable(
        "Brute-force protection state is invalid; please try again later".to_string(),
    ))
}

pub(super) async fn run_with_control<T, F>(
    control: Option<&ExecutionControl>,
    future: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    match control {
        Some(control) => control.run(future).await.map_err(Error::from)?,
        None => future.await,
    }
}

#[async_trait]
pub trait AttemptTracker: Send + Sync {
    async fn get_attempts(&self, key: &str) -> Result<(u64, i64)>;
    async fn record_failure(&self, key: &str, now: i64, ttl_secs: u64) -> Result<()>;
    async fn reset(&self, key: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct InMemoryAttemptTracker {
    cache: Arc<moka::future::Cache<String, (u64, i64)>>,
}

impl InMemoryAttemptTracker {
    #[must_use]
    pub fn new(max_capacity: u64, ttl_secs: u64) -> Self {
        Self {
            cache: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(max_capacity)
                    .time_to_live(Duration::from_secs(ttl_secs))
                    .build(),
            ),
        }
    }
}

#[async_trait]
impl AttemptTracker for InMemoryAttemptTracker {
    async fn get_attempts(&self, key: &str) -> Result<(u64, i64)> {
        Ok(self.cache.get(key).await.unwrap_or((0, 0)))
    }

    async fn record_failure(&self, key: &str, now: i64, _ttl_secs: u64) -> Result<()> {
        self.cache
            .entry(key.to_string())
            .and_upsert_with(|maybe_entry| {
                let new_count = match maybe_entry {
                    Some(entry) => entry.into_value().0 + 1,
                    None => 1,
                };
                std::future::ready((new_count, now))
            })
            .await;
        Ok(())
    }

    async fn reset(&self, key: &str) -> Result<()> {
        self.cache.remove(key).await;
        Ok(())
    }
}

#[derive(Clone)]
pub struct RedisAttemptTracker {
    pub(super) conn: Arc<dyn RedisConnectionRuntime>,
    fallback: Arc<moka::future::Cache<String, (u64, i64)>>,
    degraded: Arc<AtomicBool>,
    degraded_count: Arc<AtomicU64>,
    fail_closed: bool,
}

impl RedisAttemptTracker {
    pub(super) fn fail_closed_backend_error(detail: &str) -> Error {
        Error::ServiceUnavailable(format!(
            "Brute-force protection temporarily unavailable: {detail}"
        ))
    }

    #[must_use]
    pub fn new(
        conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        max_capacity: u64,
        ttl_secs: u64,
    ) -> Self {
        Self::from_runtime(crate::shared_runtime(conn), max_capacity, ttl_secs)
    }

    #[must_use]
    pub fn from_runtime(
        conn: Arc<dyn RedisConnectionRuntime>,
        max_capacity: u64,
        ttl_secs: u64,
    ) -> Self {
        Self {
            conn,
            fallback: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(max_capacity)
                    .time_to_live(Duration::from_secs(ttl_secs))
                    .build(),
            ),
            degraded: Arc::new(AtomicBool::new(false)),
            degraded_count: Arc::new(AtomicU64::new(0)),
            fail_closed: false,
        }
    }

    #[must_use]
    pub fn new_fail_closed(
        conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        max_capacity: u64,
        ttl_secs: u64,
    ) -> Self {
        Self::from_runtime_fail_closed(crate::shared_runtime(conn), max_capacity, ttl_secs)
    }

    #[must_use]
    pub fn from_runtime_fail_closed(
        conn: Arc<dyn RedisConnectionRuntime>,
        max_capacity: u64,
        ttl_secs: u64,
    ) -> Self {
        Self {
            conn,
            fallback: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(max_capacity)
                    .time_to_live(Duration::from_secs(ttl_secs))
                    .build(),
            ),
            degraded: Arc::new(AtomicBool::new(false)),
            degraded_count: Arc::new(AtomicU64::new(0)),
            fail_closed: true,
        }
    }

    async fn get_conn(
        &self,
        operation: &'static str,
        key: &str,
    ) -> Result<redis::aio::ConnectionManager> {
        match crate::redis_runtime_snapshot(&*self.conn, operation).await {
            Ok(conn) => Ok(conn),
            Err(error) => {
                if self.fail_closed {
                    Self::log_fail_closed_rejection(operation, &error.to_string(), key);
                    Err(Self::fail_closed_backend_error("please try again later"))
                } else {
                    self.mark_degraded();
                    tracing::warn!(
                        key = %key,
                        error = %error,
                        "Redis connection snapshot failed in brute-force tracker, using fallback"
                    );
                    Err(error)
                }
            }
        }
    }

    async fn fallback_attempts(&self, key: &str) -> (u64, i64) {
        self.fallback.get(key).await.unwrap_or((0, 0))
    }

    async fn record_fallback_failure(&self, key: &str, now: i64) {
        let (count, _) = self.fallback.get(key).await.unwrap_or((0, now));
        self.fallback
            .insert(key.to_string(), (count + 1, now))
            .await;
    }

    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn degraded_operation_count(&self) -> u64 {
        self.degraded_count.load(Ordering::Relaxed)
    }

    #[must_use]
    pub const fn is_fail_closed(&self) -> bool {
        self.fail_closed
    }

    fn mark_degraded(&self) {
        self.degraded.store(true, Ordering::Relaxed);
        let prev = self.degraded_count.fetch_add(1, Ordering::Relaxed);

        if prev.is_multiple_of(10) {
            tracing::warn!(
                degraded_count = prev + 1,
                "Redis degraded to fallback for brute-force tracking. \
                 In multi-replica deployments, lockout counters are not shared across replicas. \
                 Each replica maintains independent counters, reducing brute-force protection effectiveness."
            );
        }
    }

    fn log_fail_closed_rejection(operation: &'static str, error: &str, key: &str) {
        tracing::error!(
            operation = operation,
            key = %key,
            error = %error,
            "Redis unavailable in fail-closed mode: blocking login attempts for security. \
             Restore Redis availability to allow logins."
        );
    }

    fn clear_degraded(&self) {
        self.degraded.store(false, Ordering::Relaxed);
    }
}

#[async_trait]
impl AttemptTracker for RedisAttemptTracker {
    async fn get_attempts(&self, key: &str) -> Result<(u64, i64)> {
        let mut conn = match self.get_conn("get_attempts", key).await {
            Ok(conn) => conn,
            Err(error) if self.fail_closed => return Err(error),
            Err(_) => return Ok(self.fallback_attempts(key).await),
        };

        let redis_result = tokio::time::timeout(
            self.conn.operation_timeout(),
            conn.get::<_, Option<String>>(key),
        )
        .await;

        let Ok(redis_result) = redis_result else {
            if self.fail_closed {
                Self::log_fail_closed_rejection("get_attempts", "Redis timeout", key);
                return Err(Self::fail_closed_backend_error("please try again later"));
            }
            self.mark_degraded();
            tracing::warn!(key = %key, "Redis timeout in brute-force check, using fallback");
            return Ok(self.fallback.get(key).await.unwrap_or((0, 0)));
        };

        match redis_result {
            Ok(Some(raw)) => {
                self.clear_degraded();
                parse_redis_attempt_state(key, &raw)
            }
            Ok(None) => {
                self.clear_degraded();
                Ok((0, 0))
            }
            Err(error) => {
                if self.fail_closed {
                    Self::log_fail_closed_rejection("get_attempts", &error.to_string(), key);
                    return Err(Self::fail_closed_backend_error("please try again later"));
                }
                self.mark_degraded();
                tracing::warn!(key = %key, error = %error, "Redis error in brute-force check, using fallback");
                Ok(self.fallback.get(key).await.unwrap_or((0, 0)))
            }
        }
    }

    async fn record_failure(&self, key: &str, now: i64, ttl_secs: u64) -> Result<()> {
        let mut conn = match self.get_conn("record_failure", key).await {
            Ok(conn) => conn,
            Err(error) if self.fail_closed => return Err(error),
            Err(_) => {
                self.record_fallback_failure(key, now).await;
                return Ok(());
            }
        };

        let result: std::result::Result<u64, _> = tokio::time::timeout(
            self.conn.operation_timeout(),
            RECORD_FAILURE_SCRIPT
                .key(key)
                .arg(now)
                .arg(u64_to_i64(ttl_secs, "brute-force record TTL")?)
                .invoke_async(&mut conn),
        )
        .await
        .unwrap_or_else(|_| {
            Err(redis::RedisError::from((
                redis::ErrorKind::Io,
                "Redis timeout: record_failure",
            )))
        });

        match result {
            Ok(count) => {
                self.clear_degraded();
                tracing::debug!(key = %key, attempts = count, "Recorded failed attempt");
                Ok(())
            }
            Err(error) => {
                if self.fail_closed {
                    Self::log_fail_closed_rejection("record_failure", &error.to_string(), key);
                    return Err(Self::fail_closed_backend_error("please try again later"));
                }
                self.mark_degraded();
                tracing::warn!(key = %key, error = %error, "Redis error in record_failure, using fallback");
                let (count, _) = self.fallback.get(key).await.unwrap_or((0, now));
                self.fallback
                    .insert(key.to_string(), (count + 1, now))
                    .await;
                Ok(())
            }
        }
    }

    async fn reset(&self, key: &str) -> Result<()> {
        self.fallback.remove(key).await;

        let mut conn = match self.get_conn("reset", key).await {
            Ok(conn) => conn,
            Err(error) if self.fail_closed => return Err(error),
            Err(_) => return Ok(()),
        };
        match tokio::time::timeout(self.conn.operation_timeout(), conn.del::<_, ()>(key)).await {
            Ok(Ok(())) => {
                self.clear_degraded();
                Ok(())
            }
            Ok(Err(error)) => {
                if self.fail_closed {
                    Self::log_fail_closed_rejection("reset", &error.to_string(), key);
                    return Err(Self::fail_closed_backend_error("reset failed"));
                }
                self.mark_degraded();
                tracing::warn!(key = %key, error = %error, "Redis error in reset");
                Ok(())
            }
            Err(error) => {
                if self.fail_closed {
                    Self::log_fail_closed_rejection("reset", &error.to_string(), key);
                    return Err(Self::fail_closed_backend_error("reset timed out"));
                }
                self.mark_degraded();
                tracing::warn!(key = %key, error = %error, "Redis timeout in reset");
                Ok(())
            }
        }
    }
}
