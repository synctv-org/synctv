//! Rate limiting service with pluggable backends.
//!
//! # Usage
//!
//! This module lives in `synctv-core` because it serves **both** domain-level
//! and API-level rate limiting:
//!
//! - **Domain-level**: `ChatService` uses `RateLimiter` for per-user chat and
//!   danmaku throttling (business logic).
//! - **API-level**: `synctv-api` uses `RateLimiter` in gRPC interceptors,
//!   tower middleware layers, and HTTP handlers for request-level throttling.
//!
//! # Backends
//!
//! - **Redis** (`RedisRateLimitBackend`): Uses sorted-set sliding window for
//!   accurate cross-replica rate limiting. Falls back to in-memory governor
//!   on Redis errors (graceful degradation).
//! - **In-Memory** (`InMemoryRateLimitBackend`): Uses `governor` crate (GCRA
//!   algorithm). Per-instance only, not shared across replicas.
//!
//! ## Implications for Operators
//!
//! - **Single replica**: In-memory and Redis modes behave identically.
//! - **Multiple replicas with Redis**: Limits are globally accurate.
//! - **Multiple replicas without Redis / during Redis outage**: Limits are
//!   per-replica. If strict global enforcement is critical, consider lowering
//!   `max_requests` by a factor of the expected replica count, or ensuring Redis
//!   high availability.
//! - **Sync endpoints** (`check_rate_limit_sync` for gRPC interceptors) always
//!   use in-memory limiting regardless of Redis availability, since gRPC
//!   interceptors are synchronous.

use crate::Result;
use async_trait::async_trait;
use governor::clock::Clock;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter as GovernorRateLimiter};
use moka::sync::Cache as MokaCache;
use nonzero_ext::nonzero;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Type alias for the keyed rate limiter cache used by `InMemoryGovernorLimiter`.
type GovernorLimiterCache = MokaCache<(u32, u64), Arc<DefaultKeyedRateLimiter<String>>>;

/// Rate limiting error
#[derive(Error, Debug)]
pub enum RateLimitError {
    #[error("Rate limit exceeded. Try again in {retry_after_seconds}s")]
    RateLimitExceeded { retry_after_seconds: u64 },

    #[error("Redis error: {0}")]
    RedisError(#[from] redis::RedisError),
}

impl From<RateLimitError> for crate::Error {
    fn from(err: RateLimitError) -> Self {
        match err {
            RateLimitError::RateLimitExceeded {
                retry_after_seconds,
            } => Self::RateLimited(format!(
                "Rate limit exceeded. Try again in {retry_after_seconds}s"
            )),
            RateLimitError::RedisError(e) => {
                Self::Internal(format!("Rate limiter Redis error: {e}"))
            }
        }
    }
}

// ============================================================================
// In-memory governor limiter (always present for sync + fallback)
// ============================================================================

/// In-memory rate limiter backed by the `governor` crate (GCRA algorithm).
///
/// Uses a keyed rate limiter with `String` keys. Each unique key gets its own
/// independent rate limit bucket.
#[derive(Clone)]
struct InMemoryGovernorLimiter {
    limiters: Arc<GovernorLimiterCache>,
}

impl InMemoryGovernorLimiter {
    fn new() -> Self {
        let cache = MokaCache::builder()
            .max_capacity(64)
            .time_to_idle(Duration::from_mins(10))
            .build();
        Self {
            limiters: Arc::new(cache),
        }
    }

    fn get_limiter(
        &self,
        max_requests: u32,
        window_seconds: u64,
    ) -> Arc<DefaultKeyedRateLimiter<String>> {
        let key = (max_requests, window_seconds);
        if let Some(limiter) = self.limiters.get(&key) {
            return limiter;
        }

        let period = Duration::from_secs(window_seconds)
            .checked_div(max_requests)
            .unwrap_or(Duration::from_millis(1));
        let quota = Quota::with_period(period)
            .expect("non-zero period")
            .allow_burst(NonZeroU32::new(max_requests).unwrap_or(nonzero!(1u32)));

        let limiter = Arc::new(GovernorRateLimiter::keyed(quota));
        self.limiters.insert(key, Arc::clone(&limiter));
        limiter
    }

