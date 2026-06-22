use std::sync::Arc;

use chrono::Utc;
use futures::StreamExt;
use sha2::{Digest, Sha256};

use crate::{
    models::{
        CompleteFileUploadSession, CompleteFileUploadSessionResult, CreateFileUploadSession,
        FileBlob, FileBlobCompression, FileBlobPart, FileByteRange, FileObjectDownload,
        FileObjectMetadata, FileRangeRequest, FileReferenceTarget, FileUploadSession,
        FileUploadSessionCreateResult, FileUploadSessionKind, GetFileObject, NewStoredFile,
        StoreFileUpload, StoreFileUploadResult,
    },
    repository::{
        FileStorageRepository, UpsertFileBlobPart, UpsertFileObject, UpsertFileUploadSession,
        UpsertFileUploadSessionPart,
    },
    service::file_storage::{
        attach_file_ownership_proof_token, attach_prepared_file_urls, collect_file_object_download,
        constant_time_eq, database_file_namespace_base_path, database_file_object_url,
        decode_database_file_object_key, encode_database_file_object_key, file_object_key,
        file_ownership_proof_digest, file_part_manifest_digest, file_reuse_grant,
        file_upload_token_for_object_key, mark_upload_session_ownership_proof_verified,
        media_processing::attach_variants_to_files, new_public_file_id, optional_payload_bool,
        ownership_proof_ranges_from_payload, payload_len_i64, process_file_variants_for_object,
        register_upload_session_reference, strip_internal_file_metadata, upload_manifest_metadata,
        upload_manifest_parts_from_metadata, upload_media_type, upload_session_metadata,
        upload_session_metadata_with_manifest, upload_session_object_metadata,
        upload_session_parts_progress, upload_session_policy, upload_session_progress,
        upload_session_public_file_id, validate_create_file_upload_session,
        validate_database_file_read_token, validate_database_file_upload_token,
        validate_file_mime_type, validate_file_reuse_grant, validate_stored_files,
        validate_upload_range, validated_upload_manifest, CreateFileReuseGrant,
        DatabaseFileStorageCompressionConfig, DatabaseFileStorageService, FileObjectReader,
        FileReuseGrant, FileStorageCleanupOrigin, FileStorageContext, FileStorageService,
        UploadSessionMetadataInput, ValidatedFileReuseGrant, FILE_UPLOAD_EXPIRES_SECONDS,
        FILE_UPLOAD_TOKEN_HEADER, FILE_UPLOAD_TOKEN_KEY, MAX_DATABASE_FILE_UPLOAD_PART_SIZE_BYTES,
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
        Self::new_with_compression_config(
            storage_backend,
            repository,
            upload_token_secret,
            DatabaseFileStorageCompressionConfig::default(),
        )
    }

    #[must_use]
    pub fn new_with_compression(
        storage_backend: impl Into<String>,
        repository: Arc<FileStorageRepository>,
        upload_token_secret: impl Into<String>,
        compression: FileBlobCompression,
    ) -> Self {
        Self::new_with_compression_config(
            storage_backend,
            repository,
            upload_token_secret,
            DatabaseFileStorageCompressionConfig {
                algorithm: compression,
                ..Default::default()
            },
        )
    }

    #[must_use]
    pub fn new_with_compression_config(
        storage_backend: impl Into<String>,
        repository: Arc<FileStorageRepository>,
        upload_token_secret: impl Into<String>,
        compression: DatabaseFileStorageCompressionConfig,
    ) -> Self {
        Self {
            storage_backend: storage_backend.into(),
            repository,
            upload_token_secret: upload_token_secret.into(),
            compression,
        }
    }

    async fn load_range_data(
        &self,
        object_key: &str,
        range: Option<FileRangeRequest>,
    ) -> Result<Vec<u8>> {
        let encoded_object_key = encode_database_file_object_key(object_key);
        let read_token = super::database_file_read_token(
            &self.storage_backend,
            object_key,
            &self.upload_token_secret,
        )?;
        Ok(collect_file_object_download(
            self.get_object_stream(GetFileObject {
                encoded_object_key,
                read_token,
                range,
            })
            .await?,
        )
        .await?
        .data)
    }

    async fn object_download(&self, request: GetFileObject) -> Result<FileObjectDownload> {
        let object_key = decode_database_file_object_key(&request.encoded_object_key)?;
        validate_database_file_read_token(
            &self.storage_backend,
            &object_key,
            &request.read_token,
            &self.upload_token_secret,
        )?;
        let Some(object) = self
            .repository
            .get_object(&self.storage_backend, &object_key)
            .await?
        else {
            return Err(Error::NotFound("File object not found".to_string()));
        };
        let range = super::resolve_file_range(request.range, object.size_bytes)?;
        let read_range = range.unwrap_or(FileByteRange {
            start: 0,
            end_inclusive: object.size_bytes - 1,
        });
        let parts = self
            .repository
            .list_blob_parts_overlapping_range(
                &self.storage_backend,
                &object_key,
                read_range.start,
                read_range.end_inclusive,
            )
            .await?;
        ensure_database_parts_cover_range(&parts, read_range)?;
        let metadata = FileObjectMetadata {
            storage_backend: self.storage_backend.clone(),
            object_key,
            mime_type: object.mime_type,
            size_bytes: read_range.size_bytes(),
            total_size_bytes: object.size_bytes,
            content_manifest_sha256: object.content_manifest_sha256,
            compression: FileBlobCompression::None,
            range,
            metadata: object.metadata,
            created_at: object.created_at,
        };
        // Stream part-by-part so large database-backed downloads do not require
        // buffering the whole requested range before HTTP/gRPC starts sending.
        let stream = futures::stream::iter(parts)
            .then(move |part| async move { database_part_chunk(part, read_range).await })
            .boxed();
        Ok(FileObjectDownload { metadata, stream })
    }

    async fn finalize_upload_object(
        &self,
        session: &crate::models::FileUploadSessionRecord,
        mime_type: &str,
        expected_size: i64,
        content_manifest_sha256: &str,
        part_size_bytes: i64,
    ) -> Result<FileBlob> {
        let parts = self
            .repository
            .list_upload_session_parts(&self.storage_backend, &session.upload_session_key)
            .await?;
        let mut next_offset = 0_i64;
        let mut manifest_parts = Vec::with_capacity(parts.len());
        for part in &parts {
            if part.offset_bytes != next_offset {
                return Err(Error::InvalidInput(
                    "file upload is missing one or more parts".to_string(),
                ));
            }
            let checksum = part.checksum_sha256.as_deref().ok_or_else(|| {
                Error::InvalidInput("file upload part checksum is missing".to_string())
            })?;
            manifest_parts.push((part.part_number, part.size_bytes, checksum));
            next_offset = next_offset
                .checked_add(part.size_bytes)
                .ok_or_else(|| Error::Internal("file upload part offset overflow".to_string()))?;
        }
        if next_offset != expected_size {
            return Err(Error::InvalidInput(
                "file upload is missing one or more parts".to_string(),
            ));
        }
        let actual_content_manifest_sha256 =
            file_part_manifest_digest(expected_size, part_size_bytes, manifest_parts)?;
        if !constant_time_eq(
            actual_content_manifest_sha256.as_bytes(),
            content_manifest_sha256
                .trim()
                .to_ascii_lowercase()
                .as_bytes(),
        ) {
            self.repository
                .delete_blob_parts(&self.storage_backend, &session.upload_session_key)
                .await?;
            return Err(Error::InvalidInput(
                "file manifest does not match uploaded parts".to_string(),
            ));
        }
        let return_inline_data = parts.len() == 1;
        // Promote pending blob rows from the session key to the final object key
        // in place. The write path already stored per-part SHA-256 values, and
        // finalization verifies the manifest digest before this UPDATE. This is
        // the table-state transition for database multipart uploads: pending
        // and final bytes live in `file_blob_parts` under different object keys.
        self.repository
            .promote_blob_parts(
                &self.storage_backend,
                &session.upload_session_key,
                &session.object_key,
                parts.len(),
            )
            .await?;
        let upload_policy = upload_session_policy(&session.metadata)?;
        let metadata = upload_session_object_metadata(&session.metadata)?;
        self.repository
            .upsert_pending_object(UpsertFileObject {
                storage_backend: &self.storage_backend,
                object_key: &session.object_key,
                mime_type,
                size_bytes: expected_size,
                content_manifest_sha256: &actual_content_manifest_sha256,
                metadata: &metadata,
            })
            .await?;
        let data = if return_inline_data {
            self.load_range_data(&session.object_key, None).await?
        } else {
            Vec::new()
        };
        let mut blob = FileBlob {
            storage_backend: self.storage_backend.clone(),
            object_key: session.object_key.clone(),
            mime_type: mime_type.to_string(),
            size_bytes: expected_size,
            total_size_bytes: expected_size,
            content_manifest_sha256: actual_content_manifest_sha256,
            compression: FileBlobCompression::None,
            range: None,
            data,
            metadata,
            created_at: Utc::now(),
        };
        if let Err(error) = super::complete_uploaded_file_object(
            self,
            self.repository.as_ref(),
            &mut blob,
            &upload_policy,
        )
        .await
        {
            self.repository
                .delete_upload_session_parts(&self.storage_backend, &session.upload_session_key)
                .await?;
            self.repository
                .delete_object(&self.storage_backend, &session.upload_session_key)
                .await?;
            self.repository
                .delete_object(&self.storage_backend, &session.object_key)
                .await?;
            return Err(error);
        }
        self.repository
            .mark_object_validated(&self.storage_backend, &session.object_key)
            .await?;
        self.repository
            .complete_upload_session(&self.storage_backend, &session.upload_session_key)
            .await?;
        Ok(blob)
    }
}

