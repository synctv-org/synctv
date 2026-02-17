//! Per-account brute-force protection for login attempts.
//!
//! Tracks failed login attempts per username in Redis (with in-memory fallback)
//! and enforces exponential lockout after repeated failures:
//!
//! - 5 failures: 1 minute lockout
//! - 10 failures: 5 minute lockout
//! - 15+ failures: 15 minute lockout
//!
//! The counter auto-expires after 15 minutes of no new failures (Redis TTL).
//! A successful login resets the counter to zero.

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::{cache::KeyBuilder, Error, Result, InternalExt};

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

/// Brute-force protection service
#[derive(Clone)]
pub struct BruteForceProtection {
    redis_conn: Option<redis::aio::ConnectionManager>,
    key_builder: KeyBuilder,
    /// In-memory fallback: username -> (attempt_count, last_attempt_timestamp)
    local_attempts: Arc<moka::future::Cache<String, (u64, i64)>>,
}

impl std::fmt::Debug for BruteForceProtection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BruteForceProtection")
            .field("uses_redis", &self.redis_conn.is_some())
            .finish()
    }
}

impl BruteForceProtection {
    /// Create a new brute-force protection service.
    ///
    /// Falls back to in-memory tracking when Redis is not available.
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
        }
    }

    /// Check if a login attempt is allowed for the given username.
    ///
    /// Returns `Ok(())` if the attempt is allowed, or an authentication error
    /// with the remaining lockout duration if the account is locked.
    ///
    /// The lockout check compares the elapsed time since the last failure against
    /// the lockout duration for the current tier. Once the lockout period has
    /// elapsed, the attempt is allowed even though the failure counter is preserved
    /// (so tier escalation still works on the next failure).
    pub async fn check_allowed(&self, username: &str) -> Result<()> {
        let (attempts, last_failure_at) = self.get_attempts(username).await?;
        if let Some(lockout_secs) = Self::lockout_duration(attempts) {
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
                    "Too many failed login attempts. Please try again in {} seconds.",
                    remaining
                )));
            }
        }
        Ok(())
    }

    /// Record a failed login attempt. Increments the counter, stores the
    /// last-failure timestamp, and sets/refreshes the Redis TTL.
    pub async fn record_failure(&self, username: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let key = self.key_builder.login_attempts(username);

            // Read current state, increment, write back with timestamp.
            // Uses a Lua script for atomicity so concurrent failures don't
            // lose updates.
            let script = redis::Script::new(
                r#"
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
                "#,
            );

            let count: u64 = script
                .key(&key)
                .arg(now)
                .arg(ATTEMPTS_TTL_SECS as i64)
                .invoke_async(&mut conn)
                .await
                .internal_with_err("Failed to record login failure")?;

            tracing::debug!(
                username = %username,
                attempts = count,
                "Recorded failed login attempt"
            );
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

            let _: () = conn.del(&key)
                .await
                .internal_with_err("Failed to reset login attempts")?;
        } else {
            let key = self.key_builder.login_attempts(username);
            self.local_attempts.invalidate(&key).await;
        }

        Ok(())
    }

    /// Get the current failed attempt count and last-failure timestamp for a username.
    ///
    /// Returns `(count, last_failure_at)` where `last_failure_at` is a Unix timestamp.
    /// On Redis error, falls back to the in-memory cache rather than returning 0
    /// (fail-closed). Returning 0 on error would disable brute-force protection
    /// entirely, allowing unlimited login attempts during Redis outages.
    async fn get_attempts(&self, username: &str) -> Result<(u64, i64)> {
        let key = self.key_builder.login_attempts(username);

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            match conn.get::<_, Option<String>>(&key).await {
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
                    return Ok((0, 0));
                }
                Ok(None) => return Ok((0, 0)),
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
        Ok((count, ts))
    }

    /// Determine lockout duration based on failure count.
    ///
    /// Returns `Some(seconds)` if locked out, `None` if allowed.
    fn lockout_duration(attempts: u64) -> Option<u64> {
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

    #[test]
    fn test_lockout_duration_no_lockout() {
        assert_eq!(BruteForceProtection::lockout_duration(0), None);
        assert_eq!(BruteForceProtection::lockout_duration(1), None);
        assert_eq!(BruteForceProtection::lockout_duration(4), None);
    }

    #[test]
    fn test_lockout_duration_tier1() {
        assert_eq!(BruteForceProtection::lockout_duration(5), Some(60));
        assert_eq!(BruteForceProtection::lockout_duration(9), Some(60));
    }

    #[test]
    fn test_lockout_duration_tier2() {
        assert_eq!(BruteForceProtection::lockout_duration(10), Some(300));
        assert_eq!(BruteForceProtection::lockout_duration(14), Some(300));
    }

    #[test]
    fn test_lockout_duration_tier3() {
        assert_eq!(BruteForceProtection::lockout_duration(15), Some(900));
        assert_eq!(BruteForceProtection::lockout_duration(100), Some(900));
    }

    #[tokio::test]
    async fn test_in_memory_record_and_check() {
        let service = BruteForceProtection::new(None, "test".to_string());

        // Initially allowed
        assert!(service.check_allowed("testuser").await.is_ok());

        // Record 4 failures - still allowed
        for _ in 0..4 {
            service.record_failure("testuser").await.unwrap();
        }
        assert!(service.check_allowed("testuser").await.is_ok());

        // 5th failure triggers lockout
        service.record_failure("testuser").await.unwrap();
        assert!(service.check_allowed("testuser").await.is_err());
    }

    #[tokio::test]
    async fn test_in_memory_reset() {
        let service = BruteForceProtection::new(None, "test".to_string());

        // Record 5 failures
        for _ in 0..5 {
            service.record_failure("testuser").await.unwrap();
        }
        assert!(service.check_allowed("testuser").await.is_err());

        // Reset on successful login
        service.reset("testuser").await.unwrap();
        assert!(service.check_allowed("testuser").await.is_ok());
    }

    #[tokio::test]
    async fn test_in_memory_independent_users() {
        let service = BruteForceProtection::new(None, "test".to_string());

        // Lock out user_a
        for _ in 0..5 {
            service.record_failure("user_a").await.unwrap();
        }
        assert!(service.check_allowed("user_a").await.is_err());

        // user_b should be unaffected
        assert!(service.check_allowed("user_b").await.is_ok());
    }

    #[tokio::test]
    async fn test_in_memory_tier_escalation() {
        let service = BruteForceProtection::new(None, "test".to_string());

        // 5 failures -> tier 1 lockout (60s)
        for _ in 0..5 {
            service.record_failure("testuser").await.unwrap();
        }
        let err = service.check_allowed("testuser").await.unwrap_err();
        assert!(err.to_string().contains("60 seconds"));

        // 5 more failures -> tier 2 lockout (300s)
        for _ in 0..5 {
            service.record_failure("testuser").await.unwrap();
        }
        let err = service.check_allowed("testuser").await.unwrap_err();
        assert!(err.to_string().contains("300 seconds"));

        // 5 more failures -> tier 3 lockout (900s)
        for _ in 0..5 {
            service.record_failure("testuser").await.unwrap();
        }
        let err = service.check_allowed("testuser").await.unwrap_err();
        assert!(err.to_string().contains("900 seconds"));
    }
}
