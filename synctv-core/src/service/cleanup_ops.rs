//! Shared cleanup primitives used by both `CleanupService` and
//! `DatabaseMaintenanceService`.
//!
//! Both services historically reimplemented byte-identical deletion SQL and
//! file-reference cleanup. These free functions are the single source of truth
//! so the two schedules cannot drift apart.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use tracing::warn;

use super::partitioning::{len_to_u64, retention_seconds_to_i64, u32_to_i32};
use super::{FileStorageCleanupOrigin, FileStorageService};
use crate::models::{ChatAttachment, FileReferenceTarget, RoomId};
use crate::repository::{realtime_outbox::RealtimeOutboxStatus, FileStorageRepository};
use crate::{Error, InternalExt, Result};

const UNREFERENCED_FILE_REFERENCE_KIND: &str = "unreferenced_file";
const CHAT_MESSAGE_CLEANUP_BATCH_SIZE: i64 = 1_000;
const CHAT_CLEANUP_ROOM_BATCH_SIZE: usize = 100;
const EVENT_CLEANUP_BATCH_SIZE: i64 = 5_000;
pub(super) const CHAT_MESSAGE_COUNT_PRUNING_DAYS: i32 = 90;

#[derive(Clone, Copy)]
pub(super) enum ChatMessageCleanupScope {
    RoomCap {
        room_id: RoomId,
        keep_count: i64,
    },
    ActiveRoomsCap {
        keep_count: i64,
        activity_window_minutes: i32,
    },
    Retention {
        retention_days: i64,
    },
}

/// Compute the effective chat message event retention window.
///
/// Events are retained for at least as long as their messages: a client
/// replaying events for a room must be able to reconcile them against the
/// messages that are still present.  `config_floor` is the hard lower bound
/// from config; `message_retention_days` is the current message retention
/// setting.  The effective window is the larger of the two.
///
/// Returns `Ok(0)` when `config_floor == 0` (feature disabled).
pub(super) fn effective_chat_message_event_retention_seconds(
    config_floor: u64,
    message_retention_days: i64,
) -> crate::Result<u64> {
    if config_floor == 0 {
        return Ok(0);
    }
    let message_retention_seconds = u64::try_from(message_retention_days)
        .map_err(|_| crate::Error::Internal("chat_message_retention_days is negative".to_string()))?
        .saturating_mul(24 * 60 * 60);
    Ok(config_floor.max(message_retention_seconds))
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

#[derive(sqlx::FromRow)]
struct DeletedChatMessageRow {
    id: i64,
    created_at: DateTime<Utc>,
}

struct ChatCleanupBatch {
    deleted: u64,
    attachments: Vec<ChatAttachment>,
}

async fn chat_cleanup_attachments_for_candidates(
    executor: &mut PgConnection,
    candidates: &[DeletedChatMessageRow],
) -> Result<Vec<ChatAttachment>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let message_ids = candidates
        .iter()
        .map(|candidate| candidate.id)
        .collect::<Vec<_>>();
    let message_created_at = candidates
        .iter()
        .map(|candidate| candidate.created_at)
        .collect::<Vec<_>>();

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
        JOIN UNNEST($1::BIGINT[], $2::TIMESTAMPTZ[]) AS c(id, created_at)
          ON c.id = i.message_id
         AND c.created_at = i.message_created_at
        ORDER BY i.message_created_at, i.message_id, i.created_at
        "#,
        &message_ids,
        &message_created_at,
    )
    .fetch_all(&mut *executor)
    .await
    .internal_with_err("Failed to collect chat cleanup attachments")
}

