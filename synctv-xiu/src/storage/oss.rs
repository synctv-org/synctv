// Object Storage Service (OSS) backend for HLS
//
// Supports:
// - AWS S3
// - Aliyun OSS
// - Minio
// - Any S3-compatible storage
//
// Uses OpenDAL for unified storage access

#[cfg(feature = "oss")]
mod inner {
    use crate::storage::HlsStorage;
    use async_trait::async_trait;
    use bytes::Bytes;
    use opendal::{Operator, services::S3};
    use std::io::{Result, Error, ErrorKind};
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

        /// Get full object key: {`base_path}{app}/{stream}/{name`}
        fn get_object_key(&self, app: &str, stream: &str, name: &str) -> String {
            if self.config.base_path.is_empty() {
                format!("{app}/{stream}/{name}")
            } else {
                format!("{}{app}/{stream}/{name}", self.config.base_path)
            }
        }

        /// Get prefix for listing objects under app/stream/
        fn get_stream_prefix(&self, app: &str, stream: &str) -> String {
            if self.config.base_path.is_empty() {
                format!("{app}/{stream}/")
            } else {
                format!("{}{app}/{stream}/", self.config.base_path)
            }
        }

        /// Get prefix for listing objects under app/
        fn get_app_prefix(&self, app: &str) -> String {
            if self.config.base_path.is_empty() {
                format!("{app}/")
            } else {
                format!("{}{app}/", self.config.base_path)
            }
        }

