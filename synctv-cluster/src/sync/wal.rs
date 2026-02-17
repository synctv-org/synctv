//! Write-Ahead Log (WAL) for critical cluster events.
//!
//! Provides durable local storage for critical events when Redis XADD fails.
//! Events are stored in append-only log files and replayed on startup or
//! when Redis becomes available again.
//!
//! ## Kubernetes deployment: PersistentVolume required
//!
//! **WARNING**: The WAL uses the local filesystem. In Kubernetes, pod-local
//! storage is ephemeral -- data is lost when the pod restarts, is evicted, or
//! is rescheduled to a different node. This means WAL entries written during a
//! Redis outage will be permanently lost if the pod restarts before replay.
//!
//! To ensure WAL durability in Kubernetes, mount a **PersistentVolumeClaim**
//! at the WAL directory path. Example:
//!
//! ```yaml
//! volumes:
//!   - name: wal-storage
//!     persistentVolumeClaim:
//!       claimName: synctv-wal-pvc
//! volumeMounts:
//!   - name: wal-storage
//!     mountPath: /data/wal
//! ```
//!
//! Without a PersistentVolume, the WAL provides crash recovery only for
//! in-process failures (panic, OOM), not for pod-level restarts.
//!
//! ## File format
//!
//! Each WAL entry is a single line of JSON:
//! ```json
//! {"timestamp":"2024-01-01T12:00:00Z","event":{...}}
//! ```
//!
//! ## Rotation and cleanup
//!
//! - Files are rotated when they reach `max_file_size_bytes` (default: 10MB)
//! - Old files are deleted when total size exceeds `max_total_size_bytes` (default: 100MB)
//! - Files are named `events_{timestamp_ms}.wal` for easy sorting

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, info, warn};

use super::events::ClusterEvent;

/// WAL entry wrapping a cluster event with timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WalEntry {
    /// ISO 8601 timestamp when the event was written to the WAL
    timestamp: DateTime<Utc>,
    /// The cluster event payload
    event: ClusterEvent,
}

/// Write-Ahead Log for critical cluster events.
///
/// Stores events that failed to write to Redis Stream, allowing them to be
/// replayed when Redis recovers.
pub struct EventWal {
    /// Directory where WAL files are stored
    wal_dir: PathBuf,
    /// Currently active WAL file (for appending)
    current_file: tokio::sync::Mutex<Option<File>>,
    /// Current file path
    current_file_path: tokio::sync::Mutex<Option<PathBuf>>,
    /// Current file size in bytes (approximate)
    current_file_size: std::sync::atomic::AtomicU64,
    /// Maximum size of a single WAL file before rotation (default: 10MB)
    max_file_size_bytes: u64,
    /// Maximum total size of all WAL files before old files are deleted (default: 100MB)
    max_total_size_bytes: u64,
}

impl EventWal {
    /// Default maximum size for a single WAL file (10MB)
    const DEFAULT_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
    /// Default maximum total size for all WAL files (100MB)
    const DEFAULT_MAX_TOTAL_SIZE: u64 = 100 * 1024 * 1024;

    /// Create a new event WAL.
    ///
    /// If `wal_dir` doesn't exist, it will be created. Existing WAL files
    /// in the directory are left untouched and can be replayed with `replay()`.
    pub async fn new(wal_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&wal_dir)
            .await
            .with_context(|| format!("Failed to create WAL directory: {}", wal_dir.display()))?;

