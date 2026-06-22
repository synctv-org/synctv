use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

use super::UserId;

pub const FILE_ID_MAX_CHARS: usize = 128;
pub const FILE_STORAGE_BACKEND_MAX_CHARS: usize = 64;
pub const FILE_OBJECT_KEY_MAX_CHARS: usize = 2048;
pub const FILE_REFERENCE_KIND_MAX_CHARS: usize = 64;
pub const FILE_REFERENCE_ID_MAX_CHARS: usize = 256;
pub const FILE_CLEANUP_ORIGIN_MAX_CHARS: usize = 64;
pub const FILE_SHA256_HEX_CHARS: usize = 64;
pub const FILE_GENERATED_VARIANTS_METADATA_KEY: &str = "_synctv_file_variants";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
#[serde(rename_all = "snake_case")]
pub enum FileBlobCompression {
    None = 0,
    Zstd = 1,
    Lz4 = 2,
}

impl FileBlobCompression {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zstd => "zstd",
            Self::Lz4 => "lz4",
        }
    }
}

impl From<FileBlobCompression> for i16 {
    fn from(value: FileBlobCompression) -> Self {
        match value {
            FileBlobCompression::None => 0,
            FileBlobCompression::Zstd => 1,
            FileBlobCompression::Lz4 => 2,
        }
    }
}

impl TryFrom<i16> for FileBlobCompression {
    type Error = ();

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Zstd),
            2 => Ok(Self::Lz4),
            _ => Err(()),
        }
    }
}

