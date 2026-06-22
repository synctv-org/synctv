//! Shared cleanup primitives used by both `CleanupService` and
//! `DatabaseMaintenanceService`.
//!
//! Both services historically reimplemented byte-identical deletion SQL and
//! file-reference cleanup. These free functions are the single source of truth
//! so the two schedules cannot drift apart.

use std::sync::Arc;

use sqlx::PgPool;

use super::partitioning::{len_to_u64, retention_seconds_to_i64, u32_to_i32};
use super::{FileStorageCleanupOrigin, FileStorageService};
use crate::models::FileReferenceTarget;
use crate::repository::{FileStorageRepository, RoomResourceEventRepository};
use crate::{InternalExt, Result};

/// Delete expired media provider credentials with a buffer that prevents races
/// with in-flight refreshes. Returns the number of rows deleted.
pub(super) async fn delete_expired_credentials(pool: &PgPool, buffer_hours: u32) -> Result<u64> {
    if buffer_hours == 0 {
        return Ok(0);
    }
    let buffer_hours = u32_to_i32(buffer_hours, "expired_credential_buffer_hours")?;
    let result = sqlx::query!(
        r"
            DELETE FROM user_media_provider_credentials
            WHERE expires_at IS NOT NULL
              AND expires_at < CURRENT_TIMESTAMP - make_interval(hours => $1)
            ",
        buffer_hours
    )
    .execute(pool)
    .await
    .internal_with_err("Failed to delete expired credentials")?;
    Ok(result.rows_affected())
}

/// Delete read notifications older than the retention period. Returns rows deleted.
pub(super) async fn delete_old_read_notifications(
    pool: &PgPool,
    retention_days: u32,
) -> Result<u64> {
    if retention_days == 0 {
        return Ok(0);
    }
    let days = u32_to_i32(retention_days, "notification_retention_days")?;
    let result = sqlx::query!(
        r"
            DELETE FROM notifications
            WHERE is_read = TRUE
              AND created_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ",
        days
    )
    .execute(pool)
    .await
    .internal_with_err("Failed to delete old notifications")?;
    Ok(result.rows_affected())
}

/// Delete all notifications (read or unread) older than the max retention
/// period. Returns rows deleted.
pub(super) async fn delete_expired_notifications(
    pool: &PgPool,
    max_retention_days: u32,
) -> Result<u64> {
    if max_retention_days == 0 {
        return Ok(0);
    }
    let days = u32_to_i32(max_retention_days, "notification_max_retention_days")?;
    let result = sqlx::query!(
        r"
            DELETE FROM notifications
            WHERE created_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ",
        days
    )
    .execute(pool)
    .await
    .internal_with_err("Failed to delete expired notifications")?;
    Ok(result.rows_affected())
}

/// Delete playback progress rows older than the retention period that are no
/// longer referenced by current playback state. Returns rows deleted.
pub(super) async fn delete_stale_playback_progress(
    pool: &PgPool,
    retention_days: u32,
) -> Result<u64> {
    if retention_days == 0 {
        return Ok(0);
    }
    let days = u32_to_i32(retention_days, "playback_progress_retention_days")?;
    let result = sqlx::query!(
        r#"
            DELETE FROM room_playback_progress progress
            WHERE progress.updated_at < CURRENT_TIMESTAMP - make_interval(days => $1)
              AND NOT EXISTS (
                  SELECT 1
                  FROM room_playback_state state
                  WHERE state.current_progress_id = progress.id
              )
            "#,
        days,
    )
    .execute(pool)
    .await
    .internal_with_err("Failed to cleanup stale playback progress")?;
    Ok(result.rows_affected())
}

/// Delete room resource events older than the retention period. Returns rows deleted.
pub(super) async fn delete_old_room_resource_events(
    pool: &PgPool,
    retention_seconds: u64,
) -> Result<u64> {
    if retention_seconds == 0 {
        return Ok(0);
    }
    let retention_seconds =
        retention_seconds_to_i64(retention_seconds, "room_resource_event_retention_seconds")?;
    RoomResourceEventRepository::new(pool.clone())
        .delete_older_than(retention_seconds)
        .await
}

/// Release file references whose reference-level lifetime has expired, deleting
/// the underlying objects through `storage`. Failed deletes are persisted as
/// retry jobs. Returns the number of references released.
pub(super) async fn cleanup_expired_file_references(
    pool: &PgPool,
    storage: &Arc<dyn FileStorageService>,
) -> Result<u64> {
    let repository = FileStorageRepository::new(pool.clone());
    let references = repository.list_expired_references(100).await?;
    if references.is_empty() {
        return Ok(0);
    }

    match storage
        .delete_files(FileStorageCleanupOrigin::ReferenceExpired, &references)
        .await
    {
        Ok(()) => len_to_u64(references.len(), "expired file reference count"),
        Err(error) => {
            repository
                .enqueue_cleanup_jobs(
                    FileStorageCleanupOrigin::ReferenceExpired.as_str(),
                    &references,
                    &serde_json::Value::Object(Default::default()),
                    &error.to_string(),
                )
                .await?;
            Err(error)
        }
    }
}

/// Delete expired upload sessions and backend-specific temporary upload data.
pub(super) async fn cleanup_expired_file_upload_sessions(
    pool: &PgPool,
    storage: &Arc<dyn FileStorageService>,
) -> Result<u64> {
    let repository = FileStorageRepository::new(pool.clone());
    let sessions = repository.list_expired_upload_sessions(100).await?;
    if sessions.is_empty() {
        return Ok(0);
    }

    let mut cleaned = 0_u64;
    for session in sessions {
        if storage.cleanup_expired_upload_session(session).await? {
            cleaned += 1;
        }
    }
    Ok(cleaned)
}

/// Delete uploaded file objects that never received an active product
/// reference, deleting the underlying objects through `storage`. Failed deletes
/// are persisted as retry jobs. Returns the number of objects cleaned.
pub(super) async fn cleanup_unreferenced_file_objects(
    pool: &PgPool,
    storage: &Arc<dyn FileStorageService>,
    retention_seconds: u64,
) -> Result<u64> {
    if retention_seconds == 0 {
        return Ok(0);
    }
    let repository = FileStorageRepository::new(pool.clone());
    let older_than_seconds =
        retention_seconds_to_i64(retention_seconds, "unreferenced_file_retention_seconds")?;
    let files = repository
        .list_unreferenced_objects(older_than_seconds, 100)
        .await?;
    if files.is_empty() {
        return Ok(0);
    }

    let references = files
        .into_iter()
        .map(|file| FileReferenceTarget {
            storage_backend: file.storage_backend,
            object_key: file.object_key.clone(),
            reference_kind: "unreferenced_file".to_string(),
            reference_id: file.object_key,
        })
        .collect::<Vec<_>>();

    match storage
        .delete_files(FileStorageCleanupOrigin::UnreferencedObject, &references)
        .await
    {
        Ok(()) => len_to_u64(references.len(), "unreferenced file count"),
        Err(error) => {
            repository
                .enqueue_cleanup_jobs(
                    FileStorageCleanupOrigin::UnreferencedObject.as_str(),
                    &references,
                    &serde_json::Value::Object(Default::default()),
                    &error.to_string(),
                )
                .await?;
            Err(error)
        }
    }
}
