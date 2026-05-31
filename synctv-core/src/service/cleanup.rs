//! Data cleanup service for periodic maintenance tasks
//!
//! Coordinates cleanup of:
//! - Soft-deleted records (users, rooms) past retention period
//! - Expired email bind tokens
//! - Expired media provider credentials
//! - Old notifications
//! - Old chat messages (per-room cap)
//!
//! Runs as a background task with configurable intervals and retention periods.
//!
//! # Dynamic Settings
//!
//! The `chat_max_messages_per_room` setting can be dynamically configured via
//! `SettingsRegistry`. When a registry is provided, this value is read at
//! runtime on each cleanup cycle, allowing admins to
//! change settings without restarting the service.

use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{FileStorageCleanupOrigin, FileStorageService, LeaderCheck, SettingsRegistry};
use crate::{
    models::{ChatImage, FileReferenceTarget},
    repository::FileStorageRepository,
    InternalExt, Result,
};

/// Configuration for data cleanup retention periods
#[derive(Debug, Clone)]
pub struct CleanupConfig {
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
    /// Seconds to keep uploaded file objects that have no active product reference (0 = disabled)
    pub unreferenced_file_retention_seconds: u64,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            soft_delete_retention_days: 90,
            room_soft_delete_retention_days: 90,
            expired_token_retention_days: 7,
            expired_credential_buffer_hours: 1,
            notification_retention_days: 30,
            notification_max_retention_days: 90,
            chat_max_messages_per_room: 0, // unlimited by default
            unreferenced_file_retention_seconds: 86_400,
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
    /// Number of unreferenced file objects cleaned
    pub unreferenced_files_deleted: u64,
}

