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
use std::sync::Arc;
use std::time::Duration;

use crate::{cache::KeyBuilder, Error, Result, InternalExt};

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
    /// with the lockout duration if the account is locked.
    pub async fn check_allowed(&self, username: &str) -> Result<()> {
        let attempts = self.get_attempts(username).await?;
        if let Some(lockout_secs) = Self::lockout_duration(attempts) {
            tracing::warn!(
                username = %username,
                attempts = attempts,
                lockout_secs = lockout_secs,
                "Login attempt blocked: account temporarily locked"
            );
            return Err(Error::Authentication(format!(
                "Too many failed login attempts. Please try again in {} seconds.",
                lockout_secs
            )));
        }
        Ok(())
    }

    /// Record a failed login attempt. Increments the counter and sets/refreshes TTL.
    pub async fn record_failure(&self, username: &str) -> Result<()> {
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let key = self.key_builder.login_attempts(username);

            // INCR + EXPIRE atomically via pipeline
            let (count,): (u64,) = redis::pipe()
                .atomic()
                .incr(&key, 1u64)
                .expire(&key, ATTEMPTS_TTL_SECS as i64)
                .ignore()
                .query_async(&mut conn)
                .await
                .internal_with_err("Failed to record login failure")?;

            tracing::debug!(
                username = %username,
                attempts = count,
                "Recorded failed login attempt"
            );
        } else {
            // In-memory fallback
            let now = chrono::Utc::now().timestamp();
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

    /// Get the current failed attempt count for a username.
    ///
    /// On Redis error, falls back to the in-memory cache rather than returning 0
    /// (fail-closed). Returning 0 on error would disable brute-force protection
    /// entirely, allowing unlimited login attempts during Redis outages.
    async fn get_attempts(&self, username: &str) -> Result<u64> {
        let key = self.key_builder.login_attempts(username);

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            match conn.get::<_, Option<u64>>(&key).await {
                Ok(count) => return Ok(count.unwrap_or(0)),
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
        let count = self.local_attempts.get(&key).await.map_or(0, |(c, _)| c);
        Ok(count)
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
