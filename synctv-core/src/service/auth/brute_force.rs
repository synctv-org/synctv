//! Per-account and per-IP brute-force protection for login attempts.
//!
//! Tracks failed login attempts per username in Redis (with in-memory fallback)
//! and enforces exponential lockout after repeated failures:
//!
//! - 5 failures: 1 minute lockout
//! - 10 failures: 5 minute lockout
//! - 15+ failures: 15 minute lockout
//!
//! Additionally tracks per-IP failures: 20 failures from a single IP within
//! 10 minutes triggers an IP-level lockout (10 minutes), preventing distributed
//! username enumeration attacks.
//!
//! The counter auto-expires after 15 minutes of no new failures (Redis TTL).
//! A successful login resets the username counter to zero.

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::{cache::KeyBuilder, resilience::timeout::REDIS_OPERATION_TIMEOUT, Error, Result, InternalExt};

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

/// TTL for the failed attempts counter in Redis (15 minutes).
/// After 15 minutes of inactivity, the counter resets automatically.
const ATTEMPTS_TTL_SECS: u64 = 900;

/// Per-IP failure threshold: 20 failures from the same IP triggers lockout.
const IP_THRESHOLD: u64 = 20;
/// Per-IP lockout duration (10 minutes).
const IP_LOCKOUT_SECS: u64 = 600;
/// TTL for the per-IP failure counter in Redis (10 minutes).
const IP_ATTEMPTS_TTL_SECS: u64 = 600;

/// Brute-force protection service
#[derive(Clone)]
pub struct BruteForceProtection {
    redis_conn: Option<redis::aio::ConnectionManager>,
    key_builder: KeyBuilder,
    /// In-memory fallback: username -> (`attempt_count`, `last_attempt_timestamp`)
    local_attempts: Arc<moka::future::Cache<String, (u64, i64)>>,
    /// In-memory fallback for per-IP tracking: ip -> (`attempt_count`, `last_attempt_timestamp`)
    local_ip_attempts: Arc<moka::future::Cache<String, (u64, i64)>>,
    /// Multiplier applied to lockout thresholds when using in-memory fallback.
    ///
    /// When Redis is unavailable, each replica maintains an independent counter.
    /// An attacker can distribute requests across N replicas, effectively getting
    /// N * threshold attempts before any single replica triggers lockout. To
    /// compensate, thresholds are reduced by this factor in fallback mode.
    ///
    /// Default: 0.34 (~1/3), assuming up to 3 replicas.
    fallback_threshold_multiplier: f64,
}

impl std::fmt::Debug for BruteForceProtection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BruteForceProtection")
            .field("uses_redis", &self.redis_conn.is_some())
            .finish()
    }
}

impl BruteForceProtection {
    /// Default fallback threshold multiplier (~1/3 for 3 replicas).
    const DEFAULT_FALLBACK_THRESHOLD_MULTIPLIER: f64 = 0.34;

