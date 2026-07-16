use std::collections::BTreeMap;

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
    let date = crate::SystemClock.now();
    let body_hash = hex::encode(Sha256::digest(&data));
    let checksum_base64 = sha256_hex_to_base64(checksum_sha256)?;
    let mut headers = BTreeMap::new();
    headers.insert("x-amz-checksum-sha256".to_string(), checksum_base64.clone());
    headers.insert("x-amz-content-sha256".to_string(), body_hash.clone());
    headers.insert("x-amz-date".to_string(), amz_datetime(date));
    let auth =
        signing_context(config).authorization_header("PUT", &url, &headers, date, &body_hash)?;
    let response = http_client
        .put(url)
        .header("x-amz-checksum-sha256", checksum_base64)
        .header("x-amz-content-sha256", body_hash)
        .header("x-amz-date", amz_datetime(date))
        .header(header::AUTHORIZATION, auth)
        .body(data)
        .send()
        .await
        .map_err(|error| Error::Internal(format!("failed to upload S3 multipart part: {error}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::InvalidInput(format!(
            "failed to upload S3 multipart part: {status} {text}"
        )));
    }
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::Internal("S3 multipart upload part response did not include ETag".to_string())
        })?;
    Ok(etag.to_string())
}

fn signing_context(config: &S3FileStorageConfig) -> S3SigningContext<'_> {
    S3SigningContext::new(
        &config.access_key_id,
        &config.secret_access_key,
        &config.region,
    )
}