    fn check(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), u64> {
        let limiter = self.get_limiter(max_requests, window_seconds);
        match limiter.check_key(&key.to_string()) {
            Ok(()) => Ok(()),
            Err(not_until) => {
                let wait = not_until.wait_time_from(governor::clock::DefaultClock::default().now());
                let retry_after_seconds = wait.as_secs().max(1);
                Err(retry_after_seconds)
            }
        }
    }
}

/// Extract a low-cardinality tier label from a rate-limit key.
///
/// Issue #31: Prometheus metric labels must never include high-cardinality
/// values such as user IDs, IP addresses, or room IDs.
fn extract_rate_limit_tier(key: &str) -> &'static str {
    const KNOWN_TIERS: &[&str] = &[
        "auth",
        "read",
        "write",
        "media",
        "chat",
        "danmaku",
        "room_password_check",
        "grpc",
        "api",
        "refresh",
        "email",
    ];
    for segment in key.rsplit(':') {
        for &tier in KNOWN_TIERS {
            if segment == tier {
                return tier;
            }
        }
    }
    for &tier in KNOWN_TIERS {
        if key.starts_with(tier) {
            return tier;
        }
    }
    "unknown"
}

/// Get current timestamp in milliseconds
fn current_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

// ============================================================================
// RateLimitBackend trait
// ============================================================================

