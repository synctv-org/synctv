use opendal::{services::S3, Operator};

use super::S3FileStorageConfig;
use crate::{Error, Result};

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
        .map_err(|error| Error::Internal(format!("failed to initialize S3 file storage: {error}")))
}

pub(super) fn s3_http_client() -> reqwest::Client {
    crate::install_process_crypto_provider();
    reqwest::Client::new()
}
