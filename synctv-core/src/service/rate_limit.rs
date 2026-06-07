//! Rate limiting service with pluggable backends.
//!
//! # Usage
//!
//! This module lives in `synctv-core` because it serves **both** domain-level
//! and API-level rate limiting:
//!
//! - **Domain-level**: `ChatService` uses `RateLimiter` for per-user chat
//!   throttling.
//! - **API-level**: `synctv-api` uses `RateLimiter` from shared request
//!   execution paths and transport-adjacent helpers for request-level
//!   throttling.
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
//! - **Sync call sites** (`check_rate_limit_sync`) always use in-memory
//!   limiting regardless of Redis availability, since synchronous call paths
//!   cannot `await` shared-state backends.

use crate::{RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use synctv_common::{ExecutionControl, ExecutionControlError};
use thiserror::Error;

mod memory;
mod redis_backend;

use memory::InMemoryGovernorLimiter;
pub use memory::InMemoryRateLimitBackend;
pub use redis_backend::RedisRateLimitBackend;

/// Rate limiting error
#[derive(Error, Debug)]
pub enum RateLimitError {
    #[error("Rate limit exceeded. Try again in {retry_after_seconds}s")]
    RateLimitExceeded { retry_after_seconds: u64 },

    #[error("Rate limit backend unavailable: {0}")]
    BackendUnavailable(String),

    #[error(transparent)]
    Control(#[from] ExecutionControlError),

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
            RateLimitError::BackendUnavailable(msg) => Self::ServiceUnavailable(msg),
            RateLimitError::Control(error) => Self::Timeout(error.to_string()),
            RateLimitError::RedisError(e) => {
                Self::Internal(format!("Rate limiter Redis error: {e}"))
            }
        }
    }
}

/// Extract a low-cardinality tier label from a rate-limit key.
///
/// Prometheus metric labels must never include high-cardinality
/// values such as user IDs, IP addresses, or room IDs.
fn extract_rate_limit_tier(key: &str) -> &'static str {
    const KNOWN_TIERS: &[&str] = &[
        "auth",
        "read",
        "write",
        "media",
        "chat",
        "room_password_check",
        "grpc",
        "api",
        "refresh",
        "email",
        "streaming",
        "websocket",
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

fn timestamp_millis() -> std::result::Result<u64, RateLimitError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            RateLimitError::BackendUnavailable(format!(
                "System clock is before UNIX_EPOCH: {error}"
            ))
        })?;
    u64::try_from(duration.as_millis()).map_err(|_| {
        RateLimitError::BackendUnavailable("current timestamp exceeds u64::MAX millis".to_string())
    })
}

fn window_expire_seconds(window_seconds: u64) -> std::result::Result<i64, RateLimitError> {
    let expires = window_seconds.checked_add(1).ok_or_else(|| {
        RateLimitError::BackendUnavailable("rate limit window exceeds u64::MAX seconds".to_string())
    })?;
    i64::try_from(expires).map_err(|_| {
        RateLimitError::BackendUnavailable("rate limit window exceeds i64::MAX seconds".to_string())
    })
}

fn millis_to_i64(value: u64) -> std::result::Result<i64, RateLimitError> {
    i64::try_from(value).map_err(|_| {
        RateLimitError::BackendUnavailable("millisecond timestamp exceeds i64::MAX".to_string())
    })
}

fn window_millis(window_seconds: u64) -> std::result::Result<u64, RateLimitError> {
    window_seconds.checked_mul(1000).ok_or_else(|| {
        RateLimitError::BackendUnavailable("rate limit window exceeds u64::MAX millis".to_string())
    })
}

fn retry_after_seconds_from_oldest(
    now_millis: u64,
    oldest_score_millis: u64,
    window_seconds: u64,
) -> u64 {
    if oldest_score_millis == 0 {
        return 1;
    }

    let time_since_oldest = now_millis.saturating_sub(oldest_score_millis);
    let window_ms = window_seconds.saturating_mul(1000);
    let remaining_window = window_ms.saturating_sub(time_since_oldest);
    remaining_window.div_ceil(1000).max(1)
}

fn redis_count_to_u32(value: i64, field: &'static str) -> std::result::Result<u32, RateLimitError> {
    if value < 0 {
        return Err(RateLimitError::BackendUnavailable(format!(
            "Redis returned negative {field}"
        )));
    }
    u32::try_from(value)
        .map_err(|_| RateLimitError::BackendUnavailable(format!("Redis {field} exceeds u32::MAX")))
}

