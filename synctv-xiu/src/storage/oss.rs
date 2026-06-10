// Object Storage Service (OSS) backend for HLS
// Supports:
// - AWS S3
// - Aliyun OSS
// - Minio
// - Any S3-compatible storage
// Uses OpenDAL for unified storage access

#[cfg(feature = "oss")]
mod inner {
    use crate::storage::{
        minute_bucket_is_expired, path_leaf, segment_minute_bucket, validate_component,
        validate_storage_key, HlsStorage, HLS_SEGMENTS_ROOT,
    };
    use async_trait::async_trait;
    use bytes::Bytes;
    use futures::TryStreamExt;
    use opendal::{services::S3, EntryMode, Operator};
    use std::io::{Error, ErrorKind, Result};
    use std::sync::Arc;
    use std::time::Duration;

    /// OSS storage configuration
    #[derive(Debug, Clone)]
    pub struct OssConfig {
        /// OSS endpoint (e.g., "oss-cn-hangzhou.aliyuncs.com" or "s3.amazonaws.com")
        pub endpoint: String,
        /// Access key ID
        pub access_key_id: String,
        /// Secret access key
        pub secret_access_key: String,
        /// Bucket name
        pub bucket: String,
        /// Region (for S3)
        pub region: Option<String>,
        /// Base path prefix in bucket (e.g., "hls/")
        pub base_path: String,
        /// Public URL prefix for serving (e.g., "<https://cdn.example.com/hls>/")
        /// If empty, will generate presigned temporary URLs
        pub public_url_prefix: String,
        /// Presigned URL expiration time in seconds (default: 3600 = 1 hour)
        /// Only used when `public_url_prefix` is empty
        pub presign_expires_in: u64,
    }

    /// OSS storage backend
    pub struct OssStorage {
        config: OssConfig,
        operator: Arc<Operator>,
    }

    impl OssStorage {
        /// Create new OSS storage with configuration
        pub fn new(config: OssConfig) -> std::result::Result<Self, Box<dyn std::error::Error>> {
            tracing::info!(
                "Initializing OSS storage: bucket={}, endpoint={}",
                config.bucket,
                config.endpoint
            );

            // Configure S3 service
            let mut builder = S3::default()
                .endpoint(&config.endpoint)
                .access_key_id(&config.access_key_id)
                .secret_access_key(&config.secret_access_key)
                .bucket(&config.bucket);

            if let Some(region) = &config.region {
                builder = builder.region(region);
            }

            // Build operator
            let operator = Operator::new(builder)?.finish();

            Ok(Self {
                config,
                operator: Arc::new(operator),
            })
        }

        fn segments_root_prefix(&self) -> String {
            if self.config.base_path.is_empty() {
                format!("{HLS_SEGMENTS_ROOT}/")
            } else {
                format!("{}{HLS_SEGMENTS_ROOT}/", self.config.base_path)
            }
        }