struct DecompressedBlobPart {
    offset_bytes: i64,
    size_bytes: i64,
    data: Vec<u8>,
}

fn range_end_exclusive(range: FileByteRange) -> Result<i64> {
    range
        .end_inclusive
        .checked_add(1)
        .ok_or_else(|| Error::Internal("file range end overflow".to_string()))
}

fn ensure_database_parts_cover_range(parts: &[FileBlobPart], range: FileByteRange) -> Result<()> {
    if parts.is_empty() {
        return Err(Error::Internal(
            "file object has no readable blob parts".to_string(),
        ));
    }
    let end_exclusive = range_end_exclusive(range)?;
    let mut expected = range.start;
    for part in parts {
        let part_end = part
            .offset_bytes
            .checked_add(part.size_bytes)
            .ok_or_else(|| Error::Internal("file blob part offset overflow".to_string()))?;
        if part_end <= expected {
            continue;
        }
        if part.offset_bytes > expected {
            return Err(Error::Internal(
                "file object is missing one or more blob parts".to_string(),
            ));
        }
        expected = part_end.min(end_exclusive);
        if expected >= end_exclusive {
            return Ok(());
        }
    }
    Err(Error::Internal(
        "file object is missing one or more blob parts".to_string(),
    ))
}

async fn database_part_chunk(part: FileBlobPart, range: FileByteRange) -> Result<bytes::Bytes> {
    let part_data = decompress_blob_part(part).await?;
    let part_start_absolute = part_data.offset_bytes;
    let part_end_absolute = part_data
        .offset_bytes
        .checked_add(part_data.size_bytes)
        .ok_or_else(|| Error::Internal("file blob part offset overflow".to_string()))?;
    let read_start_absolute = range.start.max(part_start_absolute);
    let read_end_absolute = range_end_exclusive(range)?.min(part_end_absolute);
    if read_end_absolute <= read_start_absolute {
        return Ok(bytes::Bytes::new());
    }
    let part_start = read_start_absolute - part_start_absolute;
    let part_end_exclusive = read_end_absolute - part_start_absolute;
    let start = usize::try_from(part_start)
        .map_err(|_| Error::Internal("file range start is invalid".to_string()))?;
    let end = usize::try_from(part_end_exclusive)
        .map_err(|_| Error::Internal("file range end is invalid".to_string()))?;
    let slice = part_data
        .data
        .get(start..end)
        .ok_or_else(|| Error::Internal("file range does not fit blob part".to_string()))?;
    Ok(bytes::Bytes::copy_from_slice(slice))
}

