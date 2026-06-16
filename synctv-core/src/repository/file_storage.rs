use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{
    models::{
        FileBlob, FileBlobCompression, FileBlobPart, FileCleanupJob, FileObject,
        FileReferenceTarget, FileUploadSessionKind, FileUploadSessionPart, FileUploadSessionRecord,
        StoredFileReference, UserId, FILE_CLEANUP_ORIGIN_MAX_CHARS, FILE_OBJECT_KEY_MAX_CHARS,
        FILE_REFERENCE_ID_MAX_CHARS, FILE_REFERENCE_KIND_MAX_CHARS, FILE_SHA256_HEX_CHARS,
        FILE_STORAGE_BACKEND_MAX_CHARS,
    },
    Error, Result,
};

fn scalar_value<T>(value: Option<T>, query_description: &str) -> Result<T> {
    value.ok_or_else(|| {
        Error::Internal(format!(
            "{query_description} query returned no scalar value"
        ))
    })
}

fn validate_required_text(value: &str, field: &str, max_chars: usize) -> Result<()> {
    let len = value.chars().count();
    if value.trim().is_empty() || len > max_chars {
        return Err(Error::InvalidInput(format!(
            "{field} must be between 1 and {max_chars} characters"
        )));
    }
    Ok(())
}

fn validate_optional_hex(value: Option<&str>, field: &str, expected_chars: usize) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let valid = value.len() == expected_chars && value.chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        return Err(Error::InvalidInput(format!(
            "file {field} must be a {expected_chars}-character hex string"
        )));
    }
    Ok(())
}

fn validate_required_sha256(value: &str, field: &str) -> Result<()> {
    let value = value.trim();
    let valid =
        value.len() == FILE_SHA256_HEX_CHARS && value.chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        return Err(Error::InvalidInput(format!(
            "file {field} must be a {FILE_SHA256_HEX_CHARS}-character hex string"
        )));
    }
    Ok(())
}

fn validate_file_object_fields(
    storage_backend: &str,
    object_key: &str,
    size_bytes: i64,
    content_manifest_sha256: &str,
    metadata: &serde_json::Value,
) -> Result<()> {
    validate_required_text(
        storage_backend,
        "file storage_backend",
        FILE_STORAGE_BACKEND_MAX_CHARS,
    )?;
    validate_required_text(object_key, "file object_key", FILE_OBJECT_KEY_MAX_CHARS)?;
    if size_bytes <= 0 {
        return Err(Error::InvalidInput(
            "file size_bytes must be positive".to_string(),
        ));
    }
    validate_required_sha256(content_manifest_sha256, "content_manifest_sha256")?;
    if !metadata.is_object() {
        return Err(Error::InvalidInput(
            "file metadata must be a JSON object".to_string(),
        ));
    }
    Ok(())
}

fn validate_file_reference_fields(
    storage_backend: &str,
    object_key: &str,
    reference_kind: &str,
    reference_id: &str,
    metadata: &serde_json::Value,
) -> Result<()> {
    validate_required_text(
        storage_backend,
        "file storage_backend",
        FILE_STORAGE_BACKEND_MAX_CHARS,
    )?;
    validate_required_text(object_key, "file object_key", FILE_OBJECT_KEY_MAX_CHARS)?;
    validate_required_text(
        reference_kind,
        "file reference_kind",
        FILE_REFERENCE_KIND_MAX_CHARS,
    )?;
    validate_required_text(
        reference_id,
        "file reference_id",
        FILE_REFERENCE_ID_MAX_CHARS,
    )?;
    if !metadata.is_object() {
        return Err(Error::InvalidInput(
            "file metadata must be a JSON object".to_string(),
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub struct FileStorageRepository {
    pool: PgPool,
}

pub struct UpsertFileBlob<'a> {
    pub storage_backend: &'a str,
    pub object_key: &'a str,
    pub mime_type: &'a str,
    pub size_bytes: i64,
    pub checksum_sha256: &'a str,
    pub compression: FileBlobCompression,
    pub data: Vec<u8>,
    pub metadata: &'a serde_json::Value,
}

pub struct UpsertFileBlobPart<'a> {
    pub storage_backend: &'a str,
    pub object_key: &'a str,
    pub part_index: i32,
    pub offset_bytes: i64,
    pub size_bytes: i64,
    pub checksum_sha256: &'a str,
    pub compression: FileBlobCompression,
    pub data: Vec<u8>,
}

pub struct UpsertFileObject<'a> {
    pub storage_backend: &'a str,
    pub object_key: &'a str,
    pub mime_type: &'a str,
    pub size_bytes: i64,
    pub content_manifest_sha256: &'a str,
    pub metadata: &'a serde_json::Value,
}

pub struct UpsertFileUploadSession<'a> {
    pub storage_backend: &'a str,
    pub upload_session_key: &'a str,
    pub object_key: &'a str,
    pub session_kind: FileUploadSessionKind,
    pub upload_id: Option<&'a str>,
    pub user_id: UserId,
    pub storage_scope: &'a str,
    pub mime_type: &'a str,
    pub size_bytes: i64,
    pub content_manifest_sha256: &'a str,
    pub part_size_bytes: i64,
    pub metadata: &'a serde_json::Value,
    pub expires_at: DateTime<Utc>,
}

pub struct UpsertFileUploadSessionPart<'a> {
    pub storage_backend: &'a str,
    pub upload_session_key: &'a str,
    pub part_index: i32,
    pub part_number: i32,
    pub offset_bytes: i64,
    pub size_bytes: i64,
    pub checksum_sha256: Option<&'a str>,
    pub etag: Option<&'a str>,
}

