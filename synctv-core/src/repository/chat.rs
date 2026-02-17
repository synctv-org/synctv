use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    models::{ChatMessage, RoomId},
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
        let msg = sqlx::query_as::<_, ChatMessage>(
            r"
            INSERT INTO chat_messages (id, room_id, user_id, content, message_type, created_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, room_id, user_id, content, message_type, created_at
            ",
        )
        .bind(&message.id)
        .bind(message.room_id.as_str())
        .bind(message.user_id.as_str())
        .bind(&message.content)
        .bind(message.message_type)
        .bind(message.created_at)
        .fetch_one(&self.pool)
        .await?;

        Ok(msg)
    }

    /// Get chat history for a room
    /// Returns messages in reverse chronological order (newest first)
    pub async fn list_by_room(
        &self,
        room_id: &RoomId,
        before: Option<DateTime<Utc>>,
        limit: i32,
    ) -> Result<Vec<ChatMessage>> {
        let limit = limit.min(100); // Cap at 100 messages per request

        let messages = if let Some(before_time) = before {
            sqlx::query_as::<_, ChatMessage>(
                r"
                SELECT id, room_id, user_id, content, message_type, created_at
                FROM chat_messages
                WHERE room_id = $1 AND created_at < $2
                ORDER BY created_at DESC
                LIMIT $3
                ",
            )
            .bind(room_id.as_str())
            .bind(before_time)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, ChatMessage>(
                r"
                SELECT id, room_id, user_id, content, message_type, created_at
                FROM chat_messages
                WHERE room_id = $1
                ORDER BY created_at DESC
                LIMIT $2
                ",
            )
            .bind(room_id.as_str())
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(messages)
    }

    /// Get a specific message by ID
    ///
    /// Scans only recent partitions (last 90 days) to avoid full partition scan.
    /// This matches the default retention period for chat messages.
    pub async fn get_by_id(&self, message_id: &str) -> Result<Option<ChatMessage>> {
        let msg = sqlx::query_as::<_, ChatMessage>(
            r"
            SELECT id, room_id, user_id, content, message_type, created_at
            FROM chat_messages
            WHERE id = $1
              AND created_at >= NOW() - INTERVAL '90 days'
            ",
        )
        .bind(message_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(msg)
    }

    /// Delete a message (physical delete)
    ///
    /// Requires `created_at` to enable partition pruning. Without it, PostgreSQL
    /// would scan all partitions to find the row.
    pub async fn delete(&self, message_id: &str, created_at: DateTime<Utc>) -> Result<bool> {
        let result = sqlx::query(
            r"
            DELETE FROM chat_messages
            WHERE id = $1
              AND created_at = $2
            ",
        )
        .bind(message_id)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Get message count for a room.
    ///
    /// Scans only recent partitions (last 90 days) to avoid full partition scan.
    /// This matches the default retention period for chat messages.
    pub async fn count_by_room(&self, room_id: &RoomId) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*) as count
            FROM chat_messages
            WHERE room_id = $1
              AND created_at >= NOW() - INTERVAL '90 days'
            ",
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Delete old messages for a room (keep only last N messages).
    ///
    /// Uses `ROW_NUMBER()` window function instead of `NOT IN` subquery for
    /// better performance.
    pub async fn cleanup_old_messages(&self, room_id: &RoomId, keep_count: i32) -> Result<u64> {
        if keep_count <= 0 {
            return Ok(0);
        }

        let result = sqlx::query(
            r"
            DELETE FROM chat_messages
            WHERE (id, created_at) IN (
                SELECT id, created_at FROM (
                    SELECT id, created_at,
                           ROW_NUMBER() OVER (ORDER BY created_at DESC) as rn
                    FROM chat_messages
                    WHERE room_id = $1
                ) ranked
                WHERE rn > $2
            )
            ",
        )
        .bind(room_id.as_str())
        .bind(keep_count)
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
    pub async fn cleanup_all_rooms(&self, keep_count: i32, activity_window_minutes: i32) -> Result<u64> {
        // If keep_count is 0, no cleanup needed
        if keep_count <= 0 {
            return Ok(0);
        }

        let result = sqlx::query(
            r"
            DELETE FROM chat_messages
            WHERE (id, created_at) IN (
                SELECT id, created_at FROM (
                    SELECT id, created_at, room_id,
                           ROW_NUMBER() OVER (PARTITION BY room_id ORDER BY created_at DESC) as rn
                    FROM chat_messages
                    WHERE room_id IN (
                        SELECT DISTINCT room_id
                        FROM chat_messages
                        WHERE created_at >= NOW() - make_interval(mins => $2)
                    )
                ) ranked_messages
                WHERE rn > $1
            )
            ",
        )
        .bind(keep_count)
        .bind(activity_window_minutes)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

}

