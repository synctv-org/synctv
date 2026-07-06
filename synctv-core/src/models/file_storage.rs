use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use sqlx::{
    postgres::{PgArgumentBuffer, PgTypeInfo, PgValueRef},
    Decode, Encode, Postgres, Type,
};
use std::collections::BTreeMap;

use super::UserId;

pub const FILE_ID_MAX_CHARS: usize = 128;
pub const FILE_STORAGE_BACKEND_MAX_CHARS: usize = 64;
pub const FILE_OBJECT_KEY_MAX_CHARS: usize = 2048;
pub const FILE_REFERENCE_KIND_MAX_CHARS: usize = 64;
pub const FILE_REFERENCE_ID_MAX_CHARS: usize = 256;
pub const FILE_CLEANUP_ORIGIN_MAX_CHARS: usize = 64;
pub const FILE_SHA256_HEX_CHARS: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileAudioMetadata {
    pub duration_seconds: i32,
    pub bitrate_bps: i32,
    pub sample_rate_hz: Option<i32>,
    pub channels: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileMetadata {
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub blurhash: Option<String>,
    pub audio: Option<FileAudioMetadata>,
    pub variants: Vec<FileObjectVariant>,
    pub upload_token: Option<String>,
    pub ownership_proof: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileVariantMetadata {
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub blurhash: Option<String>,
}

impl FileMetadata {
    #[must_use]
    pub fn public(&self) -> Self {
        let mut metadata = self.clone();
        metadata.upload_token = None;
        metadata.ownership_proof = None;
        metadata
    }
}

impl Type<Postgres> for FileMetadata {
    fn type_info() -> PgTypeInfo {
        <sqlx::types::Json<FileMetadata> as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <sqlx::types::Json<FileMetadata> as Type<Postgres>>::compatible(ty)
    }
}

impl Encode<'_, Postgres> for FileMetadata {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        sqlx::types::Json(self).encode_by_ref(buf)
    }
}

impl<'r> Decode<'r, Postgres> for FileMetadata {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let sqlx::types::Json(metadata) =
            <sqlx::types::Json<Self> as Decode<Postgres>>::decode(value)?;
        Ok(metadata)
    }
}

impl Type<Postgres> for FileVariantMetadata {
    fn type_info() -> PgTypeInfo {
        <sqlx::types::Json<FileVariantMetadata> as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <sqlx::types::Json<FileVariantMetadata> as Type<Postgres>>::compatible(ty)
    }
}

impl Encode<'_, Postgres> for FileVariantMetadata {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        sqlx::types::Json(self).encode_by_ref(buf)
    }
}

impl<'r> Decode<'r, Postgres> for FileVariantMetadata {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let sqlx::types::Json(metadata) =
            <sqlx::types::Json<Self> as Decode<Postgres>>::decode(value)?;
        Ok(metadata)
    }
}

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
    pub metadata: FileMetadata,
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
    pub metadata: FileMetadata,
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
    pub metadata: FileMetadata,
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
    pub metadata: FileMetadata,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileObjectVariant {
    pub storage_backend: String,
    pub object_key: String,
    pub original_storage_backend: String,
    pub original_object_key: String,
    pub group_id: String,
    pub variant_key: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(default)]
    pub object_access: Option<FileObjectAccess>,
    pub url: Option<String>,
    pub mime_type: String,
    pub size_bytes: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub is_original: bool,
    pub lossy: bool,
    pub quality: Option<i32>,
    pub sort_order: i32,
    pub metadata: FileVariantMetadata,
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
    DatabaseSingle = 3,
    S3Single = 4,
}

impl From<FileUploadSessionKind> for i16 {
    fn from(value: FileUploadSessionKind) -> Self {
        match value {
            FileUploadSessionKind::DatabaseMultipart => 1,
            FileUploadSessionKind::S3Multipart => 2,
            FileUploadSessionKind::DatabaseSingle => 3,
            FileUploadSessionKind::S3Single => 4,
        }
    }
}

