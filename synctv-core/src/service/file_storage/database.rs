use std::sync::Arc;

use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::{
    models::{
        CreateFileUploadSession, FileBlob, FileBlobCompression, FileReferenceTarget,
        FileUploadSession, NewStoredFile,
    },
    repository::{FileStorageRepository, UpsertFileBlob},
    service::file_storage::{
        attach_file_ownership_proof_token, attach_file_upload_token, constant_time_eq,
        database_file_namespace_base_path, database_file_object_url,
        decode_database_file_object_key, file_content_object_key, file_id_from_request,
        file_object_key, file_ownership_proof_digest, file_upload_token_payload_from_file,
        optional_payload_bool, ownership_proof_chunks_from_bytes,
        ownership_proof_ranges_from_payload, payload_len_i64, server_file_object_id,
        strip_internal_file_metadata, upload_media_type, validate_create_file_upload_session,
        validate_database_file_read_token, validate_database_file_upload_token,
        validate_file_mime_type, validate_file_upload_tokens, validate_stored_files,
        DatabaseFileStorageService, FileStorageCleanupOrigin, FileStorageContext,
        FileStorageService, FILE_OWNERSHIP_PROOF_KEY, FILE_UPLOAD_EXPIRES_SECONDS,
        FILE_UPLOAD_TOKEN_HEADER, FILE_UPLOAD_TOKEN_KEY, MAX_DATABASE_FILE_UPLOAD_SIZE_BYTES,
    },
    Error, Result,
};

impl DatabaseFileStorageService {
    #[must_use]
    pub fn new(
        storage_backend: impl Into<String>,
        repository: Arc<FileStorageRepository>,
        upload_token_secret: impl Into<String>,
    ) -> Self {
        Self::new_with_compression(
            storage_backend,
            repository,
            upload_token_secret,
            FileBlobCompression::Zstd,
        )
    }

    #[must_use]
    pub fn new_with_compression(
        storage_backend: impl Into<String>,
        repository: Arc<FileStorageRepository>,
        upload_token_secret: impl Into<String>,
        compression: FileBlobCompression,
    ) -> Self {
        Self {
            storage_backend: storage_backend.into(),
            repository,
            upload_token_secret: upload_token_secret.into(),
            compression,
        }
    }

    async fn load_blob(&self, object_key: &str) -> Result<Option<FileBlob>> {
        match self
            .repository
            .get_blob(&self.storage_backend, object_key)
            .await?
        {
            Some(blob) => decompress_blob(blob).await.map(Some),
            None => Ok(None),
        }
    }
}

fn database_upload_max_size_bytes(policy_max_size_bytes: i64) -> i64 {
    policy_max_size_bytes.min(MAX_DATABASE_FILE_UPLOAD_SIZE_BYTES as i64)
}

fn validate_database_upload_session_size(request: &CreateFileUploadSession) -> Result<i64> {
    let max_size_bytes = database_upload_max_size_bytes(request.policy.max_size_bytes);
    if request.size_bytes > max_size_bytes {
        return Err(Error::InvalidInput(format!(
            "file size must be between 1 and {max_size_bytes} bytes for database storage"
        )));
    }
    Ok(max_size_bytes)
}

async fn compress_payload(
    compression: FileBlobCompression,
    data: Vec<u8>,
) -> Result<(FileBlobCompression, Vec<u8>)> {
    match compression {
        FileBlobCompression::None => Ok((compression, data)),
        FileBlobCompression::Lz4 => {
            let data = tokio::task::spawn_blocking(move || lz4_flex::compress_prepend_size(&data))
                .await
                .map_err(|error| {
                    Error::Internal(format!("file compression task failed: {error}"))
                })?;
            Ok((compression, data))
        }
        FileBlobCompression::Zstd => {
            let data = tokio::task::spawn_blocking(move || zstd::bulk::compress(&data, 0))
                .await
                .map_err(|error| Error::Internal(format!("file compression task failed: {error}")))?
                .map_err(|error| Error::Internal(format!("file compression failed: {error}")))?;
            Ok((compression, data))
        }
    }
}

async fn decompress_blob(mut blob: FileBlob) -> Result<FileBlob> {
    let compression = blob.compression;
    let expected_size = usize::try_from(blob.size_bytes)
        .map_err(|_| Error::Internal("file blob size is invalid".to_string()))?;
    let data = std::mem::take(&mut blob.data);
    blob.data = match compression {
        FileBlobCompression::None => data,
        FileBlobCompression::Lz4 => {
            tokio::task::spawn_blocking(move || lz4_flex::decompress_size_prepended(&data))
                .await
                .map_err(|error| {
                    Error::Internal(format!("file decompression task failed: {error}"))
                })?
                .map_err(|error| Error::Internal(format!("file decompression failed: {error}")))?
        }
        FileBlobCompression::Zstd => {
            tokio::task::spawn_blocking(move || zstd::bulk::decompress(&data, expected_size))
                .await
                .map_err(|error| {
                    Error::Internal(format!("file decompression task failed: {error}"))
                })?
                .map_err(|error| Error::Internal(format!("file decompression failed: {error}")))?
        }
    };
    blob.compression = FileBlobCompression::None;
    let actual_size = payload_len_i64(blob.data.len())?;
    if actual_size != blob.size_bytes {
        return Err(Error::Internal(format!(
            "file blob decompressed size mismatch for {}:{}",
            blob.storage_backend, blob.object_key
        )));
    }
    let actual_checksum = hex::encode(Sha256::digest(&blob.data));
    if actual_checksum != blob.checksum_sha256 {
        return Err(Error::Internal(format!(
            "file blob checksum mismatch for {}:{}",
            blob.storage_backend, blob.object_key
        )));
    }
    Ok(blob)
}