        /// Get full object key.
        ///
        /// HLS segment names must start with an epoch-minute bucket
        /// (`unix_minutes_random`). OSS stores those as
        /// `{base_path}segments/{minute}/{app}/{stream}/{name}`.
        fn get_object_key(&self, app: &str, stream: &str, name: &str) -> Result<String> {
            let bucket = segment_minute_bucket(name).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "HLS segment name must start with an epoch-minute bucket",
                )
            })?;
            Ok(format!(
                "{}{bucket}/{app}/{stream}/{name}",
                self.segments_root_prefix()
            ))
        }

        fn get_bucket_app_prefix(&self, bucket: &str, app: &str) -> String {
            format!("{}{bucket}/{app}/", self.segments_root_prefix())
        }

        fn get_bucket_stream_prefix(&self, bucket: &str, app: &str, stream: &str) -> String {
            format!("{}{bucket}/{app}/{stream}/", self.segments_root_prefix())
        }

        /// Delete all objects matching a prefix using `OpenDAL` lister.
        async fn delete_by_prefix_internal(&self, prefix: &str) -> Result<usize> {
            let lister = self
                .operator
                .lister_with(prefix)
                .recursive(true)
                .await
                .map_err(|e| Error::other(format!("OSS list failed: {e}")))?;

            let mut entries = lister;
            let mut paths = Vec::new();
            while let Some(entry) = entries
                .try_next()
                .await
                .map_err(|e| Error::other(format!("OSS list iteration failed: {e}")))?
            {
                if entry.metadata().mode() == EntryMode::DIR {
                    continue;
                }
                paths.push(entry.path().to_string());
            }

            let deleted = paths.len();
            if deleted > 0 {
                self.operator
                    .delete_iter(paths)
                    .await
                    .map_err(|e| Error::other(format!("OSS batch delete failed: {e}")))?;
            }
            Ok(deleted)
        }

        async fn list_dirs(&self, prefix: &str) -> Result<Vec<String>> {
            let lister = self
                .operator
                .lister(prefix)
                .await
                .map_err(|e| Error::other(format!("OSS list failed for {prefix}: {e}")))?;

            let mut entries = lister;
            let mut dirs = Vec::new();
            while let Some(entry) = entries
                .try_next()
                .await
                .map_err(|e| Error::other(format!("OSS list iteration failed: {e}")))?
            {
                if entry.metadata().mode() == EntryMode::DIR {
                    dirs.push(entry.path().to_string());
                }
            }
            Ok(dirs)
        }
    }

    #[async_trait]
    impl HlsStorage for OssStorage {
        async fn write(&self, app: &str, stream: &str, name: &str, data: Bytes) -> Result<()> {
            validate_storage_key(app, stream, name)?;
            let object_key = self.get_object_key(app, stream, name)?;
            let size = data.len();

            self.operator
                .write(&object_key, data)
                .await
                .map_err(|e| Error::other(format!("OSS write failed: {e}")))?;

            tracing::trace!(
                "Wrote to OSS: {} ({} bytes) for {}/{}/{}",
                object_key,
                size,
                app,
                stream,
                name
            );

            Ok(())
        }

        async fn read(&self, app: &str, stream: &str, name: &str) -> Result<Bytes> {
            validate_storage_key(app, stream, name)?;
            let object_key = self.get_object_key(app, stream, name)?;

            let buffer =
                self.operator.read(&object_key).await.map_err(|e| {
                    Error::new(ErrorKind::NotFound, format!("OSS read failed: {e}"))
                })?;

            let data = Bytes::from(buffer.to_vec());

            tracing::trace!(
                "Read from OSS: {} ({} bytes) for {}/{}/{}",
                object_key,
                data.len(),
                app,
                stream,
                name
            );

            Ok(data)
        }

        async fn delete(&self, app: &str, stream: &str, name: &str) -> Result<()> {
            validate_storage_key(app, stream, name)?;
            let object_key = self.get_object_key(app, stream, name)?;

            self.operator
                .delete(&object_key)
                .await
                .map_err(|e| Error::other(format!("OSS delete failed: {e}")))?;

            tracing::trace!(
                "Deleted from OSS: {} for {}/{}/{}",
                object_key,
                app,
                stream,
                name
            );

            Ok(())
        }

        async fn exists(&self, app: &str, stream: &str, name: &str) -> Result<bool> {
            validate_storage_key(app, stream, name)?;
            let object_key = self.get_object_key(app, stream, name)?;

            match self.operator.exists(&object_key).await {
                Ok(exists) => Ok(exists),
                Err(e) => {
                    tracing::warn!("OSS exists check failed for {}: {}", object_key, e);
                    Ok(false)
                }
            }
        }

        async fn delete_app_stream(&self, app: &str, stream: &str) -> Result<usize> {
            validate_component(app, "app")?;
            validate_component(stream, "stream")?;
            let segments_root = self.segments_root_prefix();

            let mut deleted = 0;
            for bucket_prefix in self.list_dirs(&segments_root).await? {
                let Some(bucket_name) = path_leaf(&bucket_prefix) else {
                    continue;
                };
                deleted += self
                    .delete_by_prefix_internal(&self.get_bucket_stream_prefix(
                        bucket_name,
                        app,
                        stream,
                    ))
                    .await?;
            }

            tracing::debug!(
                "delete_app_stream {}/{}: deleted {} objects",
                app,
                stream,
                deleted
            );
            Ok(deleted)
        }

        async fn delete_app(&self, app: &str) -> Result<usize> {
            validate_component(app, "app")?;
            let segments_root = self.segments_root_prefix();

            let mut deleted = 0;
            for bucket_prefix in self.list_dirs(&segments_root).await? {
                let Some(bucket_name) = path_leaf(&bucket_prefix) else {
                    continue;
                };
                deleted += self
                    .delete_by_prefix_internal(&self.get_bucket_app_prefix(bucket_name, app))
                    .await?;
            }

            tracing::debug!("delete_app {}: deleted {} objects", app, deleted);
            Ok(deleted)
        }

        async fn list_streams(&self) -> Result<Vec<(String, String)>> {
            let segments_root = self.segments_root_prefix();
            let lister = self
                .operator
                .lister_with(&segments_root)
                .recursive(true)
                .await
                .map_err(|e| Error::other(format!("OSS list failed: {e}")))?;

            let mut entries = lister;
            let mut streams = std::collections::HashSet::new();
            while let Some(entry) = entries
                .try_next()
                .await
                .map_err(|e| Error::other(format!("OSS list iteration failed: {e}")))?
            {
                if entry.metadata().mode() == EntryMode::DIR {
                    continue;
                }
                let Some(relative) = entry.path().strip_prefix(&segments_root) else {
                    continue;
                };
                let mut parts = relative.splitn(4, '/');
                let (Some(_bucket), Some(app), Some(stream), Some(_name)) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                streams.insert((app.to_string(), stream.to_string()));
            }

            let mut streams = streams.into_iter().collect::<Vec<_>>();
            streams.sort();
            Ok(streams)
        }

        async fn count_stream_segments(&self, app: &str, stream: &str) -> Result<usize> {
            validate_component(app, "app")?;
            validate_component(stream, "stream")?;
            let mut total = 0;
            for bucket_prefix in self.list_dirs(&self.segments_root_prefix()).await? {
                let Some(bucket_name) = path_leaf(&bucket_prefix) else {
                    continue;
                };
                let prefix = self.get_bucket_stream_prefix(bucket_name, app, stream);
                let lister = self
                    .operator
                    .lister_with(&prefix)
                    .recursive(true)
                    .await
                    .map_err(|e| Error::other(format!("OSS list failed: {e}")))?;

                let mut entries = lister;
                while let Some(entry) = entries
                    .try_next()
                    .await
                    .map_err(|e| Error::other(format!("OSS list iteration failed: {e}")))?
                {
                    if entry.metadata().mode() != EntryMode::DIR {
                        total += 1;
                    }
                }
            }
            Ok(total)
        }

        async fn delete_oldest_stream_segments(
            &self,
            app: &str,
            stream: &str,
            max_count: usize,
        ) -> Result<usize> {
            validate_component(app, "app")?;
            validate_component(stream, "stream")?;

            let mut paths = Vec::new();
            for bucket_prefix in self.list_dirs(&self.segments_root_prefix()).await? {
                let Some(bucket_name) = path_leaf(&bucket_prefix) else {
                    continue;
                };
                let prefix = self.get_bucket_stream_prefix(bucket_name, app, stream);
                let lister = self
                    .operator
                    .lister_with(&prefix)
                    .recursive(true)
                    .await
                    .map_err(|e| Error::other(format!("OSS list failed: {e}")))?;

                let mut entries = lister;
                while let Some(entry) = entries
                    .try_next()
                    .await
                    .map_err(|e| Error::other(format!("OSS list iteration failed: {e}")))?
                {
                    if entry.metadata().mode() != EntryMode::DIR {
                        paths.push(entry.path().to_string());
                    }
                }
            }

            if paths.len() <= max_count {
                return Ok(0);
            }

            paths.sort();
            let to_delete = paths.len() - max_count;
            let delete_paths = paths.into_iter().take(to_delete).collect::<Vec<_>>();
            self.operator
                .delete_iter(delete_paths)
                .await
                .map_err(|e| Error::other(format!("OSS batch delete failed: {e}")))?;
            Ok(to_delete)
        }

        async fn cleanup(&self, older_than: Duration) -> Result<usize> {
            let segments_root = self.segments_root_prefix();

            let mut deleted = 0;
            for bucket_prefix in self.list_dirs(&segments_root).await? {
                let Some(bucket_name) = path_leaf(&bucket_prefix) else {
                    continue;
                };

                if minute_bucket_is_expired(bucket_name, older_than) {
                    deleted += self.delete_by_prefix_internal(&bucket_prefix).await?;
                }
            }

            tracing::info!(
                "OSS cleanup completed: bucket={}, deleted {} objects older than {:?}",
                self.config.bucket,
                deleted,
                older_than
            );

            Ok(deleted)
        }

        async fn get_public_url(
            &self,
            app: &str,
            stream: &str,
            name: &str,
        ) -> Result<Option<String>> {
            validate_storage_key(app, stream, name)?;
            let object_key = self.get_object_key(app, stream, name)?;

            // If CDN is configured, return CDN URL
            if !self.config.public_url_prefix.is_empty() {
                let cdn_url = format!("{}{}", self.config.public_url_prefix, object_key);
                tracing::trace!(
                    "Generated CDN URL for {}/{}/{}: {}",
                    app,
                    stream,
                    name,
                    cdn_url
                );
                return Ok(Some(cdn_url));
            }

            // No CDN, generate presigned URL with expiration
            let expires_in = Duration::from_secs(self.config.presign_expires_in);

            let presigned_req = self
                .operator
                .presign_read(&object_key, expires_in)
                .await
                .map_err(|e| Error::other(format!("Failed to presign URL: {e}")))?;

            let url = presigned_req.uri().to_string();

            tracing::trace!(
                "Generated presigned URL for {}/{}/{}: expires in {}s",
                app,
                stream,
                name,
                self.config.presign_expires_in
            );

            Ok(Some(url))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use chrono::Utc;

        fn current_segment(suffix: &str) -> String {
            let bucket = Utc::now().timestamp() / 60;
            format!("{bucket}_{suffix}")
        }

        #[tokio::test]
        async fn test_oss_storage_path_traversal_rejected() {
            let config = OssConfig {
                endpoint: "s3.amazonaws.com".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "my-bucket".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "hls/".to_string(),
                public_url_prefix: "https://cdn.example.com/hls/".to_string(),
                presign_expires_in: 3600,
            };

            let storage = OssStorage::new(config).unwrap();

            // Path traversal via ".." in app
            assert!(storage
                .write("..", "stream", "name", Bytes::from_static(b"x"))
                .await
                .is_err());
            assert!(storage.read("..", "stream", "name").await.is_err());
            assert!(storage.delete("..", "stream", "name").await.is_err());
            assert!(storage.exists("..", "stream", "name").await.is_err());
            assert!(storage
                .get_public_url("..", "stream", "name")
                .await
                .is_err());

            // Path traversal via ".." in stream
            assert!(storage
                .write("app", "..", "name", Bytes::from_static(b"x"))
                .await
                .is_err());
            assert!(storage.read("app", "..", "name").await.is_err());
            assert!(storage.delete("app", "..", "name").await.is_err());
            assert!(storage.exists("app", "..", "name").await.is_err());
            assert!(storage.get_public_url("app", "..", "name").await.is_err());

            // Path traversal via ".." in name
            assert!(storage
                .write("app", "stream", "..", Bytes::from_static(b"x"))
                .await
                .is_err());
            assert!(storage.read("app", "stream", "..").await.is_err());
            assert!(storage.delete("app", "stream", "..").await.is_err());
            assert!(storage.exists("app", "stream", "..").await.is_err());
            assert!(storage.get_public_url("app", "stream", "..").await.is_err());

            // Slash in component
            assert!(storage
                .write("a/b", "stream", "name", Bytes::from_static(b"x"))
                .await
                .is_err());

            // Empty component
            assert!(storage
                .write("", "stream", "name", Bytes::from_static(b"x"))
                .await
                .is_err());

            // Backslash in component
            assert!(storage
                .write("a\\b", "stream", "name", Bytes::from_static(b"x"))
                .await
                .is_err());
        }

        #[tokio::test]
        async fn test_oss_storage_delete_app_stream_path_traversal_rejected() {
            let config = OssConfig {
                endpoint: "s3.amazonaws.com".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "my-bucket".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "hls/".to_string(),
                public_url_prefix: "https://cdn.example.com/hls/".to_string(),
                presign_expires_in: 3600,
            };

            let storage = OssStorage::new(config).unwrap();

            assert!(storage.delete_app_stream("..", "stream").await.is_err());
            assert!(storage.delete_app_stream("app", "..").await.is_err());
            assert!(storage.delete_app("..").await.is_err());
            assert!(storage.delete_app("a/b").await.is_err());
        }

        #[tokio::test]
        async fn test_oss_storage_public_url_with_cdn() {
            let config = OssConfig {
                endpoint: "s3.amazonaws.com".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "my-bucket".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "hls/".to_string(),
                public_url_prefix: "https://cdn.example.com/hls/".to_string(),
                presign_expires_in: 3600,
            };

            let storage = OssStorage::new(config).unwrap();

            // With CDN configured, should return CDN URL with structured path
            let segment = current_segment("segment_0");
            let url = storage
                .get_public_url("live", "room_123", &segment)
                .await
                .unwrap();
            assert!(url.is_some());
            let url_str = url.unwrap();
            assert!(url_str.starts_with("https://cdn.example.com/hls/"));
            // URL should contain the structured path
            assert!(url_str.contains(&format!(
                "segments/{}/live/room_123/{segment}",
                segment_minute_bucket(&segment).unwrap()
            )));
        }

        #[tokio::test]
        async fn test_oss_storage_public_url_no_base_path() {
            let config = OssConfig {
                endpoint: "https://minio.example.com:9000".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "hls".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: String::new(),
                public_url_prefix: "https://minio.example.com:9000/hls/".to_string(),
                presign_expires_in: 3600,
            };

            let storage = OssStorage::new(config).unwrap();

            let segment = current_segment("segment_0");
            let url = storage
                .get_public_url("room_123", "media_456", &segment)
                .await
                .unwrap();
            assert!(url.is_some());
            let url_str = url.unwrap();
            assert!(url_str.starts_with("https://minio.example.com:9000/hls/"));
            assert!(url_str.contains(&format!(
                "segments/{}/room_123/media_456/{segment}",
                segment_minute_bucket(&segment).unwrap()
            )));
        }

        #[test]
        fn test_segment_minute_bucket_parsing() {
            assert_eq!(segment_minute_bucket("29676270_abcd1234"), Some("29676270"));
            assert_eq!(segment_minute_bucket("segment_0"), None);
            assert_eq!(segment_minute_bucket("_abcd1234"), None);
            assert_eq!(segment_minute_bucket("2967627x_abcd1234"), None);
        }

        #[test]
        fn test_minute_bucket_expiration_waits_for_whole_bucket() {
            let now = Utc::now().timestamp();
            let current_bucket = (now / 60).to_string();
            let old_bucket = ((now - chrono::Duration::minutes(5).num_seconds()) / 60).to_string();

            assert!(!minute_bucket_is_expired(
                &current_bucket,
                Duration::from_mins(3)
            ));
            assert!(minute_bucket_is_expired(
                &old_bucket,
                Duration::from_mins(3)
            ));
        }

        #[test]
        fn test_minute_bucket_segment_maps_to_directory_key() {
            let config = OssConfig {
                endpoint: "s3.amazonaws.com".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "my-bucket".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "hls/".to_string(),
                public_url_prefix: String::new(),
                presign_expires_in: 3600,
            };

            let storage = OssStorage::new(config).unwrap();
            assert_eq!(
                storage
                    .get_object_key("room", "media", "29676270_abcd")
                    .unwrap(),
                "hls/segments/29676270/room/media/29676270_abcd"
            );
            assert!(storage
                .get_object_key("room", "media", "unbucketed_segment")
                .is_err());
        }
    }
}

#[cfg(feature = "oss")]
pub use inner::*;
