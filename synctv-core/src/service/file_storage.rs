use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    models::{
        CompleteFileUploadSession, CompleteFileUploadSessionResult, CreateFileUploadSession,
        FileBlob, FileBlobCompression, FileByteRange, FileObjectDownload, FileObjectMetadata,
        FileOwnershipProofRange, FileRangeRequest, FileReferenceTarget, FileUploadManifestPart,
        FileUploadPlan, FileUploadPlanPart, FileUploadRange, FileUploadSessionCreateResult,
        GetFileObject, NewStoredFile, StoreFileUpload, StoreFileUploadResult,
        SubmittedFileReference, SubmittedFileReferenceKind, UserId,
    },
    repository::FileStorageRepository,
    Error, Result,
};

mod database;
mod routing;
mod s3;
mod validation;
pub use routing::{FileStorageBackendRegistry, RoutedFileStorageService};
#[cfg(test)]
use s3::presigned_upload_headers;
pub use s3::{S3CompatibleFileStorageService, S3FileStorageConfig};
pub(crate) use validation::validate_create_file_upload_session;
use validation::{
    strip_internal_file_metadata, validate_file_mime_type, validate_s3_file_storage_config,
    validate_stored_files,
};

pub(super) const FILE_UPLOAD_EXPIRES_SECONDS: i64 = 900;
pub(crate) const FILE_UPLOAD_TOKEN_KEY: &str = "_synctv_upload_token";
pub(crate) const FILE_OWNERSHIP_PROOF_KEY: &str = "_synctv_ownership_proof";
const FILE_SESSION_METADATA_PUBLIC_ID_KEY: &str = "public_file_id";
const FILE_SESSION_METADATA_CLIENT_ID_KEY: &str = "client_file_id";
const FILE_SESSION_METADATA_USER_ID_KEY: &str = "user_id";
const FILE_SESSION_METADATA_STORAGE_SCOPE_KEY: &str = "storage_scope";
const FILE_SESSION_METADATA_FILENAME_KEY: &str = "filename";
const FILE_SESSION_METADATA_WIDTH_KEY: &str = "width";
const FILE_SESSION_METADATA_HEIGHT_KEY: &str = "height";
const FILE_SESSION_METADATA_USER_METADATA_KEY: &str = "metadata";
const FILE_SESSION_METADATA_MANIFEST_PARTS_KEY: &str = "manifest_parts";
const FILE_SESSION_METADATA_OWNERSHIP_PROOF_VERIFIED_KEY: &str = "ownership_proof_verified";
const FILE_OWNERSHIP_PROOF_ALGORITHM: &str = "synctv-file-ownership-proof-v1";
const FILE_OWNERSHIP_PROOF_RANGE_COUNT: usize = 3;
const FILE_OWNERSHIP_PROOF_RANGE_BYTES: i32 = 1024;
const FILE_UPLOAD_SESSION_REFERENCE_KIND: &str = "file_upload_session";
const FILE_UPLOAD_TOKEN_VERSION: &str = "v1";
const FILE_REUSE_TOKEN_VERSION: &str = "v1";
const FILE_REUSE_TOKEN_KIND: &str = "synctv-file-reuse-grant";
pub const FILE_UPLOAD_TOKEN_HEADER: &str = "x-synctv-file-upload-token";
const DATABASE_FILE_READ_TOKEN_VERSION: &str = "v1";
pub(super) const MAX_DATABASE_FILE_UPLOAD_PART_SIZE_BYTES: usize = 64 * 1024 * 1024;
pub(super) const DEFAULT_RESUMABLE_UPLOAD_PART_SIZE_BYTES: i64 = 8 * 1024 * 1024;
pub(super) const FILE_UPLOAD_CHECKSUM_ALGORITHM_SHA256: &str = "sha256";

// File storage contract:
// - file_objects is the registry; validated_at marks the final usable object.
// - file_blob_parts stores database-backend bytes for final objects and
//   pending database multipart sessions.
// - file_upload_sessions and file_upload_session_parts track resumable state.
// - upload parts are hashed while they are written; finalize recomputes the
//   manifest digest from stored part checksums.
// - database multipart finalization promotes blob-part rows with UPDATE, so
//   the same table stores pending and final parts through different object keys.
// - part_size_bytes is planned per session and persisted; validators must read
//   the session plan so changing the default only affects new sessions.
// - S3 upload sessions default to direct client-to-S3 presigned URLs; SyncTV
//   proxy upload/download paths remain available as a deployment fallback.
// - downloads return FileObjectDownload streams; HTTP wraps that as a normal
//   binary body and gRPC wraps it as protobuf chunks.
// See docs/src/content/docs/en/develop/implementation-contracts.mdx.

pub(super) fn payload_len_i64(len: usize) -> Result<i64> {
    i64::try_from(len)
        .map_err(|_| Error::InvalidInput("file payload size exceeds i64::MAX".to_string()))
}

pub(super) fn file_blob_to_download(blob: FileBlob) -> FileObjectDownload {
    let metadata = FileObjectMetadata {
        storage_backend: blob.storage_backend,
        object_key: blob.object_key,
        mime_type: blob.mime_type,
        size_bytes: blob.size_bytes,
        total_size_bytes: blob.total_size_bytes,
        content_manifest_sha256: blob.content_manifest_sha256,
        compression: blob.compression,
        range: blob.range,
        metadata: blob.metadata,
        created_at: blob.created_at,
    };
    let data = bytes::Bytes::from(blob.data);
    FileObjectDownload {
        metadata,
        stream: futures::stream::once(async move { Ok(data) }).boxed(),
    }
}

pub(super) async fn collect_file_object_download(
    mut download: FileObjectDownload,
) -> Result<FileBlob> {
    let capacity = usize::try_from(download.metadata.size_bytes).unwrap_or_default();
    let mut data = Vec::with_capacity(capacity);
    while let Some(chunk) = download.stream.try_next().await? {
        data.extend_from_slice(&chunk);
    }
    if payload_len_i64(data.len())? != download.metadata.size_bytes {
        return Err(Error::Internal(
            "file object stream returned an unexpected size".to_string(),
        ));
    }
    let mut blob = download.metadata.empty_blob();
    blob.data = data;
    Ok(blob)
}

pub(super) fn upload_session_part_size(_max_size_bytes: i64) -> i64 {
    // This is the server plan for new sessions. Existing sessions validate
    // against their stored part_size_bytes and manifest, so changing the
    // default must remain compatible with open sessions.
    DEFAULT_RESUMABLE_UPLOAD_PART_SIZE_BYTES
}

