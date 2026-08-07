use std::time::Duration;

use opendal::{services::S3, Operator};

use super::S3FileStorageConfig;
use crate::{Error, Result};

const S3_CONTROL_OPERATION_TIMEOUT: Duration = Duration::from_secs(60);
const S3_IO_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const S3_RETRY_MAX_TIMES: usize = 3;
const S3_RETRY_MIN_DELAY: Duration = Duration::from_millis(100);
const S3_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
const S3_MAX_CONCURRENT_OPERATIONS: usize = 64;
const S3_MAX_CONCURRENT_HTTP_REQUESTS: usize = 128;
const S3_MULTIPART_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const S3_MULTIPART_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn s3_operator_from_config(config: &S3FileStorageConfig) -> Result<Operator> {
    crate::install_process_crypto_provider();
    opendal::HttpTransporter::install_default(
        opendal_http_transport_reqwest::ReqwestTransport::default(),
    );
    let mut builder = S3::default()
        .endpoint(config.endpoint.trim())
        .access_key_id(config.access_key_id.trim())
        .secret_access_key(config.secret_access_key.trim())
        .bucket(config.bucket.trim())
        .disable_config_load()
        .disable_ec2_metadata();

    let region = config.region.trim();
    if !region.is_empty() {
        builder = builder.region(region);
    }

    Operator::new(builder)
        // Timeout must be inside retry so each retry attempt receives its own
        // deadline and stateful OpenDAL bodies can restore their state safely.
        .map(|operator| {
            operator
                .layer(
                    opendal::layers::TimeoutLayer::default()
                        .with_timeout(S3_CONTROL_OPERATION_TIMEOUT)
                        .with_io_timeout(S3_IO_OPERATION_TIMEOUT),
                )
                .layer(
                    opendal::layers::RetryLayer::default()
                        .with_jitter()
                        .with_min_delay(S3_RETRY_MIN_DELAY)
                        .with_max_delay(S3_RETRY_MAX_DELAY)
                        .with_max_times(S3_RETRY_MAX_TIMES),
                )
                .layer(
                    opendal::layers::ConcurrentLimitLayer::new(S3_MAX_CONCURRENT_OPERATIONS)
                        .with_http_concurrent_limit(S3_MAX_CONCURRENT_HTTP_REQUESTS),
                )
        })
        .map_err(|error| Error::Internal(format!("failed to initialize S3 file storage: {error}")))
}

pub(super) fn s3_http_client() -> Result<reqwest::Client> {
    crate::install_process_crypto_provider();
    reqwest::Client::builder()
        .connect_timeout(S3_MULTIPART_CONNECT_TIMEOUT)
        .timeout(S3_MULTIPART_REQUEST_TIMEOUT)
        .build()
        .map_err(|error| {
            Error::Internal(format!(
                "failed to initialize S3 multipart HTTP client: {error}"
            ))
        })
}
