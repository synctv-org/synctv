use async_trait::async_trait;
use governor::clock::Clock;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter as GovernorRateLimiter};
use moka::sync::Cache as MokaCache;
use nonzero_ext::nonzero;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use crate::Result;

use super::{RateLimitBackend, RateLimitError};

/// Type alias for the keyed rate limiter cache used by `InMemoryGovernorLimiter`.
type GovernorLimiterCache = MokaCache<(u32, u64), Arc<DefaultKeyedRateLimiter<String>>>;

/// In-memory rate limiter backed by the `governor` crate (GCRA algorithm).
///
/// Uses a keyed rate limiter with `String` keys. Each unique key gets its own
/// independent rate limit bucket.
#[derive(Clone)]
pub(super) struct InMemoryGovernorLimiter {
    limiters: Arc<GovernorLimiterCache>,
}

impl InMemoryGovernorLimiter {
    pub(super) fn new() -> Self {
        let cache = MokaCache::builder().max_capacity(64).build();
        Self {
            limiters: Arc::new(cache),
        }
    }

    fn normalize_quota_input(max_requests: u32, window_seconds: u64) -> (u32, u64) {
        (max_requests.max(1), window_seconds.max(1))
    }

    fn get_limiter(
        &self,
        max_requests: u32,
        window_seconds: u64,
    ) -> Arc<DefaultKeyedRateLimiter<String>> {
        let (max_requests, window_seconds) =
            Self::normalize_quota_input(max_requests, window_seconds);
        let key = (max_requests, window_seconds);
        if let Some(limiter) = self.limiters.get(&key) {
            return limiter;
        }

        let period = Duration::from_secs(window_seconds)
            .checked_div(max_requests)
            .unwrap_or(Duration::from_millis(1));
        let quota = Quota::with_period(period.max(Duration::from_millis(1)))
            .unwrap_or_else(|| Quota::per_second(nonzero!(1u32)))
            .allow_burst(NonZeroU32::new(max_requests).unwrap_or(nonzero!(1u32)));

        let limiter = Arc::new(GovernorRateLimiter::keyed(quota));
        self.limiters.insert(key, Arc::clone(&limiter));
        limiter
    }

    pub(super) fn check(
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

/// In-memory rate limiter using `governor` (GCRA algorithm).
///
/// Per-instance only — not shared across replicas.
pub(super) struct InMemoryRateLimitBackend {
    key_prefix: String,
    governor: InMemoryGovernorLimiter,
}

impl InMemoryRateLimitBackend {
    #[must_use]
    pub(super) fn new(key_prefix: String) -> Self {
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
        Err(RateLimitError::BackendUnavailable(
            "Distributed rate limit backend unavailable".to_string(),
        ))
    }

    async fn get_quota(
        &self,
        _key: &str,
        max_requests: u32,
        _window_seconds: u64,
    ) -> Result<(u32, u64)> {
        Ok((max_requests, 0))
    }

    async fn reset(&self, _key: &str) -> Result<()> {
        Ok(())
    }

    async fn health_check(&self) -> std::result::Result<(), String> {
        Err("Redis not configured".to_string())
    }
}