pub(super) fn create_file_upload_plan(
    size_bytes: i64,
    max_size_bytes: i64,
) -> Result<FileUploadPlan> {
    if size_bytes <= 0 || size_bytes > max_size_bytes {
        return Err(Error::InvalidInput(format!(
            "file size must be between 1 and {max_size_bytes} bytes"
        )));
    }
    let part_size_bytes = upload_session_part_size(max_size_bytes);
    let part_count = (size_bytes + part_size_bytes - 1) / part_size_bytes;
    let part_count = i32::try_from(part_count)
        .map_err(|_| Error::InvalidInput("file upload has too many parts".to_string()))?;
    let mut parts = Vec::with_capacity(usize::try_from(part_count).unwrap_or(0));
    for part_number in 1..=part_count {
        let offset_bytes = i64::from(part_number - 1)
            .checked_mul(part_size_bytes)
            .ok_or_else(|| Error::Internal("file upload part offset overflow".to_string()))?;
        let size_bytes = (size_bytes - offset_bytes).min(part_size_bytes);
        parts.push(FileUploadPlanPart {
            part_number,
            offset_bytes,
            size_bytes,
        });
    }
    Ok(FileUploadPlan {
        checksum_algorithm: FILE_UPLOAD_CHECKSUM_ALGORITHM_SHA256.to_string(),
        part_size_bytes,
        parts,
    })
}

pub(super) fn validated_upload_manifest(
    size_bytes: i64,
    max_size_bytes: i64,
    parts: &[FileUploadManifestPart],
) -> Result<(FileUploadPlan, String)> {
    let plan = create_file_upload_plan(size_bytes, max_size_bytes)?;
    if parts.is_empty() {
        return Err(Error::InvalidInput(
            "file upload manifest parts are required".to_string(),
        ));
    }
    if parts.len() != plan.parts.len() {
        return Err(Error::InvalidInput(
            "file upload manifest does not match server upload plan".to_string(),
        ));
    }
    for (expected, actual) in plan.parts.iter().zip(parts) {
        if actual.part_number != expected.part_number
            || actual.offset_bytes != expected.offset_bytes
            || actual.size_bytes != expected.size_bytes
        {
            return Err(Error::InvalidInput(
                "file upload manifest does not match server upload plan".to_string(),
            ));
        }
    }
    let digest = file_part_manifest_digest(
        size_bytes,
        plan.part_size_bytes,
        parts.iter().map(|part| {
            (
                part.part_number,
                part.size_bytes,
                part.checksum_sha256.as_str(),
            )
        }),
    )?;
    Ok((plan, digest))
}

pub(super) fn upload_manifest_metadata(
    parts: &[FileUploadManifestPart],
) -> Result<serde_json::Value> {
    Ok(json!({
        FILE_SESSION_METADATA_MANIFEST_PARTS_KEY: serde_json::to_value(parts)?,
    }))
}

pub(super) fn upload_manifest_parts_from_metadata(
    metadata: &serde_json::Value,
) -> Result<Vec<FileUploadManifestPart>> {
    let parts = metadata
        .get(FILE_SESSION_METADATA_MANIFEST_PARTS_KEY)
        .ok_or_else(|| {
            Error::InvalidInput("file upload session manifest is missing".to_string())
        })?;
    serde_json::from_value(parts.clone()).map_err(|error| {
        Error::InvalidInput(format!("invalid file upload session manifest: {error}"))
    })
}

pub(super) fn resolve_file_range(
    request: Option<FileRangeRequest>,
    total_size_bytes: i64,
) -> Result<Option<FileByteRange>> {
    let Some(request) = request else {
        return Ok(None);
    };
    if total_size_bytes <= 0 {
        return Err(Error::RangeNotSatisfiable {
            total_size: total_size_bytes.max(0),
        });
    }
    let range = match request {
        FileRangeRequest::Exact(range) => {
            if range.start < 0 || range.end_inclusive < range.start {
                return Err(Error::InvalidInput("file range is invalid".to_string()));
            }
            range
        }
        FileRangeRequest::From { start } => {
            if start < 0 {
                return Err(Error::InvalidInput("file range is invalid".to_string()));
            }
            FileByteRange {
                start,
                end_inclusive: total_size_bytes - 1,
            }
        }
        FileRangeRequest::Suffix { length } => {
            if length <= 0 {
                return Err(Error::InvalidInput("file range is invalid".to_string()));
            }
            let size = length.min(total_size_bytes);
            FileByteRange {
                start: total_size_bytes - size,
                end_inclusive: total_size_bytes - 1,
            }
        }
    };
    if range.start >= total_size_bytes || range.end_inclusive >= total_size_bytes {
        return Err(Error::RangeNotSatisfiable {
            total_size: total_size_bytes,
        });
    }
    Ok(Some(range))
}

pub(super) fn validate_upload_range(
    range: FileUploadRange,
    data_len: usize,
    expected_size: i64,
    part_size_bytes: i64,
) -> Result<i32> {
    if range.start < 0
        || range.end_inclusive < range.start
        || range.total_size != expected_size
        || range.end_inclusive >= range.total_size
        || part_size_bytes <= 0
    {
        return Err(Error::InvalidInput(
            "file upload content range is invalid".to_string(),
        ));
    }
    if range.size_bytes() != payload_len_i64(data_len)? {
        return Err(Error::InvalidInput(
            "file upload content range does not match payload size".to_string(),
        ));
    }
    if range.start % part_size_bytes != 0 {
        return Err(Error::InvalidInput(
            "file upload content range must start at a part boundary".to_string(),
        ));
    }
    let final_byte = range.total_size - 1;
    let is_final_part = range.end_inclusive == final_byte;
    if !is_final_part && range.size_bytes() != part_size_bytes {
        return Err(Error::InvalidInput(
            "file upload part size must match the upload session part size".to_string(),
        ));
    }
    i32::try_from(range.start / part_size_bytes)
        .map_err(|_| Error::InvalidInput("file upload part index is too large".to_string()))
}

pub(super) fn upload_session_parts_progress(
    parts: &[crate::models::FileUploadSessionPart],
) -> Result<(i64, Vec<i32>)> {
    let mut uploaded_size = 0_i64;
    let mut uploaded_parts = Vec::with_capacity(parts.len());
    for part in parts {
        uploaded_size = uploaded_size
            .checked_add(part.size_bytes)
            .ok_or_else(|| Error::Internal("file upload part size overflow".to_string()))?;
        uploaded_parts.push(part.part_number);
    }
    uploaded_parts.sort_unstable();
    uploaded_parts.dedup();
    Ok((uploaded_size, uploaded_parts))
}

pub(super) fn new_public_file_id() -> String {
    format!("file_{}", synctv_common::snanoid!(24))
}

pub(super) async fn upload_session_progress(
    repository: &FileStorageRepository,
    storage_backend: &str,
    object_key: &str,
) -> Result<(i64, Vec<i32>)> {
    let parts = repository
        .list_upload_session_parts(storage_backend, object_key)
        .await?;
    upload_session_parts_progress(&parts)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReuseGrant {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CreateFileReuseGrant<'a> {
    pub user_id: UserId,
    pub storage_scope: &'a str,
    pub source_kind: &'a str,
    pub source_id: &'a str,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedFileReuseGrant {
    pub user_id: UserId,
    pub storage_scope: String,
    pub source_kind: String,
    pub source_id: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy)]