fn database_upload_max_size_bytes(policy_max_size_bytes: i64) -> i64 {
    policy_max_size_bytes
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
    config: DatabaseFileStorageCompressionConfig,
    data: Vec<u8>,
) -> Result<(FileBlobCompression, Vec<u8>)> {
    if config.algorithm == FileBlobCompression::None
        || payload_len_i64(data.len())? < config.min_size_bytes.max(0)
    {
        return Ok((FileBlobCompression::None, data));
    }
    let original_len = data.len();
    let algorithm = config.algorithm;
    let (data, compressed) = match algorithm {
        FileBlobCompression::None => return Ok((FileBlobCompression::None, data)),
        FileBlobCompression::Lz4 => tokio::task::spawn_blocking(move || {
            let compressed = lz4_flex::compress_prepend_size(&data);
            (data, compressed)
        })
        .await
        .map_err(|error| Error::Internal(format!("file compression task failed: {error}")))?,
        FileBlobCompression::Zstd => tokio::task::spawn_blocking(move || {
            let compressed = zstd::bulk::compress(&data, 0)?;
            Ok::<_, std::io::Error>((data, compressed))
        })
        .await
        .map_err(|error| Error::Internal(format!("file compression task failed: {error}")))?
        .map_err(|error| Error::Internal(format!("file compression failed: {error}")))?,
    };
    if compressed.len() >= original_len {
        return Ok((FileBlobCompression::None, data));
    }
    let saved = original_len - compressed.len();
    let min_saved = original_len.saturating_mul(usize::from(config.min_savings_percent)) / 100;
    if saved < min_saved {
        return Ok((FileBlobCompression::None, data));
    }
    Ok((algorithm, compressed))
}

async fn decompress_blob_part(part: FileBlobPart) -> Result<DecompressedBlobPart> {
    let expected_size = usize::try_from(part.size_bytes)
        .map_err(|_| Error::Internal("file blob part size is invalid".to_string()))?;
    let data = match part.compression {
        FileBlobCompression::None => part.data,
        FileBlobCompression::Lz4 => {
            tokio::task::spawn_blocking(move || lz4_flex::decompress_size_prepended(&part.data))
                .await
                .map_err(|error| {
                    Error::Internal(format!("file decompression task failed: {error}"))
                })?
                .map_err(|error| Error::Internal(format!("file decompression failed: {error}")))?
        }
        FileBlobCompression::Zstd => {
            tokio::task::spawn_blocking(move || zstd::bulk::decompress(&part.data, expected_size))
                .await
                .map_err(|error| {
                    Error::Internal(format!("file decompression task failed: {error}"))
                })?
                .map_err(|error| Error::Internal(format!("file decompression failed: {error}")))?
        }
    };
    if payload_len_i64(data.len())? != part.size_bytes {
        return Err(Error::Internal(
            "file blob part decompressed size mismatch".to_string(),
        ));
    }
    let actual_checksum = hex::encode(Sha256::digest(&data));
    if actual_checksum != part.checksum_sha256 {
        return Err(Error::Internal(
            "file blob part checksum mismatch".to_string(),
        ));
    }
    Ok(DecompressedBlobPart {
        offset_bytes: part.offset_bytes,
        size_bytes: part.size_bytes,
        data,
    })
}

