use std::{collections::BTreeMap, error::Error as _, future::Future, time::Duration};

use backon::{ExponentialBuilder, Retryable};
use reqwest::header;
use sha2::{Digest, Sha256};

use super::{
    protocol::{
        amz_datetime, complete_multipart_upload_body, extract_xml_tag, sha256_hex_to_base64,
        S3CompletedPart, S3SigningContext,
    },
    url::s3_url,
    S3FileStorageConfig,
};
use crate::{models::CompleteFileUploadPart, Error, Result};

const S3_UPLOAD_PART_RETRY_MAX_TIMES: usize = 3;
const S3_UPLOAD_PART_RETRY_MIN_DELAY: Duration = Duration::from_millis(100);
const S3_UPLOAD_PART_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
enum UploadPartAttemptError {
    #[error("transport error: {0}")]
    Transport(#[source] reqwest::Error),
    #[error("S3 returned {status}: {body}")]
    Status {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("request signing failed: {0}")]
    Signing(String),
    #[error("S3 upload part response did not include ETag")]
    MissingEtag,
}

impl UploadPartAttemptError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(error) => is_retryable_transport_error(error),
            Self::Status { status, .. } => is_retryable_status(*status),
            Self::Signing(_) | Self::MissingEtag => false,
        }
    }

    fn into_service_error(self) -> Error {
        match self {
            Self::Transport(error) if error.is_timeout() => {
                Error::Timeout(format!("S3 multipart part upload timed out: {error}"))
            }
            Self::Transport(error) => {
                Error::ServiceUnavailable(format!("failed to upload S3 multipart part: {error}"))
            }
            Self::Status { status, body } if is_retryable_status(status) => {
                Error::ServiceUnavailable(format!(
                    "failed to upload S3 multipart part: {status} {body}"
                ))
            }
            Self::Status { status, body } => Error::InvalidInput(format!(
                "failed to upload S3 multipart part: {status} {body}"
            )),
            Self::Signing(message) => Error::Internal(format!(
                "failed to sign S3 multipart part upload: {message}"
            )),
            Self::MissingEtag => Error::Internal(
                "S3 multipart upload part response did not include ETag".to_string(),
            ),
        }
    }
}

pub(super) async fn create_s3_multipart_upload(
    config: &S3FileStorageConfig,
    http_client: &reqwest::Client,
    object_key: &str,
    mime_type: &str,
) -> Result<String> {
    let url = s3_url(config, object_key, &[("uploads", "")])?;
    let date = crate::SystemClock.now();
    let body_hash = hex::encode(Sha256::digest([]));
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), mime_type.to_string());
    headers.insert("x-amz-content-sha256".to_string(), body_hash.clone());
    headers.insert("x-amz-date".to_string(), amz_datetime(date));
    let auth =
        signing_context(config).authorization_header("POST", &url, &headers, date, &body_hash)?;
    let response = http_client
        .post(url)
        .header(header::CONTENT_TYPE, mime_type)
        .header("x-amz-content-sha256", body_hash)
        .header("x-amz-date", amz_datetime(date))
        .header(header::AUTHORIZATION, auth)
        .send()
        .await
        .map_err(|error| {
            Error::Internal(format!("failed to create S3 multipart upload: {error}"))
        })?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Internal(format!(
            "failed to create S3 multipart upload: {status} {text}"
        )));
    }
    let body = response.text().await.map_err(|error| {
        Error::Internal(format!("failed to read S3 multipart response: {error}"))
    })?;
    extract_xml_tag(&body, "UploadId").ok_or_else(|| {
        Error::Internal("S3 multipart upload response did not include UploadId".to_string())
    })
}

