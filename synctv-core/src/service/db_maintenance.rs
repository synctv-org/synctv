//! Unified database maintenance service
//!
//! Coordinates all periodic database maintenance in a single background task:
//! - Partition creation for `audit_logs` (monthly)
//! - Cleanup of expired email tokens, old notifications, and expired credentials
//! - Cleanup of chat messages older than the configurable retention cap (default: 90 days)
//!
//! Note: chat message partition management (creation and old partition dropping)
//! is handled exclusively by `ChatPartitionManager` to avoid conflicting operations.
//!
//! Uses the existing SQL functions defined in migrations but previously uncalled
//! by application code.

use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::{LeaderCheck, SettingsRegistry};

/// Default chat message retention in days (used when settings are unavailable).
const DEFAULT_CHAT_MESSAGE_RETENTION_DAYS: i64 = 90;

/// Unified database maintenance service.
///
/// Calls the SQL maintenance functions that exist in migrations but were
/// previously never invoked by application code. Runs as a leader-gated
/// background task to avoid duplicate work across replicas.
pub struct DatabaseMaintenanceService {
    pool: PgPool,
    leader_check: Arc<dyn LeaderCheck>,
    settings_registry: Option<Arc<SettingsRegistry>>,
}

impl DatabaseMaintenanceService {
    /// Create a new maintenance service.
    #[must_use]
    pub fn new(pool: PgPool, leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self {
            pool,
            leader_check,
            settings_registry: None,
        }
    }

    /// Set the settings registry for configurable retention periods.
    #[must_use]
    pub fn with_settings_registry(mut self, registry: Arc<SettingsRegistry>) -> Self {
        self.settings_registry = Some(registry);
        self
    }

    /// Get the configured chat message retention period in days.
    fn chat_message_retention_days(&self) -> i64 {
        self.settings_registry
            .as_ref()
            .and_then(|r| r.chat_message_retention_days.get().ok())
            .unwrap_or(DEFAULT_CHAT_MESSAGE_RETENTION_DAYS)
    }

    /// Create audit log partitions for the next `months_ahead` months.
    pub async fn run_audit_partition_maintenance(&self) -> Result<(), sqlx::Error> {
        let result =
            sqlx::query_scalar::<_, serde_json::Value>("SELECT create_audit_logs_partitions($1)")
                .bind(3i32)
                .fetch_one(&self.pool)
                .await?;

        let success = result["success_count"].as_i64().unwrap_or(0);
        let total = result["total_requested"].as_i64().unwrap_or(0);
        info!(success, total, "Audit log partition maintenance completed");
        Ok(())
    }

    /// Delete expired email tokens.
    pub async fn run_cleanup_email_tokens(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT cleanup_expired_email_tokens()")
            .execute(&self.pool)
            .await?;
        info!("Expired email token cleanup completed");
        Ok(())
    }

    /// Delete old notifications with 90-day retention.
    pub async fn run_cleanup_notifications(&self) -> Result<(), sqlx::Error> {
        let result =
            sqlx::query_scalar::<_, serde_json::Value>("SELECT cleanup_old_notifications($1, $2)")
                .bind(30i32) // read_retention_days
                .bind(90i32) // max_retention_days
                .fetch_one(&self.pool)
                .await?;

        let read_deleted = result["read_deleted"].as_i64().unwrap_or(0);
        let expired_deleted = result["expired_deleted"].as_i64().unwrap_or(0);
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
    pub async fn run_cleanup_old_chat_messages(&self) -> Result<(), sqlx::Error> {
        let retention_days = self.chat_message_retention_days();
        let interval = format!("{retention_days} days");

        let result =
            sqlx::query("DELETE FROM chat_messages WHERE created_at <= NOW() - $1::interval")
                .bind(&interval)
                .execute(&self.pool)
                .await?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            info!(
                deleted,
                retention_days, "Old chat message cleanup completed"
            );
        }
        Ok(())
    }

    /// Delete expired provider credentials.
    pub async fn run_cleanup_credentials(&self) -> Result<(), sqlx::Error> {
        let result =
            sqlx::query_scalar::<_, serde_json::Value>("SELECT cleanup_expired_credentials($1)")
                .bind(1i32) // buffer_hours
                .fetch_one(&self.pool)
                .await?;

        let deleted = result["deleted_count"].as_i64().unwrap_or(0);
        if deleted > 0 {
            info!(deleted, "Expired credential cleanup completed");
        }
        Ok(())
    }

    /// Run all maintenance tasks. Logs errors but does not fail.
    ///
    /// Note: chat message partition management is handled exclusively by
    /// `ChatPartitionManager` which also performs health checks, handles missing
    /// partitions, and drops old partitions based on the retention period.
    pub async fn run_all_maintenance(&self) {
        if let Err(e) = self.run_audit_partition_maintenance().await {
            error!(error = %e, "Audit partition maintenance failed");
        }
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
    }

    /// Spawn the maintenance background loop.
    ///
    /// Runs all maintenance once at startup, then:
    /// - Partition checks every 12 hours
    /// - Cleanup tasks every hour
    ///
    /// Only executes when this node is the leader.
    /// Stops when the `CancellationToken` is cancelled.
    #[must_use]
    pub fn spawn_maintenance_loop(&self, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        let service = Self {
            pool: self.pool.clone(),
            leader_check: self.leader_check.clone(),
            settings_registry: self.settings_registry.clone(),
        };

        crate::spawn::spawn_monitored("db_maintenance", async move {
            // Run once at startup (if leader)
            if service.leader_check.is_leader() {
                info!("Running initial database maintenance");
                service.run_all_maintenance().await;
            }

            let mut partition_interval =
                tokio::time::interval(tokio::time::Duration::from_hours(12));
            let mut cleanup_interval = tokio::time::interval(tokio::time::Duration::from_hours(1));

            // Skip the first immediate tick (we already ran at startup)
            partition_interval.tick().await;
            cleanup_interval.tick().await;

            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        info!("Database maintenance task cancelled, shutting down");
                        return;
                    }
                    _ = partition_interval.tick() => {
                        if !service.leader_check.is_leader() {
                            info!("Skipping partition maintenance (not leader)");
                            continue;
                        }
                        info!("Running scheduled partition maintenance");
                        // Note: chat partition management is handled by ChatPartitionManager
                        if let Err(e) = service.run_audit_partition_maintenance().await {
                            error!(error = %e, "Scheduled audit partition maintenance failed");
                        }
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
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M16: Verify that the default retention is 90 days when no settings registry is set.
    #[tokio::test]
    async fn test_default_retention_days() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let leader = Arc::new(AlwaysLeader);
        let svc = DatabaseMaintenanceService::new(pool, leader);
        assert_eq!(
            svc.chat_message_retention_days(),
            DEFAULT_CHAT_MESSAGE_RETENTION_DAYS
        );
    }

    /// Dummy leader check that always returns true (for tests).
    struct AlwaysLeader;
    impl LeaderCheck for AlwaysLeader {
        fn is_leader(&self) -> bool {
            true
        }
    }
}
