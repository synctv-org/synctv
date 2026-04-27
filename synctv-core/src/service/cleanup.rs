//! Data cleanup service for periodic maintenance tasks
//!
//! Coordinates cleanup of:
//! - Rooms past `room_ttl` threshold (soft-delete)
//! - Soft-deleted records (users, rooms) past retention period
//! - Expired email verification tokens
//! - Expired media provider credentials
//! - Old notifications
//! - Old chat messages (per-room cap)
//!
//! Runs as a background task with configurable intervals and retention periods.
//!
//! # Dynamic Settings
//!
//! The `room_ttl_seconds` and `chat_max_messages_per_room` settings can be
//! dynamically configured via `SettingsRegistry`. When a registry is provided,
//! these values are read at runtime on each cleanup cycle, allowing admins to
//! change settings without restarting the service.

use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{LeaderCheck, SettingsRegistry};
use crate::{InternalExt, Result};

/// Configuration for data cleanup retention periods
#[derive(Debug, Clone)]
pub struct CleanupConfig {
    /// Room TTL in seconds (0 = never expire). Rooms with `last_activity_at` older than this are soft-deleted.
    pub room_ttl_seconds: i64,
    /// Days to retain soft-deleted users before permanent deletion (0 = never purge)
    pub soft_delete_retention_days: u32,
    /// Days to retain soft-deleted rooms before permanent deletion (0 = never purge)
    pub room_soft_delete_retention_days: u32,
    /// Days to retain expired email tokens before deletion (0 = never purge)
    pub expired_token_retention_days: u32,
    /// Hours buffer for expired credential cleanup (prevents race conditions)
    pub expired_credential_buffer_hours: u32,
    /// Days to retain read notifications before deletion (0 = never purge)
    pub notification_retention_days: u32,
    /// Days to retain any notification (read or unread) before deletion (0 = never purge)
    pub notification_max_retention_days: u32,
    /// Maximum chat messages to keep per room (0 = unlimited)
    pub chat_max_messages_per_room: i64,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            room_ttl_seconds: 172_800, // 48 hours in seconds (matches global settings default)
            soft_delete_retention_days: 90,
            room_soft_delete_retention_days: 90,
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
    /// Number of rooms soft-deleted due to `room_ttl` expiration
    pub rooms_expired: u64,
    /// Number of expired email tokens deleted
    pub tokens_deleted: u64,
    /// Number of expired credentials deleted
    pub credentials_deleted: u64,
    /// Number of old notifications deleted
    pub notifications_deleted: u64,
    /// Number of old chat messages deleted
    pub chat_messages_deleted: u64,
    /// Number of expired token blacklist entries deleted
    pub token_blacklist_deleted: u64,
}

/// Data cleanup service
pub struct CleanupService {
    pool: PgPool,
    config: CleanupConfig,
    leader_check: Arc<dyn LeaderCheck>,
    /// Optional settings registry for dynamic `room_ttl` and `chat_max_messages_per_room`
    settings_registry: Option<Arc<SettingsRegistry>>,
}

impl CleanupService {
    fn u32_to_i32_saturating(value: u32) -> i32 {
        i32::try_from(value).unwrap_or(i32::MAX)
    }

    /// Create a new cleanup service with a leader check.
    ///
    /// Cleanup only runs when this node is the cluster leader (or in
    /// single-node mode where `AlwaysLeader` is used).
    #[must_use]
    pub fn new(pool: PgPool, config: CleanupConfig, leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self {
            pool,
            config,
            leader_check,
            settings_registry: None,
        }
    }

    /// Set the settings registry for dynamic `room_ttl` and `chat_max_messages_per_room`.
    ///
    /// When set, `room_ttl_seconds` and `chat_max_messages_per_room` are read from
    /// the registry at runtime on each cleanup cycle, allowing admins to change
    /// these settings without restarting the service.
    #[must_use]
    pub fn with_settings_registry(mut self, registry: Arc<SettingsRegistry>) -> Self {
        self.settings_registry = Some(registry);
        self
    }

    /// Get the effective `room_ttl_seconds` value.
    ///
    /// Reads from `SettingsRegistry` if available, otherwise falls back to config.
    fn room_ttl_seconds(&self) -> i64 {
        self.settings_registry
            .as_ref()
            .and_then(|r| r.room_ttl.get().ok())
            .unwrap_or(self.config.room_ttl_seconds)
    }

