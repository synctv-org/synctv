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
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Spawn the background lifecycle task and return its [`JoinHandle`].
    ///
    /// The task wakes up every `config.eviction_interval` and runs a
    /// single eviction cycle.  It exits when the cancellation token is
    /// cancelled.
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
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let watermark = (self.config.max_cache_size as f64 * self.config.watermark_ratio) as u64;
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
        }
    }
}

// ------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use bytes::Bytes;

    use crate::slice_cache::backend::memory::MemoryBackend;
    use crate::slice_cache::etag::StoredEntry;

    /// Helper: create a memory backend wrapped in CacheBackend.
    fn memory_backend() -> Arc<CacheBackend> {
        Arc::new(CacheBackend::Memory(MemoryBackend::new(
            64 * 1024 * 1024,
            Duration::from_hours(1),
        )))
    }

    /// Helper: config with a very short eviction interval for testing.
    fn fast_config() -> SliceCacheConfig {
        SliceCacheConfig {
            eviction_interval: Duration::from_millis(50),
            max_cache_size: 1024,
            watermark_ratio: 0.875,
            ..SliceCacheConfig::default()
        }
    }

    #[tokio::test]
    async fn test_lifecycle_evicts_expired() {
        let backend = memory_backend();

        // Insert an entry that is already expired.
        let expired_entry = StoredEntry {
            data: Bytes::from("old_data"),
            inserted_at: std::time::SystemTime::now() - Duration::from_mins(2),
            ttl: Duration::from_secs(1),
            last_accessed: std::time::SystemTime::now() - Duration::from_mins(2),
        };
        backend.put("expired_key", expired_entry).await.unwrap();

        // Insert a fresh entry.
        let fresh_entry = StoredEntry::new(Bytes::from("fresh"), Duration::from_hours(1));
        backend.put("fresh_key", fresh_entry).await.unwrap();

        // Verify both entries are retrievable before lifecycle starts.
        assert!(backend.get("expired_key").await.is_some());
        assert!(backend.get("fresh_key").await.is_some());

        // Start the lifecycle manager.
        let manager = CacheLifecycleManager::new(Arc::clone(&backend), fast_config());
        let cancel = manager.cancellation_token();
        let handle = manager.start();

        // Wait for at least one cycle.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Cancel and wait for shutdown.
        cancel.cancel();
        handle.await.unwrap();

        // The expired entry should have been evicted.
        assert!(backend.get("expired_key").await.is_none());
        // The fresh entry should still exist.
        assert!(backend.get("fresh_key").await.is_some());
    }

    #[tokio::test]
    async fn test_lifecycle_watermark_eviction() {
        let backend = memory_backend();

        let config = SliceCacheConfig {
            eviction_interval: Duration::from_millis(50),
            max_cache_size: 500,  // 500 bytes max
            watermark_ratio: 0.5, // watermark at 250 bytes
            ..SliceCacheConfig::default()
        };

        // Insert entries totaling 400 bytes (above 250 watermark).
        for i in 0..4u8 {
            let entry = StoredEntry::new(Bytes::from(vec![i; 100]), Duration::from_hours(1));
            backend.put(&format!("key_{i}"), entry).await.unwrap();
            // Small sleep so last_accessed differs for LRU.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        assert_eq!(backend.current_size(), 400);

        // Start the lifecycle manager.
        let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
        let cancel = manager.cancellation_token();
        let handle = manager.start();

        // Wait for eviction.
        tokio::time::sleep(Duration::from_millis(200)).await;

        cancel.cancel();
        handle.await.unwrap();

        // Size should be at or below the watermark (250).
        assert!(
            backend.current_size() <= 250,
            "Expected size <= 250, got {}",
            backend.current_size()
        );
    }

    #[tokio::test]
    async fn test_lifecycle_cancellation() {
        let backend = memory_backend();
        let config = SliceCacheConfig {
            eviction_interval: Duration::from_hours(1), // Long interval
            ..SliceCacheConfig::default()
        };

        let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
        let cancel = manager.cancellation_token();
        let handle = manager.start();

        // Cancel immediately.
        cancel.cancel();

        // The task should exit promptly.
        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "Lifecycle manager should have stopped within 2 seconds"
        );
        result.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_lifecycle_no_eviction_when_under_watermark() {
        let backend = memory_backend();

        let config = SliceCacheConfig {
            eviction_interval: Duration::from_millis(50),
            max_cache_size: 10_000,
            watermark_ratio: 0.875,
            ..SliceCacheConfig::default()
        };

        // Insert a small entry well below the watermark.
        let entry = StoredEntry::new(Bytes::from("tiny"), Duration::from_hours(1));
        backend.put("k1", entry).await.unwrap();

        let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
        let cancel = manager.cancellation_token();
        let handle = manager.start();

        // Let a few cycles run.
        tokio::time::sleep(Duration::from_millis(200)).await;

        cancel.cancel();
        handle.await.unwrap();

        // The entry should still be there.
        assert!(backend.get("k1").await.is_some());
        assert_eq!(backend.current_size(), 4);
    }

    #[tokio::test]
    async fn test_lifecycle_multiple_cycles() {
        let backend = memory_backend();
        let config = fast_config();

        let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
        let cancel = manager.cancellation_token();
        let handle = manager.start();

        // Insert an expired entry partway through.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let expired_entry = StoredEntry {
            data: Bytes::from("late_expired"),
            inserted_at: std::time::SystemTime::now() - Duration::from_mins(2),
            ttl: Duration::from_secs(1),
            last_accessed: std::time::SystemTime::now() - Duration::from_mins(2),
        };
        backend.put("late_key", expired_entry).await.unwrap();

        // Wait for another cycle to pick it up.
        tokio::time::sleep(Duration::from_millis(200)).await;

        cancel.cancel();
        handle.await.unwrap();

        assert!(backend.get("late_key").await.is_none());
    }
}