fn parse_sliding_window_result(result: &[i64]) -> std::result::Result<(u32, u64), RateLimitError> {
    let [count, oldest_score, ..] = result else {
        return Err(RateLimitError::BackendUnavailable(format!(
            "Redis sliding-window script returned {} values; expected 2",
            result.len()
        )));
    };
    let oldest_score = if *oldest_score < 0 {
        return Err(RateLimitError::BackendUnavailable(
            "Redis returned negative oldest score".to_string(),
        ));
    } else {
        oldest_score.cast_unsigned()
    };
    Ok((
        redis_count_to_u32(*count, "sliding-window count")?,
        oldest_score,
    ))
}

fn parse_quota_count_result(result: &[u32]) -> std::result::Result<u32, RateLimitError> {
    let [count, ..] = result else {
        return Err(RateLimitError::BackendUnavailable(format!(
            "Redis quota pipeline returned {} values; expected 1",
            result.len()
        )));
    };
    Ok(*count)
}

// RateLimitBackend trait

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

    async fn check_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        if let Some(control) = control {
            control.check_active()?;
        }
        self.check(key, max_requests, window_seconds).await
    }

    /// Strict distributed check. Fails closed when Redis is unavailable.
    async fn check_strict(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError>;

    async fn check_strict_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        if let Some(control) = control {
            control.check_active()?;
        }
        self.check_strict(key, max_requests, window_seconds).await
    }

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
}

/// Stable service boundary for request/message rate limiting.
///
/// Callers should depend on this trait rather than the concrete `RateLimiter`
/// so the implementation can be swapped transparently (local memory, Redis,
/// external coordinator, etc.).
#[async_trait]
pub trait RequestRateLimiterService: Send + Sync {
    /// Check if the backend is healthy.
    async fn health_check(&self) -> std::result::Result<(), String>;

    /// Synchronous rate limit check for sync call sites.
    fn check_rate_limit_sync(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError>;

    /// Async rate limit check using the implementation's normal semantics.
    async fn check_rate_limit(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError>;

    async fn check_rate_limit_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        if let Some(control) = control {
            control.check_active()?;
        }
        self.check_rate_limit(key, max_requests, window_seconds)
            .await
    }

    /// Strict distributed rate limit check that fails closed when the shared
    /// backend is unavailable.
    async fn check_rate_limit_distributed(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError>;

    async fn check_rate_limit_distributed_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        if let Some(control) = control {
            control.check_active()?;
        }
        self.check_rate_limit_distributed(key, max_requests, window_seconds)
            .await
    }

    /// Get remaining quota for a rate limit.
    async fn get_quota(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> Result<(u32, u64)>;

    /// Reset rate limit state for a key.
    async fn reset(&self, key: &str) -> Result<()>;
}

/// Build a request rate limiter behind the service abstraction.
///
/// Callers should depend on the returned trait object instead of branching on
/// the concrete local or shared implementation.
pub fn request_rate_limiter_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn RequestRateLimiterService>> {
    Ok(Arc::new(RateLimiter::from_shared_state_profile(profile)?))
}

#[async_trait]
impl<T> RequestRateLimiterService for Arc<T>
where
    T: RequestRateLimiterService + ?Sized,
{
    async fn health_check(&self) -> std::result::Result<(), String> {
        self.as_ref().health_check().await
    }

    fn check_rate_limit_sync(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        self.as_ref()
            .check_rate_limit_sync(key, max_requests, window_seconds)
    }

    async fn check_rate_limit(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        self.as_ref()
            .check_rate_limit(key, max_requests, window_seconds)
            .await
    }

    async fn check_rate_limit_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        self.as_ref()
            .check_rate_limit_with_control(key, max_requests, window_seconds, control)
            .await
    }

    async fn check_rate_limit_distributed(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        self.as_ref()
            .check_rate_limit_distributed(key, max_requests, window_seconds)
            .await
    }

    async fn check_rate_limit_distributed_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        self.as_ref()
            .check_rate_limit_distributed_with_control(key, max_requests, window_seconds, control)
            .await
    }

