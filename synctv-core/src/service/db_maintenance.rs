//! Unified database maintenance service
//!
//! Coordinates periodic database maintenance in a single background task:
//! - Cleanup of expired email tokens, old notifications, and expired credentials
//! - Cleanup of chat messages older than the configurable retention cap (default: 90 days)
//! - Cleanup of expired room resource events
//!
//! Note: partition creation/retention is owned by dedicated managers:
//! - `AuditPartitionManager` for `audit_logs`
//! - `ChatPartitionManager` for chat partitions
//!
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::{
    cleanup::CleanupConfig, FileStorageCleanupOrigin, FileStorageService, LeaderCheck,
    SettingsRegistry,
};
use crate::models::{ChatAttachment, FileReferenceTarget};
use crate::repository::{FileStorageRepository, RoomResourceEventRepository};
use crate::Result as CoreResult;

/// Default chat message retention in days (used when settings are unavailable).
const DEFAULT_CHAT_MESSAGE_RETENTION_DAYS: i64 = 90;
const FILE_CLEANUP_RETRY_LIMIT: i64 = 100;

fn u32_to_i32(value: u32, field: &'static str) -> CoreResult<i32> {
    i32::try_from(value).map_err(|_| crate::Error::Internal(format!("{field} exceeds i32::MAX")))
}

fn len_to_u64(len: usize, field: &'static str) -> CoreResult<u64> {
    u64::try_from(len).map_err(|_| crate::Error::Internal(format!("{field} exceeds u64::MAX")))
}

fn retention_seconds_to_i64(value: u64, field: &'static str) -> CoreResult<i64> {
    i64::try_from(value).map_err(|_| crate::Error::Internal(format!("{field} exceeds i64::MAX")))
}

/// Unified database maintenance service.
///
/// Runs SQL maintenance functions as a leader-gated background task to avoid
/// duplicate work across replicas.
#[derive(Clone, Default)]
pub struct DatabaseMaintenanceOptions {
    pub config: CleanupConfig,
    pub settings_registry: Option<Arc<SettingsRegistry>>,
    pub file_storage_service: Option<Arc<dyn FileStorageService>>,
}

pub struct DatabaseMaintenanceService {
    pool: PgPool,
    config: CleanupConfig,
    leader_check: Arc<dyn LeaderCheck>,
    settings_registry: Option<Arc<SettingsRegistry>>,
    file_storage_service: Option<Arc<dyn FileStorageService>>,
}

impl DatabaseMaintenanceService {
    fn notification_retention_days_from_config(config: &CleanupConfig) -> CoreResult<i32> {
        u32_to_i32(
            config.notification_retention_days,
            "notification_retention_days",
        )
    }

    fn notification_max_retention_days_from_config(config: &CleanupConfig) -> CoreResult<i32> {
        u32_to_i32(
            config.notification_max_retention_days,
            "notification_max_retention_days",
        )
    }

    fn expired_credential_buffer_hours_from_config(config: &CleanupConfig) -> CoreResult<i32> {
        u32_to_i32(
            config.expired_credential_buffer_hours,
            "expired_credential_buffer_hours",
        )
    }

    /// Create a new maintenance service.
    #[must_use]
    pub fn new(pool: PgPool, leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self::new_with_options(pool, leader_check, DatabaseMaintenanceOptions::default())
    }

    /// Create a new maintenance service with explicit runtime dependencies.
    #[must_use]
    pub fn new_with_options(
        pool: PgPool,
        leader_check: Arc<dyn LeaderCheck>,
        options: DatabaseMaintenanceOptions,
    ) -> Self {
        Self {
            pool,
            config: options.config,
            leader_check,
            settings_registry: options.settings_registry,
            file_storage_service: options.file_storage_service,
        }
    }

    /// Get the configured chat message retention period in days.
    fn chat_message_retention_days(&self) -> CoreResult<i64> {
        match self.settings_registry.as_ref() {
            Some(registry) => registry.chat_message_retention_days.get(),
            None => Ok(DEFAULT_CHAT_MESSAGE_RETENTION_DAYS),
        }
    }