        /// Delete all objects matching a prefix using `OpenDAL` lister.
        async fn delete_by_prefix_internal(&self, prefix: &str) -> Result<usize> {
            let lister = self.operator
                .lister(prefix)
                .await
                .map_err(|e| Error::other(format!("OSS list failed: {e}")))?;

            use futures::TryStreamExt;
            let mut entries = lister;
            let mut deleted = 0;
            while let Some(entry) = entries.try_next().await
                .map_err(|e| Error::other(format!("OSS list iteration failed: {e}")))? {
                let path = entry.path();
                if self.operator.delete(path).await.is_ok() {
                    deleted += 1;
                }
            }
            Ok(deleted)
        }
    }

    #[async_trait]
    impl HlsStorage for OssStorage {
        async fn write(&self, app: &str, stream: &str, name: &str, data: Bytes) -> Result<()> {
            let object_key = self.get_object_key(app, stream, name);
            let size = data.len();

            self.operator
                .write(&object_key, data)
                .await
                .map_err(|e| Error::other(format!("OSS write failed: {e}")))?;

            tracing::trace!("Wrote to OSS: {} ({} bytes) for {}/{}/{}", object_key, size, app, stream, name);

            Ok(())
        }

        async fn read(&self, app: &str, stream: &str, name: &str) -> Result<Bytes> {
            let object_key = self.get_object_key(app, stream, name);

            let buffer = self.operator
                .read(&object_key)
                .await
                .map_err(|e| Error::new(ErrorKind::NotFound, format!("OSS read failed: {e}")))?;

            let data = Bytes::from(buffer.to_vec());

            tracing::trace!("Read from OSS: {} ({} bytes) for {}/{}/{}", object_key, data.len(), app, stream, name);

            Ok(data)
        }

        async fn delete(&self, app: &str, stream: &str, name: &str) -> Result<()> {
            let object_key = self.get_object_key(app, stream, name);

            self.operator
                .delete(&object_key)
                .await
                .map_err(|e| Error::other(format!("OSS delete failed: {e}")))?;

            tracing::trace!("Deleted from OSS: {} for {}/{}/{}", object_key, app, stream, name);

            Ok(())
        }

        async fn exists(&self, app: &str, stream: &str, name: &str) -> Result<bool> {
            let object_key = self.get_object_key(app, stream, name);

            match self.operator.exists(&object_key).await {
                Ok(exists) => Ok(exists),
                Err(e) => {
                    tracing::warn!("OSS exists check failed for {}: {}", object_key, e);
                    Ok(false)
                }
            }
        }

        async fn delete_app_stream(&self, app: &str, stream: &str) -> Result<usize> {
            let prefix = self.get_stream_prefix(app, stream);
            let deleted = self.delete_by_prefix_internal(&prefix).await?;
            tracing::debug!("delete_app_stream {}/{}: deleted {} objects", app, stream, deleted);
            Ok(deleted)
        }

        async fn delete_app(&self, app: &str) -> Result<usize> {
            let prefix = self.get_app_prefix(app);
            let deleted = self.delete_by_prefix_internal(&prefix).await?;
            tracing::debug!("delete_app {}: deleted {} objects", app, deleted);
            Ok(deleted)
        }

        async fn cleanup(&self, older_than: Duration) -> Result<usize> {
            // Compute cutoff time using opendal's Timestamp
            let cutoff_time = opendal::raw::Timestamp::now() - older_than;
            let mut deleted = 0;

            let base_path = if self.config.base_path.is_empty() {
                String::new()
            } else {
                self.config.base_path.clone()
            };

            // List objects and stat each one to get LastModified metadata.
            // NOTE: opendal 0.55 does not support inline metadata on list
            // (no `metakey` option), so a per-object stat() call is required.
            // When upgrading opendal to a version that supports
            // `lister_with().metakey(Metakey::LastModified)`, replace the
            // stat() calls to reduce API requests from O(2N) to O(N).
            let lister = self.operator
                .lister(&base_path)
                .await
                .map_err(|e| Error::other(format!("OSS list failed: {e}")))?;

            use futures::TryStreamExt;
            let mut entries = lister;
            while let Some(entry) = entries.try_next().await
                .map_err(|e| Error::other(format!("OSS list iteration failed: {e}")))? {

                let path = entry.path();
                let metadata = self.operator.stat(path).await
                    .map_err(|e| Error::other(format!("OSS stat failed for {path}: {e}")))?;

                if let Some(last_modified) = metadata.last_modified() {
                    if last_modified < cutoff_time
                        && self.operator.delete(path).await.is_ok() {
                            deleted += 1;
                            tracing::trace!("Deleted expired OSS object: {}", path);
                        }
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

        async fn get_public_url(&self, app: &str, stream: &str, name: &str) -> Result<Option<String>> {
            let object_key = self.get_object_key(app, stream, name);

            // If CDN is configured, return CDN URL
            if !self.config.public_url_prefix.is_empty() {
                let cdn_url = format!("{}{}", self.config.public_url_prefix, object_key);
                tracing::trace!("Generated CDN URL for {}/{}/{}: {}", app, stream, name, cdn_url);
                return Ok(Some(cdn_url));
            }

            // No CDN, generate presigned URL with expiration
            let expires_in = Duration::from_secs(self.config.presign_expires_in);

            let presigned_req = self.operator
                .presign_read(&object_key, expires_in)
                .await
                .map_err(|e| Error::other(format!("Failed to presign URL: {e}")))?;

            let url = presigned_req.uri().to_string();

            tracing::trace!(
                "Generated presigned URL for {}/{}/{}: expires in {}s",
                app, stream, name,
                self.config.presign_expires_in
            );

            Ok(Some(url))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

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
            let url = storage.get_public_url("live", "room_123", "segment_0").await.unwrap();
            assert!(url.is_some());
            let url_str = url.unwrap();
            assert!(url_str.starts_with("https://cdn.example.com/hls/"));
            // URL should contain the structured path
            assert!(url_str.contains("live/room_123/segment_0"));
        }

        #[tokio::test]
        async fn test_oss_storage_public_url_no_base_path() {
            let config = OssConfig {
                endpoint: "https://minio.example.com:9000".to_string(),
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                bucket: "hls".to_string(),
                region: Some("us-east-1".to_string()),
                base_path: "".to_string(),
                public_url_prefix: "https://minio.example.com:9000/hls/".to_string(),
                presign_expires_in: 3600,
            };

            let storage = OssStorage::new(config).unwrap();

            let url = storage.get_public_url("room_123", "media_456", "segment_0").await.unwrap();
            assert!(url.is_some());
            let url_str = url.unwrap();
            assert!(url_str.starts_with("https://minio.example.com:9000/hls/"));
            assert!(url_str.contains("room_123/media_456/segment_0"));
        }
    }
}

#[cfg(feature = "oss")]
pub use inner::*;

// When oss feature is disabled, provide stub types so downstream code compiles
#[cfg(not(feature = "oss"))]
mod stub {
    /// OSS storage configuration (requires `oss` feature)
    #[derive(Debug, Clone)]
    pub struct OssConfig {
        pub endpoint: String,
        pub access_key_id: String,
        pub secret_access_key: String,
        pub bucket: String,
        pub region: Option<String>,
        pub base_path: String,
        pub public_url_prefix: String,
        pub presign_expires_in: u64,
    }

    /// OSS storage backend (requires `oss` feature)
    pub struct OssStorage;

    impl OssStorage {
        pub fn new(_config: OssConfig) -> std::result::Result<Self, Box<dyn std::error::Error>> {
            Err("OSS storage requires the `oss` feature to be enabled".into())
        }
    }
}

#[cfg(not(feature = "oss"))]
pub use stub::*;