        Ok(Self {
            wal_dir,
            current_file: tokio::sync::Mutex::new(None),
            current_file_path: tokio::sync::Mutex::new(None),
            current_file_size: std::sync::atomic::AtomicU64::new(0),
            max_file_size_bytes: Self::DEFAULT_MAX_FILE_SIZE,
            max_total_size_bytes: Self::DEFAULT_MAX_TOTAL_SIZE,
        })
    }

    /// Create a new event WAL with custom size limits.
    pub async fn with_limits(
        wal_dir: PathBuf,
        max_file_size_bytes: u64,
        max_total_size_bytes: u64,
    ) -> Result<Self> {
        fs::create_dir_all(&wal_dir)
            .await
            .with_context(|| format!("Failed to create WAL directory: {}", wal_dir.display()))?;

        Ok(Self {
            wal_dir,
            current_file: tokio::sync::Mutex::new(None),
            current_file_path: tokio::sync::Mutex::new(None),
            current_file_size: std::sync::atomic::AtomicU64::new(0),
            max_file_size_bytes,
            max_total_size_bytes,
        })
    }

    /// Append a critical event to the WAL.
    ///
    /// If the current file exceeds `max_file_size_bytes`, it is rotated.
    /// After rotation, old files exceeding `max_total_size_bytes` are deleted.
    pub async fn append(&self, event: ClusterEvent) -> Result<()> {
        let entry = WalEntry {
            timestamp: Utc::now(),
            event,
        };

        let mut entry_json = serde_json::to_string(&entry)
            .context("Failed to serialize WAL entry")?;
        entry_json.push('\n');
        let entry_bytes = entry_json.as_bytes();

        let mut file_guard = self.current_file.lock().await;
        let mut path_guard = self.current_file_path.lock().await;

        // Rotate if current file is too large
        let current_size = self.current_file_size.load(std::sync::atomic::Ordering::Relaxed);
        if current_size + entry_bytes.len() as u64 > self.max_file_size_bytes {
            self.rotate_locked(&mut file_guard, &mut path_guard).await?;
        }

        // Open a new file if none exists
        if file_guard.is_none() {
            self.open_new_file_locked(&mut file_guard, &mut path_guard).await?;
        }

        // Write to current file
        if let Some(ref mut file) = *file_guard {
            file.write_all(entry_bytes).await
                .context("Failed to write to WAL file")?;
            file.sync_data().await
                .context("Failed to sync WAL file")?;
            self.current_file_size.fetch_add(entry_bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
            debug!(
                event_type = %entry.event.event_type(),
                wal_file = ?path_guard.as_ref().map(|p| p.display()),
                "Appended event to WAL"
            );
        }

        Ok(())
    }

    /// Replay all events from all WAL files in chronological order.
    ///
    /// Returns a list of events sorted by timestamp (oldest first).
    /// Does NOT delete the WAL files after replay - use `clear()` to remove
    /// successfully replayed events.
    pub async fn replay(&self) -> Result<Vec<ClusterEvent>> {
        let mut wal_files = self.list_wal_files().await?;
        wal_files.sort();

        let mut events = Vec::new();

        for file_path in wal_files {
            match self.replay_file(&file_path).await {
                Ok(file_events) => {
                    events.extend(file_events);
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        file = %file_path.display(),
                        "Failed to replay WAL file, skipping"
                    );
                }
            }
        }

        info!(
            event_count = events.len(),
            "Replayed events from WAL"
        );

        Ok(events)
    }

    /// Replay events from a single WAL file.
    async fn replay_file(&self, file_path: &Path) -> Result<Vec<ClusterEvent>> {
        let file = File::open(file_path).await
            .with_context(|| format!("Failed to open WAL file: {}", file_path.display()))?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        let mut events = Vec::new();

        while let Some(line) = lines.next_line().await
            .with_context(|| format!("Failed to read from WAL file: {}", file_path.display()))? {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<WalEntry>(&line) {
                Ok(entry) => {
                    events.push(entry.event);
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        line = %line,
                        file = %file_path.display(),
                        "Failed to parse WAL entry, skipping line"
                    );
                }
            }
        }

        Ok(events)
    }

    /// Clear all WAL files (after successful replay and Redis write).
    ///
    /// Closes the current file (if any) and deletes all WAL files in the directory.
    pub async fn clear(&self) -> Result<()> {
        // Close current file
        let mut file_guard = self.current_file.lock().await;
        let mut path_guard = self.current_file_path.lock().await;
        *file_guard = None;
        *path_guard = None;
        self.current_file_size.store(0, std::sync::atomic::Ordering::Relaxed);
        drop(file_guard);
        drop(path_guard);

        // Delete all WAL files
        let wal_files = self.list_wal_files().await?;
        for file_path in wal_files {
            if let Err(e) = fs::remove_file(&file_path).await {
                warn!(
                    error = %e,
                    file = %file_path.display(),
                    "Failed to delete WAL file"
                );
            }
        }

        info!("Cleared all WAL files");
        Ok(())
    }

    /// Rotate the current WAL file (closes it and prepares for a new one).
    /// Caller must hold locks on `current_file` and `current_file_path`.
    async fn rotate_locked(
        &self,
        file_guard: &mut Option<File>,
        path_guard: &mut Option<PathBuf>,
    ) -> Result<()> {
        if let Some(ref mut file) = *file_guard {
            file.sync_all().await
                .context("Failed to sync WAL file before rotation")?;
        }
        *file_guard = None;
        *path_guard = None;
        self.current_file_size.store(0, std::sync::atomic::Ordering::Relaxed);

        debug!("Rotated WAL file");

        // Cleanup old files if total size exceeds limit
        self.cleanup_old_files().await?;

        Ok(())
    }

    /// Open a new WAL file for appending.
    /// Caller must hold locks on `current_file` and `current_file_path`.
    async fn open_new_file_locked(
        &self,
        file_guard: &mut Option<File>,
        path_guard: &mut Option<PathBuf>,
    ) -> Result<()> {
        let timestamp_ms = Utc::now().timestamp_millis();
        let file_name = format!("events_{}.wal", timestamp_ms);
        let file_path = self.wal_dir.join(file_name);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)
            .await
            .with_context(|| format!("Failed to create WAL file: {}", file_path.display()))?;

        *file_guard = Some(file);
        *path_guard = Some(file_path.clone());
        self.current_file_size.store(0, std::sync::atomic::Ordering::Relaxed);

        debug!(file = %file_path.display(), "Opened new WAL file");

        Ok(())
    }

    /// List all WAL files in the directory, sorted by filename (timestamp).
    async fn list_wal_files(&self) -> Result<Vec<PathBuf>> {
        let mut entries = fs::read_dir(&self.wal_dir).await
            .with_context(|| format!("Failed to read WAL directory: {}", self.wal_dir.display()))?;

        let mut files = Vec::new();
        while let Some(entry) = entries.next_entry().await
            .with_context(|| format!("Failed to read directory entry in: {}", self.wal_dir.display()))? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("wal") {
                files.push(path);
            }
        }

        files.sort();
        Ok(files)
    }

    /// Delete old WAL files if total size exceeds `max_total_size_bytes`.
    async fn cleanup_old_files(&self) -> Result<()> {
        let mut wal_files = self.list_wal_files().await?;
        wal_files.sort();

        // Calculate total size
        let mut total_size = 0u64;
        let mut file_sizes = Vec::new();
        for file_path in &wal_files {
            match fs::metadata(file_path).await {
                Ok(metadata) => {
                    let size = metadata.len();
                    total_size += size;
                    file_sizes.push((file_path.clone(), size));
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        file = %file_path.display(),
                        "Failed to get WAL file metadata"
                    );
                }
            }
        }

        if total_size <= self.max_total_size_bytes {
            return Ok(()); // No cleanup needed
        }

        // Delete oldest files until total size is under limit
        let mut deleted_count = 0;
        for (file_path, size) in file_sizes {
            if total_size <= self.max_total_size_bytes {
                break;
            }
            if let Err(e) = fs::remove_file(&file_path).await {
                error!(
                    error = %e,
                    file = %file_path.display(),
                    "Failed to delete old WAL file"
                );
            } else {
                total_size = total_size.saturating_sub(size);
                deleted_count += 1;
                debug!(file = %file_path.display(), "Deleted old WAL file");
            }
        }

        if deleted_count > 0 {
            info!(
                deleted_count = deleted_count,
                total_size_mb = total_size / (1024 * 1024),
                "Cleaned up old WAL files"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core::models::id::{RoomId, UserId};

    #[tokio::test]
    async fn test_wal_append_and_replay() {
        let temp_dir = tempfile::tempdir().unwrap();
        let wal = EventWal::new(temp_dir.path().to_path_buf()).await.unwrap();

        let event = ClusterEvent::ChatMessage {
            event_id: "test123".to_string(),
            room_id: RoomId::from_string("room1".to_string()),
            user_id: UserId::from_string("user1".to_string()),
            username: "testuser".to_string(),
            message: "Hello WAL!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        wal.append(event.clone()).await.unwrap();

        let replayed = wal.replay().await.unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].event_type(), "chat_message");
    }

    #[tokio::test]
    async fn test_wal_rotation() {
        let temp_dir = tempfile::tempdir().unwrap();
        // Set very small file size to force rotation
        let wal = EventWal::with_limits(temp_dir.path().to_path_buf(), 100, 10000)
            .await
            .unwrap();

        // Append multiple events to trigger rotation
        for i in 0..5 {
            let event = ClusterEvent::ChatMessage {
                event_id: format!("test{}", i),
                room_id: RoomId::from_string("room1".to_string()),
                user_id: UserId::from_string("user1".to_string()),
                username: "testuser".to_string(),
                message: format!("Message {} with enough text to exceed 100 bytes", i),
                timestamp: Utc::now(),
                position: None,
                color: None,
            };
            wal.append(event).await.unwrap();
        }

        let replayed = wal.replay().await.unwrap();
        assert_eq!(replayed.len(), 5);
    }

    #[tokio::test]
    async fn test_wal_clear() {
        let temp_dir = tempfile::tempdir().unwrap();
        let wal = EventWal::new(temp_dir.path().to_path_buf()).await.unwrap();

        let event = ClusterEvent::ChatMessage {
            event_id: "test123".to_string(),
            room_id: RoomId::from_string("room1".to_string()),
            user_id: UserId::from_string("user1".to_string()),
            username: "testuser".to_string(),
            message: "Hello WAL!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        wal.append(event).await.unwrap();
        wal.clear().await.unwrap();

        let replayed = wal.replay().await.unwrap();
        assert_eq!(replayed.len(), 0);
    }
}