/// Backend for async rate limiting operations.
///
/// Implementations handle the distributed (Redis) or local (in-memory) rate
/// limiting logic. The `RateLimiter` wraps this trait and adds sync support.
#[async_trait]
pub trait RateLimitBackend: Send + Sync {
    /// Check if a request is allowed. Returns `Ok(())` if allowed.
    /// On Redis errors, implementations may fall back to in-memory.
    async fn check(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError>;

    /// Strict distributed check. Fails closed when Redis is unavailable.
    async fn check_strict(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError>;

    /// Get remaining quota: (`remaining_requests`, `reset_time_seconds`).
    async fn get_quota(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> Result<(u32, u64)>;

    /// Reset rate limit for a key.
    async fn reset(&self, key: &str) -> Result<()>;

    /// Health check. Returns `Ok(())` if the backend is healthy.
    async fn health_check(&self) -> std::result::Result<(), String>;

    /// A label for logging/debug purposes.
    fn backend_name(&self) -> &'static str;
}

// ============================================================================
// Redis implementation
// ============================================================================

/// Redis-backed rate limiter using sorted-set sliding window.
///
/// Falls back to in-memory governor on Redis errors (graceful degradation).
/// Accepts the shared `Arc<RwLock<ConnectionManager>>` to follow Sentinel failover.
pub struct RedisRateLimitBackend {
    conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
    key_prefix: String,
    /// In-memory fallback for when Redis is temporarily unavailable.
    fallback: InMemoryGovernorLimiter,
}

impl RedisRateLimitBackend {
    #[must_use]
    pub fn new(
        conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        key_prefix: String,
    ) -> Self {
        Self {
            conn,
            key_prefix,
            fallback: InMemoryGovernorLimiter::new(),
        }
    }

    /// Acquire a fresh ConnectionManager clone from the shared handle.
    async fn get_conn(&self) -> redis::aio::ConnectionManager {
        self.conn.read().await.clone()
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
        let mut conn = self.get_conn().await;
        let redis_key = format!("{}{}", self.key_prefix, key);
        let now = current_timestamp_millis();
        let window_start = now.saturating_sub(window_seconds * 1000);
        let expire_seconds = (window_seconds + 1) as i64;

        // Lua script returns both current_count and oldest_score atomically,
        // eliminating the TOCTOU window from a separate ZRANGE command.
        let script = redis::Script::new(
            r"
            redis.call('ZREMRANGEBYSCORE', KEYS[1], 0, ARGV[1])
            local seq = redis.call('INCR', KEYS[1] .. ':seq')
            local member = ARGV[2] .. ':' .. seq
            redis.call('ZADD', KEYS[1], ARGV[2], member)
            local count = redis.call('ZCARD', KEYS[1])
            redis.call('EXPIRE', KEYS[1], ARGV[3])
            redis.call('EXPIRE', KEYS[1] .. ':seq', ARGV[3])
            local oldest = 0
            if count > tonumber(ARGV[4]) then
                local entries = redis.call('ZRANGE', KEYS[1], 0, 0, 'WITHSCORES')
                if #entries >= 2 then
                    oldest = tonumber(entries[2]) or 0
                end
            end
            return {count, oldest}
            ",
        );

        let result: Vec<i64> = match script
            .key(&redis_key)
            .arg(window_start as i64)
            .arg(now)
            .arg(expire_seconds)
            .arg(max_requests)
            .invoke_async(&mut conn)
            .await
        {
            Ok(vals) => vals,
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

        let current_count = result.first().copied().unwrap_or(0) as u32;
        let oldest_score = result.get(1).copied().unwrap_or(0) as u64;

        if current_count > max_requests {
            let retry_after_seconds = if oldest_score > 0 {
                let time_since_oldest = now.saturating_sub(oldest_score);
                let remaining_window = (window_seconds * 1000).saturating_sub(time_since_oldest);
                (remaining_window / 1000).max(1)
            } else {
                1
            };

            return Err(RateLimitError::RateLimitExceeded {
                retry_after_seconds,
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
        let mut conn = self.get_conn().await;
        let redis_key = format!("{}{}", self.key_prefix, key);
        let now = current_timestamp_millis();
        let window_start = now.saturating_sub(window_seconds * 1000);
        let expire_seconds = (window_seconds + 1) as i64;

        let script = redis::Script::new(
            r"
            redis.call('ZREMRANGEBYSCORE', KEYS[1], 0, ARGV[1])
            local seq = redis.call('INCR', KEYS[1] .. ':seq')
            local member = ARGV[2] .. ':' .. seq
            redis.call('ZADD', KEYS[1], ARGV[2], member)
            local count = redis.call('ZCARD', KEYS[1])
            redis.call('EXPIRE', KEYS[1], ARGV[3])
            redis.call('EXPIRE', KEYS[1] .. ':seq', ARGV[3])
            return count
            ",
        );

        let current_count: u32 = match script
            .key(&redis_key)
            .arg(window_start as i64)
            .arg(now)
            .arg(expire_seconds)
            .invoke_async(&mut conn)
            .await
        {
            Ok(count) => count,
            Err(e) => {
                tracing::error!(
                    "Redis unreachable during distributed rate limit check, denying request (fail closed): {e}"
                );
                return Err(RateLimitError::RateLimitExceeded {
                    retry_after_seconds: 1,
                });
            }
        };

        if current_count > max_requests {
            return Err(RateLimitError::RateLimitExceeded {
                retry_after_seconds: 1,
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

        let mut conn = self.get_conn().await;
        let redis_key = format!("{}{}", self.key_prefix, key);
        let now = current_timestamp_millis();
        let window_start = now.saturating_sub(window_seconds * 1000);

        let mut pipe = redis::pipe();
        pipe.atomic()
            .zrembyscore(&redis_key, 0, window_start as i64)
            .ignore()
            .zcard(&redis_key);

        let results: Vec<u32> = pipe.query_async(&mut conn).await?;
        let current_count = results.first().copied().unwrap_or(0);
        let remaining = max_requests.saturating_sub(current_count);

        let oldest: Option<u64> = conn
            .zrange_withscores(&redis_key, 0, 0)
            .await
            .ok()
            .and_then(|entries: Vec<(String, u64)>| entries.first().map(|(_, ts)| *ts));

        let reset_seconds = if let Some(oldest_ts) = oldest {
            let time_since_oldest = now.saturating_sub(oldest_ts);
            let remaining_window = (window_seconds * 1000).saturating_sub(time_since_oldest);
            remaining_window / 1000
        } else {
            0
        };

        Ok((remaining, reset_seconds))
    }

    async fn reset(&self, key: &str) -> Result<()> {
        let full_key = format!("{}{}", self.key_prefix, key);
        let mut conn = self.get_conn().await;
        let seq_key = format!("{full_key}:seq");
        let _: () = redis::cmd("DEL")
            .arg(&full_key)
            .arg(&seq_key)
            .query_async(&mut conn)
            .await?;
        Ok(())
    }

    async fn health_check(&self) -> std::result::Result<(), String> {
        let mut conn = self.get_conn().await;
        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .map_err(|e| format!("Redis ping failed: {e}"))?;
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "redis"
    }
}

// ============================================================================
// In-memory implementation
// ============================================================================

/// In-memory rate limiter using `governor` (GCRA algorithm).
///
/// Per-instance only — not shared across replicas.
pub struct InMemoryRateLimitBackend {
    key_prefix: String,
    governor: InMemoryGovernorLimiter,
}

impl InMemoryRateLimitBackend {
    #[must_use]
    pub fn new(key_prefix: String) -> Self {
        Self {
            key_prefix,
            governor: InMemoryGovernorLimiter::new(),
        }
    }
}

#[async_trait]
impl RateLimitBackend for InMemoryRateLimitBackend {
    async fn check(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        let mem_key = format!("{}{}", self.key_prefix, key);
        self.governor
            .check(&mem_key, max_requests, window_seconds)
            .map_err(|retry_after_seconds| RateLimitError::RateLimitExceeded {
                retry_after_seconds,
            })
    }

    async fn check_strict(
        &self,
        _key: &str,
        _max_requests: u32,
        _window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        // Fail-closed: Redis not configured, deny all distributed rate limit requests
        // For security-critical operations (auth, password checks), distributed
        // coordination is required. Without Redis, we request must be denied
        // to ensure global limits are never exceeded.
        Err(RateLimitError::RateLimitExceeded {
            retry_after_seconds: 1,
        })
    }

    async fn get_quota(
        &self,
        _key: &str,
        max_requests: u32,
        _window_seconds: u64,
    ) -> Result<(u32, u64)> {
        // In-memory mode returns max_requests as a best-effort estimate
        // without consuming a token.
        Ok((max_requests, 0))
    }

    async fn reset(&self, _key: &str) -> Result<()> {
        // Governor doesn't support per-key reset, but keys auto-expire
        // based on the GCRA algorithm.
        Ok(())
    }

    async fn health_check(&self) -> std::result::Result<(), String> {
        Err("Redis not configured".to_string())
    }

    fn backend_name(&self) -> &'static str {
        "memory"
    }
}

// ============================================================================
// RateLimiter (public API)
// ============================================================================

/// Rate limiter with pluggable backend and sync fallback.
///
/// Wraps an async `RateLimitBackend` (Redis or in-memory) and always
/// maintains a local `InMemoryGovernorLimiter` for synchronous operations
/// (gRPC interceptors).
#[derive(Clone)]
pub struct RateLimiter {
    backend: Arc<dyn RateLimitBackend>,
    /// In-memory governor for sync operations (always present)
    sync_limiter: InMemoryGovernorLimiter,
    key_prefix: String,
}

impl RateLimiter {
    /// Create a new `RateLimiter` with a custom backend.
    pub fn from_backend(backend: Arc<dyn RateLimitBackend>, key_prefix: String) -> Self {
        Self {
            backend,
            sync_limiter: InMemoryGovernorLimiter::new(),
            key_prefix,
        }
    }

    /// Create a new `RateLimiter`, choosing backend based on Redis availability.
    ///
    /// Accepts the shared `Arc<RwLock<ConnectionManager>>` to follow Sentinel failover.
    pub fn new(
        redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
        key_prefix: String,
    ) -> Self {
        if let Some(conn) = redis_conn {
            let backend = Arc::new(RedisRateLimitBackend::new(conn, key_prefix.clone()));
            Self::from_backend(backend, key_prefix)
        } else {
            tracing::warn!(
                "Rate limiting using in-memory fallback (governor): Redis not configured. \
                 Limits are per-instance only (not shared across replicas)."
            );
            let backend = Arc::new(InMemoryRateLimitBackend::new(key_prefix.clone()));
            Self::from_backend(backend, key_prefix)
        }
    }

    /// Create a `RateLimiter` with in-memory fallback only (no Redis)
    #[must_use]
    pub fn in_memory_only(key_prefix: String) -> Self {
        let backend = Arc::new(InMemoryRateLimitBackend::new(key_prefix.clone()));
        Self::from_backend(backend, key_prefix)
    }

    /// Check if Redis is connected and responding
    pub async fn health_check(&self) -> std::result::Result<(), String> {
        self.backend.health_check().await
    }

    /// Synchronous rate limit check using the in-memory governor limiter.
    ///
    /// Always uses in-memory governor regardless of backend. gRPC interceptors
    /// are synchronous and cannot `await` a Redis call.
    pub fn check_rate_limit_sync(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        let mem_key = format!("{}grpc:{}", self.key_prefix, key);
        self.sync_limiter
            .check(&mem_key, max_requests, window_seconds)
            .map_err(|retry_after_seconds| RateLimitError::RateLimitExceeded {
                retry_after_seconds,
            })
    }

    /// Check if a request is allowed under the rate limit (async).
    pub async fn check_rate_limit(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        self.backend.check(key, max_requests, window_seconds).await
    }

    /// Distributed rate limit check that fails closed when Redis is unavailable.
    pub async fn check_rate_limit_distributed(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        self.backend
            .check_strict(key, max_requests, window_seconds)
            .await
    }

    /// Get remaining quota for a rate limit.
    pub async fn get_quota(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> Result<(u32, u64)> {
        self.backend
            .get_quota(key, max_requests, window_seconds)
            .await
    }

    /// Reset rate limit for a key.
    pub async fn reset(&self, key: &str) -> Result<()> {
        self.backend.reset(key).await
    }

    /// Return the backend name.
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        self.backend.backend_name()
    }
}

/// Rate limit configuration
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub chat_per_second: u32,
    pub danmaku_per_second: u32,
    pub window_seconds: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            chat_per_second: 10,
            danmaku_per_second: 3,
            window_seconds: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_without_redis() {
        let limiter = RateLimiter::new(None, "test:".to_string());
        assert_eq!(limiter.backend_name(), "memory");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_rate_limit_basic() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let conn = infra.connection_manager().await;
        let conn = Arc::new(tokio::sync::RwLock::new(conn));
        let limiter = RateLimiter::new(Some(conn), "test:".to_string());

        let key = "user:test1:chat";
        limiter.reset(key).await.unwrap();

        for i in 0..10 {
            limiter
                .check_rate_limit(key, 10, 1)
                .await
                .unwrap_or_else(|_| panic!("Request {i} should succeed"));
        }

        let result = limiter.check_rate_limit(key, 10, 1).await;
        assert!(matches!(
            result,
            Err(RateLimitError::RateLimitExceeded { .. })
        ));

        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        limiter.check_rate_limit(key, 10, 1).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_rate_limit_sliding_window() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let conn = infra.connection_manager().await;
        let conn = Arc::new(tokio::sync::RwLock::new(conn));
        let limiter = RateLimiter::new(Some(conn), "test:".to_string());

        let key = "user:test2:chat";
        limiter.reset(key).await.unwrap();

        for _ in 0..5 {
            limiter.check_rate_limit(key, 5, 1).await.unwrap();
        }
        assert!(limiter.check_rate_limit(key, 5, 1).await.is_err());

        tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
        assert!(limiter.check_rate_limit(key, 5, 1).await.is_err());

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        limiter.check_rate_limit(key, 5, 1).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_quota() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let conn = infra.connection_manager().await;
        let conn = Arc::new(tokio::sync::RwLock::new(conn));
        let limiter = RateLimiter::new(Some(conn), "test:".to_string());

        let key = "user:test3:chat";
        limiter.reset(key).await.unwrap();

        let (remaining, _) = limiter.get_quota(key, 10, 1).await.unwrap();
        assert_eq!(remaining, 10);

        for _ in 0..3 {
            limiter.check_rate_limit(key, 10, 1).await.unwrap();
        }

        let (remaining, reset_time) = limiter.get_quota(key, 10, 1).await.unwrap();
        assert_eq!(remaining, 7);
        assert!(reset_time <= 1);
    }

    #[tokio::test]
    async fn test_without_redis_uses_governor_fallback() {
        let limiter = RateLimiter::new(None, "test:".to_string());

        let key = "user:test_gov:chat";
        for i in 0..10 {
            limiter
                .check_rate_limit(key, 10, 1)
                .await
                .unwrap_or_else(|_| panic!("Governor request {i} should succeed"));
        }

        let result = limiter.check_rate_limit(key, 10, 1).await;
        assert!(
            matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
            "Governor rate limiter should enforce limits"
        );
    }

    #[tokio::test]
    async fn test_governor_independent_keys() {
        let limiter = RateLimiter::new(None, "test:".to_string());

        for _ in 0..5 {
            limiter.check_rate_limit("key1", 5, 1).await.unwrap();
        }
        assert!(limiter.check_rate_limit("key1", 5, 1).await.is_err());
        assert!(limiter.check_rate_limit("key2", 5, 1).await.is_ok());
    }

    #[tokio::test]
    async fn test_room_password_rate_limit_pattern() {
        let limiter = RateLimiter::new(None, "test:".to_string());

        let ip = "192.168.1.1";
        let room_id = "room_abc";
        let key = format!("room_password_check:{ip}:{room_id}");

        for i in 0..5 {
            limiter
                .check_rate_limit(&key, 5, 300)
                .await
                .unwrap_or_else(|_| panic!("Attempt {} should succeed", i + 1));
        }

        let result = limiter.check_rate_limit(&key, 5, 300).await;
        assert!(
            matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
            "6th attempt should be rate limited"
        );
    }

    #[tokio::test]
    async fn test_room_password_rate_limit_per_ip_isolation() {
        let limiter = RateLimiter::new(None, "test:".to_string());

        let room_id = "room_xyz";
        let key_ip1 = format!("room_password_check:10.0.0.1:{room_id}");
        let key_ip2 = format!("room_password_check:10.0.0.2:{room_id}");

        for _ in 0..5 {
            limiter.check_rate_limit(&key_ip1, 5, 300).await.unwrap();
        }
        assert!(limiter.check_rate_limit(&key_ip1, 5, 300).await.is_err());
        assert!(limiter.check_rate_limit(&key_ip2, 5, 300).await.is_ok());
    }

    #[tokio::test]
    async fn test_room_password_rate_limit_per_room_isolation() {
        let limiter = RateLimiter::new(None, "test:".to_string());

        let ip = "10.0.0.1";
        let key_room1 = format!("room_password_check:{ip}:room_1");
        let key_room2 = format!("room_password_check:{ip}:room_2");

        for _ in 0..5 {
            limiter.check_rate_limit(&key_room1, 5, 300).await.unwrap();
        }
        assert!(limiter.check_rate_limit(&key_room1, 5, 300).await.is_err());
        assert!(limiter.check_rate_limit(&key_room2, 5, 300).await.is_ok());
    }

    #[tokio::test]
    async fn test_concurrent_burst_all_within_limit() {
        let limiter = RateLimiter::in_memory_only("burst_test:".to_string());

        let mut handles = Vec::new();
        for _ in 0..10 {
            let limiter = limiter.clone();
            handles.push(tokio::spawn(async move {
                limiter.check_rate_limit("burst_key", 10, 1).await
            }));
        }

        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        let successes = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(
            successes, 10,
            "All 10 concurrent requests within limit should succeed"
        );
    }

    #[tokio::test]
    async fn test_concurrent_burst_exceeding_limit() {
        let limiter = RateLimiter::in_memory_only("burst_over:".to_string());

        let mut handles = Vec::new();
        for _ in 0..20 {
            let limiter = limiter.clone();
            handles.push(tokio::spawn(async move {
                limiter.check_rate_limit("burst_over_key", 5, 1).await
            }));
        }

        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        let successes = results.iter().filter(|r| r.is_ok()).count();
        let failures = results.iter().filter(|r| r.is_err()).count();

        assert_eq!(successes, 5, "Only 5 concurrent requests should succeed");
        assert_eq!(failures, 15, "15 requests should be rate limited");
    }

    #[test]
    fn test_check_rate_limit_sync_allows_within_limit() {
        let limiter = RateLimiter::in_memory_only("sync_test:".to_string());
        assert!(limiter.check_rate_limit_sync("sync_key", 5, 1).is_ok());
    }

    #[test]
    fn test_check_rate_limit_sync_blocks_over_limit() {
        let limiter = RateLimiter::in_memory_only("sync_block:".to_string());

        for _ in 0..5 {
            limiter.check_rate_limit_sync("sync_key", 5, 1).unwrap();
        }

        let result = limiter.check_rate_limit_sync("sync_key", 5, 1);
        assert!(matches!(
            result,
            Err(RateLimitError::RateLimitExceeded { .. })
        ));
    }

    #[test]
    fn test_check_rate_limit_sync_uses_grpc_key_prefix() {
        let limiter = RateLimiter::in_memory_only("myprefix:".to_string());

        for _ in 0..3 {
            limiter.check_rate_limit_sync("key1", 3, 1).unwrap();
        }
        assert!(limiter.check_rate_limit_sync("key1", 3, 1).is_err());
    }

    /// Test that InMemoryRateLimitBackend::check_strict fails closed when Redis is not configured.
    /// For security-critical operations, all requests should be denied when Redis is unavailable
    /// to ensure global limits are never exceeded.
    #[tokio::test]
    async fn test_in_memory_check_strict_fails_closed() {
        let limiter = RateLimiter::in_memory_only("strict_test:".to_string());

        // Should deny ALL requests when Redis is not configured (fail-closed)
        let result = limiter
            .check_rate_limit_distributed("strict_key", 5, 1)
            .await;
        assert!(
            matches!(
                result,
                Err(RateLimitError::RateLimitExceeded {
                    retry_after_seconds: 1
                })
            ),
            "check_strict should deny all requests when Redis is not configured (fail-closed)"
        );
    }

    /// Test that check_strict with in-memory backend denies all requests regardless of key
    #[tokio::test]
    async fn test_in_memory_check_strict_denies_all_keys() {
        let limiter = RateLimiter::in_memory_only("strict_keys:".to_string());

        // key1 should be denied (fail-closed)
        let result1 = limiter.check_rate_limit_distributed("key1", 5, 1).await;
        assert!(
            matches!(
                result1,
                Err(RateLimitError::RateLimitExceeded {
                    retry_after_seconds: 1
                })
            ),
            "check_strict should deny key1 when Redis is not configured"
        );

        // key2 should also be denied (fail-closed)
        let result2 = limiter.check_rate_limit_distributed("key2", 5, 1).await;
        assert!(
            matches!(
                result2,
                Err(RateLimitError::RateLimitExceeded {
                    retry_after_seconds: 1
                })
            ),
            "check_strict should deny key2 when Redis is not configured"
        );
    }

    #[test]
    fn test_rate_limit_config_default_values() {
        let config = RateLimitConfig::default();
        assert_eq!(config.chat_per_second, 10);
        assert_eq!(config.danmaku_per_second, 3);
        assert_eq!(config.window_seconds, 1);
    }

    #[test]
    fn test_rate_limit_config_clone() {
        let config = RateLimitConfig::default();
        let cloned = config.clone();
        assert_eq!(cloned.chat_per_second, config.chat_per_second);
        assert_eq!(cloned.danmaku_per_second, config.danmaku_per_second);
    }

    #[test]
    fn test_rate_limit_error_to_core_error_exceeded() {
        let err = RateLimitError::RateLimitExceeded {
            retry_after_seconds: 30,
        };
        let core_err: crate::Error = err.into();
        match core_err {
            crate::Error::RateLimited(msg) => {
                assert!(msg.contains("30"));
            }
            other => panic!("Expected RateLimited, got: {other:?}"),
        }
    }

    #[test]
    fn test_rate_limit_error_to_core_error_redis() {
        let redis_err = redis::RedisError::from((redis::ErrorKind::Io, "connection refused"));
        let err = RateLimitError::RedisError(redis_err);
        let core_err: crate::Error = err.into();
        match core_err {
            crate::Error::Internal(msg) => {
                assert!(msg.contains("Rate limiter Redis error"));
            }
            other => panic!("Expected Internal, got: {other:?}"),
        }
    }

    #[test]
    fn test_rate_limit_error_display() {
        let err = RateLimitError::RateLimitExceeded {
            retry_after_seconds: 5,
        };
        let display = format!("{err}");
        assert!(display.contains("5s"));
    }

    #[tokio::test]
    async fn test_get_quota_without_redis_returns_max() {
        let limiter = RateLimiter::in_memory_only("quota_test:".to_string());

        let (remaining, reset) = limiter.get_quota("key", 10, 1).await.unwrap();
        assert_eq!(remaining, 10);
        assert_eq!(reset, 0);
    }

    #[tokio::test]
    async fn test_get_quota_without_redis_does_not_consume_token() {
        let limiter = RateLimiter::in_memory_only("quota_no_consume:".to_string());

        for _ in 0..20 {
            let (remaining, _) = limiter.get_quota("key", 10, 1).await.unwrap();
            assert_eq!(remaining, 10);
        }

        for i in 0..10 {
            limiter
                .check_rate_limit("key", 10, 1)
                .await
                .unwrap_or_else(|_| panic!("Request {i} should succeed after get_quota calls"));
        }
    }

    #[tokio::test]
    async fn test_health_check_without_redis() {
        let limiter = RateLimiter::in_memory_only("health:".to_string());
        let result = limiter.health_check().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not configured"));
    }

    #[tokio::test]
    async fn test_in_memory_different_quotas_are_independent() {
        let limiter = RateLimiter::in_memory_only("quotas:".to_string());

        for _ in 0..5 {
            limiter.check_rate_limit("same_key", 5, 1).await.unwrap();
        }
        assert!(limiter.check_rate_limit("same_key", 5, 1).await.is_err());
        assert!(limiter.check_rate_limit("same_key", 10, 1).await.is_ok());
    }

    #[tokio::test]
    async fn test_redis_failure_falls_back_to_in_memory() {
        let client = redis::Client::open("redis://127.0.0.1:1").unwrap();
        let conn = redis::aio::ConnectionManager::new(client).await;

        if conn.is_err() {
            let limiter = RateLimiter::in_memory_only("fallback_test:".to_string());
            for i in 0..5 {
                limiter
                    .check_rate_limit("fb_key", 5, 1)
                    .await
                    .unwrap_or_else(|_| panic!("Request {i} should succeed"));
            }
            assert!(
                matches!(
                    limiter.check_rate_limit("fb_key", 5, 1).await,
                    Err(RateLimitError::RateLimitExceeded { .. })
                ),
                "Should be rate limited after exhausting quota"
            );
            return;
        }

        let conn = Arc::new(tokio::sync::RwLock::new(conn.unwrap()));
        let limiter = RateLimiter::new(Some(conn), "fallback_test:".to_string());

        let result = limiter.check_rate_limit("test_key", 10, 1).await;

        match &result {
            Ok(()) => {}
            Err(RateLimitError::RateLimitExceeded { .. }) => {}
            Err(RateLimitError::RedisError(e)) => {
                panic!(
                    "check_rate_limit should NOT propagate RedisError; expected fallback to in-memory. Got: {e}"
                );
            }
        }
    }
}