async fn delete_candidate_chat_messages(
    executor: &mut PgConnection,
    candidates: &[DeletedChatMessageRow],
) -> Result<u64> {
    if candidates.is_empty() {
        return Ok(0);
    }
    let message_ids = candidates
        .iter()
        .map(|candidate| candidate.id)
        .collect::<Vec<_>>();
    let message_created_at = candidates
        .iter()
        .map(|candidate| candidate.created_at)
        .collect::<Vec<_>>();

    let result = sqlx::query!(
        r#"
        DELETE FROM chat_messages m
        USING UNNEST($1::BIGINT[], $2::TIMESTAMPTZ[]) AS c(id, created_at)
        WHERE m.id = c.id
          AND m.created_at = c.created_at
        "#,
        &message_ids,
        &message_created_at,
    )
    .execute(&mut *executor)
    .await
    .internal_with_err("Failed to delete candidate chat messages")?;

    Ok(result.rows_affected())
}

async fn cleanup_candidate_chat_messages(
    executor: &mut PgConnection,
    candidates: Vec<DeletedChatMessageRow>,
) -> Result<ChatCleanupBatch> {
    let attachments = chat_cleanup_attachments_for_candidates(executor, &candidates).await?;
    let deleted = delete_candidate_chat_messages(executor, &candidates).await?;
    Ok(ChatCleanupBatch {
        deleted,
        attachments,
    })
}

async fn cleanup_room_chat_messages_batch(
    executor: &mut PgConnection,
    room_id: RoomId,
    keep_count: i64,
) -> Result<ChatCleanupBatch> {
    if keep_count <= 0 {
        return Ok(ChatCleanupBatch {
            deleted: 0,
            attachments: Vec::new(),
        });
    }

    let candidates = sqlx::query_as!(
        DeletedChatMessageRow,
        r#"
        WITH retained AS (
            SELECT id, created_at
            FROM chat_messages
            WHERE room_id = $1
              AND created_at > NOW() - make_interval(days => $2)
            ORDER BY created_at DESC, id DESC
            LIMIT $3
        )
        SELECT m.id, m.created_at
        FROM chat_messages m
        WHERE m.room_id = $1
          AND m.created_at > NOW() - make_interval(days => $2)
          AND NOT EXISTS (
              SELECT 1
              FROM retained r
              WHERE r.id = m.id
                AND r.created_at = m.created_at
          )
        ORDER BY m.created_at ASC, m.id ASC
        LIMIT $4
        FOR UPDATE OF m SKIP LOCKED
        "#,
        room_id.as_i64(),
        CHAT_MESSAGE_COUNT_PRUNING_DAYS,
        keep_count,
        CHAT_MESSAGE_CLEANUP_BATCH_SIZE,
    )
    .fetch_all(&mut *executor)
    .await
    .internal_with_err("Failed to list room chat cleanup candidates")?;

    cleanup_candidate_chat_messages(executor, candidates).await
}

async fn cleanup_retained_chat_messages_batch(
    executor: &mut PgConnection,
    retention_days: i64,
) -> Result<ChatCleanupBatch> {
    if retention_days <= 0 {
        return Ok(ChatCleanupBatch {
            deleted: 0,
            attachments: Vec::new(),
        });
    }
    let retention_days = i32::try_from(retention_days)
        .map_err(|_| Error::InvalidInput("chat message retention days is too large".to_string()))?;

    let candidates = sqlx::query_as!(
        DeletedChatMessageRow,
        r#"
        SELECT id, created_at
        FROM chat_messages
        WHERE created_at <= NOW() - make_interval(days => $1)
        ORDER BY created_at ASC, id ASC
        LIMIT $2
        FOR UPDATE SKIP LOCKED
        "#,
        retention_days,
        CHAT_MESSAGE_CLEANUP_BATCH_SIZE,
    )
    .fetch_all(&mut *executor)
    .await
    .internal_with_err("Failed to list retained chat cleanup candidates")?;

    cleanup_candidate_chat_messages(executor, candidates).await
}

