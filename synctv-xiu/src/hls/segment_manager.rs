// HLS Segment lifecycle manager
// Responsibilities:
// - Track active streams and their segments
// - Periodic cleanup of expired segments
// - Provide segment metadata for M3U8 generation
// Public segment names remain slash-free. Storage backends may internally map
// minute-prefixed segment names into directory/prefix buckets for efficient
// cleanup.
// Architecture:
// - Storage layer: Pure KV storage (FileStorage/MemoryStorage/S3Storage)
// - SegmentManager: Business logic (retention policy, cleanup scheduling)
// - HLS layer: M3U8 generation and HTTP serving

use crate::hls::playlist::HLS_PLAYLIST_RETENTION_RESERVE;
use crate::storage::HlsStorage;
use futures::StreamExt as _;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_FINAL_PLAYLIST_GRACE: Duration = Duration::from_mins(1);
pub const DEFAULT_ENDED_SEGMENT_GRACE: Duration = Duration::from_secs(90);
pub const DEFAULT_HLS_GENERATION_RETENTION: Duration =
    DEFAULT_FINAL_PLAYLIST_GRACE.saturating_add(DEFAULT_ENDED_SEGMENT_GRACE);

/// Segment cleanup configuration
#[derive(Debug, Clone)]
pub struct CleanupConfig {
    /// How often to run cleanup.
    pub interval: Duration,
    /// Delete segments older than this.
    pub retention: Duration,
    /// Keep the final playlist visible after its publisher ends.
    pub final_playlist_grace: Duration,
    /// Keep the exact segments of an ended generation available after the
    /// playlist window closes.
    pub ended_segment_grace: Duration,
    /// Maximum number of segments per stream. 0 means unlimited (time-based only).
    /// When exceeded, the oldest segments for that stream are deleted.
    pub max_segments_per_stream: usize,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_mins(1),
            retention: Duration::from_mins(3),
            final_playlist_grace: DEFAULT_FINAL_PLAYLIST_GRACE,
            ended_segment_grace: DEFAULT_ENDED_SEGMENT_GRACE,
            max_segments_per_stream: 0,
        }
    }
}

/// HLS Segment Manager
pub struct SegmentManager {
    storage: Arc<dyn HlsStorage>,
    config: CleanupConfig,
    cleanup_authority: Arc<dyn CleanupAuthority>,
    generation_cleanup_queue: parking_lot::Mutex<VecDeque<ScheduledGenerationCleanup>>,
}

struct ScheduledGenerationCleanup {
    due_at: time::Instant,
    marked: MarkedStreamCleanup,
}

/// A stream that has ended and exposed an explicit set of segment names safe to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkedStreamCleanup {
    pub app_name: String,
    pub stream_name: String,
    pub segment_names: Vec<String>,
}

/// Decides whether this replica should run storage cleanup.
///
/// Local-only storage backends should use the default always-true authority so
/// every replica cleans its own data. Shared storage backends can plug in a
/// leader check so only the elected leader scans and deletes shared data.
pub trait CleanupAuthority: Send + Sync {
    fn should_cleanup(&self) -> bool;
}

#[derive(Debug)]
pub struct AlwaysCleanupAuthority;

impl CleanupAuthority for AlwaysCleanupAuthority {
    fn should_cleanup(&self) -> bool {
        true
    }
}

impl SegmentManager {
    const CLEANUP_CONCURRENCY: usize = 8;

    /// Create new segment manager
    pub fn new(storage: Arc<dyn HlsStorage>, config: CleanupConfig) -> Self {
        Self {
            storage,
            config,
            cleanup_authority: Arc::new(AlwaysCleanupAuthority),
            generation_cleanup_queue: parking_lot::Mutex::new(VecDeque::new()),
        }
    }