/// Data cleanup service
pub struct CleanupService {
    pool: PgPool,
    config: CleanupConfig,
    leader_check: Arc<dyn LeaderCheck>,
    /// Optional settings registry for dynamic `chat_max_messages_per_room`
    settings_registry: Option<Arc<SettingsRegistry>>,
    file_storage_service: Option<Arc<dyn FileStorageService>>,
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
            file_storage_service: None,
        }
    }

    /// Set the settings registry for dynamic `chat_max_messages_per_room`.
    ///
    /// When set, `chat_max_messages_per_room` is read from the registry at runtime
    /// on each cleanup cycle, allowing admins to change it without restarting the service.
    #[must_use]
    pub fn with_settings_registry(mut self, registry: Arc<SettingsRegistry>) -> Self {
        self.settings_registry = Some(registry);
        self
    }

    /// Set the file storage service used to clean file objects when
    /// per-room chat message cap cleanup purges file-backed messages.
    #[must_use]
    pub fn with_file_storage_service(mut self, service: Arc<dyn FileStorageService>) -> Self {
        self.file_storage_service = Some(service);
        self
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
        let chat_max_messages = self.chat_max_messages_per_room();

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

        // 8. Cleanup uploaded file objects that were never attached to a product row.
        match self.cleanup_expired_file_references().await {
            Ok(count) => {
                if count > 0 {
                    info!(count, "Released expired file references");
                }
            }
            Err(e) => warn!(error = %e, "Failed to cleanup expired file references"),
        }

        if self.config.unreferenced_file_retention_seconds > 0 {
            match self.cleanup_unreferenced_files().await {
                Ok(count) => {
                    result.unreferenced_files_deleted = count;
                    if count > 0 {
                        info!(count, "Deleted unreferenced file objects");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to cleanup unreferenced file objects"),
            }
        }

        result
    }

    /// Permanently delete users that were soft-deleted beyond the retention period
    async fn purge_soft_deleted_users(&self) -> Result<u64> {
        let days = Self::u32_to_i32_saturating(self.config.soft_delete_retention_days);
        let user_ids = sqlx::query_scalar!(
            r#"
            SELECT id as "id: crate::models::UserId"
            FROM users
            WHERE deleted_at IS NOT NULL
              AND deleted_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ORDER BY deleted_at ASC, id ASC
            "#,
            days,
        )
        .fetch_all(&self.pool)
        .await
        .internal_with_err("Failed to list soft-deleted users for purge")?;

        let mut purged = 0u64;
        for user_id in user_ids {
            let mut tx = self.pool.begin().await?;

            // User soft-delete keeps historical memberships by marking them as
            // `left`. Those rows still carry `ON DELETE RESTRICT` FKs, so they
            // must be removed before the hard delete can succeed.
            sqlx::query!(
                "DELETE FROM room_members WHERE user_id = $1",
                user_id as crate::models::UserId,
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to delete room memberships during user purge")?;

            match sqlx::query!(
                r"
                DELETE FROM users
                WHERE id = $1
                  AND deleted_at IS NOT NULL
                  AND deleted_at < CURRENT_TIMESTAMP - make_interval(days => $2)
                ",
                user_id as crate::models::UserId,
                days,
            )
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
        let room_ids = sqlx::query_scalar!(
            r#"
            SELECT id as "id: crate::models::RoomId"
            FROM rooms
            WHERE deleted_at IS NOT NULL
              AND deleted_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ORDER BY deleted_at ASC, id ASC
            "#,
            days,
        )
        .fetch_all(&self.pool)
        .await
        .internal_with_err("Failed to list soft-deleted rooms for purge")?;

        let mut purged = 0u64;
        for room_id in room_ids {
            let mut tx = self.pool.begin().await?;
            let deleted = crate::repository::room_cleanup::hard_delete_room_and_cleanup_in_tx(
                &mut tx, &room_id,
            )
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
        let result = sqlx::query!(
            r"
            DELETE FROM auth_email_tokens
            WHERE expires_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ",
            days
        )
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to delete expired tokens")?;

        Ok(result.rows_affected())
    }

    /// Delete expired media provider credentials with buffer to prevent race conditions.
    async fn delete_expired_credentials(&self) -> Result<u64> {
        let buffer_hours = Self::u32_to_i32_saturating(self.config.expired_credential_buffer_hours);
        let result = sqlx::query!(
            r"
            DELETE FROM user_media_provider_credentials
            WHERE expires_at IS NOT NULL
              AND expires_at < CURRENT_TIMESTAMP - make_interval(hours => $1)
            ",
            buffer_hours
        )
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to delete expired credentials")?;

        Ok(result.rows_affected())
    }

    /// Delete read notifications older than the retention period
    async fn delete_old_notifications(&self) -> Result<u64> {
        let days = Self::u32_to_i32_saturating(self.config.notification_retention_days);
        let result = sqlx::query!(
            r"
            DELETE FROM notifications
            WHERE is_read = TRUE
              AND created_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ",
            days
        )
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
        let result = sqlx::query!(
            r"
            DELETE FROM notifications
            WHERE created_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ",
            days
        )
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to delete expired notifications")?;

        Ok(result.rows_affected())
    }

    /// Remove expired entries from the token blacklist table.
    ///
    /// Deletes expired token blacklist rows directly in PostgreSQL.
    async fn cleanup_token_blacklist(&self) -> Result<u64> {
        let deleted_count = sqlx::query_scalar_unchecked!(
            r"
            WITH deleted AS (
                DELETE FROM auth_token_blacklist
                WHERE expires_at < CURRENT_TIMESTAMP
                RETURNING 1
            )
            SELECT COUNT(*)::BIGINT
            "
        )
        .fetch_one(&self.pool)
        .await
        .internal_with_err("Failed to cleanup token blacklist")?
        .unwrap_or(0);
        Ok(deleted_count.max(0).cast_unsigned())
    }

    async fn cleanup_expired_file_references(&self) -> Result<u64> {
        let Some(storage) = &self.file_storage_service else {
            return Ok(0);
        };
        let repository = FileStorageRepository::new(self.pool.clone());
        let references = repository.list_expired_references(100).await?;
        if references.is_empty() {
            return Ok(0);
        }

        match storage
            .delete_files(FileStorageCleanupOrigin::ReferenceExpired, &references)
            .await
        {
            Ok(()) => Ok(u64::try_from(references.len()).unwrap_or(u64::MAX)),
            Err(error) => {
                repository
                    .enqueue_cleanup_jobs(
                        FileStorageCleanupOrigin::ReferenceExpired.as_str(),
                        &references,
                        &serde_json::Value::Object(Default::default()),
                        &error.to_string(),
                    )
                    .await?;
                Err(error).internal_with_err("Failed to cleanup expired file references")
            }
        }
    }

    async fn cleanup_unreferenced_files(&self) -> Result<u64> {
        let Some(storage) = &self.file_storage_service else {
            return Ok(0);
        };
        let repository = FileStorageRepository::new(self.pool.clone());
        let older_than_seconds =
            i64::try_from(self.config.unreferenced_file_retention_seconds).unwrap_or(i64::MAX);
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
            Ok(()) => Ok(u64::try_from(references.len()).unwrap_or(u64::MAX)),
            Err(error) => {
                repository
                    .enqueue_cleanup_jobs(
                        FileStorageCleanupOrigin::UnreferencedObject.as_str(),
                        &references,
                        &serde_json::Value::Object(Default::default()),
                        &error.to_string(),
                    )
                    .await?;
                Err(error).internal_with_err("Failed to cleanup unreferenced file objects")
            }
        }
    }

    /// Cleanup chat messages exceeding per-room cap
    ///
    /// Uses window functions for efficient batch cleanup across all rooms.
    async fn cleanup_chat_messages(&self, keep_count: i64) -> Result<u64> {
        if keep_count <= 0 {
            return Ok(0);
        }

        let images = if let Some(storage) = &self.file_storage_service {
            let images = sqlx::query_as_unchecked!(
ChatImage,
r"
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
                SELECT i.id, i.room_id, i.message_id, i.message_created_at, i.storage_backend,
                       i.object_key, i.url, i.mime_type, i.size_bytes, i.width, i.height,
                       i.metadata, i.created_at
                FROM chat_message_images i
                INNER JOIN candidates c
                    ON c.id = i.message_id AND c.created_at = i.message_created_at
                ORDER BY i.message_created_at, i.message_id, i.created_at
                ",
keep_count
)
            .fetch_all(&self.pool)
            .await
            .internal_with_err("Failed to collect chat image cleanup candidates")?;
            if images.is_empty() {
                None
            } else {
                Some((storage.clone(), images))
            }
        } else {
            None
        };

        let result = sqlx::query!(
r"
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
            ",
keep_count
)
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to cleanup chat messages")?;

        if let Some((storage, images)) = images {
            let file_references = images
                .iter()
                .map(crate::models::ChatImage::file_reference_target)
                .collect::<Vec<_>>();
            if let Err(error) = storage
                .delete_files(
                    FileStorageCleanupOrigin::ReferenceCapExceeded,
                    &file_references,
                )
                .await
            {
                warn!(
                    error = %error,
                    deleted = result.rows_affected(),
                    keep_count,
                    "Chat image cleanup after per-room cap purge failed"
                );
                if let Err(enqueue_error) = FileStorageRepository::new(self.pool.clone())
                    .enqueue_cleanup_jobs(
                        FileStorageCleanupOrigin::ReferenceCapExceeded.as_str(),
                        &file_references,
                        &serde_json::Value::Object(Default::default()),
                        &error.to_string(),
                    )
                    .await
                {
                    warn!(
                        error = %enqueue_error,
                        keep_count,
                        "Failed to enqueue chat image cleanup retry after per-room cap purge"
                    );
                }
            }
        }

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
            file_storage_service: self.file_storage_service.clone(),
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
                let chat_max_messages = service.chat_max_messages_per_room();

                info!(
                    chat_max_messages,
                    room_retention_days = service.config.room_soft_delete_retention_days,
                    user_retention_days = service.config.soft_delete_retention_days,
                    "Starting periodic data cleanup"
                );
                let result = service.run_all().await;

                let total = result.users_purged
                    + result.rooms_purged
                    + result.tokens_deleted
                    + result.credentials_deleted
                    + result.notifications_deleted
                    + result.chat_messages_deleted
                    + result.token_blacklist_deleted;

                if total > 0 {
                    info!(
                        users = result.users_purged,
                        rooms_purged = result.rooms_purged,
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