fn ensure_session_open(session: &crate::models::FileUploadSessionRecord) -> Result<()> {
    if session.completed_at.is_some() || session.expires_at <= Utc::now() {
        return Err(Error::InvalidInput(
            "file upload session is not active".to_string(),
        ));
    }
    Ok(())
}

#[async_trait::async_trait]
impl FileStorageService for DatabaseFileStorageService {
    fn backend_name(&self) -> &str {
        &self.storage_backend
    }

    fn repository(&self) -> Option<Arc<FileStorageRepository>> {
        Some(self.repository.clone())
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
    ) -> Result<FileUploadSessionCreateResult> {
        validate_create_file_upload_session(&request)?;
        let max_size_bytes = validate_database_upload_session_size(&request)?;
        if request.parts.is_empty() {
            return Ok(FileUploadSessionCreateResult::Plan(
                super::create_file_upload_plan(request.size_bytes, max_size_bytes)?,
            ));
        }
        let (plan, content_manifest_sha256) =
            validated_upload_manifest(request.size_bytes, max_size_bytes, &request.parts)?;
        let public_file_id = new_public_file_id();
        let session_metadata = upload_session_metadata(UploadSessionMetadataInput {
            public_file_id: &public_file_id,
            user_id: request.user_id,
            storage_scope: &request.storage_scope,
            client_file_id: request.client_file_id.as_deref(),
            filename: request.filename.as_deref(),
            width: request.width,
            height: request.height,
            metadata: request.metadata.clone(),
            upload_policy: &request.policy,
        });
        let expires_at = Utc::now() + chrono::Duration::seconds(FILE_UPLOAD_EXPIRES_SECONDS);
        if let Some(existing) = self
            .repository
            .get_object_by_manifest(
                &self.storage_backend,
                &content_manifest_sha256,
                request.size_bytes,
            )
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
                id: public_file_id,
                filename: request.filename.clone(),
                storage_backend: self.storage_backend.clone(),
                object_key: existing.object_key.clone(),
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
                &self.upload_token_secret,
                &content_manifest_sha256,
                request.size_bytes,
            )?;
            validate_stored_files(std::slice::from_ref(&file))?;
            let mut reference_metadata = session_metadata.clone();
            let object = reference_metadata.as_object_mut().ok_or_else(|| {
                Error::InvalidInput("file upload session metadata is invalid".to_string())
            })?;
            object.insert(
                "ownership_proof_required".to_string(),
                serde_json::Value::Bool(true),
            );
            object.insert(
                "ownership_proof_nonce".to_string(),
                serde_json::Value::String(nonce.clone()),
            );
            object.insert(
                "ownership_proof_ranges".to_string(),
                serde_json::to_value(&ranges)?,
            );
            register_upload_session_reference(
                &self.repository,
                &self.storage_backend,
                &file.object_key,
                &file.id,
                expires_at,
                &reference_metadata,
            )
            .await?;
            return Ok(FileUploadSessionCreateResult::Session(FileUploadSession {
                file,
                encoded_object_key: encode_database_file_object_key(&existing.object_key),
                upload_required: false,
                ownership_proof_required: true,
                ownership_proof_nonce: Some(nonce),
                ownership_proof_ranges: ranges,
                upload_url: None,
                upload_method: None,
                upload_headers: Default::default(),
                expires_at: Some(expires_at),
                max_size_bytes,
                resumable: true,
                part_size_bytes: plan.part_size_bytes,
                uploaded_size_bytes: 0,
                uploaded_parts: Vec::new(),
                upload_id: None,
                part_urls: Vec::new(),
            }));
        }

        let existing_session = self
            .repository
            .get_pending_upload_session_by_manifest(
                &self.storage_backend,
                request.user_id,
                &request.storage_scope,
                &content_manifest_sha256,
                request.size_bytes,
            )
            .await?;
        let object_key = file_object_key(
            &database_file_namespace_base_path(&request.policy.storage_namespace),
            "manifest",
            &content_manifest_sha256,
            &request.mime_type,
        );
        let upload_session_key = if let Some(existing) = existing_session.as_ref() {
            existing.upload_session_key.clone()
        } else {
            file_object_key(
                &database_file_namespace_base_path(&request.policy.storage_namespace),
                "sessions",
                &public_file_id,
                &request.mime_type,
            )
        };
        let public_file_id = if let Some(existing) = existing_session.as_ref() {
            upload_session_public_file_id(&existing.metadata)?
        } else {
            public_file_id
        };
        let session_metadata = upload_session_metadata(UploadSessionMetadataInput {
            public_file_id: &public_file_id,
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
            upload_session_metadata_with_manifest(&session_metadata, &request.parts)?;

        let mut file = NewStoredFile {
            id: public_file_id,
            filename: request.filename,
            storage_backend: self.storage_backend.clone(),
            object_key: object_key.clone(),
            url: None,
            mime_type: Some(request.mime_type),
            size_bytes: Some(request.size_bytes),
            width: request.width,
            height: request.height,
            metadata: request.metadata,
        };
        let upload_token = file_upload_token_for_object_key(
            &file,
            &upload_session_key,
            request.user_id,
            &request.storage_scope,
            expires_at,
            &self.upload_token_secret,
            Some(&content_manifest_sha256),
        )?;
        file.metadata
            .as_object_mut()
            .ok_or_else(|| Error::InvalidInput("file metadata must be a JSON object".to_string()))?
            .insert(
                FILE_UPLOAD_TOKEN_KEY.to_string(),
                serde_json::Value::String(upload_token),
            );
        file.url = Some(database_file_object_url(
            &request.policy.database_object_route_prefix,
            &self.storage_backend,
            &file.object_key,
            &self.upload_token_secret,
        )?);
        validate_stored_files(std::slice::from_ref(&file))?;
        self.repository
            .upsert_pending_object(UpsertFileObject {
                storage_backend: &self.storage_backend,
                object_key: &file.object_key,
                mime_type: file
                    .mime_type
                    .as_deref()
                    .ok_or_else(|| Error::InvalidInput("file mime_type is required".to_string()))?,
                size_bytes: request.size_bytes,
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &upload_manifest_metadata(&request.parts)?,
            })
            .await?;
        self.repository
            .upsert_pending_object(UpsertFileObject {
                storage_backend: &self.storage_backend,
                object_key: &upload_session_key,
                mime_type: file
                    .mime_type
                    .as_deref()
                    .ok_or_else(|| Error::InvalidInput("file mime_type is required".to_string()))?,
                size_bytes: request.size_bytes,
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &session_metadata,
            })
            .await?;
        self.repository
            .upsert_upload_session(UpsertFileUploadSession {
                storage_backend: &self.storage_backend,
                upload_session_key: &upload_session_key,
                object_key: &file.object_key,
                session_kind: FileUploadSessionKind::DatabaseMultipart,
                upload_id: None,
                user_id: request.user_id,
                storage_scope: &request.storage_scope,
                mime_type: file
                    .mime_type
                    .as_deref()
                    .ok_or_else(|| Error::InvalidInput("file mime_type is required".to_string()))?,
                size_bytes: request.size_bytes,
                content_manifest_sha256: &content_manifest_sha256,
                part_size_bytes: plan.part_size_bytes,
                metadata: &session_metadata,
                expires_at,
            })
            .await?;
        register_upload_session_reference(
            &self.repository,
            &self.storage_backend,
            &file.object_key,
            &file.id,
            expires_at,
            &session_metadata,
        )
        .await?;
        let (uploaded_size_bytes, uploaded_parts) =
            upload_session_progress(&self.repository, &self.storage_backend, &upload_session_key)
                .await?;
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
            &upload_session_key,
            &self.upload_token_secret,
        )?;
        Ok(FileUploadSessionCreateResult::Session(FileUploadSession {
            file,
            encoded_object_key: encode_database_file_object_key(&upload_session_key),
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            upload_url: Some(object_url),
            upload_method: Some("PUT".to_string()),
            upload_headers,
            expires_at: Some(expires_at),
            max_size_bytes,
            resumable: true,
            part_size_bytes: plan.part_size_bytes,
            uploaded_size_bytes,
            uploaded_parts,
            upload_id: None,
            part_urls: Vec::new(),
        }))
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
        attach_prepared_file_urls(self, &mut files, context.database_object_route_prefix)?;
        attach_variants_to_files(
            self,
            self.repository.as_ref(),
            &mut files,
            context.database_object_route_prefix,
        )
        .await?;
        strip_internal_file_metadata(&mut files);
        Ok(files)
    }

    fn create_reuse_grant(&self, request: CreateFileReuseGrant<'_>) -> Result<FileReuseGrant> {
        file_reuse_grant(&request, &self.upload_token_secret)
    }

    async fn validate_reuse_grant(
        &self,
        token: &str,
        context: FileStorageContext<'_>,
    ) -> Result<ValidatedFileReuseGrant> {
        validate_file_reuse_grant(token, context, Utc::now(), &self.upload_token_secret)
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
            let delete_claimed = self
                .repository
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
            let derived_variants = self
                .repository
                .list_derived_object_variants(&file.storage_backend, &file.object_key)
                .await?;
            let derived_objects = derived_variants
                .into_iter()
                .map(|variant| (variant.storage_backend, variant.object_key))
                .collect::<Vec<_>>();
            for (storage_backend, object_key) in &derived_objects {
                if let Err(error) = self
                    .repository
                    .delete_blob(storage_backend, object_key)
                    .await
                {
                    crate::metrics::file_storage::FILE_OBJECT_DELETE_FAILURES
                        .with_label_values(&[origin_label, &file.storage_backend])
                        .inc();
                    return Err(error);
                }
            }
            for (storage_backend, object_key) in derived_objects {
                self.repository
                    .delete_object(&storage_backend, &object_key)
                    .await?;
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

    async fn cleanup_expired_upload_session(
        &self,
        session: crate::models::FileUploadSessionRecord,
    ) -> Result<bool> {
        if session.storage_backend != self.storage_backend {
            return Ok(false);
        }
        if session.completed_at.is_some() || session.expires_at > Utc::now() {
            return Ok(false);
        }
        self.repository
            .delete_blob_parts(&self.storage_backend, &session.upload_session_key)
            .await?;
        self.repository
            .delete_upload_session_parts(&self.storage_backend, &session.upload_session_key)
            .await?;
        self.repository
            .delete_object(&self.storage_backend, &session.upload_session_key)
            .await?;
        let (_, reference_id) = super::upload_session_reference_target(
            session
                .metadata
                .get(super::FILE_SESSION_METADATA_PUBLIC_ID_KEY)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        );
        if !reference_id.is_empty() {
            self.repository
                .release_reference(
                    super::FILE_UPLOAD_SESSION_REFERENCE_KIND,
                    &reference_id,
                    &self.storage_backend,
                    &session.object_key,
                )
                .await?;
        }
        let session_deleted = self
            .repository
            .delete_upload_session(&self.storage_backend, &session.upload_session_key)
            .await?;
        if !self
            .repository
            .object_validated(&self.storage_backend, &session.object_key)
            .await?
            && self
                .repository
                .object_reference_count_excluding_kind(
                    &self.storage_backend,
                    &session.object_key,
                    super::FILE_UPLOAD_SESSION_REFERENCE_KIND,
                )
                .await?
                == 0
        {
            self.repository
                .delete_object(&self.storage_backend, &session.object_key)
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
        if data.is_empty() || data.len() > MAX_DATABASE_FILE_UPLOAD_PART_SIZE_BYTES {
            return Err(Error::InvalidInput(format!(
                "file upload part must be between 1 and {MAX_DATABASE_FILE_UPLOAD_PART_SIZE_BYTES} bytes"
            )));
        }
        let upload_session_key = decode_database_file_object_key(&encoded_object_key)?;
        let payload = validate_database_file_upload_token(
            &self.storage_backend,
            &upload_token,
            &upload_session_key,
            Utc::now(),
            &self.upload_token_secret,
        )?;
        let session = self
            .repository
            .get_upload_session(&self.storage_backend, &upload_session_key)
            .await?
            .ok_or_else(|| Error::InvalidInput("file upload session was not found".to_string()))?;
        ensure_session_open(&session)?;
        let mime_type = payload
            .get("mime_type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
        if let Some(content_type) = content_type.as_deref() {
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
        let actual_part_checksum = hex::encode(Sha256::digest(&data));
        let range = range.unwrap_or(crate::models::FileUploadRange {
            start: 0,
            end_inclusive: payload_len_i64(data.len())? - 1,
            total_size: expected_size,
        });
        // Validate against the persisted session plan. The global default part
        // size can change over time, while an open upload session keeps its own
        // `part_size_bytes` and manifest for resume/idempotency.
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
        if expected_part.offset_bytes != range.start
            || expected_part.size_bytes != payload_len_i64(data.len())?
            || expected_part.checksum_sha256 != actual_part_checksum
        {
            return Err(Error::InvalidInput(
                "file upload part does not match manifest".to_string(),
            ));
        }
        if let Some(existing) = self
            .repository
            .list_upload_session_parts(&self.storage_backend, &upload_session_key)
            .await?
            .into_iter()
            .find(|part| part.part_index == part_index)
        {
            if existing.offset_bytes != range.start
                || existing.size_bytes != payload_len_i64(data.len())?
                || existing.checksum_sha256.as_deref() != Some(actual_part_checksum.as_str())
            {
                return Err(Error::InvalidInput(
                    "file upload part conflicts with an existing part".to_string(),
                ));
            }
            let parts = self
                .repository
                .list_upload_session_parts(&self.storage_backend, &upload_session_key)
                .await?;
            let (uploaded_size_bytes, uploaded_parts) = upload_session_parts_progress(&parts)?;
            return Ok(StoreFileUploadResult::PartAccepted {
                uploaded_size_bytes,
                uploaded_parts,
            });
        }
        let part_size = payload_len_i64(data.len())?;
        let (compression, stored_data) = compress_payload(self.compression, data).await?;
        self.repository
            .upsert_blob_part(UpsertFileBlobPart {
                storage_backend: &self.storage_backend,
                object_key: &upload_session_key,
                part_index,
                offset_bytes: range.start,
                size_bytes: part_size,
                checksum_sha256: &actual_part_checksum,
                compression,
                data: stored_data,
            })
            .await?;
        self.repository
            .upsert_upload_session_part(UpsertFileUploadSessionPart {
                storage_backend: &self.storage_backend,
                upload_session_key: &upload_session_key,
                part_index,
                part_number,
                offset_bytes: range.start,
                size_bytes: part_size,
                checksum_sha256: Some(&actual_part_checksum),
                etag: None,
            })
            .await?;
        let parts = self
            .repository
            .list_upload_session_parts(&self.storage_backend, &upload_session_key)
            .await?;
        let (uploaded_size_bytes, uploaded_parts) = upload_session_parts_progress(&parts)?;
        let uploaded_complete = uploaded_size_bytes == expected_size;
        if !uploaded_complete {
            return Ok(StoreFileUploadResult::PartAccepted {
                uploaded_size_bytes,
                uploaded_parts,
            });
        }
        let blob = self
            .finalize_upload_object(
                &session,
                mime_type,
                expected_size,
                &session.content_manifest_sha256,
                session.part_size_bytes,
            )
            .await?;
        Ok(StoreFileUploadResult::Complete(blob))
    }

    async fn complete_upload_session(
        &self,
        request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        let decoded_object_key = decode_database_file_object_key(&request.encoded_object_key)?;
        let payload = validate_database_file_upload_token(
            &self.storage_backend,
            &request.upload_token,
            &decoded_object_key,
            Utc::now(),
            &self.upload_token_secret,
        )?;
        if let Some(file_id) = request
            .file_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            let (reference_kind, reference_id) =
                crate::service::file_storage::upload_session_reference_target(file_id);
            let reference_session = self
                .repository
                .get_upload_session_by_reference(reference_kind, &reference_id)
                .await?
                .ok_or_else(|| Error::InvalidInput("file reference was not found".to_string()))?;
            if reference_session.storage_backend != self.storage_backend
                || (reference_session.object_key != decoded_object_key
                    && reference_session.upload_session_key != decoded_object_key)
            {
                return Err(Error::InvalidInput(
                    "file reference does not match upload session".to_string(),
                ));
            }
            if optional_payload_bool(
                &reference_session.metadata,
                "ownership_proof_required",
                "file upload session metadata",
            )? {
                if reference_session.expires_at <= Utc::now() {
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
                let nonce = reference_session
                    .metadata
                    .get("ownership_proof_nonce")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        Error::InvalidInput("invalid file upload session metadata".to_string())
                    })?;
                let ranges = ownership_proof_ranges_from_payload(&reference_session.metadata)?;
                let mut chunks = Vec::with_capacity(ranges.len());
                for range in &ranges {
                    let data = self
                        .load_range_data(
                            &reference_session.object_key,
                            Some(FileRangeRequest::Exact(FileByteRange {
                                start: range.offset,
                                end_inclusive: range.offset + i64::from(range.length) - 1,
                            })),
                        )
                        .await?;
                    chunks.push(data);
                }
                let expected = file_ownership_proof_digest(
                    nonce,
                    &ranges,
                    &reference_session.content_manifest_sha256,
                    reference_session.size_bytes,
                    chunks.iter().map(Vec::as_slice),
                );
                if !constant_time_eq(proof.as_bytes(), expected.as_bytes()) {
                    return Err(Error::InvalidInput(
                        "file ownership proof does not match object".to_string(),
                    ));
                }
                let metadata =
                    mark_upload_session_ownership_proof_verified(&reference_session.metadata)?;
                self.repository
                    .update_reference_metadata(
                        reference_kind,
                        &reference_id,
                        &self.storage_backend,
                        &reference_session.object_key,
                        &metadata,
                    )
                    .await?;
                return Ok(CompleteFileUploadSessionResult {
                    object: Some(FileBlob {
                        storage_backend: self.storage_backend.clone(),
                        object_key: reference_session.object_key,
                        mime_type: reference_session.mime_type,
                        size_bytes: reference_session.size_bytes,
                        total_size_bytes: reference_session.size_bytes,
                        content_manifest_sha256: reference_session.content_manifest_sha256,
                        compression: FileBlobCompression::None,
                        range: None,
                        data: Vec::new(),
                        metadata: serde_json::Value::Object(Default::default()),
                        created_at: Utc::now(),
                    }),
                    uploaded_size_bytes: reference_session.size_bytes,
                    uploaded_parts: Vec::new(),
                });
            }
        }
        let session = self
            .repository
            .get_upload_session(&self.storage_backend, &decoded_object_key)
            .await?
            .ok_or_else(|| Error::InvalidInput("file upload session was not found".to_string()))?;
        ensure_session_open(&session)?;
        let parts = self
            .repository
            .list_upload_session_parts(&self.storage_backend, &decoded_object_key)
            .await?;
        let (uploaded_size_bytes, uploaded_parts) = upload_session_parts_progress(&parts)?;
        if uploaded_size_bytes != session.size_bytes {
            return Ok(CompleteFileUploadSessionResult {
                object: None,
                uploaded_size_bytes,
                uploaded_parts,
            });
        }
        let _ = payload;
        let blob = self
            .finalize_upload_object(
                &session,
                &session.mime_type,
                session.size_bytes,
                &session.content_manifest_sha256,
                session.part_size_bytes,
            )
            .await?;
        Ok(CompleteFileUploadSessionResult {
            object: Some(blob),
            uploaded_size_bytes: session.size_bytes,
            uploaded_parts,
        })
    }

    async fn get_object(&self, request: GetFileObject) -> Result<FileBlob> {
        collect_file_object_download(self.object_download(request).await?).await
    }

    async fn get_object_stream(&self, request: GetFileObject) -> Result<FileObjectDownload> {
        self.object_download(request).await
    }

    async fn get_object_by_key(&self, storage_backend: &str, object_key: &str) -> Result<FileBlob> {
        if storage_backend != self.storage_backend {
            return Err(Error::InvalidInput(format!(
                "file storage_backend must be {}",
                self.storage_backend
            )));
        }
        let read_token = super::database_file_read_token(
            &self.storage_backend,
            object_key,
            &self.upload_token_secret,
        )?;
        self.get_object(GetFileObject {
            encoded_object_key: encode_database_file_object_key(object_key),
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
        if storage_backend != self.storage_backend {
            return Err(Error::InvalidInput(format!(
                "file storage_backend must be {}",
                self.storage_backend
            )));
        }
        let Some(object) = self
            .repository
            .get_object(&self.storage_backend, object_key)
            .await?
        else {
            return Err(Error::NotFound("File object not found".to_string()));
        };
        let repository = Arc::clone(&self.repository);
        let storage_backend = self.storage_backend.clone();
        let object_key = object_key.to_string();
        let chunk_size = usize::try_from(object.size_bytes.min(1024 * 1024)).unwrap_or(1024 * 1024);
        let reader = super::read_seek::RangeSeekReader::new(
            object.size_bytes,
            chunk_size,
            move |offset, length| {
                let repository = Arc::clone(&repository);
                let storage_backend = storage_backend.clone();
                let object_key = object_key.clone();
                Box::pin(async move {
                    let length_i64 = i64::try_from(length).map_err(|_| {
                        Error::Internal("file reader length is invalid".to_string())
                    })?;
                    let end_inclusive = offset
                        .checked_add(length_i64)
                        .and_then(|end| end.checked_sub(1))
                        .ok_or_else(|| Error::Internal("file reader range overflow".to_string()))?;
                    let range = FileByteRange {
                        start: offset,
                        end_inclusive,
                    };
                    let parts = repository
                        .list_blob_parts_overlapping_range(
                            &storage_backend,
                            &object_key,
                            range.start,
                            range.end_inclusive,
                        )
                        .await?;
                    ensure_database_parts_cover_range(&parts, range)?;
                    let mut out = bytes::BytesMut::with_capacity(length);
                    for part in parts {
                        let chunk = database_part_chunk(part, range).await?;
                        out.extend_from_slice(&chunk);
                    }
                    Ok(out.freeze())
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
        metadata: serde_json::Value,
    ) -> Result<FileBlob> {
        if storage_backend != self.storage_backend {
            return Err(Error::InvalidInput(format!(
                "file storage_backend must be {}",
                self.storage_backend
            )));
        }
        if data.is_empty() {
            return Err(Error::InvalidInput(
                "file object payload must be non-empty".to_string(),
            ));
        }
        let size_bytes = payload_len_i64(data.len())?;
        let checksum = hex::encode(Sha256::digest(&data));
        self.repository
            .upsert_object(UpsertFileObject {
                storage_backend: &self.storage_backend,
                object_key,
                mime_type,
                size_bytes,
                content_manifest_sha256: &checksum,
                metadata: &metadata,
            })
            .await?;
        self.repository
            .upsert_blob_part(UpsertFileBlobPart {
                storage_backend: &self.storage_backend,
                object_key,
                part_index: 0,
                offset_bytes: 0,
                size_bytes,
                checksum_sha256: &checksum,
                compression: FileBlobCompression::None,
                data: data.clone(),
            })
            .await?;
        Ok(FileBlob {
            storage_backend: self.storage_backend.clone(),
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
        database_object_route_prefix: &str,
        upload_policy: &crate::models::FileUploadPolicy,
    ) -> Result<Vec<crate::models::FileObjectVariant>> {
        if storage_backend != self.storage_backend {
            return Ok(Vec::new());
        }
        process_file_variants_for_object(
            self,
            self.repository.clone(),
            storage_backend,
            object_key,
            database_object_route_prefix,
            upload_policy,
        )
        .await
        .map(|result| result.variants)
    }
}
