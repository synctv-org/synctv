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
//! ### Cluster Mode Requirements
//!
//! 1. Enable cluster runtime in the caller's service options
//! 2. Provide a shared Redis runtime to all replicas
//! 3. Use `fail_closed=true` for brute-force protection
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
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::Arc;
use synctv_common::ExecutionControl;

use crate::{
    cache::KeyBuilder, Error, RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile,
};

mod tracker;
use tracker::run_with_control;
pub use tracker::{AttemptTracker, InMemoryAttemptTracker, RedisAttemptTracker};

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
    async fn check_subject_key_allowed_with_control(
        &self,
        subject_key: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()>;
    async fn record_subject_key_failure_with_control(
        &self,
        subject_key: &str,
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
    async fn reset_subject_key_with_control(
        &self,
        subject_key: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()>;
    async fn reset_ip(&self, ip: &IpAddr) -> Result<()>;
    async fn reset_ip_with_control(
        &self,
        ip: &IpAddr,
        control: Option<&ExecutionControl>,
    ) -> Result<()>;
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

fn nonnegative_elapsed_secs(now: i64, last_failure_at: i64) -> u64 {
    u64::try_from(now.saturating_sub(last_failure_at)).unwrap_or(0)
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
                profile.best_effort_shared_runtime("brute-force protection state")?,
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
        let key = self.key_builder.login_attempts(username);
        self.check_subject_key_allowed_with_control_and_message(
            &key,
            ip,
            control,
            "Login attempt",
            "Too many failed login attempts",
        )
        .await
    }

    /// Check if an attempt is allowed for an already-built subject key.
    ///
    /// This is for non-login domains that share attempt tracking mechanics but
    /// must own their own key namespace, such as room password verification.
    pub async fn check_subject_key_allowed_with_control(
        &self,
        subject_key: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        self.check_subject_key_allowed_with_control_and_message(
            subject_key,
            ip,
            control,
            "Attempt",
            "Too many failed attempts",
        )
        .await
    }

    async fn check_subject_key_allowed_with_control_and_message(
        &self,
        subject_key: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
        log_subject: &'static str,
        user_message_prefix: &'static str,
    ) -> Result<()> {
        // Check IP-level lockout first
        self.check_ip_lockout(ip, control, log_subject, user_message_prefix)
            .await?;

        let (attempts, last_failure_at) =
            run_with_control(control, self.username_tracker.get_attempts(subject_key)).await?;
        let lockout_secs = self.lockout_duration_with_config(attempts);
        if let Some(lockout_secs) = lockout_secs {
            let now = crate::SystemClock.now().timestamp();
            let elapsed = nonnegative_elapsed_secs(now, last_failure_at);
            if elapsed < lockout_secs {
                let remaining = lockout_secs - elapsed;
                tracing::warn!(
                    subject_key = %subject_key,
                    attempts = attempts,
                    lockout_secs = lockout_secs,
                    remaining_secs = remaining,
                    "{} blocked: subject temporarily locked",
                    log_subject
                );
                return Err(Error::Authentication(format!(
                    "{user_message_prefix}. Please try again in {remaining} seconds.",
                )));
            }
        }
        Ok(())
    }

    async fn check_ip_lockout(
        &self,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
        log_subject: &'static str,
        user_message_prefix: &'static str,
    ) -> Result<()> {
        if let Some(ip_addr) = ip {
            let ip_key = self.key_builder.login_attempts_ip(&ip_addr.to_string());
            let (ip_attempts, ip_last_failure_at) =
                run_with_control(control, self.ip_tracker.get_attempts(&ip_key)).await?;
            if ip_attempts >= self.config.ip_threshold {
                let now = crate::SystemClock.now().timestamp();
                let elapsed = nonnegative_elapsed_secs(now, ip_last_failure_at);
                if elapsed < self.config.ip_lockout_secs {
                    let remaining = self.config.ip_lockout_secs - elapsed;
                    tracing::warn!(
                        ip = %ip_addr,
                        attempts = ip_attempts,
                        remaining_secs = remaining,
                        "{} blocked: IP temporarily locked",
                        log_subject
                    );
                    return Err(Error::Authentication(format!(
                        "{user_message_prefix}. Please try again in {remaining} seconds.",
                    )));
                }
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
        let key = self.key_builder.login_attempts(username);
        self.record_subject_key_failure_with_control(&key, ip, control)
            .await
    }

    pub async fn record_subject_key_failure_with_control(
        &self,
        subject_key: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        let now = crate::SystemClock.now().timestamp();

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

        run_with_control(
            control,
            self.username_tracker
                .record_failure(subject_key, now, self.config.attempts_ttl_secs),
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
            let now = crate::SystemClock.now().timestamp();
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
        self.check_ip_lockout(
            ip,
            control,
            "Login attempt",
            "Too many failed login attempts",
        )
        .await
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
        self.reset_subject_key_with_control(&key, control).await
    }

    pub async fn reset_subject_key_with_control(
        &self,
        subject_key: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        run_with_control(control, self.username_tracker.reset(subject_key)).await?;
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

    async fn check_subject_key_allowed_with_control(
        &self,
        subject_key: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::check_subject_key_allowed_with_control(self, subject_key, ip, control).await
    }

    async fn record_subject_key_failure_with_control(
        &self,
        subject_key: &str,
        ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::record_subject_key_failure_with_control(self, subject_key, ip, control).await
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

    async fn reset_subject_key_with_control(
        &self,
        subject_key: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::reset_subject_key_with_control(self, subject_key, control).await
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
mod tests;