impl FileStorageRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn upsert_blob(&self, blob: UpsertFileBlob<'_>) -> Result<FileBlob> {
        self.upsert_object(UpsertFileObject {
            storage_backend: blob.storage_backend,
            object_key: blob.object_key,
            mime_type: blob.mime_type,
            size_bytes: blob.size_bytes,
            content_manifest_sha256: blob.checksum_sha256,
            metadata: blob.metadata,
        })
        .await?;
        self.delete_blob_parts(blob.storage_backend, blob.object_key)
            .await?;
        self.upsert_blob_part(UpsertFileBlobPart {
            storage_backend: blob.storage_backend,
            object_key: blob.object_key,
            part_index: 0,
            offset_bytes: 0,
            size_bytes: blob.size_bytes,
            checksum_sha256: blob.checksum_sha256,
            compression: blob.compression,
            data: blob.data.clone(),
        })
        .await?;
        Ok(FileBlob {
            storage_backend: blob.storage_backend.to_string(),
            object_key: blob.object_key.to_string(),
            mime_type: blob.mime_type.to_string(),
            size_bytes: blob.size_bytes,
            total_size_bytes: blob.size_bytes,
            content_manifest_sha256: blob.checksum_sha256.to_string(),
            compression: blob.compression,
            range: None,
            data: blob.data,
            metadata: blob.metadata.clone(),
            created_at: Utc::now(),
        })
    }

    pub async fn upsert_object(&self, object: UpsertFileObject<'_>) -> Result<FileObject> {
        validate_file_object_fields(
            object.storage_backend,
            object.object_key,
            object.size_bytes,
            object.content_manifest_sha256,
            object.metadata,
        )?;
        let row = sqlx::query(
            r"
            INSERT INTO file_objects (
                storage_backend, object_key, mime_type, size_bytes,
                content_manifest_sha256, metadata, validated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, CURRENT_TIMESTAMP)
            ON CONFLICT (storage_backend, object_key)
            DO UPDATE SET
                mime_type = EXCLUDED.mime_type,
                size_bytes = EXCLUDED.size_bytes,
                content_manifest_sha256 = EXCLUDED.content_manifest_sha256,
                metadata = EXCLUDED.metadata,
                validated_at = CURRENT_TIMESTAMP
            RETURNING storage_backend, object_key, mime_type, size_bytes,
                      content_manifest_sha256, metadata, created_at, validated_at
            ",
        )
        .bind(object.storage_backend)
        .bind(object.object_key)
        .bind(object.mime_type)
        .bind(object.size_bytes)
        .bind(object.content_manifest_sha256.trim().to_ascii_lowercase())
        .bind(object.metadata)
        .fetch_one(&self.pool)
        .await?;
        file_object_from_row(&row)
    }

    pub async fn upsert_pending_object(&self, object: UpsertFileObject<'_>) -> Result<FileObject> {
        validate_file_object_fields(
            object.storage_backend,
            object.object_key,
            object.size_bytes,
            object.content_manifest_sha256,
            object.metadata,
        )?;
        let row = sqlx::query(
            r"
            INSERT INTO file_objects (
                storage_backend, object_key, mime_type, size_bytes,
                content_manifest_sha256, metadata, validated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, NULL)
            ON CONFLICT (storage_backend, object_key)
            DO UPDATE SET
                mime_type = EXCLUDED.mime_type,
                size_bytes = EXCLUDED.size_bytes,
                content_manifest_sha256 = EXCLUDED.content_manifest_sha256,
                metadata = EXCLUDED.metadata,
                validated_at = NULL
            RETURNING storage_backend, object_key, mime_type, size_bytes,
                      content_manifest_sha256, metadata, created_at, validated_at
            ",
        )
        .bind(object.storage_backend)
        .bind(object.object_key)
        .bind(object.mime_type)
        .bind(object.size_bytes)
        .bind(object.content_manifest_sha256.trim().to_ascii_lowercase())
        .bind(object.metadata)
        .fetch_one(&self.pool)
        .await?;
        file_object_from_row(&row)
    }

    pub async fn get_object(
        &self,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<Option<FileObject>> {
        let row = sqlx::query(
            r"
            SELECT storage_backend, object_key, mime_type, size_bytes,
                   content_manifest_sha256, metadata, created_at, validated_at
            FROM file_objects
            WHERE storage_backend = $1 AND object_key = $2
            ",
        )
        .bind(storage_backend)
        .bind(object_key)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(file_object_from_row).transpose()
    }

    pub async fn get_object_by_manifest(
        &self,
        storage_backend: &str,
        content_manifest_sha256: &str,
        size_bytes: i64,
    ) -> Result<Option<FileObject>> {
        validate_required_sha256(content_manifest_sha256, "content_manifest_sha256")?;
        let row = sqlx::query(
            r"
            SELECT storage_backend, object_key, mime_type, size_bytes,
                   content_manifest_sha256, metadata, created_at, validated_at
            FROM file_objects
            WHERE storage_backend = $1
              AND content_manifest_sha256 = $2
              AND size_bytes = $3
              AND validated_at IS NOT NULL
            ORDER BY created_at ASC
            LIMIT 1
            ",
        )
        .bind(storage_backend)
        .bind(content_manifest_sha256.trim().to_ascii_lowercase())
        .bind(size_bytes)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(file_object_from_row).transpose()
    }

    pub async fn get_any_object_by_manifest(
        &self,
        storage_backend: &str,
        content_manifest_sha256: &str,
        size_bytes: i64,
    ) -> Result<Option<FileObject>> {
        validate_required_sha256(content_manifest_sha256, "content_manifest_sha256")?;
        let row = sqlx::query(
            r"
            SELECT storage_backend, object_key, mime_type, size_bytes,
                   content_manifest_sha256, metadata, created_at, validated_at
            FROM file_objects
            WHERE storage_backend = $1
              AND content_manifest_sha256 = $2
              AND size_bytes = $3
            ORDER BY validated_at DESC NULLS LAST, created_at ASC
            LIMIT 1
            ",
        )
        .bind(storage_backend)
        .bind(content_manifest_sha256.trim().to_ascii_lowercase())
        .bind(size_bytes)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(file_object_from_row).transpose()
    }

    pub async fn get_pending_upload_session_by_manifest(
        &self,
        storage_backend: &str,
        user_id: UserId,
        storage_scope: &str,
        content_manifest_sha256: &str,
        size_bytes: i64,
    ) -> Result<Option<FileUploadSessionRecord>> {
        validate_required_text(storage_scope, "file storage_scope", 512)?;
        validate_required_sha256(content_manifest_sha256, "content_manifest_sha256")?;
        let row = sqlx::query(
            r"
            SELECT storage_backend, upload_session_key, object_key, session_kind, upload_id, user_id,
                   storage_scope, mime_type, size_bytes, content_manifest_sha256,
                   part_size_bytes, metadata,
                   expires_at, completed_at, created_at, updated_at
            FROM file_upload_sessions
            WHERE storage_backend = $1
              AND user_id = $2
              AND storage_scope = $3
              AND content_manifest_sha256 = $4
              AND size_bytes = $5
              AND completed_at IS NULL
              AND expires_at > CURRENT_TIMESTAMP
            ORDER BY updated_at DESC
            LIMIT 1
            ",
        )
        .bind(storage_backend)
        .bind(user_id.as_i64())
        .bind(storage_scope)
        .bind(content_manifest_sha256.trim().to_ascii_lowercase())
        .bind(size_bytes)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(file_upload_session_from_row).transpose()
    }

    pub async fn get_pending_upload_session_by_object_key(
        &self,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<Option<FileUploadSessionRecord>> {
        let row = sqlx::query(
            r"
            SELECT storage_backend, upload_session_key, object_key, session_kind, upload_id, user_id,
                   storage_scope, mime_type, size_bytes, content_manifest_sha256,
                   part_size_bytes, metadata,
                   expires_at, completed_at, created_at, updated_at
            FROM file_upload_sessions
            WHERE storage_backend = $1
              AND object_key = $2
              AND completed_at IS NULL
              AND expires_at > CURRENT_TIMESTAMP
            ORDER BY updated_at DESC
            LIMIT 1
            ",
        )
        .bind(storage_backend)
        .bind(object_key)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(file_upload_session_from_row).transpose()
    }

    pub async fn object_exists(&self, storage_backend: &str, object_key: &str) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM file_objects WHERE storage_backend = $1 AND object_key = $2)",
            storage_backend,
            object_key,
        )
        .fetch_one(&self.pool)
        .await?;
        scalar_value(exists, "file object EXISTS")
    }

    pub async fn object_validated(&self, storage_backend: &str, object_key: &str) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM file_objects
                WHERE storage_backend = $1
                  AND object_key = $2
                  AND validated_at IS NOT NULL
            )
            "#,
            storage_backend,
            object_key,
        )
        .fetch_one(&self.pool)
        .await?;
        scalar_value(exists, "file object validation EXISTS")
    }

    pub async fn object_reference_count(
        &self,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*)::BIGINT
            FROM file_references
            WHERE storage_backend = $1 AND object_key = $2
              AND released_at IS NULL
            "#,
            storage_backend,
            object_key,
        )
        .fetch_one(&self.pool)
        .await?;
        scalar_value(count, "file reference COUNT")
    }

    pub async fn insert_reference_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        storage_backend: &str,
        object_key: &str,
        reference_kind: &str,
        reference_id: &str,
        expires_at: Option<DateTime<Utc>>,
        metadata: &serde_json::Value,
    ) -> Result<Option<i64>> {
        validate_file_reference_fields(
            storage_backend,
            object_key,
            reference_kind,
            reference_id,
            metadata,
        )?;
        let object_registered = sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM file_objects WHERE storage_backend = $1 AND object_key = $2)",
            storage_backend,
            object_key,
        )
        .fetch_one(&mut **tx)
        .await?;
        let object_registered = object_registered.ok_or_else(|| {
            Error::Internal("file object registration EXISTS query returned NULL".to_string())
        })?;
        if !object_registered {
            return Ok(None);
        }
        let reference_id_row = sqlx::query!(
            r#"
            INSERT INTO file_references (
                storage_backend, object_key, reference_kind, reference_id, expires_at, metadata
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (reference_kind, reference_id, storage_backend, object_key)
            DO UPDATE SET
                expires_at = EXCLUDED.expires_at,
                released_at = NULL,
                metadata = EXCLUDED.metadata,
                updated_at = CURRENT_TIMESTAMP
            RETURNING id
            "#,
            storage_backend,
            object_key,
            reference_kind,
            reference_id,
            expires_at,
            metadata,
        )
        .fetch_one(&mut **tx)
        .await?;
        Ok(Some(reference_id_row.id))
    }

    pub async fn insert_reference(
        &self,
        storage_backend: &str,
        object_key: &str,
        reference_kind: &str,
        reference_id: &str,
        expires_at: Option<DateTime<Utc>>,
        metadata: &serde_json::Value,
    ) -> Result<Option<i64>> {
        let mut tx = self.pool.begin().await?;
        let id = Self::insert_reference_in_tx(
            &mut tx,
            storage_backend,
            object_key,
            reference_kind,
            reference_id,
            expires_at,
            metadata,
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn get_reference_by_id(&self, id: i64) -> Result<Option<StoredFileReference>> {
        let row = sqlx::query(
            r"
            SELECT r.id AS file_reference_id,
                   o.storage_backend,
                   o.object_key,
                   o.mime_type,
                   o.size_bytes,
                   o.content_manifest_sha256,
                   r.metadata,
                   o.created_at,
                   o.validated_at
            FROM file_references r
            JOIN file_objects o
              ON o.storage_backend = r.storage_backend
             AND o.object_key = r.object_key
            WHERE r.id = $1
              AND r.released_at IS NULL
            ",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(stored_file_reference_from_row).transpose()
    }

    pub async fn get_upload_session_by_reference(
        &self,
        reference_kind: &str,
        reference_id: &str,
    ) -> Result<Option<FileUploadSessionRecord>> {
        let row = sqlx::query(
            r"
            SELECT r.storage_backend,
                   COALESCE(s.upload_session_key, r.object_key) AS upload_session_key,
                   r.object_key,
                   COALESCE(s.session_kind, 1::SMALLINT) AS session_kind,
                   s.upload_id,
                   COALESCE(s.user_id, (r.metadata->>'user_id')::BIGINT) AS user_id,
                   COALESCE(s.storage_scope, r.metadata->>'storage_scope') AS storage_scope,
                   COALESCE(s.mime_type, o.mime_type) AS mime_type,
                   COALESCE(s.size_bytes, o.size_bytes) AS size_bytes,
                   COALESCE(s.content_manifest_sha256, o.content_manifest_sha256) AS content_manifest_sha256,
                   COALESCE(s.part_size_bytes, o.size_bytes) AS part_size_bytes,
                   r.metadata,
                   COALESCE(s.expires_at, r.expires_at) AS expires_at,
                   s.completed_at,
                   COALESCE(s.created_at, r.created_at) AS created_at,
                   COALESCE(s.updated_at, r.updated_at) AS updated_at
            FROM file_references r
            JOIN file_objects o
              ON o.storage_backend = r.storage_backend
             AND o.object_key = r.object_key
            LEFT JOIN file_upload_sessions s
              ON s.storage_backend = r.storage_backend
             AND s.object_key = r.object_key
             AND (s.metadata->>'public_file_id') = r.reference_id
            WHERE r.reference_kind = $1
              AND r.reference_id = $2
              AND r.released_at IS NULL
            ORDER BY r.updated_at DESC, r.id DESC
            LIMIT 1
            ",
        )
        .bind(reference_kind)
        .bind(reference_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(file_upload_session_from_row).transpose()
    }

    pub async fn get_active_reference_by_target(
        &self,
        reference_kind: &str,
        reference_id: &str,
    ) -> Result<Option<StoredFileReference>> {
        let row = sqlx::query(
            r"
            SELECT r.id AS file_reference_id,
                   o.storage_backend,
                   o.object_key,
                   o.mime_type,
                   o.size_bytes,
                   o.content_manifest_sha256,
                   r.metadata,
                   o.created_at,
                   o.validated_at
            FROM file_references r
            JOIN file_objects o
              ON o.storage_backend = r.storage_backend
             AND o.object_key = r.object_key
            WHERE r.reference_kind = $1
              AND r.reference_id = $2
              AND r.released_at IS NULL
            ORDER BY r.updated_at DESC, r.id DESC
            LIMIT 1
            ",
        )
        .bind(reference_kind)
        .bind(reference_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(stored_file_reference_from_row).transpose()
    }

    pub async fn release_reference(
        &self,
        reference_kind: &str,
        reference_id: &str,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<()> {
        sqlx::query!(
            r#"
            UPDATE file_references
            SET released_at = COALESCE(released_at, CURRENT_TIMESTAMP),
                updated_at = CURRENT_TIMESTAMP
            WHERE reference_kind = $1
              AND reference_id = $2
              AND storage_backend = $3
              AND object_key = $4
            "#,
            reference_kind,
            reference_id,
            storage_backend,
            object_key,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_reference_metadata(
        &self,
        reference_kind: &str,
        reference_id: &str,
        storage_backend: &str,
        object_key: &str,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        validate_file_reference_fields(
            storage_backend,
            object_key,
            reference_kind,
            reference_id,
            metadata,
        )?;
        sqlx::query(
            r"
            UPDATE file_references
            SET metadata = $5,
                updated_at = CURRENT_TIMESTAMP
            WHERE reference_kind = $1
              AND reference_id = $2
              AND storage_backend = $3
              AND object_key = $4
              AND released_at IS NULL
            ",
        )
        .bind(reference_kind)
        .bind(reference_id)
        .bind(storage_backend)
        .bind(object_key)
        .bind(metadata)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn blob_exists(&self, storage_backend: &str, object_key: &str) -> Result<bool> {
        let exists: Option<bool> = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM file_blob_parts WHERE storage_backend = $1 AND object_key = $2)",
        )
        .bind(storage_backend)
        .bind(object_key)
        .fetch_one(&self.pool)
        .await?;
        scalar_value(exists, "file blob part EXISTS")
    }

    pub async fn get_blob(
        &self,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<Option<FileBlob>> {
        let Some(object) = self.get_object(storage_backend, object_key).await? else {
            return Ok(None);
        };
        let parts = self.list_blob_parts(storage_backend, object_key).await?;
        if parts.is_empty() {
            return Ok(None);
        }
        let compression = parts
            .first()
            .map_or(FileBlobCompression::None, |part| part.compression);
        let mut data = Vec::new();
        for part in parts {
            data.extend_from_slice(&part.data);
        }
        Ok(Some(FileBlob {
            storage_backend: object.storage_backend,
            object_key: object.object_key,
            mime_type: object.mime_type,
            size_bytes: object.size_bytes,
            total_size_bytes: object.size_bytes,
            content_manifest_sha256: object.content_manifest_sha256,
            compression,
            range: None,
            data,
            metadata: object.metadata,
            created_at: object.created_at,
        }))
    }

    pub async fn delete_blob(&self, storage_backend: &str, object_key: &str) -> Result<bool> {
        Ok(self.delete_blob_parts(storage_backend, object_key).await? > 0)
    }

    pub async fn upsert_blob_part(&self, part: UpsertFileBlobPart<'_>) -> Result<FileBlobPart> {
        validate_file_object_fields(
            part.storage_backend,
            part.object_key,
            part.size_bytes,
            part.checksum_sha256,
            &serde_json::Value::Object(Default::default()),
        )?;
        if part.part_index < 0 || part.offset_bytes < 0 {
            return Err(Error::InvalidInput(
                "file blob part index and offset must be non-negative".to_string(),
            ));
        }
        let row = sqlx::query(
            r"
            INSERT INTO file_blob_parts (
                storage_backend, object_key, part_index, offset_bytes,
                size_bytes, checksum_sha256, compression, data
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (storage_backend, object_key, part_index)
            DO UPDATE SET
                offset_bytes = EXCLUDED.offset_bytes,
                size_bytes = EXCLUDED.size_bytes,
                checksum_sha256 = EXCLUDED.checksum_sha256,
                compression = EXCLUDED.compression,
                data = EXCLUDED.data
            RETURNING storage_backend, object_key, part_index, offset_bytes,
                      size_bytes, checksum_sha256, compression, data, created_at
            ",
        )
        .bind(part.storage_backend)
        .bind(part.object_key)
        .bind(part.part_index)
        .bind(part.offset_bytes)
        .bind(part.size_bytes)
        .bind(part.checksum_sha256.trim().to_ascii_lowercase())
        .bind(i16::from(part.compression))
        .bind(part.data)
        .fetch_one(&self.pool)
        .await?;
        file_blob_part_from_row(&row)
    }

    pub async fn list_blob_parts(
        &self,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<Vec<FileBlobPart>> {
        let rows = sqlx::query(
            r"
            SELECT storage_backend, object_key, part_index, offset_bytes,
                   size_bytes, checksum_sha256, compression, data, created_at
            FROM file_blob_parts
            WHERE storage_backend = $1 AND object_key = $2
            ORDER BY offset_bytes ASC, part_index ASC
            ",
        )
        .bind(storage_backend)
        .bind(object_key)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(file_blob_part_from_row).collect()
    }

    pub async fn list_blob_parts_overlapping_range(
        &self,
        storage_backend: &str,
        object_key: &str,
        start: i64,
        end_inclusive: i64,
    ) -> Result<Vec<FileBlobPart>> {
        if start < 0 || end_inclusive < start {
            return Err(Error::InvalidInput("file range is invalid".to_string()));
        }
        let rows = sqlx::query(
            r"
            SELECT storage_backend, object_key, part_index, offset_bytes,
                   size_bytes, checksum_sha256, compression, data, created_at
            FROM file_blob_parts
            WHERE storage_backend = $1
              AND object_key = $2
              AND offset_bytes <= $4
              AND offset_bytes + size_bytes > $3
            ORDER BY offset_bytes ASC, part_index ASC
            ",
        )
        .bind(storage_backend)
        .bind(object_key)
        .bind(start)
        .bind(end_inclusive)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(file_blob_part_from_row).collect()
    }

    pub async fn delete_blob_parts(&self, storage_backend: &str, object_key: &str) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM file_blob_parts WHERE storage_backend = $1 AND object_key = $2",
        )
        .bind(storage_backend)
        .bind(object_key)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn promote_blob_parts(
        &self,
        storage_backend: &str,
        source_object_key: &str,
        destination_object_key: &str,
        expected_source_part_count: usize,
    ) -> Result<u64> {
        validate_required_text(
            storage_backend,
            "file storage_backend",
            FILE_STORAGE_BACKEND_MAX_CHARS,
        )?;
        validate_required_text(
            source_object_key,
            "file source object_key",
            FILE_OBJECT_KEY_MAX_CHARS,
        )?;
        validate_required_text(
            destination_object_key,
            "file destination object_key",
            FILE_OBJECT_KEY_MAX_CHARS,
        )?;
        if source_object_key == destination_object_key {
            return Ok(0);
        }
        let expected_source_part_count = i64::try_from(expected_source_part_count)
            .map_err(|_| Error::InvalidInput("file upload has too many parts".to_string()))?;

        let mut transaction = self.pool.begin().await?;
        let locked_objects = sqlx::query(
            r"
            SELECT object_key
            FROM file_objects
            WHERE storage_backend = $1 AND object_key IN ($2, $3)
            ORDER BY object_key
            FOR UPDATE
            ",
        )
        .bind(storage_backend)
        .bind(source_object_key)
        .bind(destination_object_key)
        .fetch_all(&mut *transaction)
        .await?;
        if locked_objects.len() != 2 {
            transaction.rollback().await?;
            return Err(Error::Internal(
                "file blob promotion requires source and destination objects".to_string(),
            ));
        }

        let existing_destination_parts = sqlx::query_scalar::<_, i64>(
            r"
            SELECT COUNT(*)::BIGINT
            FROM file_blob_parts
            WHERE storage_backend = $1 AND object_key = $2
            ",
        )
        .bind(storage_backend)
        .bind(destination_object_key)
        .fetch_one(&mut *transaction)
        .await?;
        if existing_destination_parts > 0 {
            let deleted = sqlx::query(
                "DELETE FROM file_blob_parts WHERE storage_backend = $1 AND object_key = $2",
            )
            .bind(storage_backend)
            .bind(source_object_key)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
            transaction.commit().await?;
            return Ok(deleted);
        }

        let source_parts = sqlx::query_scalar::<_, i64>(
            r"
            SELECT COUNT(*)::BIGINT
            FROM file_blob_parts
            WHERE storage_backend = $1 AND object_key = $2
            ",
        )
        .bind(storage_backend)
        .bind(source_object_key)
        .fetch_one(&mut *transaction)
        .await?;
        if source_parts != expected_source_part_count {
            transaction.rollback().await?;
            return Err(Error::InvalidInput(
                "file upload is missing one or more blob parts".to_string(),
            ));
        }

        let promoted = sqlx::query(
            r"
            UPDATE file_blob_parts
            SET object_key = $3
            WHERE storage_backend = $1 AND object_key = $2
            ",
        )
        .bind(storage_backend)
        .bind(source_object_key)
        .bind(destination_object_key)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if promoted
            != u64::try_from(expected_source_part_count)
                .map_err(|_| Error::InvalidInput("file upload has too many parts".to_string()))?
        {
            transaction.rollback().await?;
            return Err(Error::InvalidInput(
                "file upload is missing one or more blob parts".to_string(),
            ));
        }
        transaction.commit().await?;
        Ok(promoted)
    }

    pub async fn upsert_upload_session(
        &self,
        session: UpsertFileUploadSession<'_>,
    ) -> Result<FileUploadSessionRecord> {
        validate_file_object_fields(
            session.storage_backend,
            session.object_key,
            session.size_bytes,
            session.content_manifest_sha256,
            session.metadata,
        )?;
        validate_required_text(session.storage_scope, "file storage_scope", 512)?;
        if session.part_size_bytes <= 0 {
            return Err(Error::InvalidInput(
                "file upload part_size_bytes must be positive".to_string(),
            ));
        }
        let row = sqlx::query(
            r"
            INSERT INTO file_upload_sessions (
                storage_backend, upload_session_key, object_key, session_kind, upload_id, user_id,
                storage_scope, mime_type, size_bytes, content_manifest_sha256,
                part_size_bytes, metadata, expires_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            ON CONFLICT (storage_backend, upload_session_key)
            DO UPDATE SET
                object_key = EXCLUDED.object_key,
                session_kind = EXCLUDED.session_kind,
                upload_id = EXCLUDED.upload_id,
                user_id = EXCLUDED.user_id,
                storage_scope = EXCLUDED.storage_scope,
                mime_type = EXCLUDED.mime_type,
                size_bytes = EXCLUDED.size_bytes,
                content_manifest_sha256 = EXCLUDED.content_manifest_sha256,
                part_size_bytes = EXCLUDED.part_size_bytes,
                metadata = EXCLUDED.metadata,
                expires_at = EXCLUDED.expires_at,
                completed_at = NULL,
                updated_at = CURRENT_TIMESTAMP
            RETURNING storage_backend, upload_session_key, object_key, session_kind, upload_id, user_id,
                      storage_scope, mime_type, size_bytes, content_manifest_sha256,
                      part_size_bytes, metadata,
                      expires_at, completed_at, created_at, updated_at
            ",
        )
        .bind(session.storage_backend)
        .bind(session.upload_session_key)
        .bind(session.object_key)
        .bind(i16::from(session.session_kind))
        .bind(session.upload_id)
        .bind(session.user_id.as_i64())
        .bind(session.storage_scope)
        .bind(session.mime_type)
        .bind(session.size_bytes)
        .bind(session.content_manifest_sha256.trim().to_ascii_lowercase())
        .bind(session.part_size_bytes)
        .bind(session.metadata)
        .bind(session.expires_at)
        .fetch_one(&self.pool)
        .await?;
        file_upload_session_from_row(&row)
    }

    pub async fn get_upload_session(
        &self,
        storage_backend: &str,
        upload_session_key: &str,
    ) -> Result<Option<FileUploadSessionRecord>> {
        let row = sqlx::query(
            r"
            SELECT storage_backend, upload_session_key, object_key, session_kind, upload_id, user_id,
                   storage_scope, mime_type, size_bytes, content_manifest_sha256,
                   part_size_bytes, metadata,
                   expires_at, completed_at, created_at, updated_at
            FROM file_upload_sessions
            WHERE storage_backend = $1 AND upload_session_key = $2
            ",
        )
        .bind(storage_backend)
        .bind(upload_session_key)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(file_upload_session_from_row).transpose()
    }

    pub async fn complete_upload_session(
        &self,
        storage_backend: &str,
        upload_session_key: &str,
    ) -> Result<()> {
        sqlx::query(
            r"
            UPDATE file_upload_sessions
            SET completed_at = CURRENT_TIMESTAMP,
                updated_at = CURRENT_TIMESTAMP
            WHERE storage_backend = $1 AND upload_session_key = $2
            ",
        )
        .bind(storage_backend)
        .bind(upload_session_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_upload_session_metadata(
        &self,
        storage_backend: &str,
        upload_session_key: &str,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        if !metadata.is_object() {
            return Err(Error::InvalidInput(
                "file upload session metadata must be a JSON object".to_string(),
            ));
        }
        sqlx::query(
            r"
            UPDATE file_upload_sessions
            SET metadata = $3,
                updated_at = CURRENT_TIMESTAMP
            WHERE storage_backend = $1 AND upload_session_key = $2
            ",
        )
        .bind(storage_backend)
        .bind(upload_session_key)
        .bind(metadata)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_upload_session_part(
        &self,
        part: UpsertFileUploadSessionPart<'_>,
    ) -> Result<FileUploadSessionPart> {
        if part.part_index < 0 || part.part_number <= 0 || part.offset_bytes < 0 {
            return Err(Error::InvalidInput(
                "file upload session part index, number, and offset are invalid".to_string(),
            ));
        }
        if part.size_bytes <= 0 {
            return Err(Error::InvalidInput(
                "file upload session part size_bytes must be positive".to_string(),
            ));
        }
        validate_optional_hex(
            part.checksum_sha256,
            "checksum_sha256",
            FILE_SHA256_HEX_CHARS,
        )?;
        let row = sqlx::query(
            r"
            INSERT INTO file_upload_session_parts (
                storage_backend, upload_session_key, part_index, part_number, offset_bytes,
                size_bytes, checksum_sha256, etag
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (storage_backend, upload_session_key, part_index)
            DO UPDATE SET
                part_number = EXCLUDED.part_number,
                offset_bytes = EXCLUDED.offset_bytes,
                size_bytes = EXCLUDED.size_bytes,
                checksum_sha256 = EXCLUDED.checksum_sha256,
                etag = EXCLUDED.etag,
                updated_at = CURRENT_TIMESTAMP
            RETURNING storage_backend, upload_session_key, part_index, part_number,
                      offset_bytes, size_bytes, checksum_sha256, etag,
                      created_at, updated_at
            ",
        )
        .bind(part.storage_backend)
        .bind(part.upload_session_key)
        .bind(part.part_index)
        .bind(part.part_number)
        .bind(part.offset_bytes)
        .bind(part.size_bytes)
        .bind(part.checksum_sha256.map(str::to_ascii_lowercase))
        .bind(part.etag)
        .fetch_one(&self.pool)
        .await?;
        file_upload_session_part_from_row(&row)
    }

    pub async fn list_upload_session_parts(
        &self,
        storage_backend: &str,
        upload_session_key: &str,
    ) -> Result<Vec<FileUploadSessionPart>> {
        let rows = sqlx::query(
            r"
            SELECT storage_backend, upload_session_key, part_index, part_number,
                   offset_bytes, size_bytes, checksum_sha256, etag,
                   created_at, updated_at
            FROM file_upload_session_parts
            WHERE storage_backend = $1 AND upload_session_key = $2
            ORDER BY offset_bytes ASC, part_index ASC
            ",
        )
        .bind(storage_backend)
        .bind(upload_session_key)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(file_upload_session_part_from_row).collect()
    }

    pub async fn delete_upload_session_parts(
        &self,
        storage_backend: &str,
        upload_session_key: &str,
    ) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM file_upload_session_parts WHERE storage_backend = $1 AND upload_session_key = $2",
        )
        .bind(storage_backend)
        .bind(upload_session_key)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn delete_object(&self, storage_backend: &str, object_key: &str) -> Result<bool> {
        let result = sqlx::query!(
            "DELETE FROM file_objects WHERE storage_backend = $1 AND object_key = $2",
            storage_backend,
            object_key,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn list_unreferenced_objects(
        &self,
        older_than_seconds: i64,
        limit: i64,
    ) -> Result<Vec<FileObject>> {
        let rows = sqlx::query(
            r"
            SELECT o.storage_backend, o.object_key, o.mime_type, o.size_bytes,
                   o.content_manifest_sha256, o.metadata, o.created_at, o.validated_at
            FROM file_objects o
            WHERE o.created_at < CURRENT_TIMESTAMP - ($1::BIGINT * INTERVAL '1 second')
              AND NOT EXISTS (
                  SELECT 1
                  FROM file_references r
                  WHERE r.storage_backend = o.storage_backend
                    AND r.object_key = o.object_key
                    AND r.released_at IS NULL
              )
            ORDER BY o.created_at ASC, o.storage_backend ASC, o.object_key ASC
            LIMIT $2
            ",
        )
        .bind(older_than_seconds.max(1))
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(file_object_from_row).collect()
    }

    pub async fn list_expired_references(&self, limit: i64) -> Result<Vec<FileReferenceTarget>> {
        let rows = sqlx::query!(
            r#"
            SELECT storage_backend, object_key, reference_kind, reference_id
            FROM file_references
            WHERE released_at IS NULL
              AND expires_at IS NOT NULL
              AND expires_at <= CURRENT_TIMESTAMP
            ORDER BY expires_at ASC, id ASC
            LIMIT $1
            "#,
            limit.clamp(1, 1000),
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| FileReferenceTarget {
                storage_backend: row.storage_backend,
                object_key: row.object_key,
                reference_kind: row.reference_kind,
                reference_id: row.reference_id,
            })
            .collect())
    }

    pub async fn enqueue_cleanup_jobs(
        &self,
        origin: &str,
        files: &[FileReferenceTarget],
        metadata: &serde_json::Value,
        error: &str,
    ) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        validate_required_text(origin, "file cleanup origin", FILE_CLEANUP_ORIGIN_MAX_CHARS)?;
        if !metadata.is_object() {
            return Err(Error::InvalidInput(
                "file cleanup metadata must be a JSON object".to_string(),
            ));
        }

        let mut tx = self.pool.begin().await?;
        for file in files {
            validate_file_reference_fields(
                &file.storage_backend,
                &file.object_key,
                &file.reference_kind,
                &file.reference_id,
                metadata,
            )?;
            sqlx::query!(
                r#"
                INSERT INTO file_cleanup_jobs (
                    origin, storage_backend, object_key, reference_kind,
                    reference_id, metadata, last_error
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (reference_kind, reference_id, storage_backend, object_key)
                DO UPDATE SET
                    origin = EXCLUDED.origin,
                    metadata = EXCLUDED.metadata,
                    last_error = EXCLUDED.last_error,
                    next_attempt_at = CURRENT_TIMESTAMP,
                    locked_at = NULL,
                    locked_by = NULL,
                    completed_at = NULL,
                    updated_at = CURRENT_TIMESTAMP
                "#,
                origin,
                &file.storage_backend,
                &file.object_key,
                &file.reference_kind,
                &file.reference_id,
                metadata,
                error,
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn claim_due_cleanup_jobs(
        &self,
        limit: i64,
        locked_by: &str,
    ) -> Result<Vec<FileCleanupJob>> {
        let jobs = sqlx::query_as!(
            FileCleanupJob,
            r#"
            WITH candidates AS (
                SELECT id
                FROM file_cleanup_jobs
                WHERE completed_at IS NULL
                  AND next_attempt_at <= CURRENT_TIMESTAMP
                  AND (
                      locked_at IS NULL
                      OR locked_at < CURRENT_TIMESTAMP - INTERVAL '15 minutes'
                  )
                ORDER BY next_attempt_at ASC, id ASC
                LIMIT $1
                FOR UPDATE SKIP LOCKED
            )
            UPDATE file_cleanup_jobs j
            SET locked_at = CURRENT_TIMESTAMP,
                locked_by = $2,
                updated_at = CURRENT_TIMESTAMP
            FROM candidates c
            WHERE j.id = c.id
            RETURNING j.id, j.origin, j.storage_backend, j.object_key,
                      j.reference_kind, j.reference_id, j.metadata as "metadata: serde_json::Value", j.attempt_count,
                      j.last_error, j.next_attempt_at, j.locked_at, j.locked_by,
                      j.completed_at, j.created_at, j.updated_at
            "#,
            limit.max(0),
            locked_by,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(jobs)
    }

    pub async fn count_due_cleanup_jobs(&self) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*)::BIGINT
            FROM file_cleanup_jobs
            WHERE completed_at IS NULL
              AND next_attempt_at <= CURRENT_TIMESTAMP
              AND (
                  locked_at IS NULL
                  OR locked_at < CURRENT_TIMESTAMP - INTERVAL '15 minutes'
              )
            "#
        )
        .fetch_one(&self.pool)
        .await?;
        scalar_value(count, "file cleanup job COUNT")
    }

    pub async fn complete_cleanup_job(&self, job_id: i64) -> Result<()> {
        sqlx::query!(
            r#"
            UPDATE file_cleanup_jobs
            SET completed_at = CURRENT_TIMESTAMP,
                locked_at = NULL,
                locked_by = NULL,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = $1
            "#,
            job_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn reschedule_cleanup_job(
        &self,
        job_id: i64,
        error: &str,
        delay_seconds: i64,
    ) -> Result<()> {
        sqlx::query!(
            r#"
            UPDATE file_cleanup_jobs
            SET attempt_count = attempt_count + 1,
                last_error = $2,
                next_attempt_at = CURRENT_TIMESTAMP + ($3::BIGINT * INTERVAL '1 second'),
                locked_at = NULL,
                locked_by = NULL,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = $1
            "#,
            job_id,
            error,
            delay_seconds.max(1),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn file_object_from_row(row: &sqlx::postgres::PgRow) -> Result<FileObject> {
    Ok(FileObject {
        storage_backend: row.try_get("storage_backend")?,
        object_key: row.try_get("object_key")?,
        mime_type: row.try_get("mime_type")?,
        size_bytes: row.try_get("size_bytes")?,
        content_manifest_sha256: row.try_get("content_manifest_sha256")?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        validated_at: row.try_get("validated_at")?,
    })
}

fn stored_file_reference_from_row(row: &sqlx::postgres::PgRow) -> Result<StoredFileReference> {
    Ok(StoredFileReference {
        file_reference_id: row.try_get("file_reference_id")?,
        storage_backend: row.try_get("storage_backend")?,
        object_key: row.try_get("object_key")?,
        mime_type: row.try_get("mime_type")?,
        size_bytes: row.try_get("size_bytes")?,
        content_manifest_sha256: row.try_get("content_manifest_sha256")?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        validated_at: row.try_get("validated_at")?,
    })
}

fn file_blob_part_from_row(row: &sqlx::postgres::PgRow) -> Result<FileBlobPart> {
    let compression = row.try_get::<i16, _>("compression")?;
    let compression = FileBlobCompression::try_from(compression).map_err(|()| {
        Error::Internal(format!("unknown file blob compression value {compression}"))
    })?;
    Ok(FileBlobPart {
        storage_backend: row.try_get("storage_backend")?,
        object_key: row.try_get("object_key")?,
        part_index: row.try_get("part_index")?,
        offset_bytes: row.try_get("offset_bytes")?,
        size_bytes: row.try_get("size_bytes")?,
        checksum_sha256: row.try_get("checksum_sha256")?,
        compression,
        data: row.try_get("data")?,
        created_at: row.try_get("created_at")?,
    })
}

fn file_upload_session_from_row(row: &sqlx::postgres::PgRow) -> Result<FileUploadSessionRecord> {
    let session_kind = row.try_get::<i16, _>("session_kind")?;
    let session_kind = FileUploadSessionKind::try_from(session_kind).map_err(|()| {
        Error::Internal(format!("unknown file upload session kind {session_kind}"))
    })?;
    Ok(FileUploadSessionRecord {
        storage_backend: row.try_get("storage_backend")?,
        upload_session_key: row.try_get("upload_session_key")?,
        object_key: row.try_get("object_key")?,
        session_kind,
        upload_id: row.try_get("upload_id")?,
        user_id: row.try_get("user_id")?,
        storage_scope: row.try_get("storage_scope")?,
        mime_type: row.try_get("mime_type")?,
        size_bytes: row.try_get("size_bytes")?,
        content_manifest_sha256: row.try_get("content_manifest_sha256")?,
        part_size_bytes: row.try_get("part_size_bytes")?,
        metadata: row.try_get("metadata")?,
        expires_at: row.try_get("expires_at")?,
        completed_at: row.try_get("completed_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn file_upload_session_part_from_row(row: &sqlx::postgres::PgRow) -> Result<FileUploadSessionPart> {
    Ok(FileUploadSessionPart {
        storage_backend: row.try_get("storage_backend")?,
        upload_session_key: row.try_get("upload_session_key")?,
        part_index: row.try_get("part_index")?,
        part_number: row.try_get("part_number")?,
        offset_bytes: row.try_get("offset_bytes")?,
        size_bytes: row.try_get("size_bytes")?,
        checksum_sha256: row.try_get("checksum_sha256")?,
        etag: row.try_get("etag")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_metadata() -> serde_json::Value {
        serde_json::json!({})
    }

    fn checksum() -> String {
        "a".repeat(FILE_SHA256_HEX_CHARS)
    }

    #[test]
    fn file_object_validation_rejects_business_field_policy_violations() {
        let metadata = object_metadata();
        assert!(matches!(
            validate_file_object_fields("", "object", 1, &checksum(), &metadata),
            Err(Error::InvalidInput(message)) if message.contains("storage_backend")
        ));
        assert!(matches!(
            validate_file_object_fields(
                "backend",
                &"k".repeat(FILE_OBJECT_KEY_MAX_CHARS + 1),
                1,
                &checksum(),
                &metadata,
            ),
            Err(Error::InvalidInput(message)) if message.contains("object_key")
        ));
        assert!(matches!(
            validate_file_object_fields("backend", "object", 0, &checksum(), &metadata),
            Err(Error::InvalidInput(message)) if message.contains("size_bytes")
        ));
        assert!(matches!(
            validate_file_object_fields("backend", "object", 1, "bad", &metadata),
            Err(Error::InvalidInput(message)) if message.contains("content_manifest_sha256")
        ));
        assert!(
            validate_file_object_fields("backend", "object", 1, &checksum(), &metadata).is_ok()
        );
        assert!(matches!(
            validate_file_object_fields(
                "backend",
                "object",
                1,
                &checksum(),
                &serde_json::json!([])
            ),
            Err(Error::InvalidInput(message)) if message.contains("metadata")
        ));
    }

    #[test]
    fn file_reference_validation_rejects_business_field_policy_violations() {
        let metadata = object_metadata();
        assert!(matches!(
            validate_file_reference_fields("backend", "object", "", "ref", &metadata),
            Err(Error::InvalidInput(message)) if message.contains("reference_kind")
        ));
        assert!(matches!(
            validate_file_reference_fields(
                "backend",
                "object",
                "kind",
                &"r".repeat(FILE_REFERENCE_ID_MAX_CHARS + 1),
                &metadata,
            ),
            Err(Error::InvalidInput(message)) if message.contains("reference_id")
        ));
        assert!(matches!(
            validate_required_text("", "file cleanup origin", FILE_CLEANUP_ORIGIN_MAX_CHARS),
            Err(Error::InvalidInput(message)) if message.contains("cleanup origin")
        ));
    }
}