pub struct FileStorageContext<'a> {
    pub user_id: UserId,
    pub storage_scope: &'a str,
    pub database_object_route_prefix: &'a str,
    pub client_request_id: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStorageCleanupOrigin {
    ReferenceReleased,
    ReferenceExpired,
    RetentionExpired,
    ReferenceCapExceeded,
    CleanupRetry,
    UnreferencedObject,
}

impl FileStorageCleanupOrigin {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReferenceReleased => "reference_released",
            Self::ReferenceExpired => "reference_expired",
            Self::RetentionExpired => "retention_expired",
            Self::ReferenceCapExceeded => "reference_cap_exceeded",
            Self::CleanupRetry => "cleanup_retry",
            Self::UnreferencedObject => "unreferenced_object",
        }
    }
}

#[async_trait::async_trait]
pub trait FileStorageService: Send + Sync {
    fn backend_name(&self) -> &str;

    fn repository(&self) -> Option<Arc<FileStorageRepository>> {
        None
    }

    fn object_url(
        &self,
        _storage_backend: &str,
        _object_key: &str,
        _database_object_route_prefix: &str,
    ) -> Result<Option<String>> {
        Ok(None)
    }

    async fn create_upload_session(
        &self,
        request: CreateFileUploadSession,
    ) -> Result<FileUploadSessionCreateResult>;

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>>;

    async fn prepare_submitted_files(
        &self,
        context: FileStorageContext<'_>,
        files: Vec<SubmittedFileReference>,
    ) -> Result<Vec<NewStoredFile>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }

        let repository = self.repository().ok_or_else(|| {
            Error::InvalidInput(
                "file upload references are not supported by this storage".to_string(),
            )
        })?;
        let mut prepared = Vec::with_capacity(files.len());
        for reference in files {
            prepared.push(match reference.kind {
                SubmittedFileReferenceKind::Upload => {
                    let id = reference.id.trim();
                    if id.is_empty() {
                        return Err(Error::InvalidInput(
                            "file reference id is required".to_string(),
                        ));
                    }
                    let (reference_kind, reference_id) = upload_session_reference_target(id);
                    let session = repository
                        .get_upload_session_by_reference(reference_kind, &reference_id)
                        .await?
                        .ok_or_else(|| {
                            Error::InvalidInput("file reference was not found".to_string())
                        })?;
                    if session
                        .metadata
                        .get(FILE_SESSION_METADATA_USER_ID_KEY)
                        .and_then(serde_json::Value::as_i64)
                        != Some(context.user_id.as_i64())
                        || session
                            .metadata
                            .get(FILE_SESSION_METADATA_STORAGE_SCOPE_KEY)
                            .and_then(serde_json::Value::as_str)
                            != Some(context.storage_scope)
                    {
                        return Err(Error::InvalidInput(
                            "file reference does not belong to this request".to_string(),
                        ));
                    }
                    let ownership_proof_required = optional_payload_bool(
                        &session.metadata,
                        "ownership_proof_required",
                        "file upload session metadata",
                    )?;
                    if ownership_proof_required {
                        if !upload_session_ownership_proof_verified(&session.metadata)? {
                            return Err(Error::InvalidInput(
                                "file ownership proof has not been verified".to_string(),
                            ));
                        }
                    } else if session.completed_at.is_none() {
                        return Err(Error::InvalidInput(
                            "file upload session has not been completed".to_string(),
                        ));
                    }
                    upload_session_record_to_new_file(&session, id)?
                }
                SubmittedFileReferenceKind::Reuse => {
                    return Err(Error::InvalidInput(
                        "file reuse references must be resolved by the product service".to_string(),
                    ));
                }
            });
        }
        let files = prepared;
        self.prepare_files(context, files).await
    }

    fn create_reuse_grant(&self, _request: CreateFileReuseGrant<'_>) -> Result<FileReuseGrant> {
        Err(Error::InvalidInput(
            "file reuse grants are not supported by this storage".to_string(),
        ))
    }

    async fn validate_reuse_grant(
        &self,
        _token: &str,
        _context: FileStorageContext<'_>,
    ) -> Result<ValidatedFileReuseGrant> {
        Err(Error::InvalidInput(
            "file reuse grants are not supported by this storage".to_string(),
        ))
    }

    async fn delete_files(
        &self,
        _origin: FileStorageCleanupOrigin,
        _files: &[FileReferenceTarget],
    ) -> Result<()> {
        Ok(())
    }

    async fn store_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        data: Vec<u8>,
    ) -> Result<FileBlob> {
        match self
            .store_upload(StoreFileUpload {
                encoded_object_key: encoded_object_key.to_string(),
                upload_token: upload_token.to_string(),
                content_type: content_type.map(str::to_string),
                range: None,
                data,
            })
            .await?
        {
            StoreFileUploadResult::Complete(blob) => Ok(blob),
            StoreFileUploadResult::PartAccepted { .. } => Err(Error::Internal(
                "full file upload returned an incomplete upload result".to_string(),
            )),
        }
    }

    async fn store_upload(&self, _upload: StoreFileUpload) -> Result<StoreFileUploadResult> {
        Err(Error::InvalidInput(
            "file object upload is not supported by this storage backend".to_string(),
        ))
    }

    async fn complete_upload_session(
        &self,
        _request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        Err(Error::InvalidInput(
            "file upload completion is not supported by this storage backend".to_string(),
        ))
    }

    async fn get_object(&self, _request: GetFileObject) -> Result<FileBlob> {
        Err(Error::NotFound("File object not found".to_string()))
    }

    async fn get_object_stream(&self, request: GetFileObject) -> Result<FileObjectDownload> {
        self.get_object(request).await.map(file_blob_to_download)
    }
}

#[derive(Debug, Clone)]
pub struct DisabledFileStorageService;

#[derive(Clone)]
pub struct DatabaseFileStorageService {
    pub(crate) storage_backend: String,
    pub(crate) repository: Arc<FileStorageRepository>,
    pub(crate) upload_token_secret: String,
    pub(crate) compression: DatabaseFileStorageCompressionConfig,
}

#[derive(Debug, Clone, Copy)]
pub struct DatabaseFileStorageCompressionConfig {
    pub algorithm: FileBlobCompression,
    pub min_size_bytes: i64,
    pub min_savings_percent: u8,
}

impl Default for DatabaseFileStorageCompressionConfig {
    fn default() -> Self {
        Self {
            algorithm: FileBlobCompression::Zstd,
            min_size_bytes: 4096,
            min_savings_percent: 10,
        }
    }
}