    async fn get_quota(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> Result<(u32, u64)> {
        self.as_ref()
            .get_quota(key, max_requests, window_seconds)
            .await
    }

    async fn reset(&self, key: &str) -> Result<()> {
        self.as_ref().reset(key).await
    }
}

// RateLimiter (public API)

/// Rate limiter with pluggable backend and sync fallback.
///
/// Wraps an async `RateLimitBackend` (Redis or in-memory) and always
/// maintains a local `InMemoryGovernorLimiter` for synchronous operations.
#[derive(Clone)]
pub struct RateLimiter {
    backend: Arc<dyn RateLimitBackend>,
    /// In-memory governor for sync operations (always present)
    sync_limiter: InMemoryGovernorLimiter,
    key_prefix: String,
    strict_distributed: bool,
}

impl RateLimiter {
    /// Create a new `RateLimiter` with a custom backend.
    pub fn from_backend(backend: Arc<dyn RateLimitBackend>, key_prefix: String) -> Self {
        Self {
            backend,
            sync_limiter: InMemoryGovernorLimiter::new(),
            key_prefix,
            strict_distributed: false,
        }
    }

    #[must_use]
    pub fn from_redis_runtime(
        redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        key_prefix: String,
    ) -> Self {
        if let Some(conn) = redis_runtime {
            let backend = Arc::new(RedisRateLimitBackend::from_runtime(
                conn,
                key_prefix.clone(),
            ));
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

    pub fn from_shared_state_profile(profile: &SharedStateProfile) -> Result<Self> {
        match profile.state_mode() {
            SharedStateMode::SharedRequired => Ok(Self::from_redis_runtime(
                Some(profile.require_shared_runtime("rate-limit state")?),
                profile.key_prefix().to_string(),
            )
            .with_strict_distributed()),
            SharedStateMode::SharedBestEffort => Ok(Self::from_redis_runtime(
                profile.shared_runtime(),
                profile.key_prefix().to_string(),
            )),
            SharedStateMode::LocalOnly => Ok(Self::local_only(profile.key_prefix().to_string())),
        }
    }

    /// Create a local-only `RateLimiter` without shared state.
    #[must_use]
    pub fn local_only(key_prefix: String) -> Self {
        let backend = Arc::new(InMemoryRateLimitBackend::new(key_prefix.clone()));
        Self::from_backend(backend, key_prefix)
    }

    /// Enable strict distributed checks for operations that must fail closed.
    #[must_use]
    pub const fn with_strict_distributed(mut self) -> Self {
        self.strict_distributed = true;
        self
    }

    /// Check if Redis is connected and responding
    pub async fn health_check(&self) -> std::result::Result<(), String> {
        self.backend.health_check().await
    }

    /// Synchronous rate limit check using the in-memory governor limiter.
    ///
    /// Always uses in-memory governor regardless of backend. Synchronous call
    /// sites cannot `await` a Redis call.
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
        self.check_rate_limit_with_control(key, max_requests, window_seconds, None)
            .await
    }

    pub async fn check_rate_limit_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        if self.strict_distributed {
            self.backend
                .check_strict_with_control(key, max_requests, window_seconds, control)
                .await
        } else {
            self.backend
                .check_with_control(key, max_requests, window_seconds, control)
                .await
        }
    }

    /// Distributed rate limit check that fails closed when Redis is unavailable.
    pub async fn check_rate_limit_distributed(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        self.check_rate_limit_distributed_with_control(key, max_requests, window_seconds, None)
            .await
    }

    pub async fn check_rate_limit_distributed_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        self.backend
            .check_strict_with_control(key, max_requests, window_seconds, control)
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
}

#[async_trait]
impl RequestRateLimiterService for RateLimiter {
    async fn health_check(&self) -> std::result::Result<(), String> {
        Self::health_check(self).await
    }

    fn check_rate_limit_sync(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        Self::check_rate_limit_sync(self, key, max_requests, window_seconds)
    }

    async fn check_rate_limit(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        Self::check_rate_limit(self, key, max_requests, window_seconds).await
    }

    async fn check_rate_limit_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        Self::check_rate_limit_with_control(self, key, max_requests, window_seconds, control).await
    }

    async fn check_rate_limit_distributed(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), RateLimitError> {
        Self::check_rate_limit_distributed(self, key, max_requests, window_seconds).await
    }

    async fn check_rate_limit_distributed_with_control(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<(), RateLimitError> {
        Self::check_rate_limit_distributed_with_control(
            self,
            key,
            max_requests,
            window_seconds,
            control,
        )
        .await
    }

    async fn get_quota(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> Result<(u32, u64)> {
        Self::get_quota(self, key, max_requests, window_seconds).await
    }

    async fn reset(&self, key: &str) -> Result<()> {
        Self::reset(self, key).await
    }
}

/// Rate limit configuration
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub chat_per_second: u32,
    pub window_seconds: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            chat_per_second: 10,
            window_seconds: 1,
        }
    }
}

#[cfg(test)]
mod tests;
