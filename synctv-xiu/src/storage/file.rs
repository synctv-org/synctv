// File system storage backend for HLS
// Default storage backend using local filesystem
// With structured directory-based paths: base_path/segments/minute/app/stream/name

use super::HlsStorage;
use crate::storage::{
    is_minute_bucket, minute_bucket_is_expired, segment_minute_bucket, validate_component,
    validate_storage_key, HLS_SEGMENTS_ROOT,
};
use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt, TryStreamExt};
use std::io::{Error, ErrorKind, Result};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::fs;
use tokio::io::AsyncWriteExt as _;
use uuid::Uuid;

const BUCKET_IO_CONCURRENCY: usize = 8;
const STAGING_DIR: &str = ".staging";

struct StagedFile {
    path: Option<PathBuf>,
}

impl StagedFile {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn committed(&mut self) {
        self.path = None;
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            // This also runs when an async write future is cancelled. The
            // staging file is local and unopened at this point in normal error
            // paths, so the small synchronous unlink keeps cancellation safe.
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Drain every result so Tokio's blocking file operations finish before an
/// error is returned to the caller.
async fn sum_io_results<S>(results: S) -> Result<usize>
where
    S: Stream<Item = Result<usize>>,
{
    results
        .fold(Ok(0usize), |aggregate, result| async move {
            match (aggregate, result) {
                (Ok(total), Ok(count)) => Ok(total.saturating_add(count)),
                (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            }
        })
        .await
}

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

    fn staging_root_path(&self) -> PathBuf {
        self.base_path.join(STAGING_DIR)
    }

    fn bucket_app_path(&self, bucket: &str, app: &str) -> PathBuf {
        self.segments_root_path().join(bucket).join(app)
    }

    fn bucket_stream_path(&self, bucket: &str, app: &str, stream: &str) -> PathBuf {
        self.bucket_app_path(bucket, app).join(stream)
    }

    async fn write_atomic(&self, file_path: &std::path::Path, data: &[u8]) -> Result<()> {
        let staging_root = self.staging_root_path();
        fs::create_dir_all(&staging_root).await?;
        let staging_path = staging_root.join(Uuid::new_v4().to_string());
        let mut staged_file = StagedFile::new(staging_path.clone());

        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staging_path)
            .await?;
        file.write_all(data).await?;
        file.flush().await?;
        drop(file);

        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::rename(&staging_path, file_path).await?;
        staged_file.committed();
        Ok(())
    }

    async fn remove_empty_dir_if_exists(dir: PathBuf) -> Result<()> {
        match fs::remove_dir(&dir).await {
            Ok(()) => Ok(()),
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty | ErrorKind::NotADirectory
                ) =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    async fn count_files_under(dir: &std::path::Path) -> Result<usize> {
        let mut count = 0;
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
                    count += 1;
                }
            }
        }