#[async_trait::async_trait]
impl FileStorageService for DisabledFileStorageService {
    fn backend_name(&self) -> &'static str {
        "disabled"
    }

    async fn create_upload_session(
        &self,
        _request: CreateFileUploadSession,
    ) -> Result<FileUploadSessionCreateResult> {
        Err(Error::InvalidInput("file storage is disabled".to_string()))
    }

    async fn prepare_files(
        &self,
        _context: FileStorageContext<'_>,
        files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        if files.is_empty() {
            Ok(files)
        } else {
            Err(Error::InvalidInput("file storage is disabled".to_string()))
        }
    }
}

pub(super) fn attach_file_ownership_proof_token(
    file: &mut NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
    expires_at: DateTime<Utc>,
    secret: &str,
    content_manifest_sha256: &str,
    size_bytes: i64,
) -> Result<(String, Vec<FileOwnershipProofRange>)> {
    let nonce = synctv_common::snanoid!(32);
    let ranges = file_ownership_proof_ranges(content_manifest_sha256, &nonce, size_bytes)?;
    attach_file_upload_token(
        file,
        user_id,
        storage_scope,
        expires_at,
        secret,
        Some(content_manifest_sha256),
        Some((&nonce, &ranges)),
    )?;
    Ok((nonce, ranges))
}

pub(super) fn attach_file_upload_token(
    file: &mut NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
    expires_at: DateTime<Utc>,
    secret: &str,
    content_manifest_sha256: Option<&str>,
    ownership_proof: Option<(&str, &[FileOwnershipProofRange])>,
) -> Result<()> {
    let token = file_upload_token(
        file,
        user_id,
        storage_scope,
        expires_at,
        secret,
        content_manifest_sha256,
        ownership_proof,
    )?;
    let Some(metadata) = file.metadata.as_object_mut() else {
        return Err(Error::InvalidInput(
            "file metadata must be a JSON object".to_string(),
        ));
    };
    metadata.insert(
        FILE_UPLOAD_TOKEN_KEY.to_string(),
        serde_json::Value::String(token),
    );
    Ok(())
}

pub(super) fn file_upload_token_for_object_key(
    file: &NewStoredFile,
    object_key: &str,
    user_id: UserId,
    storage_scope: &str,
    expires_at: DateTime<Utc>,
    secret: &str,
    content_manifest_sha256: Option<&str>,
) -> Result<String> {
    let mut token_file = file.clone();
    token_file.object_key = object_key.to_string();
    file_upload_token(
        &token_file,
        user_id,
        storage_scope,
        expires_at,
        secret,
        content_manifest_sha256,
        None,
    )
}

pub(super) fn attach_prepared_file_urls(
    storage: &dyn FileStorageService,
    files: &mut [NewStoredFile],
    database_object_route_prefix: &str,
) -> Result<()> {
    for file in files {
        file.url = storage.object_url(
            &file.storage_backend,
            &file.object_key,
            database_object_route_prefix,
        )?;
    }
    Ok(())
}

fn file_upload_token(
    file: &NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
    expires_at: DateTime<Utc>,
    secret: &str,
    content_manifest_sha256: Option<&str>,
    ownership_proof: Option<(&str, &[FileOwnershipProofRange])>,
) -> Result<String> {
    let payload = file_upload_token_payload(
        file,
        user_id,
        storage_scope,
        expires_at,
        content_manifest_sha256,
        ownership_proof,
    );
    let payload_bytes = serde_json::to_vec(&payload)?;
    let signature = hex::encode(hmac_sha256(
        file_upload_token_key(user_id, storage_scope, secret).as_bytes(),
        &payload_bytes,
    )?);
    let encoded_payload = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        payload_bytes,
    );
    Ok(format!(
        "{FILE_UPLOAD_TOKEN_VERSION}.{encoded_payload}.{signature}"
    ))
}

fn file_upload_token_payload(
    file: &NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
    expires_at: DateTime<Utc>,
    content_manifest_sha256: Option<&str>,
    ownership_proof: Option<(&str, &[FileOwnershipProofRange])>,
) -> serde_json::Value {
    let mut payload = json!({
        "user_id": user_id.as_i64(),
        "storage_scope": storage_scope,
        "file_id": file.id,
        "filename": file.filename,
        "storage_backend": file.storage_backend,
        "object_key": file.object_key,
        "mime_type": file.mime_type,
        "size_bytes": file.size_bytes,
        "width": file.width,
        "height": file.height,
        "metadata": public_file_metadata(file),
        "expires_at": expires_at.timestamp(),
    });
    if let Some(content_manifest_sha256) = content_manifest_sha256 {
        payload["content_manifest_sha256"] =
            serde_json::Value::String(content_manifest_sha256.to_ascii_lowercase());
    }
    if let Some((nonce, ranges)) = ownership_proof {
        payload["ownership_proof_required"] = serde_json::Value::Bool(true);
        payload["ownership_proof_algorithm"] =
            serde_json::Value::String(FILE_OWNERSHIP_PROOF_ALGORITHM.to_string());
        payload["ownership_proof_nonce"] = serde_json::Value::String(nonce.to_string());
        payload["ownership_proof_ranges"] = ownership_proof_ranges_to_json(ranges);
    }
    payload
}

fn public_file_metadata(file: &NewStoredFile) -> serde_json::Value {
    let mut metadata = file.metadata.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.remove(FILE_UPLOAD_TOKEN_KEY);
        object.remove(FILE_OWNERSHIP_PROOF_KEY);
    }
    metadata
}

pub(super) struct UploadSessionMetadataInput<'a> {
    pub public_file_id: &'a str,
    pub user_id: UserId,
    pub storage_scope: &'a str,
    pub client_file_id: Option<&'a str>,
    pub filename: Option<&'a str>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub metadata: serde_json::Value,
}

pub(super) fn upload_session_metadata(input: UploadSessionMetadataInput<'_>) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        FILE_SESSION_METADATA_PUBLIC_ID_KEY.to_string(),
        serde_json::Value::String(input.public_file_id.to_string()),
    );
    object.insert(
        FILE_SESSION_METADATA_USER_ID_KEY.to_string(),
        serde_json::json!(input.user_id.as_i64()),
    );
    object.insert(
        FILE_SESSION_METADATA_STORAGE_SCOPE_KEY.to_string(),
        serde_json::Value::String(input.storage_scope.to_string()),
    );
    if let Some(client_file_id) = input
        .client_file_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        object.insert(
            FILE_SESSION_METADATA_CLIENT_ID_KEY.to_string(),
            serde_json::Value::String(client_file_id.to_string()),
        );
    }
    if let Some(filename) = input
        .filename
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        object.insert(
            FILE_SESSION_METADATA_FILENAME_KEY.to_string(),
            serde_json::Value::String(filename.to_string()),
        );
    }
    if let Some(width) = input.width {
        object.insert(
            FILE_SESSION_METADATA_WIDTH_KEY.to_string(),
            serde_json::json!(width),
        );
    }
    if let Some(height) = input.height {
        object.insert(
            FILE_SESSION_METADATA_HEIGHT_KEY.to_string(),
            serde_json::json!(height),
        );
    }
    object.insert(
        FILE_SESSION_METADATA_USER_METADATA_KEY.to_string(),
        input.metadata,
    );
    serde_json::Value::Object(object)
}

