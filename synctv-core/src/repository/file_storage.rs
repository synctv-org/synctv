use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{
    models::{
        FileBlob, FileBlobCompression, FileCleanupJob, FileObject, FileReferenceTarget,
        StoredFileReference, FILE_CHECKSUM_SHA256_HEX_CHARS, FILE_CLEANUP_ORIGIN_MAX_CHARS,
        FILE_OBJECT_KEY_MAX_CHARS, FILE_REFERENCE_ID_MAX_CHARS, FILE_REFERENCE_KIND_MAX_CHARS,
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

fn validate_file_object_fields(
    storage_backend: &str,
    object_key: &str,
    size_bytes: i64,
    checksum_sha256: &str,
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
    let valid_checksum = checksum_sha256.len() == FILE_CHECKSUM_SHA256_HEX_CHARS
        && checksum_sha256
            .chars()
            .all(|value| value.is_ascii_hexdigit());
    if !valid_checksum {
        return Err(Error::InvalidInput(format!(
            "file checksum_sha256 must be a {FILE_CHECKSUM_SHA256_HEX_CHARS}-character hex string"
        )));
    }
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

impl FileStorageRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn upsert_blob(&self, blob: UpsertFileBlob<'_>) -> Result<FileBlob> {
        validate_file_object_fields(
            blob.storage_backend,
            blob.object_key,
            blob.size_bytes,
            blob.checksum_sha256,
            blob.metadata,
        )?;
        let row = sqlx::query(
            r"
            INSERT INTO file_blobs (
                storage_backend, object_key, mime_type, size_bytes, checksum_sha256,
                compression, data, metadata
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (storage_backend, object_key)
            DO UPDATE SET
                mime_type = EXCLUDED.mime_type,
                size_bytes = EXCLUDED.size_bytes,
                checksum_sha256 = EXCLUDED.checksum_sha256,
                compression = EXCLUDED.compression,
                data = EXCLUDED.data,
                metadata = EXCLUDED.metadata,
                created_at = CURRENT_TIMESTAMP
            RETURNING storage_backend, object_key, mime_type, size_bytes, checksum_sha256,
                      compression, data, metadata, created_at
            ",
        )
        .bind(blob.storage_backend)
        .bind(blob.object_key)
        .bind(blob.mime_type)
        .bind(blob.size_bytes)
        .bind(blob.checksum_sha256)
        .bind(i16::from(blob.compression))
        .bind(blob.data)
        .bind(blob.metadata)
        .fetch_one(&self.pool)
        .await?;
        file_blob_from_row(&row)
    }

    pub async fn upsert_object(
        &self,
        storage_backend: &str,
        object_key: &str,
        mime_type: &str,
        size_bytes: i64,
        checksum_sha256: &str,
        metadata: &serde_json::Value,
    ) -> Result<FileObject> {
        validate_file_object_fields(
            storage_backend,
            object_key,
            size_bytes,
            checksum_sha256,
            metadata,
        )?;
        let row = sqlx::query_as!(
            FileObject,
            r#"
            INSERT INTO file_objects (
                storage_backend, object_key, mime_type, size_bytes,
                checksum_sha256, metadata, validated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, CURRENT_TIMESTAMP)
            ON CONFLICT (storage_backend, object_key)
            DO UPDATE SET
                mime_type = EXCLUDED.mime_type,
                size_bytes = EXCLUDED.size_bytes,
                checksum_sha256 = EXCLUDED.checksum_sha256,
                metadata = EXCLUDED.metadata,
                validated_at = CURRENT_TIMESTAMP
            RETURNING storage_backend, object_key, mime_type, size_bytes,
                      checksum_sha256, metadata as "metadata: serde_json::Value", created_at, validated_at
            "#,
            storage_backend,
            object_key,
            mime_type,
            size_bytes,
            checksum_sha256,
            metadata,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn upsert_pending_object(
        &self,
        storage_backend: &str,
        object_key: &str,
        mime_type: &str,
        size_bytes: i64,
        checksum_sha256: &str,
        metadata: &serde_json::Value,
    ) -> Result<FileObject> {
        validate_file_object_fields(
            storage_backend,
            object_key,
            size_bytes,
            checksum_sha256,
            metadata,
        )?;
        let row = sqlx::query_as!(
            FileObject,
            r#"
            INSERT INTO file_objects (
                storage_backend, object_key, mime_type, size_bytes,
                checksum_sha256, metadata, validated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, NULL)
            ON CONFLICT (storage_backend, object_key)
            DO UPDATE SET
                mime_type = EXCLUDED.mime_type,
                size_bytes = EXCLUDED.size_bytes,
                checksum_sha256 = EXCLUDED.checksum_sha256,
                metadata = EXCLUDED.metadata
            WHERE file_objects.validated_at IS NULL
            RETURNING storage_backend, object_key, mime_type, size_bytes,
                      checksum_sha256, metadata as "metadata: serde_json::Value", created_at, validated_at
            "#,
            storage_backend,
            object_key,
            mime_type,
            size_bytes,
            checksum_sha256,
            metadata,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn get_object_by_checksum(
        &self,
        storage_backend: &str,
        checksum_sha256: &str,
        size_bytes: i64,
    ) -> Result<Option<FileObject>> {
        let row = sqlx::query_as!(
            FileObject,
            r#"
            SELECT storage_backend, object_key, mime_type, size_bytes,
                   checksum_sha256, metadata as "metadata: serde_json::Value", created_at, validated_at
            FROM file_objects
            WHERE storage_backend = $1
              AND checksum_sha256 = $2
              AND size_bytes = $3
              AND validated_at IS NOT NULL
            "#,
            storage_backend,
            checksum_sha256,
            size_bytes,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn get_any_object_by_checksum(
        &self,
        storage_backend: &str,
        checksum_sha256: &str,
        size_bytes: i64,
    ) -> Result<Option<FileObject>> {
        let row = sqlx::query_as!(
            FileObject,
            r#"
            SELECT storage_backend, object_key, mime_type, size_bytes,
                   checksum_sha256, metadata as "metadata: serde_json::Value", created_at, validated_at
            FROM file_objects
            WHERE storage_backend = $1
              AND checksum_sha256 = $2
              AND size_bytes = $3
            ORDER BY validated_at DESC NULLS LAST, created_at ASC
            LIMIT 1
            "#,
            storage_backend,
            checksum_sha256,
            size_bytes,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
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

    pub async fn get_reference_by_id(&self, id: i64) -> Result<Option<StoredFileReference>> {
        let row = sqlx::query_as!(
            StoredFileReference,
            r#"
            SELECT r.id AS file_reference_id,
                   o.storage_backend,
                   o.object_key,
                   o.mime_type,
                   o.size_bytes,
                   o.checksum_sha256,
                   o.metadata as "metadata: serde_json::Value",
                   o.created_at,
                   o.validated_at
            FROM file_references r
            JOIN file_objects o
              ON o.storage_backend = r.storage_backend
             AND o.object_key = r.object_key
            WHERE r.id = $1
              AND r.released_at IS NULL
            "#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn get_active_reference_by_target(
        &self,
        reference_kind: &str,
        reference_id: &str,
    ) -> Result<Option<StoredFileReference>> {
        let row = sqlx::query_as!(
            StoredFileReference,
            r#"
            SELECT r.id AS file_reference_id,
                   o.storage_backend,
                   o.object_key,
                   o.mime_type,
                   o.size_bytes,
                   o.checksum_sha256,
                   o.metadata as "metadata: serde_json::Value",
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
            "#,
            reference_kind,
            reference_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
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

    pub async fn blob_exists(&self, storage_backend: &str, object_key: &str) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM file_blobs WHERE storage_backend = $1 AND object_key = $2)",
            storage_backend,
            object_key,
        )
        .fetch_one(&self.pool)
        .await?;
        scalar_value(exists, "file blob EXISTS")
    }

    pub async fn get_blob(
        &self,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<Option<FileBlob>> {
        let row = sqlx::query(
            r"
            SELECT storage_backend, object_key, mime_type, size_bytes, checksum_sha256,
                   compression, data, metadata, created_at
            FROM file_blobs
            WHERE storage_backend = $1 AND object_key = $2
            ",
        )
        .bind(storage_backend)
        .bind(object_key)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(file_blob_from_row).transpose()
    }

    pub async fn delete_blob(&self, storage_backend: &str, object_key: &str) -> Result<bool> {
        let result = sqlx::query!(
            "DELETE FROM file_blobs WHERE storage_backend = $1 AND object_key = $2",
            storage_backend,
            object_key,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
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
        let rows = sqlx::query_as!(
            FileObject,
            r#"
            SELECT o.storage_backend, o.object_key, o.mime_type, o.size_bytes,
                   o.checksum_sha256, o.metadata as "metadata: serde_json::Value", o.created_at, o.validated_at
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
            "#,
            older_than_seconds.max(1),
            limit.clamp(1, 1000),
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
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

fn file_blob_from_row(row: &sqlx::postgres::PgRow) -> Result<FileBlob> {
    let compression = row.try_get::<i16, _>("compression")?;
    let compression = FileBlobCompression::try_from(compression).map_err(|()| {
        Error::Internal(format!("unknown file blob compression value {compression}"))
    })?;
    Ok(FileBlob {
        storage_backend: row.try_get("storage_backend")?,
        object_key: row.try_get("object_key")?,
        mime_type: row.try_get("mime_type")?,
        size_bytes: row.try_get("size_bytes")?,
        checksum_sha256: row.try_get("checksum_sha256")?,
        compression,
        data: row.try_get("data")?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_metadata() -> serde_json::Value {
        serde_json::json!({})
    }

    fn checksum() -> String {
        "a".repeat(FILE_CHECKSUM_SHA256_HEX_CHARS)
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
            Err(Error::InvalidInput(message)) if message.contains("checksum_sha256")
        ));
        assert!(matches!(
            validate_file_object_fields("backend", "object", 1, &checksum(), &serde_json::json!([])),
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
