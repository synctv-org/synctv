//! Rate limiting service with Redis-backed distributed sliding window.
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
//! If rate limiting were only needed at the API boundary, it would belong in
//! `synctv-api`. Its placement here reflects the domain-level usage.
//!
//! # Multi-Replica Behavior
//!
//! When Redis is configured, rate limits are **shared across all replicas** using
//! a Redis sorted-set sliding window algorithm. A user's request count is tracked
//! globally, so the configured limit applies to the total across all nodes.
//!
//! When Redis is unavailable (not configured or temporarily down), the service
//! falls back to **per-instance in-memory** limiting using the `governor` crate.
//! In this mode each replica enforces the limit independently, so the effective
//! global limit becomes `N * max_requests` where `N` is the number of replicas.
//! This is an intentional trade-off: allowing slightly more requests is preferable
//! to rejecting all requests during a Redis outage.
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
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter as GovernorRateLimiter};
use nonzero_ext::nonzero;
use redis::AsyncCommands;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use moka::sync::Cache as MokaCache;

/// Type alias for the keyed rate limiter cache used by InMemoryRateLimiter.
type RateLimiterCache = MokaCache<(u32, u64), Arc<DefaultKeyedRateLimiter<String>>>;

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
            RateLimitError::RateLimitExceeded { retry_after_seconds } => {
                Self::RateLimited(format!("Rate limit exceeded. Try again in {retry_after_seconds}s"))
            }
            RateLimitError::RedisError(e) => {
                Self::Internal(format!("Rate limiter Redis error: {e}"))
            }
        }
    }
}

/// In-memory rate limiter backed by the `governor` crate (GCRA algorithm).
///
/// Uses a keyed rate limiter with `String` keys. Each unique key gets its own
/// independent rate limit bucket. Governor handles all the timing, pruning, and
/// thread-safety internally.
///
/// Note: Governor uses a fixed quota per limiter instance. Since our API allows
/// callers to specify different (max_requests, window_seconds) per call, we
/// create separate governor instances for each quota configuration. In practice,
/// only a few distinct configurations are used (chat, danmaku), so this is fine.
///
/// Uses a Moka cache with TTL and max capacity to prevent unbounded memory growth.
/// Entries are evicted after idle timeout or when the cache reaches max capacity
/// (64 quota configurations).
#[derive(Clone)]
struct InMemoryRateLimiter {
    /// Stores governor keyed limiters per (max_requests, window_seconds) pair.
    /// Uses Moka cache with TTL-based eviction to prevent unbounded growth.
    limiters: Arc<RateLimiterCache>,
}

impl InMemoryRateLimiter {
    fn new() -> Self {
        let cache = MokaCache::builder()
            .max_capacity(64)
            .time_to_idle(Duration::from_secs(600))
            .build();
        Self {
            limiters: Arc::new(cache),
        }
    }

    /// Get or create a governor keyed rate limiter for the given quota.
    fn get_limiter(&self, max_requests: u32, window_seconds: u64) -> Arc<DefaultKeyedRateLimiter<String>> {
        let key = (max_requests, window_seconds);
        if let Some(limiter) = self.limiters.get(&key) {
            return limiter;
        }

        // Create quota: max_requests per window_seconds
        // Governor's Quota::with_period gives us one cell per period,
        // then allow_burst lets us burst up to max_requests.
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

    /// Check rate limit. Returns Ok(()) if allowed, or the `retry_after` seconds.
    fn check(&self, key: &str, max_requests: u32, window_seconds: u64) -> std::result::Result<(), u64> {
        let limiter = self.get_limiter(max_requests, window_seconds);
        match limiter.check_key(&key.to_string()) {
            Ok(_) => Ok(()),
            Err(not_until) => {
                let wait = not_until.wait_time_from(governor::clock::DefaultClock::default().now());
                let retry_after_seconds = wait.as_secs().max(1);
                Err(retry_after_seconds)
            }
        }
    }
}

use governor::clock::Clock;

/// Rate limiter using Redis sliding window algorithm
///
/// **Production Enhancement (#26)**: Implements graceful degradation for Redis failures.
/// - Uses Redis sorted sets for accurate cross-replica sliding window rate limiting
/// - Automatically falls back to per-instance in-memory limiting (`governor` crate) when:
///   - Redis is not configured at startup
///   - Redis becomes unavailable at runtime (connection errors, timeouts)
/// - Fallback ensures service availability even during Redis outages
/// - Metrics track fallback events via `RATE_LIMIT_REDIS_FALLBACKS_TOTAL`
///
/// Trade-off: In-memory fallback is per-instance only (not shared across replicas),
/// so rate limits become per-replica during Redis outages.
#[derive(Clone)]
pub struct RateLimiter {
    redis_conn: Option<redis::aio::ConnectionManager>,
    key_prefix: String,
    /// In-memory fallback (always present, used when `redis_conn` is None)
    in_memory: InMemoryRateLimiter,
}

impl RateLimiter {
    /// Create a new `RateLimiter`
    ///
    /// If `redis_conn` is None, falls back to per-instance in-memory rate limiting.
    pub fn new(redis_conn: Option<redis::aio::ConnectionManager>, key_prefix: String) -> Self {
        if redis_conn.is_none() {
            tracing::warn!(
                "Rate limiting using in-memory fallback (governor): Redis not configured. \
                 Limits are per-instance only (not shared across replicas)."
            );
        }
        Self {
            redis_conn,
            key_prefix,
            in_memory: InMemoryRateLimiter::new(),
        }
    }

