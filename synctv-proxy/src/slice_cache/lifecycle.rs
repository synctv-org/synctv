//! Background cache lifecycle manager (nginx `ngx_http_file_cache_manager`
//! equivalent).
//!
//! Runs a periodic background task that:
//!
//! 1. **Evicts expired entries** -- removes entries whose TTL has elapsed.
//! 2. **Watermark eviction** -- when total cache size exceeds
//!    `max_cache_size * watermark_ratio` (default 7/8, matching nginx), evicts
//!    the least-recently-used entries until the target is reached.
//! 3. **Temp file cleanup** -- for the file backend only, removes orphaned
//!    temp files left behind by failed atomic writes.
//!
//! # Design notes
//!
//! The nginx cache manager (`ngx_http_file_cache_manager`) wakes up on a
//! configurable interval (`manager_sleep`), runs `ngx_http_file_cache_expire`
//! to purge expired entries, then checks the `max_size` / `watermark` /
//! `min_free` thresholds and calls `ngx_http_file_cache_forced_expire` if
//! the cache is over-full.  We follow the same pattern using
//! `tokio::time::interval` and the `CancellationToken` for graceful shutdown.

use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::backend::CacheBackend;
use super::backend::SliceCacheBackend;
use super::config::SliceCacheConfig;
use super::maintenance::u64_to_f64;

fn watermark_bytes(max_cache_size: u64, ratio: f64) -> u64 {
    if !ratio.is_finite() {
        return max_cache_size;
    }

    let clamped = ratio.clamp(0.0, 1.0);
    if clamped <= 0.0 {
        return 0;
    }
    if clamped >= 1.0 {
        return max_cache_size;
    }

    let target = u64_to_f64(max_cache_size) * clamped;
    let mut low = 0;
    let mut high = max_cache_size;

    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if u64_to_f64(mid) <= target {
            low = mid;
        } else {
            high = mid - 1;
        }
    }

    low
}

/// Background cache lifecycle manager.
///
/// Owns a shared reference to the [`CacheBackend`] and a copy of the
/// [`SliceCacheConfig`].  Call [`start`](Self::start) to spawn the
/// background task; use the [`CancellationToken`] returned by
/// [`cancellation_token`](Self::cancellation_token) to signal shutdown.
pub struct CacheLifecycleManager {
    backend: Arc<CacheBackend>,
    config: SliceCacheConfig,
    cancel: CancellationToken,
}

impl CacheLifecycleManager {
    /// Create a new lifecycle manager.
    ///
    /// The manager will not start doing work until [`start`](Self::start)
    /// is called.
    pub fn new(backend: Arc<CacheBackend>, config: SliceCacheConfig) -> Self {
        Self {
            backend,
            config,
            cancel: CancellationToken::new(),
        }
    }

    /// Return a clone of the internal `CancellationToken`.
    ///
    /// Cancel this token to request a graceful shutdown of the
    /// background task.
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Spawn the background lifecycle task and return its [`JoinHandle`].
    ///
    /// The task wakes up every `config.eviction_interval` and runs a
    /// single eviction cycle.  It exits when the cancellation token is
    /// cancelled.
    #[must_use]
    pub fn start(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.config.eviction_interval);
            // The first tick completes immediately; consuming it avoids
            // running an eviction cycle at startup before the cache has
            // had time to fill.
            interval.tick().await;

            loop {
                tokio::select! {
                    () = self.cancel.cancelled() => {
                        tracing::debug!("Cache lifecycle manager shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        self.run_cycle().await;
                    }
                }
            }
        })
    }

    /// Execute a single eviction cycle.
    ///
    /// This is the core of the cache manager, equivalent to nginx's
    /// `ngx_http_file_cache_manager` function.
    async fn run_cycle(&self) {
        // 1. Evict expired entries.
        let expired = self.backend.evict_expired().await;
        if expired > 0 {
            tracing::debug!(expired, "Cache lifecycle: evicted expired entries");
        }

        // 2. Check watermark (7/8 of max_size like nginx).
        //    nginx: `if (size < cache->max_size && count < watermark) { break; }`
        let watermark = watermark_bytes(self.config.max_cache_size, self.config.watermark_ratio);
        let current = self.backend.current_size();
        if current > watermark {
            let freed = self.backend.evict_to_size(watermark).await;
            if freed > 0 {
                tracing::debug!(
                    freed,
                    current,
                    watermark,
                    "Cache lifecycle: watermark eviction"
                );
            }
        }

        // 3. Cleanup orphaned temp files (file backend only).
        if let CacheBackend::File(ref fb) = *self.backend {
            fb.cleanup_temp_files().await;
            fb.persist_access_times().await;
        }
    }
}

// Tests

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