#[async_trait::async_trait]
impl FileStorageService for DatabaseFileStorageService {
    fn backend_name(&self) -> &str {
        &self.storage_backend
    }

    fn object_url(
        &self,
        storage_backend: &str,
        object_key: &str,
        database_object_route_prefix: &str,
    ) -> Result<Option<String>> {
        if storage_backend != self.storage_backend {
            return Ok(None);
        }
        database_file_object_url(
            database_object_route_prefix,
            storage_backend,
            object_key,
            &self.upload_token_secret,
        )
        .map(Some)
    }

    async fn create_upload_session(
        &self,
        request: CreateFileUploadSession,
    ) -> Result<FileUploadSession> {
        validate_create_file_upload_session(&request)?;
        let max_size_bytes = validate_database_upload_session_size(&request)?;
        let checksum_sha256 = request
            .checksum_sha256
            .as_deref()
            .map(|value| value.trim().to_ascii_lowercase());
        let file_id = file_id_from_request(request.client_file_id.as_deref());
        let expires_at = Utc::now() + chrono::Duration::seconds(FILE_UPLOAD_EXPIRES_SECONDS);
        if let Some(checksum) = checksum_sha256.as_deref() {
            if let Some(existing) = self
                .repository
                .get_object_by_checksum(&self.storage_backend, checksum, request.size_bytes)
                .await?
            {
                validate_file_mime_type(&request.policy, &existing.mime_type)?;
                if !self
                    .repository
                    .blob_exists(&self.storage_backend, &existing.object_key)
                    .await?
                {
                    return Err(Error::Internal(
                        "file object registry points to a missing database blob".to_string(),
                    ));
                }
                let mut file = NewStoredFile {
                    id: file_id,
                    filename: request.filename.clone(),
                    storage_backend: self.storage_backend.clone(),
                    object_key: existing.object_key.clone(),
                    url: Some(database_file_object_url(
                        &request.policy.database_object_route_prefix,
                        &self.storage_backend,
                        &existing.object_key,
                        &self.upload_token_secret,
                    )?),
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
                    &self.upload_token_secret,
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
                    max_size_bytes,
                });
            }
        }
        let object_key = if let Some(checksum) = checksum_sha256.as_deref() {
            match self
                .repository
                .get_any_object_by_checksum(&self.storage_backend, checksum, request.size_bytes)
                .await?
            {
                Some(existing) => existing.object_key,
                None => file_content_object_key(
                    &database_file_namespace_base_path(&request.policy.storage_namespace),
                    checksum,
                ),
            }
        } else {
            file_object_key(
                &database_file_namespace_base_path(&request.policy.storage_namespace),
                &request.storage_scope,
                &server_file_object_id(),
                &request.mime_type,
            )
        };
        let mut file = NewStoredFile {
            id: file_id,
            filename: request.filename,
            storage_backend: self.storage_backend.clone(),
            object_key,
            url: None,
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
            &self.upload_token_secret,
            checksum_sha256.as_deref(),
            None,
        )?;
        file.url = Some(database_file_object_url(
            &request.policy.database_object_route_prefix,
            &self.storage_backend,
            &file.object_key,
            &self.upload_token_secret,
        )?);
        validate_stored_files(std::slice::from_ref(&file))?;
        let mut upload_headers = std::collections::BTreeMap::new();
        upload_headers.insert(
            "content-type".to_string(),
            file.mime_type
                .as_deref()
                .ok_or_else(|| Error::InvalidInput("file mime_type is required".to_string()))?
                .to_string(),
        );
        if let Some(token) = file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
        {
            upload_headers.insert(FILE_UPLOAD_TOKEN_HEADER.to_string(), token.to_string());
        }
        let object_url = database_file_object_url(
            &request.policy.database_object_route_prefix,
            &self.storage_backend,
            &file.object_key,
            &self.upload_token_secret,
        )?;
        Ok(FileUploadSession {
            file,
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            ownership_proof_metadata_key: None,
            upload_url: Some(object_url),
            upload_method: Some("PUT".to_string()),
            upload_headers,
            expires_at: Some(Utc::now() + chrono::Duration::seconds(FILE_UPLOAD_EXPIRES_SECONDS)),
            max_size_bytes,
        })
    }

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        mut files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        validate_stored_files(&files)?;
        for file in &files {
            if file.storage_backend != self.storage_backend {
                return Err(Error::InvalidInput(format!(
                    "file storage_backend must be {}",
                    self.storage_backend
                )));
            }
            if !self
                .repository
                .blob_exists(&self.storage_backend, &file.object_key)
                .await?
            {
                return Err(Error::InvalidInput(
                    "file object has not been uploaded".to_string(),
                ));
            }
        }
        validate_file_upload_tokens(context, &files, &self.upload_token_secret)?;
        for file in &files {
            let payload =
                file_upload_token_payload_from_file(file, context.user_id, context.storage_scope)?;
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
                let ranges = ownership_proof_ranges_from_payload(&payload)?;
                let blob = self.load_blob(&file.object_key).await?.ok_or_else(|| {
                    Error::InvalidInput("file object has not been uploaded".to_string())
                })?;
                let chunks = ownership_proof_chunks_from_bytes(&blob.data, &ranges)?;
                let expected =
                    file_ownership_proof_digest(nonce, &ranges, chunks.iter().map(Vec::as_slice));
                if !constant_time_eq(proof.as_bytes(), expected.as_bytes()) {
                    return Err(Error::InvalidInput(
                        "file ownership proof does not match object".to_string(),
                    ));
                }
            }
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
        for file in files {
            if file.storage_backend != self.storage_backend {
                continue;
            }
            crate::metrics::file_storage::FILE_OBJECT_DELETE_ATTEMPTS
                .with_label_values(&[origin_label, &file.storage_backend])
                .inc();
            self.repository
                .release_reference(
                    &file.reference_kind,
                    &file.reference_id,
                    &file.storage_backend,
                    &file.object_key,
                )
                .await?;
            if self
                .repository
                .object_reference_count(&file.storage_backend, &file.object_key)
                .await?
                > 0
            {
                continue;
            }
            if let Err(error) = self
                .repository
                .delete_blob(&self.storage_backend, &file.object_key)
                .await
            {
                crate::metrics::file_storage::FILE_OBJECT_DELETE_FAILURES
                    .with_label_values(&[origin_label, &file.storage_backend])
                    .inc();
                return Err(error);
            }
            self.repository
                .delete_object(&file.storage_backend, &file.object_key)
                .await?;
        }
        Ok(())
    }

    async fn store_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        data: Vec<u8>,
    ) -> Result<FileBlob> {
        if data.is_empty() || data.len() > MAX_DATABASE_FILE_UPLOAD_SIZE_BYTES {
            return Err(Error::InvalidInput(format!(
                "file payload must be between 1 and {MAX_DATABASE_FILE_UPLOAD_SIZE_BYTES} bytes"
            )));
        }
        let object_key = decode_database_file_object_key(encoded_object_key)?;
        let payload = validate_database_file_upload_token(
            &self.storage_backend,
            upload_token,
            &object_key,
            Utc::now(),
            &self.upload_token_secret,
        )?;
        let mime_type = payload
            .get("mime_type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
        if let Some(content_type) = content_type {
            if upload_media_type(content_type)? != mime_type {
                return Err(Error::InvalidInput(
                    "file content-type does not match upload session".to_string(),
                ));
            }
        }
        let expected_size = payload
            .get("size_bytes")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
        if expected_size != payload_len_i64(data.len())? {
            return Err(Error::InvalidInput(
                "file payload size does not match upload session".to_string(),
            ));
        }
        let actual_checksum = hex::encode(Sha256::digest(&data));
        if let Some(expected_checksum) = payload
            .get("checksum_sha256")
            .and_then(serde_json::Value::as_str)
        {
            if !constant_time_eq(
                actual_checksum.as_bytes(),
                expected_checksum.to_ascii_lowercase().as_bytes(),
            ) {
                return Err(Error::InvalidInput(
                    "file payload checksum does not match upload session".to_string(),
                ));
            }
        }
        let (compression, stored_data) = compress_payload(self.compression, data).await?;
        let empty_metadata = serde_json::Value::Object(Default::default());
        let blob = self
            .repository
            .upsert_blob(UpsertFileBlob {
                storage_backend: &self.storage_backend,
                object_key: &object_key,
                mime_type,
                size_bytes: expected_size,
                checksum_sha256: &actual_checksum,
                compression,
                data: stored_data,
                metadata: &empty_metadata,
            })
            .await?;
        self.repository
            .upsert_object(
                &self.storage_backend,
                &object_key,
                mime_type,
                expected_size,
                &actual_checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await?;
        decompress_blob(blob).await
    }

    async fn get_object(&self, encoded_object_key: &str, read_token: &str) -> Result<FileBlob> {
        let object_key = decode_database_file_object_key(encoded_object_key)?;
        validate_database_file_read_token(
            &self.storage_backend,
            &object_key,
            read_token,
            &self.upload_token_secret,
        )?;
        self.load_blob(&object_key)
            .await?
            .ok_or_else(|| Error::NotFound("File object not found".to_string()))
    }
}
