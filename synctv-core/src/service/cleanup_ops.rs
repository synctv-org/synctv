//! Shared cleanup primitives used by both `CleanupService` and
//! `DatabaseMaintenanceService`.
//!
//! Both services historically reimplemented byte-identical deletion SQL and
//! file-reference cleanup. These free functions are the single source of truth
//! so the two schedules cannot drift apart.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::warn;

use super::partitioning::{len_to_u64, retention_seconds_to_i64, u32_to_i32};
use super::{FileStorageCleanupOrigin, FileStorageService};
use crate::models::{ChatAttachment, FileReferenceTarget, RoomId};
use crate::repository::{
    realtime_outbox::RealtimeOutboxStatus, FileStorageRepository, RoomResourceEventRepository,
};
use crate::{InternalExt, Result};

const UNREFERENCED_FILE_REFERENCE_KIND: &str = "unreferenced_file";

#[derive(Clone, Copy)]
pub(super) enum ChatMessageCleanupScope {
    RoomCap {
        room_id: RoomId,
        keep_count: i32,
    },
    AllRoomsCap {
        keep_count: i64,
    },
    ActiveRoomsCap {
        keep_count: i32,
        activity_window_minutes: i32,
    },
    Retention {
        retention_days: i64,
    },
}

fn unreferenced_file_reference_id(storage_backend: &str, object_key: &str) -> String {
    let digest = Sha256::digest(format!("{storage_backend}\0{object_key}").as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

fn chat_attachment_file_references(attachments: &[ChatAttachment]) -> Vec<FileReferenceTarget> {
    attachments
        .iter()
        .map(ChatAttachment::file_reference_target)
        .collect()
}

async fn schedule_chat_attachment_cleanup(
    storage: &Arc<dyn FileStorageService>,
    origin: FileStorageCleanupOrigin,
    attachments: &[ChatAttachment],
    deleted: u64,
    context: &'static str,
) {
    if attachments.is_empty() {
        return;
    }
    let file_references = chat_attachment_file_references(attachments);
    if let Err(error) = storage
        .schedule_delete_files(origin, &file_references)
        .await
    {
        warn!(error = %error, deleted, context, "Chat attachment cleanup scheduling failed");
    }
}

async fn list_chat_attachments_for_cleanup(
    pool: &PgPool,
    scope: ChatMessageCleanupScope,
) -> Result<Vec<ChatAttachment>> {
    let attachments = match scope {
        ChatMessageCleanupScope::RoomCap {
            room_id,
            keep_count,
        } => {
            if keep_count <= 0 {
                return Ok(Vec::new());
            }
            sqlx::query_as!(
                ChatAttachment,
                r#"
                WITH ranked AS (
                    SELECT id, created_at,
                           ROW_NUMBER() OVER (ORDER BY created_at DESC, id DESC) AS rn
                    FROM chat_messages
                    WHERE room_id = $1
                      AND created_at > NOW() - INTERVAL '90 days'
                ),
                candidates AS (
                    SELECT id, created_at
                    FROM ranked
                    WHERE rn > $2
                )
                SELECT i.id,
                       i.kind AS "kind!: crate::models::ChatAttachmentKind",
                       i.room_id AS "room_id!: crate::models::RoomId",
                       i.message_id,
                       i.message_created_at,
                       i.filename,
                       i.storage_backend,
                       i.object_key,
                       i.url,
                       i.mime_type,
                       i.size_bytes,
                       i.width,
                       i.height,
                       i.metadata AS "metadata!: serde_json::Value",
                       i.created_at,
                       NULL::TEXT AS "reuse_token?",
                       NULL::TIMESTAMPTZ AS "reuse_expires_at?"
                FROM chat_message_attachments i
                INNER JOIN candidates c
                    ON c.id = i.message_id AND c.created_at = i.message_created_at
                ORDER BY i.message_created_at, i.message_id, i.created_at
                "#,
                room_id.as_i64(),
                i64::from(keep_count),
            )
            .fetch_all(pool)
            .await
            .internal_with_err("Failed to collect room chat attachment cleanup candidates")?
        }
        ChatMessageCleanupScope::AllRoomsCap { keep_count } => {
            if keep_count <= 0 {
                return Ok(Vec::new());
            }
            sqlx::query_as!(
                ChatAttachment,
                r#"
                WITH ranked AS (
                    SELECT id, created_at,
                           ROW_NUMBER() OVER (PARTITION BY room_id ORDER BY created_at DESC, id DESC) AS rn
                    FROM chat_messages
                ),
                candidates AS (
                    SELECT id, created_at
                    FROM ranked
                    WHERE rn > $1
                )
                SELECT i.id,
                       i.kind AS "kind!: crate::models::ChatAttachmentKind",
                       i.room_id AS "room_id!: crate::models::RoomId",
                       i.message_id,
                       i.message_created_at,
                       i.filename,
                       i.storage_backend,
                       i.object_key,
                       i.url,
                       i.mime_type,
                       i.size_bytes,
                       i.width,
                       i.height,
                       i.metadata AS "metadata!: serde_json::Value",
                       i.created_at,
                       NULL::TEXT AS "reuse_token?",
                       NULL::TIMESTAMPTZ AS "reuse_expires_at?"
                FROM chat_message_attachments i
                INNER JOIN candidates c
                    ON c.id = i.message_id AND c.created_at = i.message_created_at
                ORDER BY i.message_created_at, i.message_id, i.created_at
                "#,
                keep_count,
            )
            .fetch_all(pool)
            .await
            .internal_with_err("Failed to collect chat attachment cleanup candidates")?
        }
        ChatMessageCleanupScope::ActiveRoomsCap {
            keep_count,
            activity_window_minutes,
        } => {
            if keep_count <= 0 {
                return Ok(Vec::new());
            }
            sqlx::query_as!(
                ChatAttachment,
                r#"
                WITH active_rooms AS (
                    SELECT DISTINCT room_id
                    FROM chat_messages
                    WHERE created_at >= NOW() - make_interval(mins => $2)
                ),
                ranked AS (
                    SELECT id, created_at, room_id,
                           ROW_NUMBER() OVER (PARTITION BY room_id ORDER BY created_at DESC, id DESC) AS rn
                    FROM chat_messages
                    WHERE room_id IN (SELECT room_id FROM active_rooms)
                      AND created_at > NOW() - INTERVAL '90 days'
                ),
                candidates AS (
                    SELECT id, created_at
                    FROM ranked
                    WHERE rn > $1
                )
                SELECT i.id,
                       i.kind AS "kind!: crate::models::ChatAttachmentKind",
                       i.room_id AS "room_id!: crate::models::RoomId",
                       i.message_id,
                       i.message_created_at,
                       i.filename,
                       i.storage_backend,
                       i.object_key,
                       i.url,
                       i.mime_type,
                       i.size_bytes,
                       i.width,
                       i.height,
                       i.metadata AS "metadata!: serde_json::Value",
                       i.created_at,
                       NULL::TEXT AS "reuse_token?",
                       NULL::TIMESTAMPTZ AS "reuse_expires_at?"
                FROM chat_message_attachments i
                INNER JOIN candidates c
                    ON c.id = i.message_id AND c.created_at = i.message_created_at
                ORDER BY i.message_created_at, i.message_id, i.created_at
                "#,
                i64::from(keep_count),
                activity_window_minutes,
            )
            .fetch_all(pool)
            .await
            .internal_with_err("Failed to collect active-room chat attachment cleanup candidates")?
        }
        ChatMessageCleanupScope::Retention { retention_days } => {
            if retention_days <= 0 {
                return Ok(Vec::new());
            }
            sqlx::query_as!(
                ChatAttachment,
                r#"
                SELECT i.id,
                       i.kind AS "kind!: crate::models::ChatAttachmentKind",
                       i.room_id AS "room_id!: crate::models::RoomId",
                       i.message_id,
                       i.message_created_at,
                       i.filename,
                       i.storage_backend,
                       i.object_key,
                       i.url,
                       i.mime_type,
                       i.size_bytes,
                       i.width,
                       i.height,
                       i.metadata AS "metadata!: serde_json::Value",
                       i.created_at,
                       NULL::TEXT AS "reuse_token?",
                       NULL::TIMESTAMPTZ AS "reuse_expires_at?"
                FROM chat_message_attachments i
                INNER JOIN chat_messages m
                    ON m.id = i.message_id AND m.created_at = i.message_created_at
                WHERE m.created_at <= NOW() - make_interval(days => $1)
                ORDER BY m.created_at, m.id, i.created_at
                "#,
                i32::try_from(retention_days).map_err(|_| {
                    crate::Error::InvalidInput(
                        "chat message retention days is too large".to_string(),
                    )
                })?,
            )
            .fetch_all(pool)
            .await
            .internal_with_err("Failed to collect retained chat attachment cleanup candidates")?
        }
    };
    Ok(attachments)
}

async fn delete_chat_messages_for_cleanup(
    pool: &PgPool,
    scope: ChatMessageCleanupScope,
) -> Result<u64> {
    let deleted = match scope {
        ChatMessageCleanupScope::RoomCap {
            room_id,
            keep_count,
        } => {
            if keep_count <= 0 {
                return Ok(0);
            }
            sqlx::query!(
                r#"
                DELETE FROM chat_messages
                WHERE room_id = $1
                  AND created_at > NOW() - INTERVAL '90 days'
                  AND (id, created_at) IN (
                    SELECT id, created_at FROM (
                        SELECT id, created_at,
                               ROW_NUMBER() OVER (ORDER BY created_at DESC, id DESC) AS rn
                        FROM chat_messages
                        WHERE room_id = $1
                          AND created_at > NOW() - INTERVAL '90 days'
                    ) ranked
                    WHERE rn > $2
                )
                "#,
                room_id.as_i64(),
                i64::from(keep_count),
            )
            .execute(pool)
            .await
            .internal_with_err("Failed to cleanup room chat messages")?
            .rows_affected()
        }
        ChatMessageCleanupScope::AllRoomsCap { keep_count } => {
            if keep_count <= 0 {
                return Ok(0);
            }
            sqlx::query!(
                r#"
                WITH ranked AS (
                    SELECT id,
                           created_at,
                           ROW_NUMBER() OVER (PARTITION BY room_id ORDER BY created_at DESC, id DESC) AS rn
                    FROM chat_messages
                ),
                candidates AS (
                    SELECT id, created_at
                    FROM ranked
                    WHERE rn > $1
                )
                DELETE FROM chat_messages m
                USING candidates c
                WHERE m.id = c.id AND m.created_at = c.created_at
                "#,
                keep_count,
            )
            .execute(pool)
            .await
            .internal_with_err("Failed to cleanup chat messages")?
            .rows_affected()
        }
        ChatMessageCleanupScope::ActiveRoomsCap {
            keep_count,
            activity_window_minutes,
        } => {
            if keep_count <= 0 {
                return Ok(0);
            }
            sqlx::query!(
                r#"
                DELETE FROM chat_messages
                WHERE created_at > NOW() - INTERVAL '90 days'
                  AND (id, created_at) IN (
                    SELECT id, created_at FROM (
                        SELECT id, created_at, room_id,
                               ROW_NUMBER() OVER (PARTITION BY room_id ORDER BY created_at DESC, id DESC) AS rn
                        FROM chat_messages
                        WHERE room_id IN (
                            SELECT DISTINCT room_id
                            FROM chat_messages
                            WHERE created_at >= NOW() - make_interval(mins => $2)
                        )
                          AND created_at > NOW() - INTERVAL '90 days'
                    ) ranked_messages
                    WHERE rn > $1
                )
                "#,
                i64::from(keep_count),
                activity_window_minutes,
            )
            .execute(pool)
            .await
            .internal_with_err("Failed to cleanup active-room chat messages")?
            .rows_affected()
        }
        ChatMessageCleanupScope::Retention { retention_days } => {
            if retention_days <= 0 {
                return Ok(0);
            }
            sqlx::query!(
                r#"
                DELETE FROM chat_messages
                WHERE created_at <= NOW() - make_interval(days => $1)
                "#,
                i32::try_from(retention_days).map_err(|_| {
                    crate::Error::InvalidInput(
                        "chat message retention days is too large".to_string(),
                    )
                })?,
            )
            .execute(pool)
            .await
            .internal_with_err("Failed to cleanup retained chat messages")?
            .rows_affected()
        }
    };
    Ok(deleted)
}

pub(super) async fn cleanup_chat_messages_with_files(
    pool: &PgPool,
    storage: Option<&Arc<dyn FileStorageService>>,
    scope: ChatMessageCleanupScope,
    origin: FileStorageCleanupOrigin,
    log_context: &'static str,
) -> Result<u64> {
    let attachments = if storage.is_some() {
        list_chat_attachments_for_cleanup(pool, scope).await?
    } else {
        Vec::new()
    };
    let deleted = delete_chat_messages_for_cleanup(pool, scope).await?;
    if let Some(storage) = storage {
        schedule_chat_attachment_cleanup(storage, origin, &attachments, deleted, log_context).await;
    }
    Ok(deleted)
}

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

/// Delete delivered realtime outbox rows after their diagnostic retention windows.
pub(super) async fn delete_delivered_realtime_outbox(
    pool: &PgPool,
    sent_retention_days: u32,
    dead_retention_days: u32,
) -> Result<u64> {
    if sent_retention_days == 0 && dead_retention_days == 0 {
        return Ok(0);
    }
    let sent_days = u32_to_i32(sent_retention_days, "realtime_outbox_sent_retention_days")?;
    let dead_days = u32_to_i32(dead_retention_days, "realtime_outbox_dead_retention_days")?;
    let result = sqlx::query!(
        r"
            DELETE FROM realtime_outbox
            WHERE (
                    $1 > 0
                AND status = $2
                AND dispatched_at IS NOT NULL
                AND dispatched_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            )
            OR (
                    $3 > 0
                AND status = $4
                AND created_at < CURRENT_TIMESTAMP - make_interval(days => $3)
            )
            ",
        sent_days,
        RealtimeOutboxStatus::Sent.as_i16(),
        dead_days,
        RealtimeOutboxStatus::Dead.as_i16(),
    )
    .execute(pool)
    .await
    .internal_with_err("Failed to cleanup delivered realtime outbox rows")?;
    Ok(result.rows_affected())
}

/// Release file references whose reference-level lifetime has expired and
/// schedule backend object deletion through `storage`. Returns the number of
/// references scheduled.
pub(super) async fn cleanup_expired_file_references(
    pool: &PgPool,
    storage: &Arc<dyn FileStorageService>,
) -> Result<u64> {
    let repository = FileStorageRepository::new(pool.clone());
    let references = repository.list_expired_references(100).await?;
    if references.is_empty() {
        return Ok(0);
    }

    storage
        .schedule_delete_files(FileStorageCleanupOrigin::ReferenceExpired, &references)
        .await?;
    len_to_u64(references.len(), "expired file reference count")
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

/// Schedule deletion of uploaded file objects that never received an active
/// product reference. Returns the number of objects scheduled.
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
            reference_id: unreferenced_file_reference_id(&file.storage_backend, &file.object_key),
            storage_backend: file.storage_backend,
            object_key: file.object_key.clone(),
            reference_kind: UNREFERENCED_FILE_REFERENCE_KIND.to_string(),
        })
        .collect::<Vec<_>>();

    storage
        .schedule_delete_files(FileStorageCleanupOrigin::UnreferencedObject, &references)
        .await?;
    len_to_u64(references.len(), "unreferenced file count")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::FILE_REFERENCE_ID_MAX_CHARS;

    #[test]
    fn unreferenced_file_reference_id_is_stable_and_bounded() {
        let long_object_key = format!("database/chat/attachments/{}.webp", "x".repeat(1800));
        let id = unreferenced_file_reference_id("database", &long_object_key);

        assert_eq!(
            id,
            unreferenced_file_reference_id("database", &long_object_key)
        );
        assert!(id.len() <= FILE_REFERENCE_ID_MAX_CHARS);
        assert_ne!(id, unreferenced_file_reference_id("s3", &long_object_key));
    }
}