impl TryFrom<i16> for FileUploadSessionKind {
    type Error = ();

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::DatabaseMultipart),
            2 => Ok(Self::S3Multipart),
            3 => Ok(Self::DatabaseSingle),
            4 => Ok(Self::S3Single),
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
    pub metadata: FileUploadSessionMetadata,
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
    pub metadata: FileMetadata,
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
#[serde(rename_all = "camelCase")]
pub struct FileReferenceTarget {
    pub storage_backend: String,
    pub object_key: String,
    pub reference_kind: String,
    pub reference_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FileReferenceMetadata {
    File(FileMetadata),
    UploadSession(FileUploadSessionMetadata),
}

impl FileReferenceMetadata {
    #[must_use]
    pub fn file_metadata(&self) -> FileMetadata {
        match self {
            Self::File(metadata) => metadata.clone(),
            Self::UploadSession(metadata) => metadata.metadata.clone(),
        }
    }
}

impl Default for FileReferenceMetadata {
    fn default() -> Self {
        Self::File(FileMetadata::default())
    }
}

impl From<FileMetadata> for FileReferenceMetadata {
    fn from(value: FileMetadata) -> Self {
        Self::File(value)
    }
}

impl From<FileUploadSessionMetadata> for FileReferenceMetadata {
    fn from(value: FileUploadSessionMetadata) -> Self {
        Self::UploadSession(value)
    }
}

impl Type<Postgres> for FileReferenceMetadata {
    fn type_info() -> PgTypeInfo {
        <sqlx::types::Json<FileReferenceMetadata> as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <sqlx::types::Json<FileReferenceMetadata> as Type<Postgres>>::compatible(ty)
    }
}

impl Encode<'_, Postgres> for FileReferenceMetadata {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        sqlx::types::Json(self).encode_by_ref(buf)
    }
}

impl<'r> Decode<'r, Postgres> for FileReferenceMetadata {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let sqlx::types::Json(metadata) =
            <sqlx::types::Json<Self> as Decode<Postgres>>::decode(value)?;
        Ok(metadata)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCleanupMetadata {
    pub reason: Option<String>,
}

impl Type<Postgres> for FileCleanupMetadata {
    fn type_info() -> PgTypeInfo {
        <sqlx::types::Json<FileCleanupMetadata> as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <sqlx::types::Json<FileCleanupMetadata> as Type<Postgres>>::compatible(ty)
    }
}

impl Encode<'_, Postgres> for FileCleanupMetadata {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        sqlx::types::Json(self).encode_by_ref(buf)
    }
}

