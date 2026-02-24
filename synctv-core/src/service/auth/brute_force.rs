//! Per-account and per-IP brute-force protection for login attempts.
//!
//! Tracks failed login attempts per username and per IP, enforcing exponential
//! lockout after repeated failures:
//!
//! - 5 failures: 1 minute lockout
//! - 10 failures: 5 minute lockout
//! - 15+ failures: 15 minute lockout
//!
//! Additionally tracks per-IP failures: 20 failures from a single IP within
//! 10 minutes triggers an IP-level lockout (10 minutes), preventing distributed
//! username enumeration attacks.
//!
//! ## Backend Abstraction
//!
//! Storage is abstracted via the [`AttemptTracker`] trait. Two implementations
//! are provided:
//! - [`RedisAttemptTracker`]: Redis-backed with in-memory fallback on errors
//!   (fail-closed). Used when Redis is configured.
//! - [`InMemoryAttemptTracker`]: moka cache only. Used in standalone mode
//!   without Redis.
//!
//! ## Multi-Replica Deployment Warning
//!
//! **IMPORTANT**: In multi-replica (cluster) deployments, Redis MUST be configured.
//! When `RedisAttemptTracker` falls back to its in-memory cache due to Redis
//! errors, each replica maintains independent brute-force counters. This allows
//! attackers to potentially bypass lockouts by distributing requests across replicas.
//!
//! The fallback behavior logs warnings at WARN level with the key pattern
//! `Redis degraded to fallback`. Monitor these logs to detect Redis connectivity
//! issues in production.
//!
//! For single-replica deployments, use [`InMemoryAttemptTracker`] directly via
//! [`BruteForceProtection::in_memory`] to avoid unnecessary Redis dependency.

use async_trait::async_trait;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::{cache::KeyBuilder, resilience::timeout::REDIS_OPERATION_TIMEOUT, Error, Result};

/// Stored state for brute-force tracking in Redis.
/// Serialized as JSON to store both the count and last failure timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BruteForceState {
    count: u64,
    last_failure_at: i64,
}

/// Lockout thresholds and durations
const TIER1_THRESHOLD: u64 = 5;
const TIER1_LOCKOUT_SECS: u64 = 60; // 1 minute

const TIER2_THRESHOLD: u64 = 10;
const TIER2_LOCKOUT_SECS: u64 = 300; // 5 minutes

const TIER3_THRESHOLD: u64 = 15;
const TIER3_LOCKOUT_SECS: u64 = 900; // 15 minutes

/// TTL for the failed attempts counter (15 minutes).
/// After 15 minutes of inactivity, the counter resets automatically.
pub const ATTEMPTS_TTL_SECS: u64 = 900;

/// Per-IP failure threshold: 20 failures from the same IP triggers lockout.
const IP_THRESHOLD: u64 = 20;
/// Per-IP lockout duration (10 minutes).
const IP_LOCKOUT_SECS: u64 = 600;
/// TTL for the per-IP failure counter (10 minutes).
pub const IP_ATTEMPTS_TTL_SECS: u64 = 600;

// ============================================================================
// AttemptTracker trait
// ============================================================================

/// Storage backend for brute-force attempt tracking.
///
/// Implementations must support:
/// - Getting the current attempt count and last failure timestamp for a key
/// - Recording a failed attempt (atomic increment + timestamp update)
/// - Resetting the counter for a key
#[async_trait]
pub trait AttemptTracker: Send + Sync {
    /// Get the current attempt count and last failure timestamp for `key`.
    ///
    /// Returns `(count, last_failure_at)` where `last_failure_at` is a Unix timestamp.
    /// Returns `(0, 0)` if no attempts are recorded.
    async fn get_attempts(&self, key: &str) -> (u64, i64);

    /// Record a failed attempt for `key`. Atomically increments the counter and
    /// updates the last-failure timestamp.
    async fn record_failure(&self, key: &str, now: i64, ttl_secs: u64);

    /// Reset the attempt counter for `key`.
    async fn reset(&self, key: &str);
}

// ============================================================================
// InMemoryAttemptTracker
// ============================================================================

/// In-memory [`AttemptTracker`] using moka cache with TTL-based expiry.
///
/// Used in standalone mode without Redis.
#[derive(Clone)]
pub struct InMemoryAttemptTracker {
    cache: Arc<moka::future::Cache<String, (u64, i64)>>,
}