pub(super) fn upload_session_metadata_with_manifest(
    metadata: &serde_json::Value,
    parts: &[FileUploadManifestPart],
) -> Result<serde_json::Value> {
    let mut metadata = metadata.clone();
    let Some(target) = metadata.as_object_mut() else {
        return Err(Error::InvalidInput(
            "file upload session metadata is invalid".to_string(),
        ));
    };
    let manifest = upload_manifest_metadata(parts)?;
    let Some(manifest) = manifest.as_object() else {
        return Err(Error::Internal(
            "file upload manifest metadata is invalid".to_string(),
        ));
    };
    target.extend(manifest.clone());
    Ok(metadata)
}

pub(super) fn upload_session_public_file_id(metadata: &serde_json::Value) -> Result<String> {
    required_payload_string(
        metadata,
        FILE_SESSION_METADATA_PUBLIC_ID_KEY,
        "file upload session metadata",
    )
}

struct UploadSessionPublicFields {
    filename: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    metadata: serde_json::Value,
}

fn upload_session_metadata_public_fields(
    metadata: &serde_json::Value,
) -> Result<UploadSessionPublicFields> {
    let filename = optional_payload_string(
        metadata,
        FILE_SESSION_METADATA_FILENAME_KEY,
        "file upload session metadata",
    )?;
    let width = optional_payload_i32(
        metadata,
        FILE_SESSION_METADATA_WIDTH_KEY,
        "file upload session metadata",
    )?;
    let height = optional_payload_i32(
        metadata,
        FILE_SESSION_METADATA_HEIGHT_KEY,
        "file upload session metadata",
    )?;
    let user_metadata = metadata
        .get(FILE_SESSION_METADATA_USER_METADATA_KEY)
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if !user_metadata.is_object() {
        return Err(Error::InvalidInput(
            "file upload session metadata is invalid".to_string(),
        ));
    }
    Ok(UploadSessionPublicFields {
        filename,
        width,
        height,
        metadata: user_metadata,
    })
}

pub(super) fn upload_session_record_to_new_file(
    session: &crate::models::FileUploadSessionRecord,
    file_id: &str,
) -> Result<NewStoredFile> {
    let fields = upload_session_metadata_public_fields(&session.metadata)?;
    Ok(NewStoredFile {
        id: file_id.to_string(),
        filename: fields.filename,
        storage_backend: session.storage_backend.clone(),
        object_key: session.object_key.clone(),
        url: None,
        mime_type: Some(session.mime_type.clone()),
        size_bytes: Some(session.size_bytes),
        width: fields.width,
        height: fields.height,
        metadata: fields.metadata,
    })
}

pub(super) fn upload_session_reference_target(file_id: &str) -> (&'static str, String) {
    (
        FILE_UPLOAD_SESSION_REFERENCE_KIND,
        file_id.trim().to_string(),
    )
}

pub(super) async fn register_upload_session_reference(
    repository: &FileStorageRepository,
    storage_backend: &str,
    object_key: &str,
    file_id: &str,
    expires_at: DateTime<Utc>,
    metadata: &serde_json::Value,
) -> Result<()> {
    let (reference_kind, reference_id) = upload_session_reference_target(file_id);
    repository
        .insert_reference(
            storage_backend,
            object_key,
            reference_kind,
            &reference_id,
            Some(expires_at),
            metadata,
        )
        .await?
        .ok_or_else(|| Error::InvalidInput("file upload object is not registered".to_string()))?;
    Ok(())
}

pub(super) fn upload_session_ownership_proof_verified(
    metadata: &serde_json::Value,
) -> Result<bool> {
    optional_payload_bool(
        metadata,
        FILE_SESSION_METADATA_OWNERSHIP_PROOF_VERIFIED_KEY,
        "file upload session metadata",
    )
}

pub(super) fn mark_upload_session_ownership_proof_verified(
    metadata: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut metadata = metadata.clone();
    let Some(object) = metadata.as_object_mut() else {
        return Err(Error::InvalidInput(
            "file upload session metadata is invalid".to_string(),
        ));
    };
    object.insert(
        FILE_SESSION_METADATA_OWNERSHIP_PROOF_VERIFIED_KEY.to_string(),
        serde_json::Value::Bool(true),
    );
    Ok(metadata)
}

fn optional_payload_string(
    payload: &serde_json::Value,
    key: &'static str,
    token_name: &'static str,
) -> Result<Option<String>> {
    match payload.get(key) {
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
            Ok(Some(value.clone()))
        }
        Some(serde_json::Value::String(_) | serde_json::Value::Null) | None => Ok(None),
        Some(_) => Err(Error::InvalidInput(format!("invalid {token_name}"))),
    }
}

fn required_payload_string(
    payload: &serde_json::Value,
    key: &'static str,
    token_name: &'static str,
) -> Result<String> {
    optional_payload_string(payload, key, token_name)?
        .ok_or_else(|| Error::InvalidInput(format!("invalid {token_name}")))
}

fn optional_payload_i64(
    payload: &serde_json::Value,
    key: &'static str,
    token_name: &'static str,
) -> Result<Option<i64>> {
    match payload.get(key) {
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| Error::InvalidInput(format!("invalid {token_name}"))),
        None => Ok(None),
    }
}

fn optional_payload_i32(
    payload: &serde_json::Value,
    key: &'static str,
    token_name: &'static str,
) -> Result<Option<i32>> {
    optional_payload_i64(payload, key, token_name)?
        .map(|value| {
            i32::try_from(value).map_err(|_| Error::InvalidInput(format!("invalid {token_name}")))
        })
        .transpose()
}

pub fn submitted_file_reference_from_session_file(
    file: &NewStoredFile,
) -> Result<SubmittedFileReference> {
    Ok(SubmittedFileReference {
        id: file.id.clone(),
        kind: SubmittedFileReferenceKind::Upload,
    })
}

pub fn submitted_file_reference_from_reuse_token(
    token: impl Into<String>,
) -> SubmittedFileReference {
    SubmittedFileReference {
        id: token.into(),
        kind: SubmittedFileReferenceKind::Reuse,
    }
}

pub fn upload_token_from_session_file(file: &NewStoredFile) -> Result<String> {
    file.metadata
        .get(FILE_UPLOAD_TOKEN_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Error::Internal("file upload session token is missing".to_string()))
}

pub(super) fn optional_payload_bool(
    payload: &serde_json::Value,
    key: &'static str,
    token_name: &'static str,
) -> Result<bool> {
    match payload.get(key) {
        Some(serde_json::Value::Bool(value)) => Ok(*value),
        Some(_) => Err(Error::InvalidInput(format!(
            "invalid {token_name}: {key} must be a boolean"
        ))),
        None => Ok(false),
    }
}

