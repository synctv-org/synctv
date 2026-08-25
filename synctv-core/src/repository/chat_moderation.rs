use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    models::{RoomId, UserId, UserRole},
    Error, Result,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i16)]
pub enum ChatModerationJobStatus {
    Pending = 1,
    Processing = 2,
    Completed = 3,
    Failed = 4,
}

impl TryFrom<i16> for ChatModerationJobStatus {
    type Error = Error;

    fn try_from(value: i16) -> Result<Self> {
        match value {
            1 => Ok(Self::Pending),
            2 => Ok(Self::Processing),
            3 => Ok(Self::Completed),
            4 => Ok(Self::Failed),
            _ => Err(Error::Internal(format!(
                "Unknown chat moderation job status: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i16)]
pub enum ChatModerationJobPhase {
    Messages = 1,
    Reactions = 2,
    Done = 3,
}

impl TryFrom<i16> for ChatModerationJobPhase {
    type Error = Error;

    fn try_from(value: i16) -> Result<Self> {
        match value {
            1 => Ok(Self::Messages),
            2 => Ok(Self::Reactions),
            3 => Ok(Self::Done),
            _ => Err(Error::Internal(format!(
                "Unknown chat moderation job phase: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewChatModerationJob {
    pub id: String,
    pub room_id: RoomId,
    pub target_user_id: UserId,
    pub actor_user_id: UserId,
    pub actor_username: String,
    pub actor_role: UserRole,
    pub message_id: Option<i64>,
    pub ban_user: bool,
    pub delete_all_messages: bool,
    pub delete_all_reactions: bool,
    pub reason: Option<String>,
    pub snapshot_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ChatModerationJob {
    pub id: String,
    pub room_id: RoomId,
    pub target_user_id: UserId,
    pub actor_user_id: UserId,
    pub actor_username: String,
    pub actor_role: UserRole,
    pub message_id: Option<i64>,
    pub explicit_message_done: bool,
    pub ban_user: bool,
    pub ban_done: bool,
    pub delete_all_messages: bool,
    pub delete_all_reactions: bool,
    pub reason: Option<String>,
    pub phase: ChatModerationJobPhase,
    pub status: ChatModerationJobStatus,
    pub snapshot_at: DateTime<Utc>,
    pub message_cursor: Option<(DateTime<Utc>, i64)>,
    pub reaction_cursor: Option<(DateTime<Utc>, i64)>,
    pub hidden_reaction_cursor: Option<(DateTime<Utc>, i64, String)>,
    pub deleted_messages: i64,
    pub deleted_reactions: i64,
    pub attempts: i32,
    pub lock_version: i64,
    pub last_error: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ChatModerationJobRow {
    id: String,
    room_id: RoomId,
    target_user_id: UserId,
    actor_user_id: UserId,
    actor_username: String,
    actor_role: i16,
    message_id: Option<i64>,
    explicit_message_done: bool,
    ban_user: bool,
    ban_done: bool,
    delete_all_messages: bool,
    delete_all_reactions: bool,
    reason: Option<String>,
    phase: i16,
    status: i16,
    snapshot_at: DateTime<Utc>,
    message_cursor_created_at: Option<DateTime<Utc>>,
    message_cursor_id: Option<i64>,
    reaction_cursor_created_at: Option<DateTime<Utc>>,
    reaction_cursor_id: Option<i64>,
    hidden_reaction_cursor_created_at: Option<DateTime<Utc>>,
    hidden_reaction_cursor_id: Option<i64>,
    hidden_reaction_cursor_key: Option<String>,
    deleted_messages: i64,
    deleted_reactions: i64,
    attempts: i32,
    lock_version: i64,
    last_error: Option<String>,
}

impl TryFrom<ChatModerationJobRow> for ChatModerationJob {
    type Error = Error;

    fn try_from(row: ChatModerationJobRow) -> Result<Self> {
        let message_cursor = row.message_cursor_created_at.zip(row.message_cursor_id);
        let reaction_cursor = row.reaction_cursor_created_at.zip(row.reaction_cursor_id);
        let hidden_reaction_cursor = row
            .hidden_reaction_cursor_created_at
            .zip(row.hidden_reaction_cursor_id)
            .zip(row.hidden_reaction_cursor_key)
            .map(|((created_at, id), key)| (created_at, id, key));
        Ok(Self {
            id: row.id,
            room_id: row.room_id,
            target_user_id: row.target_user_id,
            actor_user_id: row.actor_user_id,
            actor_username: row.actor_username,
            actor_role: UserRole::try_from(row.actor_role).map_err(|error| {
                Error::Internal(format!("Invalid moderation actor role: {error}"))
            })?,
            message_id: row.message_id,
            explicit_message_done: row.explicit_message_done,
            ban_user: row.ban_user,
            ban_done: row.ban_done,
            delete_all_messages: row.delete_all_messages,
            delete_all_reactions: row.delete_all_reactions,
            reason: row.reason,
            phase: ChatModerationJobPhase::try_from(row.phase)?,
            status: ChatModerationJobStatus::try_from(row.status)?,
            snapshot_at: row.snapshot_at,
            message_cursor,
            reaction_cursor,
            hidden_reaction_cursor,
            deleted_messages: row.deleted_messages,
            deleted_reactions: row.deleted_reactions,
            attempts: row.attempts,
            lock_version: row.lock_version,
            last_error: row.last_error,
        })
    }
}

#[derive(Clone)]
pub struct ChatModerationJobRepository {
    pool: PgPool,
}

impl ChatModerationJobRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn insert(&self, job: &NewChatModerationJob) -> Result<ChatModerationJob> {
        validate_new_chat_moderation_job(job)?;
        let row = sqlx::query_as!(
            ChatModerationJobRow,
            r#"
            INSERT INTO chat_moderation_jobs (
                id, room_id, target_user_id, actor_user_id,
                actor_username,
                actor_role, message_id, explicit_message_done,
                ban_user, ban_done,
                delete_all_messages, delete_all_reactions, reason, snapshot_at,
                next_attempt_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, FALSE, $8, FALSE, $9, $10, $11, $12, NOW())
            RETURNING id,
                      room_id AS "room_id!: RoomId",
                      target_user_id AS "target_user_id!: UserId",
                      actor_user_id AS "actor_user_id!: UserId",
                      actor_username,
                      actor_role,
                      message_id,
                      explicit_message_done,
                      ban_user,
                      ban_done,
                      delete_all_messages,
                      delete_all_reactions,
                      reason,
                      phase AS "phase!",
                      status AS "status!",
                      snapshot_at,
                      message_cursor_created_at,
                      message_cursor_id,
                      reaction_cursor_created_at,
                      reaction_cursor_id,
                      hidden_reaction_cursor_created_at,
                      hidden_reaction_cursor_id,
                      hidden_reaction_cursor_key,
                      deleted_messages,
                      deleted_reactions,
                      attempts,
                      lock_version,
                      last_error
            "#,
            &job.id,
            job.room_id.as_i64(),
            job.target_user_id.as_i64(),
            job.actor_user_id.as_i64(),
            &job.actor_username,
            i16::from(job.actor_role),
            job.message_id,
            job.ban_user,
            job.delete_all_messages,
            job.delete_all_reactions,
            job.reason.as_deref(),
            job.snapshot_at,
        )
        .fetch_one(&self.pool)
        .await?;
        row.try_into()
    }

    pub async fn get(&self, id: &str) -> Result<Option<ChatModerationJob>> {
        let row = sqlx::query_as!(
            ChatModerationJobRow,
            r#"
            SELECT id,
                   room_id AS "room_id!: RoomId",
                   target_user_id AS "target_user_id!: UserId",
                   actor_user_id AS "actor_user_id!: UserId",
                   actor_username,
                   actor_role,
                   message_id,
                   explicit_message_done,
                   ban_user,
                   ban_done,
                   delete_all_messages,
                   delete_all_reactions,
                   reason,
                   phase AS "phase!",
                   status AS "status!",
                   snapshot_at,
                   message_cursor_created_at,
                   message_cursor_id,
                   reaction_cursor_created_at,
                   reaction_cursor_id,
                   hidden_reaction_cursor_created_at,
                   hidden_reaction_cursor_id,
                   hidden_reaction_cursor_key,
                   deleted_messages,
                   deleted_reactions,
                   attempts,
                   lock_version,
                   last_error
            FROM chat_moderation_jobs
            WHERE id = $1
            "#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    pub async fn claim_batch(&self, worker_id: &str, limit: i64) -> Result<Vec<ChatModerationJob>> {
        let rows = sqlx::query_as!(
            ChatModerationJobRow,
            r#"
            WITH picked AS (
                SELECT id
                FROM chat_moderation_jobs
                WHERE status = 1
                  AND next_attempt_at <= NOW()
                ORDER BY updated_at, id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
            )
            UPDATE chat_moderation_jobs AS job
            SET status = 2,
                locked_by = $2,
                locked_at = NOW(),
                lock_version = lock_version + 1,
                updated_at = NOW()
            FROM picked
            WHERE job.id = picked.id
            RETURNING job.id,
                      job.room_id AS "room_id!: RoomId",
                      job.target_user_id AS "target_user_id!: UserId",
                      job.actor_user_id AS "actor_user_id!: UserId",
                      job.actor_username,
                      job.actor_role,
                      job.message_id,
                      job.explicit_message_done,
                      job.ban_user,
                      job.ban_done,
                      job.delete_all_messages,
                      job.delete_all_reactions,
                      job.reason,
                      job.phase AS "phase!",
                      job.status AS "status!",
                      job.snapshot_at,
                      job.message_cursor_created_at,
                      job.message_cursor_id,
                      job.reaction_cursor_created_at,
                      job.reaction_cursor_id,
                      job.hidden_reaction_cursor_created_at,
                      job.hidden_reaction_cursor_id,
                      job.hidden_reaction_cursor_key,
                      job.deleted_messages,
                      job.deleted_reactions,
                      job.attempts,
                      job.lock_version,
                      job.last_error
            "#,
            limit,
            worker_id,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn update_progress(
        &self,
        job: &ChatModerationJob,
        worker_id: &str,
        phase: ChatModerationJobPhase,
        message_cursor: Option<(DateTime<Utc>, i64)>,
        reaction_cursor: Option<(DateTime<Utc>, i64)>,
        hidden_reaction_cursor: Option<(DateTime<Utc>, i64, String)>,
        deleted_messages: i64,
        deleted_reactions: i64,
        explicit_message_done: bool,
        ban_done: bool,
    ) -> Result<bool> {
        let result = sqlx::query!(
            r#"
            UPDATE chat_moderation_jobs
            SET phase = $4,
                status = CASE
                    WHEN $16 THEN 2::SMALLINT
                    ELSE 1::SMALLINT
                END,
                message_cursor_created_at = $5,
                message_cursor_id = $6,
                reaction_cursor_created_at = $7,
                reaction_cursor_id = $8,
                hidden_reaction_cursor_created_at = $9,
                hidden_reaction_cursor_id = $10,
                hidden_reaction_cursor_key = $11,
                deleted_messages = $12,
                deleted_reactions = $13,
                explicit_message_done = $14,
                ban_done = $15,
                snapshot_at = $17,
                locked_by = CASE WHEN $16 THEN locked_by ELSE NULL END,
                locked_at = CASE WHEN $16 THEN NOW() ELSE NULL END,
                updated_at = NOW(),
                last_error = NULL,
                lock_version = lock_version + 1
            WHERE id = $1 AND locked_by = $2 AND lock_version = $3 AND status = 2
            "#,
            &job.id,
            worker_id,
            job.lock_version,
            phase as i16,
            message_cursor.map(|(at, _)| at),
            message_cursor.map(|(_, id)| id),
            reaction_cursor.map(|(at, _)| at),
            reaction_cursor.map(|(_, id)| id),
            hidden_reaction_cursor.as_ref().map(|(at, _, _)| *at),
            hidden_reaction_cursor.as_ref().map(|(_, id, _)| *id),
            hidden_reaction_cursor
                .as_ref()
                .map(|(_, _, key)| key.as_str()),
            deleted_messages,
            deleted_reactions,
            explicit_message_done,
            ban_done,
            phase == ChatModerationJobPhase::Done,
            job.snapshot_at,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn mark_completed(
        &self,
        job: &ChatModerationJob,
        worker_id: &str,
        deleted_messages: i64,
        deleted_reactions: i64,
    ) -> Result<bool> {
        let result = sqlx::query!(
            r#"
            UPDATE chat_moderation_jobs
            SET phase = 3,
                status = 3,
                deleted_messages = $4,
                deleted_reactions = $5,
                locked_by = NULL,
                locked_at = NULL,
                last_error = NULL,
                completed_at = NOW(),
                updated_at = NOW()
            WHERE id = $1 AND locked_by = $2 AND lock_version = $3 AND status = 2
            "#,
            &job.id,
            worker_id,
            job.lock_version,
            deleted_messages,
            deleted_reactions,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn mark_failed(
        &self,
        job: &ChatModerationJob,
        worker_id: &str,
        error: &str,
    ) -> Result<bool> {
        let attempts = job.attempts.saturating_add(1);
        let status = if attempts >= 10 { 4 } else { 1 };
        let retry_delay_seconds =
            f64::from(2_i32.saturating_pow(attempts.min(10) as u32).min(3600));
        let result = sqlx::query!(
            r#"
            UPDATE chat_moderation_jobs
            SET status = $4::SMALLINT,
                attempts = $5,
                next_attempt_at = CASE
                    WHEN $4::SMALLINT = 1 THEN NOW() + ($7::DOUBLE PRECISION * INTERVAL '1 second')
                    ELSE next_attempt_at
                END,
                locked_by = NULL,
                locked_at = NULL,
                last_error = $6,
                updated_at = NOW()
            WHERE id = $1 AND locked_by = $2 AND lock_version = $3 AND status = 2
            "#,
            &job.id,
            worker_id,
            job.lock_version,
            status,
            attempts,
            error,
            retry_delay_seconds,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn requeue_stale_processing(&self, stale_after_seconds: i64) -> Result<u64> {
        let result = sqlx::query!(
            r#"
            UPDATE chat_moderation_jobs
            SET status = CASE WHEN attempts + 1 >= 10 THEN 4 ELSE 1 END,
                attempts = attempts + 1,
                locked_by = NULL,
                locked_at = NULL,
                next_attempt_at = CASE
                    WHEN attempts + 1 >= 10 THEN next_attempt_at
                    ELSE NOW()
                END,
                last_error = 'Worker lease expired',
                updated_at = NOW()
            WHERE status = 2
              AND locked_at < NOW() - ($1::DOUBLE PRECISION * INTERVAL '1 second')
            "#,
            stale_after_seconds as f64,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn delete_terminal_before(&self, retention_seconds: i64) -> Result<u64> {
        let result = sqlx::query!(
            r#"
            DELETE FROM chat_moderation_jobs
            WHERE status IN (3, 4)
              AND updated_at < NOW() - ($1::DOUBLE PRECISION * INTERVAL '1 second')
            "#,
            retention_seconds as f64,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

fn validate_new_chat_moderation_job(job: &NewChatModerationJob) -> Result<()> {
    if !job.actor_role.is_admin_or_above() {
        return Err(Error::Authorization(
            "Admin role is required for chat moderation".to_string(),
        ));
    }
    if job.message_id.is_none()
        && !job.delete_all_messages
        && !job.delete_all_reactions
        && !job.ban_user
    {
        return Err(Error::InvalidInput(
            "At least one chat moderation action is required".to_string(),
        ));
    }
    if job.message_id.is_some_and(|id| id <= 0) {
        return Err(Error::InvalidInput(
            "Chat moderation message ID must be positive".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> NewChatModerationJob {
        NewChatModerationJob {
            id: "test".to_string(),
            room_id: RoomId::new(),
            target_user_id: UserId::new(),
            actor_user_id: UserId::new(),
            actor_username: "admin".to_string(),
            actor_role: UserRole::Admin,
            message_id: Some(1),
            ban_user: false,
            delete_all_messages: false,
            delete_all_reactions: false,
            reason: None,
            snapshot_at: Utc::now(),
        }
    }

    #[test]
    fn new_job_requires_an_action() {
        let mut job = job();
        job.message_id = None;
        assert!(matches!(
            validate_new_chat_moderation_job(&job),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn new_job_requires_an_admin_actor() {
        let mut job = job();
        job.actor_role = UserRole::User;
        assert!(matches!(
            validate_new_chat_moderation_job(&job),
            Err(Error::Authorization(_))
        ));
    }
}