pub(super) async fn complete_s3_multipart_upload(
    config: &S3FileStorageConfig,
    http_client: &reqwest::Client,
    object_key: &str,
    upload_id: &str,
    parts: &[CompleteFileUploadPart],
) -> Result<()> {
    let mut sorted = parts.to_vec();
    sorted.sort_by_key(|part| part.part_number);
    let wire_parts = sorted
        .iter()
        .map(|part| {
            S3CompletedPart::new(
                part.part_number,
                &part.etag,
                part.checksum_sha256.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    let body = complete_multipart_upload_body(&wire_parts)?;
    let url = s3_url(config, object_key, &[("uploadId", upload_id)])?;
    let date = crate::SystemClock.now();
    let body_hash = hex::encode(Sha256::digest(body.as_bytes()));
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/xml".to_string());
    headers.insert("x-amz-content-sha256".to_string(), body_hash.clone());
    headers.insert("x-amz-date".to_string(), amz_datetime(date));
    let auth =
        signing_context(config).authorization_header("POST", &url, &headers, date, &body_hash)?;
    let response = http_client
        .post(url)
        .header(header::CONTENT_TYPE, "application/xml")
        .header("x-amz-content-sha256", body_hash)
        .header("x-amz-date", amz_datetime(date))
        .header(header::AUTHORIZATION, auth)
        .body(body)
        .send()
        .await
        .map_err(|error| {
            Error::Internal(format!("failed to complete S3 multipart upload: {error}"))
        })?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::InvalidInput(format!(
            "failed to complete S3 multipart upload: {status} {text}"
        )));
    }
    Ok(())
}

pub(super) async fn abort_s3_multipart_upload(
    config: &S3FileStorageConfig,
    http_client: &reqwest::Client,
    object_key: &str,
    upload_id: &str,
) -> Result<()> {
    let url = s3_url(config, object_key, &[("uploadId", upload_id)])?;
    let date = crate::SystemClock.now();
    let body_hash = hex::encode(Sha256::digest([]));
    let mut headers = BTreeMap::new();
    headers.insert("x-amz-content-sha256".to_string(), body_hash.clone());
    headers.insert("x-amz-date".to_string(), amz_datetime(date));
    let auth =
        signing_context(config).authorization_header("DELETE", &url, &headers, date, &body_hash)?;
    let response = http_client
        .delete(url)
        .header("x-amz-content-sha256", body_hash)
        .header("x-amz-date", amz_datetime(date))
        .header(header::AUTHORIZATION, auth)
        .send()
        .await
        .map_err(|error| {
            Error::Internal(format!("failed to abort S3 multipart upload: {error}"))
        })?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Internal(format!(
            "failed to abort S3 multipart upload: {status} {text}"
        )));
    }
    Ok(())
}

pub(super) async fn upload_s3_multipart_part(
    config: &S3FileStorageConfig,
    http_client: &reqwest::Client,
    object_key: &str,
    upload_id: &str,
    part_number: i32,
    checksum_sha256: &str,
    data: bytes::Bytes,
) -> Result<String> {
    let url = s3_url(
        config,
        object_key,
        &[
            ("partNumber", &part_number.to_string()),
            ("uploadId", upload_id),
        ],
    )?;
    let body_hash = hex::encode(Sha256::digest(&data));
    let checksum_base64 = sha256_hex_to_base64(checksum_sha256)?;

    retry_upload_part(|| {
        let url = url.clone();
        let body_hash = body_hash.clone();
        let checksum_base64 = checksum_base64.clone();
        let data = data.clone();
        async move {
            let date = crate::SystemClock.now();
            let mut headers = BTreeMap::new();
            headers.insert("x-amz-checksum-sha256".to_string(), checksum_base64.clone());
            headers.insert("x-amz-content-sha256".to_string(), body_hash.clone());
            headers.insert("x-amz-date".to_string(), amz_datetime(date));
            let auth = signing_context(config)
                .authorization_header("PUT", &url, &headers, date, &body_hash)
                .map_err(|error| UploadPartAttemptError::Signing(error.to_string()))?;
            let response = http_client
                .put(url)
                .header("x-amz-checksum-sha256", checksum_base64)
                .header("x-amz-content-sha256", body_hash)
                .header("x-amz-date", amz_datetime(date))
                .header(header::AUTHORIZATION, auth)
                .body(data)
                .send()
                .await
                .map_err(UploadPartAttemptError::Transport)?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(UploadPartAttemptError::Status { status, body });
            }
            response
                .headers()
                .get(header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .ok_or(UploadPartAttemptError::MissingEtag)
        }
    })
    .await
    .map_err(UploadPartAttemptError::into_service_error)
}

