// S3-compatible object storage backend for HLS
// Supports AWS S3, MinIO, RustFS, Cloudflare R2, and compatible services.
// Uses OpenDAL for unified storage access

#[cfg(feature = "s3")]
mod inner {
    use crate::storage::{
        minute_bucket_is_expired, path_leaf, segment_minute_bucket, validate_component,
        validate_storage_key, HlsStorage, HLS_SEGMENTS_ROOT,
    };
    use async_trait::async_trait;
    use bytes::Bytes;
    use futures::{StreamExt as _, TryStreamExt};
    use opendal::{
        layers::{ConcurrentLimitLayer, RetryLayer, TimeoutLayer},
        services::S3,
        EntryMode, ErrorKind as OpendalErrorKind, Operator,
    };
    use std::io::{Error, ErrorKind, Result};
    use std::sync::Arc;
    use std::time::Duration;

    /// S3 storage configuration
    #[derive(Debug, Clone)]
    pub struct S3Config {
        /// S3-compatible endpoint (e.g., "https://s3.amazonaws.com")
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

    /// S3 storage backend
    pub struct S3Storage {
        config: S3Config,
        operator: Arc<Operator>,
    }

    impl S3Storage {
        const BUCKET_LIST_CONCURRENCY: usize = 8;
        const MAX_CONCURRENT_OPERATIONS: usize = 64;

        fn map_error(operation: &str, error: &opendal::Error) -> Error {
            let kind = match error.kind() {
                OpendalErrorKind::NotFound => ErrorKind::NotFound,
                OpendalErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
                OpendalErrorKind::AlreadyExists => ErrorKind::AlreadyExists,
                OpendalErrorKind::RateLimited => ErrorKind::WouldBlock,
                _ => ErrorKind::Other,
            };
            Error::new(kind, format!("S3 {operation} failed: {error}"))
        }

        /// Create new S3 storage with configuration.
        ///
        /// Enable `tls-aws-lc` or `tls-ring` when the process has not already
        /// installed a rustls crypto provider.
        pub fn new(mut config: S3Config) -> opendal::Result<Self> {
            synctv_common::install_process_crypto_provider();
            if rustls::crypto::CryptoProvider::get_default().is_none() {
                return Err(opendal::Error::new(
                    OpendalErrorKind::ConfigInvalid,
                    "S3 requires either the tls-aws-lc or tls-ring feature",
                ));
            }
            config.base_path = config.base_path.trim_matches('/').to_string();
            if !config.base_path.is_empty() {
                config.base_path.push('/');
            }
            if !config.public_url_prefix.is_empty() && !config.public_url_prefix.ends_with('/') {
                config.public_url_prefix.push('/');
            }
            tracing::info!(
                "Initializing S3 storage: bucket={}, endpoint={}",
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
            opendal::HttpTransporter::install_default(
                opendal_http_transport_reqwest::ReqwestTransport::default(),
            );
            let operator = Operator::new(builder)?
                .layer(
                    TimeoutLayer::new()
                        .with_timeout(Duration::from_secs(30))
                        .with_io_timeout(Duration::from_secs(30)),
                )
                .layer(
                    RetryLayer::new()
                        .with_max_times(3)
                        .with_min_delay(Duration::from_millis(100))
                        .with_max_delay(Duration::from_secs(2))
                        .with_jitter(),
                )
                .layer(ConcurrentLimitLayer::new(Self::MAX_CONCURRENT_OPERATIONS));

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
        /// (`unix_minutes_random`). S3 stores those as
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
            let paths = self.list_object_paths(prefix).await?;
            let deleted = paths.len();
            if deleted > 0 {
                self.operator
                    .delete_iter(paths)
                    .await
                    .map_err(|e| Error::other(format!("S3 batch delete failed: {e}")))?;
            }
            Ok(deleted)
        }

        async fn list_object_paths(&self, prefix: &str) -> Result<Vec<String>> {
            let lister = self
                .operator
                .lister_with(prefix)
                .recursive(true)
                .await
                .map_err(|e| Error::other(format!("S3 list failed: {e}")))?;

            let mut entries = lister;
            let mut paths = Vec::new();
            while let Some(entry) = entries
                .try_next()
                .await
                .map_err(|e| Error::other(format!("S3 list iteration failed: {e}")))?
            {
                if entry.metadata().mode() == EntryMode::DIR {
                    continue;
                }
                paths.push(entry.path().to_string());
            }
            Ok(paths)
        }

        async fn list_objects_by_modified(
            &self,
            prefix: &str,
        ) -> Result<Vec<(Option<opendal::raw::Timestamp>, String)>> {
            let paths = self.list_object_paths(prefix).await?;
            futures::stream::iter(paths)
                .map(|path| async move {
                    let metadata = self
                        .operator
                        .stat(&path)
                        .await
                        .map_err(|error| Self::map_error("stat", &error))?;
                    Ok::<_, Error>((metadata.last_modified(), path))
                })
                .buffer_unordered(Self::BUCKET_LIST_CONCURRENCY)
                .try_collect()
                .await
        }

        async fn list_dirs(&self, prefix: &str) -> Result<Vec<String>> {
            let lister = self
                .operator
                .lister(prefix)
                .await
                .map_err(|e| Error::other(format!("S3 list failed for {prefix}: {e}")))?;

            let mut entries = lister;
            let mut dirs = Vec::new();
            while let Some(entry) = entries
                .try_next()
                .await
                .map_err(|e| Error::other(format!("S3 list iteration failed: {e}")))?
            {
                if entry.metadata().mode() == EntryMode::DIR {
                    dirs.push(entry.path().to_string());
                }
            }
            Ok(dirs)
        }
    }

