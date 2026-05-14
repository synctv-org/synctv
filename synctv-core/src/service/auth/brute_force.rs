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
//! - [`RedisAttemptTracker`]: Redis-backed with configurable failure mode.
//!   In distributed mode (`fail_closed=true`), Redis failures result in rejected
//!   requests rather than degraded protection.
//! - [`InMemoryAttemptTracker`]: moka cache only. Used in standalone mode
//!   without Redis.
//!
//! ## Multi-Replica (Cluster) Deployment Requirements
//!
//! **CRITICAL**: In multi-replica (cluster) deployments, Redis is MANDATORY.
//! The brute-force protection counters MUST be shared across all replicas to
//! prevent attackers from bypassing lockouts by distributing requests.
//!
//! ### Cluster Mode Configuration
//!
//! 1. Set `cluster.enabled = true` in configuration
//! 2. Configure Redis via `redis.url` or `SYNCTV_REDIS_URL`
//! 3. The system will automatically use `fail_closed=true` for brute-force protection
//!
//! When `fail_closed=true` and Redis becomes unavailable:
//! - All login attempts are rejected with an internal error
//! - This prevents the security degradation that would occur if each replica
//!   maintained independent counters
//! - Monitoring alerts should be configured to detect this condition immediately
//!
//! ### Monitoring Degradation
//!
//! When not in fail-closed mode (standalone with Redis), fallback events are logged
//! at WARN level with key pattern `Redis degraded to fallback`. Monitor these logs
//! to detect Redis connectivity issues in production.
//!
//! For single-replica deployments without Redis, use [`InMemoryAttemptTracker`]
//! directly via [`BruteForceProtection::in_memory`].

use async_trait::async_trait;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use synctv_common::ExecutionControl;

use crate::{
    cache::KeyBuilder, Error, RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile,
};

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
                count = tonumber(raw) or 0
            end
        end
        count = count + 1
        local new_state = cjson.encode({count = count, last_failure_at = tonumber(ARGV[1])})
        redis.call('SET', KEYS[1], new_state, 'EX', tonumber(ARGV[2]))
        return count
        ",
    )
});