    /// Return the count limit that preserves every segment that can still be
    /// named by the live playlist plus the one-refresh reserve.
    ///
    /// A configured value below this floor would make count-based cleanup
    /// delete an object that a client can already have in its playlist. Zero
    /// keeps count cleanup disabled.
    fn effective_max_segments_per_stream(&self) -> usize {
        if self.config.max_segments_per_stream == 0 {
            0
        } else {
            self.config
                .max_segments_per_stream
                .max(HLS_PLAYLIST_RETENTION_RESERVE)
        }
    }

    /// Set the cleanup authority used by startup and periodic cleanup.
    #[must_use]
    pub fn with_cleanup_authority(mut self, cleanup_authority: Arc<dyn CleanupAuthority>) -> Self {
        self.cleanup_authority = cleanup_authority;
        self
    }

    /// Start periodic cleanup task with optional cancellation support.
    ///
    /// This spawns a background task that periodically calls `storage.cleanup()`
    /// to delete expired segments. The task stops when the `CancellationToken` is cancelled.
    ///
    /// Returns the `JoinHandle` so callers can wait for graceful shutdown or abort if needed.
    #[must_use]
    pub fn start_cleanup_task(
        self: Arc<Self>,
        shutdown_token: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = Arc::clone(&self);
        tokio::spawn(async move {
            manager.run_cleanup_loop(shutdown_token).await;
        })
    }

