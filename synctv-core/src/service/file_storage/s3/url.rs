use super::S3FileStorageConfig;
use crate::{Error, Result};

pub(super) fn s3_url(
    config: &S3FileStorageConfig,
    object_key: &str,
    query: &[(&str, &str)],
) -> Result<url::Url> {
    let mut base = url::Url::parse(config.endpoint.trim())
        .map_err(|error| Error::InvalidInput(format!("Invalid S3 endpoint: {error}")))?;
    {
        let mut segments = base
            .path_segments_mut()
            .map_err(|()| Error::InvalidInput("S3 endpoint must be hierarchical".to_string()))?;
        segments.push(config.bucket.trim());
        for segment in object_key.split('/').filter(|segment| !segment.is_empty()) {
            segments.push(segment);
        }
    }
    if !query.is_empty() {
        let mut pairs = base.query_pairs_mut();
        for (key, value) in query {
            if value.is_empty() {
                pairs.append_key_only(key);
            } else {
                pairs.append_pair(key, value);
            }
        }
    }
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestResultExt;

    fn config(endpoint: &str, bucket: &str) -> S3FileStorageConfig {
        S3FileStorageConfig {
            endpoint: endpoint.to_string(),
            access_key_id: "access".to_string(),
            secret_access_key: "secret".to_string(),
            bucket: bucket.to_string(),
            region: "us-east-1".to_string(),
            base_path: String::new(),
            public_base_url: None,
            upload_expires_seconds: 900,
            storage_backend: "s3".to_string(),
            upload_token_secret: "secret".to_string(),
        }
    }

    #[test]
    fn s3_url_builds_bucket_object_and_query() {
        let config = config("https://storage.example.test/root", "bucket");

        assert_eq!(
            s3_url(
                &config,
                "files/sha256/object.webp",
                &[("partNumber", "1"), ("uploads", "")]
            )
            .checked("url should build")
            .as_str(),
            "https://storage.example.test/root/bucket/files/sha256/object.webp?partNumber=1&uploads"
        );
    }

    #[test]
    fn s3_url_rejects_invalid_endpoint() {
        let error = s3_url(&config("://bad", "bucket"), "", &[]).failed("url should fail");

        assert!(matches!(
            error,
            Error::InvalidInput(message) if message.contains("Invalid S3 endpoint")
        ));
    }
}
