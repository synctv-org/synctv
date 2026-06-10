// HLS Segment lifecycle manager
// Responsibilities:
// - Track active streams and their segments
// - Periodic cleanup of expired segments
// - Provide segment metadata for M3U8 generation
// Public segment names remain slash-free. Storage backends may internally map
// minute-prefixed segment names into directory/prefix buckets for efficient
// cleanup.
// Architecture:
// - Storage layer: Pure KV storage (FileStorage/MemoryStorage/OssStorage)
// - SegmentManager: Business logic (retention policy, cleanup scheduling)
// - HLS layer: M3U8 generation and HTTP serving

use crate::storage::HlsStorage;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tokio_util::sync::CancellationToken;

/// Segment cleanup configuration
#[derive(Debug, Clone)]
pub struct CleanupConfig {
    /// How often to run cleanup.
    pub interval: Duration,
    /// Delete segments older than this.
    pub retention: Duration,
    /// Maximum number of segments per stream. 0 means unlimited (time-based only).
    /// When exceeded, the oldest segments for that stream are deleted.
    pub max_segments_per_stream: usize,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_mins(1),
            retention: Duration::from_mins(3),
            max_segments_per_stream: 0,
        }
    }
}

/// HLS Segment Manager
pub struct SegmentManager {
    storage: Arc<dyn HlsStorage>,
    config: CleanupConfig,
    cleanup_authority: Arc<dyn CleanupAuthority>,
}

/// A stream that has ended and exposed an explicit set of segment names safe to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkedStreamCleanup {
    pub app_name: String,
    pub stream_name: String,
    pub segment_names: Vec<String>,
}