    /// Run the cleanup loop until cancelled.
    async fn run_cleanup_loop(&self, shutdown_token: CancellationToken) {
        let mut interval = time::interval(self.config.interval);
        let effective_retention = self.config.retention.max(self.generation_cleanup_delay());
        let effective_max_segments = self.effective_max_segments_per_stream();

        tracing::info!(
            "Segment cleanup task started: interval={:?}, retention={:?}, max_segments_per_stream={}",
            self.config.interval,
            effective_retention,
            if effective_max_segments == 0 {
                "unlimited".to_string()
            } else {
                effective_max_segments.to_string()
            }
        );

        loop {
            tokio::select! {
                _ = interval.tick() => {}
                () = shutdown_token.cancelled() => {
                    tracing::info!("Segment cleanup task shutting down");
                    break;
                }
            }

            if !self.cleanup_authority.should_cleanup() {
                tracing::trace!("Skipping HLS segment cleanup because this replica is not the cleanup authority");
                continue;
            }

            self.cleanup_due_generations().await;

            // Enforce per-stream segment count bound (if configured)
            if effective_max_segments > 0 {
                match self.storage.list_streams().await {
                    Ok(streams) => {
                        futures::stream::iter(streams)
                            .for_each_concurrent(
                                Self::CLEANUP_CONCURRENCY,
                                |(app, stream)| async move {
                                    match self
                                        .storage
                                        .delete_oldest_stream_segments(
                                            &app,
                                            &stream,
                                            effective_max_segments,
                                        )
                                        .await
                                    {
                                        Ok(deleted) if deleted > 0 => {
                                            tracing::info!(
                                                "Count-based cleanup: deleted {} excess segments for {}/{} (max {})",
                                                deleted,
                                                app,
                                                stream,
                                                effective_max_segments
                                            );
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "Count-based cleanup failed for {}/{}: {}",
                                                app,
                                                stream,
                                                e
                                            );
                                        }
                                        _ => {}
                                    }
                                }
                            )
                            .await;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to list streams for count-based cleanup: {}", e);
                    }
                }
            }

            match self.storage.cleanup(effective_retention).await {
                Ok(deleted) => {
                    if deleted > 0 {
                        tracing::info!("Cleaned up {} expired HLS segments", deleted);
                    } else {
                        tracing::trace!("No expired segments to clean up");
                    }
                }
                Err(e) => {
                    tracing::error!("Segment cleanup failed: {}", e);
                }
            }
        }
    }

    /// Get storage backend for direct access
    #[must_use]
    pub fn storage(&self) -> &Arc<dyn HlsStorage> {
        &self.storage
    }

    /// Delay cleanup of one ended publisher generation while retaining its
    /// exact object names independently from the current stream registry entry.
    pub fn schedule_generation_cleanup(
        &self,
        app_name: String,
        stream_name: String,
        segment_names: Vec<String>,
    ) {
        if segment_names.is_empty() {
            return;
        }
        self.generation_cleanup_queue
            .lock()
            .push_back(ScheduledGenerationCleanup {
                due_at: time::Instant::now() + self.generation_cleanup_delay(),
                marked: MarkedStreamCleanup {
                    app_name,
                    stream_name,
                    segment_names,
                },
            });
    }

    #[must_use]
    pub fn final_playlist_grace(&self) -> Duration {
        self.config.final_playlist_grace
    }

    fn generation_cleanup_delay(&self) -> Duration {
        self.config
            .final_playlist_grace
            .saturating_add(self.config.ended_segment_grace)
    }

    async fn cleanup_due_generations(&self) -> usize {
        let now = time::Instant::now();
        let due = {
            let mut queue = self.generation_cleanup_queue.lock();
            let mut due = Vec::new();
            let mut pending = VecDeque::new();
            while let Some(cleanup) = queue.pop_front() {
                if cleanup.due_at <= now {
                    due.push(cleanup.marked);
                } else {
                    pending.push_back(cleanup);
                }
            }
            *queue = pending;
            due
        };

        let outcomes = futures::stream::iter(due)
            .map(|marked| async move {
                match self
                    .cleanup_marked_stream_segments(
                        &marked.app_name,
                        &marked.stream_name,
                        &marked.segment_names,
                    )
                    .await
                {
                    Ok(count) => {
                        if count > 0 {
                            tracing::info!(
                                "Generation cleanup: deleted {} segments for {}/{}",
                                count,
                                marked.app_name,
                                marked.stream_name
                            );
                        }
                        (None, count)
                    }
                    Err(error) => {
                        tracing::warn!(
                            "Generation cleanup failed for {}/{}: {}; retrying",
                            marked.app_name,
                            marked.stream_name,
                            error
                        );
                        (Some(marked), 0)
                    }
                }
            })
            .buffer_unordered(Self::CLEANUP_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        let deleted = outcomes.iter().map(|(_, count)| count).sum();
        let failed = outcomes
            .into_iter()
            .filter_map(|(marked, _)| marked)
            .collect::<Vec<_>>();

        if !failed.is_empty() {
            let retry_at = time::Instant::now() + self.config.interval;
            let mut queue = self.generation_cleanup_queue.lock();
            queue.extend(failed.into_iter().map(|marked| ScheduledGenerationCleanup {
                due_at: retry_at,
                marked,
            }));
        }
        deleted
    }

    /// Cleanup segments older than the configured retention window immediately.
    ///
    /// This uses the same age-based policy as the periodic cleanup loop. It is
    /// intentionally not a full purge: shared backends may be used by multiple
    /// replicas, so startup/manual cleanup must not remove fresh segments.
    pub async fn cleanup_expired(&self) -> std::io::Result<usize> {
        if !self.cleanup_authority.should_cleanup() {
            tracing::trace!("Skipping HLS segment startup cleanup because this replica is not the cleanup authority");
            return Ok(0);
        }
        let generation_deleted = self.cleanup_due_generations().await;
        let age_deleted = self
            .storage
            .cleanup(self.config.retention.max(self.generation_cleanup_delay()))
            .await?;
        Ok(generation_deleted + age_deleted)
    }

    /// Cleanup only the explicitly captured segments for a stream.
    ///
    /// This is used during the post-end grace period so an old handler cannot
    /// delete segments created by a newer handler reusing the same app/stream key.
    pub async fn cleanup_marked_stream_segments(
        &self,
        app_name: &str,
        stream_name: &str,
        segment_names: &[String],
    ) -> std::io::Result<usize> {
        let mut deleted = 0;
        for segment_name in segment_names {
            if self
                .storage
                .exists(app_name, stream_name, segment_name)
                .await?
            {
                self.storage
                    .delete(app_name, stream_name, segment_name)
                    .await?;
                deleted += 1;
            }
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct NeverCleanupAuthority;

    impl CleanupAuthority for NeverCleanupAuthority {
        fn should_cleanup(&self) -> bool {
            false
        }
    }

    struct ToggleCleanupAuthority(AtomicBool);

    impl CleanupAuthority for ToggleCleanupAuthority {
        fn should_cleanup(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
    use bytes::Bytes;
    use std::time::Duration;
    type TestResult = std::io::Result<()>;

    #[tokio::test]
    async fn test_segment_manager_cleanup_expired_skips_without_authority() {
        let storage = Arc::new(MemoryStorage::new());

        storage
            .write("live", "room_123", "segment_0", Bytes::from_static(b"data"))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        let config = CleanupConfig {
            interval: Duration::from_hours(1),
            retention: Duration::from_millis(10),
            final_playlist_grace: Duration::from_mins(1),
            ended_segment_grace: Duration::from_mins(1),
            max_segments_per_stream: 0,
        };
        let manager = SegmentManager::new(storage.clone(), config)
            .with_cleanup_authority(Arc::new(NeverCleanupAuthority));

        let deleted = manager.cleanup_expired().await.unwrap();

        assert_eq!(deleted, 0);
        assert!(storage
            .exists("live", "room_123", "segment_0")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn generation_queue_waits_for_cleanup_authority_takeover() {
        let storage = Arc::new(MemoryStorage::new());
        storage
            .write(
                "live",
                "room_123",
                "old_generation",
                Bytes::from_static(b"old"),
            )
            .await
            .unwrap();
        let authority = Arc::new(ToggleCleanupAuthority(AtomicBool::new(false)));
        let manager = Arc::new(
            SegmentManager::new(
                storage.clone(),
                CleanupConfig {
                    interval: Duration::from_millis(10),
                    retention: Duration::from_hours(1),
                    final_playlist_grace: Duration::ZERO,
                    ended_segment_grace: Duration::ZERO,
                    max_segments_per_stream: 0,
                },
            )
            .with_cleanup_authority(authority.clone()),
        );
        manager.schedule_generation_cleanup(
            "live".to_string(),
            "room_123".to_string(),
            vec!["old_generation".to_string()],
        );
        let shutdown = CancellationToken::new();
        let cleanup_task = Arc::clone(&manager).start_cleanup_task(shutdown.clone());

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(storage
            .exists("live", "room_123", "old_generation")
            .await
            .unwrap());

        authority.0.store(true, Ordering::SeqCst);
        let deadline = time::Instant::now() + Duration::from_secs(1);
        while storage
            .exists("live", "room_123", "old_generation")
            .await
            .unwrap()
        {
            assert!(time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        shutdown.cancel();
        cleanup_task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn generation_cleanup_starts_after_playlist_and_segment_grace() {
        let storage = Arc::new(MemoryStorage::new());
        storage
            .write(
                "live",
                "room_123",
                "ended_generation",
                Bytes::from_static(b"ended"),
            )
            .await
            .unwrap();
        let manager = SegmentManager::new(
            storage.clone(),
            CleanupConfig {
                interval: Duration::from_secs(1),
                retention: Duration::from_hours(1),
                final_playlist_grace: Duration::from_secs(40),
                ended_segment_grace: Duration::from_secs(60),
                max_segments_per_stream: 0,
            },
        );
        manager.schedule_generation_cleanup(
            "live".to_string(),
            "room_123".to_string(),
            vec!["ended_generation".to_string()],
        );

        tokio::time::advance(Duration::from_secs(40)).await;
        manager.cleanup_due_generations().await;
        assert!(storage
            .exists("live", "room_123", "ended_generation")
            .await
            .unwrap());

        tokio::time::advance(Duration::from_secs(59)).await;
        manager.cleanup_due_generations().await;
        assert!(storage
            .exists("live", "room_123", "ended_generation")
            .await
            .unwrap());

        tokio::time::advance(Duration::from_secs(1)).await;
        manager.cleanup_due_generations().await;
        assert!(!storage
            .exists("live", "room_123", "ended_generation")
            .await
            .unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn manual_cleanup_processes_due_generation_queue() {
        let storage = Arc::new(MemoryStorage::new());
        storage
            .write(
                "live",
                "room_123",
                "ended_generation",
                Bytes::from_static(b"ended"),
            )
            .await
            .unwrap();
        let manager = SegmentManager::new(
            storage.clone(),
            CleanupConfig {
                interval: Duration::from_secs(30),
                retention: Duration::from_hours(1),
                final_playlist_grace: Duration::from_secs(10),
                ended_segment_grace: Duration::from_secs(20),
                max_segments_per_stream: 0,
            },
        );
        manager.schedule_generation_cleanup(
            "live".to_string(),
            "room_123".to_string(),
            vec!["ended_generation".to_string()],
        );

        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(manager.cleanup_expired().await.unwrap(), 1);
        assert!(!storage
            .exists("live", "room_123", "ended_generation")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_cleanup_marked_stream_segments_counts_existing_segments_only() {
        let storage = Arc::new(MemoryStorage::new());
        storage
            .write("live", "room_123", "old_seg_0", Bytes::from_static(b"old0"))
            .await
            .unwrap();
        let manager = SegmentManager::new(
            storage.clone(),
            CleanupConfig {
                interval: Duration::from_hours(1),
                retention: Duration::from_hours(1),
                final_playlist_grace: Duration::from_mins(1),
                ended_segment_grace: Duration::from_mins(1),
                max_segments_per_stream: 0,
            },
        );

        let deleted = manager
            .cleanup_marked_stream_segments(
                "live",
                "room_123",
                &["old_seg_0".to_string(), "missing_seg".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(deleted, 1);
        assert!(!storage
            .exists("live", "room_123", "old_seg_0")
            .await
            .unwrap());
    }

    /// Test that max_segments_per_stream config enforces count bounds.
    #[tokio::test]
    async fn test_segment_count_bound_deletes_oldest() {
        let storage = Arc::new(MemoryStorage::new());

        // Write 5 segments for one stream
        for i in 0..5 {
            storage
                .write(
                    "live",
                    "room_123",
                    &format!("seg_{i}"),
                    Bytes::from(format!("data_{i}")),
                )
                .await
                .unwrap();
        }
        assert_eq!(storage.key_count(), 5);

        // Enforce max 3 segments
        let deleted = storage
            .delete_oldest_stream_segments("live", "room_123", 3)
            .await
            .unwrap();

        assert_eq!(deleted, 2);
        assert_eq!(storage.key_count(), 3);

        // Oldest two (seg_0, seg_1) should be deleted
        assert!(!storage.exists("live", "room_123", "seg_0").await.unwrap());
        assert!(!storage.exists("live", "room_123", "seg_1").await.unwrap());
        // Newest three should remain
        assert!(storage.exists("live", "room_123", "seg_2").await.unwrap());
        assert!(storage.exists("live", "room_123", "seg_3").await.unwrap());
        assert!(storage.exists("live", "room_123", "seg_4").await.unwrap());
    }

    /// Test that count bound does nothing when under limit.
    #[tokio::test]
    async fn test_segment_count_bound_under_limit_no_op() {
        let storage = Arc::new(MemoryStorage::new());

        storage
            .write("live", "room_123", "seg_0", Bytes::from_static(b"data"))
            .await
            .unwrap();

        let deleted = storage
            .delete_oldest_stream_segments("live", "room_123", 5)
            .await
            .unwrap();

        assert_eq!(deleted, 0);
        assert_eq!(storage.key_count(), 1);
    }

    /// Test list_streams returns all distinct app/stream pairs.
    #[tokio::test]
    async fn test_list_streams() {
        let storage = Arc::new(MemoryStorage::new());

        storage
            .write("app1", "stream1", "seg0", Bytes::from_static(b"d"))
            .await
            .unwrap();
        storage
            .write("app1", "stream1", "seg1", Bytes::from_static(b"d"))
            .await
            .unwrap();
        storage
            .write("app1", "stream2", "seg0", Bytes::from_static(b"d"))
            .await
            .unwrap();
        storage
            .write("app2", "stream1", "seg0", Bytes::from_static(b"d"))
            .await
            .unwrap();

        let mut streams = storage.list_streams().await.unwrap();
        streams.sort();
        assert_eq!(streams.len(), 3);
        assert!(streams.contains(&("app1".to_string(), "stream1".to_string())));
        assert!(streams.contains(&("app1".to_string(), "stream2".to_string())));
        assert!(streams.contains(&("app2".to_string(), "stream1".to_string())));
    }

    /// Test count_stream_segments.
    #[tokio::test]
    async fn test_count_stream_segments() {
        let storage = Arc::new(MemoryStorage::new());

        storage
            .write("app1", "s1", "seg0", Bytes::from_static(b"d"))
            .await
            .unwrap();
        storage
            .write("app1", "s1", "seg1", Bytes::from_static(b"d"))
            .await
            .unwrap();
        storage
            .write("app1", "s2", "seg0", Bytes::from_static(b"d"))
            .await
            .unwrap();

        assert_eq!(
            storage.count_stream_segments("app1", "s1").await.unwrap(),
            2
        );
        assert_eq!(
            storage.count_stream_segments("app1", "s2").await.unwrap(),
            1
        );
        assert_eq!(
            storage.count_stream_segments("app1", "s3").await.unwrap(),
            0
        );
    }

    /// Test CleanupConfig with max_segments_per_stream.
    #[tokio::test]
    async fn test_cleanup_config_with_segment_limit() {
        let config = CleanupConfig {
            interval: Duration::from_secs(10),
            retention: Duration::from_mins(1),
            final_playlist_grace: Duration::from_mins(1),
            ended_segment_grace: Duration::from_mins(1),
            max_segments_per_stream: 100,
        };
        assert_eq!(config.max_segments_per_stream, 100);

        // Default should have unlimited segments
        let default_config = CleanupConfig::default();
        assert_eq!(default_config.max_segments_per_stream, 0);
    }

    #[test]
    fn count_cleanup_floor_preserves_live_playlist_and_refresh_reserve() {
        let storage = Arc::new(MemoryStorage::new());
        let manager = SegmentManager::new(
            storage,
            CleanupConfig {
                max_segments_per_stream: 1,
                ..CleanupConfig::default()
            },
        );

        assert_eq!(
            manager.effective_max_segments_per_stream(),
            HLS_PLAYLIST_RETENTION_RESERVE
        );
    }

    #[tokio::test]
    async fn count_cleanup_keeps_a_refreshable_playlist_window() -> TestResult {
        let storage = Arc::new(MemoryStorage::new());
        for sequence in 0..=HLS_PLAYLIST_RETENTION_RESERVE {
            storage
                .write(
                    "live",
                    "room",
                    &format!("segment_{sequence}"),
                    Bytes::from_static(b"segment"),
                )
                .await?;
        }

        let manager = Arc::new(SegmentManager::new(
            storage.clone(),
            CleanupConfig {
                interval: Duration::from_millis(10),
                retention: Duration::from_hours(1),
                max_segments_per_stream: 1,
                ..CleanupConfig::default()
            },
        ));
        let shutdown = CancellationToken::new();
        let task = manager.clone().start_cleanup_task(shutdown.clone());

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if storage.count_stream_segments("live", "room").await?
                    == HLS_PLAYLIST_RETENTION_RESERVE
                {
                    break Ok::<(), std::io::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("count cleanup should reach its safe floor")?;

        shutdown.cancel();
        task.await.unwrap();
        assert_eq!(
            storage.count_stream_segments("live", "room").await?,
            HLS_PLAYLIST_RETENTION_RESERVE
        );
        Ok(())
    }
}
