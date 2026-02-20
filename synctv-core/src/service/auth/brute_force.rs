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

/// Brute-force protection service (Redis-backed)
#[derive(Clone)]
pub struct BruteForceProtection {
    redis_conn: redis::aio::ConnectionManager,
    key_builder: KeyBuilder,
    /// In-memory fallback: username -> (`attempt_count`, `last_attempt_timestamp`)
    /// Used as fail-closed fallback when Redis operations time out or error.
    local_attempts: Arc<moka::future::Cache<String, (u64, i64)>>,
    /// In-memory fallback for per-IP tracking: ip -> (`attempt_count`, `last_attempt_timestamp`)
    /// Used as fail-closed fallback when Redis operations time out or error.
    local_ip_attempts: Arc<moka::future::Cache<String, (u64, i64)>>,
}

impl std::fmt::Debug for BruteForceProtection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BruteForceProtection")
            .finish()
    }
}

impl BruteForceProtection {
    /// Create a new brute-force protection service (Redis-backed).
    ///
    /// In-memory caches are used as fail-closed fallback when Redis operations
    /// time out or error, ensuring brute-force protection remains active.
    #[must_use]
    pub fn new(redis_conn: redis::aio::ConnectionManager, key_prefix: String) -> Self {
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
            let (ip_attempts, ip_last_failure_at) = self.get_ip_attempts(&ip_addr).await?;
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
        let (attempts, last_failure_at) = self.get_attempts(username).await?;
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
        let mut conn = self.redis_conn.clone();
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
                // remains active during transient Redis issues)
                tracing::warn!(
                    username = %username,
                    error = %e,
                    "Redis error in record_failure, falling back to in-memory tracking"
                );
                let (count, _) = self.local_attempts.get(&key).await.unwrap_or((0, now));
                self.local_attempts.insert(key, (count + 1, now)).await;
            }
        }

        Ok(())
    }

    /// Reset the failed login attempt counter on successful login.
    pub async fn reset(&self, username: &str) -> Result<()> {
        let mut conn = self.redis_conn.clone();
        let key = self.key_builder.login_attempts(username);

        let _: () = tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.del(&key))
            .await
            .map_err(|_| Error::Internal("Redis timeout: reset login attempts".to_string()))?
            .internal_with_err("Failed to reset login attempts")?;

        Ok(())
    }

    /// Reset the per-IP failed login attempt counter on successful login.
    ///
    /// This prevents shared IPs (e.g., behind NAT/VPN) from accumulating
    /// failures across different users and eventually locking out the IP.
    pub async fn reset_ip(&self, ip: &IpAddr) -> Result<()> {
        let ip_str = ip.to_string();
        let key = self.key_builder.login_attempts_ip(&ip_str);

        let mut conn = self.redis_conn.clone();
        let _: () = tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.del(&key))
            .await
            .map_err(|_| Error::Internal("Redis timeout: reset IP login attempts".to_string()))?
            .internal_with_err("Failed to reset IP login attempts")?;

        Ok(())
    }

    /// Record a failed login attempt for a specific IP address.
    async fn record_ip_failure(&self, ip: &IpAddr) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let ip_str = ip.to_string();
        let mut conn = self.redis_conn.clone();
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

        Ok(())
    }

    /// Get the current failed attempt count and last-failure timestamp for an IP address.
    ///
    /// Returns `(count, last_failure_at)`. Falls back to in-memory cache on Redis errors.
    async fn get_ip_attempts(&self, ip: &IpAddr) -> Result<(u64, i64)> {
        let ip_str = ip.to_string();
        let key = self.key_builder.login_attempts_ip(&ip_str);
        let mut conn = self.redis_conn.clone();

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
            return Ok((count, ts));
        };

        match redis_result {
            Ok(Some(raw)) => {
                if let Ok(state) = serde_json::from_str::<BruteForceState>(&raw) {
                    return Ok((state.count, state.last_failure_at));
                }
                Ok((0, 0))
            }
            Ok(None) => Ok((0, 0)),
            Err(e) => {
                tracing::warn!(
                    ip = %ip,
                    error = %e,
                    "Redis error in IP brute-force check, falling back to in-memory cache"
                );
                let (count, ts) = self.local_ip_attempts.get(&key).await.unwrap_or((0, 0));
                Ok((count, ts))
            }
        }
    }

    /// Get the current failed attempt count and last-failure timestamp for a username.
    ///
    /// Returns `(count, last_failure_at)` where `last_failure_at` is a Unix timestamp.
    ///
    /// On Redis error, falls back to the in-memory cache rather than returning 0
    /// (fail-closed). Returning 0 on error would disable brute-force protection
    /// entirely, allowing unlimited login attempts during transient Redis issues.
    async fn get_attempts(&self, username: &str) -> Result<(u64, i64)> {
        let key = self.key_builder.login_attempts(username);
        let mut conn = self.redis_conn.clone();

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
            let (count, ts) = self.local_attempts.get(&key).await.unwrap_or((0, 0));
            return Ok((count, ts));
        };

        match redis_result {
            Ok(Some(raw)) => {
                // Try parsing as JSON state first, fall back to plain integer
                // for backward compatibility with pre-existing counters.
                if let Ok(state) = serde_json::from_str::<BruteForceState>(&raw) {
                    return Ok((state.count, state.last_failure_at));
                }
                // Legacy plain integer format (no timestamp available)
                if let Ok(count) = raw.parse::<u64>() {
                    return Ok((count, 0));
                }
                Ok((0, 0))
            }
            Ok(None) => Ok((0, 0)),
            Err(e) => {
                tracing::warn!(
                    username = %username,
                    error = %e,
                    "Redis error in brute-force check, falling back to in-memory cache"
                );
                let (count, ts) = self.local_attempts.get(&key).await.unwrap_or((0, 0));
                Ok((count, ts))
            }
        }
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
}
