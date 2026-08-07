//! Unified database maintenance service
//!
//! Coordinates periodic database maintenance in a single background task:
//! - Cleanup of expired email tokens, old notifications, and expired credentials
//! - Cleanup of chat messages older than the configurable retention cap (default: 90 days)
//! - Cleanup of expired chat and room resource events
//! - Cleanup of delivered realtime outbox rows
//!
//! Note: partition creation/retention is owned by dedicated managers:
//! - `AuditPartitionManager` for `audit_logs`
//! - `TimePartitionManager` for daily chat and playback-history partitions
//!
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::{
    cleanup::CleanupConfig, cleanup_ops, FileStorageCleanupOrigin, FileStorageService, LeaderCheck,
    RuntimeSettingsStore,
};
use crate::repository::FileStorageRepository;
use crate::service::partitioning::u32_to_i32;
use crate::Result as CoreResult;

/// Default chat message retention in days (used when settings are unavailable).
const DEFAULT_CHAT_MESSAGE_RETENTION_DAYS: i64 = 90;
const FILE_CLEANUP_RETRY_LIMIT: i64 = 100;

/// Unified database maintenance service.
///
/// Runs SQL maintenance functions as a leader-gated background task to avoid
/// duplicate work across replicas.
#[derive(Clone, Default)]
pub struct DatabaseMaintenanceOptions {
    pub config: CleanupConfig,
    pub runtime_settings_store: Option<Arc<RuntimeSettingsStore>>,
    pub file_storage_service: Option<Arc<dyn FileStorageService>>,
}

pub struct DatabaseMaintenanceService {
    pool: PgPool,
    config: CleanupConfig,
    leader_check: Arc<dyn LeaderCheck>,
    runtime_settings_store: Option<Arc<RuntimeSettingsStore>>,
    resource_tasks: DatabaseMaintenanceResourceTasks,
}

#[derive(Clone)]
struct DatabaseMaintenanceResourceTasks {
    pool: PgPool,
    file_storage_service: Option<Arc<dyn FileStorageService>>,
}

impl DatabaseMaintenanceResourceTasks {
    fn new(pool: PgPool, file_storage_service: Option<Arc<dyn FileStorageService>>) -> Self {
        Self {
            pool,
            file_storage_service,
        }
    }

    async fn cleanup_retained_chat_messages(&self, retention_days: i64) -> crate::Result<u64> {
        cleanup_ops::cleanup_chat_messages_with_files(
            &self.pool,
            self.file_storage_service.as_ref(),
            cleanup_ops::ChatMessageCleanupScope::Retention { retention_days },
            FileStorageCleanupOrigin::RetentionExpired,
            "old message purge",
        )
        .await
    }

    async fn retry_file_cleanup_jobs(&self) -> crate::Result<()> {
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
            let delete_origin = FileStorageCleanupOrigin::parse(&job.origin)
                .unwrap_or(FileStorageCleanupOrigin::CleanupRetry);
            match storage
                .delete_files(delete_origin, std::slice::from_ref(&file_reference))
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

    async fn cleanup_unreferenced_file_objects(
        &self,
        retention_seconds: u64,
    ) -> crate::Result<u64> {
        let Some(storage) = &self.file_storage_service else {
            return Ok(0);
        };
        cleanup_ops::cleanup_unreferenced_file_objects(&self.pool, storage, retention_seconds).await
    }

    async fn cleanup_expired_file_references(&self) -> crate::Result<u64> {
        let Some(storage) = &self.file_storage_service else {
            return Ok(0);
        };
        cleanup_ops::cleanup_expired_file_references(&self.pool, storage).await
    }

    async fn cleanup_expired_file_upload_sessions(&self) -> crate::Result<u64> {
        let Some(storage) = &self.file_storage_service else {
            return Ok(0);
        };
        cleanup_ops::cleanup_expired_file_upload_sessions(&self.pool, storage).await
    }
}

impl DatabaseMaintenanceService {
    #[cfg(test)]
    fn notification_retention_days_from_config(config: &CleanupConfig) -> CoreResult<i32> {
        u32_to_i32(
            config.notification_retention_days,
            "notification_retention_days",
        )
    }