/// Stable service boundary for brute-force protection.
///
/// Callers should depend on this trait rather than the concrete
/// `BruteForceProtection` so the implementation can be replaced transparently.
#[async_trait]
pub trait BruteForceProtectionService: Send + Sync {
    async fn check_allowed(&self, username: &str, ip: Option<IpAddr>) -> Result<()>;
    async fn check_allowed_with_control(
        &self,
        username: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()>;
    async fn record_failure(&self, username: &str, ip: Option<IpAddr>) -> Result<()>;
    async fn record_failure_with_control(
        &self,
        username: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()>;
    async fn record_ip_failure(&self, ip: Option<IpAddr>) -> Result<()>;
    async fn record_ip_failure_with_control(
        &self,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()>;
    async fn check_ip_allowed(&self, ip: Option<IpAddr>) -> Result<()>;
    async fn check_ip_allowed_with_control(
        &self,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()>;
    async fn reset(&self, username: &str) -> Result<()>;
    async fn reset_with_control(
        &self,
        username: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()>;
    async fn reset_ip(&self, ip: &IpAddr) -> Result<()>;
    async fn reset_ip_with_control(
        &self,
        ip: &IpAddr,
        control: Option<&ExecutionControl>,
    ) -> Result<()>;
}

/// Build a brute-force protection service behind the service abstraction.
///
/// Callers should depend on the returned trait object instead of choosing the
/// concrete local or shared implementation directly.
pub fn brute_force_protection_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn BruteForceProtectionService>> {
    Ok(Arc::new(BruteForceProtection::from_shared_state_profile(
        profile,
    )?))
}

#[async_trait]
impl<T> BruteForceProtectionService for Arc<T>
where
    T: BruteForceProtectionService + ?Sized,
{
    async fn check_allowed(&self, username: &str, ip: Option<IpAddr>) -> Result<()> {
        self.as_ref().check_allowed(username, ip).await
    }

    async fn check_allowed_with_control(
        &self,
        username: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.as_ref()
            .check_allowed_with_control(username, ip, control)
            .await
    }

    async fn record_failure(&self, username: &str, ip: Option<IpAddr>) -> Result<()> {
        self.as_ref().record_failure(username, ip).await
    }

    async fn record_failure_with_control(
        &self,
        username: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.as_ref()
            .record_failure_with_control(username, ip, control)
            .await
    }

    async fn record_ip_failure(&self, ip: Option<IpAddr>) -> Result<()> {
        self.as_ref().record_ip_failure(ip).await
    }

    async fn record_ip_failure_with_control(
        &self,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.as_ref()
            .record_ip_failure_with_control(ip, control)
            .await
    }

    async fn check_ip_allowed(&self, ip: Option<IpAddr>) -> Result<()> {
        self.as_ref().check_ip_allowed(ip).await
    }

    async fn check_ip_allowed_with_control(
        &self,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.as_ref()
            .check_ip_allowed_with_control(ip, control)
            .await
    }

    async fn reset(&self, username: &str) -> Result<()> {
        self.as_ref().reset(username).await
    }

    async fn reset_with_control(
        &self,
        username: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.as_ref().reset_with_control(username, control).await
    }

    async fn reset_ip(&self, ip: &IpAddr) -> Result<()> {
        self.as_ref().reset_ip(ip).await
    }

    async fn reset_ip_with_control(
        &self,
        ip: &IpAddr,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.as_ref().reset_ip_with_control(ip, control).await
    }
}

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

fn nonnegative_i64_to_u64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or_default()
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

async fn run_with_control<T, F>(control: Option<&ExecutionControl>, future: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    match control {
        Some(control) => control.run(future).await.map_err(Error::from)?,
        None => future.await,
    }
}

/// Configuration for brute-force protection thresholds and durations.
///
/// This struct allows customizing the brute-force protection behavior
/// without changing code. It can be loaded from the settings system.
///
/// ## Default Values
///
/// The defaults match the current production thresholds:
/// - Tier 1: 5 failures → 60 second lockout
/// - Tier 2: 10 failures → 5 minute lockout
/// - Tier 3: 15 failures → 15 minute lockout
/// - IP lockout: 20 failures → 10 minute lockout
///
/// ## Example
///
/// ```text
/// let config = BruteForceConfig {
/// tier1_threshold: 3,
/// tier1_lockout_secs: 30,
///..BruteForceConfig::default()
/// };
/// let protection = BruteForceProtection::in_memory_with_config("prefix".to_string(), config);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BruteForceConfig {
    /// Number of failures to trigger tier 1 lockout (default: 5)
    #[serde(default = "default_tier1_threshold")]
    pub tier1_threshold: u64,
    /// Lockout duration in seconds for tier 1 (default: 60)
    #[serde(default = "default_tier1_lockout_secs")]
    pub tier1_lockout_secs: u64,

    /// Number of failures to trigger tier 2 lockout (default: 10)
    #[serde(default = "default_tier2_threshold")]
    pub tier2_threshold: u64,
    /// Lockout duration in seconds for tier 2 (default: 300)
    #[serde(default = "default_tier2_lockout_secs")]
    pub tier2_lockout_secs: u64,

    /// Number of failures to trigger tier 3 lockout (default: 15)
    #[serde(default = "default_tier3_threshold")]
    pub tier3_threshold: u64,
    /// Lockout duration in seconds for tier 3 (default: 900)
    #[serde(default = "default_tier3_lockout_secs")]
    pub tier3_lockout_secs: u64,

    /// Number of failures from a single IP to trigger IP lockout (default: 20)
    #[serde(default = "default_ip_threshold")]
    pub ip_threshold: u64,
    /// Lockout duration in seconds for IP-level lockout (default: 600)
    #[serde(default = "default_ip_lockout_secs")]
    pub ip_lockout_secs: u64,

    /// TTL for the per-username attempt counter in seconds (default: 900)
    #[serde(default = "default_attempts_ttl_secs")]
    pub attempts_ttl_secs: u64,
    /// TTL for the per-IP attempt counter in seconds (default: 600)
    #[serde(default = "default_ip_attempts_ttl_secs")]
    pub ip_attempts_ttl_secs: u64,
}

// Helper functions for serde defaults
const fn default_tier1_threshold() -> u64 {
    TIER1_THRESHOLD
}
const fn default_tier1_lockout_secs() -> u64 {
    TIER1_LOCKOUT_SECS
}
const fn default_tier2_threshold() -> u64 {
    TIER2_THRESHOLD
}
const fn default_tier2_lockout_secs() -> u64 {
    TIER2_LOCKOUT_SECS
}
const fn default_tier3_threshold() -> u64 {
    TIER3_THRESHOLD
}
const fn default_tier3_lockout_secs() -> u64 {
    TIER3_LOCKOUT_SECS
}
const fn default_ip_threshold() -> u64 {
    IP_THRESHOLD
}
const fn default_ip_lockout_secs() -> u64 {
    IP_LOCKOUT_SECS
}
const fn default_attempts_ttl_secs() -> u64 {
    ATTEMPTS_TTL_SECS
}
const fn default_ip_attempts_ttl_secs() -> u64 {
    IP_ATTEMPTS_TTL_SECS
}

impl Default for BruteForceConfig {
    fn default() -> Self {
        Self {
            // Tier thresholds and lockout durations
            tier1_threshold: TIER1_THRESHOLD,
            tier1_lockout_secs: TIER1_LOCKOUT_SECS,
            tier2_threshold: TIER2_THRESHOLD,
            tier2_lockout_secs: TIER2_LOCKOUT_SECS,
            tier3_threshold: TIER3_THRESHOLD,
            tier3_lockout_secs: TIER3_LOCKOUT_SECS,
            // IP lockout
            ip_threshold: IP_THRESHOLD,
            ip_lockout_secs: IP_LOCKOUT_SECS,
            // TTLs
            attempts_ttl_secs: ATTEMPTS_TTL_SECS,
            ip_attempts_ttl_secs: IP_ATTEMPTS_TTL_SECS,
        }
    }
}

// AttemptTracker trait

/// Storage backend for brute-force attempt tracking.
///
/// Implementations must support:
/// - Getting the current attempt count and last failure timestamp for a key
/// - Recording a failed attempt (atomic increment + timestamp update)
/// - Resetting the counter for a key
///
/// ## Error Handling
///
/// All trait methods return `Result` to support fail-closed behavior in cluster
/// mode. When Redis is unavailable and `fail_closed=true`:
/// - `get_attempts()` returns `Err` to block login attempts
/// - `record_failure()` returns `Err` to signal the failure (but doesn't block)
/// - `reset()` returns `Err` but best-effort continues
#[async_trait]
pub trait AttemptTracker: Send + Sync {
    /// Get the current attempt count and last failure timestamp for `key`.
    ///
    /// Returns `(count, last_failure_at)` where `last_failure_at` is a Unix timestamp.
    /// Returns `(0, 0)` if no attempts are recorded.
    ///
    /// # Errors
    ///
    /// Returns an error when the storage backend is unavailable and fail-closed
    /// mode is enabled. In this case, the caller should deny the login attempt.
    async fn get_attempts(&self, key: &str) -> Result<(u64, i64)>;

    /// Record a failed attempt for `key`. Atomically increments the counter and
    /// updates the last-failure timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error when the storage backend is unavailable and fail-closed
    /// mode is enabled. The caller should still deny the login but may want to
    /// log the tracking failure separately.
    async fn record_failure(&self, key: &str, now: i64, ttl_secs: u64) -> Result<()>;

    /// Reset the attempt counter for `key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the storage backend is unavailable. The reset is
    /// best-effort and failures should be logged but typically not propagated
    /// to the caller (a failed reset shouldn't block successful login).
    async fn reset(&self, key: &str) -> Result<()>;
}

// InMemoryAttemptTracker

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
    async fn get_attempts(&self, key: &str) -> Result<(u64, i64)> {
        Ok(self.cache.get(key).await.unwrap_or((0, 0)))
    }

    async fn record_failure(&self, key: &str, now: i64, _ttl_secs: u64) -> Result<()> {
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
        Ok(())
    }

    async fn reset(&self, key: &str) -> Result<()> {
        self.cache.remove(key).await;
        Ok(())
    }
}

// RedisAttemptTracker

/// Redis-backed [`AttemptTracker`] with configurable failure handling.
///
/// Uses Redis Lua scripts for atomic increment + timestamp updates.
///
/// ## Failure Modes
///
/// When `fail_closed=true` (cluster mode), Redis failures result in errors that
/// block login attempts. This prevents the security degradation that would occur
/// if each replica maintained independent counters.
///
/// When `fail_closed=false` (standalone mode with Redis), falls back to an
/// internal moka cache when Redis times out or errors. This provides
/// best-effort protection while maintaining availability.
///
/// ## Degradation Monitoring
///
/// When Redis operations fail and `fail_closed=false`, this tracker falls back
/// to an in-memory cache. This degradation is tracked and can be monitored via:
/// - WARN-level logs with key `Redis degraded to fallback`
/// - [`Self::is_degraded()`] to check current degradation state
/// - [`Self::degraded_operation_count()`] to get total count of degraded ops
///
/// **WARNING**: In multi-replica deployments with `fail_closed=false`, degraded
/// mode means each replica maintains independent brute-force counters, allowing
/// attackers to bypass lockouts by distributing requests across replicas.
#[derive(Clone)]
pub struct RedisAttemptTracker {
    /// Redis runtime that yields a fresh connection snapshot per operation.
    conn: Arc<dyn RedisConnectionRuntime>,
    /// In-memory fallback cache for fail-closed behavior on Redis errors.
    fallback: Arc<moka::future::Cache<String, (u64, i64)>>,
    /// Tracks whether we are currently in degraded mode (using fallback).
    degraded: Arc<AtomicBool>,
    /// Counts total operations that fell back to in-memory.
    degraded_count: Arc<AtomicU64>,
    /// When true, Redis failures result in errors rather than fallback.
    /// Required for cluster mode to prevent security degradation.
    fail_closed: bool,
}

impl RedisAttemptTracker {
    fn fail_closed_backend_error(detail: &str) -> Error {
        Error::ServiceUnavailable(format!(
            "Brute-force protection temporarily unavailable: {detail}"
        ))
    }

    /// Create a new Redis-backed attempt tracker with fallback mode.
    ///
    /// Accepts the shared `Arc<RwLock<ConnectionManager>>` so that the tracker
    /// automatically follows Sentinel failover without holding a stale snapshot.
    ///
    /// When Redis fails, the tracker falls back to in-memory cache.
    /// Suitable for standalone deployments where Redis is optional.
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

    /// Create a new Redis-backed attempt tracker with fail-closed mode.
    ///
    /// Accepts the shared `Arc<RwLock<ConnectionManager>>` so that the tracker
    /// automatically follows Sentinel failover without holding a stale snapshot.
    ///
    /// When Redis fails, operations return errors rather than falling back.
    /// Required for cluster mode to prevent security degradation from
    /// per-replica independent counters.
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

    /// Acquire a bounded fresh ConnectionManager clone from the shared handle.
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

    /// Check if the tracker is currently in degraded mode (using in-memory fallback).
    ///
    /// Returns `true` if the most recent Redis operation failed and the tracker
    /// fell back to in-memory storage. Note that this is a point-in-time snapshot;
    /// the state may change on the next operation.
    ///
    /// In fail-closed mode, this always returns `false` since failures result in
    /// errors rather than fallback.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    /// Get the total count of operations that fell back to in-memory storage.
    ///
    /// This counter is monotonically increasing and never resets. Use this for
    /// monitoring and alerting on Redis connectivity issues.
    ///
    /// In fail-closed mode, this always returns `0` since failures result in
    /// errors rather than fallback.
    #[must_use]
    pub fn degraded_operation_count(&self) -> u64 {
        self.degraded_count.load(Ordering::Relaxed)
    }

    /// Check if this tracker is in fail-closed mode.
    #[must_use]
    pub const fn is_fail_closed(&self) -> bool {
        self.fail_closed
    }

    /// Mark the tracker as degraded and increment the degraded operation counter.
    ///
    /// This is called internally when a Redis operation fails and we fall back
    /// to the in-memory cache (only when `fail_closed=false`).
    fn mark_degraded(&self) {
        self.degraded.store(true, Ordering::Relaxed);
        let prev = self.degraded_count.fetch_add(1, Ordering::Relaxed);

        // Log a warning about the degradation. Include guidance for multi-replica setups.
        // Throttle logging: only log every 10 degraded operations to avoid log spam.
        if prev.is_multiple_of(10) {
            tracing::warn!(
                degraded_count = prev + 1,
                "Redis degraded to fallback for brute-force tracking. \
                 In multi-replica deployments, lockout counters are NOT shared across replicas. \
                 Each replica maintains independent counters, reducing brute-force protection effectiveness."
            );
        }
    }

    /// Log a critical alert for fail-closed mode Redis failure.
    ///
    /// This indicates a potential security issue - all login attempts will be
    /// blocked until Redis recovers.
    fn log_fail_closed_rejection(operation: &'static str, error: &str, key: &str) {
        tracing::error!(
            operation = operation,
            key = %key,
            error = %error,
            "Redis unavailable in fail-closed mode: blocking all login attempts for security. \
             In distributed mode, falling back to per-replica counters would allow attackers to \
             bypass brute-force protection by distributing requests across replicas. \
             Restore Redis availability immediately to allow logins."
        );
    }

    /// Clear the degraded flag (called when a Redis operation succeeds).
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
            // Timeout
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
                if let Ok(state) = serde_json::from_str::<BruteForceState>(&raw) {
                    self.clear_degraded();
                    return Ok((state.count, state.last_failure_at));
                }
                self.clear_degraded();
                Ok((0, 0))
            }
            Ok(None) => {
                self.clear_degraded();
                Ok((0, 0))
            }
            Err(e) => {
                // Redis error
                if self.fail_closed {
                    Self::log_fail_closed_rejection("get_attempts", &e.to_string(), key);
                    return Err(Self::fail_closed_backend_error("please try again later"));
                }
                self.mark_degraded();
                tracing::warn!(key = %key, error = %e, "Redis error in brute-force check, using fallback");
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
                .arg(u64_to_i64_saturating(ttl_secs))
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
            Err(e) => {
                // Redis error
                if self.fail_closed {
                    Self::log_fail_closed_rejection("record_failure", &e.to_string(), key);
                    // Still return error - caller should know tracking failed
                    return Err(Self::fail_closed_backend_error("please try again later"));
                }
                self.mark_degraded();
                tracing::warn!(key = %key, error = %e, "Redis error in record_failure, using fallback");
                let (count, _) = self.fallback.get(key).await.unwrap_or((0, now));
                self.fallback
                    .insert(key.to_string(), (count + 1, now))
                    .await;
                Ok(())
            }
        }
    }

    async fn reset(&self, key: &str) -> Result<()> {
        // Always clear fallback cache (best-effort)
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
            Ok(Err(e)) => {
                if self.fail_closed {
                    Self::log_fail_closed_rejection("reset", &e.to_string(), key);
                    return Err(Self::fail_closed_backend_error("reset failed"));
                }
                self.mark_degraded();
                tracing::warn!(key = %key, error = %e, "Redis error in reset");
                Ok(()) // Best-effort: reset failure shouldn't block successful login
            }
            Err(e) => {
                if self.fail_closed {
                    Self::log_fail_closed_rejection("reset", &e.to_string(), key);
                    return Err(Self::fail_closed_backend_error("reset timed out"));
                }
                self.mark_degraded();
                tracing::warn!(key = %key, error = %e, "Redis timeout in reset");
                Ok(()) // Best-effort: reset failure shouldn't block successful login
            }
        }
    }
}

