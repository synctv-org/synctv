// File system storage backend for HLS
//
// Default storage backend using local filesystem
// With structured directory-based paths: base_path/app/stream/name

use super::HlsStorage;
use crate::storage::validate_storage_key;
use async_trait::async_trait;
use bytes::Bytes;
use std::io::Result;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
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

    /// Get full file path from structured components: `base_path/app/stream/name`
    fn get_path(&self, app: &str, stream: &str, name: &str) -> PathBuf {
        self.base_path.join(app).join(stream).join(name)
    }
}

#[async_trait]
impl HlsStorage for FileStorage {
    async fn write(&self, app: &str, stream: &str, name: &str, data: Bytes) -> Result<()> {
        validate_storage_key(app, stream, name)?;
        let file_path = self.get_path(app, stream, name);
        let size = data.len();

        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&file_path, data).await?;

        tracing::trace!(
            "Wrote: {:?} ({} bytes) for {}/{}/{}",
            file_path, size, app, stream, name
        );

        Ok(())
    }

    async fn read(&self, app: &str, stream: &str, name: &str) -> Result<Bytes> {
        validate_storage_key(app, stream, name)?;
        let file_path = self.get_path(app, stream, name);
        let data = fs::read(&file_path).await?;

        tracing::trace!(
            "Read: {:?} ({} bytes) for {}/{}/{}",
            file_path, data.len(), app, stream, name
        );

        Ok(Bytes::from(data))
    }

    async fn delete(&self, app: &str, stream: &str, name: &str) -> Result<()> {
        validate_storage_key(app, stream, name)?;
        let file_path = self.get_path(app, stream, name);

        if fs::try_exists(&file_path).await.unwrap_or(false) {
            fs::remove_file(&file_path).await?;
            tracing::trace!("Deleted: {:?} for {}/{}/{}", file_path, app, stream, name);
        }

        Ok(())
    }

    async fn exists(&self, app: &str, stream: &str, name: &str) -> Result<bool> {
        validate_storage_key(app, stream, name)?;
        let file_path = self.get_path(app, stream, name);
        fs::try_exists(&file_path).await
    }

    async fn delete_app_stream(&self, app: &str, stream: &str) -> Result<usize> {
        crate::storage::validate_component(app, "app")?;
        crate::storage::validate_component(stream, "stream")?;
        let dir = self.base_path.join(app).join(stream);

        if !fs::try_exists(&dir).await.unwrap_or(false) {
            return Ok(0);
        }

        let mut deleted = 0;
        let mut entries = fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let ft = match entry.file_type().await {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_file()
                && fs::remove_file(entry.path()).await.is_ok() {
                    deleted += 1;
                }
        }

        // Remove empty stream dir, then try removing empty app dir
        let _ = fs::remove_dir(&dir).await;
        let _ = fs::remove_dir(self.base_path.join(app)).await;

        tracing::debug!(
            "delete_app_stream {}/{}: deleted {} files",
            app, stream, deleted
        );
        Ok(deleted)
    }

    async fn delete_app(&self, app: &str) -> Result<usize> {
        crate::storage::validate_component(app, "app")?;
        let app_dir = self.base_path.join(app);

        if !fs::try_exists(&app_dir).await.unwrap_or(false) {
            return Ok(0);
        }

        let mut deleted = 0;
        let mut stream_dirs = fs::read_dir(&app_dir).await?;
        while let Some(stream_entry) = stream_dirs.next_entry().await? {
            let ft = match stream_entry.file_type().await {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if !ft.is_dir() {
                continue;
            }
            let stream_dir = stream_entry.path();
            let mut entries = fs::read_dir(&stream_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let ft = match entry.file_type().await {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if ft.is_file()
                    && fs::remove_file(entry.path()).await.is_ok() {
                        deleted += 1;
                    }
            }
            let _ = fs::remove_dir(&stream_dir).await;
        }

        let _ = fs::remove_dir(&app_dir).await;

        tracing::debug!("delete_app {}: deleted {} files", app, deleted);
        Ok(deleted)
    }

    async fn cleanup(&self, older_than: Duration) -> Result<usize> {
        if !fs::try_exists(&self.base_path).await.unwrap_or(false) {
            tracing::debug!("Cleanup base path does not exist: {:?}", self.base_path);
            return Ok(0);
        }

        let cutoff_time = SystemTime::now() - older_than;
        let mut deleted = 0;

        // Walk base_path/app/stream/files recursively
        let mut app_dirs = fs::read_dir(&self.base_path).await?;
        while let Some(app_entry) = app_dirs.next_entry().await? {
            let ft = match app_entry.file_type().await {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if !ft.is_dir() {
                continue;
            }
            let app_dir = app_entry.path();
            let mut stream_dirs = fs::read_dir(&app_dir).await?;
            while let Some(stream_entry) = stream_dirs.next_entry().await? {
                let ft = match stream_entry.file_type().await {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if !ft.is_dir() {
                    continue;
                }
                let stream_dir = stream_entry.path();
                let mut entries = fs::read_dir(&stream_dir).await?;
                while let Some(entry) = entries.next_entry().await? {
                    let path = entry.path();
                    let ft = match entry.file_type().await {
                        Ok(ft) => ft,
                        Err(_) => continue,
                    };
                    if !ft.is_file() {
                        continue;
                    }
                    if let Ok(metadata) = fs::metadata(&path).await {
                        if let Ok(modified) = metadata.modified() {
                            if modified < cutoff_time
                                && fs::remove_file(&path).await.is_ok() {
                                    deleted += 1;
                                    tracing::trace!("Deleted expired file: {:?}", path);
                                }
                        }
                    }
                }
                // Remove empty stream dir
                let _ = fs::remove_dir(&stream_dir).await;
            }
            // Remove empty app dir
            let _ = fs::remove_dir(&app_dir).await;
        }

        tracing::info!(
            "Cleanup completed: scanned {:?}, deleted {} files older than {:?}",
            self.base_path,
            deleted,
            older_than
        );

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_file_storage_write_read() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        let data = Bytes::from_static(b"test segment data");
        let result = storage.write("live", "room_123", "segment_0", data.clone()).await;
        assert!(result.is_ok());

        let read_data = storage.read("live", "room_123", "segment_0").await.unwrap();
        assert_eq!(data, read_data);

        let exists = storage.exists("live", "room_123", "segment_0").await.unwrap();
        assert!(exists);

        let result = storage.delete("live", "room_123", "segment_0").await;
        assert!(result.is_ok());

        let exists = storage.exists("live", "room_123", "segment_0").await.unwrap();
        assert!(!exists);
    }

    #[tokio::test]
    async fn test_file_storage_public_url() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        let url = storage.get_public_url("live", "room_123", "segment_0").await.unwrap();
        assert_eq!(url, None);
    }

    #[tokio::test]
    async fn test_file_storage_cleanup() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        storage.write("live", "room_123", "segment_0", Bytes::from_static(b"data0")).await.unwrap();
        storage.write("live", "room_123", "segment_1", Bytes::from_static(b"data1")).await.unwrap();
        storage.write("live", "room_456", "segment_0", Bytes::from_static(b"data2")).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        storage.write("live", "room_123", "segment_2", Bytes::from_static(b"data3")).await.unwrap();

        let deleted = storage.cleanup(Duration::from_millis(50)).await.unwrap();

        assert_eq!(deleted, 3);
        assert!(!storage.exists("live", "room_123", "segment_0").await.unwrap());
        assert!(!storage.exists("live", "room_123", "segment_1").await.unwrap());
        assert!(storage.exists("live", "room_123", "segment_2").await.unwrap());
        assert!(!storage.exists("live", "room_456", "segment_0").await.unwrap());
    }

    #[tokio::test]
    async fn test_file_storage_delete_app_stream() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        storage.write("app1", "stream1", "seg0", Bytes::from_static(b"d0")).await.unwrap();
        storage.write("app1", "stream1", "seg1", Bytes::from_static(b"d1")).await.unwrap();
        storage.write("app1", "stream2", "seg0", Bytes::from_static(b"d2")).await.unwrap();

        let deleted = storage.delete_app_stream("app1", "stream1").await.unwrap();
        assert_eq!(deleted, 2);

        assert!(!storage.exists("app1", "stream1", "seg0").await.unwrap());
        assert!(!storage.exists("app1", "stream1", "seg1").await.unwrap());
        assert!(storage.exists("app1", "stream2", "seg0").await.unwrap());
    }

    #[tokio::test]
    async fn test_file_storage_delete_app() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        storage.write("app1", "stream1", "seg0", Bytes::from_static(b"d0")).await.unwrap();
        storage.write("app1", "stream2", "seg0", Bytes::from_static(b"d1")).await.unwrap();
        storage.write("app2", "stream1", "seg0", Bytes::from_static(b"d2")).await.unwrap();

        let deleted = storage.delete_app("app1").await.unwrap();
        assert_eq!(deleted, 2);

        assert!(!storage.exists("app1", "stream1", "seg0").await.unwrap());
        assert!(!storage.exists("app1", "stream2", "seg0").await.unwrap());
        assert!(storage.exists("app2", "stream1", "seg0").await.unwrap());
    }

    #[tokio::test]
    async fn test_file_storage_path_traversal_rejected() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());

        assert!(storage.write("..", "stream", "name", Bytes::from_static(b"x")).await.is_err());
        assert!(storage.write("app", "..", "name", Bytes::from_static(b"x")).await.is_err());
        assert!(storage.write("app", "stream", "..", Bytes::from_static(b"x")).await.is_err());
        assert!(storage.write("a/b", "stream", "name", Bytes::from_static(b"x")).await.is_err());
        assert!(storage.write("", "stream", "name", Bytes::from_static(b"x")).await.is_err());
    }
}
