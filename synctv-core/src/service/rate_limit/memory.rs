use async_trait::async_trait;
use governor::clock::Clock;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter as GovernorRateLimiter};
use moka::sync::Cache as MokaCache;
use nonzero_ext::nonzero;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
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
    checks_since_sweep: Arc<AtomicU64>,
}

impl InMemoryGovernorLimiter {
    /// Sweep the keyed limiter state stores after this many checks. The sweep
    /// is O(live keys), so amortizing it across checks keeps the hot path
    /// cheap while bounding memory retention to recently active subjects.
    const SWEEP_EVERY_CHECKS: u64 = 4096;

    pub(super) fn new() -> Self {
        let cache = MokaCache::builder().max_capacity(64).build();
        Self {
            limiters: Arc::new(cache),
            checks_since_sweep: Arc::new(AtomicU64::new(0)),
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

    /// Drop keyed entries whose GCRA state is indistinguishable from a fresh
    /// bucket (idle beyond the quota window). Without this, per-subject keys -
    /// including attacker-chosen IPs in anonymous subjects - are retained
    /// forever and only a process restart reclaims the memory.
    fn sweep_stale_keys(&self) {
        for (_quota, limiter) in self.limiters.iter() {
            limiter.retain_recent();
            limiter.shrink_to_fit();
        }
    }

    pub(super) fn check(
        &self,
        key: &str,
        max_requests: u32,
        window_seconds: u64,
    ) -> std::result::Result<(), u64> {
        let limiter = self.get_limiter(max_requests, window_seconds);
        let result = match limiter.check_key(&key.to_string()) {
            Ok(()) => Ok(()),
            Err(not_until) => {
                let wait = not_until.wait_time_from(governor::clock::DefaultClock::default().now());
                let retry_after_seconds = wait.as_secs().max(1);
                Err(retry_after_seconds)
            }
        };
        if self.checks_since_sweep.fetch_add(1, Ordering::Relaxed) >= Self::SWEEP_EVERY_CHECKS {
            self.checks_since_sweep.store(0, Ordering::Relaxed);
            self.sweep_stale_keys();
        }
        result
    }

    #[cfg(test)]
    pub(super) fn sweep_stale_keys_for_test(&self) {
        self.sweep_stale_keys();
    }

    #[cfg(test)]
    pub(super) fn live_key_count(&self) -> usize {
        self.limiters
            .iter()
            .map(|(_quota, limiter)| limiter.len())
            .sum()
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
