use std::{sync::Arc, time::Duration as StdDuration};

use chrono::Utc;
use opendal::{services::S3, Operator};
use sha2::{Digest, Sha256};

use crate::{
    models::{
        CreateFileUploadSession, FileOwnershipProofRange, FileReferenceTarget, FileUploadSession,
        NewStoredFile,
    },
    repository::FileStorageRepository,
    service::file_storage::{
        attach_file_ownership_proof_token, attach_file_upload_token, constant_time_eq,
        file_content_object_key, file_id_from_request, file_object_key,
        file_ownership_proof_digest, file_storage_object_base_path,
        file_upload_token_payload_from_file, normalized_checksum_sha256,
        optional_file_storage_public_url, optional_payload_bool, payload_len_i64,
        server_file_object_id, strip_internal_file_metadata, validate_create_file_upload_session,
        validate_file_mime_type, validate_file_upload_tokens, validate_s3_file_storage_config,
        validate_stored_files, FileStorageCleanupOrigin, FileStorageContext, FileStorageService,
        FILE_OWNERSHIP_PROOF_KEY, FILE_UPLOAD_EXPIRES_SECONDS,
    },
    Error, Result,
};

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
        })
    }

    #[cfg(test)]
    pub(crate) fn with_operator(mut self, operator: Operator) -> Self {
        self.operator = operator;
        self
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
}

#[async_trait::async_trait]
impl FileStorageService for S3CompatibleFileStorageService {
    fn backend_name(&self) -> &str {
        &self.config.storage_backend
    }

    fn object_url(
        &self,
        storage_backend: &str,
        object_key: &str,
        _database_object_route_prefix: &str,
    ) -> Result<Option<String>> {
        if storage_backend != self.config.storage_backend {
            return Ok(None);
        }
        optional_file_storage_public_url(&self.config, object_key)
    }