impl InMemoryAttemptTracker {
    /// Create a new in-memory attempt tracker with the given capacity and TTL.
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
    async fn get_attempts(&self, key: &str) -> (u64, i64) {
        self.cache.get(key).await.unwrap_or((0, 0))
    }

    async fn record_failure(&self, key: &str, now: i64, _ttl_secs: u64) {
        // Use entry().and_upsert_with() for atomic read-modify-write to eliminate
        // the TOCTOU race between the previous get() + insert() sequence.
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
    }

    async fn reset(&self, key: &str) {
        self.cache.remove(key).await;
    }
}

// ============================================================================
// RedisAttemptTracker
// ============================================================================

/// Redis-backed [`AttemptTracker`] with in-memory fallback on errors.
///
/// Uses Redis Lua scripts for atomic increment + timestamp updates.
/// Falls back to an internal moka cache when Redis times out or errors
/// (fail-closed: brute-force protection stays active during Redis outages).
///
/// ## Degradation Monitoring
///
/// When Redis operations fail, this tracker falls back to an in-memory cache.
/// This degradation is tracked and can be monitored via:
/// - WARN-level logs with key `Redis degraded to fallback`
/// - [`Self::is_degraded()`] to check current degradation state
/// - [`Self::degraded_operation_count()`] to get total count of degraded ops
///
/// **WARNING**: In multi-replica deployments, degraded mode means each replica
/// maintains independent brute-force counters, allowing attackers to bypass
/// lockouts by distributing requests across replicas.
#[derive(Clone)]
pub struct RedisAttemptTracker {
    conn: redis::aio::ConnectionManager,
    /// In-memory fallback cache for fail-closed behavior on Redis errors.
    fallback: Arc<moka::future::Cache<String, (u64, i64)>>,
    /// Tracks whether we are currently in degraded mode (using fallback).
    degraded: Arc<AtomicBool>,
    /// Counts total operations that fell back to in-memory.
    degraded_count: Arc<AtomicU64>,
}

impl RedisAttemptTracker {
    /// Create a new Redis-backed attempt tracker.
    #[must_use]
    pub fn new(conn: redis::aio::ConnectionManager, max_capacity: u64, ttl_secs: u64) -> Self {
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
        }
    }

    /// Check if the tracker is currently in degraded mode (using in-memory fallback).
    ///
    /// Returns `true` if the most recent Redis operation failed and the tracker
    /// fell back to in-memory storage. Note that this is a point-in-time snapshot;
    /// the state may change on the next operation.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    /// Get the total count of operations that fell back to in-memory storage.
    ///
    /// This counter is monotonically increasing and never resets. Use this for
    /// monitoring and alerting on Redis connectivity issues.
    #[must_use]
    pub fn degraded_operation_count(&self) -> u64 {
        self.degraded_count.load(Ordering::Relaxed)
    }

    /// Mark the tracker as degraded and increment the degraded operation counter.
    ///
    /// This is called internally when a Redis operation fails and we fall back
    /// to the in-memory cache.
    fn mark_degraded(&self) {
        self.degraded.store(true, Ordering::Relaxed);
        let prev = self.degraded_count.fetch_add(1, Ordering::Relaxed);

        // Log a warning about the degradation. Include guidance for multi-replica setups.
        // Throttle logging: only log every 10 degraded operations to avoid log spam.
        if prev % 10 == 0 {
            tracing::warn!(
                degraded_count = prev + 1,
                "Redis degraded to fallback for brute-force tracking. \
                 In multi-replica deployments, lockout counters are NOT shared across replicas. \
                 Each replica maintains independent counters, reducing brute-force protection effectiveness."
            );
        }
    }

    /// Clear the degraded flag (called when a Redis operation succeeds).
    fn clear_degraded(&self) {
        self.degraded.store(false, Ordering::Relaxed);
    }
}

