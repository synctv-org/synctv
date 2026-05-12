use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    models::{ChatMessage, RoomId, UserId},
    Result,
};

/// Chat message repository for database operations
#[derive(Clone)]
pub struct ChatRepository {
    pool: PgPool,
}

impl ChatRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new chat message
    pub async fn create(&self, message: &ChatMessage) -> Result<ChatMessage> {
        let msg = sqlx::query_as!(
            ChatMessage,
            r#"
            INSERT INTO chat_messages (room_id, user_id, content, message_type, created_at)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id,
                      room_id as "room_id: RoomId",
                      user_id as "user_id: UserId",
                      content,
                      message_type,
                      created_at
            "#,
            message.room_id as RoomId,
            message.user_id as Option<UserId>,
            message.content.as_str(),
            message.message_type,
            message.created_at,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(msg)
    }

    /// Get chat history using keyset (cursor) pagination.
    ///
    /// Returns at most `limit` messages (capped at 100) ordered by
    /// `(created_at DESC, id DESC)`. The cursor is a composite of
    /// `(cursor_created_at, cursor_id)` — this avoids relying on base62 ID string
    /// comparison which is NOT lexicographically sortable by time.
    ///
    /// Returns `(messages, next_cursor)` where `next_cursor` is the
    /// `(created_at, id)` of the oldest returned message (to be used in the
    /// next call), or `None` when no more messages exist.
    pub async fn list_by_room_cursor(
        &self,
        room_id: &RoomId,
        cursor: Option<(DateTime<Utc>, i64)>,
        limit: i32,
    ) -> Result<(Vec<ChatMessage>, Option<(DateTime<Utc>, i64)>)> {
        // Enforce maximum page size to prevent OOM on very large rooms
        let limit = limit.clamp(1, 100);

        let messages = if let Some((cursor_created_at, cursor_id)) = cursor {
            sqlx::query_as!(
                ChatMessage,
                r#"
                SELECT id,
                       room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       content,
                       message_type,
                       created_at
                FROM chat_messages
                WHERE room_id = $1 AND (created_at, id) < ($2, $3)
                ORDER BY created_at DESC, id DESC
                LIMIT $4::int4
                "#,
                room_id as &RoomId,
                cursor_created_at,
                cursor_id,
                limit,
            )
            .fetch_all(&self.pool)
            .await?
        } else {
            // Initial load (no cursor): add a created_at lower bound so PostgreSQL
            // can prune old partitions. Matches the 90-day retention period.
            sqlx::query_as!(
                ChatMessage,
                r#"
                SELECT id,
                       room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       content,
                       message_type,
                       created_at
                FROM chat_messages
                WHERE room_id = $1
                  AND created_at >= NOW() - INTERVAL '90 days'
                ORDER BY created_at DESC, id DESC
                LIMIT $2::int4
                "#,
                room_id as &RoomId,
                limit,
            )
            .fetch_all(&self.pool)
            .await?
        };

        // Determine next cursor: the (created_at, id) of the oldest (last)
        // message in this page. If we got a full page there may be more;
        // otherwise we're at the beginning.
        let next_cursor = if i32::try_from(messages.len()).ok() == Some(limit) {
            messages.last().map(|m| (m.created_at, m.id))
        } else {
            None
        };

        Ok((messages, next_cursor))
    }

    /// Get a specific message by ID
    ///
    /// Queries by primary key without time restriction. This allows fetching
    /// any message for audit/logging scenarios where historical messages may
    /// need to be retrieved.
    ///
    /// Note: Without a `created_at` filter, this query cannot use partition
    /// pruning and may scan multiple partitions. However, since this is a
    /// primary key lookup, it remains efficient.
    pub async fn get_by_id(&self, message_id: i64) -> Result<Option<ChatMessage>> {
        let msg = sqlx::query_as!(
            ChatMessage,
            r#"
            SELECT id,
                   room_id as "room_id: RoomId",
                   user_id as "user_id: UserId",
                   content,
                   message_type,
                   created_at
            FROM chat_messages
            WHERE id = $1
            "#,
            message_id,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(msg)
    }

    /// Get a specific message by ID, scoped to a room.
    pub async fn get_by_room_and_id(
        &self,
        room_id: &RoomId,
        message_id: i64,
    ) -> Result<Option<ChatMessage>> {
        let msg = sqlx::query_as::<_, ChatMessage>(
            r"
            SELECT id,
                   room_id,
                   user_id,
                   content,
                   message_type,
                   created_at
            FROM chat_messages
            WHERE room_id = $1 AND id = $2
            ",
        )
        .bind(room_id.as_i64())
        .bind(message_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(msg)
    }

    /// Delete a message (physical delete)
    ///
    /// Requires `created_at` to enable partition pruning. Without it, `PostgreSQL`
    /// would scan all partitions to find the row.
    pub async fn delete(&self, message_id: i64, created_at: DateTime<Utc>) -> Result<bool> {
        let result = sqlx::query!(
            r"
            DELETE FROM chat_messages
            WHERE id = $1
              AND created_at = $2
            ",
            message_id,
            created_at,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete a message scoped to a room.
    pub async fn delete_in_room(
        &self,
        room_id: &RoomId,
        message_id: i64,
        created_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = sqlx::query!(
            r"
            DELETE FROM chat_messages
            WHERE room_id = $1
              AND id = $2
              AND created_at = $3
            ",
            room_id.as_i64(),
            message_id,
            created_at
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Get message count for a room.
    ///
    /// Scans only recent partitions (last 90 days) to avoid full partition scan.
    /// This matches the default retention period for chat messages.
    pub async fn count_by_room(&self, room_id: &RoomId) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r"
            SELECT COUNT(*) as count
            FROM chat_messages
            WHERE room_id = $1
              AND created_at >= NOW() - INTERVAL '90 days'
            ",
            room_id as &RoomId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count.unwrap_or(0))
    }

    /// Delete old messages for a room (keep only last N messages).
    ///
    /// Uses `ROW_NUMBER()` window function instead of `NOT IN` subquery for
    /// better performance. Limits the scan to messages within the retention
    /// window (90 days) to enable partition pruning and avoid scanning all
    /// historical partitions.
    ///
    /// The redundant `created_at > NOW() - INTERVAL '90 days'` filter on the
    /// outer DELETE ensures `PostgreSQL` can apply partition pruning at the
    /// top-level query without relying on constraint exclusion from the subquery.
    pub async fn cleanup_old_messages(&self, room_id: &RoomId, keep_count: i32) -> Result<u64> {
        if keep_count <= 0 {
            return Ok(0);
        }

        let result = sqlx::query!(
            r"
            DELETE FROM chat_messages
            WHERE room_id = $1
              AND created_at > NOW() - INTERVAL '90 days'
              AND (id, created_at) IN (
                SELECT id, created_at FROM (
                    SELECT id, created_at,
                           ROW_NUMBER() OVER (ORDER BY created_at DESC) as rn
                    FROM chat_messages
                    WHERE room_id = $1
                      AND created_at > NOW() - INTERVAL '90 days'
                ) ranked
                WHERE rn > $2::int4
            )
            ",
            room_id as &RoomId,
            keep_count,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Delete ALL messages older than the absolute retention period (90 days).
    ///
    /// This enforces the hard 90-day cap for rooms that haven't had recent activity
    /// and would never be reached by the per-room count-based cleanup, which only
    /// processes rooms within the activity window.
    ///
    /// Should be called periodically (e.g., daily) as a background maintenance task.
    ///
    /// # Returns
    /// Total number of messages deleted
    pub async fn delete_messages_older_than_retention(&self) -> Result<u64> {
        let result = sqlx::query!(
            r"
            DELETE FROM chat_messages
            WHERE created_at <= NOW() - INTERVAL '90 days'
            ",
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Delete old messages for all rooms in a single query (keep only last N messages per room)
    ///
    /// This is much more efficient than calling `cleanup_old_messages()` for each room individually.
    /// Uses window functions to identify messages to delete across all rooms.
    /// Only processes rooms with recent activity (messages within the last few minutes).
    ///
    /// # Arguments
    /// * `keep_count` - Maximum messages to keep per room (0 = unlimited, no cleanup)
    /// * `activity_window_minutes` - Only cleanup rooms with messages in the last N minutes
    ///
    /// # Returns
    /// Total number of messages deleted across all rooms
    ///
    /// # Partition Pruning
    ///
    /// The redundant `created_at > NOW() - INTERVAL '90 days'` filter on the outer DELETE
    /// ensures `PostgreSQL` can apply partition pruning at the top-level query without
    /// relying on constraint exclusion from the subquery.
    pub async fn cleanup_all_rooms(
        &self,
        keep_count: i32,
        activity_window_minutes: i32,
    ) -> Result<u64> {
        // If keep_count is 0, no cleanup needed
        if keep_count <= 0 {
            return Ok(0);
        }

        let result = sqlx::query!(
            r"
            DELETE FROM chat_messages
            WHERE created_at > NOW() - INTERVAL '90 days'
              AND (id, created_at) IN (
                SELECT id, created_at FROM (
                    SELECT id, created_at, room_id,
                           ROW_NUMBER() OVER (PARTITION BY room_id ORDER BY created_at DESC) as rn
                    FROM chat_messages
                    WHERE room_id IN (
                        SELECT DISTINCT room_id
                        FROM chat_messages
                        WHERE created_at >= NOW() - make_interval(mins => $2)
                    )
                      AND created_at > NOW() - INTERVAL '90 days'
                ) ranked_messages
                WHERE rn > $1::int4
            )
            ",
            keep_count,
            activity_window_minutes,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}