        Ok(count)
    }

    async fn remove_dir_all_counting_files(dir: &std::path::Path) -> Result<usize> {
        let deleted = match Self::count_files_under(dir).await {
            Ok(count) => count,
            Err(e) if e.kind() == ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };

        match fs::remove_dir_all(dir).await {
            Ok(()) => Ok(deleted),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e),
        }
    }

    /// Delete expired files within a minute bucket that has not fully crossed
    /// the retention cutoff yet. Fully expired buckets use the faster bulk
    /// removal path in `cleanup`.
    async fn remove_files_older_than(
        bucket_path: &std::path::Path,
        cutoff: SystemTime,
    ) -> Result<usize> {
        let mut deleted = 0;
        let mut stack = vec![bucket_path.to_path_buf()];
        let mut directories = Vec::new();

        while let Some(directory) = stack.pop() {
            directories.push(directory.clone());
            let mut entries = match fs::read_dir(&directory).await {
                Ok(entries) => entries,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            while let Some(entry) = entries.next_entry().await? {
                let file_type = match entry.file_type().await {
                    Ok(file_type) => file_type,
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                if file_type.is_dir() {
                    stack.push(entry.path());
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }

                let modified = match entry
                    .metadata()
                    .await
                    .and_then(|metadata| metadata.modified())
                {
                    Ok(modified) => modified,
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                if modified >= cutoff {
                    continue;
                }
                match fs::remove_file(entry.path()).await {
                    Ok(()) => deleted += 1,
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }

        directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        for directory in directories {
            Self::remove_empty_dir_if_exists(directory).await?;
        }
        Ok(deleted)
    }

    /// Yield validated `(bucket_name, bucket_path)` pairs under the segments
    /// root. Returns an empty vec when the root does not exist. Non-directory
    /// entries and names with invalid UTF-8 are skipped.
    async fn collect_bucket_dirs(&self) -> Result<Vec<(String, PathBuf)>> {
        let segments_root = self.segments_root_path();

        let mut buckets = Vec::new();
        let mut bucket_dirs = match fs::read_dir(&segments_root).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        while let Some(bucket_entry) = bucket_dirs.next_entry().await? {
            let Ok(file_type) = bucket_entry.file_type().await else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let Some(bucket_name) = bucket_entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if !is_minute_bucket(&bucket_name) {
                continue;
            }
            buckets.push((bucket_name, bucket_entry.path()));
        }

        Ok(buckets)
    }

    async fn collect_stream_dirs(&self) -> Result<Vec<(String, String, PathBuf)>> {
        let mut streams = Vec::new();
        for (_bucket_name, bucket_path) in self.collect_bucket_dirs().await? {
            let mut app_dirs = fs::read_dir(bucket_path).await?;
            while let Some(app_entry) = app_dirs.next_entry().await? {
                let Ok(app_file_type) = app_entry.file_type().await else {
                    continue;
                };
                if !app_file_type.is_dir() {
                    continue;
                }

                let Some(app_name) = app_entry.file_name().to_str().map(ToOwned::to_owned) else {
                    continue;
                };

                let mut stream_dirs = fs::read_dir(app_entry.path()).await?;
                while let Some(stream_entry) = stream_dirs.next_entry().await? {
                    let Ok(stream_file_type) = stream_entry.file_type().await else {
                        continue;
                    };
                    if !stream_file_type.is_dir() {
                        continue;
                    }

                    let Some(stream_name) =
                        stream_entry.file_name().to_str().map(ToOwned::to_owned)
                    else {
                        continue;
                    };
                    let stream_path = stream_entry.path();
                    if Self::count_files_under(&stream_path).await? == 0 {
                        continue;
                    }
                    streams.push((app_name.clone(), stream_name, stream_path));
                }
            }
        }

        Ok(streams)
    }

    async fn collect_app_stream_dirs(&self, app: &str) -> Result<Vec<(String, PathBuf)>> {
        let mut streams = Vec::new();
        for (bucket_name, _bucket_path) in self.collect_bucket_dirs().await? {
            let app_path = self.bucket_app_path(&bucket_name, app);
            let mut stream_dirs = match fs::read_dir(app_path).await {
                Ok(entries) => entries,
                Err(err) if err.kind() == ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };

            while let Some(stream_entry) = stream_dirs.next_entry().await? {
                let Ok(file_type) = stream_entry.file_type().await else {
                    continue;
                };
                if !file_type.is_dir() {
                    continue;
                }

                let Some(stream_name) = stream_entry.file_name().to_str().map(ToOwned::to_owned)
                else {
                    continue;
                };
                streams.push((stream_name, stream_entry.path()));
            }
        }

        Ok(streams)
    }
}

#[async_trait]
impl HlsStorage for FileStorage {
    async fn write(&self, app: &str, stream: &str, name: &str, data: Bytes) -> Result<()> {
        validate_storage_key(app, stream, name)?;
        let file_path = self.get_path(app, stream, name)?;
        let size = data.len();

        self.write_atomic(&file_path, &data).await?;

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

        match fs::remove_file(&file_path).await {
            Ok(()) => tracing::trace!("Deleted: {:?} for {}/{}/{}", file_path, app, stream, name),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err),
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
        let operations = futures::stream::iter(self.collect_bucket_dirs().await?)
            .map(|(bucket_name, bucket_path)| async move {
                let stream_dir = self.bucket_stream_path(&bucket_name, app, stream);
                let deleted = Self::remove_dir_all_counting_files(&stream_dir).await?;
                Self::remove_empty_dir_if_exists(self.bucket_app_path(&bucket_name, app)).await?;
                Self::remove_empty_dir_if_exists(bucket_path).await?;
                Ok::<usize, Error>(deleted)
            })
            .buffer_unordered(BUCKET_IO_CONCURRENCY);
        let deleted = sum_io_results(operations).await?;

        Self::remove_empty_dir_if_exists(self.segments_root_path()).await?;

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
        let operations = futures::stream::iter(self.collect_bucket_dirs().await?)
            .map(|(bucket_name, bucket_path)| async move {
                let deleted =
                    Self::remove_dir_all_counting_files(&self.bucket_app_path(&bucket_name, app))
                        .await?;
                Self::remove_empty_dir_if_exists(bucket_path).await?;
                Ok::<usize, Error>(deleted)
            })
            .buffer_unordered(BUCKET_IO_CONCURRENCY);
        let deleted = sum_io_results(operations).await?;

        Self::remove_empty_dir_if_exists(self.segments_root_path()).await?;

        tracing::debug!("delete_app {}: deleted {} files", app, deleted);
        Ok(deleted)
    }

    async fn list_streams(&self) -> Result<Vec<(String, String)>> {
        let mut streams = self
            .collect_stream_dirs()
            .await?
            .into_iter()
            .map(|(app, stream, _)| (app, stream))
            .collect::<Vec<_>>();
        streams.sort();
        streams.dedup();
        Ok(streams)
    }

    async fn count_stream_segments(&self, app: &str, stream: &str) -> Result<usize> {
        validate_component(app, "app")?;
        validate_component(stream, "stream")?;
        let stream_dirs = self
            .collect_app_stream_dirs(app)
            .await?
            .into_iter()
            .filter_map(|(stream_name, stream_dir)| (stream_name == stream).then_some(stream_dir));
        futures::stream::iter(stream_dirs)
            .map(|stream_dir| async move { Self::count_files_under(&stream_dir).await })
            .buffer_unordered(BUCKET_IO_CONCURRENCY)
            .try_fold(0usize, |total, count| async move {
                Ok(total.saturating_add(count))
            })
            .await
    }

    async fn delete_oldest_stream_segments(
        &self,
        app: &str,
        stream: &str,
        max_count: usize,
    ) -> Result<usize> {
        validate_component(app, "app")?;
        validate_component(stream, "stream")?;

        let mut segments = Vec::new();
        for (stream_name, stream_dir) in self.collect_app_stream_dirs(app).await? {
            if stream_name != stream {
                continue;
            }

            let mut entries = fs::read_dir(&stream_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let Ok(file_type) = entry.file_type().await else {
                    continue;
                };
                if !file_type.is_file() {
                    continue;
                }
                let modified = entry
                    .metadata()
                    .await
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                segments.push((modified, entry.path()));
            }
        }

        if segments.len() <= max_count {
            return Ok(0);
        }

        segments.sort();
        let to_delete = segments.len() - max_count;
        let operations = futures::stream::iter(segments.into_iter().take(to_delete))
            .map(|(_, path)| async move {
                match fs::remove_file(&path).await {
                    Ok(()) => Ok(1usize),
                    Err(err) if err.kind() == ErrorKind::NotFound => Ok(0),
                    Err(err) => Err(err),
                }
            })
            .buffer_unordered(BUCKET_IO_CONCURRENCY);
        sum_io_results(operations).await
    }

    async fn cleanup(&self, older_than: Duration) -> Result<usize> {
        let segments_root = self.segments_root_path();
        let cutoff = SystemTime::now().checked_sub(older_than).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "retention duration exceeds the system clock range",
            )
        })?;
        let operations = futures::stream::iter(self.collect_bucket_dirs().await?)
            .map(|(bucket_name, bucket_path)| async move {
                let deleted = if minute_bucket_is_expired(&bucket_name, older_than) {
                    let deleted = Self::remove_dir_all_counting_files(&bucket_path).await?;
                    tracing::trace!("Deleted expired minute bucket: {:?}", bucket_path);
                    deleted
                } else {
                    Self::remove_files_older_than(&bucket_path, cutoff).await?
                };
                Ok::<usize, Error>(deleted)
            })
            .buffer_unordered(BUCKET_IO_CONCURRENCY);
        let deleted = sum_io_results(operations).await?;

        Self::remove_empty_dir_if_exists(segments_root).await?;

        tracing::info!(
            "Cleanup completed: scanned {:?}, deleted {} files older than {:?}",
            self.segments_root_path(),
            deleted,
            older_than
        );

        Ok(deleted)
    }

    async fn get_public_url(&self, app: &str, stream: &str, name: &str) -> Result<Option<String>> {
        validate_storage_key(app, stream, name)?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use futures::FutureExt as _;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
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
    async fn test_sum_io_results_waits_for_all_operations_after_error() {
        let delayed_completed = Arc::new(AtomicBool::new(false));
        let delayed_completed_for_operation = delayed_completed.clone();
        let operations = futures::stream::iter(vec![
            async { Err(Error::other("first operation failed")) }.boxed(),
            async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                delayed_completed_for_operation.store(true, Ordering::Release);
                Ok(1)
            }
            .boxed(),
        ])
        .buffer_unordered(2);

        let error = sum_io_results(operations).await.unwrap_err();

        assert_eq!(error.to_string(), "first operation failed");
        assert!(delayed_completed.load(Ordering::Acquire));
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
    async fn file_write_failure_preserves_previously_published_object() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());
        let segment = segment_name(&current_bucket(), "atomic_failure");

        storage
            .write("live", "stream", &segment, Bytes::from_static(b"published"))
            .await
            .unwrap();

        let staging_root = storage.staging_root_path();
        fs::remove_dir(&staging_root).await.unwrap();
        fs::write(&staging_root, b"block staging directory creation")
            .await
            .unwrap();

        let error = storage
            .write(
                "live",
                "stream",
                &segment,
                Bytes::from_static(b"replacement"),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error.kind(),
            ErrorKind::AlreadyExists | ErrorKind::NotADirectory
        ));
        assert_eq!(
            storage.read("live", "stream", &segment).await.unwrap(),
            Bytes::from_static(b"published")
        );
    }

    #[tokio::test]
    async fn file_publish_failure_removes_staged_object() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());
        fs::write(
            storage.segments_root_path(),
            b"block final directory creation",
        )
        .await
        .unwrap();

        let segment = segment_name(&current_bucket(), "failed_publish");
        assert!(storage
            .write(
                "live",
                "stream",
                &segment,
                Bytes::from_static(b"complete staged data"),
            )
            .await
            .is_err());

        let mut staging_entries = fs::read_dir(storage.staging_root_path()).await.unwrap();
        assert!(staging_entries.next_entry().await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn file_write_publishes_only_complete_segment_data() {
        let temp_dir = tempdir().unwrap();
        let storage = Arc::new(FileStorage::new(temp_dir.path()));
        let segment = segment_name(&current_bucket(), "large_atomic_publish");
        let expected = Bytes::from(vec![0x5a; 8 * 1024 * 1024]);

        let writer_storage = storage.clone();
        let writer_segment = segment.clone();
        let writer_data = expected.clone();
        let writer = tokio::spawn(async move {
            writer_storage
                .write("live", "stream", &writer_segment, writer_data)
                .await
                .unwrap();
        });

        loop {
            match storage.read("live", "stream", &segment).await {
                Ok(data) => {
                    assert_eq!(data.len(), expected.len());
                    assert_eq!(data, expected);
                    break;
                }
                Err(error) if error.kind() == ErrorKind::NotFound => tokio::task::yield_now().await,
                Err(error) => panic!("segment read failed: {error}"),
            }
        }
        writer.await.unwrap();

        let mut staging_entries = fs::read_dir(storage.staging_root_path()).await.unwrap();
        assert!(staging_entries.next_entry().await.unwrap().is_none());
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
    async fn test_file_storage_lists_counts_and_trims_stream_segments() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());
        let bucket = current_bucket();
        let seg0 = segment_name(&bucket, "seg0");
        let seg1 = segment_name(&bucket, "seg1");
        let seg2 = segment_name(&bucket, "seg2");
        let other = segment_name(&bucket, "other");

        storage
            .write("app1", "stream1", &seg0, Bytes::from_static(b"d0"))
            .await
            .unwrap();
        storage
            .write("app1", "stream1", &seg1, Bytes::from_static(b"d1"))
            .await
            .unwrap();
        storage
            .write("app1", "stream1", &seg2, Bytes::from_static(b"d2"))
            .await
            .unwrap();
        storage
            .write("app1", "stream2", &other, Bytes::from_static(b"d3"))
            .await
            .unwrap();

        let mut streams = storage.list_streams().await.unwrap();
        streams.sort();
        assert_eq!(
            streams,
            vec![
                ("app1".to_string(), "stream1".to_string()),
                ("app1".to_string(), "stream2".to_string())
            ]
        );
        assert_eq!(
            storage
                .count_stream_segments("app1", "stream1")
                .await
                .unwrap(),
            3
        );

        let deleted = storage
            .delete_oldest_stream_segments("app1", "stream1", 1)
            .await
            .unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(
            storage
                .count_stream_segments("app1", "stream1")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage
                .count_stream_segments("app1", "stream2")
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn file_stream_listing_is_sorted_deduplicated_and_bucket_scoped() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());
        let current = current_bucket();
        let previous = (current.parse::<u64>().unwrap() - 1).to_string();

        for (bucket, app, stream, suffix) in [
            (&current, "app_b", "stream_z", "one"),
            (&current, "app_a", "stream_b", "two"),
            (&previous, "app_a", "stream_a", "three"),
            (&current, "app_a", "stream_a", "four"),
        ] {
            storage
                .write(
                    app,
                    stream,
                    &segment_name(bucket, suffix),
                    Bytes::from_static(b"segment"),
                )
                .await
                .unwrap();
        }

        let invalid_bucket_stream = temp_dir
            .path()
            .join(HLS_SEGMENTS_ROOT)
            .join("invalid-bucket")
            .join("rogue_app")
            .join("rogue_stream");
        fs::create_dir_all(&invalid_bucket_stream).await.unwrap();
        fs::write(invalid_bucket_stream.join("rogue.ts"), b"rogue")
            .await
            .unwrap();
        fs::create_dir_all(
            temp_dir
                .path()
                .join(HLS_SEGMENTS_ROOT)
                .join(&current)
                .join("empty_app")
                .join("empty_stream"),
        )
        .await
        .unwrap();

        assert_eq!(
            storage.list_streams().await.unwrap(),
            vec![
                ("app_a".to_string(), "stream_a".to_string()),
                ("app_a".to_string(), "stream_b".to_string()),
                ("app_b".to_string(), "stream_z".to_string()),
            ]
        );
        assert_eq!(
            storage
                .count_stream_segments("app_a", "stream_a")
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_file_writes_keep_streams_isolated() {
        const STREAMS: usize = 6;
        const SEGMENTS: usize = 8;

        let temp_dir = tempdir().unwrap();
        let storage = Arc::new(FileStorage::new(temp_dir.path()));
        let bucket = current_bucket();
        let writers = (0..STREAMS).map(|stream_index| {
            let storage = storage.clone();
            let bucket = bucket.clone();
            tokio::spawn(async move {
                let stream = format!("stream_{stream_index}");
                for segment_index in 0..SEGMENTS {
                    let name = segment_name(
                        &bucket,
                        &format!("stream_{stream_index}_segment_{segment_index}"),
                    );
                    storage
                        .write(
                            "live",
                            &stream,
                            &name,
                            Bytes::from(vec![u8::try_from(stream_index).unwrap(); 4096]),
                        )
                        .await
                        .unwrap();
                }
            })
        });
        futures::future::join_all(writers)
            .await
            .into_iter()
            .for_each(|result| result.unwrap());

        assert_eq!(storage.list_streams().await.unwrap().len(), STREAMS);
        for stream_index in 0..STREAMS {
            let stream = format!("stream_{stream_index}");
            assert_eq!(
                storage
                    .count_stream_segments("live", &stream)
                    .await
                    .unwrap(),
                SEGMENTS
            );
            for segment_index in 0..SEGMENTS {
                let name = segment_name(
                    &bucket,
                    &format!("stream_{stream_index}_segment_{segment_index}"),
                );
                assert_eq!(
                    storage.read("live", &stream, &name).await.unwrap(),
                    Bytes::from(vec![u8::try_from(stream_index).unwrap(); 4096])
                );
            }
        }
    }

    #[tokio::test]
    async fn file_count_bound_uses_write_time_for_random_segment_names() {
        let temp_dir = tempdir().unwrap();
        let storage = FileStorage::new(temp_dir.path());
        let bucket = current_bucket();
        let old_name = segment_name(&bucket, "z_old");
        let new_name = segment_name(&bucket, "a_new");

        storage
            .write("app", "stream", &old_name, Bytes::from_static(b"old"))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        storage
            .write("app", "stream", &new_name, Bytes::from_static(b"new"))
            .await
            .unwrap();

        assert_eq!(
            storage
                .delete_oldest_stream_segments("app", "stream", 1)
                .await
                .unwrap(),
            1
        );
        assert!(!storage.exists("app", "stream", &old_name).await.unwrap());
        assert!(storage.exists("app", "stream", &new_name).await.unwrap());
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