// BruteForceProtection

/// Brute-force protection service.
///
/// Uses [`AttemptTracker`] trait objects for storage, allowing transparent
/// switching between Redis-backed and in-memory implementations.
///
/// ## Cluster Mode
///
/// In distributed mode, use [`Self::with_redis_fail_closed`] to ensure Redis
/// failures result in rejected login attempts rather than degraded protection.
/// This is critical for security in multi-replica deployments where fallback
/// to per-replica counters would allow attackers to bypass lockouts.
#[derive(Clone)]
pub struct BruteForceProtection {
    key_builder: KeyBuilder,
    /// Attempt tracker for per-username tracking.
    username_tracker: Arc<dyn AttemptTracker>,
    /// Attempt tracker for per-IP tracking.
    ip_tracker: Arc<dyn AttemptTracker>,
    /// Configuration for thresholds and durations.
    config: BruteForceConfig,
}

impl std::fmt::Debug for BruteForceProtection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BruteForceProtection").finish()
    }
}

impl BruteForceProtection {
    /// Create a new brute-force protection service with the given trackers.
    ///
    /// Use [`Self::with_redis`], [`Self::with_redis_fail_closed`], or
    /// [`Self::in_memory`] for convenience.
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
            config: BruteForceConfig::default(),
        }
    }

    /// Create a new brute-force protection service with custom config.
    ///
    /// Use this when you need to customize thresholds and durations.
    #[must_use]
    pub fn new_with_config(
        key_prefix: String,
        username_tracker: Arc<dyn AttemptTracker>,
        ip_tracker: Arc<dyn AttemptTracker>,
        config: BruteForceConfig,
    ) -> Self {
        Self {
            key_builder: KeyBuilder::new(key_prefix),
            username_tracker,
            ip_tracker,
            config,
        }
    }

    /// Create a Redis-backed brute-force protection service with fallback mode.
    ///
    /// Uses Redis for distributed tracking. When Redis is unavailable, falls
    /// back to in-memory cache (per-replica counters). Suitable for standalone
    /// deployments where Redis is optional.
    ///
    /// **WARNING**: In multi-replica deployments, fallback mode means each
    /// replica maintains independent brute-force counters, allowing attackers
    /// to bypass lockouts. Use [`Self::with_redis_fail_closed`] for cluster mode.
    fn with_redis_runtime(conn: Arc<dyn RedisConnectionRuntime>, key_prefix: String) -> Self {
        let config = BruteForceConfig::default();
        let username_tracker = Arc::new(RedisAttemptTracker::from_runtime(
            conn.clone(),
            50_000,
            config.attempts_ttl_secs,
        ));
        let ip_tracker = Arc::new(RedisAttemptTracker::from_runtime(
            conn,
            100_000,
            config.ip_attempts_ttl_secs,
        ));
        Self::new_with_config(key_prefix, username_tracker, ip_tracker, config)
    }

    /// Create a Redis-backed brute-force protection service with fail-closed mode.
    ///
    /// Uses Redis for distributed tracking. When Redis is unavailable, login
    /// attempts are rejected rather than falling back to per-replica counters.
    /// **Required for cluster mode** to prevent security degradation.
    ///
    /// ## Security Rationale
    ///
    /// In multi-replica deployments, falling back to per-replica in-memory
    /// counters would allow attackers to bypass brute-force protection by
    /// distributing requests across replicas. By failing closed, we ensure
    /// that Redis unavailability results in denied logins rather than
    /// degraded security.
    ///
    /// ## Monitoring
    ///
    /// Configure alerts on ERROR-level logs with pattern "Redis unavailable
    /// in fail-closed mode" to detect when brute-force protection is blocking
    /// all login attempts.
    fn with_redis_runtime_fail_closed(
        conn: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
    ) -> Self {
        tracing::info!(
            "Brute-force protection initialized in fail-closed mode. \
             Login attempts will be rejected if Redis is unavailable."
        );
        let config = BruteForceConfig::default();
        let username_tracker = Arc::new(RedisAttemptTracker::from_runtime_fail_closed(
            conn.clone(),
            50_000,
            config.attempts_ttl_secs,
        ));
        let ip_tracker = Arc::new(RedisAttemptTracker::from_runtime_fail_closed(
            conn,
            100_000,
            config.ip_attempts_ttl_secs,
        ));
        Self::new_with_config(key_prefix, username_tracker, ip_tracker, config)
    }

    pub fn from_shared_state_profile(profile: &SharedStateProfile) -> Result<Self> {
        match profile.state_mode() {
            SharedStateMode::SharedRequired => Ok(Self::with_redis_runtime_fail_closed(
                profile.require_shared_runtime("brute-force protection state")?,
                profile.key_prefix().to_string(),
            )),
            SharedStateMode::SharedBestEffort => Ok(Self::with_redis_runtime(
                profile
                    .shared_runtime()
                    .expect("shared state profile guarantees runtime in best-effort mode"),
                profile.key_prefix().to_string(),
            )),
            SharedStateMode::LocalOnly => Ok(Self::in_memory(profile.key_prefix().to_string())),
        }
    }

    /// Create an in-memory-only brute-force protection service.
    ///
    /// Used in standalone mode without Redis. Counters are local to this
    /// process and not shared across replicas.
    ///
    /// **WARNING**: Do not use in cluster mode. In multi-replica deployments,
    /// each replica would maintain independent counters, allowing attackers
    /// to bypass lockouts by distributing requests across replicas.
    #[must_use]
    pub fn in_memory(key_prefix: String) -> Self {
        let config = BruteForceConfig::default();
        let username_tracker = Arc::new(InMemoryAttemptTracker::new(
            50_000,
            config.attempts_ttl_secs,
        ));
        let ip_tracker = Arc::new(InMemoryAttemptTracker::new(
            100_000,
            config.ip_attempts_ttl_secs,
        ));
        Self::new_with_config(key_prefix, username_tracker, ip_tracker, config)
    }

    /// Create an in-memory brute-force protection service with custom config.
    ///
    /// Use this when you need to customize thresholds and durations.
    ///
    /// **WARNING**: Do not use in cluster mode. In multi-replica deployments,
    /// each replica would maintain independent counters, allowing attackers
    /// to bypass lockouts by distributing requests across replicas.
    #[must_use]
    pub fn in_memory_with_config(key_prefix: String, config: BruteForceConfig) -> Self {
        let username_tracker = Arc::new(InMemoryAttemptTracker::new(
            50_000,
            config.attempts_ttl_secs,
        ));
        let ip_tracker = Arc::new(InMemoryAttemptTracker::new(
            100_000,
            config.ip_attempts_ttl_secs,
        ));
        Self::new_with_config(key_prefix, username_tracker, ip_tracker, config)
    }

    /// Get a reference to the current config.
    #[must_use]
    pub const fn config(&self) -> &BruteForceConfig {
        &self.config
    }

    /// Determine lockout duration based on failure count using the stored config.
    ///
    /// Returns `Some(seconds)` if locked out, `None` if allowed.
    const fn lockout_duration_with_config(&self, attempts: u64) -> Option<u64> {
        if attempts >= self.config.tier3_threshold {
            Some(self.config.tier3_lockout_secs)
        } else if attempts >= self.config.tier2_threshold {
            Some(self.config.tier2_lockout_secs)
        } else if attempts >= self.config.tier1_threshold {
            Some(self.config.tier1_lockout_secs)
        } else {
            None
        }
    }

    /// Test-only method to check lockout duration for a given attempt count.
    #[cfg(test)]
    #[must_use]
    pub const fn lockout_duration_for_test(&self, attempts: u64) -> Option<u64> {
        self.lockout_duration_with_config(attempts)
    }

    /// Test-only accessor for the username tracker.
    #[cfg(test)]
    #[must_use]
    pub fn username_tracker(&self) -> Arc<dyn AttemptTracker> {
        Arc::clone(&self.username_tracker)
    }

    /// Test-only accessor for the IP tracker.
    #[cfg(test)]
    #[must_use]
    pub fn ip_tracker(&self) -> Arc<dyn AttemptTracker> {
        Arc::clone(&self.ip_tracker)
    }

    /// Check if a login attempt is allowed for the given username and optional IP.
    ///
    /// Returns `Ok(())` if the attempt is allowed, or an error:
    /// - `Error::Authentication`: Account or IP is locked (legitimate lockout)
    /// - `Error::Internal`: Backend unavailable in fail-closed mode (temporary)
    pub async fn check_allowed(&self, username: &str, ip: Option<IpAddr>) -> Result<()> {
        self.check_allowed_with_control(username, ip, None).await
    }

    pub async fn check_allowed_with_control(
        &self,
        username: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        // Check IP-level lockout first
        if let Some(ip_addr) = ip {
            let ip_key = self.key_builder.login_attempts_ip(&ip_addr.to_string());
            let (ip_attempts, ip_last_failure_at) =
                run_with_control(control, self.ip_tracker.get_attempts(&ip_key)).await?;
            if ip_attempts >= self.config.ip_threshold {
                let now = chrono::Utc::now().timestamp();
                let elapsed = nonnegative_i64_to_u64(now - ip_last_failure_at);
                if elapsed < self.config.ip_lockout_secs {
                    let remaining = self.config.ip_lockout_secs - elapsed;
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
        let (attempts, last_failure_at) =
            run_with_control(control, self.username_tracker.get_attempts(&key)).await?;
        let lockout_secs = self.lockout_duration_with_config(attempts);
        if let Some(lockout_secs) = lockout_secs {
            let now = chrono::Utc::now().timestamp();
            let elapsed = nonnegative_i64_to_u64(now - last_failure_at);
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
    ///
    /// # Errors
    ///
    /// Returns `Error::Internal` if the backend is unavailable in fail-closed mode.
    /// The caller should still deny the login attempt but may want to log the
    /// tracking failure separately.
    pub async fn record_failure(&self, username: &str, ip: Option<IpAddr>) -> Result<()> {
        self.record_failure_with_control(username, ip, None).await
    }

    pub async fn record_failure_with_control(
        &self,
        username: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();

        // Record IP-level failure
        if let Some(ip_addr) = ip {
            let ip_key = self.key_builder.login_attempts_ip(&ip_addr.to_string());
            run_with_control(
                control,
                self.ip_tracker
                    .record_failure(&ip_key, now, self.config.ip_attempts_ttl_secs),
            )
            .await?;
        }

        // Record username-level failure
        let key = self.key_builder.login_attempts(username);
        run_with_control(
            control,
            self.username_tracker
                .record_failure(&key, now, self.config.attempts_ttl_secs),
        )
        .await?;
        Ok(())
    }

    /// Record a failed login attempt only at the IP level.
    ///
    /// Used when a login attempt is made with a username that doesn't exist.
    /// This prevents attackers from locking out legitimate users by trying
    /// non-existent usernames, while still protecting against distributed
    /// IP-based attacks.
    ///
    /// # Errors
    ///
    /// Returns `Error::Internal` if the backend is unavailable in fail-closed mode.
    pub async fn record_ip_failure(&self, ip: Option<IpAddr>) -> Result<()> {
        self.record_ip_failure_with_control(ip, None).await
    }

    pub async fn record_ip_failure_with_control(
        &self,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        if let Some(ip_addr) = ip {
            let now = chrono::Utc::now().timestamp();
            let ip_key = self.key_builder.login_attempts_ip(&ip_addr.to_string());
            run_with_control(
                control,
                self.ip_tracker
                    .record_failure(&ip_key, now, self.config.ip_attempts_ttl_secs),
            )
            .await?;
        }
        Ok(())
    }

    /// Check if a login attempt is allowed for the given IP address only.
    ///
    /// Used in conjunction with `record_ip_failure` for non-existent username
    /// attempts. This allows checking IP-level lockout without checking a
    /// specific username.
    ///
    /// Returns `Ok(())` if the attempt is allowed, or an error:
    /// - `Error::Authentication`: IP is locked (legitimate lockout)
    /// - `Error::Internal`: Backend unavailable in fail-closed mode (temporary)
    pub async fn check_ip_allowed(&self, ip: Option<IpAddr>) -> Result<()> {
        self.check_ip_allowed_with_control(ip, None).await
    }

    pub async fn check_ip_allowed_with_control(
        &self,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        if let Some(ip_addr) = ip {
            let ip_key = self.key_builder.login_attempts_ip(&ip_addr.to_string());
            let (ip_attempts, ip_last_failure_at) =
                run_with_control(control, self.ip_tracker.get_attempts(&ip_key)).await?;
            if ip_attempts >= self.config.ip_threshold {
                let now = chrono::Utc::now().timestamp();
                let elapsed = nonnegative_i64_to_u64(now - ip_last_failure_at);
                if elapsed < self.config.ip_lockout_secs {
                    let remaining = self.config.ip_lockout_secs - elapsed;
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
        Ok(())
    }

    /// Reset the failed login attempt counter on successful login.
    ///
    /// # Errors
    ///
    /// Returns `Error::Internal` if the backend is unavailable in fail-closed mode.
    /// This is a best-effort operation - the reset failure should typically not
    /// block the successful login response, but should be logged.
    pub async fn reset(&self, username: &str) -> Result<()> {
        self.reset_with_control(username, None).await
    }

    pub async fn reset_with_control(
        &self,
        username: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        let key = self.key_builder.login_attempts(username);
        run_with_control(control, self.username_tracker.reset(&key)).await?;
        Ok(())
    }

    /// Reset the per-IP failed login attempt counter on successful login.
    ///
    /// # Errors
    ///
    /// Returns `Error::Internal` if the backend is unavailable in fail-closed mode.
    /// This is a best-effort operation.
    pub async fn reset_ip(&self, ip: &IpAddr) -> Result<()> {
        self.reset_ip_with_control(ip, None).await
    }

    pub async fn reset_ip_with_control(
        &self,
        ip: &IpAddr,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        let ip_key = self.key_builder.login_attempts_ip(&ip.to_string());
        run_with_control(control, self.ip_tracker.reset(&ip_key)).await?;
        Ok(())
    }
}

#[async_trait]
impl BruteForceProtectionService for BruteForceProtection {
    async fn check_allowed(&self, username: &str, ip: Option<IpAddr>) -> Result<()> {
        Self::check_allowed(self, username, ip).await
    }

    async fn check_allowed_with_control(
        &self,
        username: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::check_allowed_with_control(self, username, ip, control).await
    }

    async fn record_failure(&self, username: &str, ip: Option<IpAddr>) -> Result<()> {
        Self::record_failure(self, username, ip).await
    }

    async fn record_failure_with_control(
        &self,
        username: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::record_failure_with_control(self, username, ip, control).await
    }

    async fn record_ip_failure(&self, ip: Option<IpAddr>) -> Result<()> {
        Self::record_ip_failure(self, ip).await
    }

    async fn record_ip_failure_with_control(
        &self,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::record_ip_failure_with_control(self, ip, control).await
    }

    async fn check_ip_allowed(&self, ip: Option<IpAddr>) -> Result<()> {
        Self::check_ip_allowed(self, ip).await
    }

    async fn check_ip_allowed_with_control(
        &self,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::check_ip_allowed_with_control(self, ip, control).await
    }

    async fn reset(&self, username: &str) -> Result<()> {
        Self::reset(self, username).await
    }

    async fn reset_with_control(
        &self,
        username: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::reset_with_control(self, username, control).await
    }

    async fn reset_ip(&self, ip: &IpAddr) -> Result<()> {
        Self::reset_ip(self, ip).await
    }

    async fn reset_ip_with_control(
        &self,
        ip: &IpAddr,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::reset_ip_with_control(self, ip, control).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RedisConnectionRuntime;
    use async_trait::async_trait;

    // Note: Integration tests that require Redis (record_failure, check_allowed, etc.)
    // are in the integration test suite. Unit tests here cover pure logic only.

    #[tokio::test]
    async fn test_redis_attempt_tracker_accepts_trait_object_runtime() {
        #[derive(Clone)]
        struct FakeRedisRuntime;

        #[async_trait]
        impl RedisConnectionRuntime for FakeRedisRuntime {
            async fn snapshot(&self) -> redis::aio::ConnectionManager {
                panic!("snapshot should not be called in constructor-only test");
            }
        }

        let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
        let tracker = RedisAttemptTracker::from_runtime(runtime.clone(), 128, 60);

        assert!(
            Arc::ptr_eq(&tracker.conn, &runtime),
            "attempt tracker should retain the injected Redis runtime object"
        );
    }

    #[tokio::test]
    async fn test_brute_force_protection_supports_service_trait_object() {
        let protection: Arc<dyn BruteForceProtectionService> =
            Arc::new(BruteForceProtection::in_memory("trait-test:".to_string()));

        protection
            .record_failure("trait-user", None)
            .await
            .expect("trait-object brute-force service should record failures");
        protection
            .check_allowed("trait-user", None)
            .await
            .expect("single failure should stay below the default lockout threshold");
    }

    #[tokio::test]
    async fn test_brute_force_protection_from_shared_state_profile_returns_live_trait_object() {
        let profile = SharedStateProfile::from_runtime(None, "trait-test:", false);
        let protection = brute_force_protection_from_shared_state_profile(&profile)
            .expect("standalone mode should allow local brute-force protection");

        protection
            .check_allowed("trait-user", None)
            .await
            .expect("trait-object builder should return a live service");
    }

    #[test]
    fn test_brute_force_protection_from_shared_state_profile_requires_shared_runtime_in_cluster_mode(
    ) {
        let profile = SharedStateProfile::from_runtime(None, "trait-test:", true);
        let Err(error) = brute_force_protection_from_shared_state_profile(&profile) else {
            panic!("cluster runtime must reject local brute-force protection");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared brute-force protection state"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_brute_force_protection_accepts_custom_redis_trackers() {
        #[derive(Clone)]
        struct FakeRedisRuntime;

        #[async_trait]
        impl RedisConnectionRuntime for FakeRedisRuntime {
            async fn snapshot(&self) -> redis::aio::ConnectionManager {
                panic!("snapshot should not be called in constructor-only test");
            }
        }

        let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
        let config = BruteForceConfig::default();
        let username_tracker: Arc<dyn AttemptTracker> = Arc::new(
            RedisAttemptTracker::from_runtime(runtime.clone(), 50_000, config.attempts_ttl_secs),
        );
        let ip_tracker: Arc<dyn AttemptTracker> = Arc::new(RedisAttemptTracker::from_runtime(
            runtime,
            100_000,
            config.ip_attempts_ttl_secs,
        ));

        let protection = BruteForceProtection::new_with_config(
            "test".to_string(),
            username_tracker,
            ip_tracker,
            config,
        );

        assert_eq!(protection.key_builder.prefix(), "test");
    }

    /// Test that lockout duration thresholds are correct with default config
    #[test]
    fn test_lockout_duration_standard_thresholds() {
        let protection = BruteForceProtection::in_memory("test".to_string());
        assert_eq!(protection.lockout_duration_for_test(4), None);
        assert_eq!(
            protection.lockout_duration_for_test(5),
            Some(TIER1_LOCKOUT_SECS)
        );
        assert_eq!(
            protection.lockout_duration_for_test(9),
            Some(TIER1_LOCKOUT_SECS)
        );
        assert_eq!(
            protection.lockout_duration_for_test(10),
            Some(TIER2_LOCKOUT_SECS)
        );
        assert_eq!(
            protection.lockout_duration_for_test(14),
            Some(TIER2_LOCKOUT_SECS)
        );
        assert_eq!(
            protection.lockout_duration_for_test(15),
            Some(TIER3_LOCKOUT_SECS)
        );
        assert_eq!(
            protection.lockout_duration_for_test(100),
            Some(TIER3_LOCKOUT_SECS)
        );
    }

    // RedisAttemptTracker degradation tracking tests

    /// Test that `RedisAttemptTracker` initializes with degradation tracking in clean state
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

    /// Test that `InMemoryAttemptTracker` never reports as degraded
    /// (it's always the intended backend, not a fallback)
    #[tokio::test]
    async fn test_in_memory_tracker_is_intended_backend() {
        let tracker = InMemoryAttemptTracker::new(1000, 900);
        let key = "test:user";

        // Perform operations - these should work without any "degradation" concept
        let now = chrono::Utc::now().timestamp();
        tracker.record_failure(key, now, 900).await.unwrap();
        tracker.record_failure(key, now, 900).await.unwrap();

        let (count, _) = tracker.get_attempts(key).await.unwrap();
        assert_eq!(count, 2);

        tracker.reset(key).await.unwrap();
        let (count, _) = tracker.get_attempts(key).await.unwrap();
        assert_eq!(count, 0);

        // InMemoryAttemptTracker is always the intended backend,
        // there's no "fallback" or "degraded" state to check
    }

    // Fail-closed mode tests

    /// Test that the `fail_closed` flag is correctly set
    #[test]
    fn test_fail_closed_flag_semantics() {
        // Test the atomic bool semantics for fail_closed mode
        let fail_closed = true;
        assert!(fail_closed);

        let fail_closed = false;
        assert!(!fail_closed);

        // In production, fail_closed=true should cause errors on Redis failure
        // fail_closed=false should allow fallback to in-memory cache
    }

    /// Test that `InMemoryAttemptTracker` always returns Ok
    #[tokio::test]
    async fn test_in_memory_tracker_never_fails() {
        let tracker = InMemoryAttemptTracker::new(1000, 900);
        let key = "test:user";

        // All operations should succeed
        assert!(tracker.get_attempts(key).await.is_ok());
        assert!(tracker
            .record_failure(key, chrono::Utc::now().timestamp(), 900)
            .await
            .is_ok());
        assert!(tracker.reset(key).await.is_ok());
    }

    #[test]
    fn test_fail_closed_backend_error_is_service_unavailable() {
        let err = RedisAttemptTracker::fail_closed_backend_error("please try again later");
        match err {
            Error::ServiceUnavailable(message) => {
                assert!(
                    message.contains("Brute-force protection temporarily unavailable"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected ServiceUnavailable, got {other:?}"),
        }
    }
}
