// File system storage backend for HLS
// Default storage backend using local filesystem
// With structured directory-based paths: base_path/segments/minute/app/stream/name

use super::HlsStorage;
use crate::storage::{
    minute_bucket_is_expired, segment_minute_bucket, validate_component, validate_storage_key,
    HLS_SEGMENTS_ROOT,
};
use async_trait::async_trait;
use bytes::Bytes;
use std::io::{Error, ErrorKind, Result};
use std::path::PathBuf;
use std::time::Duration;
use tokio::fs;

/// File system storage backend
pub struct FileStorage {
    base_path: PathBuf,
}

impl FileStorage {
    /// Create new file storage with base path
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
        }
    }

    /// Get full file path.
    ///
    /// HLS segment names must start with an epoch-minute bucket
    /// (`unix_minutes_random`) and are stored as
    /// `base_path/segments/minute/app/stream/name`.
    fn get_path(&self, app: &str, stream: &str, name: &str) -> Result<PathBuf> {
        let bucket = segment_minute_bucket(name).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "HLS segment name must start with an epoch-minute bucket",
            )
        })?;

        Ok(self
            .base_path
            .join(HLS_SEGMENTS_ROOT)
            .join(bucket)
            .join(app)
            .join(stream)
            .join(name))
    }

    fn segments_root_path(&self) -> PathBuf {
        self.base_path.join(HLS_SEGMENTS_ROOT)
    }

    fn bucket_app_path(&self, bucket: &str, app: &str) -> PathBuf {
        self.segments_root_path().join(bucket).join(app)
    }

    fn bucket_stream_path(&self, bucket: &str, app: &str, stream: &str) -> PathBuf {
        self.bucket_app_path(bucket, app).join(stream)
    }

    async fn count_files_under(dir: &std::path::Path) -> Result<usize> {
        let mut deleted = 0;
        let mut stack = vec![dir.to_path_buf()];

        while let Some(current_dir) = stack.pop() {
            let mut entries = match fs::read_dir(&current_dir).await {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };

            loop {
                let entry = match entries.next_entry().await {
                    Ok(Some(entry)) => entry,
                    Ok(None) => break,
                    Err(e) if e.kind() == ErrorKind::NotFound => break,
                    Err(e) => return Err(e),
                };
                let Ok(file_type) = entry.file_type().await else {
                    continue;
                };
                if file_type.is_dir() {
                    stack.push(entry.path());
                } else if file_type.is_file() {
                    deleted += 1;
                }
            }
        }

        Ok(deleted)
    }

    async fn remove_dir_all_counting_files(dir: &std::path::Path) -> Result<usize> {
        if !fs::try_exists(dir).await.unwrap_or(false) {
            return Ok(0);
        }

        let deleted = match Self::count_files_under(dir).await {
            Ok(deleted) => deleted,
            Err(e) if e.kind() == ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };

        match fs::remove_dir_all(dir).await {
            Ok(()) => Ok(deleted),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl HlsStorage for FileStorage {
    async fn write(&self, app: &str, stream: &str, name: &str, data: Bytes) -> Result<()> {
        validate_storage_key(app, stream, name)?;
        let file_path = self.get_path(app, stream, name)?;
        let size = data.len();

        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&file_path, data).await?;

        tracing::trace!(
            "Wrote: {:?} ({} bytes) for {}/{}/{}",
            file_path,
            size,
            app,
            stream,
            name
        );

        Ok(())
    }

    async fn read(&self, app: &str, stream: &str, name: &str) -> Result<Bytes> {
        validate_storage_key(app, stream, name)?;
        let file_path = self.get_path(app, stream, name)?;
        let data = fs::read(&file_path).await?;

        tracing::trace!(
            "Read: {:?} ({} bytes) for {}/{}/{}",
            file_path,
            data.len(),
            app,
            stream,
            name
        );

        Ok(Bytes::from(data))
    }

    async fn delete(&self, app: &str, stream: &str, name: &str) -> Result<()> {
        validate_storage_key(app, stream, name)?;
        let file_path = self.get_path(app, stream, name)?;

        if fs::try_exists(&file_path).await.unwrap_or(false) {
            fs::remove_file(&file_path).await?;
            tracing::trace!("Deleted: {:?} for {}/{}/{}", file_path, app, stream, name);
        }

        Ok(())
    }

    async fn exists(&self, app: &str, stream: &str, name: &str) -> Result<bool> {
        validate_storage_key(app, stream, name)?;
        let file_path = self.get_path(app, stream, name)?;
        fs::try_exists(&file_path).await
    }

    async fn delete_app_stream(&self, app: &str, stream: &str) -> Result<usize> {
        validate_component(app, "app")?;
        validate_component(stream, "stream")?;
        let mut deleted = 0;

        let segments_root = self.segments_root_path();
        if fs::try_exists(&segments_root).await.unwrap_or(false) {
            let mut bucket_dirs = fs::read_dir(&segments_root).await?;
            while let Some(bucket_entry) = bucket_dirs.next_entry().await? {
                let Ok(file_type) = bucket_entry.file_type().await else {
                    continue;
                };
                if !file_type.is_dir() {
                    continue;
                }
                let bucket_name = bucket_entry.file_name();
                let Some(bucket_name) = bucket_name.to_str() else {
                    continue;
                };
                let stream_dir = self.bucket_stream_path(bucket_name, app, stream);
                deleted += Self::remove_dir_all_counting_files(&stream_dir).await?;
                let _ = fs::remove_dir(self.bucket_app_path(bucket_name, app)).await;
                let _ = fs::remove_dir(bucket_entry.path()).await;
            }
        }

        let _ = fs::remove_dir(segments_root).await;

        tracing::debug!(
            "delete_app_stream {}/{}: deleted {} files",
            app,
            stream,
            deleted
        );
        Ok(deleted)
    }

    async fn delete_app(&self, app: &str) -> Result<usize> {
        validate_component(app, "app")?;
        let mut deleted = 0;

        let segments_root = self.segments_root_path();
        if fs::try_exists(&segments_root).await.unwrap_or(false) {
            let mut bucket_dirs = fs::read_dir(&segments_root).await?;
            while let Some(bucket_entry) = bucket_dirs.next_entry().await? {
                let Ok(file_type) = bucket_entry.file_type().await else {
                    continue;
                };
                if !file_type.is_dir() {
                    continue;
                }
                let bucket_name = bucket_entry.file_name();
                let Some(bucket_name) = bucket_name.to_str() else {
                    continue;
                };
                deleted +=
                    Self::remove_dir_all_counting_files(&self.bucket_app_path(bucket_name, app))
                        .await?;
                let _ = fs::remove_dir(bucket_entry.path()).await;
            }
        }

        let _ = fs::remove_dir(segments_root).await;

        tracing::debug!("delete_app {}: deleted {} files", app, deleted);
        Ok(deleted)
    }

    async fn cleanup(&self, older_than: Duration) -> Result<usize> {
        let segments_root = self.segments_root_path();
        if !fs::try_exists(&segments_root).await.unwrap_or(false) {
            tracing::debug!("Cleanup segments root does not exist: {:?}", segments_root);
            return Ok(0);
        }

        let mut deleted = 0;

        let mut bucket_dirs = fs::read_dir(&segments_root).await?;
        while let Some(bucket_entry) = bucket_dirs.next_entry().await? {
            let Ok(file_type) = bucket_entry.file_type().await else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }

            let bucket_name = bucket_entry.file_name();
            let Some(bucket_name) = bucket_name.to_str() else {
                continue;
            };

            if minute_bucket_is_expired(bucket_name, older_than) {
                deleted += Self::remove_dir_all_counting_files(&bucket_entry.path()).await?;
                tracing::trace!("Deleted expired minute bucket: {:?}", bucket_entry.path());
            }
        }

        let _ = fs::remove_dir(segments_root).await;

        tracing::info!(
            "Cleanup completed: scanned {:?}, deleted {} files older than {:?}",
            self.segments_root_path(),
            deleted,
            older_than
        );

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::tempdir;

    fn current_bucket() -> String {
        (Utc::now().timestamp() / 60).to_string()
    }

    fn old_bucket() -> String {
        ((Utc::now().timestamp() - chrono::Duration::minutes(5).num_seconds()) / 60).to_string()
    }

    fn segment_name(bucket: &str, suffix: &str) -> String {
        format!("{bucket}_{suffix}")
    }

    #[tokio::test]
    async fn test_file_storage_write_read() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        let segment = segment_name(&current_bucket(), "segment_0");
        let data = Bytes::from_static(b"test segment data");
        let result = storage
            .write("live", "room_123", &segment, data.clone())
            .await;
        assert!(result.is_ok());

        let read_data = storage.read("live", "room_123", &segment).await.unwrap();
        assert_eq!(data, read_data);

        let exists = storage.exists("live", "room_123", &segment).await.unwrap();
        assert!(exists);

        let result = storage.delete("live", "room_123", &segment).await;
        assert!(result.is_ok());

        let exists = storage.exists("live", "room_123", &segment).await.unwrap();
        assert!(!exists);
    }

    #[tokio::test]
    async fn test_file_storage_public_url() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        let url = storage
            .get_public_url("live", "room_123", "segment_0")
            .await
            .unwrap();
        assert_eq!(url, None);
    }

    #[tokio::test]
    async fn test_file_storage_cleanup() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());
        let old_bucket = old_bucket();
        let current_bucket = current_bucket();
        let old_segment_0 = segment_name(&old_bucket, "segment_0");
        let old_segment_1 = segment_name(&old_bucket, "segment_1");
        let old_segment_2 = segment_name(&old_bucket, "segment_2");
        let current_segment = segment_name(&current_bucket, "segment_3");

        storage
            .write(
                "live",
                "room_123",
                &old_segment_0,
                Bytes::from_static(b"data0"),
            )
            .await
            .unwrap();
        storage
            .write(
                "live",
                "room_123",
                &old_segment_1,
                Bytes::from_static(b"data1"),
            )
            .await
            .unwrap();
        storage
            .write(
                "live",
                "room_456",
                &old_segment_2,
                Bytes::from_static(b"data2"),
            )
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        storage
            .write(
                "live",
                "room_123",
                &current_segment,
                Bytes::from_static(b"data3"),
            )
            .await
            .unwrap();

        let deleted = storage.cleanup(Duration::from_mins(3)).await.unwrap();

        assert_eq!(deleted, 3);
        assert!(!storage
            .exists("live", "room_123", &old_segment_0)
            .await
            .unwrap());
        assert!(!storage
            .exists("live", "room_123", &old_segment_1)
            .await
            .unwrap());
        assert!(storage
            .exists("live", "room_123", &current_segment)
            .await
            .unwrap());
        assert!(!storage
            .exists("live", "room_456", &old_segment_2)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_file_storage_minute_bucket_path_and_cleanup() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        let current_bucket = current_bucket();
        let old_bucket = old_bucket();
        let current_segment = segment_name(&current_bucket, "current");
        let old_segment = segment_name(&old_bucket, "old");

        storage
            .write(
                "live",
                "room_123",
                &current_segment,
                Bytes::from_static(b"current"),
            )
            .await
            .unwrap();
        storage
            .write("live", "room_123", &old_segment, Bytes::from_static(b"old"))
            .await
            .unwrap();

        assert!(temp_dir
            .path()
            .join(HLS_SEGMENTS_ROOT)
            .join(&current_bucket)
            .join("live")
            .join("room_123")
            .join(&current_segment)
            .exists());

        let deleted = storage.cleanup(Duration::from_mins(3)).await.unwrap();
        assert_eq!(deleted, 1);
        assert!(storage
            .exists("live", "room_123", &current_segment)
            .await
            .unwrap());
        assert!(!storage
            .exists("live", "room_123", &old_segment)
            .await
            .unwrap());
        assert!(!temp_dir
            .path()
            .join(HLS_SEGMENTS_ROOT)
            .join(&old_bucket)
            .exists());
    }

    #[tokio::test]
    async fn test_file_storage_rejects_unbucketed_segments() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        assert!(storage
            .write("live", "room_123", "segment_0", Bytes::from_static(b"data"))
            .await
            .is_err());
        assert!(storage.read("live", "room_123", "segment_0").await.is_err());
        assert!(storage
            .exists("live", "room_123", "segment_0")
            .await
            .is_err());
        assert!(storage
            .delete("live", "room_123", "segment_0")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_file_storage_cleanup_ignores_numeric_top_level_dirs() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        let numeric_app_dir = temp_dir.path().join("123").join("media");
        fs::create_dir_all(&numeric_app_dir).await.unwrap();
        fs::write(numeric_app_dir.join("active.ts"), b"active")
            .await
            .unwrap();

        let deleted = storage.cleanup(Duration::from_mins(3)).await.unwrap();

        assert_eq!(deleted, 0);
        assert!(numeric_app_dir.join("active.ts").exists());
    }

    #[tokio::test]
    async fn test_file_storage_delete_app_stream() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());
        let bucket = current_bucket();
        let seg0 = segment_name(&bucket, "seg0");
        let seg1 = segment_name(&bucket, "seg1");
        let seg2 = segment_name(&bucket, "seg2");

        storage
            .write("app1", "stream1", &seg0, Bytes::from_static(b"d0"))
            .await
            .unwrap();
        storage
            .write("app1", "stream1", &seg1, Bytes::from_static(b"d1"))
            .await
            .unwrap();
        storage
            .write("app1", "stream2", &seg2, Bytes::from_static(b"d2"))
            .await
            .unwrap();

        let deleted = storage.delete_app_stream("app1", "stream1").await.unwrap();
        assert_eq!(deleted, 2);

        assert!(!storage.exists("app1", "stream1", &seg0).await.unwrap());
        assert!(!storage.exists("app1", "stream1", &seg1).await.unwrap());
        assert!(storage.exists("app1", "stream2", &seg2).await.unwrap());
    }

    #[tokio::test]
    async fn test_file_storage_delete_app() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());
        let bucket = current_bucket();
        let seg0 = segment_name(&bucket, "seg0");
        let seg1 = segment_name(&bucket, "seg1");
        let seg2 = segment_name(&bucket, "seg2");

        storage
            .write("app1", "stream1", &seg0, Bytes::from_static(b"d0"))
            .await
            .unwrap();
        storage
            .write("app1", "stream2", &seg1, Bytes::from_static(b"d1"))
            .await
            .unwrap();
        storage
            .write("app2", "stream1", &seg2, Bytes::from_static(b"d2"))
            .await
            .unwrap();

        let deleted = storage.delete_app("app1").await.unwrap();
        assert_eq!(deleted, 2);

        assert!(!storage.exists("app1", "stream1", &seg0).await.unwrap());
        assert!(!storage.exists("app1", "stream2", &seg1).await.unwrap());
        assert!(storage.exists("app2", "stream1", &seg2).await.unwrap());
    }

    #[tokio::test]
    async fn test_file_storage_path_traversal_rejected() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        assert!(storage
            .write("..", "stream", "name", Bytes::from_static(b"x"))
            .await
            .is_err());
        assert!(storage
            .write("app", "..", "name", Bytes::from_static(b"x"))
            .await
            .is_err());
        assert!(storage
            .write("app", "stream", "..", Bytes::from_static(b"x"))
            .await
            .is_err());
        assert!(storage
            .write("a/b", "stream", "name", Bytes::from_static(b"x"))
            .await
            .is_err());
        assert!(storage
            .write("", "stream", "name", Bytes::from_static(b"x"))
            .await
            .is_err());
    }
}
