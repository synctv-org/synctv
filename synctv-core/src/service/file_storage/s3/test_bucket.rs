use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use reqwest::header;
use sha2::{Digest, Sha256};

use super::{
    protocol::{amz_datetime, S3SigningContext},
    setup::{s3_http_client, s3_operator_from_config},
    url::s3_url,
    S3CompatibleFileStorageService, S3FileStorageConfig,
};
use crate::{Error, Result};

impl S3CompatibleFileStorageService {
    pub async fn ensure_test_bucket(config: &S3FileStorageConfig) -> Result<()> {
        validate_s3_bucket_setup_config(config)?;
        let service = Self {
            config: config.clone(),
            operator: s3_operator_from_config(config)?,
            repository: None,
            http_client: s3_http_client(),
            #[cfg(test)]
            test_multipart_upload_id: None,
            #[cfg(test)]
            test_force_stat_error: false,
        };
        service.create_bucket().await
    }

    async fn create_bucket(&self) -> Result<()> {
        let url = s3_url(&self.config, "", &[])?;
        let date = crate::SystemClock.now();
        let body_hash = hex::encode(Sha256::digest([]));
        let mut headers = BTreeMap::new();
        headers.insert("x-amz-content-sha256".to_string(), body_hash.clone());
        headers.insert("x-amz-date".to_string(), amz_datetime(date));
        let auth = self.authorization_header("PUT", &url, &headers, date, &body_hash)?;
        let response = self
            .http_client
            .put(url)
            .header("x-amz-content-sha256", body_hash)
            .header("x-amz-date", amz_datetime(date))
            .header(header::AUTHORIZATION, auth)
            .send()
            .await
            .map_err(|error| Error::Internal(format!("failed to create S3 bucket: {error}")))?;
        match response.status() {
            reqwest::StatusCode::OK
            | reqwest::StatusCode::NO_CONTENT
            | reqwest::StatusCode::CONFLICT => Ok(()),
            status => {
                let text = response.text().await.unwrap_or_default();
                Err(Error::Internal(format!(
                    "failed to create S3 bucket: {status} {text}"
                )))
            }
        }
    }

    fn signing_context(&self) -> S3SigningContext<'_> {
        S3SigningContext::new(
            &self.config.access_key_id,
            &self.config.secret_access_key,
            &self.config.region,
        )
    }

    fn authorization_header(
        &self,
        method: &str,
        url: &::url::Url,
        headers: &BTreeMap<String, String>,
        date: DateTime<Utc>,
        payload_hash: &str,
    ) -> Result<String> {
        self.signing_context()
            .authorization_header(method, url, headers, date, payload_hash)
    }
}

fn validate_s3_bucket_setup_config(config: &S3FileStorageConfig) -> Result<()> {
    if config.endpoint.trim().is_empty()
        || config.access_key_id.trim().is_empty()
        || config.secret_access_key.trim().is_empty()
        || config.bucket.trim().is_empty()
        || config.region.trim().is_empty()
    {
        return Err(Error::InvalidInput(
            "S3 bucket setup requires endpoint, bucket, region, access_key_id, and secret_access_key"
                .to_string(),
        ));
    }
    url::Url::parse(config.endpoint.trim())
        .map_err(|error| Error::InvalidInput(format!("Invalid S3 endpoint: {error}")))?;
    Ok(())
}