fn file_upload_token_key(user_id: UserId, storage_scope: &str, secret: &str) -> String {
    format!(
        "synctv:file-upload:{}:{}:{}",
        user_id.as_i64(),
        storage_scope,
        secret
    )
}

pub(super) fn file_reuse_grant(
    request: &CreateFileReuseGrant<'_>,
    secret: &str,
) -> Result<FileReuseGrant> {
    validate_reuse_grant_request(request)?;
    let payload = json!({
        "kind": FILE_REUSE_TOKEN_KIND,
        "user_id": request.user_id.as_i64(),
        "storage_scope": request.storage_scope,
        "source_kind": request.source_kind,
        "source_id": request.source_id,
        "expires_at": request.expires_at.timestamp(),
    });
    let payload_bytes = serde_json::to_vec(&payload)?;
    let signature = hex::encode(hmac_sha256(
        file_reuse_token_key(request.user_id, request.storage_scope, secret).as_bytes(),
        &payload_bytes,
    )?);
    let encoded_payload = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        payload_bytes,
    );
    Ok(FileReuseGrant {
        token: format!("{FILE_REUSE_TOKEN_VERSION}.{encoded_payload}.{signature}"),
        expires_at: request.expires_at,
    })
}

pub(super) fn validate_file_reuse_grant(
    token: &str,
    context: FileStorageContext<'_>,
    now: DateTime<Utc>,
    secret: &str,
) -> Result<ValidatedFileReuseGrant> {
    let payload = decode_versioned_hmac_token_payload(token, FILE_REUSE_TOKEN_VERSION)?;
    let user_id = payload
        .get("user_id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| Error::InvalidInput("invalid file reuse token".to_string()))?;
    let storage_scope = payload
        .get("storage_scope")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Error::InvalidInput("invalid file reuse token".to_string()))?;
    let user_id = UserId::try_from(user_id)
        .map_err(|_| Error::InvalidInput("invalid file reuse token".to_string()))?;
    let payload = validate_versioned_hmac_token(
        token,
        FILE_REUSE_TOKEN_VERSION,
        file_reuse_token_key(user_id, storage_scope, secret).as_bytes(),
        "invalid file reuse token",
    )?;
    if payload.get("kind").and_then(serde_json::Value::as_str) != Some(FILE_REUSE_TOKEN_KIND) {
        return Err(Error::InvalidInput("invalid file reuse token".to_string()));
    }
    if user_id != context.user_id || storage_scope != context.storage_scope {
        return Err(Error::InvalidInput(
            "file reuse token does not belong to this request".to_string(),
        ));
    }
    let expires_at = payload
        .get("expires_at")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| Error::InvalidInput("invalid file reuse token".to_string()))?;
    if expires_at <= now.timestamp() {
        return Err(Error::InvalidInput(
            "file reuse token has expired".to_string(),
        ));
    }
    let expires_at = DateTime::<Utc>::from_timestamp(expires_at, 0)
        .ok_or_else(|| Error::InvalidInput("invalid file reuse token".to_string()))?;
    Ok(ValidatedFileReuseGrant {
        user_id,
        storage_scope: storage_scope.to_string(),
        source_kind: required_payload_string(&payload, "source_kind", "file reuse token")?,
        source_id: required_payload_string(&payload, "source_id", "file reuse token")?,
        expires_at,
    })
}

fn validate_reuse_grant_request(request: &CreateFileReuseGrant<'_>) -> Result<()> {
    if request.storage_scope.trim().is_empty()
        || request.source_kind.trim().is_empty()
        || request.source_id.trim().is_empty()
    {
        return Err(Error::InvalidInput(
            "file reuse grant request is invalid".to_string(),
        ));
    }
    Ok(())
}

fn file_reuse_token_key(user_id: UserId, storage_scope: &str, secret: &str) -> String {
    format!(
        "synctv:file-reuse:{}:{}:{}",
        user_id.as_i64(),
        storage_scope,
        secret
    )
}

pub(super) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn file_ownership_proof_ranges(
    content_manifest_sha256: &str,
    nonce: &str,
    size_bytes: i64,
) -> Result<Vec<FileOwnershipProofRange>> {
    if size_bytes <= 0 {
        return Ok(Vec::new());
    }
    let range_len = FILE_OWNERSHIP_PROOF_RANGE_BYTES
        .min(i32::try_from(size_bytes).unwrap_or(FILE_OWNERSHIP_PROOF_RANGE_BYTES));
    if size_bytes <= i64::from(range_len) {
        return Ok(vec![FileOwnershipProofRange {
            offset: 0,
            length: range_len,
        }]);
    }

    let seed = Sha256::digest(format!("{content_manifest_sha256}:{nonce}").as_bytes());
    let max_start = size_bytes - i64::from(range_len);
    let max_start = u64::try_from(max_start)
        .map_err(|_| Error::Internal("ownership proof max offset is negative".to_string()))?;
    let modulo = max_start
        .checked_add(1)
        .ok_or_else(|| Error::Internal("ownership proof offset range overflow".to_string()))?;
    let mut ranges = Vec::with_capacity(FILE_OWNERSHIP_PROOF_RANGE_COUNT);
    for index in 0..FILE_OWNERSHIP_PROOF_RANGE_COUNT {
        let start = index * 8;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&seed[start..start + 8]);
        let offset = i64::try_from(u64::from_be_bytes(bytes) % modulo)
            .map_err(|_| Error::Internal("ownership proof offset exceeds i64::MAX".to_string()))?;
        ranges.push(FileOwnershipProofRange {
            offset,
            length: range_len,
        });
    }
    ranges.sort_by_key(|range| range.offset);
    ranges.dedup_by_key(|range| range.offset);
    Ok(ranges)
}

fn ownership_proof_ranges_to_json(ranges: &[FileOwnershipProofRange]) -> serde_json::Value {
    serde_json::Value::Array(
        ranges
            .iter()
            .map(|range| {
                json!({
                    "offset": range.offset,
                    "length": range.length,
                })
            })
            .collect(),
    )
}

pub(super) fn ownership_proof_ranges_from_payload(
    payload: &serde_json::Value,
) -> Result<Vec<FileOwnershipProofRange>> {
    let ranges = payload
        .get("ownership_proof_ranges")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
    ranges
        .iter()
        .map(|range| {
            let offset = range
                .get("offset")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
            let length = range
                .get("length")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
            let length = i32::try_from(length)
                .map_err(|_| Error::InvalidInput("invalid file upload token".to_string()))?;
            Ok(FileOwnershipProofRange { offset, length })
        })
        .collect()
}

