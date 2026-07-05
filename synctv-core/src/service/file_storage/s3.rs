use std::{collections::BTreeMap, fmt::Write as _, sync::Arc};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use hmac::{Hmac, KeyInit, Mac};
use opendal::{services::S3, Operator};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use reqwest::header;
use sha2::{Digest, Sha256};

use crate::{
    models::{
        CompleteFileUploadPart, CompleteFileUploadSession, CompleteFileUploadSessionResult,
        CreateFileUploadSession, FileBlob, FileBlobCompression, FileObjectAccess,
        FileObjectDownload, FileObjectKind, FileObjectMetadata, FileOwnershipProofRange,
        FileReferenceTarget, FileUploadManifestPart, FileUploadOwnershipProofMetadata,
        FileUploadPartUrl, FileUploadSession, FileUploadSessionCreateResult, FileUploadSessionKind,
        GetFileObject, NewStoredFile, StoreFileUpload, StoreFileUploadResult,
    },
    repository::{
        FileStorageRepository, UpsertFileObject, UpsertFileUploadSession,
        UpsertFileUploadSessionPart,
    },
    service::file_storage::{
        attach_file_ownership_proof_token, attach_prepared_file_urls, collect_file_object_download,
        constant_time_eq, decode_file_object_key, encode_file_object_key, file_object_access,
        file_object_key, file_ownership_proof_digest, file_part_manifest_digest, file_reuse_grant,
        file_storage_object_base_path, mark_upload_session_ownership_proof_verified, new_file_id,
        optional_file_storage_public_url, payload_len_i64, register_upload_session_reference,
        strip_internal_file_metadata, upload_manifest_is_single_object,
        upload_manifest_parts_from_metadata, upload_media_type, upload_session_file_id,
        upload_session_is_multipart, upload_session_is_single_object, upload_session_metadata,
        upload_session_metadata_with_manifest, upload_session_object_metadata,
        upload_session_parts_progress, upload_session_policy, upload_session_progress,
        validate_create_file_upload_session, validate_file_mime_type,
        validate_file_object_read_token, validate_file_reuse_grant, validate_file_upload_token,
        validate_file_upload_token_context, validate_s3_file_storage_config,
        validate_session_file_for_storage, validate_stored_files, validate_upload_range,
        validated_upload_manifest, CreateFileReuseGrant, FileObjectReader, FileReuseGrant,
        FileStorageCleanupOrigin, FileStorageContext, FileStorageService,
        UploadSessionMetadataInput, ValidatedFileReuseGrant, FILE_UPLOAD_EXPIRES_SECONDS,
    },
    Error, Result,
};

const AWS4_ALGORITHM: &str = "AWS4-HMAC-SHA256";
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
const S3_MAX_PRESIGNED_PART_URLS: i32 = 1000;

const PATH_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

const QUERY_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

#[derive(Debug, Clone)]
pub struct S3FileStorageConfig {
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub region: String,
    pub base_path: String,
    pub public_base_url: Option<String>,
    pub upload_expires_seconds: i64,
    pub storage_backend: String,
    pub upload_token_secret: String,
}

pub struct S3CompatibleFileStorageService {
    pub(crate) config: S3FileStorageConfig,
    pub(crate) operator: Operator,
    pub(crate) repository: Option<Arc<FileStorageRepository>>,
    http_client: reqwest::Client,
    #[cfg(test)]
    test_multipart_upload_id: Option<String>,
    #[cfg(test)]
    test_force_stat_error: bool,
}

impl S3CompatibleFileStorageService {
    pub fn new(config: S3FileStorageConfig) -> Result<Self> {
        Self::new_with_repository(config, None)
    }

    pub fn new_with_repository(
        config: S3FileStorageConfig,
        repository: Option<Arc<FileStorageRepository>>,
    ) -> Result<Self> {
        validate_s3_file_storage_config(&config)?;
        let operator = s3_operator_from_config(&config)?;
        Ok(Self {
            config,
            operator,
            repository,
            http_client: s3_http_client(),
            #[cfg(test)]
            test_multipart_upload_id: None,
            #[cfg(test)]
            test_force_stat_error: false,
        })
    }

    #[cfg(feature = "test-support")]
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

    #[cfg(feature = "test-support")]
    async fn create_bucket(&self) -> Result<()> {
        let url = self.s3_url("", &[])?;
        let date = Utc::now();
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

    #[cfg(test)]
    pub(crate) fn with_operator(mut self, operator: Operator) -> Self {
        self.operator = operator;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_multipart_upload_id(mut self, upload_id: impl Into<String>) -> Self {
        self.test_multipart_upload_id = Some(upload_id.into());
        self
    }

    #[cfg(test)]
    pub(crate) const fn with_test_force_stat_error(mut self) -> Self {
        self.test_force_stat_error = true;
        self
    }

    fn repository(&self) -> Result<&Arc<FileStorageRepository>> {
        self.repository.as_ref().ok_or_else(|| {
            Error::Internal(
                "S3 multipart file uploads require a file storage repository".to_string(),
            )
        })
    }

    async fn read_object_range(
        &self,
        object_key: &str,
        range: &FileOwnershipProofRange,
    ) -> Result<Vec<u8>> {
        if range.offset < 0 || range.length <= 0 {
            return Err(Error::InvalidInput(
                "invalid file ownership proof range".to_string(),
            ));
        }
        let start = u64::try_from(range.offset)
            .map_err(|_| Error::InvalidInput("invalid file ownership proof range".to_string()))?;
        let length = u64::try_from(range.length)
            .map_err(|_| Error::InvalidInput("invalid file ownership proof range".to_string()))?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| Error::InvalidInput("invalid file ownership proof range".to_string()))?;
        let bytes = self
            .operator
            .read_with(object_key)
            .range(start..end)
            .await
            .map_err(|error| {
                Error::InvalidInput(format!("file object range is not readable: {error}"))
            })?;
        Ok(bytes.to_vec())
    }

    async fn delete_invalid_upload_object(&self, object_key: &str, reason: &'static str) {
        if let Err(error) = self.operator.delete(object_key).await {
            tracing::warn!(
                storage_backend = %self.config.storage_backend,
                object_key,
                reason,
                error = %error,
                "Failed to delete invalid uploaded file object"
            );
        }
    }

    async fn create_multipart_upload(&self, object_key: &str, mime_type: &str) -> Result<String> {
        #[cfg(test)]
        if let Some(upload_id) = &self.test_multipart_upload_id {
            let _ = (object_key, mime_type);
            return Ok(upload_id.clone());
        }

        let url = self.s3_url(object_key, &[("uploads", "")])?;
        let date = Utc::now();
        let body_hash = hex::encode(Sha256::digest([]));
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), mime_type.to_string());
        headers.insert("x-amz-content-sha256".to_string(), body_hash.clone());
        headers.insert("x-amz-date".to_string(), amz_datetime(date));
        let auth = self.authorization_header("POST", &url, &headers, date, &body_hash)?;
        let response = self
            .http_client
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