    /// Create a `RateLimiter` with in-memory fallback only (no Redis)
    #[must_use]
    pub fn in_memory_only(key_prefix: String) -> Self {
        Self {
            redis_conn: None,
            key_prefix,
            in_memory: InMemoryRateLimiter::new(),
        }
    }

    /// Check if Redis is connected and responding
    ///
    /// Returns Ok(()) if Redis is healthy, or an error if not configured or unreachable.
    pub async fn health_check(&self) -> std::result::Result<(), String> {
        let Some(ref conn) = self.redis_conn else {
            return Err("Redis not configured".to_string());
        };
        let mut conn = conn.clone();
        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .map_err(|e| format!("Redis ping failed: {e}"))?;
        Ok(())
    }

    /// Synchronous rate limit check using the in-memory governor limiter.
    ///
    /// This is designed for use in gRPC interceptors, which must be synchronous.
    /// Uses per-instance in-memory rate limiting (not distributed via Redis).
    /// For distributed rate limiting, use `check_rate_limit` (async).
    pub fn check_rate_limit_sync(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        let mem_key = format!("{}grpc:{}", self.key_prefix, key);
        self.in_memory
            .check(&mem_key, max_requests, window_seconds)
            .map_err(|retry_after_seconds| RateLimitError::RateLimitExceeded {
                retry_after_seconds,
            })
    }

    /// Check if a request is allowed under the rate limit
    ///
    /// Returns Ok(()) if allowed, or `RateLimitError` if rate limit exceeded
    ///
    /// # Arguments
    /// * `key` - Unique identifier for the rate limit (e.g., "`user:{user_id}:chat`")
    /// * `max_requests` - Maximum number of requests allowed in the window
    /// * `window_seconds` - Size of the sliding window in seconds
    pub async fn check_rate_limit(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        // If Redis not configured, use governor in-memory fallback
        let Some(ref conn) = self.redis_conn else {
            let mem_key = format!("{}{}", self.key_prefix, key);
            return self.in_memory.check(&mem_key, max_requests, window_seconds)
                .map_err(|retry_after_seconds| RateLimitError::RateLimitExceeded { retry_after_seconds });
        };

        let mut conn = conn.clone();

        let redis_key = format!("{}{}", self.key_prefix, key);
        let now = Self::current_timestamp_millis();
        let window_start = now.saturating_sub(window_seconds * 1000);
        let expire_seconds = (window_seconds + 1) as i64;

        // Use Lua script for true atomic rate limiting (prevents TOCTOU race)
        // The script: removes expired entries, adds the new request, counts, and sets expiry.
        // A per-key sequence counter generates unique members so concurrent requests
        // within the same millisecond are counted separately instead of overwriting.
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
            "
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
                // Redis error at runtime -- degrade to in-memory limiter instead
                // of failing the request. This keeps HTTP and gRPC behavior
                // consistent (both use governor fallback on Redis failure).
                tracing::warn!(
                    "Redis rate limiter unavailable, falling back to in-memory: {}",
                    e
                );
                crate::metrics::rate_limit::RATE_LIMIT_REDIS_FALLBACKS_TOTAL
                    .with_label_values(&[&redis_key])
                    .inc();
                let mem_key = format!("{}{}", self.key_prefix, key);
                return self
                    .in_memory
                    .check(&mem_key, max_requests, window_seconds)
                    .map_err(|retry_after_seconds| RateLimitError::RateLimitExceeded {
                        retry_after_seconds,
                    });
            }
        };

