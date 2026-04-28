//! Unified database maintenance service
//!
//! Coordinates periodic database maintenance in a single background task:
//! - Cleanup of expired email tokens, old notifications, and expired credentials
//! - Cleanup of chat messages older than the configurable retention cap (default: 90 days)
//!
//! Note: partition creation/retention is owned by dedicated managers:
//! - `AuditPartitionManager` for `audit_logs`
//! - `ChatPartitionManager` for chat partitions
//!
//! Uses the existing SQL functions defined in migrations but previously uncalled
//! by application code.

use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::{cleanup::CleanupConfig, LeaderCheck, SettingsRegistry};

/// Default chat message retention in days (used when settings are unavailable).
const DEFAULT_CHAT_MESSAGE_RETENTION_DAYS: i64 = 90;

fn u32_to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// Unified database maintenance service.
///
/// Calls the SQL maintenance functions that exist in migrations but were
/// previously never invoked by application code. Runs as a leader-gated
/// background task to avoid duplicate work across replicas.
pub struct DatabaseMaintenanceService {
    pool: PgPool,
    config: CleanupConfig,
    leader_check: Arc<dyn LeaderCheck>,
    settings_registry: Option<Arc<SettingsRegistry>>,
}

impl DatabaseMaintenanceService {
    /// Create a new maintenance service.
    #[must_use]
    pub fn new(pool: PgPool, leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self {
            pool,
            config: CleanupConfig::default(),
            leader_check,
            settings_registry: None,
        }
    }

    /// Override cleanup retention/buffer configuration so maintenance and
    /// runtime cleanup share the same source of truth.
    #[must_use]
    pub const fn with_cleanup_config(mut self, config: CleanupConfig) -> Self {
        self.config = config;
        self
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

    fn notification_retention_days(&self) -> i32 {
        u32_to_i32(self.config.notification_retention_days)
    }

    fn notification_max_retention_days(&self) -> i32 {
        u32_to_i32(self.config.notification_max_retention_days)
    }

    fn expired_credential_buffer_hours(&self) -> i32 {
        u32_to_i32(self.config.expired_credential_buffer_hours)
    }

    /// Delete expired email tokens.
    pub async fn run_cleanup_email_tokens(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT cleanup_expired_auth_email_tokens()")
            .execute(&self.pool)
            .await?;
        info!("Expired email token cleanup completed");
        Ok(())
    }

    /// Delete old notifications using the shared cleanup retention settings.
    pub async fn run_cleanup_notifications(&self) -> Result<(), sqlx::Error> {
        let result =
            sqlx::query_scalar::<_, serde_json::Value>("SELECT cleanup_old_notifications($1, $2)")
                .bind(self.notification_retention_days())
                .bind(self.notification_max_retention_days())
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
                .bind(self.expired_credential_buffer_hours())
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
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_custom_cleanup_config_is_used_by_db_maintenance() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let leader = Arc::new(AlwaysLeader);
        let svc =
            DatabaseMaintenanceService::new(pool, leader).with_cleanup_config(CleanupConfig {
                expired_credential_buffer_hours: 6,
                notification_retention_days: 14,
                notification_max_retention_days: 45,
                ..CleanupConfig::default()
            });

        assert_eq!(svc.notification_retention_days(), 14);
        assert_eq!(svc.notification_max_retention_days(), 45);
        assert_eq!(svc.expired_credential_buffer_hours(), 6);
    }

    /// Dummy leader check that always returns true (for tests).
    struct AlwaysLeader;
    impl LeaderCheck for AlwaysLeader {
        fn is_leader(&self) -> bool {
            true
        }
    }
}