    async fn create_upload_session(
        &self,
        request: CreateFileUploadSession,
    ) -> Result<FileUploadSession> {
        validate_create_file_upload_session(&request)?;
        let checksum_sha256 = normalized_checksum_sha256(request.checksum_sha256.as_deref());
        let file_id = file_id_from_request(request.client_file_id.as_deref());
        let now = Utc::now();
        let expires = self
            .config
            .upload_expires_seconds
            .clamp(60, FILE_UPLOAD_EXPIRES_SECONDS);
        let expires_at = now + chrono::Duration::seconds(expires);
        if let (Some(repository), Some(checksum)) =
            (self.repository.as_ref(), checksum_sha256.as_deref())
        {
            if let Some(existing) = repository
                .get_object_by_checksum(&self.config.storage_backend, checksum, request.size_bytes)
                .await?
            {
                if self.operator.stat(&existing.object_key).await.is_ok() {
                    validate_file_mime_type(&request.policy, &existing.mime_type)?;
                    let mut file = NewStoredFile {
                        id: file_id,
                        storage_backend: self.config.storage_backend.clone(),
                        object_key: existing.object_key.clone(),
                        url: optional_file_storage_public_url(&self.config, &existing.object_key)?,
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
                        checksum,
                        request.size_bytes,
                    )?;
                    validate_stored_files(std::slice::from_ref(&file))?;
                    return Ok(FileUploadSession {
                        file,
                        upload_required: false,
                        ownership_proof_required: true,
                        ownership_proof_nonce: Some(nonce),
                        ownership_proof_ranges: ranges,
                        ownership_proof_metadata_key: Some(FILE_OWNERSHIP_PROOF_KEY.to_string()),
                        upload_url: None,
                        upload_method: None,
                        upload_headers: Default::default(),
                        expires_at: Some(expires_at),
                        max_size_bytes: request.policy.max_size_bytes,
                    });
                }
            }
        }
        let object_base_path = file_storage_object_base_path(
            &self.config.base_path,
            &request.policy.storage_namespace,
        );
        let object_key = if let (Some(repository), Some(checksum)) =
            (self.repository.as_ref(), checksum_sha256.as_deref())
        {
            match repository
                .get_any_object_by_checksum(
                    &self.config.storage_backend,
                    checksum,
                    request.size_bytes,
                )
                .await?
            {
                Some(existing) => existing.object_key,
                None => file_content_object_key(&object_base_path, checksum),
            }
        } else {
            checksum_sha256.as_deref().map_or_else(
                || {
                    file_object_key(
                        &object_base_path,
                        &request.storage_scope,
                        &server_file_object_id(),
                        &request.mime_type,
                    )
                },
                |checksum| file_content_object_key(&object_base_path, checksum),
            )
        };
        if let (Some(repository), Some(checksum)) =
            (self.repository.as_ref(), checksum_sha256.as_deref())
        {
            repository
                .upsert_pending_object(
                    &self.config.storage_backend,
                    &object_key,
                    &request.mime_type,
                    request.size_bytes,
                    checksum,
                    &serde_json::Value::Object(Default::default()),
                )
                .await?;
        }
        let presigned = self
            .operator
            .presign_write_with(&object_key, StdDuration::from_secs(expires.cast_unsigned()))
            .content_type(&request.mime_type)
            .await
            .map_err(|error| Error::Internal(format!("failed to presign S3 upload: {error}")))?;
        let upload_url = presigned.uri().to_string();
        let public_url = optional_file_storage_public_url(&self.config, &object_key)?;
        let mut file = NewStoredFile {
            id: file_id,
            storage_backend: self.config.storage_backend.clone(),
            object_key,
            url: public_url,
            mime_type: Some(request.mime_type),
            size_bytes: Some(request.size_bytes),
            width: request.width,
            height: request.height,
            metadata: request.metadata,
        };
        attach_file_upload_token(
            &mut file,
            request.user_id,
            &request.storage_scope,
            expires_at,
            &self.config.upload_token_secret,
            checksum_sha256.as_deref(),
            None,
        )?;
        validate_stored_files(std::slice::from_ref(&file))?;
        let upload_headers = presigned_upload_headers(presigned.header())?;
        Ok(FileUploadSession {
            file,
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            ownership_proof_metadata_key: None,
            upload_url: Some(upload_url),
            upload_method: Some(presigned.method().to_string()),
            upload_headers,
            expires_at: Some(expires_at),
            max_size_bytes: request.policy.max_size_bytes,
        })
    }

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        mut files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        validate_stored_files(&files)?;
        for file in &files {
            if file.storage_backend != self.config.storage_backend {
                return Err(Error::InvalidInput(format!(
                    "file storage_backend must be {}",
                    self.config.storage_backend
                )));
            }
        }
        validate_file_upload_tokens(context, &files, &self.config.upload_token_secret)?;
        for file in &files {
            let payload =
                file_upload_token_payload_from_file(file, context.user_id, context.storage_scope)?;
            let checksum = payload
                .get("checksum_sha256")
                .and_then(serde_json::Value::as_str)
                .map(str::to_ascii_lowercase);
            if optional_payload_bool(&payload, "ownership_proof_required", "file upload token")? {
                let proof = file
                    .metadata
                    .get(FILE_OWNERSHIP_PROOF_KEY)
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        Error::InvalidInput("file ownership proof is required".to_string())
                    })?;
                let nonce = payload
                    .get("ownership_proof_nonce")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
                let ranges = super::ownership_proof_ranges_from_payload(&payload)?;
                let mut chunks = Vec::with_capacity(ranges.len());
                for range in &ranges {
                    chunks.push(self.read_object_range(&file.object_key, range).await?);
                }
                let expected =
                    file_ownership_proof_digest(nonce, &ranges, chunks.iter().map(Vec::as_slice));
                if !constant_time_eq(proof.as_bytes(), expected.as_bytes()) {
                    return Err(Error::InvalidInput(
                        "file ownership proof does not match object".to_string(),
                    ));
                }
                continue;
            }

            let Some(checksum) = checksum else {
                continue;
            };
            let Some(repository) = self.repository.as_ref() else {
                continue;
            };
            if repository
                .object_validated(&self.config.storage_backend, &file.object_key)
                .await?
            {
                continue;
            }
            let bytes = self
                .operator
                .read(&file.object_key)
                .await
                .map_err(|error| {
                    Error::InvalidInput(format!("file object is not readable: {error}"))
                })?;
            let data = bytes.to_vec();
            let expected_size = payload
                .get("size_bytes")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
            if expected_size != payload_len_i64(data.len())? {
                self.delete_invalid_upload_object(&file.object_key, "size_mismatch")
                    .await;
                return Err(Error::InvalidInput(
                    "file payload size does not match upload session".to_string(),
                ));
            }
            let actual_checksum = hex::encode(Sha256::digest(&data));
            if !constant_time_eq(actual_checksum.as_bytes(), checksum.as_bytes()) {
                self.delete_invalid_upload_object(&file.object_key, "checksum_mismatch")
                    .await;
                return Err(Error::InvalidInput(
                    "file payload checksum does not match upload session".to_string(),
                ));
            }
            let mime_type = file
                .mime_type
                .as_deref()
                .ok_or_else(|| Error::InvalidInput("file mime_type is required".to_string()))?;
            repository
                .upsert_object(
                    &self.config.storage_backend,
                    &file.object_key,
                    mime_type,
                    expected_size,
                    &actual_checksum,
                    &serde_json::Value::Object(Default::default()),
                )
                .await?;
        }
        strip_internal_file_metadata(&mut files);
        Ok(files)
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
                repository
                    .release_reference(
                        &file.reference_kind,
                        &file.reference_id,
                        &file.storage_backend,
                        &file.object_key,
                    )
                    .await?;
                if repository
                    .object_reference_count(&file.storage_backend, &file.object_key)
                    .await?
                    > 0
                {
                    continue;
                }
            }
            match self.operator.delete(&file.object_key).await {
                Ok(()) => {}
                Err(error) => {
                    failed_count += 1;
                    last_error = Some(error.to_string());
                    crate::metrics::file_storage::FILE_OBJECT_DELETE_FAILURES
                        .with_label_values(&[origin_label, &file.storage_backend])
                        .inc();
                    tracing::warn!(
                        error = %error,
                        object_key = %file.object_key,
                        "failed to delete file object"
                    );
                }
            }
            if let Some(repository) = self.repository.as_ref() {
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
}

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