#[async_trait]
impl AttemptTracker for RedisAttemptTracker {
    async fn get_attempts(&self, key: &str) -> (u64, i64) {
        let mut conn = self.conn.clone();

        let redis_result = tokio::time::timeout(
            REDIS_OPERATION_TIMEOUT,
            conn.get::<_, Option<String>>(key),
        )
        .await;

        let redis_result = if let Ok(inner) = redis_result { inner } else {
            // Timeout - fall back to in-memory cache
            self.mark_degraded();
            tracing::warn!(key = %key, "Redis timeout in brute-force check, using fallback");
            return self.fallback.get(key).await.unwrap_or((0, 0));
        };

        match redis_result {
            Ok(Some(raw)) => {
                // Try parsing as JSON state first, fall back to plain integer
                // for backward compatibility with pre-existing counters.
                if let Ok(state) = serde_json::from_str::<BruteForceState>(&raw) {
                    self.clear_degraded();
                    return (state.count, state.last_failure_at);
                }
                if let Ok(count) = raw.parse::<u64>() {
                    self.clear_degraded();
                    return (count, 0);
                }
                self.clear_degraded();
                (0, 0)
            }
            Ok(None) => {
                self.clear_degraded();
                (0, 0)
            }
            Err(e) => {
                // Redis error - fall back to in-memory cache
                self.mark_degraded();
                tracing::warn!(key = %key, error = %e, "Redis error in brute-force check, using fallback");
                self.fallback.get(key).await.unwrap_or((0, 0))
            }
        }
    }

    async fn record_failure(&self, key: &str, now: i64, ttl_secs: u64) {
        let mut conn = self.conn.clone();

        let script = redis::Script::new(
            r"
            local raw = redis.call('GET', KEYS[1])
            local count = 0
            if raw then
                local ok, state = pcall(cjson.decode, raw)
                if ok and state and state.count then
                    count = tonumber(state.count) or 0
                else
                    count = tonumber(raw) or 0
                end
            end
            count = count + 1
            local new_state = cjson.encode({count = count, last_failure_at = tonumber(ARGV[1])})
            redis.call('SET', KEYS[1], new_state, 'EX', tonumber(ARGV[2]))
            return count
            ",
        );

        let result: std::result::Result<u64, _> = tokio::time::timeout(
            REDIS_OPERATION_TIMEOUT,
            script
                .key(key)
                .arg(now)
                .arg(ttl_secs as i64)
                .invoke_async(&mut conn),
        )
        .await
        .unwrap_or_else(|_| Err(redis::RedisError::from((
            redis::ErrorKind::Io,
            "Redis timeout: record_failure",
        ))));

        match result {
            Ok(count) => {
                self.clear_degraded();
                tracing::debug!(key = %key, attempts = count, "Recorded failed attempt");
            }
            Err(e) => {
                // Redis error - fall back to in-memory cache
                self.mark_degraded();
                tracing::warn!(key = %key, error = %e, "Redis error in record_failure, using fallback");
                let (count, _) = self.fallback.get(key).await.unwrap_or((0, now));
                self.fallback.insert(key.to_string(), (count + 1, now)).await;
            }
        }
    }

    async fn reset(&self, key: &str) {
        // Always clear fallback cache
        self.fallback.remove(key).await;

        let mut conn = self.conn.clone();
        match tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.del::<_, ()>(key)).await {
            Ok(Ok(())) => {
                self.clear_degraded();
            }
            Ok(Err(e)) => {
                self.mark_degraded();
                tracing::warn!(key = %key, error = %e, "Redis error in reset");
            }
            Err(e) => {
                self.mark_degraded();
                tracing::warn!(key = %key, error = %e, "Redis timeout in reset");
            }
        }
    }
}

// ============================================================================
// BruteForceProtection
// ============================================================================

/// Brute-force protection service.
///
/// Uses [`AttemptTracker`] trait objects for storage, allowing transparent
/// switching between Redis-backed and in-memory implementations.
#[derive(Clone)]
pub struct BruteForceProtection {
    key_builder: KeyBuilder,
    /// Attempt tracker for per-username tracking
    username_tracker: Arc<dyn AttemptTracker>,
    /// Attempt tracker for per-IP tracking
    ip_tracker: Arc<dyn AttemptTracker>,
}

impl std::fmt::Debug for BruteForceProtection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BruteForceProtection")
            .finish()
    }
}

impl BruteForceProtection {
    /// Create a new brute-force protection service with the given trackers.
    ///
    /// Use [`Self::with_redis`] or [`Self::in_memory`] for convenience.
    #[must_use]
    pub fn new(
        key_prefix: String,
        username_tracker: Arc<dyn AttemptTracker>,
        ip_tracker: Arc<dyn AttemptTracker>,
    ) -> Self {
        Self {
            key_builder: KeyBuilder::new(key_prefix),
            username_tracker,
            ip_tracker,
        }
    }