    async fn complete_s3_multipart_upload(
        &self,
        object_key: &str,
        upload_id: &str,
        parts: &[CompleteFileUploadPart],
    ) -> Result<()> {
        #[cfg(test)]
        if self.test_multipart_upload_id.is_some() {
            let _ = (object_key, upload_id);
            return Ok(());
        }

        let mut body = String::from("<CompleteMultipartUpload>");
        let mut sorted = parts.to_vec();
        sorted.sort_by_key(|part| part.part_number);
        for part in &sorted {
            if part.part_number <= 0 || part.etag.trim().is_empty() {
                return Err(Error::InvalidInput(
                    "S3 multipart completion requires positive part numbers and ETags".to_string(),
                ));
            }
            body.push_str("<Part><PartNumber>");
            body.push_str(&part.part_number.to_string());
            body.push_str("</PartNumber><ETag>");
            body.push_str(&escape_xml(part.etag.trim()));
            body.push_str("</ETag>");
            if let Some(checksum) = part.checksum_sha256.as_deref() {
                body.push_str("<ChecksumSHA256>");
                body.push_str(&escape_xml(&sha256_hex_to_base64(checksum)?));
                body.push_str("</ChecksumSHA256>");
            }
            body.push_str("</Part>");
        }
        body.push_str("</CompleteMultipartUpload>");
        let url = self.s3_url(object_key, &[("uploadId", upload_id)])?;
        let date = Utc::now();
        let body_hash = hex::encode(Sha256::digest(body.as_bytes()));
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/xml".to_string());
        headers.insert("x-amz-content-sha256".to_string(), body_hash.clone());
        headers.insert("x-amz-date".to_string(), amz_datetime(date));
        let auth = self.authorization_header("POST", &url, &headers, date, &body_hash)?;
        let response = self
            .http_client
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

    async fn abort_s3_multipart_upload(&self, object_key: &str, upload_id: &str) -> Result<()> {
        #[cfg(test)]
        if self.test_multipart_upload_id.is_some() {
            let _ = (object_key, upload_id);
            return Ok(());
        }

        let url = self.s3_url(object_key, &[("uploadId", upload_id)])?;
        let date = Utc::now();
        let body_hash = hex::encode(Sha256::digest([]));
        let mut headers = BTreeMap::new();
        headers.insert("x-amz-content-sha256".to_string(), body_hash.clone());
        headers.insert("x-amz-date".to_string(), amz_datetime(date));
        let auth = self.authorization_header("DELETE", &url, &headers, date, &body_hash)?;
        let response = self
            .http_client
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

    async fn upload_s3_multipart_part(
        &self,
        object_key: &str,
        upload_id: &str,
        part_number: i32,
        checksum_sha256: &str,
        #[cfg(test)] offset_bytes: i64,
        #[cfg(not(test))] _offset_bytes: i64,
        data: Vec<u8>,
    ) -> Result<String> {
        if part_number <= 0 {
            return Err(Error::InvalidInput(
                "S3 multipart upload requires a positive part number".to_string(),
            ));
        }

        #[cfg(test)]
        if self.test_multipart_upload_id.is_some() {
            let _ = upload_id;
            let offset = usize::try_from(offset_bytes).map_err(|_| {
                Error::InvalidInput("S3 multipart upload part offset is invalid".to_string())
            })?;
            let mut object = self
                .operator
                .read(object_key)
                .await
                .map(|bytes| bytes.to_vec())
                .unwrap_or_default();
            if object.len() < offset {
                object.resize(offset, 0);
            }
            let end = offset.checked_add(data.len()).ok_or_else(|| {
                Error::Internal("S3 multipart test object size overflow".to_string())
            })?;
            if object.len() < end {
                object.resize(end, 0);
            }
            object[offset..end].copy_from_slice(&data);
            self.operator
                .write_with(object_key, object)
                .await
                .map_err(|error| {
                    Error::Internal(format!("failed to write test S3 upload part: {error}"))
                })?;
            return Ok(format!("\"{}\"", hex::encode(Sha256::digest(&data))));
        }

        let url = self.s3_url(
            object_key,
            &[
                ("partNumber", &part_number.to_string()),
                ("uploadId", upload_id),
            ],
        )?;
        let date = Utc::now();
        let body_hash = hex::encode(Sha256::digest(&data));
        let checksum_base64 = sha256_hex_to_base64(checksum_sha256)?;
        let mut headers = BTreeMap::new();
        headers.insert("x-amz-checksum-sha256".to_string(), checksum_base64.clone());
        headers.insert("x-amz-content-sha256".to_string(), body_hash.clone());
        headers.insert("x-amz-date".to_string(), amz_datetime(date));
        let auth = self.authorization_header("PUT", &url, &headers, date, &body_hash)?;
        let response = self
            .http_client
            .put(url)
            .header("x-amz-checksum-sha256", checksum_base64)
            .header("x-amz-content-sha256", body_hash)
            .header("x-amz-date", amz_datetime(date))
            .header(header::AUTHORIZATION, auth)
            .body(data)
            .send()
            .await
            .map_err(|error| {
                Error::Internal(format!("failed to upload S3 multipart part: {error}"))
            })?;
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
                Error::Internal(
                    "S3 multipart upload part response did not include ETag".to_string(),
                )
            })?;
        Ok(etag.to_string())
    }

    async fn validate_completed_s3_object_size(
        &self,
        object_key: &str,
        expected_size: i64,
    ) -> Result<()> {
        let metadata = self.operator.stat(object_key).await.map_err(|error| {
            Error::InvalidInput(format!("file object is not readable: {error}"))
        })?;
        let size = i64::try_from(metadata.content_length())
            .map_err(|_| Error::Internal("file object size exceeds i64::MAX".to_string()))?;
        if size != expected_size {
            self.delete_invalid_upload_object(object_key, "size_mismatch")
                .await;
            return Err(Error::InvalidInput(
                "file payload size does not match upload session".to_string(),
            ));
        }
        Ok(())
    }

    async fn stat_object(&self, object_key: &str) -> Option<opendal::Metadata> {
        #[cfg(test)]
        if self.test_force_stat_error {
            return None;
        }
        match self.operator.stat(object_key).await {
            Ok(metadata) => Some(metadata),
            Err(error) => {
                tracing::debug!(
                    storage_backend = %self.config.storage_backend,
                    object_key,
                    error = %error,
                    "S3 object stat failed; falling back to object read path"
                );
                None
            }
        }
    }

