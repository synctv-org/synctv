// HLS Segment lifecycle manager
//
// Responsibilities:
// - Track active streams and their segments
// - Periodic cleanup of expired segments
// - Provide segment metadata for M3U8 generation
//
// Storage key format (flat structure):
// - Format: "app_name-stream_name-ts_name"
// - Example: "live-room123-a1b2c3d4e5f6"
// - No prefix, no extension, no directory hierarchy
//
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
    /// How often to run cleanup (e.g., every 10 seconds)
    pub interval: Duration,
    /// Delete segments older than this (e.g., 60 seconds)
    pub retention: Duration,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            retention: Duration::from_mins(1),
        }
    }
}

/// HLS Segment Manager
pub struct SegmentManager {
    storage: Arc<dyn HlsStorage>,
    config: CleanupConfig,
}

impl SegmentManager {
    /// Create new segment manager
    pub fn new(storage: Arc<dyn HlsStorage>, config: CleanupConfig) -> Self {
        Self { storage, config }
    }

    /// Start periodic cleanup task with optional cancellation support.
    ///
    /// This spawns a background task that periodically calls `storage.cleanup()`
    /// to delete expired segments. The task stops when the `CancellationToken` is cancelled.
    pub fn start_cleanup_task(self: Arc<Self>, shutdown_token: CancellationToken) {
        let manager = Arc::clone(&self);
        tokio::spawn(async move {
            manager.run_cleanup_loop(shutdown_token).await;
        });
    }

    /// Run the cleanup loop until cancelled.
    async fn run_cleanup_loop(&self, shutdown_token: CancellationToken) {
        let mut interval = time::interval(self.config.interval);

        tracing::info!(
            "Segment cleanup task started: interval={:?}, retention={:?}",
            self.config.interval,
            self.config.retention
        );

        loop {
            tokio::select! {
                _ = interval.tick() => {}
                () = shutdown_token.cancelled() => {
                    tracing::info!("Segment cleanup task shutting down");
                    break;
                }
            }

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

    /// Cleanup all expired segments immediately
    ///
    /// Note: Due to hash-based storage, we cannot filter by app/room.
    /// This will delete ALL expired segments across all rooms.
    ///
    /// For per-room cleanup, consider using separate storage instances per room.
    pub async fn cleanup_expired(&self) -> std::io::Result<usize> {
        self.storage.cleanup(Duration::from_secs(0)).await
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
    pub async fn cleanup_stream(&self, app_name: &str, stream_name: &str) -> std::io::Result<usize> {
        let prefix = crate::hls::hls_stream_storage_prefix(app_name, stream_name);
        let deleted = self.storage.delete_by_prefix(&prefix).await?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use bytes::Bytes;
    use std::time::Duration;

    #[tokio::test]
    async fn test_segment_manager_cleanup() {
        let storage = Arc::new(MemoryStorage::new());

        // Write some segments (flat key format)
        storage.write("live-room_123-segment_0", Bytes::from_static(b"data0"))
            .await
            .unwrap();
        storage.write("live-room_123-segment_1", Bytes::from_static(b"data1"))
            .await
            .unwrap();

        assert_eq!(storage.key_count().await, 2);

        // Sleep
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Create manager with short retention
        let config = CleanupConfig {
            interval: Duration::from_secs(3600), // Don't auto-run in test
            retention: Duration::from_millis(50),
        };

        let _manager = SegmentManager::new(storage.clone(), config);

        // Manual cleanup
        let deleted = storage.cleanup(Duration::from_millis(50))
            .await
            .unwrap();

        assert_eq!(deleted, 2);
        assert_eq!(storage.key_count().await, 0);
    }

    #[tokio::test]
    async fn test_segment_manager_cleanup_expired() {
        let storage = Arc::new(MemoryStorage::new());

        // Write segments for two rooms (flat key format)
        storage.write("live-room_123-segment_0", Bytes::from_static(b"data0"))
            .await
            .unwrap();
        storage.write("live-room_456-segment_0", Bytes::from_static(b"data1"))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        let config = CleanupConfig::default();
        let manager = SegmentManager::new(storage.clone(), config);

        // Cleanup all expired segments
        let deleted = manager.cleanup_expired().await.unwrap();

        // Both segments are deleted since they're expired
        assert_eq!(deleted, 2);
        assert!(!storage.exists("live-room_123-segment_0").await.unwrap());
        assert!(!storage.exists("live-room_456-segment_0").await.unwrap());
    }
}
