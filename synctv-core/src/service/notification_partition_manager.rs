//! Notification partition management service
//!
//! Automatically manages notification partition creation, retention cleanup,
//! and health monitoring with fixed monthly granularity.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::LeaderCheck;
use crate::{Error, Result};

/// Default retention period in months for notifications
const DEFAULT_RETENTION_MONTHS: i32 = 6;

/// Default months to create ahead
const DEFAULT_MONTHS_AHEAD: i32 = 3;

/// Health check result for notification partitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPartitionHealth {
    pub total_partitions: i32,
    pub total_size_mb: f64,
    pub missing_partitions: Vec<String>,
    pub missing_count: i32,
    pub health_status: String,
}

/// Notification partition manager (fixed monthly granularity)
#[derive(Clone)]
pub struct NotificationPartitionManager {
    pool: PgPool,
    leader_check: Arc<dyn LeaderCheck>,
}

impl NotificationPartitionManager {
    /// Create a new partition manager with a leader check.
    ///
    /// Automatic partition management only runs on the leader node.
    #[must_use]
    pub fn new(pool: PgPool, leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self { pool, leader_check }
    }

    /// Ensure partitions exist for the next N months
    pub async fn ensure_future_partitions(&self, months_ahead: i32) -> Result<serde_json::Value> {
        info!(
            "Ensuring notification partitions for next {} months",
            months_ahead
        );

        let result =
            sqlx::query_scalar::<_, serde_json::Value>("SELECT create_notification_partitions($1)")
                .bind(months_ahead)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    Error::Internal(format!("Failed to create notification partitions: {e}"))
                })?;

        let success_count = result["success_count"].as_i64().unwrap_or(0);
        let total_requested = result["total_requested"].as_i64().unwrap_or(0);
        info!(
            "Notification partitions created: {}/{} successful",
            success_count, total_requested
        );

        Ok(result)
    }

    /// Drop partitions older than the configured retention period
    pub async fn drop_old_partitions(&self, retain_months: i32) -> Result<i64> {
        info!(
            "Dropping notification partitions older than {} months",
            retain_months
        );

        let result = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT drop_old_notification_partitions($1)",
        )
        .bind(retain_months)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to drop old notification partitions: {e}")))?;

        let dropped_count = result["dropped_count"].as_i64().unwrap_or(0);
        if dropped_count > 0 {
            info!("Dropped {} old notification partitions", dropped_count);
        }

        Ok(dropped_count)
    }

    /// Start background task for automatic partition management and retention cleanup.
    ///
    /// This task performs time-based partition operations (fixed monthly granularity):
    /// 1. Ensures future partitions exist (default: 3 months ahead)
    /// 2. Drops old partitions (default: keep 6 months)
    ///
    /// The task will shut down gracefully when the provided `CancellationToken` is cancelled.
    #[must_use]
    pub fn start_auto_management(
        &self,
        check_interval_hours: u64,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();

        crate::spawn::spawn_monitored("notification_partition_manager", async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
                check_interval_hours * 3600,
            ));

            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    () = cancel.cancelled() => {
                        info!("Notification partition management task cancelled, shutting down");
                        return;
                    }
                }

                // Only run partition management on the leader node
                if !manager.leader_check.is_leader() {
                    info!("Skipping notification partition management (not leader)");
                    continue;
                }

                // 1. Ensure future partitions exist
                if let Err(e) = manager.ensure_future_partitions(DEFAULT_MONTHS_AHEAD).await {
                    error!("Failed to create notification partitions: {}", e);
                }

                // 2. Drop old partitions (time-based retention)
                if let Err(e) = manager.drop_old_partitions(DEFAULT_RETENTION_MONTHS).await {
                    error!("Failed to drop old notification partitions: {}", e);
                }
            }
        })
    }
}

/// Ensure notification partitions exist on application startup
///
/// Should be called during application bootstrap, after migrations.
///
/// Note: Startup partition initialization runs on every node (not leader-gated)
/// because partitions must exist before any node can insert data. Only the
/// periodic `start_auto_management` task is leader-gated.
pub async fn ensure_notification_partitions_on_startup(pool: &PgPool) -> Result<()> {
    // Startup uses AlwaysLeader since this is initialization, not periodic management
    let manager = NotificationPartitionManager::new(pool.clone(), Arc::new(super::AlwaysLeader));

    // Step 1: Ensure future partitions exist
    manager
        .ensure_future_partitions(DEFAULT_MONTHS_AHEAD)
        .await?;

    // Step 2: Drop old partitions (initial cleanup)
    manager
        .drop_old_partitions(DEFAULT_RETENTION_MONTHS)
        .await?;

    info!(
        "Notification partition initialization completed (monthly granularity, {} months ahead, {} months retention)",
        DEFAULT_MONTHS_AHEAD, DEFAULT_RETENTION_MONTHS
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_partition_health_deserialization() {
        let json = r#"{
            "total_partitions": 4,
            "total_size_mb": 32.5,
            "missing_partitions": [],
            "missing_count": 0,
            "health_status": "healthy"
        }"#;

        let health: NotificationPartitionHealth = serde_json::from_str(json).unwrap();
        assert_eq!(health.total_partitions, 4);
        assert_eq!(health.missing_count, 0);
        assert_eq!(health.health_status, "healthy");
    }

    #[test]
    fn test_notification_partition_health_warning() {
        let json = r#"{
            "total_partitions": 3,
            "total_size_mb": 16.0,
            "missing_partitions": ["notifications_2026_09"],
            "missing_count": 1,
            "health_status": "warning"
        }"#;

        let health: NotificationPartitionHealth = serde_json::from_str(json).unwrap();
        assert_eq!(health.total_partitions, 3);
        assert_eq!(health.missing_count, 1);
        assert_eq!(health.health_status, "warning");
        assert_eq!(health.missing_partitions.len(), 1);
    }

    #[test]
    fn test_default_retention_months() {
        assert_eq!(DEFAULT_RETENTION_MONTHS, 6);
    }

    #[test]
    fn test_default_months_ahead() {
        assert_eq!(DEFAULT_MONTHS_AHEAD, 3);
    }
}