impl sqlx::Type<sqlx::Postgres> for FileBlobCompression {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for FileBlobCompression {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&i16::from(*self), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for FileBlobCompression {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let value = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Self::try_from(value)
            .map_err(|()| format!("unknown file blob compression value {value}").into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileBlob {
    pub storage_backend: String,
    pub object_key: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub total_size_bytes: i64,
    pub content_manifest_sha256: String,
    pub compression: FileBlobCompression,
    pub range: Option<FileByteRange>,
    pub data: Vec<u8>,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileObjectMetadata {
    pub storage_backend: String,
    pub object_key: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub total_size_bytes: i64,
    pub content_manifest_sha256: String,
    pub compression: FileBlobCompression,
    pub range: Option<FileByteRange>,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
}

impl FileObjectMetadata {
    #[must_use]
    pub fn empty_blob(&self) -> FileBlob {
        FileBlob {
            storage_backend: self.storage_backend.clone(),
            object_key: self.object_key.clone(),
            mime_type: self.mime_type.clone(),
            size_bytes: self.size_bytes,
            total_size_bytes: self.total_size_bytes,
            content_manifest_sha256: self.content_manifest_sha256.clone(),
            compression: self.compression,
            range: self.range,
            data: Vec::new(),
            metadata: self.metadata.clone(),
            created_at: self.created_at,
        }
    }
}

pub type FileObjectStream = BoxStream<'static, crate::Result<bytes::Bytes>>;

pub struct FileObjectDownload {
    pub metadata: FileObjectMetadata,
    pub stream: FileObjectStream,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileObject {
    pub storage_backend: String,
    pub object_key: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub content_manifest_sha256: String,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
    pub validated_at: Option<DateTime<Utc>>,
    pub deleting_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileObjectGroup {
    pub id: String,
    pub storage_backend: String,
    pub original_object_key: String,
    pub media_kind: String,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileObjectVariant {
    pub storage_backend: String,
    pub object_key: String,
    pub original_storage_backend: String,
    pub original_object_key: String,
    pub group_id: String,
    pub variant_key: String,
    pub label: String,
    pub url: Option<String>,
    pub mime_type: String,
    pub size_bytes: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub is_original: bool,
    pub lossy: bool,
    pub quality: Option<i32>,
    pub sort_order: i32,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileBlobPart {
    pub storage_backend: String,
    pub object_key: String,
    pub part_index: i32,
    pub offset_bytes: i64,
    pub size_bytes: i64,
    pub checksum_sha256: String,
    pub compression: FileBlobCompression,
    pub data: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
#[serde(rename_all = "snake_case")]
pub enum FileUploadSessionKind {
    DatabaseMultipart = 1,
    S3Multipart = 2,
}

impl From<FileUploadSessionKind> for i16 {
    fn from(value: FileUploadSessionKind) -> Self {
        match value {
            FileUploadSessionKind::DatabaseMultipart => 1,
            FileUploadSessionKind::S3Multipart => 2,
        }
    }
}

impl TryFrom<i16> for FileUploadSessionKind {
    type Error = ();

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::DatabaseMultipart),
            2 => Ok(Self::S3Multipart),
            _ => Err(()),
        }
    }
}

impl sqlx::Type<sqlx::Postgres> for FileUploadSessionKind {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for FileUploadSessionKind {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&i16::from(*self), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for FileUploadSessionKind {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let value = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Self::try_from(value)
            .map_err(|()| format!("unknown file upload session kind {value}").into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileUploadSessionRecord {
    pub storage_backend: String,
    pub upload_session_key: String,
    pub object_key: String,
    pub session_kind: FileUploadSessionKind,
    pub upload_id: Option<String>,
    pub user_id: i64,
    pub storage_scope: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub content_manifest_sha256: String,
    pub part_size_bytes: i64,
    pub metadata: JsonValue,
    pub expires_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileUploadSessionPart {
    pub storage_backend: String,
    pub upload_session_key: String,
    pub part_index: i32,
    pub part_number: i32,
    pub offset_bytes: i64,
    pub size_bytes: i64,
    pub checksum_sha256: Option<String>,
    pub etag: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct StoredFileReference {
    pub file_reference_id: i64,
    pub storage_backend: String,
    pub object_key: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub content_manifest_sha256: String,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
    pub validated_at: Option<DateTime<Utc>>,
}

impl StoredFileReference {
    #[must_use]
    pub fn reference_target(
        &self,
        reference_kind: impl Into<String>,
        reference_id: impl Into<String>,
    ) -> FileReferenceTarget {
        FileReferenceTarget {
            storage_backend: self.storage_backend.clone(),
            object_key: self.object_key.clone(),
            reference_kind: reference_kind.into(),
            reference_id: reference_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReferenceTarget {
    pub storage_backend: String,
    pub object_key: String,
    pub reference_kind: String,
    pub reference_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileCleanupJob {
    pub id: i64,
    pub origin: String,
    pub storage_backend: String,
    pub object_key: String,
    pub reference_kind: String,
    pub reference_id: String,
    pub metadata: JsonValue,
    pub attempt_count: i32,
    pub last_error: Option<String>,
    pub next_attempt_at: DateTime<Utc>,
    pub locked_at: Option<DateTime<Utc>>,
    pub locked_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl FileCleanupJob {
    #[must_use]
    pub fn reference_target(&self) -> FileReferenceTarget {
        FileReferenceTarget {
            storage_backend: self.storage_backend.clone(),
            object_key: self.object_key.clone(),
            reference_kind: self.reference_kind.clone(),
            reference_id: self.reference_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOwnershipProofRange {
    pub offset: i64,
    pub length: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadPolicy {
    pub kind: String,
    pub max_size_bytes: i64,
    pub max_width: Option<i32>,
    pub max_height: Option<i32>,
    pub require_image_dimensions: bool,
    pub max_audio_duration_seconds: Option<i32>,
    pub max_audio_bitrate_bps: Option<i32>,
    pub require_audio_metadata: bool,
    pub allowed_mime_prefixes: Vec<String>,
    pub allowed_mime_types: Vec<String>,
    pub storage_namespace: String,
    pub database_object_route_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewStoredFile {
    pub id: String,
    pub filename: Option<String>,
    pub storage_backend: String,
    pub object_key: String,
    pub url: Option<String>,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub metadata: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmittedFileReferenceKind {
    Upload,
    Reuse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmittedFileReference {
    pub id: String,
    pub kind: SubmittedFileReferenceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateFileUploadSession {
    pub user_id: UserId,
    pub storage_scope: String,
    pub client_file_id: Option<String>,
    pub filename: Option<String>,
    pub mime_type: String,
    pub size_bytes: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_seconds: Option<i32>,
    pub bitrate_bps: Option<i32>,
    pub parts: Vec<FileUploadManifestPart>,
    pub metadata: JsonValue,
    pub policy: FileUploadPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadManifestPart {
    pub part_number: i32,
    pub offset_bytes: i64,
    pub size_bytes: i64,
    pub checksum_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadPlanPart {
    pub part_number: i32,
    pub offset_bytes: i64,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadPlan {
    pub checksum_algorithm: String,
    pub part_size_bytes: i64,
    pub parts: Vec<FileUploadPlanPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadPartUrl {
    pub part_number: i32,
    pub offset_bytes: i64,
    pub size_bytes: i64,
    pub upload_url: String,
    pub upload_method: String,
    pub upload_headers: BTreeMap<String, String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadSession {
    pub file: NewStoredFile,
    pub encoded_object_key: String,
    pub upload_required: bool,
    pub ownership_proof_required: bool,
    pub ownership_proof_nonce: Option<String>,
    pub ownership_proof_ranges: Vec<FileOwnershipProofRange>,
    pub upload_url: Option<String>,
    pub upload_method: Option<String>,
    pub upload_headers: BTreeMap<String, String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_size_bytes: i64,
    pub resumable: bool,
    pub part_size_bytes: i64,
    pub uploaded_size_bytes: i64,
    pub uploaded_parts: Vec<i32>,
    pub upload_id: Option<String>,
    pub part_urls: Vec<FileUploadPartUrl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileUploadSessionCreateResult {
    Plan(FileUploadPlan),
    Session(FileUploadSession),
}

impl FileUploadSessionCreateResult {
    #[must_use]
    pub const fn as_session(&self) -> Option<&FileUploadSession> {
        match self {
            Self::Session(session) => Some(session),
            Self::Plan(_) => None,
        }
    }

    #[must_use]
    pub fn into_session(self) -> Option<FileUploadSession> {
        match self {
            Self::Session(session) => Some(session),
            Self::Plan(_) => None,
        }
    }

    #[must_use]
    pub const fn as_plan(&self) -> Option<&FileUploadPlan> {
        match self {
            Self::Plan(plan) => Some(plan),
            Self::Session(_) => None,
        }
    }

    #[must_use]
    pub fn into_plan(self) -> Option<FileUploadPlan> {
        match self {
            Self::Plan(plan) => Some(plan),
            Self::Session(_) => None,
        }
    }
}

#[cfg(test)]
impl std::ops::Deref for FileUploadSessionCreateResult {
    type Target = FileUploadSession;

    fn deref(&self) -> &Self::Target {
        self.as_session()
            .expect("upload session create result should contain a session")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileUploadRange {
    pub start: i64,
    pub end_inclusive: i64,
    pub total_size: i64,
}

impl FileUploadRange {
    #[must_use]
    pub const fn size_bytes(self) -> i64 {
        self.end_inclusive - self.start + 1
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreFileUpload {
    pub encoded_object_key: String,
    pub upload_token: String,
    pub content_type: Option<String>,
    pub range: Option<FileUploadRange>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteFileUploadPart {
    pub part_number: i32,
    pub etag: String,
    pub size_bytes: i64,
    pub checksum_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteFileUploadSession {
    pub file_id: Option<String>,
    pub encoded_object_key: String,
    pub upload_token: String,
    pub upload_id: Option<String>,
    pub ownership_proof: Option<String>,
    pub parts: Vec<CompleteFileUploadPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StoreFileUploadResult {
    Complete(FileBlob),
    PartAccepted {
        uploaded_size_bytes: i64,
        uploaded_parts: Vec<i32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileByteRange {
    pub start: i64,
    pub end_inclusive: i64,
}

impl FileByteRange {
    #[must_use]
    pub const fn size_bytes(self) -> i64 {
        self.end_inclusive - self.start + 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileRangeRequest {
    Exact(FileByteRange),
    From { start: i64 },
    Suffix { length: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetFileObject {
    pub encoded_object_key: String,
    pub read_token: String,
    pub range: Option<FileRangeRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileObjectData {
    pub storage_backend: String,
    pub object_key: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub content_manifest_sha256: String,
    pub data: Vec<u8>,
    pub range: Option<FileByteRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteFileUploadSessionResult {
    pub object: Option<FileBlob>,
    pub uploaded_size_bytes: i64,
    pub uploaded_parts: Vec<i32>,
}
