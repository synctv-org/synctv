use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use super::{
    protocol::{sha256_hex_to_base64, S3SigningContext},
    url::s3_url,
    S3FileStorageConfig,
};
use crate::{
    models::{FileUploadManifestPart, FileUploadPartUrl},
    Error, Result,
};

const S3_MAX_PRESIGNED_PART_URLS: i32 = 1000;

pub(super) fn s3_upload_part_urls(
    config: &S3FileStorageConfig,
    object_key: &str,
    upload_id: &str,
    expires_at: DateTime<Utc>,
    parts: &[FileUploadManifestPart],
) -> Result<Vec<FileUploadPartUrl>> {
    if parts.len() > usize::try_from(S3_MAX_PRESIGNED_PART_URLS).unwrap_or(usize::MAX) {
        return Err(Error::InvalidInput(
            "too many S3 multipart upload parts".to_string(),
        ));
    }
    let mut urls = Vec::with_capacity(parts.len());
    for part in parts {
        let (upload_url, upload_headers) = presigned_upload_part_url(
            config,
            object_key,
            upload_id,
            part.part_number,
            expires_at,
            &part.checksum_sha256,
        )?;
        urls.push(FileUploadPartUrl {
            part_number: part.part_number,
            offset_bytes: part.offset_bytes,
            size_bytes: part.size_bytes,
            upload_url,
            upload_method: "PUT".to_string(),
            upload_headers,
            expires_at: Some(expires_at),
        });
    }
    Ok(urls)
}

fn presigned_upload_part_url(
    config: &S3FileStorageConfig,
    object_key: &str,
    upload_id: &str,
    part_number: i32,
    expires_at: DateTime<Utc>,
    checksum_sha256: &str,
) -> Result<(String, BTreeMap<String, String>)> {
    let url = s3_url(
        config,
        object_key,
        &[
            ("partNumber", &part_number.to_string()),
            ("uploadId", upload_id),
        ],
    )?;
    let now = crate::SystemClock.now();
    let expires = (expires_at - now).num_seconds().clamp(1, 604_800);
    let mut headers = BTreeMap::new();
    headers.insert(
        "x-amz-checksum-sha256".to_string(),
        sha256_hex_to_base64(checksum_sha256)?,
    );
    let url = signing_context(config).presign_url("PUT", url, now, expires, &headers)?;
    Ok((url, headers))
}

fn signing_context(config: &S3FileStorageConfig) -> S3SigningContext<'_> {
    S3SigningContext::new(
        &config.access_key_id,
        &config.secret_access_key,
        &config.region,
    )
}

#[cfg(test)]
pub(in crate::service::file_storage) fn presigned_upload_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<std::collections::BTreeMap<String, String>> {
    headers
        .into_iter()
        .filter(|(name, _)| !name.eq_ignore_ascii_case("host"))
        .map(|(name, value)| {
            let value = std::str::from_utf8(value).map_err(|error| {
                Error::Internal(format!(
                    "S3 presigned upload header {name} is not valid UTF-8: {error}",
                ))
            })?;
            Ok((name.to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}