pub(crate) fn file_ownership_proof_digest<'a, I>(
    nonce: &str,
    ranges: &[FileOwnershipProofRange],
    content_manifest_sha256: &str,
    size_bytes: i64,
    chunks: I,
) -> String
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut hasher = Sha256::new();
    hasher.update(FILE_OWNERSHIP_PROOF_ALGORITHM.as_bytes());
    hasher.update([0]);
    hasher.update(nonce.as_bytes());
    hasher.update([0]);
    hasher.update(
        content_manifest_sha256
            .trim()
            .to_ascii_lowercase()
            .as_bytes(),
    );
    hasher.update(size_bytes.to_be_bytes());
    hasher.update((ranges.len() as u64).to_be_bytes());
    for (range, chunk) in ranges.iter().zip(chunks) {
        hasher.update(range.offset.to_be_bytes());
        hasher.update(range.length.to_be_bytes());
        hasher.update(chunk);
    }
    hex::encode(hasher.finalize())
}

pub(crate) fn file_part_manifest_digest<'a, I>(
    size_bytes: i64,
    part_size_bytes: i64,
    parts: I,
) -> Result<String>
where
    I: IntoIterator<Item = (i32, i64, &'a str)>,
{
    if size_bytes <= 0 || part_size_bytes <= 0 {
        return Err(Error::InvalidInput(
            "file manifest size and part size must be positive".to_string(),
        ));
    }
    let mut normalized_parts = parts
        .into_iter()
        .map(|(part_number, size_bytes, checksum)| {
            if part_number <= 0 || size_bytes <= 0 {
                return Err(Error::InvalidInput(
                    "file manifest part number and size must be positive".to_string(),
                ));
            }
            let checksum = checksum.trim().to_ascii_lowercase();
            let valid = checksum.len() == crate::models::FILE_SHA256_HEX_CHARS
                && checksum.chars().all(|c| c.is_ascii_hexdigit());
            if !valid {
                return Err(Error::InvalidInput(
                    "file manifest part checksum must be a 64-character hex string".to_string(),
                ));
            }
            Ok((part_number, size_bytes, checksum))
        })
        .collect::<Result<Vec<_>>>()?;
    normalized_parts.sort_by_key(|part| part.0);
    let mut expected_part_number = 1_i32;
    let mut total_size = 0_i64;
    let mut hasher = Sha256::new();
    hasher.update(b"synctv-file-part-manifest-sha256-v1");
    hasher.update([0]);
    hasher.update(size_bytes.to_be_bytes());
    hasher.update(part_size_bytes.to_be_bytes());
    hasher.update((normalized_parts.len() as u64).to_be_bytes());
    for (part_number, part_size, checksum) in normalized_parts {
        if part_number != expected_part_number {
            return Err(Error::InvalidInput(
                "file manifest part numbers must be contiguous".to_string(),
            ));
        }
        total_size = total_size
            .checked_add(part_size)
            .ok_or_else(|| Error::Internal("file manifest size overflow".to_string()))?;
        hasher.update(part_number.to_be_bytes());
        hasher.update(part_size.to_be_bytes());
        hasher.update(checksum.as_bytes());
        expected_part_number = expected_part_number
            .checked_add(1)
            .ok_or_else(|| Error::Internal("file manifest part number overflow".to_string()))?;
    }
    if total_size != size_bytes {
        return Err(Error::InvalidInput(
            "file manifest parts do not match object size".to_string(),
        ));
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
pub(crate) fn ownership_proof_chunks_from_bytes(
    data: &[u8],
    ranges: &[FileOwnershipProofRange],
) -> Result<Vec<Vec<u8>>> {
    ranges
        .iter()
        .map(|range| {
            if range.offset < 0 || range.length <= 0 {
                return Err(Error::InvalidInput(
                    "invalid file ownership proof range".to_string(),
                ));
            }
            let start = usize::try_from(range.offset).map_err(|_| {
                Error::InvalidInput("invalid file ownership proof range".to_string())
            })?;
            let len = usize::try_from(range.length).map_err(|_| {
                Error::InvalidInput("invalid file ownership proof range".to_string())
            })?;
            let end = start.checked_add(len).ok_or_else(|| {
                Error::InvalidInput("invalid file ownership proof range".to_string())
            })?;
            let chunk = data.get(start..end).ok_or_else(|| {
                Error::InvalidInput("invalid file ownership proof range".to_string())
            })?;
            Ok(chunk.to_vec())
        })
        .collect()
}

pub(super) fn database_file_namespace_base_path(storage_namespace: &str) -> String {
    let namespace = storage_namespace.trim().trim_matches('/');
    if namespace.is_empty() {
        "database".to_string()
    } else {
        format!("database/{namespace}")
    }
}

pub(crate) fn file_storage_object_base_path(config_base_path: &str, policy_prefix: &str) -> String {
    let config_base_path = config_base_path.trim().trim_matches('/');
    let policy_prefix = policy_prefix.trim().trim_matches('/');
    match (config_base_path.is_empty(), policy_prefix.is_empty()) {
        (true, true) => String::new(),
        (true, false) => policy_prefix.to_string(),
        (false, true) => config_base_path.to_string(),
        (false, false) => format!("{config_base_path}/{policy_prefix}"),
    }
}

pub(super) fn file_object_key(
    base_path: &str,
    storage_scope: &str,
    file_id: &str,
    mime_type: &str,
) -> String {
    let extension = match mime_type.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/avif" => "avif",
        "image/webp" => "webp",
        _ => "bin",
    };
    let prefix = base_path.trim().trim_matches('/');
    let scope = storage_scope.trim().trim_matches('/');
    let key = if scope.is_empty() {
        format!("{file_id}.{extension}")
    } else {
        format!("{scope}/{file_id}.{extension}")
    };
    if prefix.is_empty() {
        key
    } else {
        format!("{prefix}/{key}")
    }
}

pub(crate) fn file_storage_public_url(
    config: &S3FileStorageConfig,
    object_key: &str,
) -> Result<String> {
    let base = config
        .public_base_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| Error::InvalidInput("S3 public_base_url is not configured".to_string()))?;
    s3_path_style_url(base, &config.bucket, object_key)
}

pub(crate) fn optional_file_storage_public_url(
    config: &S3FileStorageConfig,
    object_key: &str,
) -> Result<Option<String>> {
    if config
        .public_base_url
        .as_deref()
        .is_none_or(|url| url.trim().is_empty())
    {
        return Ok(None);
    }
    file_storage_public_url(config, object_key).map(Some)
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|error| Error::Internal(format!("failed to initialize HMAC-SHA256: {error}")))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

pub(super) fn encode_database_file_object_key(object_key: &str) -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        object_key.as_bytes(),
    )
}

pub(super) fn decode_database_file_object_key(encoded: &str) -> Result<String> {
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, encoded)
        .map_err(|_| Error::InvalidInput("invalid file object key".to_string()))?;
    String::from_utf8(bytes).map_err(|_| Error::InvalidInput("invalid file object key".to_string()))
}