    async fn object_download(&self, request: GetFileObject) -> Result<FileObjectDownload> {
        let object_key = decode_file_object_key(&request.encoded_object_key)?;
        validate_file_object_read_token(
            &self.config.storage_backend,
            &object_key,
            &request.read_token,
            &self.config.upload_token_secret,
        )?;
        let object = if let Some(repository) = self.repository.as_ref() {
            repository
                .get_object(&self.config.storage_backend, &object_key)
                .await?
        } else {
            None
        };
        let stat = if object.is_some() {
            None
        } else {
            self.stat_object(&object_key).await
        };
        let total_size_bytes = object.as_ref().map(|object| object.size_bytes).or_else(|| {
            stat.as_ref()
                .and_then(|stat| i64::try_from(stat.content_length()).ok())
        });
        let Some(total_size_bytes) = total_size_bytes else {
            if request.range.is_some() {
                return Err(Error::InvalidInput(
                    "file range requires known object size".to_string(),
                ));
            }
            let data = self
                .operator
                .read(&object_key)
                .await
                .map_err(|error| Error::NotFound(format!("File object not found: {error}")))?;
            let size_bytes = payload_len_i64(data.len())?;
            let mime_type = object
                .as_ref()
                .map(|object| object.mime_type.clone())
                .or_else(|| {
                    stat.as_ref()
                        .and_then(|stat| stat.content_type().map(ToString::to_string))
                })
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let content_manifest_sha256 = object
                .as_ref()
                .map(|object| object.content_manifest_sha256.clone())
                .unwrap_or_default();
            let metadata = object
                .as_ref()
                .map_or_else(Default::default, |object| object.metadata.clone());
            let created_at = object
                .as_ref()
                .map_or_else(Utc::now, |object| object.created_at);
            return Ok(FileObjectDownload {
                metadata: FileObjectMetadata {
                    storage_backend: self.config.storage_backend.clone(),
                    object_key,
                    mime_type,
                    size_bytes,
                    total_size_bytes: size_bytes,
                    content_manifest_sha256,
                    compression: FileBlobCompression::None,
                    range: None,
                    metadata,
                    created_at,
                },
                stream: futures::stream::once(async move { Ok(data.to_bytes()) }).boxed(),
            });
        };
        let range = super::resolve_file_range(request.range, total_size_bytes)?;
        let read_range = range.unwrap_or(crate::models::FileByteRange {
            start: 0,
            end_inclusive: total_size_bytes - 1,
        });
        let start = u64::try_from(read_range.start)
            .map_err(|_| Error::InvalidInput("file range is invalid".to_string()))?;
        let end = read_range
            .end_inclusive
            .checked_add(1)
            .and_then(|end| u64::try_from(end).ok())
            .ok_or_else(|| Error::InvalidInput("file range is invalid".to_string()))?;
        let mime_type = object
            .as_ref()
            .map(|object| object.mime_type.clone())
            .or_else(|| {
                stat.as_ref()
                    .and_then(|stat| stat.content_type().map(ToString::to_string))
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let content_manifest_sha256 = object
            .as_ref()
            .map(|object| object.content_manifest_sha256.clone())
            .unwrap_or_default();
        let metadata = object
            .as_ref()
            .map_or_else(Default::default, |object| object.metadata.clone());
        let created_at = object
            .as_ref()
            .map_or_else(Utc::now, |object| object.created_at);
        let reader = self
            .operator
            .reader(&object_key)
            .await
            .map_err(|error| Error::NotFound(format!("File object not found: {error}")))?;
        let bytes_stream = reader
            .into_bytes_stream(start..end)
            .await
            .map_err(|error| Error::NotFound(format!("File object not found: {error}")))?;
        let stream = bytes_stream
            .map(|chunk| {
                chunk.map_err(|error| {
                    Error::Internal(format!("failed to read S3 file object stream: {error}"))
                })
            })
            .boxed();
        Ok(FileObjectDownload {
            metadata: FileObjectMetadata {
                storage_backend: self.config.storage_backend.clone(),
                object_key,
                mime_type,
                size_bytes: read_range.size_bytes(),
                total_size_bytes,
                content_manifest_sha256,
                compression: FileBlobCompression::None,
                range,
                metadata,
                created_at,
            },
            stream,
        })
    }

    fn s3_url(&self, object_key: &str, query: &[(&str, &str)]) -> Result<url::Url> {
        let mut base = url::Url::parse(self.config.endpoint.trim())
            .map_err(|error| Error::InvalidInput(format!("Invalid S3 endpoint: {error}")))?;
        {
            let mut segments = base.path_segments_mut().map_err(|()| {
                Error::InvalidInput("S3 endpoint must be hierarchical".to_string())
            })?;
            segments.push(self.config.bucket.trim());
            for segment in object_key.split('/').filter(|segment| !segment.is_empty()) {
                segments.push(segment);
            }
        }
        if !query.is_empty() {
            {
                let mut pairs = base.query_pairs_mut();
                for (key, value) in query {
                    if value.is_empty() {
                        pairs.append_key_only(key);
                    } else {
                        pairs.append_pair(key, value);
                    }
                }
            }
        }
        Ok(base)
    }

    fn presigned_upload_part_url(
        &self,
        object_key: &str,
        upload_id: &str,
        part_number: i32,
        expires_at: DateTime<Utc>,
        checksum_sha256: &str,
    ) -> Result<(String, BTreeMap<String, String>)> {
        let url = self.s3_url(
            object_key,
            &[
                ("partNumber", &part_number.to_string()),
                ("uploadId", upload_id),
            ],
        )?;
        let now = Utc::now();
        let expires = (expires_at - now).num_seconds().clamp(1, 604_800);
        let mut headers = BTreeMap::new();
        headers.insert(
            "x-amz-checksum-sha256".to_string(),
            sha256_hex_to_base64(checksum_sha256)?,
        );
        let url = self.presign_url("PUT", url, now, expires, &headers)?;
        Ok((url, headers))
    }

    fn presign_url(
        &self,
        method: &str,
        mut url: url::Url,
        date: DateTime<Utc>,
        expires_seconds: i64,
        headers: &BTreeMap<String, String>,
    ) -> Result<String> {
        let host = host_header(&url)?;
        let credential_scope = credential_scope(date, &self.config.region);
        let credential = format!("{}/{}", self.config.access_key_id.trim(), credential_scope);
        let mut signed_header_pairs = headers
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        signed_header_pairs.push(("host", host.as_str()));
        signed_header_pairs.sort_by_key(|(key, _)| *key);
        let signed_headers_value = signed_headers(&signed_header_pairs);
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("X-Amz-Algorithm", AWS4_ALGORITHM);
            pairs.append_pair("X-Amz-Credential", &credential);
            pairs.append_pair("X-Amz-Date", &amz_datetime(date));
            pairs.append_pair("X-Amz-Expires", &expires_seconds.to_string());
            pairs.append_pair("X-Amz-SignedHeaders", &signed_headers_value);
        }
        let canonical_request = canonical_request(
            method,
            &url,
            &signed_header_pairs,
            &canonical_query(&url),
            UNSIGNED_PAYLOAD,
        )?;
        let string_to_sign = string_to_sign(date, &self.config.region, &canonical_request);
        let signature = sign_hex(
            self.config.secret_access_key.trim(),
            date,
            &self.config.region,
            &string_to_sign,
        )?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("X-Amz-Signature", &signature);
        }
        Ok(url.to_string())
    }

    fn authorization_header(
        &self,
        method: &str,
        url: &url::Url,
        headers: &BTreeMap<String, String>,
        date: DateTime<Utc>,
        payload_hash: &str,
    ) -> Result<String> {
        let mut all_headers = headers.clone();
        all_headers.insert("host".to_string(), host_header(url)?);
        let header_pairs = all_headers
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        let canonical_query = canonical_query(url);
        let canonical_request =
            canonical_request(method, url, &header_pairs, &canonical_query, payload_hash)?;
        let string_to_sign = string_to_sign(date, &self.config.region, &canonical_request);
        let signature = sign_hex(
            self.config.secret_access_key.trim(),
            date,
            &self.config.region,
            &string_to_sign,
        )?;
        let signed_headers = signed_headers(&header_pairs);
        Ok(format!(
            "{AWS4_ALGORITHM} Credential={}/{}, SignedHeaders={}, Signature={}",
            self.config.access_key_id.trim(),
            credential_scope(date, &self.config.region),
            signed_headers,
            signature
        ))
    }

    fn completion_parts_from_session_parts(
        parts: &[crate::models::FileUploadSessionPart],
    ) -> Result<Vec<CompleteFileUploadPart>> {
        let mut complete_parts = Vec::with_capacity(parts.len());
        for part in parts {
            let etag = part.etag.as_deref().map(str::trim).ok_or_else(|| {
                Error::InvalidInput(
                    "S3 multipart completion requires every recorded part ETag".to_string(),
                )
            })?;
            if etag.is_empty() {
                return Err(Error::InvalidInput(
                    "S3 multipart completion requires every recorded part ETag".to_string(),
                ));
            }
            complete_parts.push(CompleteFileUploadPart {
                part_number: part.part_number,
                etag: etag.to_string(),
                size_bytes: part.size_bytes,
                checksum_sha256: part.checksum_sha256.clone(),
            });
        }
        complete_parts.sort_by_key(|part| part.part_number);
        Ok(complete_parts)
    }

    fn completed_upload_part_manifest_digest(
        parts: &[crate::models::FileUploadSessionPart],
        size_bytes: i64,
        part_size_bytes: i64,
    ) -> Result<String> {
        let manifest_parts = parts
            .iter()
            .map(|part| {
                let checksum = part.checksum_sha256.as_deref().ok_or_else(|| {
                    Error::InvalidInput(
                        "S3 multipart completion requires every part checksum_sha256".to_string(),
                    )
                })?;
                Ok((part.part_number, part.size_bytes, checksum))
            })
            .collect::<Result<Vec<_>>>()?;
        file_part_manifest_digest(size_bytes, part_size_bytes, manifest_parts)
    }