    #[async_trait]
    impl HlsStorage for S3Storage {
        async fn write(&self, app: &str, stream: &str, name: &str, data: Bytes) -> Result<()> {
            validate_storage_key(app, stream, name)?;
            let object_key = self.get_object_key(app, stream, name)?;
            let size = data.len();

            self.operator
                .write(&object_key, data)
                .await
                .map_err(|error| Self::map_error("write", &error))?;

            tracing::trace!(
                "Wrote to S3: {} ({} bytes) for {}/{}/{}",
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

            let buffer = self
                .operator
                .read(&object_key)
                .await
                .map_err(|error| Self::map_error("read", &error))?;

            let data = buffer.to_bytes();

            tracing::trace!(
                "Read from S3: {} ({} bytes) for {}/{}/{}",
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
                .map_err(|error| Self::map_error("delete", &error))?;

            tracing::trace!(
                "Deleted from S3: {} for {}/{}/{}",
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

            self.operator
                .exists(&object_key)
                .await
                .map_err(|error| Self::map_error("exists", &error))
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
                .map_err(|e| Error::other(format!("S3 list failed: {e}")))?;

            let mut entries = lister;
            let mut streams = std::collections::HashSet::new();
            while let Some(entry) = entries
                .try_next()
                .await
                .map_err(|e| Error::other(format!("S3 list iteration failed: {e}")))?
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
            let prefixes = self
                .list_dirs(&self.segments_root_prefix())
                .await?
                .into_iter()
                .filter_map(|bucket_prefix| {
                    path_leaf(&bucket_prefix)
                        .map(|bucket_name| self.get_bucket_stream_prefix(bucket_name, app, stream))
                });
            futures::stream::iter(prefixes)
                .map(|prefix| async move { self.list_object_paths(&prefix).await })
                .buffer_unordered(Self::BUCKET_LIST_CONCURRENCY)
                .try_fold(0usize, |total, paths| async move {
                    Ok(total.saturating_add(paths.len()))
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

            let prefixes = self
                .list_dirs(&self.segments_root_prefix())
                .await?
                .into_iter()
                .filter_map(|bucket_prefix| {
                    path_leaf(&bucket_prefix)
                        .map(|bucket_name| self.get_bucket_stream_prefix(bucket_name, app, stream))
                });
            let mut objects = futures::stream::iter(prefixes)
                .map(|prefix| async move { self.list_objects_by_modified(&prefix).await })
                .buffer_unordered(Self::BUCKET_LIST_CONCURRENCY)
                .try_collect::<Vec<Vec<(Option<opendal::raw::Timestamp>, String)>>>()
                .await?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();

            if objects.len() <= max_count {
                return Ok(0);
            }

            objects.sort();
            let to_delete = objects.len() - max_count;
            let delete_paths = objects
                .into_iter()
                .take(to_delete)
                .map(|(_, path)| path)
                .collect::<Vec<_>>();
            self.operator
                .delete_iter(delete_paths)
                .await
                .map_err(|e| Error::other(format!("S3 batch delete failed: {e}")))?;
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
                "S3 cleanup completed: bucket={}, deleted {} objects older than {:?}",
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
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        fn current_segment(suffix: &str) -> String {
            let bucket = Utc::now().timestamp() / 60;
            format!("{bucket}_{suffix}")
        }

        #[cfg(not(any(feature = "tls-aws-lc", feature = "tls-ring")))]
        #[test]
        fn constructor_reports_missing_crypto_provider() {
            let result = S3Storage::new(S3Config {
                endpoint: "https://s3.amazonaws.com".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "bucket".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "hls".to_string(),
                public_url_prefix: String::new(),
                presign_expires_in: 60,
            });
            let error = match result {
                Ok(_) => panic!("constructor must require a rustls crypto provider"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), OpendalErrorKind::ConfigInvalid);
        }

        #[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
        #[tokio::test]
        async fn transient_s3_failures_are_retried_by_the_operator() {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let request_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let server_count = Arc::clone(&request_count);
            let server = tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    let mut byte = [0_u8; 1];
                    while !request.ends_with(b"\r\n\r\n") {
                        if stream.read(&mut byte).await.unwrap() == 0 {
                            break;
                        }
                        request.push(byte[0]);
                    }

                    let attempt =
                        server_count.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
                    if attempt < 3 {
                        stream
                            .write_all(
                                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await
                            .unwrap();
                    } else {
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nETag: \"test\"\r\nLast-Modified: Sat, 01 Aug 2026 00:00:00 GMT\r\nConnection: close\r\n\r\n",
                            )
                            .await
                            .unwrap();
                        break;
                    }
                }
            });

            let storage = S3Storage::new(S3Config {
                endpoint: format!("http://{address}"),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "bucket".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "hls".to_string(),
                public_url_prefix: String::new(),
                presign_expires_in: 60,
            })
            .unwrap();

            assert!(storage
                .exists("room", "media", &current_segment("retry"))
                .await
                .unwrap());
            server.await.unwrap();
            assert_eq!(request_count.load(std::sync::atomic::Ordering::Acquire), 3);
        }

        #[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
        #[tokio::test]
        async fn test_s3_storage_path_traversal_rejected() {
            let config = S3Config {
                endpoint: "s3.amazonaws.com".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "my-bucket".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "hls/".to_string(),
                public_url_prefix: "https://cdn.example.com/hls/".to_string(),
                presign_expires_in: 3600,
            };

            let storage = S3Storage::new(config).unwrap();

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

        #[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
        #[tokio::test]
        async fn test_s3_storage_delete_app_stream_path_traversal_rejected() {
            let config = S3Config {
                endpoint: "s3.amazonaws.com".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "my-bucket".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "hls/".to_string(),
                public_url_prefix: "https://cdn.example.com/hls/".to_string(),
                presign_expires_in: 3600,
            };

            let storage = S3Storage::new(config).unwrap();

            assert!(storage.delete_app_stream("..", "stream").await.is_err());
            assert!(storage.delete_app_stream("app", "..").await.is_err());
            assert!(storage.delete_app("..").await.is_err());
            assert!(storage.delete_app("a/b").await.is_err());
        }

        #[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
        #[tokio::test]
        async fn test_s3_storage_public_url_with_cdn() {
            let config = S3Config {
                endpoint: "s3.amazonaws.com".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "my-bucket".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "hls/".to_string(),
                public_url_prefix: "https://cdn.example.com/hls/".to_string(),
                presign_expires_in: 3600,
            };

            let storage = S3Storage::new(config).unwrap();

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

        #[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
        #[tokio::test]
        async fn test_s3_storage_public_url_no_base_path() {
            let config = S3Config {
                endpoint: "https://minio.example.com:9000".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "hls".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: String::new(),
                public_url_prefix: "https://minio.example.com:9000/hls/".to_string(),
                presign_expires_in: 3600,
            };

            let storage = S3Storage::new(config).unwrap();

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

        #[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
        #[test]
        fn test_minute_bucket_segment_maps_to_directory_key() {
            let config = S3Config {
                endpoint: "s3.amazonaws.com".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "my-bucket".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "hls/".to_string(),
                public_url_prefix: String::new(),
                presign_expires_in: 3600,
            };

            let storage = S3Storage::new(config).unwrap();
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

#[cfg(feature = "s3")]
pub use inner::*;