impl<'r> Decode<'r, Postgres> for FileCleanupMetadata {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let sqlx::types::Json(metadata) =
            <sqlx::types::Json<Self> as Decode<Postgres>>::decode(value)?;
        Ok(metadata)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileCleanupJob {
    pub id: i64,
    pub origin: String,
    pub storage_backend: String,
    pub object_key: String,
    pub reference_kind: String,
    pub reference_id: String,
    pub metadata: FileCleanupMetadata,
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
#[serde(rename_all = "camelCase")]
pub struct FileOwnershipProofRange {
    pub offset: i64,
    pub length: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileObjectKind {
    ChatAttachment,
    UserAvatar,
    MediaCover,
    MediaThumbnail,
    RoomCover,
    PlaylistCover,
    #[default]
    Generic,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadPolicy {
    pub kind: String,
    pub object_kind: FileObjectKind,
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
}

impl<'de> Deserialize<'de> for FileUploadPolicy {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Default, Deserialize)]
        #[serde(default, rename_all = "camelCase")]
        struct WireFileUploadPolicy {
            kind: String,
            object_kind: Option<FileObjectKind>,
            max_size_bytes: i64,
            max_width: Option<i32>,
            max_height: Option<i32>,
            require_image_dimensions: bool,
            max_audio_duration_seconds: Option<i32>,
            max_audio_bitrate_bps: Option<i32>,
            require_audio_metadata: bool,
            allowed_mime_prefixes: Vec<String>,
            allowed_mime_types: Vec<String>,
            storage_namespace: String,
        }

        let wire = WireFileUploadPolicy::deserialize(deserializer)?;
        let object_kind = match wire.object_kind {
            Some(object_kind) => object_kind,
            None => file_object_kind_for_upload_policy(&wire.kind).ok_or_else(|| {
                serde::de::Error::custom(format!("unknown file upload policy kind: {}", wire.kind))
            })?,
        };
        Ok(Self {
            object_kind,
            kind: wire.kind,
            max_size_bytes: wire.max_size_bytes,
            max_width: wire.max_width,
            max_height: wire.max_height,
            require_image_dimensions: wire.require_image_dimensions,
            max_audio_duration_seconds: wire.max_audio_duration_seconds,
            max_audio_bitrate_bps: wire.max_audio_bitrate_bps,
            require_audio_metadata: wire.require_audio_metadata,
            allowed_mime_prefixes: wire.allowed_mime_prefixes,
            allowed_mime_types: wire.allowed_mime_types,
            storage_namespace: wire.storage_namespace,
        })
    }
}

fn file_object_kind_for_upload_policy(kind: &str) -> Option<FileObjectKind> {
    match kind {
        "chat_attachment" => Some(FileObjectKind::ChatAttachment),
        "user_avatar" => Some(FileObjectKind::UserAvatar),
        "media_cover" => Some(FileObjectKind::MediaCover),
        "media_thumbnail" => Some(FileObjectKind::MediaThumbnail),
        "room_cover" => Some(FileObjectKind::RoomCover),
        "playlist_cover" => Some(FileObjectKind::PlaylistCover),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewStoredFile {
    pub id: String,
    pub filename: Option<String>,
    pub storage_backend: String,
    pub object_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_access: Option<FileObjectAccess>,
    pub url: Option<String>,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub metadata: FileMetadata,
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
    pub metadata: FileMetadata,
    pub policy: FileUploadPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadManifestPart {
    pub part_number: i32,
    pub offset_bytes: i64,
    pub size_bytes: i64,
    pub checksum_sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadOwnershipProofMetadata {
    pub nonce: String,
    pub ranges: Vec<FileOwnershipProofRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadSessionMetadata {
    #[serde(alias = "publicFileId")]
    pub file_id: String,
    pub user_id: UserId,
    pub storage_scope: String,
    pub client_file_id: Option<String>,
    pub filename: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub metadata: FileMetadata,
    pub upload_policy: FileUploadPolicy,
    pub manifest_parts: Vec<FileUploadManifestPart>,
    pub ownership_proof: Option<FileUploadOwnershipProofMetadata>,
    pub ownership_proof_verified: bool,
}

impl Type<Postgres> for FileUploadSessionMetadata {
    fn type_info() -> PgTypeInfo {
        <sqlx::types::Json<FileUploadSessionMetadata> as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <sqlx::types::Json<FileUploadSessionMetadata> as Type<Postgres>>::compatible(ty)
    }
}

impl Encode<'_, Postgres> for FileUploadSessionMetadata {
    fn encode_by_ref(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        sqlx::types::Json(self).encode_by_ref(buf)
    }
}

impl<'r> Decode<'r, Postgres> for FileUploadSessionMetadata {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let sqlx::types::Json(metadata) =
            <sqlx::types::Json<Self> as Decode<Postgres>>::decode(value)?;
        Ok(metadata)
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileObjectAccess {
    pub object_kind: FileObjectKind,
    pub encoded_object_key: String,
    pub read_token: String,
}

impl Type<Postgres> for FileObjectAccess {
    fn type_info() -> PgTypeInfo {
        <sqlx::types::Json<FileObjectAccess> as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <sqlx::types::Json<FileObjectAccess> as Type<Postgres>>::compatible(ty)
    }
}

impl<'r> Decode<'r, Postgres> for FileObjectAccess {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let sqlx::types::Json(access) =
            <sqlx::types::Json<Self> as Decode<Postgres>>::decode(value)?;
        Ok(access)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadSession {
    pub file: NewStoredFile,
    pub encoded_object_key: String,
    pub upload_required: bool,
    pub ownership_proof_required: bool,
    pub ownership_proof_nonce: Option<String>,
    pub ownership_proof_ranges: Vec<FileOwnershipProofRange>,
    pub upload_object_access: Option<FileObjectAccess>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_object_access_uses_camel_case_json_fields() {
        let access = FileObjectAccess {
            object_kind: FileObjectKind::MediaCover,
            encoded_object_key: "encoded-key".to_string(),
            read_token: "read-token".to_string(),
        };

        let json = serde_json::to_value(&access).expect("access should serialize");

        assert_eq!(json["objectKind"], "media_cover");
        assert_eq!(json["encodedObjectKey"], "encoded-key");
        assert_eq!(json["readToken"], "read-token");
        assert!(json.get("encoded_object_key").is_none());
        assert!(json.get("read_token").is_none());
    }

    #[test]
    fn file_upload_policy_derives_object_kind_from_known_kind() {
        let json = serde_json::json!({
            "kind": "media_cover",
            "maxSizeBytes": 2048,
            "maxWidth": 1920,
            "maxHeight": 1080,
            "requireImageDimensions": true,
            "maxAudioDurationSeconds": null,
            "maxAudioBitrateBps": null,
            "requireAudioMetadata": false,
            "allowedMimePrefixes": ["image/"],
            "allowedMimeTypes": [],
            "storageNamespace": "media-covers"
        });

        let policy: FileUploadPolicy =
            serde_json::from_value(json).expect("policy should deserialize");

        assert_eq!(policy.kind, "media_cover");
        assert_eq!(policy.object_kind, FileObjectKind::MediaCover);
        assert_eq!(policy.max_size_bytes, 2048);
    }

    #[test]
    fn file_upload_policy_rejects_unknown_kind_without_object_kind() {
        let json = serde_json::json!({
            "kind": "custom_file",
            "maxSizeBytes": 2048,
            "allowedMimeTypes": ["application/octet-stream"],
            "storageNamespace": "custom-files"
        });

        let error =
            serde_json::from_value::<FileUploadPolicy>(json).expect_err("policy should fail");

        assert!(error
            .to_string()
            .contains("unknown file upload policy kind"));
    }

    #[test]
    fn file_reference_metadata_derives_upload_policy_object_kind() {
        let json = serde_json::json!({
            "kind": "uploadSession",
            "data": {
                "fileId": "file-1",
                "userId": 1,
                "storageScope": "room:1",
                "clientFileId": null,
                "filename": "cover.webp",
                "width": 320,
                "height": 180,
                "metadata": {},
                "uploadPolicy": {
                    "kind": "room_cover",
                    "maxSizeBytes": 4096,
                    "maxWidth": 1920,
                    "maxHeight": 1080,
                    "requireImageDimensions": true,
                    "maxAudioDurationSeconds": null,
                    "maxAudioBitrateBps": null,
                    "requireAudioMetadata": false,
                    "allowedMimePrefixes": ["image/"],
                    "allowedMimeTypes": [],
                    "storageNamespace": "room-covers"
                },
                "manifestParts": [],
                "ownershipProof": null,
                "ownershipProofVerified": false
            }
        });

        let metadata: FileReferenceMetadata =
            serde_json::from_value(json).expect("reference metadata should deserialize");

        let FileReferenceMetadata::UploadSession(session) = metadata else {
            panic!("metadata should be upload session");
        };
        assert_eq!(session.file_id, "file-1");
        assert_eq!(session.upload_policy.object_kind, FileObjectKind::RoomCover);
    }
}