async fn active_chat_rooms_for_cleanup<'e, E>(
    executor: E,
    keep_count: i64,
    activity_window_minutes: i32,
    after_room_id: Option<RoomId>,
) -> Result<Vec<RoomId>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    if keep_count <= 0 {
        return Ok(Vec::new());
    }
    if activity_window_minutes < 0 {
        return Err(Error::InvalidInput(
            "activity_window_minutes must be greater than or equal to 0".to_string(),
        ));
    }

    let rows = sqlx::query_scalar!(
        r#"
        WITH active_rooms AS (
            SELECT room_id
            FROM chat_messages
            WHERE created_at >= NOW() - make_interval(mins => $1)
              AND ($5::BIGINT IS NULL OR room_id > $5)
            GROUP BY room_id
        )
        SELECT active_rooms.room_id AS "room_id: RoomId"
        FROM active_rooms
        WHERE EXISTS (
            SELECT 1
            FROM chat_messages m
            WHERE m.room_id = active_rooms.room_id
              AND m.created_at > NOW() - make_interval(days => $2)
            ORDER BY m.created_at DESC, m.id DESC
            OFFSET $3
            LIMIT 1
        )
        ORDER BY active_rooms.room_id ASC
        LIMIT $4
        "#,
        activity_window_minutes,
        CHAT_MESSAGE_COUNT_PRUNING_DAYS,
        keep_count,
        i64::try_from(CHAT_CLEANUP_ROOM_BATCH_SIZE).map_err(|_| Error::Internal(
            "chat cleanup room batch size is too large".to_string()
        ))?,
        after_room_id.map(|room_id| room_id.as_i64()),
    )
    .fetch_all(executor)
    .await
    .internal_with_err("Failed to list active rooms for chat cleanup")?;

    Ok(rows)
}

async fn cleanup_active_room_chat_messages_with_files(
    pool: &PgPool,
    storage: Option<&Arc<dyn FileStorageService>>,
    keep_count: i64,
    activity_window_minutes: i32,
    origin: FileStorageCleanupOrigin,
    log_context: &'static str,
) -> Result<u64> {
    let mut total_deleted = 0;
    let mut after_room_id = None;

    loop {
        let mut tx = pool.begin().await?;
        let room_ids = active_chat_rooms_for_cleanup(
            &mut *tx,
            keep_count,
            activity_window_minutes,
            after_room_id,
        )
        .await?;
        let Some(last_room_id) = room_ids.last().copied() else {
            tx.commit()
                .await
                .internal_with_err("Failed to commit active-room chat cleanup scan")?;
            break;
        };
        let has_next_page = room_ids.len() == CHAT_CLEANUP_ROOM_BATCH_SIZE;

        let mut batch = ChatCleanupBatch {
            deleted: 0,
            attachments: Vec::new(),
        };
        for room_id in room_ids {
            let room_batch = cleanup_room_chat_messages_batch(&mut tx, room_id, keep_count).await?;
            batch.deleted += room_batch.deleted;
            batch.attachments.extend(room_batch.attachments);
        }

        tx.commit()
            .await
            .internal_with_err("Failed to commit active-room chat cleanup batch")?;

        if let Some(storage) = storage {
            schedule_chat_attachment_cleanup(
                storage,
                origin,
                &batch.attachments,
                batch.deleted,
                log_context,
            )
            .await;
        }
        total_deleted += batch.deleted;
        after_room_id = Some(last_room_id);

        if !has_next_page {
            break;
        }
    }

    Ok(total_deleted)
}