    /// Create a new brute-force protection service.
    ///
    /// Falls back to in-memory tracking when Redis is not available.
    /// In fallback mode, thresholds are reduced by `DEFAULT_FALLBACK_THRESHOLD_MULTIPLIER`
    /// to compensate for per-replica independent counting.
    #[must_use]
    pub fn new(redis_conn: Option<redis::aio::ConnectionManager>, key_prefix: String) -> Self {
        Self {
            redis_conn,
            key_builder: KeyBuilder::new(key_prefix),
            local_attempts: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(50_000)
                    .time_to_live(Duration::from_secs(ATTEMPTS_TTL_SECS))
                    .build(),
            ),
            local_ip_attempts: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(100_000)
                    .time_to_live(Duration::from_secs(IP_ATTEMPTS_TTL_SECS))
                    .build(),
            ),
            fallback_threshold_multiplier: Self::DEFAULT_FALLBACK_THRESHOLD_MULTIPLIER,
        }
    }

    /// Check if a login attempt is allowed for the given username and optional IP.
    ///
    /// Returns `Ok(())` if the attempt is allowed, or an authentication error
    /// with the remaining lockout duration if the account or IP is locked.
    ///
    /// The lockout check compares the elapsed time since the last failure against
    /// the lockout duration for the current tier. Once the lockout period has
    /// elapsed, the attempt is allowed even though the failure counter is preserved
    /// (so tier escalation still works on the next failure).
    pub async fn check_allowed(&self, username: &str, ip: Option<IpAddr>) -> Result<()> {
        // Check IP-level lockout first
        if let Some(ip_addr) = ip {
            let (ip_attempts, ip_last_failure_at, ip_is_fallback) = self.get_ip_attempts(&ip_addr).await?;
            // Apply reduced threshold when using in-memory fallback to compensate
            // for per-replica independent counting in multi-replica deployments.
            let effective_ip_threshold = if ip_is_fallback {
                ((IP_THRESHOLD as f64) * self.fallback_threshold_multiplier).ceil().max(1.0) as u64
            } else {
                IP_THRESHOLD
            };
            if ip_attempts >= effective_ip_threshold {
                let now = chrono::Utc::now().timestamp();
                let elapsed = (now - ip_last_failure_at).max(0) as u64;
                if elapsed < IP_LOCKOUT_SECS {
                    let remaining = IP_LOCKOUT_SECS - elapsed;
                    tracing::warn!(
                        ip = %ip_addr,
                        attempts = ip_attempts,
                        threshold = effective_ip_threshold,
                        is_fallback = ip_is_fallback,
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
        let (attempts, last_failure_at, is_fallback) = self.get_attempts(username).await?;
        // Use reduced thresholds in fallback mode to maintain effective protection
        // across multiple replicas with independent counters.
        let lockout_secs = if is_fallback {
            Self::lockout_duration_fallback(attempts, self.fallback_threshold_multiplier)
        } else {
            Self::lockout_duration(attempts)
        };
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
                    is_fallback = is_fallback,
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
    /// last-failure timestamp, and sets/refreshes the Redis TTL.
    /// Also records the failure against the IP address if provided.
    pub async fn record_failure(&self, username: &str, ip: Option<IpAddr>) -> Result<()> {
        // Record IP-level failure
        if let Some(ip_addr) = ip {
            self.record_ip_failure(&ip_addr).await?;
        }
        self.record_username_failure(username).await
    }

    /// Record a failed login attempt for a specific username.
    async fn record_username_failure(&self, username: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let key = self.key_builder.login_attempts(username);

            // Read current state, increment, write back with timestamp.
            // Uses a Lua script for atomicity so concurrent failures don't
            // lose updates.
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
                    .key(&key)
                    .arg(now)
                    .arg(ATTEMPTS_TTL_SECS as i64)
                    .invoke_async(&mut conn),
            )
                .await
                .unwrap_or_else(|_| Err(redis::RedisError::from((
                    redis::ErrorKind::Io,
                    "Redis timeout: record_failure",
                ))));

            match result {
                Ok(count) => {
                    tracing::debug!(
                        username = %username,
                        attempts = count,
                        "Recorded failed login attempt"
                    );
                }
                Err(e) => {
                    // Degrade to in-memory fallback on Redis error (fail-closed:
                    // we still record the attempt so brute-force protection
                    // remains active during Redis outages)
                    tracing::warn!(
                        username = %username,
                        error = %e,
                        "Redis error in record_failure, falling back to in-memory tracking"
                    );
                    let (count, _) = self.local_attempts.get(&key).await.unwrap_or((0, now));
                    self.local_attempts.insert(key, (count + 1, now)).await;
                }
            }
        } else {
            // In-memory fallback
            let key = self.key_builder.login_attempts(username);
            let (count, _) = self.local_attempts.get(&key).await.unwrap_or((0, now));
            self.local_attempts.insert(key, (count + 1, now)).await;
        }

        Ok(())
    }

    /// Reset the failed login attempt counter on successful login.
    pub async fn reset(&self, username: &str) -> Result<()> {
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let key = self.key_builder.login_attempts(username);

            let _: () = tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.del(&key))
                .await
                .map_err(|_| Error::Internal("Redis timeout: reset login attempts".to_string()))?
                .internal_with_err("Failed to reset login attempts")?;
        } else {
            let key = self.key_builder.login_attempts(username);
            self.local_attempts.invalidate(&key).await;
        }

        Ok(())
    }

    /// Record a failed login attempt for a specific IP address.
    async fn record_ip_failure(&self, ip: &IpAddr) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let ip_str = ip.to_string();

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let key = self.key_builder.login_attempts_ip(&ip_str);

            let script = redis::Script::new(
                r"
                local raw = redis.call('GET', KEYS[1])
                local count = 0
                if raw then
                    local ok, state = pcall(cjson.decode, raw)
                    if ok and state and state.count then
                        count = tonumber(state.count) or 0
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
                    .key(&key)
                    .arg(now)
                    .arg(IP_ATTEMPTS_TTL_SECS as i64)
                    .invoke_async(&mut conn),
            )
                .await
                .unwrap_or_else(|_| Err(redis::RedisError::from((
                    redis::ErrorKind::Io,
                    "Redis timeout: record_ip_failure",
                ))));

            match result {
                Ok(count) => {
                    tracing::debug!(
                        ip = %ip,
                        attempts = count,
                        "Recorded failed login attempt for IP"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        ip = %ip,
                        error = %e,
                        "Redis error in record_ip_failure, falling back to in-memory tracking"
                    );
                    let (count, _) = self.local_ip_attempts.get(&key).await.unwrap_or((0, now));
                    self.local_ip_attempts.insert(key, (count + 1, now)).await;
                }
            }
        } else {
            let key = self.key_builder.login_attempts_ip(&ip_str);
            let (count, _) = self.local_ip_attempts.get(&key).await.unwrap_or((0, now));
            self.local_ip_attempts.insert(key, (count + 1, now)).await;
        }

        Ok(())
    }

    /// Get the current failed attempt count and last-failure timestamp for an IP address.
    ///
    /// Returns `(count, last_failure_at, is_fallback)` where `is_fallback` indicates
    /// whether the data came from in-memory cache (true) or Redis (false).
    async fn get_ip_attempts(&self, ip: &IpAddr) -> Result<(u64, i64, bool)> {
        let ip_str = ip.to_string();
        let key = self.key_builder.login_attempts_ip(&ip_str);

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let redis_result = tokio::time::timeout(
                REDIS_OPERATION_TIMEOUT,
                conn.get::<_, Option<String>>(&key),
            )
                .await;

            let redis_result = if let Ok(inner) = redis_result { inner } else {
                tracing::warn!(
                    ip = %ip,
                    "Redis timeout in IP brute-force check, falling back to in-memory cache"
                );
                let (count, ts) = self.local_ip_attempts.get(&key).await.unwrap_or((0, 0));
                return Ok((count, ts, true));
            };

            match redis_result {
                Ok(Some(raw)) => {
                    if let Ok(state) = serde_json::from_str::<BruteForceState>(&raw) {
                        return Ok((state.count, state.last_failure_at, false));
                    }
                    return Ok((0, 0, false));
                }
                Ok(None) => return Ok((0, 0, false)),
                Err(e) => {
                    tracing::warn!(
                        ip = %ip,
                        error = %e,
                        "Redis error in IP brute-force check, falling back to in-memory cache"
                    );
                }
            }
        }

        let (count, ts) = self.local_ip_attempts.get(&key).await.unwrap_or((0, 0));
        Ok((count, ts, true))
    }

    /// Get the current failed attempt count and last-failure timestamp for a username.
    ///
    /// Returns `(count, last_failure_at, is_fallback)` where `last_failure_at` is a Unix
    /// timestamp and `is_fallback` indicates whether the data came from in-memory cache
    /// (true) or Redis (false).
    ///
    /// On Redis error, falls back to the in-memory cache rather than returning 0
    /// (fail-closed). Returning 0 on error would disable brute-force protection
    /// entirely, allowing unlimited login attempts during Redis outages.
    async fn get_attempts(&self, username: &str) -> Result<(u64, i64, bool)> {
        let key = self.key_builder.login_attempts(username);

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let redis_result = tokio::time::timeout(
                REDIS_OPERATION_TIMEOUT,
                conn.get::<_, Option<String>>(&key),
            )
                .await;

            // Flatten timeout into a Redis-style error for unified fallback handling
            let redis_result = if let Ok(inner) = redis_result { inner } else {
                tracing::warn!(
                    username = %username,
                    "Redis timeout in brute-force check, falling back to in-memory cache"
                );
                // Fall through to in-memory lookup below
                let (count, ts) = self.local_attempts.get(&key).await.unwrap_or((0, 0));
                return Ok((count, ts, true));
            };

            match redis_result {
                Ok(Some(raw)) => {
                    // Try parsing as JSON state first, fall back to plain integer
                    // for backward compatibility with pre-existing counters.
                    if let Ok(state) = serde_json::from_str::<BruteForceState>(&raw) {
                        return Ok((state.count, state.last_failure_at, false));
                    }
                    // Legacy plain integer format (no timestamp available)
                    if let Ok(count) = raw.parse::<u64>() {
                        return Ok((count, 0, false));
                    }
                    return Ok((0, 0, false));
                }
                Ok(None) => return Ok((0, 0, false)),
                Err(e) => {
                    tracing::warn!(
                        username = %username,
                        error = %e,
                        "Redis error in brute-force check, falling back to in-memory cache"
                    );
                    // Fall through to in-memory lookup below
                }
            }
        }

        // In-memory fallback (used when Redis is unavailable or errored)
        let (count, ts) = self.local_attempts.get(&key).await.unwrap_or((0, 0));
        Ok((count, ts, true))
    }

    /// Determine lockout duration based on failure count (using standard Redis thresholds).
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

    /// Determine lockout duration with adjusted thresholds for in-memory fallback mode.
    ///
    /// When Redis is unavailable, each replica maintains an independent counter.
    /// Thresholds are reduced by `fallback_threshold_multiplier` so the effective
    /// per-cluster threshold stays close to the intended value.
    fn lockout_duration_fallback(attempts: u64, multiplier: f64) -> Option<u64> {
        let t1 = ((TIER1_THRESHOLD as f64) * multiplier).ceil().max(1.0) as u64;
        let t2 = ((TIER2_THRESHOLD as f64) * multiplier).ceil().max((t1 + 1) as f64) as u64;
        let t3 = ((TIER3_THRESHOLD as f64) * multiplier).ceil().max((t2 + 1) as f64) as u64;

        if attempts >= t3 {
            Some(TIER3_LOCKOUT_SECS)
        } else if attempts >= t2 {
            Some(TIER2_LOCKOUT_SECS)
        } else if attempts >= t1 {
            Some(TIER1_LOCKOUT_SECS)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: lockout_duration tests have been consolidated into
    // test_lockout_duration_standard_thresholds and test_lockout_duration_fallback_thresholds
    // at the bottom of this test module.

    #[tokio::test]
    async fn test_in_memory_record_and_check() {
        let service = BruteForceProtection::new(None, "test".to_string());

        // In-memory mode uses fallback thresholds (multiplier = 0.34):
        // Tier 1: ceil(5 * 0.34) = 2

        // Initially allowed
        assert!(service.check_allowed("testuser", None).await.is_ok());

        // Record 1 failure - still allowed (threshold is 2)
        service.record_failure("testuser", None).await.unwrap();
        assert!(service.check_allowed("testuser", None).await.is_ok());

        // 2nd failure triggers lockout (fallback tier 1 threshold = 2)
        service.record_failure("testuser", None).await.unwrap();
        assert!(service.check_allowed("testuser", None).await.is_err());
    }

    #[tokio::test]
    async fn test_in_memory_reset() {
        let service = BruteForceProtection::new(None, "test".to_string());

        // Record enough failures to trigger lockout (fallback tier 1 threshold = 2)
        for _ in 0..2 {
            service.record_failure("testuser", None).await.unwrap();
        }
        assert!(service.check_allowed("testuser", None).await.is_err());

        // Reset on successful login
        service.reset("testuser").await.unwrap();
        assert!(service.check_allowed("testuser", None).await.is_ok());
    }

    #[tokio::test]
    async fn test_in_memory_independent_users() {
        let service = BruteForceProtection::new(None, "test".to_string());

        // Lock out user_a (fallback tier 1 threshold = 2)
        for _ in 0..2 {
            service.record_failure("user_a", None).await.unwrap();
        }
        assert!(service.check_allowed("user_a", None).await.is_err());

        // user_b should be unaffected
        assert!(service.check_allowed("user_b", None).await.is_ok());
    }

    #[tokio::test]
    async fn test_in_memory_tier_escalation() {
        let service = BruteForceProtection::new(None, "test".to_string());

        // Fallback thresholds (multiplier = 0.34):
        // Tier 1: ceil(5 * 0.34)  = 2
        // Tier 2: max(ceil(10 * 0.34), 2+1) = max(4, 3) = 4
        // Tier 3: max(ceil(15 * 0.34), 4+1) = max(6, 5) = 6

        // 2 failures -> tier 1 lockout (60s)
        for _ in 0..2 {
            service.record_failure("testuser", None).await.unwrap();
        }
        let err = service.check_allowed("testuser", None).await.unwrap_err();
        assert!(err.to_string().contains("60 seconds"));

        // 2 more failures (total 4) -> tier 2 lockout (300s)
        for _ in 0..2 {
            service.record_failure("testuser", None).await.unwrap();
        }
        let err = service.check_allowed("testuser", None).await.unwrap_err();
        assert!(err.to_string().contains("300 seconds"));

        // 2 more failures (total 6) -> tier 3 lockout (900s)
        for _ in 0..2 {
            service.record_failure("testuser", None).await.unwrap();
        }
        let err = service.check_allowed("testuser", None).await.unwrap_err();
        assert!(err.to_string().contains("900 seconds"));
    }

    #[tokio::test]
    async fn test_ip_lockout() {
        let service = BruteForceProtection::new(None, "test".to_string());
        let ip: IpAddr = "192.168.1.100".parse().unwrap();

        // Fallback IP threshold: ceil(20 * 0.34) = 7
        // Record 6 failures from the same IP across different usernames - still allowed
        for i in 0..6 {
            service.record_failure(&format!("user_{i}"), Some(ip)).await.unwrap();
        }
        assert!(service.check_allowed("any_user", Some(ip)).await.is_ok());

        // 7th failure triggers IP lockout
        service.record_failure("user_6", Some(ip)).await.unwrap();
        assert!(service.check_allowed("any_user", Some(ip)).await.is_err());

        // Different IP should still be allowed
        let other_ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(service.check_allowed("any_user", Some(other_ip)).await.is_ok());
    }

    /// Test that standard Redis thresholds are still correct (used when Redis is available)
    #[test]
    fn test_lockout_duration_standard_thresholds() {
        // Standard thresholds are unchanged
        assert_eq!(BruteForceProtection::lockout_duration(4), None);
        assert_eq!(BruteForceProtection::lockout_duration(5), Some(TIER1_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration(9), Some(TIER1_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration(10), Some(TIER2_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration(14), Some(TIER2_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration(15), Some(TIER3_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration(100), Some(TIER3_LOCKOUT_SECS));
    }

    /// Test that fallback thresholds are reduced by the multiplier
    #[test]
    fn test_lockout_duration_fallback_thresholds() {
        let m = BruteForceProtection::DEFAULT_FALLBACK_THRESHOLD_MULTIPLIER;

        // Fallback thresholds: ceil(5*0.34)=2, max(ceil(10*0.34),3)=4, max(ceil(15*0.34),5)=6
        assert_eq!(BruteForceProtection::lockout_duration_fallback(1, m), None);
        assert_eq!(BruteForceProtection::lockout_duration_fallback(2, m), Some(TIER1_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration_fallback(3, m), Some(TIER1_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration_fallback(4, m), Some(TIER2_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration_fallback(5, m), Some(TIER2_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration_fallback(6, m), Some(TIER3_LOCKOUT_SECS));
        assert_eq!(BruteForceProtection::lockout_duration_fallback(100, m), Some(TIER3_LOCKOUT_SECS));
    }
}