    fn notification_retention_days(&self) -> CoreResult<i32> {
        Self::notification_retention_days_from_config(&self.config)
    }

    fn notification_max_retention_days(&self) -> CoreResult<i32> {
        Self::notification_max_retention_days_from_config(&self.config)
    }

    fn expired_token_retention_days(&self) -> CoreResult<i32> {
        u32_to_i32(
            self.config.expired_token_retention_days,
            "expired_token_retention_days",
        )
    }

    fn expired_credential_buffer_hours(&self) -> CoreResult<i32> {
        Self::expired_credential_buffer_hours_from_config(&self.config)
    }

    fn unreferenced_file_retention_seconds(&self) -> CoreResult<i64> {
        retention_seconds_to_i64(
            self.config.unreferenced_file_retention_seconds,
            "unreferenced_file_retention_seconds",
        )
    }

    fn room_resource_event_retention_seconds(&self) -> CoreResult<i64> {
        retention_seconds_to_i64(
            self.config.room_resource_event_retention_seconds,
            "room_resource_event_retention_seconds",
        )
    }

    fn playback_progress_retention_days(&self) -> CoreResult<i32> {
        u32_to_i32(
            self.config.playback_progress_retention_days,
            "playback_progress_retention_days",
        )
    }

    /// Delete expired email tokens.
    pub async fn run_cleanup_email_tokens(&self) -> crate::Result<()> {
        if self.config.expired_token_retention_days == 0 {
            return Ok(());
        }

        let result = sqlx::query!(
            r"
            DELETE FROM auth_email_tokens
            WHERE expires_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ",
            self.expired_token_retention_days()?
        )
        .execute(&self.pool)
        .await?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            info!(deleted, "Expired email token cleanup completed");
        }
        Ok(())
    }

    /// Delete old notifications using the shared cleanup retention settings.
    pub async fn run_cleanup_notifications(&self) -> crate::Result<()> {
        let read_deleted = if self.config.notification_retention_days > 0 {
            sqlx::query!(
                r"
                DELETE FROM notifications
                WHERE is_read = TRUE
                  AND created_at < CURRENT_TIMESTAMP - make_interval(days => $1)
                ",
                self.notification_retention_days()?
            )
            .execute(&self.pool)
            .await?
            .rows_affected()
        } else {
            0
        };

        let expired_deleted = if self.config.notification_max_retention_days > 0 {
            sqlx::query!(
                r"
                DELETE FROM notifications
                WHERE created_at < CURRENT_TIMESTAMP - make_interval(days => $1)
                ",
                self.notification_max_retention_days()?
            )
            .execute(&self.pool)
            .await?
            .rows_affected()
        } else {
            0
        };

        if read_deleted > 0 || expired_deleted > 0 {
            info!(
                read_deleted,
                expired_deleted, "Old notification cleanup completed"
            );
        }
        Ok(())
    }

    /// Delete all chat messages older than the configured retention cap.
    ///
    /// The retention period is read from `chat.message_retention_days` in the
    /// settings registry (default: 90 days). This enforces the hard retention
    /// limit for rooms that are inactive and therefore never processed by the
    /// per-room count-based cleanup (which only targets rooms with recent
    /// activity). Partition pruning makes this fast because the `created_at`
    /// filter maps directly to daily partitions.
    pub async fn run_cleanup_old_chat_messages(&self) -> CoreResult<()> {
        let retention_days = self.chat_message_retention_days()?;
        let interval = format!("{retention_days} days");

        let attachments = if let Some(storage) = &self.file_storage_service {
            let attachments = sqlx::query_as::<_, ChatAttachment>(
                r"
                SELECT i.id,
                       i.kind,
                       i.room_id,
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
                       i.metadata,
                       i.created_at
                FROM chat_message_attachments i
                INNER JOIN chat_messages m
                    ON m.id = i.message_id AND m.created_at = i.message_created_at
                WHERE m.created_at <= NOW() - $1::text::interval
                ORDER BY m.created_at, m.id, i.created_at
                ",
            )
            .bind(&interval)
            .fetch_all(&self.pool)
            .await?;
            if attachments.is_empty() {
                None
            } else {
                Some((storage.clone(), attachments))
            }
        } else {
            None
        };

        let result = sqlx::query!(
            "DELETE FROM chat_messages WHERE created_at <= NOW() - $1::text::interval",
            interval,
        )
        .execute(&self.pool)
        .await?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            info!(
                deleted,
                retention_days, "Old chat message cleanup completed"
            );
        }

        if let Some((storage, attachments)) = attachments {
            let file_references = attachments
                .iter()
                .map(crate::models::ChatAttachment::file_reference_target)
                .collect::<Vec<_>>();
            if let Err(error) = storage
                .delete_files(FileStorageCleanupOrigin::RetentionExpired, &file_references)
                .await
            {
                warn!(
                    error = %error,
                    deleted,
                    retention_days,
                    "Chat attachment cleanup after old message purge failed"
                );
                if let Err(enqueue_error) = FileStorageRepository::new(self.pool.clone())
                    .enqueue_cleanup_jobs(
                        FileStorageCleanupOrigin::RetentionExpired.as_str(),
                        &file_references,
                        &serde_json::Value::Object(Default::default()),
                        &error.to_string(),
                    )
                    .await
                {
                    warn!(
                        error = %enqueue_error,
                        deleted,
                        retention_days,
                        "Failed to enqueue chat attachment cleanup retry after old message purge"
                    );
                }
            }
        }
        Ok(())
    }

    pub async fn run_cleanup_room_resource_events(&self) -> crate::Result<()> {
        if self.config.room_resource_event_retention_seconds == 0 {
            return Ok(());
        }

        let deleted = RoomResourceEventRepository::new(self.pool.clone())
            .delete_older_than(self.room_resource_event_retention_seconds()?)
            .await?;
        if deleted > 0 {
            info!(deleted, "Expired room resource event cleanup completed");
        }
        Ok(())
    }

    pub async fn run_cleanup_playback_progress(&self) -> crate::Result<()> {
        if self.config.playback_progress_retention_days == 0 {
            return Ok(());
        }

        let deleted = sqlx::query!(
            r#"
            DELETE FROM room_playback_progress progress
            WHERE progress.updated_at < CURRENT_TIMESTAMP - make_interval(days => $1)
              AND NOT EXISTS (
                  SELECT 1
                  FROM room_playback_state state
                  WHERE state.current_progress_id = progress.id
              )
            "#,
            self.playback_progress_retention_days()?,
        )
        .execute(&self.pool)
        .await?
        .rows_affected();

        if deleted > 0 {
            info!(deleted, "Stale playback progress cleanup completed");
        }
        Ok(())
    }

    /// Retry due file object cleanup jobs that were persisted after a previous
    /// delete attempt failed.
    pub async fn run_retry_file_cleanup_jobs(&self) -> crate::Result<()> {
        let repository = FileStorageRepository::new(self.pool.clone());
        let due_jobs = repository.count_due_cleanup_jobs().await?;
        crate::metrics::file_storage::FILE_CLEANUP_JOBS_DUE.set(due_jobs);

        let Some(storage) = &self.file_storage_service else {
            return Ok(());
        };

        let jobs = repository
            .claim_due_cleanup_jobs(FILE_CLEANUP_RETRY_LIMIT, "db_maintenance")
            .await?;
        if jobs.is_empty() {
            return Ok(());
        }

        let mut completed = 0_u64;
        let mut rescheduled = 0_u64;
        for job in jobs {
            record_file_cleanup_job_metric("claimed", &job.origin, &job.storage_backend);
            let file_reference = job.reference_target();
            match storage
                .delete_files(
                    FileStorageCleanupOrigin::CleanupRetry,
                    std::slice::from_ref(&file_reference),
                )
                .await
            {
                Ok(()) => {
                    repository.complete_cleanup_job(job.id).await?;
                    record_file_cleanup_job_metric("completed", &job.origin, &job.storage_backend);
                    completed += 1;
                }
                Err(error) => {
                    let delay_seconds = file_cleanup_retry_delay_seconds(job.attempt_count);
                    repository
                        .reschedule_cleanup_job(job.id, &error.to_string(), delay_seconds)
                        .await?;
                    record_file_cleanup_job_metric(
                        "rescheduled",
                        &job.origin,
                        &job.storage_backend,
                    );
                    rescheduled += 1;
                    warn!(
                        job_id = job.id,
                        object_key = %job.object_key,
                        delay_seconds,
                        error = %error,
                        "File cleanup retry failed"
                    );
                }
            }
        }

        let due_jobs_after_retry = repository.count_due_cleanup_jobs().await?;
        crate::metrics::file_storage::FILE_CLEANUP_JOBS_DUE.set(due_jobs_after_retry);

        info!(completed, rescheduled, "File cleanup retry cycle completed");
        Ok(())
    }

    /// Delete uploaded file objects that never received an active product reference.
    ///
    /// This handles interrupted direct uploads where bytes were stored but the
    /// product mutation that would attach the file never completed.
    pub async fn run_cleanup_unreferenced_file_objects(&self) -> crate::Result<u64> {
        if self.config.unreferenced_file_retention_seconds == 0 {
            return Ok(0);
        }
        let Some(storage) = &self.file_storage_service else {
            return Ok(0);
        };
        let repository = FileStorageRepository::new(self.pool.clone());
        let files = repository
            .list_unreferenced_objects(self.unreferenced_file_retention_seconds()?, 100)
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
            Ok(()) => {
                let deleted = len_to_u64(references.len(), "unreferenced file count")?;
                if deleted > 0 {
                    info!(deleted, "Unreferenced file object cleanup completed");
                }
                Ok(deleted)
            }
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

    /// Release file references whose reference-level lifetime has expired.
    pub async fn run_cleanup_expired_file_references(&self) -> crate::Result<u64> {
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

    /// Delete expired provider credentials.
    pub async fn run_cleanup_credentials(&self) -> crate::Result<()> {
        if self.config.expired_credential_buffer_hours == 0 {
            return Ok(());
        }

        let result = sqlx::query!(
            r"
            DELETE FROM user_media_provider_credentials
            WHERE expires_at IS NOT NULL
              AND expires_at < CURRENT_TIMESTAMP - make_interval(hours => $1)
            ",
            self.expired_credential_buffer_hours()?
        )
        .execute(&self.pool)
        .await?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            info!(deleted, "Expired credential cleanup completed");
        }
        Ok(())
    }

    /// Run all maintenance tasks. Logs errors but does not fail.
    ///
    /// Partition maintenance is intentionally excluded here:
    /// `AuditPartitionManager` and `ChatPartitionManager` are the single owners.
    pub async fn run_all_maintenance(&self) {
        if let Err(e) = self.run_cleanup_email_tokens().await {
            error!(error = %e, "Email token cleanup failed");
        }
        if let Err(e) = self.run_cleanup_notifications().await {
            error!(error = %e, "Notification cleanup failed");
        }
        if let Err(e) = self.run_cleanup_credentials().await {
            error!(error = %e, "Credential cleanup failed");
        }
        if let Err(e) = self.run_cleanup_old_chat_messages().await {
            error!(error = %e, "Old chat message cleanup failed");
        }
        if let Err(e) = self.run_cleanup_room_resource_events().await {
            error!(error = %e, "Room resource event cleanup failed");
        }
        if let Err(e) = self.run_cleanup_playback_progress().await {
            error!(error = %e, "Playback progress cleanup failed");
        }
        if let Err(e) = self.run_cleanup_expired_file_references().await {
            error!(error = %e, "Expired file reference cleanup failed");
        }
        if let Err(e) = self.run_cleanup_unreferenced_file_objects().await {
            error!(error = %e, "Unreferenced file object cleanup failed");
        }
        if let Err(e) = self.run_retry_file_cleanup_jobs().await {
            error!(error = %e, "File cleanup retry failed");
        }
    }

    /// Spawn the maintenance background loop.
    ///
    /// Runs all cleanup maintenance once at startup, then every hour.
    ///
    /// Only executes when this node is the leader.
    /// Stops when the `CancellationToken` is cancelled.
    #[must_use]
    pub fn spawn_maintenance_loop(&self, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        let service = Self {
            pool: self.pool.clone(),
            config: self.config.clone(),
            leader_check: self.leader_check.clone(),
            settings_registry: self.settings_registry.clone(),
            file_storage_service: self.file_storage_service.clone(),
        };

        crate::spawn::spawn_monitored("db_maintenance", async move {
            // Run once at startup (if leader)
            if service.leader_check.is_leader() {
                info!("Running initial database maintenance");
                service.run_all_maintenance().await;
            }

            let mut cleanup_interval = tokio::time::interval(tokio::time::Duration::from_hours(1));

            // Skip the first immediate tick (we already ran at startup)
            cleanup_interval.tick().await;

            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        info!("Database maintenance task cancelled, shutting down");
                        return;
                    }
                    _ = cleanup_interval.tick() => {
                        if !service.leader_check.is_leader() {
                            info!("Skipping cleanup maintenance (not leader)");
                            continue;
                        }
                        info!("Running scheduled cleanup maintenance");
                        if let Err(e) = service.run_cleanup_email_tokens().await {
                            error!(error = %e, "Scheduled email token cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_notifications().await {
                            error!(error = %e, "Scheduled notification cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_credentials().await {
                            error!(error = %e, "Scheduled credential cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_old_chat_messages().await {
                            error!(error = %e, "Scheduled old chat message cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_room_resource_events().await {
                            error!(error = %e, "Scheduled room resource event cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_playback_progress().await {
                            error!(error = %e, "Scheduled playback progress cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_expired_file_references().await {
                            error!(error = %e, "Scheduled expired file reference cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_unreferenced_file_objects().await {
                            error!(error = %e, "Scheduled unreferenced file object cleanup failed");
                        }
                        if let Err(e) = service.run_retry_file_cleanup_jobs().await {
                            error!(error = %e, "Scheduled file cleanup retry failed");
                        }
                    }
                }
            }
        })
    }
}

fn file_cleanup_retry_delay_seconds(attempt_count: i32) -> i64 {
    let exponent = u32::try_from(attempt_count.clamp(0, 6)).unwrap_or(6);
    (60_i64 * 2_i64.pow(exponent)).min(3600)
}

fn record_file_cleanup_job_metric(action: &'static str, origin: &str, backend: &str) {
    crate::metrics::file_storage::FILE_CLEANUP_JOBS_TOTAL
        .with_label_values(&[action, origin, backend])
        .inc();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[tokio::test]
    async fn test_custom_cleanup_config_is_used_by_db_maintenance() {
        let config = CleanupConfig {
            expired_credential_buffer_hours: 6,
            notification_retention_days: 14,
            notification_max_retention_days: 45,
            ..CleanupConfig::default()
        };

        assert_eq!(
            ok(
                DatabaseMaintenanceService::notification_retention_days_from_config(&config),
                "notification retention days should fit i32",
            ),
            14
        );
        assert_eq!(
            ok(
                DatabaseMaintenanceService::notification_max_retention_days_from_config(&config),
                "notification max retention days should fit i32",
            ),
            45
        );
        assert_eq!(
            ok(
                DatabaseMaintenanceService::expired_credential_buffer_hours_from_config(&config),
                "expired credential buffer hours should fit i32",
            ),
            6
        );
    }
}
