//! Data cleanup service for periodic maintenance tasks
//!
//! Coordinates cleanup of:
//! - Soft-deleted records (users, rooms, media) past retention period
//! - Expired email verification tokens
//! - Expired media provider credentials
//! - Old notifications
//! - Old chat messages (per-room cap)
//!
//! Runs as a background task with configurable intervals and retention periods.

use std::sync::Arc;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{Result, InternalExt};
use super::LeaderCheck;

/// Configuration for data cleanup retention periods
#[derive(Debug, Clone)]
pub struct CleanupConfig {
    /// Days to retain soft-deleted users before permanent deletion (0 = never purge)
    pub soft_delete_retention_days: u32,
    /// Days to retain soft-deleted rooms before permanent deletion (0 = never purge)
    pub room_soft_delete_retention_days: u32,
    /// Days to retain soft-deleted media before permanent deletion (0 = never purge)
    pub media_soft_delete_retention_days: u32,
    /// Days to retain expired email tokens before deletion (0 = never purge)
    pub expired_token_retention_days: u32,
    /// Hours buffer for expired credential cleanup (prevents race conditions)
    pub expired_credential_buffer_hours: u32,
    /// Days to retain read notifications before deletion (0 = never purge)
    pub notification_retention_days: u32,
    /// Days to retain any notification (read or unread) before deletion (0 = never purge)
    pub notification_max_retention_days: u32,
    /// Maximum chat messages to keep per room (0 = unlimited)
    pub chat_max_messages_per_room: i32,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            soft_delete_retention_days: 90,
            room_soft_delete_retention_days: 90,
            media_soft_delete_retention_days: 30,
            expired_token_retention_days: 7,
            expired_credential_buffer_hours: 1,
            notification_retention_days: 30,
            notification_max_retention_days: 90,
            chat_max_messages_per_room: 0, // unlimited by default
        }
    }
}

/// Result of a cleanup run
#[derive(Debug, Clone, Default)]
pub struct CleanupResult {
    /// Number of soft-deleted users permanently deleted
    pub users_purged: u64,
    /// Number of soft-deleted rooms permanently deleted
    pub rooms_purged: u64,
    /// Number of soft-deleted media permanently deleted
    pub media_purged: u64,
    /// Number of expired email tokens deleted
    pub tokens_deleted: u64,
    /// Number of expired credentials deleted
    pub credentials_deleted: u64,
    /// Number of old notifications deleted
    pub notifications_deleted: u64,
    /// Number of old chat messages deleted
    pub chat_messages_deleted: u64,
}

/// Data cleanup service
pub struct CleanupService {
    pool: PgPool,
    config: CleanupConfig,
    leader_check: Arc<dyn LeaderCheck>,
}

impl CleanupService {
    /// Create a new cleanup service with a leader check.
    ///
    /// Cleanup only runs when this node is the cluster leader (or in
    /// single-node mode where `AlwaysLeader` is used).
    #[must_use]
    pub fn new(pool: PgPool, config: CleanupConfig, leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self { pool, config, leader_check }
    }