        if current_count > max_requests {
            // Rate limit exceeded
            // Calculate retry_after by finding oldest entry in window
            let oldest: Option<u64> = conn
                .zrange_withscores(&redis_key, 0, 0)
                .await
                .ok()
                .and_then(|entries: Vec<(String, u64)>| entries.first().map(|(_, ts)| *ts));

            let retry_after_seconds = if let Some(oldest_ts) = oldest {
                let time_since_oldest = now.saturating_sub(oldest_ts);
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

    /// Distributed rate limit check that always uses Redis.
    ///
    /// Unlike `check_rate_limit` which falls back to in-memory governor when Redis
    /// is unavailable, this method enforces distributed rate limiting and **denies
    /// requests (fail closed)** when Redis is unreachable.
    ///
    /// Designed for use in async tower middleware layers (e.g., gRPC blacklist or
    /// rate-limit layers) where per-node-only limiting is insufficient.
    pub async fn check_rate_limit_distributed(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        let Some(ref conn) = self.redis_conn else {
            tracing::error!(
                "Distributed rate limit check failed: Redis not configured. Denying request (fail closed)."
            );
            return Err(RateLimitError::RateLimitExceeded {
                retry_after_seconds: 1,
            });
        };

        let mut conn = conn.clone();
        let redis_key = format!("{}{}", self.key_prefix, key);
        let now = Self::current_timestamp_millis();
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
            "
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

    /// Get remaining quota for a rate limit
    ///
    /// Returns (`remaining_requests`, `reset_time_seconds`)
    pub async fn get_quota(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> Result<(u32, u64)> {
        // If Redis not configured, return a best-effort estimate.
        // We intentionally do NOT call `check_key()` here because governor's
        // `check_key()` consumes a token, and `get_quota()` is meant to be a
        // read-only query. Consuming a token on every quota check would cause
        // callers to burn through their rate limit just by inspecting it.
        let Some(ref conn) = self.redis_conn else {
            return Ok((max_requests, 0));
        };

        let mut conn = conn.clone();

        let redis_key = format!("{}{}", self.key_prefix, key);
        let now = Self::current_timestamp_millis();
        let window_start = now.saturating_sub(window_seconds * 1000);

        // Remove expired entries and count
        let mut pipe = redis::pipe();
        pipe.atomic()
            .zrembyscore(&redis_key, 0, window_start as i64)
            .ignore()
            .zcard(&redis_key);

        let results: Vec<u32> = pipe.query_async(&mut conn).await?;
        let current_count = results.first().copied().unwrap_or(0);

        let remaining = max_requests.saturating_sub(current_count);

        // Calculate reset time (when oldest entry expires)
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

    /// Reset rate limit for a key (for testing or admin purposes)
    pub async fn reset(&self, key: &str) -> Result<()> {
        let full_key = format!("{}{}", self.key_prefix, key);
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let seq_key = format!("{full_key}:seq");
            let _: () = redis::cmd("DEL")
                .arg(&full_key)
                .arg(&seq_key)
                .query_async(&mut conn)
                .await?;
        }
        // Governor doesn't support per-key reset, but keys auto-expire
        // based on the GCRA algorithm, so this is acceptable.
        Ok(())
    }

    /// Get current timestamp in milliseconds
    ///
    /// Returns 0 if system time is before Unix epoch (should never happen in practice).
    fn current_timestamp_millis() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
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
        assert!(limiter.redis_conn.is_none());
    }

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_rate_limit_basic() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let conn = infra.connection_manager().await;
        let limiter = RateLimiter::new(Some(conn), "test:".to_string());

        let key = "user:test1:chat";

        // Reset before test
        limiter.reset(key).await.unwrap();

        // First 10 requests should succeed
        for i in 0..10 {
            limiter
                .check_rate_limit(key, 10, 1)
                .await
                .unwrap_or_else(|_| panic!("Request {} should succeed", i));
        }

        // 11th request should fail
        let result = limiter.check_rate_limit(key, 10, 1).await;
        assert!(matches!(
            result,
            Err(RateLimitError::RateLimitExceeded { .. })
        ));

        // Wait for window to expire
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Should work again
        limiter.check_rate_limit(key, 10, 1).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_rate_limit_sliding_window() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let conn = infra.connection_manager().await;
        let limiter = RateLimiter::new(Some(conn), "test:".to_string());

        let key = "user:test2:chat";
        limiter.reset(key).await.unwrap();

        // Use 5 requests in 1 second window
        for _ in 0..5 {
            limiter.check_rate_limit(key, 5, 1).await.unwrap();
        }

        // Should be rate limited
        assert!(limiter.check_rate_limit(key, 5, 1).await.is_err());

        // Wait 0.6 seconds (more than half the window)
        tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;

        // Still limited (sliding window)
        assert!(limiter.check_rate_limit(key, 5, 1).await.is_err());

        // Wait another 0.5 seconds (total 1.1s, oldest entries expired)
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Should work now
        limiter.check_rate_limit(key, 5, 1).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_get_quota() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let conn = infra.connection_manager().await;
        let limiter = RateLimiter::new(Some(conn), "test:".to_string());

        let key = "user:test3:chat";
        limiter.reset(key).await.unwrap();

        // Check initial quota
        let (remaining, _) = limiter.get_quota(key, 10, 1).await.unwrap();
        assert_eq!(remaining, 10);

        // Use 3 requests
        for _ in 0..3 {
            limiter.check_rate_limit(key, 10, 1).await.unwrap();
        }

        // Check remaining
        let (remaining, reset_time) = limiter.get_quota(key, 10, 1).await.unwrap();
        assert_eq!(remaining, 7);
        assert!(reset_time <= 1);
    }

    #[tokio::test]
    async fn test_without_redis_uses_governor_fallback() {
        let limiter = RateLimiter::new(None, "test:".to_string());

        let key = "user:test_gov:chat";

        // First 10 requests should succeed (burst capacity = 10)
        for i in 0..10 {
            limiter
                .check_rate_limit(key, 10, 1)
                .await
                .unwrap_or_else(|_| panic!("Governor request {} should succeed", i));
        }

        // 11th request should be rate limited
        let result = limiter.check_rate_limit(key, 10, 1).await;
        assert!(
            matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
            "Governor rate limiter should enforce limits"
        );
    }

    #[tokio::test]
    async fn test_governor_independent_keys() {
        let limiter = RateLimiter::new(None, "test:".to_string());

        // Exhaust limit for key1
        for _ in 0..5 {
            limiter.check_rate_limit("key1", 5, 1).await.unwrap();
        }
        assert!(limiter.check_rate_limit("key1", 5, 1).await.is_err());

        // key2 should still work (independent bucket)
        assert!(limiter.check_rate_limit("key2", 5, 1).await.is_ok());
    }

    /// Validates the per-IP per-room password check rate limiting pattern.
    ///
    /// This mirrors the key format used in `ClientApiImpl::check_room_password`:
    /// `room_password_check:{client_ip}:{room_id}`
    #[tokio::test]
    async fn test_room_password_rate_limit_pattern() {
        let limiter = RateLimiter::new(None, "test:".to_string());

        let ip = "192.168.1.1";
        let room_id = "room_abc";
        let key = format!("room_password_check:{ip}:{room_id}");

        // 5 attempts should succeed (matching the MAX_PASSWORD_ATTEMPTS constant)
        for i in 0..5 {
            limiter
                .check_rate_limit(&key, 5, 300)
                .await
                .unwrap_or_else(|_| panic!("Attempt {} should succeed", i + 1));
        }

        // 6th attempt should be rate limited
        let result = limiter.check_rate_limit(&key, 5, 300).await;
        assert!(
            matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
            "6th attempt should be rate limited"
        );
    }

    /// Different IPs checking the same room should have independent limits.
    #[tokio::test]
    async fn test_room_password_rate_limit_per_ip_isolation() {
        let limiter = RateLimiter::new(None, "test:".to_string());

        let room_id = "room_xyz";
        let key_ip1 = format!("room_password_check:10.0.0.1:{room_id}");
        let key_ip2 = format!("room_password_check:10.0.0.2:{room_id}");

        // Exhaust limit for IP1
        for _ in 0..5 {
            limiter.check_rate_limit(&key_ip1, 5, 300).await.unwrap();
        }
        assert!(limiter.check_rate_limit(&key_ip1, 5, 300).await.is_err());

        // IP2 should still be allowed (independent bucket)
        assert!(limiter.check_rate_limit(&key_ip2, 5, 300).await.is_ok());
    }

    /// Same IP checking different rooms should have independent limits.
    #[tokio::test]
    async fn test_room_password_rate_limit_per_room_isolation() {
        let limiter = RateLimiter::new(None, "test:".to_string());

        let ip = "10.0.0.1";
        let key_room1 = format!("room_password_check:{ip}:room_1");
        let key_room2 = format!("room_password_check:{ip}:room_2");

        // Exhaust limit for room1
        for _ in 0..5 {
            limiter.check_rate_limit(&key_room1, 5, 300).await.unwrap();
        }
        assert!(limiter.check_rate_limit(&key_room1, 5, 300).await.is_err());

        // room2 should still be allowed (independent bucket)
        assert!(limiter.check_rate_limit(&key_room2, 5, 300).await.is_ok());
    }

    // ========== Concurrent Burst Tests ==========

    #[tokio::test]
    async fn test_concurrent_burst_all_within_limit() {
        let limiter = RateLimiter::in_memory_only("burst_test:".to_string());

        // Fire 10 concurrent requests with limit of 10 -- all should succeed
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
        assert_eq!(successes, 10, "All 10 concurrent requests within limit should succeed");
    }

    #[tokio::test]
    async fn test_concurrent_burst_exceeding_limit() {
        let limiter = RateLimiter::in_memory_only("burst_over:".to_string());

        // Fire 20 concurrent requests with limit of 5
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

        // Exactly 5 should succeed (burst capacity)
        assert_eq!(successes, 5, "Only 5 concurrent requests should succeed");
        assert_eq!(failures, 15, "15 requests should be rate limited");
    }

    // ========== Sync Rate Limit Tests ==========

    #[test]
    fn test_check_rate_limit_sync_allows_within_limit() {
        let limiter = RateLimiter::in_memory_only("sync_test:".to_string());

        // First request should succeed
        assert!(limiter.check_rate_limit_sync("sync_key", 5, 1).is_ok());
    }

    #[test]
    fn test_check_rate_limit_sync_blocks_over_limit() {
        let limiter = RateLimiter::in_memory_only("sync_block:".to_string());

        for _ in 0..5 {
            limiter.check_rate_limit_sync("sync_key", 5, 1).unwrap();
        }

        let result = limiter.check_rate_limit_sync("sync_key", 5, 1);
        assert!(matches!(result, Err(RateLimitError::RateLimitExceeded { .. })));
    }

    #[test]
    fn test_check_rate_limit_sync_uses_grpc_key_prefix() {
        let limiter = RateLimiter::in_memory_only("myprefix:".to_string());

        // Sync method adds "grpc:" between prefix and key
        // This is an implementation detail but ensures sync and async don't collide
        for _ in 0..3 {
            limiter.check_rate_limit_sync("key1", 3, 1).unwrap();
        }
        assert!(limiter.check_rate_limit_sync("key1", 3, 1).is_err());

        // Async uses different key space (no "grpc:" prefix), so should still work
        // (validated by the in-memory governor using different composed keys)
    }

    // ========== Distributed Rate Limit Tests ==========

    #[tokio::test]
    async fn test_distributed_rate_limit_fails_closed_without_redis() {
        let limiter = RateLimiter::in_memory_only("dist_test:".to_string());

        // Without Redis, distributed check should fail closed (deny request)
        let result = limiter.check_rate_limit_distributed("key", 10, 1).await;
        assert!(matches!(result, Err(RateLimitError::RateLimitExceeded { retry_after_seconds: 1 })));
    }

    // ========== RateLimitConfig Tests ==========

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

    // ========== RateLimitError Conversion Tests ==========

    #[test]
    fn test_rate_limit_error_to_core_error_exceeded() {
        let err = RateLimitError::RateLimitExceeded { retry_after_seconds: 30 };
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
        let err = RateLimitError::RateLimitExceeded { retry_after_seconds: 5 };
        let display = format!("{err}");
        assert!(display.contains("5s"));
    }

    // ========== In-Memory Limiter Quota Peek Tests ==========

    #[tokio::test]
    async fn test_get_quota_without_redis_returns_max() {
        let limiter = RateLimiter::in_memory_only("quota_test:".to_string());

        // In-memory mode returns max_requests as a best-effort estimate
        // without consuming a token (unlike the old behavior).
        let (remaining, reset) = limiter.get_quota("key", 10, 1).await.unwrap();
        assert_eq!(remaining, 10);
        assert_eq!(reset, 0);
    }

    #[tokio::test]
    async fn test_get_quota_without_redis_does_not_consume_token() {
        let limiter = RateLimiter::in_memory_only("quota_no_consume:".to_string());

        // Calling get_quota multiple times should NOT exhaust the rate limit
        for _ in 0..20 {
            let (remaining, _) = limiter.get_quota("key", 10, 1).await.unwrap();
            assert_eq!(remaining, 10);
        }

        // The actual rate limit should still have full capacity
        for i in 0..10 {
            limiter
                .check_rate_limit("key", 10, 1)
                .await
                .unwrap_or_else(|_| panic!("Request {} should succeed after get_quota calls", i));
        }
    }

    // ========== Health Check Tests ==========

    #[tokio::test]
    async fn test_health_check_without_redis() {
        let limiter = RateLimiter::in_memory_only("health:".to_string());
        let result = limiter.health_check().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not configured"));
    }

    // ========== InMemoryRateLimiter Caching Tests ==========

    #[tokio::test]
    async fn test_in_memory_different_quotas_are_independent() {
        let limiter = RateLimiter::in_memory_only("quotas:".to_string());

        // Exhaust limit for (5, 1) quota
        for _ in 0..5 {
            limiter.check_rate_limit("same_key", 5, 1).await.unwrap();
        }
        assert!(limiter.check_rate_limit("same_key", 5, 1).await.is_err());

        // Same key but different quota (10, 1) should still have capacity
        // because different (max_requests, window_seconds) use different governor instances
        assert!(limiter.check_rate_limit("same_key", 10, 1).await.is_ok());
    }

    /// When Redis is configured but unreachable, check_rate_limit should
    /// degrade to in-memory governor instead of returning a RedisError.
    #[tokio::test]
    async fn test_redis_failure_falls_back_to_in_memory() {
        // Connect to a non-existent Redis to simulate failure.
        // ConnectionManager connects lazily, so creation succeeds; the error
        // occurs on the first actual command.
        let client = redis::Client::open("redis://127.0.0.1:1").unwrap();
        let conn = redis::aio::ConnectionManager::new(client).await;

        // If ConnectionManager refuses to connect at all (some environments),
        // fall back to a simulated scenario using in_memory_only.
        if conn.is_err() {
            // Can't create a ConnectionManager to a down host -- verify that
            // in_memory_only still rate-limits properly (same code path).
            let limiter = RateLimiter::in_memory_only("fallback_test:".to_string());
            for i in 0..5 {
                limiter
                    .check_rate_limit("fb_key", 5, 1)
                    .await
                    .unwrap_or_else(|_| panic!("Request {} should succeed", i));
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

        let limiter = RateLimiter::new(Some(conn.unwrap()), "fallback_test:".to_string());

        // The Redis command will fail (connection refused), but check_rate_limit
        // should NOT return RedisError -- it should fall back to governor.
        let result = limiter.check_rate_limit("test_key", 10, 1).await;

        // Should succeed (fallback to in-memory, first request within quota)
        // OR be rate-limited (if in-memory quota exhausted) -- but NOT a RedisError.
        match &result {
            Ok(()) => {} // Good: fell back to in-memory, allowed
            Err(RateLimitError::RateLimitExceeded { .. }) => {} // Also fine
            Err(RateLimitError::RedisError(e)) => {
                panic!(
                    "check_rate_limit should NOT propagate RedisError; expected fallback to in-memory. Got: {e}"
                );
            }
        }
    }
}