pub(super) async fn cleanup_chat_messages_with_files(
    pool: &PgPool,
    storage: Option<&Arc<dyn FileStorageService>>,
    scope: ChatMessageCleanupScope,
    origin: FileStorageCleanupOrigin,
    log_context: &'static str,
) -> Result<u64> {
    match scope {
        ChatMessageCleanupScope::ActiveRoomsCap {
            keep_count,
            activity_window_minutes,
        } => {
            cleanup_active_room_chat_messages_with_files(
                pool,
                storage,
                keep_count,
                activity_window_minutes,
                origin,
                log_context,
            )
            .await
        }
        ChatMessageCleanupScope::RoomCap {
            room_id,
            keep_count,
        } => {
            let mut tx = pool.begin().await?;
            let batch = cleanup_room_chat_messages_batch(&mut tx, room_id, keep_count).await?;
            tx.commit()
                .await
                .internal_with_err("Failed to commit chat message cleanup batch")?;
            if let Some(storage) = storage {
                schedule_chat_attachment_cleanup(
                    storage,
                    origin,
                    &batch.attachments,
                    batch.deleted,
                    log_context,
                )
                .await;
            }
            Ok(batch.deleted)
        }
        ChatMessageCleanupScope::Retention { retention_days } => {
            let mut tx = pool.begin().await?;
            let batch = cleanup_retained_chat_messages_batch(&mut tx, retention_days).await?;
            tx.commit()
                .await
                .internal_with_err("Failed to commit chat message cleanup batch")?;
            if let Some(storage) = storage {
                schedule_chat_attachment_cleanup(
                    storage,
                    origin,
                    &batch.attachments,
                    batch.deleted,
                    log_context,
                )
                .await;
            }
            Ok(batch.deleted)
        }
    }
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
    let mut total_deleted = 0;
    loop {
        let deleted = delete_old_room_resource_events_batch(pool, retention_seconds).await?;
        total_deleted += deleted;
        if deleted < EVENT_CLEANUP_BATCH_SIZE.cast_unsigned() {
            break;
        }
    }
    Ok(total_deleted)
}

async fn delete_old_room_resource_events_batch(
    pool: &PgPool,
    retention_seconds: i64,
) -> Result<u64> {
    let result = sqlx::query!(
        r#"
        WITH candidates AS (
            SELECT sequence
            FROM room_resource_events
            WHERE created_at < NOW() - ($1::bigint::text || ' seconds')::interval
            ORDER BY created_at ASC, sequence ASC
            LIMIT $2
            FOR UPDATE SKIP LOCKED
        )
        DELETE FROM room_resource_events e
        USING candidates c
        WHERE e.sequence = c.sequence
        "#,
        retention_seconds.max(1),
        EVENT_CLEANUP_BATCH_SIZE,
    )
    .execute(pool)
    .await
    .internal_with_err("Failed to cleanup expired room resource events")?;
    Ok(result.rows_affected())
}

/// Delete durable chat message events older than the replay retention window.
pub(super) async fn delete_old_chat_message_events(
    pool: &PgPool,
    retention_seconds: u64,
) -> Result<u64> {
    if retention_seconds == 0 {
        return Ok(0);
    }
    let retention_seconds =
        retention_seconds_to_i64(retention_seconds, "chat_message_event_retention_seconds")?;
    let mut total_deleted = 0;
    loop {
        let deleted = delete_old_chat_message_events_batch(pool, retention_seconds).await?;
        total_deleted += deleted;
        if deleted < EVENT_CLEANUP_BATCH_SIZE.cast_unsigned() {
            break;
        }
    }
    Ok(total_deleted)
}

async fn delete_old_chat_message_events_batch(
    pool: &PgPool,
    retention_seconds: i64,
) -> Result<u64> {
    let result = sqlx::query!(
        r#"
        WITH candidates AS (
            SELECT sequence
            FROM chat_message_events
            WHERE created_at < NOW() - ($1::bigint::text || ' seconds')::interval
            ORDER BY created_at ASC, sequence ASC
            LIMIT $2
            FOR UPDATE SKIP LOCKED
        )
        DELETE FROM chat_message_events e
        USING candidates c
        WHERE e.sequence = c.sequence
        "#,
        retention_seconds.max(1),
        EVENT_CLEANUP_BATCH_SIZE,
    )
    .execute(pool)
    .await
    .internal_with_err("Failed to cleanup expired chat message events")?;
    Ok(result.rows_affected())
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