/// Trait for checking which streams are marked for cleanup.
/// Implemented by the stream registry to allow `SegmentManager`
/// to query cleanup eligibility without tight coupling.
pub trait StreamCleanupChecker: Send + Sync {
    /// Returns streams marked for cleanup (handler ended, grace period started)
    /// together with the exact segment names captured when cleanup was marked.
    fn get_streams_marked_for_cleanup(&self) -> Vec<MarkedStreamCleanup>;
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
    /// Create new segment manager
    pub fn new(storage: Arc<dyn HlsStorage>, config: CleanupConfig) -> Self {
        Self {
            storage,
            config,
            cleanup_authority: Arc::new(AlwaysCleanupAuthority),
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
            manager.run_cleanup_loop(shutdown_token, None).await;
        })
    }

    /// Start periodic cleanup task with stream registry for priority cleanup.
    ///
    /// The stream registry is used to identify streams marked for cleanup
    /// (handler ended but still in grace period). These streams can be
    /// cleaned up earlier based on memory pressure rather than waiting
    /// for the full grace period.
    ///
    /// Returns the `JoinHandle` so callers can wait for graceful shutdown or abort if needed.
    pub fn start_cleanup_task_with_registry(
        self: Arc<Self>,
        shutdown_token: CancellationToken,
        registry: Arc<dyn StreamCleanupChecker>,
    ) -> tokio::task::JoinHandle<()> {
        let manager = Arc::clone(&self);
        tokio::spawn(async move {
            manager
                .run_cleanup_loop(shutdown_token, Some(registry))
                .await;
        })
    }

    /// Run the cleanup loop until cancelled.
    async fn run_cleanup_loop(
        &self,
        shutdown_token: CancellationToken,
        registry: Option<Arc<dyn StreamCleanupChecker>>,
    ) {
        let mut interval = time::interval(self.config.interval);

        tracing::info!(
            "Segment cleanup task started: interval={:?}, retention={:?}, max_segments_per_stream={}",
            self.config.interval,
            self.config.retention,
            if self.config.max_segments_per_stream == 0 {
                "unlimited".to_string()
            } else {
                self.config.max_segments_per_stream.to_string()
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

            // First, clean up streams marked for cleanup (priority cleanup)
            // This helps reduce memory usage when handlers end but are still
            // in the 60-second grace period
            if let Some(ref registry) = registry {
                let marked_streams = registry.get_streams_marked_for_cleanup();
                for marked in marked_streams {
                    match self
                        .cleanup_marked_stream_segments(
                            &marked.app_name,
                            &marked.stream_name,
                            &marked.segment_names,
                        )
                        .await
                    {
                        Ok(deleted) => {
                            if deleted > 0 {
                                tracing::info!(
                                    "Priority cleanup: deleted {} segments for marked stream {}/{}",
                                    deleted,
                                    marked.app_name,
                                    marked.stream_name
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Priority cleanup failed for {}/{}: {}",
                                marked.app_name,
                                marked.stream_name,
                                e
                            );
                        }
                    }
                }
            }

            // Enforce per-stream segment count bound (if configured)
            if self.config.max_segments_per_stream > 0 {
                match self.storage.list_streams().await {
                    Ok(streams) => {
                        for (app, stream) in streams {
                            match self
                                .storage
                                .delete_oldest_stream_segments(
                                    &app,
                                    &stream,
                                    self.config.max_segments_per_stream,
                                )
                                .await
                            {
                                Ok(deleted) if deleted > 0 => {
                                    tracing::info!(
                                        "Count-based cleanup: deleted {} excess segments for {}/{} (max {})",
                                        deleted, app, stream, self.config.max_segments_per_stream
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
                    }
                    Err(e) => {
                        tracing::warn!("Failed to list streams for count-based cleanup: {}", e);
                    }
                }
            }

            // Then, clean up expired segments by time
            match self.storage.cleanup(self.config.retention).await {
                Ok(deleted) => {
                    if deleted > 0 {
                        tracing::info!(
                            "Cleaned up {} expired segments (older than {:?})",
                            deleted,
                            self.config.retention
                        );
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
        self.storage.cleanup(self.config.retention).await
    }

    /// Cleanup all segments for a specific stream immediately.
    ///
    /// Called when a stream ends (publisher disconnect, idle timeout) to
    /// immediately free memory rather than waiting for the periodic cleanup cycle.
    ///
    /// # Arguments
    /// * `app_name` - Application/room name (e.g., "room123")
    /// * `stream_name` - Stream/media name (e.g., "media456")
    ///
    /// # Returns
    /// Number of segments deleted
    pub async fn cleanup_stream(
        &self,
        app_name: &str,
        stream_name: &str,
    ) -> std::io::Result<usize> {
        let deleted = self
            .storage
            .delete_app_stream(app_name, stream_name)
            .await?;
        if deleted > 0 {
            tracing::info!(
                "Cleaned up {} segments for stream {}/{}",
                deleted,
                app_name,
                stream_name
            );
        }
        Ok(deleted)
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

    struct NeverCleanupAuthority;

    impl CleanupAuthority for NeverCleanupAuthority {
        fn should_cleanup(&self) -> bool {
            false
        }
    }
    use bytes::Bytes;
    use std::time::Duration;

    struct StaticCleanupChecker {
        marked: Vec<MarkedStreamCleanup>,
    }

    impl StreamCleanupChecker for StaticCleanupChecker {
        fn get_streams_marked_for_cleanup(&self) -> Vec<MarkedStreamCleanup> {
            self.marked.clone()
        }
    }

    #[tokio::test]
    async fn test_segment_manager_cleanup() {
        let storage = Arc::new(MemoryStorage::new());

        // Write some segments
        storage
            .write(
                "live",
                "room_123",
                "segment_0",
                Bytes::from_static(b"data0"),
            )
            .await
            .unwrap();
        storage
            .write(
                "live",
                "room_123",
                "segment_1",
                Bytes::from_static(b"data1"),
            )
            .await
            .unwrap();

        assert_eq!(storage.key_count(), 2);

        // Sleep
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Create manager with short retention
        let config = CleanupConfig {
            interval: Duration::from_hours(1), // Don't auto-run in test
            retention: Duration::from_millis(50),
            max_segments_per_stream: 0,
        };

        let _manager = SegmentManager::new(storage.clone(), config);

        // Manual cleanup
        let deleted = storage.cleanup(Duration::from_millis(50)).await.unwrap();

        assert_eq!(deleted, 2);
        assert_eq!(storage.key_count(), 0);
    }

    #[tokio::test]
    async fn test_segment_manager_cleanup_expired() {
        let storage = Arc::new(MemoryStorage::new());

        // Write segments for two rooms
        storage
            .write(
                "live",
                "room_123",
                "segment_0",
                Bytes::from_static(b"data0"),
            )
            .await
            .unwrap();
        storage
            .write(
                "live",
                "room_456",
                "segment_0",
                Bytes::from_static(b"data1"),
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        let config = CleanupConfig {
            interval: Duration::from_hours(1),
            retention: Duration::from_millis(10),
            max_segments_per_stream: 0,
        };
        let manager = SegmentManager::new(storage.clone(), config);

        // Cleanup segments older than the configured retention window.
        let deleted = manager.cleanup_expired().await.unwrap();

        // Both segments are deleted since they're older than retention.
        assert_eq!(deleted, 2);
        assert!(!storage
            .exists("live", "room_123", "segment_0")
            .await
            .unwrap());
        assert!(!storage
            .exists("live", "room_456", "segment_0")
            .await
            .unwrap());
    }

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
    async fn test_priority_cleanup_only_deletes_captured_segments() {
        let storage = Arc::new(MemoryStorage::new());
        storage
            .write("live", "room_123", "old_seg_0", Bytes::from_static(b"old0"))
            .await
            .unwrap();
        storage
            .write("live", "room_123", "old_seg_1", Bytes::from_static(b"old1"))
            .await
            .unwrap();
        storage
            .write("live", "room_123", "new_seg_0", Bytes::from_static(b"new0"))
            .await
            .unwrap();

        let manager = Arc::new(SegmentManager::new(
            storage.clone(),
            CleanupConfig {
                interval: Duration::from_millis(10),
                retention: Duration::from_hours(1),
                max_segments_per_stream: 0,
            },
        ));
        let checker = Arc::new(StaticCleanupChecker {
            marked: vec![MarkedStreamCleanup {
                app_name: "live".to_string(),
                stream_name: "room_123".to_string(),
                segment_names: vec!["old_seg_0".to_string(), "old_seg_1".to_string()],
            }],
        });
        let shutdown = CancellationToken::new();
        let join = manager
            .clone()
            .start_cleanup_task_with_registry(shutdown.clone(), checker);

        tokio::time::sleep(Duration::from_millis(30)).await;
        shutdown.cancel();
        join.await.unwrap();

        assert!(!storage
            .exists("live", "room_123", "old_seg_0")
            .await
            .unwrap());
        assert!(!storage
            .exists("live", "room_123", "old_seg_1")
            .await
            .unwrap());
        assert!(storage
            .exists("live", "room_123", "new_seg_0")
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
            max_segments_per_stream: 100,
        };
        assert_eq!(config.max_segments_per_stream, 100);

        // Default should have unlimited segments
        let default_config = CleanupConfig::default();
        assert_eq!(default_config.max_segments_per_stream, 0);
    }
}