    #[cfg(test)]
    fn notification_max_retention_days_from_config(config: &CleanupConfig) -> CoreResult<i32> {
        u32_to_i32(
            config.notification_max_retention_days,
            "notification_max_retention_days",
        )
    }

    #[cfg(test)]
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
            resource_tasks: DatabaseMaintenanceResourceTasks::new(
                pool.clone(),
                options.file_storage_service.clone(),
            ),
            pool,
            config: options.config,
            leader_check,
            runtime_settings_store: options.runtime_settings_store,
        }
    }

    /// Get the configured chat message retention period in days.
    fn chat_message_retention_days(&self) -> CoreResult<i64> {
        match self.runtime_settings_store.as_ref() {
            Some(registry) => registry.chat.message_retention_days.get(),
            None => Ok(DEFAULT_CHAT_MESSAGE_RETENTION_DAYS),
        }
    }

    fn chat_message_event_retention_seconds(&self) -> CoreResult<u64> {
        let message_retention_days = self.chat_message_retention_days()?;
        cleanup_ops::effective_chat_message_event_retention_seconds(
            self.config.chat_message_event_retention_seconds,
            message_retention_days,
        )
    }

    fn expired_token_retention_days(&self) -> CoreResult<i32> {
        u32_to_i32(
            self.config.expired_token_retention_days,
            "expired_token_retention_days",
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
        let read_deleted = cleanup_ops::delete_old_read_notifications(
            &self.pool,
            self.config.notification_retention_days,
        )
        .await?;
        let expired_deleted = cleanup_ops::delete_expired_notifications(
            &self.pool,
            self.config.notification_max_retention_days,
        )
        .await?;

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
    /// runtime settings store (default: 90 days). This enforces the hard retention
    /// limit for rooms that are inactive and therefore never processed by the
    /// per-room count-based cleanup (which only targets rooms with recent
    /// activity). Partition pruning makes this fast because the `created_at`
    /// filter maps directly to daily partitions.
    pub async fn run_cleanup_old_chat_messages(&self) -> CoreResult<()> {
        let retention_days = self.chat_message_retention_days()?;
        let deleted = self
            .resource_tasks
            .cleanup_retained_chat_messages(retention_days)
            .await?;
        if deleted > 0 {
            info!(
                deleted,
                retention_days, "Old chat message cleanup completed"
            );
        }
        Ok(())
    }

    pub async fn run_cleanup_room_resource_events(&self) -> crate::Result<()> {
        let deleted = cleanup_ops::delete_old_room_resource_events(
            &self.pool,
            self.config.room_resource_event_retention_seconds,
        )
        .await?;
        if deleted > 0 {
            info!(deleted, "Expired room resource event cleanup completed");
        }
        Ok(())
    }

    pub async fn run_cleanup_chat_message_events(&self) -> crate::Result<()> {
        let deleted = cleanup_ops::delete_old_chat_message_events(
            &self.pool,
            self.chat_message_event_retention_seconds()?,
        )
        .await?;
        if deleted > 0 {
            info!(deleted, "Expired chat message event cleanup completed");
        }
        Ok(())
    }

    pub async fn run_cleanup_realtime_outbox(&self) -> crate::Result<()> {
        let deleted = cleanup_ops::delete_delivered_realtime_outbox(
            &self.pool,
            self.config.realtime_outbox_sent_retention_days,
            self.config.realtime_outbox_dead_retention_days,
        )
        .await?;
        if deleted > 0 {
            info!(deleted, "Delivered realtime outbox cleanup completed");
        }
        Ok(())
    }

    pub async fn run_cleanup_playback_progress(&self) -> crate::Result<()> {
        let deleted = cleanup_ops::delete_stale_playback_progress(
            &self.pool,
            self.config.playback_progress_retention_days,
        )
        .await?;

        if deleted > 0 {
            info!(deleted, "Stale playback progress cleanup completed");
        }
        Ok(())
    }

    pub async fn run_cleanup_playback_history(&self) -> crate::Result<()> {
        let (retention_days, max_entries_per_room) = match &self.runtime_settings_store {
            Some(settings) => (
                settings.playback_history.retention_days.get()?,
                settings.playback_history.max_entries_per_room.get()?,
            ),
            None => (90, 1_000),
        };
        let deleted = crate::repository::PlaybackHistoryRepository::new(self.pool.clone())
            .cleanup(retention_days, max_entries_per_room)
            .await?;
        if deleted > 0 {
            info!(deleted, "Playback history cleanup completed");
        }
        Ok(())
    }

    /// Retry due file object cleanup jobs that were persisted after a previous
    /// delete attempt failed.
    pub async fn run_retry_file_cleanup_jobs(&self) -> crate::Result<()> {
        self.resource_tasks.retry_file_cleanup_jobs().await
    }

    /// Delete uploaded file objects that never received an active product reference.
    ///
    /// This handles interrupted direct uploads where bytes were stored but the
    /// product mutation that would attach the file never completed.
    pub async fn run_cleanup_unreferenced_file_objects(&self) -> crate::Result<u64> {
        let deleted = self
            .resource_tasks
            .cleanup_unreferenced_file_objects(self.config.unreferenced_file_retention_seconds)
            .await?;
        if deleted > 0 {
            info!(deleted, "Unreferenced file object cleanup completed");
        }
        Ok(deleted)
    }

    /// Release file references whose reference-level lifetime has expired.
    pub async fn run_cleanup_expired_file_references(&self) -> crate::Result<u64> {
        self.resource_tasks.cleanup_expired_file_references().await
    }

    /// Delete expired upload sessions and backend-specific temporary upload data.
    pub async fn run_cleanup_expired_file_upload_sessions(&self) -> crate::Result<u64> {
        let deleted = self
            .resource_tasks
            .cleanup_expired_file_upload_sessions()
            .await?;
        if deleted > 0 {
            info!(deleted, "Expired file upload session cleanup completed");
        }
        Ok(deleted)
    }

    /// Delete expired provider credentials.
    pub async fn run_cleanup_credentials(&self) -> crate::Result<()> {
        let deleted = cleanup_ops::delete_expired_credentials(
            &self.pool,
            self.config.expired_credential_buffer_hours,
        )
        .await?;
        if deleted > 0 {
            info!(deleted, "Expired credential cleanup completed");
        }
        Ok(())
    }

    /// Run all maintenance tasks. Logs errors but does not fail.
    ///
    /// Partition maintenance is intentionally excluded here:
    /// `AuditPartitionManager`, `TimePartitionManager`, and
    /// `NotificationPartitionManager` are the single owners.
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
        if let Err(e) = self.run_cleanup_chat_message_events().await {
            error!(error = %e, "Chat message event cleanup failed");
        }
        if let Err(e) = self.run_cleanup_realtime_outbox().await {
            error!(error = %e, "Realtime outbox cleanup failed");
        }
        if let Err(e) = self.run_cleanup_playback_progress().await {
            error!(error = %e, "Playback progress cleanup failed");
        }
        if let Err(e) = self.run_cleanup_playback_history().await {
            error!(error = %e, "Playback history cleanup failed");
        }
        if let Err(e) = self.run_cleanup_expired_file_references().await {
            error!(error = %e, "Expired file reference cleanup failed");
        }
        if let Err(e) = self.run_cleanup_expired_file_upload_sessions().await {
            error!(error = %e, "Expired file upload session cleanup failed");
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
            runtime_settings_store: self.runtime_settings_store.clone(),
            resource_tasks: self.resource_tasks.clone(),
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
                        if let Err(e) = service.run_cleanup_chat_message_events().await {
                            error!(error = %e, "Scheduled chat message event cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_realtime_outbox().await {
                            error!(error = %e, "Scheduled realtime outbox cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_playback_progress().await {
                            error!(error = %e, "Scheduled playback progress cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_expired_file_references().await {
                            error!(error = %e, "Scheduled expired file reference cleanup failed");
                        }
                        if let Err(e) = service.run_cleanup_expired_file_upload_sessions().await {
                            error!(error = %e, "Scheduled expired file upload session cleanup failed");
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