    async fn finalize_multipart_upload_session(
        &self,
        session: &crate::models::FileUploadSessionRecord,
        session_parts: &[crate::models::FileUploadSessionPart],
        upload_id: &str,
    ) -> Result<CompleteFileUploadSessionResult> {
        let (uploaded_size_bytes, uploaded_parts) = upload_session_parts_progress(session_parts)?;
        if uploaded_size_bytes != session.size_bytes {
            return Ok(CompleteFileUploadSessionResult {
                object: None,
                uploaded_size_bytes,
                uploaded_parts,
            });
        }
        let completed_parts = Self::completion_parts_from_session_parts(session_parts)?;
        self.complete_s3_multipart_upload(&session.object_key, upload_id, &completed_parts)
            .await?;
        self.validate_completed_s3_object_size(&session.object_key, session.size_bytes)
            .await?;
        let content_manifest_sha256 = Self::completed_upload_part_manifest_digest(
            session_parts,
            session.size_bytes,
            session.part_size_bytes,
        )?;
        if !constant_time_eq(
            content_manifest_sha256.as_bytes(),
            session.content_manifest_sha256.as_bytes(),
        ) {
            self.delete_invalid_upload_object(&session.object_key, "manifest_mismatch")
                .await;
            return Err(Error::InvalidInput(
                "file manifest does not match uploaded parts".to_string(),
            ));
        }
        let repository = self.repository()?;
        let upload_policy = upload_session_policy(&session.metadata);
        let metadata = upload_session_object_metadata(&session.metadata);
        repository
            .upsert_pending_object(UpsertFileObject {
                storage_backend: &self.config.storage_backend,
                object_key: &session.object_key,
                mime_type: &session.mime_type,
                size_bytes: session.size_bytes,
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &metadata,
            })
            .await?;
        let mut blob = FileBlob {
            storage_backend: self.config.storage_backend.clone(),
            object_key: session.object_key.clone(),
            mime_type: session.mime_type.clone(),
            size_bytes: session.size_bytes,
            total_size_bytes: session.size_bytes,
            content_manifest_sha256: content_manifest_sha256.clone(),
            compression: FileBlobCompression::None,
            range: None,
            data: Vec::new(),
            metadata,
            created_at: Utc::now(),
        };
        if let Err(error) =
            super::complete_uploaded_file_object(self, repository, &mut blob, &upload_policy).await
        {
            self.delete_invalid_upload_object(&session.object_key, "media_validation_failed")
                .await;
            repository
                .delete_upload_session_parts(
                    &self.config.storage_backend,
                    &session.upload_session_key,
                )
                .await?;
            repository
                .delete_object(&self.config.storage_backend, &session.object_key)
                .await?;
            return Err(error);
        }
        repository
            .mark_object_validated(&self.config.storage_backend, &session.object_key)
            .await?;
        repository
            .complete_upload_session(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        Ok(CompleteFileUploadSessionResult {
            object: Some(blob),
            uploaded_size_bytes: session.size_bytes,
            uploaded_parts,
        })
    }

    async fn complete_single_upload_session(
        &self,
        session: &crate::models::FileUploadSessionRecord,
    ) -> Result<FileBlob> {
        self.validate_completed_s3_object_size(&session.object_key, session.size_bytes)
            .await?;
        let repository = self.repository()?;
        let upload_policy = upload_session_policy(&session.metadata);
        let metadata = upload_session_object_metadata(&session.metadata);
        let mut blob = super::session_record_blob(session, Vec::new(), metadata.clone());
        if let Err(error) =
            super::complete_uploaded_file_object(self, repository, &mut blob, &upload_policy).await
        {
            self.delete_invalid_upload_object(&session.object_key, "media_validation_failed")
                .await;
            repository
                .delete_object(&self.config.storage_backend, &session.object_key)
                .await?;
            return Err(error);
        }
        repository
            .mark_object_validated(&self.config.storage_backend, &session.object_key)
            .await?;
        repository
            .update_object_metadata(
                &self.config.storage_backend,
                &session.object_key,
                &blob.metadata,
            )
            .await?;
        repository
            .complete_upload_session(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        Ok(blob)
    }
}

#[async_trait::async_trait]
impl FileStorageService for S3CompatibleFileStorageService {
    fn backend_name(&self) -> &str {
        &self.config.storage_backend
    }

    fn supports_reuse_grants(&self) -> bool {
        true
    }

    fn repository(&self) -> Option<Arc<FileStorageRepository>> {
        self.repository.clone()
    }

    fn public_object_url(&self, storage_backend: &str, object_key: &str) -> Result<Option<String>> {
        if storage_backend != self.config.storage_backend {
            return Ok(None);
        }
        optional_file_storage_public_url(&self.config, object_key)
    }

    fn file_object_access(
        &self,
        storage_backend: &str,
        object_key: &str,
        object_kind: FileObjectKind,
    ) -> Result<Option<FileObjectAccess>> {
        if storage_backend != self.config.storage_backend {
            return Ok(None);
        }
        file_object_access(
            object_kind,
            &self.config.storage_backend,
            object_key,
            &self.config.upload_token_secret,
        )
        .map(Some)
    }

    async fn create_upload_session(
        &self,
        request: CreateFileUploadSession,
    ) -> Result<FileUploadSessionCreateResult> {
        validate_create_file_upload_session(&request)?;
        if request.parts.is_empty() {
            return Ok(FileUploadSessionCreateResult::Plan(
                super::create_file_upload_plan(request.size_bytes, request.policy.max_size_bytes)?,
            ));
        }
        let (plan, content_manifest_sha256) = validated_upload_manifest(
            request.size_bytes,
            request.policy.max_size_bytes,
            &request.parts,
        )?;
        let file_id = new_file_id();
        let session_metadata = upload_session_metadata(UploadSessionMetadataInput {
            file_id: &file_id,
            user_id: request.user_id,
            storage_scope: &request.storage_scope,
            client_file_id: request.client_file_id.as_deref(),
            filename: request.filename.as_deref(),
            width: request.width,
            height: request.height,
            metadata: request.metadata.clone(),
            upload_policy: &request.policy,
        });
        let now = Utc::now();
        let expires = self
            .config
            .upload_expires_seconds
            .clamp(60, FILE_UPLOAD_EXPIRES_SECONDS);
        let expires_at = now + chrono::Duration::seconds(expires);
        let repository = self.repository()?;
        if let Some(existing) = repository
            .get_object_by_manifest(
                &self.config.storage_backend,
                &content_manifest_sha256,
                request.size_bytes,
            )
            .await?
        {
            if self.operator.stat(&existing.object_key).await.is_ok() {
                validate_file_mime_type(&request.policy, &existing.mime_type)?;
                let mut file = NewStoredFile {
                    id: file_id,
                    filename: request.filename.clone(),
                    storage_backend: self.config.storage_backend.clone(),
                    object_key: existing.object_key.clone(),
                    object_access: None,
                    url: None,
                    mime_type: Some(existing.mime_type),
                    size_bytes: Some(existing.size_bytes),
                    width: request.width,
                    height: request.height,
                    metadata: request.metadata,
                };
                let (nonce, ranges) = attach_file_ownership_proof_token(
                    &mut file,
                    request.user_id,
                    &request.storage_scope,
                    expires_at,
                    &self.config.upload_token_secret,
                    &content_manifest_sha256,
                    request.size_bytes,
                )?;
                validate_session_file_for_storage(&file)?;
                let mut reference_metadata = session_metadata.clone();
                reference_metadata.ownership_proof = Some(FileUploadOwnershipProofMetadata {
                    nonce: nonce.clone(),
                    ranges: ranges.clone(),
                });
                register_upload_session_reference(
                    repository,
                    &self.config.storage_backend,
                    &file.object_key,
                    &file.id,
                    expires_at,
                    &reference_metadata,
                )
                .await?;
                return Ok(FileUploadSessionCreateResult::Session(FileUploadSession {
                    file,
                    encoded_object_key: encode_file_object_key(&existing.object_key),
                    upload_required: false,
                    ownership_proof_required: true,
                    ownership_proof_nonce: Some(nonce),
                    ownership_proof_ranges: ranges,
                    upload_object_access: None,
                    upload_url: None,
                    upload_method: None,
                    upload_headers: Default::default(),
                    expires_at: Some(expires_at),
                    max_size_bytes: request.policy.max_size_bytes,
                    resumable: true,
                    part_size_bytes: plan.part_size_bytes,
                    uploaded_size_bytes: 0,
                    uploaded_parts: Vec::new(),
                    upload_id: None,
                    part_urls: Vec::new(),
                }));
            }
        }
        let object_base_path = file_storage_object_base_path(
            &self.config.base_path,
            &request.policy.storage_namespace,
        );
        let existing_session = repository
            .get_pending_upload_session_by_manifest(
                &self.config.storage_backend,
                request.user_id,
                &request.storage_scope,
                &content_manifest_sha256,
                request.size_bytes,
            )
            .await?;
        let object_key = file_object_key(
            &object_base_path,
            "manifest",
            &content_manifest_sha256,
            &request.mime_type,
        );
        let upload_session_key = if let Some(existing) = existing_session.as_ref() {
            existing.upload_session_key.clone()
        } else {
            file_object_key(&object_base_path, "sessions", &file_id, &request.mime_type)
        };
        let single_object_upload =
            upload_manifest_is_single_object(request.size_bytes, &request.parts);
        let session_kind = if single_object_upload {
            FileUploadSessionKind::S3Single
        } else {
            FileUploadSessionKind::S3Multipart
        };
        let upload_id = if single_object_upload {
            None
        } else if let Some(session) = existing_session
            .as_ref()
            .filter(|session| session.completed_at.is_none() && session.expires_at > now)
            .and_then(|session| session.upload_id.clone())
        {
            Some(session)
        } else {
            Some(
                self.create_multipart_upload(&object_key, &request.mime_type)
                    .await?,
            )
        };
        let file_id = if let Some(existing) = existing_session.as_ref() {
            upload_session_file_id(&existing.metadata)?
        } else {
            file_id
        };
        let session_metadata = upload_session_metadata(UploadSessionMetadataInput {
            file_id: &file_id,
            user_id: request.user_id,
            storage_scope: &request.storage_scope,
            client_file_id: request.client_file_id.as_deref(),
            filename: request.filename.as_deref(),
            width: request.width,
            height: request.height,
            metadata: request.metadata.clone(),
            upload_policy: &request.policy,
        });
        let session_metadata =
            upload_session_metadata_with_manifest(&session_metadata, &request.parts);
        repository
            .upsert_pending_object(UpsertFileObject {
                storage_backend: &self.config.storage_backend,
                object_key: &object_key,
                mime_type: &request.mime_type,
                size_bytes: request.size_bytes,
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &request.metadata,
            })
            .await?;
        repository
            .upsert_upload_session(UpsertFileUploadSession {
                storage_backend: &self.config.storage_backend,
                upload_session_key: &upload_session_key,
                object_key: &object_key,
                session_kind,
                upload_id: upload_id.as_deref(),
                user_id: request.user_id,
                storage_scope: &request.storage_scope,
                mime_type: &request.mime_type,
                size_bytes: request.size_bytes,
                content_manifest_sha256: &content_manifest_sha256,
                part_size_bytes: plan.part_size_bytes,
                metadata: &session_metadata,
                expires_at,
            })
            .await?;
        let public_url = optional_file_storage_public_url(&self.config, &object_key)?;
        let mut file = NewStoredFile {
            id: file_id,
            filename: request.filename,
            storage_backend: self.config.storage_backend.clone(),
            object_key,
            object_access: None,
            url: public_url,
            mime_type: Some(request.mime_type),
            size_bytes: Some(request.size_bytes),
            width: request.width,
            height: request.height,
            metadata: request.metadata,
        };
        let upload_token = super::file_upload_token_for_object_key(
            &file,
            &upload_session_key,
            request.user_id,
            &request.storage_scope,
            expires_at,
            &self.config.upload_token_secret,
            Some(&content_manifest_sha256),
        )?;
        file.metadata.upload_token = Some(upload_token);
        validate_session_file_for_storage(&file)?;
        register_upload_session_reference(
            repository,
            &self.config.storage_backend,
            &file.object_key,
            &file.id,
            expires_at,
            &session_metadata,
        )
        .await?;
        let (uploaded_size_bytes, uploaded_parts) = upload_session_progress(
            repository,
            &self.config.storage_backend,
            &upload_session_key,
        )
        .await?;
        let part_urls = s3_upload_part_urls(
            self,
            &file.object_key,
            upload_id.as_deref().unwrap_or_default(),
            expires_at,
            if single_object_upload {
                &[]
            } else {
                &request.parts
            },
        )?;
        let mut upload_headers = BTreeMap::new();
        upload_headers.insert(
            "content-type".to_string(),
            file.mime_type
                .as_deref()
                .ok_or_else(|| Error::InvalidInput("file mime_type is required".to_string()))?
                .to_string(),
        );
        if let Some(token) = file.metadata.upload_token.as_deref() {
            upload_headers.insert(
                super::FILE_UPLOAD_TOKEN_HEADER.to_string(),
                token.to_string(),
            );
        }
        let upload_object_access = file_object_access(
            request.policy.object_kind,
            &self.config.storage_backend,
            &upload_session_key,
            &self.config.upload_token_secret,
        )?;
        Ok(FileUploadSessionCreateResult::Session(FileUploadSession {
            encoded_object_key: encode_file_object_key(&upload_session_key),
            file,
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            upload_object_access: Some(upload_object_access),
            upload_url: None,
            upload_method: Some("PUT".to_string()),
            upload_headers,
            expires_at: Some(expires_at),
            max_size_bytes: request.policy.max_size_bytes,
            resumable: !single_object_upload,
            part_size_bytes: plan.part_size_bytes,
            uploaded_size_bytes,
            uploaded_parts,
            upload_id,
            part_urls,
        }))
    }

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        mut files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        for file in &files {
            if file.storage_backend != self.config.storage_backend {
                return Err(Error::InvalidInput(format!(
                    "file storage_backend must be {}",
                    self.config.storage_backend
                )));
            }
        }
        for file in &files {
            if let Some(token) = file
                .metadata
                .upload_token
                .as_deref()
                .map(str::trim)
                .filter(|token| !token.is_empty())
            {
                let payload = validate_file_upload_token_context(
                    token,
                    Utc::now(),
                    &self.config.upload_token_secret,
                )?;
                if payload.user_id != context.user_id.as_i64()
                    || payload.storage_scope != context.storage_scope
                {
                    return Err(Error::InvalidInput(
                        "file upload token does not belong to this request".to_string(),
                    ));
                }
            }
            if let Some(repository) = self.repository.as_ref() {
                if repository
                    .object_validated(&self.config.storage_backend, &file.object_key)
                    .await?
                {
                    continue;
                }
                return Err(Error::InvalidInput(
                    "file upload session has not been completed".to_string(),
                ));
            }
        }
        strip_internal_file_metadata(&mut files);
        validate_stored_files(&files)?;
        attach_prepared_file_urls(self, &mut files)?;
        for file in &mut files {
            file.object_access = self.file_object_access(
                &file.storage_backend,
                &file.object_key,
                context.object_kind,
            )?;
        }
        if let Some(repository) = self.repository.as_ref() {
            super::media_processing::attach_variants_to_files(
                self,
                repository.as_ref(),
                &mut files,
                context.object_kind,
            )
            .await?;
        }
        Ok(files)
    }

    fn create_reuse_grant(&self, request: CreateFileReuseGrant<'_>) -> Result<FileReuseGrant> {
        file_reuse_grant(&request, &self.config.upload_token_secret)
    }

    async fn validate_reuse_grant(
        &self,
        token: &str,
        context: FileStorageContext<'_>,
    ) -> Result<ValidatedFileReuseGrant> {
        validate_file_reuse_grant(token, context, Utc::now(), &self.config.upload_token_secret)
    }

    async fn delete_files(
        &self,
        origin: FileStorageCleanupOrigin,
        files: &[FileReferenceTarget],
    ) -> Result<()> {
        let origin_label = origin.as_str();
        let mut failed_count = 0_u64;
        let mut last_error = None;
        for file in files {
            if file.storage_backend != self.config.storage_backend {
                continue;
            }
            crate::metrics::file_storage::FILE_OBJECT_DELETE_ATTEMPTS
                .with_label_values(&[origin_label, &file.storage_backend])
                .inc();
            if let Some(repository) = self.repository.as_ref() {
                let delete_claimed = repository
                    .claim_object_for_delete(
                        &file.storage_backend,
                        &file.object_key,
                        &file.reference_kind,
                        &file.reference_id,
                        origin == FileStorageCleanupOrigin::UnreferencedObject,
                    )
                    .await?;
                if !delete_claimed {
                    continue;
                }
            }
            let mut objects = Vec::new();
            if let Some(repository) = self.repository.as_ref() {
                let derived_variants = repository
                    .list_derived_object_variants(&file.storage_backend, &file.object_key)
                    .await?;
                objects.extend(
                    derived_variants
                        .into_iter()
                        .map(|variant| (variant.storage_backend, variant.object_key)),
                );
            }
            objects.push((file.storage_backend.clone(), file.object_key.clone()));
            let mut file_delete_failed = false;
            for (_, object_key) in &objects {
                match self.operator.delete(object_key).await {
                    Ok(()) => {}
                    Err(error) => {
                        file_delete_failed = true;
                        failed_count += 1;
                        last_error = Some(error.to_string());
                        crate::metrics::file_storage::FILE_OBJECT_DELETE_FAILURES
                            .with_label_values(&[origin_label, &file.storage_backend])
                            .inc();
                        tracing::warn!(
                            error = %error,
                            object_key,
                            "failed to delete file object"
                        );
                    }
                }
            }
            if file_delete_failed {
                continue;
            }
            if let Some(repository) = self.repository.as_ref() {
                for (storage_backend, object_key) in objects
                    .iter()
                    .filter(|(_, object_key)| object_key != &file.object_key)
                {
                    repository
                        .delete_object(storage_backend, object_key)
                        .await?;
                }
                repository
                    .delete_object(&file.storage_backend, &file.object_key)
                    .await?;
            }
        }
        if failed_count > 0 {
            return Err(Error::Internal(format!(
                "failed to delete {failed_count} file object(s): {}",
                last_error.unwrap_or_else(|| "unknown error".to_string())
            )));
        }
        Ok(())
    }

    async fn cleanup_expired_upload_session(
        &self,
        session: crate::models::FileUploadSessionRecord,
    ) -> Result<bool> {
        if session.storage_backend != self.config.storage_backend {
            return Ok(false);
        }
        if session.completed_at.is_some() || session.expires_at > Utc::now() {
            return Ok(false);
        }
        let repository = self.repository()?;
        if let Some(upload_id) = session.upload_id.as_deref() {
            if let Err(error) = self
                .abort_s3_multipart_upload(&session.object_key, upload_id)
                .await
            {
                tracing::warn!(
                    error = %error,
                    object_key = %session.object_key,
                    upload_id,
                    "failed to abort expired S3 multipart upload"
                );
            }
        }
        if let Err(error) = self.operator.delete(&session.object_key).await {
            tracing::debug!(
                error = %error,
                object_key = %session.object_key,
                "expired S3 upload object delete skipped or failed"
            );
        }
        repository
            .delete_upload_session_parts(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        let (_, reference_id) =
            super::upload_session_reference_target(session.metadata.file_id.as_str());
        if !reference_id.is_empty() {
            repository
                .release_reference(
                    super::FILE_UPLOAD_SESSION_REFERENCE_KIND,
                    &reference_id,
                    &self.config.storage_backend,
                    &session.object_key,
                )
                .await?;
        }
        let session_deleted = repository
            .delete_upload_session(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        if !repository
            .object_validated(&self.config.storage_backend, &session.object_key)
            .await?
            && repository
                .object_reference_count_excluding_kind(
                    &self.config.storage_backend,
                    &session.object_key,
                    super::FILE_UPLOAD_SESSION_REFERENCE_KIND,
                )
                .await?
                == 0
        {
            repository
                .delete_object(&self.config.storage_backend, &session.object_key)
                .await?;
        }
        Ok(session_deleted)
    }

    async fn store_upload(&self, upload: StoreFileUpload) -> Result<StoreFileUploadResult> {
        let StoreFileUpload {
            encoded_object_key,
            upload_token,
            content_type,
            range,
            data,
        } = upload;
        if data.is_empty() {
            return Err(Error::InvalidInput(
                "file upload part must be non-empty".to_string(),
            ));
        }
        let upload_session_key = decode_file_object_key(&encoded_object_key)?;
        let payload = validate_file_upload_token(
            &self.config.storage_backend,
            &upload_token,
            &upload_session_key,
            Utc::now(),
            &self.config.upload_token_secret,
        )?;
        let repository = self.repository()?;
        let session = repository
            .get_upload_session(&self.config.storage_backend, &upload_session_key)
            .await?
            .ok_or_else(|| Error::InvalidInput("file upload session was not found".to_string()))?;
        if session.completed_at.is_some() || session.expires_at <= Utc::now() {
            return Err(Error::InvalidInput(
                "file upload session is not active".to_string(),
            ));
        }
        let mime_type = payload
            .mime_type
            .as_deref()
            .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
        if mime_type != session.mime_type {
            return Err(Error::InvalidInput(
                "file upload token does not match upload session".to_string(),
            ));
        }
        if let Some(content_type) = content_type.as_deref() {
            if upload_media_type(content_type)? != mime_type {
                return Err(Error::InvalidInput(
                    "file content-type does not match upload session".to_string(),
                ));
            }
        }
        let expected_size = payload
            .size_bytes
            .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
        if expected_size != session.size_bytes {
            return Err(Error::InvalidInput(
                "file upload token does not match upload session".to_string(),
            ));
        }
        let actual_part_checksum = hex::encode(Sha256::digest(&data));
        let range = range.unwrap_or(crate::models::FileUploadRange {
            start: 0,
            end_inclusive: payload_len_i64(data.len())? - 1,
            total_size: expected_size,
        });
        // Validate against the persisted session plan. S3 and database
        // multipart sessions share the same resume/idempotency contract: the
        // per-session manifest defines accepted offsets, sizes, and checksums.
        let part_index =
            validate_upload_range(range, data.len(), expected_size, session.part_size_bytes)?;
        let part_number = part_index.checked_add(1).ok_or_else(|| {
            Error::InvalidInput("file upload part number is too large".to_string())
        })?;
        let manifest_parts = upload_manifest_parts_from_metadata(&session.metadata)?;
        let expected_part = manifest_parts
            .iter()
            .find(|part| part.part_number == part_number)
            .ok_or_else(|| {
                Error::InvalidInput("file upload part is not in manifest".to_string())
            })?;
        let part_size = payload_len_i64(data.len())?;
        if expected_part.offset_bytes != range.start
            || expected_part.size_bytes != part_size
            || expected_part.checksum_sha256 != actual_part_checksum
        {
            return Err(Error::InvalidInput(
                "file upload part does not match manifest".to_string(),
            ));
        }
        if upload_session_is_single_object(session.session_kind) {
            if range.start != 0 || part_size != expected_size {
                return Err(Error::InvalidInput(
                    "file upload object does not match manifest".to_string(),
                ));
            }
            let actual_content_manifest_sha256 = file_part_manifest_digest(
                expected_size,
                session.part_size_bytes,
                [(
                    expected_part.part_number,
                    part_size,
                    actual_part_checksum.as_str(),
                )],
            )?;
            if !constant_time_eq(
                actual_content_manifest_sha256.as_bytes(),
                session.content_manifest_sha256.as_bytes(),
            ) {
                return Err(Error::InvalidInput(
                    "file manifest does not match uploaded object".to_string(),
                ));
            }
            self.operator
                .write_with(&session.object_key, data)
                .content_type(&session.mime_type)
                .await
                .map_err(|error| {
                    Error::Internal(format!("failed to write S3 file object: {error}"))
                })?;
            let blob = self.complete_single_upload_session(&session).await?;
            return Ok(StoreFileUploadResult::Complete(blob));
        }
        if !upload_session_is_multipart(session.session_kind) {
            return Err(Error::InvalidInput(
                "file upload session kind is invalid".to_string(),
            ));
        }
        let existing_parts = repository
            .list_upload_session_parts(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        if let Some(existing) = existing_parts
            .iter()
            .find(|part| part.part_index == part_index)
        {
            if existing.offset_bytes != range.start
                || existing.size_bytes != part_size
                || existing.checksum_sha256.as_deref() != Some(actual_part_checksum.as_str())
            {
                return Err(Error::InvalidInput(
                    "file upload part conflicts with an existing part".to_string(),
                ));
            }
            let (uploaded_size_bytes, uploaded_parts) =
                upload_session_parts_progress(&existing_parts)?;
            return Ok(StoreFileUploadResult::PartAccepted {
                uploaded_size_bytes,
                uploaded_parts,
            });
        }
        let upload_id = session
            .upload_id
            .as_deref()
            .ok_or_else(|| Error::InvalidInput("S3 upload_id is required".to_string()))?;
        let etag = self
            .upload_s3_multipart_part(
                &session.object_key,
                upload_id,
                part_number,
                &actual_part_checksum,
                range.start,
                data,
            )
            .await?;
        repository
            .upsert_upload_session_part(UpsertFileUploadSessionPart {
                storage_backend: &self.config.storage_backend,
                upload_session_key: &session.upload_session_key,
                part_index,
                part_number,
                offset_bytes: range.start,
                size_bytes: part_size,
                checksum_sha256: Some(&actual_part_checksum),
                etag: Some(&etag),
            })
            .await?;
        let session_parts = repository
            .list_upload_session_parts(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        let result = self
            .finalize_multipart_upload_session(&session, &session_parts, upload_id)
            .await?;
        Ok(match result.object {
            Some(blob) => StoreFileUploadResult::Complete(blob),
            None => StoreFileUploadResult::PartAccepted {
                uploaded_size_bytes: result.uploaded_size_bytes,
                uploaded_parts: result.uploaded_parts,
            },
        })
    }

    async fn complete_upload_session(
        &self,
        request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        let repository = self.repository()?;
        let object_key = decode_file_object_key(&request.encoded_object_key)?;
        let _payload = validate_file_upload_token(
            &self.config.storage_backend,
            &request.upload_token,
            &object_key,
            Utc::now(),
            &self.config.upload_token_secret,
        )?;
        let Some(file_id) = request
            .file_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return Err(Error::InvalidInput("file_id is required".to_string()));
        };
        let (reference_kind, reference_id) =
            crate::service::file_storage::upload_session_reference_target(file_id);
        let mut reference_session = repository
            .get_upload_session_by_reference(reference_kind, &reference_id)
            .await?;
        let reference_file = if let Some(session) = reference_session.as_ref() {
            if session.storage_backend != self.config.storage_backend
                || (session.object_key != object_key && session.upload_session_key != object_key)
            {
                return Err(Error::InvalidInput(
                    "file reference does not match upload session".to_string(),
                ));
            }
            if let Some(ownership_proof) = session.metadata.ownership_proof.as_ref() {
                if session.expires_at <= Utc::now() {
                    return Err(Error::InvalidInput(
                        "file upload session is not active".to_string(),
                    ));
                }
                Some((
                    session.object_key.clone(),
                    session.mime_type.clone(),
                    session.size_bytes,
                    session.content_manifest_sha256.clone(),
                    session.metadata.clone(),
                    ownership_proof.clone(),
                ))
            } else {
                if upload_session_is_single_object(session.session_kind)
                    && session.completed_at.is_some()
                {
                    return Ok(CompleteFileUploadSessionResult {
                        object: Some(super::session_record_blob(
                            session,
                            Vec::new(),
                            upload_session_object_metadata(&session.metadata),
                        )),
                        uploaded_size_bytes: session.size_bytes,
                        uploaded_parts: vec![1],
                    });
                }
                if session.completed_at.is_some() || session.expires_at <= Utc::now() {
                    return Err(Error::InvalidInput(
                        "file upload session is not active".to_string(),
                    ));
                }
                if upload_session_is_single_object(session.session_kind) {
                    return Ok(CompleteFileUploadSessionResult {
                        object: None,
                        uploaded_size_bytes: 0,
                        uploaded_parts: Vec::new(),
                    });
                }
                None
            }
        } else {
            let reference = repository
                .get_active_reference_metadata_by_target(reference_kind, &reference_id)
                .await?
                .ok_or_else(|| Error::InvalidInput("file reference was not found".to_string()))?;
            if reference.storage_backend != self.config.storage_backend {
                return Err(Error::InvalidInput(
                    "file reference does not match upload session".to_string(),
                ));
            }
            if reference.object_key == object_key {
                let crate::models::FileReferenceMetadata::UploadSession(metadata) =
                    reference.metadata
                else {
                    return Err(Error::InvalidInput(
                        "file reference was not found".to_string(),
                    ));
                };
                let ownership_proof = metadata.ownership_proof.clone().ok_or_else(|| {
                    Error::InvalidInput("file reference was not found".to_string())
                })?;
                Some((
                    reference.object_key,
                    reference.mime_type,
                    reference.size_bytes,
                    reference.content_manifest_sha256,
                    metadata,
                    ownership_proof,
                ))
            } else {
                let session = repository
                    .get_upload_session(&self.config.storage_backend, &object_key)
                    .await?
                    .ok_or_else(|| {
                        Error::InvalidInput(
                            "file reference does not match upload session".to_string(),
                        )
                    })?;
                if session.object_key != reference.object_key
                    || upload_session_file_id(&session.metadata)? != reference_id
                {
                    return Err(Error::InvalidInput(
                        "file reference does not match upload session".to_string(),
                    ));
                }
                reference_session = Some(session);
                None
            }
        };
        if let Some((
            reference_object_key,
            mime_type,
            size_bytes,
            content_manifest_sha256,
            metadata,
            ownership_proof,
        )) = reference_file
        {
            if metadata.ownership_proof_verified {
                return Err(Error::InvalidInput(
                    "file upload session is not active".to_string(),
                ));
            }
            let proof = request
                .ownership_proof
                .as_deref()
                .map(str::trim)
                .filter(|proof| !proof.is_empty())
                .ok_or_else(|| {
                    Error::InvalidInput("file ownership proof is required".to_string())
                })?;
            let nonce = ownership_proof.nonce.as_str();
            let ranges = ownership_proof.ranges.clone();
            let mut chunks = Vec::with_capacity(ranges.len());
            for range in &ranges {
                chunks.push(self.read_object_range(&reference_object_key, range).await?);
            }
            let expected = file_ownership_proof_digest(
                nonce,
                &ranges,
                &content_manifest_sha256,
                size_bytes,
                chunks.iter().map(Vec::as_slice),
            );
            if !constant_time_eq(proof.as_bytes(), expected.as_bytes()) {
                return Err(Error::InvalidInput(
                    "file ownership proof does not match object".to_string(),
                ));
            }
            let metadata = mark_upload_session_ownership_proof_verified(&metadata);
            repository
                .update_reference_metadata(
                    reference_kind,
                    &reference_id,
                    &self.config.storage_backend,
                    &reference_object_key,
                    &crate::models::FileReferenceMetadata::UploadSession(metadata),
                )
                .await?;
            return Ok(CompleteFileUploadSessionResult {
                object: Some(FileBlob {
                    storage_backend: self.config.storage_backend.clone(),
                    object_key: reference_object_key,
                    mime_type,
                    size_bytes,
                    total_size_bytes: size_bytes,
                    content_manifest_sha256,
                    compression: FileBlobCompression::None,
                    range: None,
                    data: Vec::new(),
                    metadata: Default::default(),
                    created_at: Utc::now(),
                }),
                uploaded_size_bytes: size_bytes,
                uploaded_parts: Vec::new(),
            });
        }
        let session = reference_session
            .take()
            .ok_or_else(|| Error::InvalidInput("file reference was not found".to_string()))?;
        if upload_session_is_single_object(session.session_kind) {
            let blob = self.complete_single_upload_session(&session).await?;
            return Ok(CompleteFileUploadSessionResult {
                object: Some(blob),
                uploaded_size_bytes: session.size_bytes,
                uploaded_parts: vec![1],
            });
        }
        if !upload_session_is_multipart(session.session_kind) {
            return Err(Error::InvalidInput(
                "file upload session kind is invalid".to_string(),
            ));
        }
        let upload_id = request
            .upload_id
            .as_deref()
            .or(session.upload_id.as_deref())
            .ok_or_else(|| Error::InvalidInput("S3 upload_id is required".to_string()))?;
        let mut uploaded_parts = Vec::with_capacity(request.parts.len());
        for part in &request.parts {
            if part.part_number <= 0 || part.size_bytes <= 0 {
                return Err(Error::InvalidInput(
                    "S3 multipart completion requires positive part number and size".to_string(),
                ));
            }
            let checksum = part.checksum_sha256.as_deref().ok_or_else(|| {
                Error::InvalidInput(
                    "S3 multipart completion requires every part checksum_sha256".to_string(),
                )
            })?;
            let offset = i64::from(part.part_number - 1)
                .checked_mul(session.part_size_bytes)
                .ok_or_else(|| {
                    Error::InvalidInput("file upload part offset overflow".to_string())
                })?;
            let expected_part_size = (session.size_bytes - offset).min(session.part_size_bytes);
            if offset < 0 || expected_part_size <= 0 || part.size_bytes != expected_part_size {
                return Err(Error::InvalidInput(
                    "S3 multipart completion part size does not match upload session".to_string(),
                ));
            }
            repository
                .upsert_upload_session_part(UpsertFileUploadSessionPart {
                    storage_backend: &self.config.storage_backend,
                    upload_session_key: &session.upload_session_key,
                    part_index: part.part_number - 1,
                    part_number: part.part_number,
                    offset_bytes: offset,
                    size_bytes: part.size_bytes,
                    checksum_sha256: Some(checksum),
                    etag: Some(&part.etag),
                })
                .await?;
            uploaded_parts.push(part.part_number);
        }
        let session_parts = repository
            .list_upload_session_parts(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        self.finalize_multipart_upload_session(&session, &session_parts, upload_id)
            .await
    }

    async fn get_object(&self, request: GetFileObject) -> Result<FileBlob> {
        collect_file_object_download(self.object_download(request).await?).await
    }

    async fn get_object_stream(&self, request: GetFileObject) -> Result<FileObjectDownload> {
        self.object_download(request).await
    }

    async fn get_object_by_key(&self, storage_backend: &str, object_key: &str) -> Result<FileBlob> {
        if storage_backend != self.config.storage_backend {
            return Err(Error::InvalidInput(format!(
                "file storage_backend must be {}",
                self.config.storage_backend
            )));
        }
        let read_token = super::file_object_read_token(
            &self.config.storage_backend,
            object_key,
            &self.config.upload_token_secret,
        )?;
        self.get_object(GetFileObject {
            encoded_object_key: encode_file_object_key(object_key),
            read_token,
            range: None,
        })
        .await
    }

    async fn get_object_reader_by_key(
        &self,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<FileObjectReader> {
        if storage_backend != self.config.storage_backend {
            return Err(Error::InvalidInput(format!(
                "file storage_backend must be {}",
                self.config.storage_backend
            )));
        }
        let size_bytes = if let Some(repository) = self.repository.as_ref() {
            repository
                .get_object(&self.config.storage_backend, object_key)
                .await?
                .map(|object| object.size_bytes)
        } else {
            None
        };
        let size_bytes = if let Some(size_bytes) = size_bytes {
            size_bytes
        } else {
            let stat = self
                .operator
                .stat(object_key)
                .await
                .map_err(|error| Error::NotFound(format!("File object not found: {error}")))?;
            i64::try_from(stat.content_length())
                .map_err(|_| Error::Internal("file object size exceeds i64::MAX".to_string()))?
        };
        let operator = self.operator.clone();
        let object_key = object_key.to_string();
        let reader = super::read_seek::RangeSeekReader::new(
            size_bytes,
            usize::try_from(size_bytes.min(1024 * 1024)).unwrap_or(1024 * 1024),
            move |offset, length| {
                let operator = operator.clone();
                let object_key = object_key.clone();
                Box::pin(async move {
                    let start = u64::try_from(offset)
                        .map_err(|_| Error::InvalidInput("file range is invalid".to_string()))?;
                    let end = start
                        .checked_add(u64::try_from(length).map_err(|_| {
                            Error::InvalidInput("file range is invalid".to_string())
                        })?)
                        .ok_or_else(|| Error::InvalidInput("file range is invalid".to_string()))?;
                    let bytes = operator
                        .read_with(&object_key)
                        .range(start..end)
                        .await
                        .map_err(|error| {
                            Error::Internal(format!("failed to read S3 file object range: {error}"))
                        })?;
                    Ok(bytes.to_bytes())
                })
            },
        )?;
        Ok(Box::new(reader))
    }

    async fn put_object_by_key(
        &self,
        storage_backend: &str,
        object_key: &str,
        mime_type: &str,
        data: Vec<u8>,
        metadata: crate::models::FileMetadata,
    ) -> Result<FileBlob> {
        if storage_backend != self.config.storage_backend {
            return Err(Error::InvalidInput(format!(
                "file storage_backend must be {}",
                self.config.storage_backend
            )));
        }
        if data.is_empty() {
            return Err(Error::InvalidInput(
                "file object payload must be non-empty".to_string(),
            ));
        }
        let size_bytes = payload_len_i64(data.len())?;
        let checksum = hex::encode(Sha256::digest(&data));
        self.operator
            .write(object_key, data.clone())
            .await
            .map_err(|error| {
                Error::Internal(format!("failed to write S3 file object variant: {error}"))
            })?;
        self.repository()?
            .upsert_object(UpsertFileObject {
                storage_backend: &self.config.storage_backend,
                object_key,
                mime_type,
                size_bytes,
                content_manifest_sha256: &checksum,
                metadata: &metadata,
            })
            .await?;
        Ok(FileBlob {
            storage_backend: self.config.storage_backend.clone(),
            object_key: object_key.to_string(),
            mime_type: mime_type.to_string(),
            size_bytes,
            total_size_bytes: size_bytes,
            content_manifest_sha256: checksum,
            compression: FileBlobCompression::None,
            range: None,
            data,
            metadata,
            created_at: Utc::now(),
        })
    }

    async fn process_object_variants(
        &self,
        storage_backend: &str,
        object_key: &str,
        object_kind: FileObjectKind,
        upload_policy: &crate::models::FileUploadPolicy,
    ) -> Result<Vec<crate::models::FileObjectVariant>> {
        if storage_backend != self.config.storage_backend {
            return Ok(Vec::new());
        }
        let repository = self.repository()?;
        super::process_file_variants_for_object(
            self,
            repository.clone(),
            storage_backend,
            object_key,
            object_kind,
            upload_policy,
        )
        .await
        .map(|result| result.variants)
    }
}

fn s3_upload_part_urls(
    service: &S3CompatibleFileStorageService,
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
        let (upload_url, upload_headers) = service.presigned_upload_part_url(
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

fn s3_operator_from_config(config: &S3FileStorageConfig) -> Result<Operator> {
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
        .map(opendal::OperatorBuilder::finish)
        .map_err(|error| Error::Internal(format!("failed to initialize S3 file storage: {error}")))
}

fn s3_http_client() -> reqwest::Client {
    crate::install_process_crypto_provider();
    reqwest::Client::new()
}

#[cfg(feature = "test-support")]
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

fn extract_xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn sha256_hex_to_base64(value: &str) -> Result<String> {
    let bytes = hex::decode(value.trim()).map_err(|_| {
        Error::InvalidInput("checksum_sha256 must be a 64-character hex string".to_string())
    })?;
    if bytes.len() != 32 {
        return Err(Error::InvalidInput(
            "checksum_sha256 must be a 64-character hex string".to_string(),
        ));
    }
    Ok(BASE64_STANDARD.encode(bytes))
}

fn amz_datetime(date: DateTime<Utc>) -> String {
    date.format("%Y%m%dT%H%M%SZ").to_string()
}

fn amz_date(date: DateTime<Utc>) -> String {
    date.format("%Y%m%d").to_string()
}

fn credential_scope(date: DateTime<Utc>, region: &str) -> String {
    format!("{}/{}/s3/aws4_request", amz_date(date), region.trim())
}

fn string_to_sign(date: DateTime<Utc>, region: &str, canonical_request: &str) -> String {
    let request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    format!(
        "{AWS4_ALGORITHM}\n{}\n{}\n{}",
        amz_datetime(date),
        credential_scope(date, region),
        request_hash
    )
}

fn sign_hex(
    secret: &str,
    date: DateTime<Utc>,
    region: &str,
    string_to_sign: &str,
) -> Result<String> {
    let date_key = hmac_sha256(
        format!("AWS4{secret}").as_bytes(),
        amz_date(date).as_bytes(),
    )?;
    let date_region_key = hmac_sha256(&date_key, region.trim().as_bytes())?;
    let date_region_service_key = hmac_sha256(&date_region_key, b"s3")?;
    let signing_key = hmac_sha256(&date_region_service_key, b"aws4_request")?;
    Ok(hex::encode(hmac_sha256(
        &signing_key,
        string_to_sign.as_bytes(),
    )?))
}

fn hmac_sha256(key: &[u8], payload: &[u8]) -> Result<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|error| Error::Internal(format!("failed to initialize HMAC-SHA256: {error}")))?;
    mac.update(payload);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn host_header(url: &url::Url) -> Result<String> {
    let host = url
        .host_str()
        .ok_or_else(|| Error::InvalidInput("S3 URL is missing host".to_string()))?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

fn canonical_request(
    method: &str,
    url: &url::Url,
    headers: &[(&str, &str)],
    canonical_query: &str,
    payload_hash: &str,
) -> Result<String> {
    let mut canonical_headers = headers
        .iter()
        .map(|(name, value)| {
            (
                name.trim().to_ascii_lowercase(),
                value.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect::<Vec<_>>();
    canonical_headers.sort_by(|left, right| left.0.cmp(&right.0));
    let mut canonical_header_lines = String::new();
    for (name, value) in &canonical_headers {
        writeln!(&mut canonical_header_lines, "{name}:{value}").map_err(|error| {
            Error::Internal(format!("failed to build canonical headers: {error}"))
        })?;
    }
    Ok(format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method,
        canonical_uri(url),
        canonical_query,
        canonical_header_lines,
        signed_headers(headers),
        payload_hash
    ))
}

fn signed_headers(headers: &[(&str, &str)]) -> String {
    let mut names = headers
        .iter()
        .map(|(name, _)| name.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names.join(";")
}

fn canonical_uri(url: &url::Url) -> String {
    let path = url.path();
    if path.is_empty() {
        return "/".to_string();
    }
    path.split('/')
        .map(|segment| utf8_percent_encode(segment, PATH_ENCODE_SET).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn canonical_query(url: &url::Url) -> String {
    let mut pairs = url
        .query_pairs()
        .map(|(key, value)| {
            (
                utf8_percent_encode(&key, QUERY_ENCODE_SET).to_string(),
                utf8_percent_encode(&value, QUERY_ENCODE_SET).to_string(),
            )
        })
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
pub(super) fn presigned_upload_headers(
    headers: &http::HeaderMap,
) -> Result<std::collections::BTreeMap<String, String>> {
    headers
        .iter()
        .filter(|(name, _)| name.as_str() != "host")
        .map(|(name, value)| {
            let value = value.to_str().map_err(|error| {
                Error::Internal(format!(
                    "S3 presigned upload header {} is not valid UTF-8: {error}",
                    name.as_str()
                ))
            })?;
            Ok((name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}