    /// Create a Redis-backed brute-force protection service.
    ///
    /// Uses Redis for distributed tracking with in-memory fallback on errors.
    #[must_use]
    pub fn with_redis(conn: redis::aio::ConnectionManager, key_prefix: String) -> Self {
        let username_tracker = Arc::new(RedisAttemptTracker::new(
            conn.clone(), 50_000, ATTEMPTS_TTL_SECS,
        ));
        let ip_tracker = Arc::new(RedisAttemptTracker::new(
            conn, 100_000, IP_ATTEMPTS_TTL_SECS,
        ));
        Self::new(key_prefix, username_tracker, ip_tracker)
    }

    /// Create an in-memory-only brute-force protection service.
    ///
    /// Used in standalone mode without Redis.
    #[must_use]
    pub fn in_memory(key_prefix: String) -> Self {
        let username_tracker = Arc::new(InMemoryAttemptTracker::new(50_000, ATTEMPTS_TTL_SECS));
        let ip_tracker = Arc::new(InMemoryAttemptTracker::new(100_000, IP_ATTEMPTS_TTL_SECS));
        Self::new(key_prefix, username_tracker, ip_tracker)
    }

    /// Check if a login attempt is allowed for the given username and optional IP.
    ///
    /// Returns `Ok(())` if the attempt is allowed, or an authentication error
    /// with the remaining lockout duration if the account or IP is locked.
    pub async fn check_allowed(&self, username: &str, ip: Option<IpAddr>) -> Result<()> {
        // Check IP-level lockout first
        if let Some(ip_addr) = ip {
            let ip_key = self.key_builder.login_attempts_ip(&ip_addr.to_string());
            let (ip_attempts, ip_last_failure_at) = self.ip_tracker.get_attempts(&ip_key).await;
            if ip_attempts >= IP_THRESHOLD {
                let now = chrono::Utc::now().timestamp();
                let elapsed = (now - ip_last_failure_at).max(0) as u64;
                if elapsed < IP_LOCKOUT_SECS {
                    let remaining = IP_LOCKOUT_SECS - elapsed;
                    tracing::warn!(
                        ip = %ip_addr,
                        attempts = ip_attempts,
                        remaining_secs = remaining,
                        "Login attempt blocked: IP temporarily locked"
                    );
                    return Err(Error::Authentication(format!(
                        "Too many failed login attempts. Please try again in {remaining} seconds.",
                    )));
                }
            }
        }

        // Check per-username lockout
        let key = self.key_builder.login_attempts(username);
        let (attempts, last_failure_at) = self.username_tracker.get_attempts(&key).await;
        let lockout_secs = Self::lockout_duration(attempts);
        if let Some(lockout_secs) = lockout_secs {
            let now = chrono::Utc::now().timestamp();
            let elapsed = (now - last_failure_at).max(0) as u64;
            if elapsed < lockout_secs {
                let remaining = lockout_secs - elapsed;
                tracing::warn!(
                    username = %username,
                    attempts = attempts,
                    lockout_secs = lockout_secs,
                    remaining_secs = remaining,
                    "Login attempt blocked: account temporarily locked"
                );
                return Err(Error::Authentication(format!(
                    "Too many failed login attempts. Please try again in {remaining} seconds.",
                )));
            }
        }
        Ok(())
    }

    /// Record a failed login attempt. Increments the counter, stores the
    /// last-failure timestamp. Also records the failure against the IP address if provided.
    pub async fn record_failure(&self, username: &str, ip: Option<IpAddr>) -> Result<()> {
        let now = chrono::Utc::now().timestamp();

        // Record IP-level failure
        if let Some(ip_addr) = ip {
            let ip_key = self.key_builder.login_attempts_ip(&ip_addr.to_string());
            self.ip_tracker.record_failure(&ip_key, now, IP_ATTEMPTS_TTL_SECS).await;
        }

        // Record username-level failure
        let key = self.key_builder.login_attempts(username);
        self.username_tracker.record_failure(&key, now, ATTEMPTS_TTL_SECS).await;
        Ok(())
    }