async fn retry_upload_part<F, Fut>(
    operation: F,
) -> std::result::Result<String, UploadPartAttemptError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<String, UploadPartAttemptError>>,
{
    operation
        .retry(upload_part_backoff())
        .when(UploadPartAttemptError::is_retryable)
        .notify(|error, delay| {
            tracing::warn!(%error, ?delay, "retrying S3 multipart part upload");
        })
        .await
}

fn upload_part_backoff() -> ExponentialBuilder {
    ExponentialBuilder::default()
        .with_min_delay(S3_UPLOAD_PART_RETRY_MIN_DELAY)
        .with_max_delay(S3_UPLOAD_PART_RETRY_MAX_DELAY)
        .with_max_times(S3_UPLOAD_PART_RETRY_MAX_TIMES)
        .with_jitter()
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::REQUEST_TIMEOUT
            | reqwest::StatusCode::TOO_MANY_REQUESTS
            | reqwest::StatusCode::INTERNAL_SERVER_ERROR
            | reqwest::StatusCode::BAD_GATEWAY
            | reqwest::StatusCode::SERVICE_UNAVAILABLE
            | reqwest::StatusCode::GATEWAY_TIMEOUT
    )
}

fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    if error.is_timeout() || error.is_connect() {
        return true;
    }
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(io_error) = cause.downcast_ref::<std::io::Error>() {
            if is_retryable_io_kind(io_error.kind()) {
                return true;
            }
        }
        source = cause.source();
    }
    false
}

fn is_retryable_io_kind(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::UnexpectedEof
    )
}

fn signing_context(config: &S3FileStorageConfig) -> S3SigningContext<'_> {
    S3SigningContext::new(
        &config.access_key_id,
        &config.secret_access_key,
        &config.region,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::*;

    #[test]
    fn retryable_statuses_are_explicitly_classified() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(is_retryable_status(
                reqwest::StatusCode::from_u16(status).expect("valid status")
            ));
        }
        for status in [400, 401, 403, 404, 409, 501, 505] {
            assert!(!is_retryable_status(
                reqwest::StatusCode::from_u16(status).expect("valid status")
            ));
        }
    }

    #[test]
    fn retryable_io_kinds_are_explicitly_classified() {
        for kind in [
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionRefused,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::Interrupted,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            assert!(is_retryable_io_kind(kind));
        }
        for kind in [
            std::io::ErrorKind::InvalidData,
            std::io::ErrorKind::InvalidInput,
            std::io::ErrorKind::PermissionDenied,
        ] {
            assert!(!is_retryable_io_kind(kind));
        }
    }

    #[tokio::test]
    async fn upload_part_retries_transient_status_then_succeeds() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let result = retry_upload_part({
            let attempts = Arc::clone(&attempts);
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt < 2 {
                        Err(UploadPartAttemptError::Status {
                            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                            body: String::new(),
                        })
                    } else {
                        Ok("etag".to_string())
                    }
                }
            }
        })
        .await;

        assert_eq!(result.expect("retry should succeed"), "etag");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn upload_part_does_not_retry_permanent_failure() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let result = retry_upload_part({
            let attempts = Arc::clone(&attempts);
            move || {
                attempts.fetch_add(1, Ordering::SeqCst);
                async {
                    Err(UploadPartAttemptError::Status {
                        status: reqwest::StatusCode::BAD_REQUEST,
                        body: String::new(),
                    })
                }
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