    /// Run all cleanup tasks once
    ///
    /// Each task runs independently; if one fails, others still execute.
    /// Returns a summary of what was cleaned up.
    pub async fn run_all(&self) -> CleanupResult {
        let mut result = CleanupResult::default();

        // 1. Purge soft-deleted users
        if self.config.soft_delete_retention_days > 0 {
            match self.purge_soft_deleted_users().await {
                Ok(count) => {
                    result.users_purged = count;
                    if count > 0 {
                        info!(count, "Purged soft-deleted users");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to purge soft-deleted users"),
            }
        }

        // 2. Purge soft-deleted rooms
        if self.config.room_soft_delete_retention_days > 0 {
            match self.purge_soft_deleted_rooms().await {
                Ok(count) => {
                    result.rooms_purged = count;
                    if count > 0 {
                        info!(count, "Purged soft-deleted rooms");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to purge soft-deleted rooms"),
            }
        }

        // 3. Purge soft-deleted media
        if self.config.media_soft_delete_retention_days > 0 {
            match self.purge_soft_deleted_media().await {
                Ok(count) => {
                    result.media_purged = count;
                    if count > 0 {
                        info!(count, "Purged soft-deleted media");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to purge soft-deleted media"),
            }
        }

        // 4. Delete expired email tokens
        if self.config.expired_token_retention_days > 0 {
            match self.delete_expired_tokens().await {
                Ok(count) => {
                    result.tokens_deleted = count;
                    if count > 0 {
                        info!(count, "Deleted expired email tokens");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to delete expired email tokens"),
            }
        }

        // 4b. Delete expired media provider credentials
        if self.config.expired_credential_buffer_hours > 0 {
            match self.delete_expired_credentials().await {
                Ok(count) => {
                    result.credentials_deleted = count;
                    if count > 0 {
                        info!(count, "Deleted expired media provider credentials");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to delete expired credentials"),
            }
        }

        // 5. Delete old read notifications
        if self.config.notification_retention_days > 0 {
            match self.delete_old_notifications().await {
                Ok(count) => {
                    result.notifications_deleted += count;
                    if count > 0 {
                        info!(count, "Deleted old read notifications");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to delete old read notifications"),
            }
        }

        // 5b. Delete all notifications (including unread) past max retention
        if self.config.notification_max_retention_days > 0 {
            match self.delete_expired_notifications().await {
                Ok(count) => {
                    result.notifications_deleted += count;
                    if count > 0 {
                        info!(count, "Deleted expired notifications (past max retention)");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to delete expired notifications"),
            }
        }

        // 6. Cleanup chat messages (per-room cap)
        if self.config.chat_max_messages_per_room > 0 {
            match self.cleanup_chat_messages().await {
                Ok(count) => {
                    result.chat_messages_deleted = count;
                    if count > 0 {
                        info!(count, "Cleaned up old chat messages");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to cleanup chat messages"),
            }
        }

        result
    }

    /// Permanently delete users that were soft-deleted beyond the retention period
    async fn purge_soft_deleted_users(&self) -> Result<u64> {
        let days = self.config.soft_delete_retention_days as i32;
        let result = sqlx::query(
            r"
            DELETE FROM users
            WHERE deleted_at IS NOT NULL
              AND deleted_at < CURRENT_TIMESTAMP - ($1 || ' days')::INTERVAL
            ",
        )
        .bind(days)
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to purge soft-deleted users")?;

        Ok(result.rows_affected())
    }

    /// Permanently delete rooms that were soft-deleted beyond the retention period
    async fn purge_soft_deleted_rooms(&self) -> Result<u64> {
        let days = self.config.room_soft_delete_retention_days as i32;
        let result = sqlx::query(
            r"
            DELETE FROM rooms
            WHERE deleted_at IS NOT NULL
              AND deleted_at < CURRENT_TIMESTAMP - ($1 || ' days')::INTERVAL
            ",
        )
        .bind(days)
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to purge soft-deleted rooms")?;

        Ok(result.rows_affected())
    }

    /// Permanently delete media that were soft-deleted beyond the retention period
    async fn purge_soft_deleted_media(&self) -> Result<u64> {
        let days = self.config.media_soft_delete_retention_days as i32;
        let result = sqlx::query(
            r"
            DELETE FROM media
            WHERE deleted_at IS NOT NULL
              AND deleted_at < CURRENT_TIMESTAMP - ($1 || ' days')::INTERVAL
            ",
        )
        .bind(days)
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to purge soft-deleted media")?;

        Ok(result.rows_affected())
    }

    /// Delete email tokens that expired beyond the retention period
    async fn delete_expired_tokens(&self) -> Result<u64> {
        let days = self.config.expired_token_retention_days as i32;
        let result = sqlx::query(
            r"
            DELETE FROM email_tokens
            WHERE expires_at < CURRENT_TIMESTAMP - ($1 || ' days')::INTERVAL
            ",
        )
        .bind(days)
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to delete expired tokens")?;

        Ok(result.rows_affected())
    }

    /// Delete expired media provider credentials with buffer to prevent race conditions
    ///
    /// Calls the database function cleanup_expired_credentials() which deletes credentials
    /// that expired more than buffer_hours ago.
    async fn delete_expired_credentials(&self) -> Result<u64> {
        let buffer_hours = self.config.expired_credential_buffer_hours as i32;
        let result_json = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT cleanup_expired_credentials($1)"
        )
        .bind(buffer_hours)
        .fetch_one(&self.pool)
        .await
        .internal_with_err("Failed to delete expired credentials")?;

        let deleted_count = result_json["deleted_count"]
            .as_i64()
            .unwrap_or(0) as u64;

        Ok(deleted_count)
    }

    /// Delete read notifications older than the retention period
    async fn delete_old_notifications(&self) -> Result<u64> {
        let days = self.config.notification_retention_days as i32;
        let result = sqlx::query(
            r"
            DELETE FROM notifications
            WHERE is_read = TRUE
              AND created_at < CURRENT_TIMESTAMP - ($1 || ' days')::INTERVAL
            ",
        )
        .bind(days)
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to delete old notifications")?;

        Ok(result.rows_affected())
    }

    /// Delete all notifications (including unread) older than the max retention period
    ///
    /// This prevents unbounded growth from unread notifications that are never
    /// acknowledged by users.
    async fn delete_expired_notifications(&self) -> Result<u64> {
        let days = self.config.notification_max_retention_days as i32;
        let result = sqlx::query(
            r"
            DELETE FROM notifications
            WHERE created_at < CURRENT_TIMESTAMP - ($1 || ' days')::INTERVAL
            ",
        )
        .bind(days)
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to delete expired notifications")?;

        Ok(result.rows_affected())
    }

    /// Cleanup chat messages exceeding per-room cap
    ///
    /// Uses window functions for efficient batch cleanup across all rooms.
    async fn cleanup_chat_messages(&self) -> Result<u64> {
        let keep_count = self.config.chat_max_messages_per_room;
        if keep_count <= 0 {
            return Ok(0);
        }

        let result = sqlx::query(
            r"
            DELETE FROM chat_messages
            WHERE id IN (
                SELECT id FROM (
                    SELECT id,
                           ROW_NUMBER() OVER (PARTITION BY room_id ORDER BY created_at DESC) as rn
                    FROM chat_messages
                ) ranked
                WHERE rn > $1
            )
            ",
        )
        .bind(keep_count)
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to cleanup chat messages")?;

        Ok(result.rows_affected())
    }

    /// Start the periodic cleanup background task
    ///
    /// Runs cleanup at the specified interval. Shuts down gracefully when
    /// the `CancellationToken` is cancelled.
    ///
    /// # Arguments
    /// * `interval_hours` - Hours between cleanup runs (e.g., 24 for daily)
    /// * `cancel` - Cancellation token for graceful shutdown
    #[must_use]
    pub fn start_periodic(
        &self,
        interval_hours: u64,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let service = Self {
            pool: self.pool.clone(),
            config: self.config.clone(),
            leader_check: self.leader_check.clone(),
        };

        crate::spawn::spawn_monitored("data_cleanup", async move {
            let interval_duration = tokio::time::Duration::from_secs(interval_hours * 3600);
            let mut interval = tokio::time::interval(interval_duration);

            // Skip the first immediate tick -- let the app fully start up first
            interval.tick().await;

            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    () = cancel.cancelled() => {
                        info!("Data cleanup task cancelled, shutting down");
                        return;
                    }
                }

                // Only run cleanup on the leader node to avoid duplicate work
                if !service.leader_check.is_leader() {
                    info!("Skipping data cleanup (not leader)");
                    continue;
                }

                info!(
                    room_retention_days = service.config.room_soft_delete_retention_days,
                    user_retention_days = service.config.soft_delete_retention_days,
                    media_retention_days = service.config.media_soft_delete_retention_days,
                    "Starting periodic data cleanup"
                );
                let result = service.run_all().await;

                let total = result.users_purged
                    + result.rooms_purged
                    + result.media_purged
                    + result.tokens_deleted
                    + result.credentials_deleted
                    + result.notifications_deleted
                    + result.chat_messages_deleted;

                if total > 0 {
                    info!(
                        users = result.users_purged,
                        rooms = result.rooms_purged,
                        media = result.media_purged,
                        tokens = result.tokens_deleted,
                        credentials = result.credentials_deleted,
                        notifications = result.notifications_deleted,
                        chat_messages = result.chat_messages_deleted,
                        total,
                        "Data cleanup completed"
                    );
                } else {
                    info!("Data cleanup completed, nothing to clean");
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cleanup_config_default() {
        let config = CleanupConfig::default();
        assert_eq!(config.soft_delete_retention_days, 90);
        assert_eq!(config.room_soft_delete_retention_days, 90);
        assert_eq!(config.media_soft_delete_retention_days, 30);
        assert_eq!(config.expired_token_retention_days, 7);
        assert_eq!(config.expired_credential_buffer_hours, 1);
        assert_eq!(config.notification_retention_days, 30);
        assert_eq!(config.notification_max_retention_days, 90);
        assert_eq!(config.chat_max_messages_per_room, 0);
    }

    #[test]
    fn test_cleanup_result_default() {
        let result = CleanupResult::default();
        assert_eq!(result.users_purged, 0);
        assert_eq!(result.rooms_purged, 0);
        assert_eq!(result.media_purged, 0);
        assert_eq!(result.tokens_deleted, 0);
        assert_eq!(result.credentials_deleted, 0);
        assert_eq!(result.notifications_deleted, 0);
        assert_eq!(result.chat_messages_deleted, 0);
    }

    #[test]
    fn test_cleanup_config_custom() {
        let config = CleanupConfig {
            soft_delete_retention_days: 30,
            room_soft_delete_retention_days: 60,
            media_soft_delete_retention_days: 14,
            expired_token_retention_days: 3,
            expired_credential_buffer_hours: 2,
            notification_retention_days: 14,
            notification_max_retention_days: 60,
            chat_max_messages_per_room: 1000,
        };
        assert_eq!(config.soft_delete_retention_days, 30);
        assert_eq!(config.room_soft_delete_retention_days, 60);
        assert_eq!(config.media_soft_delete_retention_days, 14);
        assert_eq!(config.expired_token_retention_days, 3);
        assert_eq!(config.expired_credential_buffer_hours, 2);
        assert_eq!(config.notification_retention_days, 14);
        assert_eq!(config.notification_max_retention_days, 60);
        assert_eq!(config.chat_max_messages_per_room, 1000);
    }

    #[test]
    fn test_cleanup_config_zero_disables() {
        let config = CleanupConfig {
            soft_delete_retention_days: 0,
            room_soft_delete_retention_days: 0,
            media_soft_delete_retention_days: 0,
            expired_token_retention_days: 0,
            expired_credential_buffer_hours: 0,
            notification_retention_days: 0,
            notification_max_retention_days: 0,
            chat_max_messages_per_room: 0,
        };
        // All zero means all cleanup is disabled
        assert_eq!(config.soft_delete_retention_days, 0);
        assert_eq!(config.expired_credential_buffer_hours, 0);
        assert_eq!(config.notification_max_retention_days, 0);
        assert_eq!(config.chat_max_messages_per_room, 0);
    }

    #[test]
    fn test_cleanup_result_total() {
        let result = CleanupResult {
            users_purged: 5,
            rooms_purged: 3,
            media_purged: 10,
            tokens_deleted: 20,
            credentials_deleted: 15,
            notifications_deleted: 50,
            chat_messages_deleted: 100,
        };
        let total = result.users_purged
            + result.rooms_purged
            + result.media_purged
            + result.tokens_deleted
            + result.credentials_deleted
            + result.notifications_deleted
            + result.chat_messages_deleted;
        assert_eq!(total, 203);
    }
}