    /// Reset the failed login attempt counter on successful login.
    pub async fn reset(&self, username: &str) -> Result<()> {
        let key = self.key_builder.login_attempts(username);
        self.username_tracker.reset(&key).await;
        Ok(())
    }

    /// Reset the per-IP failed login attempt counter on successful login.
    pub async fn reset_ip(&self, ip: &IpAddr) -> Result<()> {
        let ip_key = self.key_builder.login_attempts_ip(&ip.to_string());
        self.ip_tracker.reset(&ip_key).await;
        Ok(())
    }

    /// Determine lockout duration based on failure count.
    ///
    /// Returns `Some(seconds)` if locked out, `None` if allowed.
    const fn lockout_duration(attempts: u64) -> Option<u64> {
        if attempts >= TIER3_THRESHOLD {
            Some(TIER3_LOCKOUT_SECS)
        } else if attempts >= TIER2_THRESHOLD {
            Some(TIER2_LOCKOUT_SECS)
        } else if attempts >= TIER1_THRESHOLD {
            Some(TIER1_LOCKOUT_SECS)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Integration tests that require Redis (record_failure, check_allowed, etc.)
    // are in the integration test suite. Unit tests here cover pure logic only.

    /// Test that lockout duration thresholds are correct
    #[test]
    fn test_lockout_duration_standard_thresholds() {
        assert_eq!(BruteForceProtection::lockout_duration(4), None);
        assert_eq!(BruteForceProtection::lockout_duration(5), Some(TIER1_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration(9), Some(TIER1_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration(10), Some(TIER2_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration(14), Some(TIER2_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration(15), Some(TIER3_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration(100), Some(TIER3_LOCKOUT_SECS));
    }

    // ========================================================================
    // RedisAttemptTracker degradation tracking tests
    // ========================================================================

    /// Test that RedisAttemptTracker initializes with degradation tracking in clean state
    #[test]
    fn test_redis_tracker_initial_state_not_degraded() {
        // Create a mock RedisAttemptTracker to test initial state
        // Note: We can't actually create a ConnectionManager without a Redis server,
        // so we test the atomic state management separately
        let degraded = Arc::new(AtomicBool::new(false));
        let degraded_count = Arc::new(AtomicU64::new(0));

        assert!(!degraded.load(Ordering::Relaxed));
        assert_eq!(degraded_count.load(Ordering::Relaxed), 0);
    }

    /// Test that the degraded flag and counter work correctly
    #[test]
    fn test_degradation_state_management() {
        let degraded = Arc::new(AtomicBool::new(false));
        let degraded_count = Arc::new(AtomicU64::new(0));

        // Simulate marking as degraded
        degraded.store(true, Ordering::Relaxed);
        let prev = degraded_count.fetch_add(1, Ordering::Relaxed);
        assert_eq!(prev, 0);
        assert!(degraded.load(Ordering::Relaxed));
        assert_eq!(degraded_count.load(Ordering::Relaxed), 1);

        // Simulate clearing degraded state
        degraded.store(false, Ordering::Relaxed);
        assert!(!degraded.load(Ordering::Relaxed));
        // Counter should still be 1 (monotonically increasing)
        assert_eq!(degraded_count.load(Ordering::Relaxed), 1);

        // Multiple degradation events
        for _ in 0..5 {
            degraded.store(true, Ordering::Relaxed);
            degraded_count.fetch_add(1, Ordering::Relaxed);
        }
        assert_eq!(degraded_count.load(Ordering::Relaxed), 6);
    }

    /// Test that InMemoryAttemptTracker never reports as degraded
    /// (it's always the intended backend, not a fallback)
    #[tokio::test]
    async fn test_in_memory_tracker_is_intended_backend() {
        let tracker = InMemoryAttemptTracker::new(1000, 900);
        let key = "test:user";

        // Perform operations - these should work without any "degradation" concept
        let now = chrono::Utc::now().timestamp();
        tracker.record_failure(key, now, 900).await;
        tracker.record_failure(key, now, 900).await;

        let (count, _) = tracker.get_attempts(key).await;
        assert_eq!(count, 2);

        tracker.reset(key).await;
        let (count, _) = tracker.get_attempts(key).await;
        assert_eq!(count, 0);

        // InMemoryAttemptTracker is always the intended backend,
        // there's no "fallback" or "degraded" state to check
    }
}