    /// Get the effective `chat_max_messages_per_room` value.
    ///
    /// Reads from `SettingsRegistry` if available, otherwise falls back to config.
    fn chat_max_messages_per_room(&self) -> i64 {
        self.settings_registry
            .as_ref()
            .and_then(|r| r.max_chat_messages_per_room.get().ok())
            .map_or(self.config.chat_max_messages_per_room, |value| {
                i64::try_from(value).unwrap_or(i64::MAX)
            })
    }

    /// Run all cleanup tasks once
    ///
    /// Each task runs independently; if one fails, others still execute.
    /// Returns a summary of what was cleaned up.
    pub async fn run_all(&self) -> CleanupResult {
        // Short-circuit if we are not the leader to avoid duplicate work
        // across cluster replicas. This guards direct `run_all()` callers;
        // the periodic loop in `start_periodic` has its own check but calling
        // `run_all()` directly (e.g. from an admin endpoint) should also be safe.
        if !self.leader_check.is_leader() {
            return CleanupResult::default();
        }

        let mut result = CleanupResult::default();

        // Read dynamic settings at runtime
        let room_ttl_seconds = self.room_ttl_seconds();
        let chat_max_messages = self.chat_max_messages_per_room();

        // 0. Soft-delete rooms past room_ttl threshold
        if room_ttl_seconds > 0 {
            match self.soft_delete_expired_rooms(room_ttl_seconds).await {
                Ok(count) => {
                    result.rooms_expired = count;
                    if count > 0 {
                        info!(count, "Soft-deleted expired rooms (past room_ttl)");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to soft-delete expired rooms"),
            }
        }

        // 1. Purge soft-deleted rooms first.
        // Soft-deleted users can still be referenced by their owned room rows
        // (`rooms.created_by` is `ON DELETE RESTRICT`). Purging rooms before
        // users lets a single cleanup run fully retire the common "user deleted
        // together with their rooms" path instead of deferring user purge by a
        // whole cleanup interval.
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

        // 2. Purge soft-deleted users
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

        // 3. Delete expired email tokens
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
        if chat_max_messages > 0 {
            match self.cleanup_chat_messages(chat_max_messages).await {
                Ok(count) => {
                    result.chat_messages_deleted = count;
                    if count > 0 {
                        info!(count, "Cleaned up old chat messages");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to cleanup chat messages"),
            }
        }

        // 7. Cleanup expired token blacklist entries (prevents unbounded table growth)
        match self.cleanup_token_blacklist().await {
            Ok(count) => {
                result.token_blacklist_deleted = count;
                if count > 0 {
                    info!(count, "Deleted expired token blacklist entries");
                }
            }
            Err(e) => warn!(error = %e, "Failed to cleanup token blacklist"),
        }

        result
    }

    /// Soft-delete rooms that have exceeded the `room_ttl` threshold.
    ///
    /// Rooms with `last_activity_at` older than `ttl_seconds` ago are soft-deleted
    /// by setting `deleted_at = CURRENT_TIMESTAMP`. This prevents unbounded room
    /// growth and ensures inactive rooms are eventually cleaned up.
    ///
    /// Only affects rooms that are not already soft-deleted.
    async fn soft_delete_expired_rooms(&self, ttl_seconds: i64) -> Result<u64> {
        let result = sqlx::query(
            r"
            UPDATE rooms
            SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP, version = version + 1
            WHERE deleted_at IS NULL
              AND last_activity_at < CURRENT_TIMESTAMP - ($1 || ' seconds')::INTERVAL
            ",
        )
        .bind(ttl_seconds)
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to soft-delete expired rooms")?;

        Ok(result.rows_affected())
    }

    /// Permanently delete users that were soft-deleted beyond the retention period
    async fn purge_soft_deleted_users(&self) -> Result<u64> {
        let days = Self::u32_to_i32_saturating(self.config.soft_delete_retention_days);
        let user_ids: Vec<crate::models::UserId> = sqlx::query_scalar(
            r"
            SELECT id
            FROM users
            WHERE deleted_at IS NOT NULL
              AND deleted_at < CURRENT_TIMESTAMP - ($1 || ' days')::INTERVAL
            ORDER BY deleted_at ASC, id ASC
            ",
        )
        .bind(days)
        .fetch_all(&self.pool)
        .await
        .internal_with_err("Failed to list soft-deleted users for purge")?;

        let mut purged = 0u64;
        for user_id in user_ids {
            let mut tx = self.pool.begin().await?;

            // User soft-delete keeps historical memberships by marking them as
            // `left`. Those rows still carry `ON DELETE RESTRICT` FKs, so they
            // must be removed before the hard delete can succeed.
            sqlx::query("DELETE FROM room_members WHERE user_id = $1")
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .internal_with_err("Failed to delete room memberships during user purge")?;

            match sqlx::query(
                r"
                DELETE FROM users
                WHERE id = $1
                  AND deleted_at IS NOT NULL
                  AND deleted_at < CURRENT_TIMESTAMP - ($2 || ' days')::INTERVAL
                ",
            )
            .bind(user_id)
            .bind(days)
            .execute(&mut *tx)
            .await
            {
                Ok(result) => {
                    tx.commit()
                        .await
                        .internal_with_err("Failed to commit user purge transaction")?;
                    purged += result.rows_affected();
                }
                Err(error) => {
                    warn!(
                        user_id = %user_id,
                        error = %error,
                        "Failed to purge soft-deleted user; leaving row for a future retry"
                    );
                }
            }
        }

        Ok(purged)
    }

    /// Permanently delete rooms that were soft-deleted beyond the retention period
    async fn purge_soft_deleted_rooms(&self) -> Result<u64> {
        let days = Self::u32_to_i32_saturating(self.config.room_soft_delete_retention_days);
        let room_ids: Vec<crate::models::RoomId> = sqlx::query_scalar(
            r"
            SELECT id
            FROM rooms
            WHERE deleted_at IS NOT NULL
              AND deleted_at < CURRENT_TIMESTAMP - ($1 || ' days')::INTERVAL
            ORDER BY deleted_at ASC, id ASC
            ",
        )
        .bind(days)
        .fetch_all(&self.pool)
        .await
        .internal_with_err("Failed to list soft-deleted rooms for purge")?;

        let mut purged = 0u64;
        for room_id in room_ids {
            let mut tx = self.pool.begin().await?;
            let deleted =
                crate::service::room::hard_delete_room_and_cleanup_in_tx(&mut tx, &room_id)
                    .await
                    .internal_with_err("Failed to clean up room dependencies during purge")?;
            tx.commit()
                .await
                .internal_with_err("Failed to commit room purge transaction")?;
            if deleted {
                purged += 1;
            }
        }

        Ok(purged)
    }

    /// Delete email tokens that expired beyond the retention period
    async fn delete_expired_tokens(&self) -> Result<u64> {
        let days = Self::u32_to_i32_saturating(self.config.expired_token_retention_days);
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
    /// Calls the database function `cleanup_expired_credentials()` which deletes credentials
    /// that expired more than `buffer_hours` ago.
    async fn delete_expired_credentials(&self) -> Result<u64> {
        let buffer_hours = Self::u32_to_i32_saturating(self.config.expired_credential_buffer_hours);
        let result_json =
            sqlx::query_scalar::<_, serde_json::Value>("SELECT cleanup_expired_credentials($1)")
                .bind(buffer_hours)
                .fetch_one(&self.pool)
                .await
                .internal_with_err("Failed to delete expired credentials")?;

        let deleted_count = result_json["deleted_count"]
            .as_i64()
            .unwrap_or(0)
            .max(0)
            .cast_unsigned();

        Ok(deleted_count)
    }

    /// Delete read notifications older than the retention period
    async fn delete_old_notifications(&self) -> Result<u64> {
        let days = Self::u32_to_i32_saturating(self.config.notification_retention_days);
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
        let days = Self::u32_to_i32_saturating(self.config.notification_max_retention_days);
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

    /// Remove expired entries from the token blacklist table.
    ///
    /// Deletes expired token blacklist rows directly in PostgreSQL.
    async fn cleanup_token_blacklist(&self) -> Result<u64> {
        let deleted_count = sqlx::query_scalar::<_, i64>(
            r"
            WITH deleted AS (
                DELETE FROM token_blacklist
                WHERE expires_at < CURRENT_TIMESTAMP
                RETURNING 1
            )
            SELECT COUNT(*)::BIGINT FROM deleted
            ",
        )
        .fetch_one(&self.pool)
        .await
        .internal_with_err("Failed to cleanup token blacklist")?;
        Ok(deleted_count.max(0).cast_unsigned())
    }

    /// Cleanup chat messages exceeding per-room cap
    ///
    /// Uses window functions for efficient batch cleanup across all rooms.
    async fn cleanup_chat_messages(&self, keep_count: i64) -> Result<u64> {
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
            settings_registry: self.settings_registry.clone(),
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

                // Read dynamic settings at runtime for logging
                let room_ttl_seconds = service.room_ttl_seconds();
                let chat_max_messages = service.chat_max_messages_per_room();

                info!(
                    room_ttl_seconds,
                    chat_max_messages,
                    room_retention_days = service.config.room_soft_delete_retention_days,
                    user_retention_days = service.config.soft_delete_retention_days,
                    "Starting periodic data cleanup"
                );
                let result = service.run_all().await;

                let total = result.users_purged
                    + result.rooms_purged
                    + result.rooms_expired
                    + result.tokens_deleted
                    + result.credentials_deleted
                    + result.notifications_deleted
                    + result.chat_messages_deleted
                    + result.token_blacklist_deleted;

                if total > 0 {
                    info!(
                        users = result.users_purged,
                        rooms_purged = result.rooms_purged,
                        rooms_expired = result.rooms_expired,
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

    /// Test that `room_ttl_seconds` falls back to config when no registry is set.
    #[tokio::test]
    async fn test_room_ttl_seconds_fallback_to_config() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let config = CleanupConfig {
            room_ttl_seconds: 3600, // 1 hour
            ..CleanupConfig::default()
        };
        let leader: Arc<dyn LeaderCheck> = Arc::new(AlwaysLeader);
        let service = CleanupService::new(pool, config, leader);

        // No registry set, should use config value
        assert_eq!(service.room_ttl_seconds(), 3600);
    }

    /// Test that `chat_max_messages_per_room` falls back to config when no registry is set.
    #[tokio::test]
    async fn test_chat_max_messages_fallback_to_config() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let config = CleanupConfig {
            chat_max_messages_per_room: 500,
            ..CleanupConfig::default()
        };
        let leader: Arc<dyn LeaderCheck> = Arc::new(AlwaysLeader);
        let service = CleanupService::new(pool, config, leader);

        // No registry set, should use config value
        assert_eq!(service.chat_max_messages_per_room(), 500);
    }

    /// Test that `with_settings_registry` builder method works.
    #[tokio::test]
    async fn test_with_settings_registry_builder() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let config = CleanupConfig::default();
        let leader: Arc<dyn LeaderCheck> = Arc::new(AlwaysLeader);

        // Create a mock settings service (this won't actually connect)
        let settings_service = Arc::new(crate::service::SettingsService::new(
            crate::repository::SettingsRepository::new(pool.clone()),
            pool.clone(),
        ));
        let registry = Arc::new(SettingsRegistry::new(settings_service));

        let service = CleanupService::new(pool, config, leader).with_settings_registry(registry);

        // Service should have registry set (we can't easily test the value read without DB)
        assert!(service.settings_registry.is_some());
    }

    /// Service construction should work without an optional settings registry.
    #[tokio::test]
    async fn test_cleanup_service_without_registry() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let config = CleanupConfig::default();
        let leader: Arc<dyn LeaderCheck> = Arc::new(AlwaysLeader);

        let service = CleanupService::new(pool, config, leader);

        // Should work fine without registry
        assert!(service.settings_registry.is_none());
        // Should use config defaults
        assert_eq!(service.room_ttl_seconds(), 172_800);
        assert_eq!(service.chat_max_messages_per_room(), 0);
    }

    /// Dummy leader check that always returns true (for tests).
    struct AlwaysLeader;
    impl LeaderCheck for AlwaysLeader {
        fn is_leader(&self) -> bool {
            true
        }
    }
}
