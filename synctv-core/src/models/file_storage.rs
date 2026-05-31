use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

use super::UserId;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileBlob {
    pub storage_backend: String,
    pub object_key: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub checksum_sha256: String,
    pub data: Vec<u8>,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FileObject {
    pub storage_backend: String,
    pub object_key: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub checksum_sha256: String,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
    pub validated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct StoredFileReference {
    pub file_reference_id: i64,
    pub storage_backend: String,
    pub object_key: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub checksum_sha256: String,
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
    pub allowed_mime_prefixes: Vec<String>,
    pub allowed_mime_types: Vec<String>,
    pub storage_namespace: String,
    pub database_object_route_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewStoredFile {
    pub id: String,
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
pub struct CreateFileUploadSession {
    pub user_id: UserId,
    pub storage_scope: String,
    pub client_file_id: Option<String>,
    pub mime_type: String,
    pub size_bytes: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub checksum_sha256: Option<String>,
    pub metadata: JsonValue,
    pub policy: FileUploadPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadSession {
    pub file: NewStoredFile,
    pub upload_required: bool,
    pub ownership_proof_required: bool,
    pub ownership_proof_nonce: Option<String>,
    pub ownership_proof_ranges: Vec<FileOwnershipProofRange>,
    pub ownership_proof_metadata_key: Option<String>,
    pub upload_url: Option<String>,
    pub upload_method: Option<String>,
    pub upload_headers: BTreeMap<String, String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_size_bytes: i64,
}