pub(super) fn database_file_object_url(
    route_prefix: &str,
    storage_backend: &str,
    object_key: &str,
    secret: &str,
) -> Result<String> {
    let encoded_key = encode_database_file_object_key(object_key);
    let read_token = database_file_read_token(storage_backend, object_key, secret)?;
    let route_prefix = route_prefix.trim().trim_end_matches('/');
    if !route_prefix.starts_with('/') {
        return Err(Error::InvalidInput(
            "database object route prefix must be absolute".to_string(),
        ));
    }
    Ok(format!("{route_prefix}/{encoded_key}?token={read_token}"))
}

pub(super) fn database_file_read_token(
    storage_backend: &str,
    object_key: &str,
    secret: &str,
) -> Result<String> {
    let payload = json!({ "storage_backend": storage_backend, "object_key": object_key });
    let payload_bytes = serde_json::to_vec(&payload)?;
    let signature = hex::encode(hmac_sha256(
        format!("synctv:file-read:{secret}").as_bytes(),
        &payload_bytes,
    )?);
    let encoded_payload = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        payload_bytes,
    );
    Ok(format!(
        "{DATABASE_FILE_READ_TOKEN_VERSION}.{encoded_payload}.{signature}"
    ))
}

pub(super) fn database_file_read_token_storage_backend(token: &str) -> Result<String> {
    decode_versioned_hmac_token_payload(token, DATABASE_FILE_READ_TOKEN_VERSION)?
        .get("storage_backend")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Error::InvalidInput("invalid file read token".to_string()))
}

pub(super) fn validate_database_file_read_token(
    storage_backend: &str,
    object_key: &str,
    token: &str,
    secret: &str,
) -> Result<()> {
    let payload = validate_versioned_hmac_token(
        token,
        DATABASE_FILE_READ_TOKEN_VERSION,
        format!("synctv:file-read:{secret}").as_bytes(),
        "invalid file read token",
    )?;
    if payload
        .get("storage_backend")
        .and_then(serde_json::Value::as_str)
        != Some(storage_backend)
    {
        return Err(Error::InvalidInput("invalid file read token".to_string()));
    }
    if payload
        .get("object_key")
        .and_then(serde_json::Value::as_str)
        != Some(object_key)
    {
        return Err(Error::InvalidInput("invalid file read token".to_string()));
    }
    Ok(())
}

pub(super) fn file_upload_token_storage_backend(token: &str) -> Result<String> {
    decode_versioned_hmac_token_payload(token, FILE_UPLOAD_TOKEN_VERSION)?
        .get("storage_backend")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))
}

pub(super) fn validate_database_file_upload_token(
    storage_backend: &str,
    token: &str,
    object_key: &str,
    now: DateTime<Utc>,
    secret: &str,
) -> Result<serde_json::Value> {
    let payload = decode_versioned_hmac_token_payload(token, FILE_UPLOAD_TOKEN_VERSION)?;
    let user_id = payload
        .get("user_id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
    let storage_scope = payload
        .get("storage_scope")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
    let key = file_upload_token_key(
        UserId::try_from(user_id)
            .map_err(|_| Error::InvalidInput("invalid file upload token".to_string()))?,
        storage_scope,
        secret,
    );
    let payload = validate_versioned_hmac_token(
        token,
        FILE_UPLOAD_TOKEN_VERSION,
        key.as_bytes(),
        "invalid file upload token",
    )?;
    if payload
        .get("storage_backend")
        .and_then(serde_json::Value::as_str)
        != Some(storage_backend)
        || payload
            .get("object_key")
            .and_then(serde_json::Value::as_str)
            != Some(object_key)
    {
        return Err(Error::InvalidInput("invalid file upload token".to_string()));
    }
    let expires_at = payload
        .get("expires_at")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
    if expires_at <= now.timestamp() {
        return Err(Error::InvalidInput(
            "file upload session has expired".to_string(),
        ));
    }
    Ok(payload)
}

fn decode_versioned_hmac_token_payload(
    token: &str,
    expected_version: &str,
) -> Result<serde_json::Value> {
    let (_version, encoded_payload, _signature) =
        split_versioned_hmac_token(token, expected_version, "invalid token")?;
    let payload_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        encoded_payload,
    )
    .map_err(|_| Error::InvalidInput("invalid token".to_string()))?;
    serde_json::from_slice(&payload_bytes)
        .map_err(|_| Error::InvalidInput("invalid token".to_string()))
}

fn validate_versioned_hmac_token(
    token: &str,
    expected_version: &str,
    key: &[u8],
    error_message: &str,
) -> Result<serde_json::Value> {
    let (_version, encoded_payload, signature) =
        split_versioned_hmac_token(token, expected_version, error_message)?;
    let payload_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        encoded_payload,
    )
    .map_err(|_| Error::InvalidInput(error_message.to_string()))?;
    let expected_signature = hex::encode(hmac_sha256(key, &payload_bytes)?);
    if !constant_time_eq(signature.as_bytes(), expected_signature.as_bytes()) {
        return Err(Error::InvalidInput(error_message.to_string()));
    }
    serde_json::from_slice(&payload_bytes)
        .map_err(|_| Error::InvalidInput(error_message.to_string()))
}

fn split_versioned_hmac_token<'a>(
    token: &'a str,
    expected_version: &str,
    error_message: &str,
) -> Result<(&'a str, &'a str, &'a str)> {
    let mut parts = token.split('.');
    let Some(version) = parts.next().filter(|part| !part.is_empty()) else {
        return Err(Error::InvalidInput(error_message.to_string()));
    };
    let Some(encoded_payload) = parts.next().filter(|part| !part.is_empty()) else {
        return Err(Error::InvalidInput(error_message.to_string()));
    };
    let Some(signature) = parts.next().filter(|part| !part.is_empty()) else {
        return Err(Error::InvalidInput(error_message.to_string()));
    };
    if version != expected_version || parts.next().is_some() {
        return Err(Error::InvalidInput(error_message.to_string()));
    }
    Ok((version, encoded_payload, signature))
}

pub(super) fn upload_media_type(content_type: &str) -> Result<&str> {
    let media_type = content_type
        .split_once(';')
        .map_or(content_type, |(media_type, _)| media_type)
        .trim();
    if media_type.is_empty() {
        return Err(Error::InvalidInput(
            "file content-type media type is empty".to_string(),
        ));
    }
    Ok(media_type)
}

fn s3_path_style_url(base: &str, bucket: &str, object_key: &str) -> Result<String> {
    let mut url = url::Url::parse(base.trim())
        .map_err(|error| Error::InvalidInput(format!("Invalid S3 public URL base: {error}")))?;
    {
        let mut segments = url.path_segments_mut().map_err(|()| {
            Error::InvalidInput("S3 public URL base must be hierarchical".to_string())
        })?;
        segments.push(bucket.trim());
        for segment in object_key.split('/').filter(|segment| !segment.is_empty()) {
            segments.push(segment);
        }
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests;
