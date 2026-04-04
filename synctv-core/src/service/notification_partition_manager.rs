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
use crate::bootstrap::acquire_unbounded_ddl_connection;
use crate::{Error, Result};

/// Default retention period in months for notifications
const DEFAULT_RETENTION_MONTHS: i32 = 6;

/// Default months to create ahead
const DEFAULT_MONTHS_AHEAD: i32 = 3;

const INITIAL_LEADER_RETRY_INTERVAL_SECS: u64 = 5;
const STARTUP_RUNS_RETENTION_CLEANUP: bool = false;

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

        let mut conn = acquire_unbounded_ddl_connection(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("Failed to acquire DDL connection: {e}")))?;
        let result =
            sqlx::query_scalar::<_, serde_json::Value>("SELECT create_notification_partitions($1)")
                .bind(months_ahead)
                .fetch_one(&mut *conn)
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

        let mut conn = acquire_unbounded_ddl_connection(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("Failed to acquire DDL connection: {e}")))?;
        let result = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT drop_old_notification_partitions($1)",
        )
        .bind(retain_months)
        .fetch_one(&mut *conn)
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
            if !wait_for_initial_leader(
                manager.leader_check.clone(),
                cancel.clone(),
                "notification partition management",
            )
            .await
            {
                info!(
                    "Notification partition management task cancelled before leadership was established"
                );
                return;
            }

            run_notification_partition_maintenance(&manager).await;

            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
                check_interval_hours * 3600,
            ));
            interval.tick().await;

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

                run_notification_partition_maintenance(&manager).await;
            }
        })
    }
}

async fn run_notification_partition_maintenance(manager: &NotificationPartitionManager) {
    if let Err(e) = manager.ensure_future_partitions(DEFAULT_MONTHS_AHEAD).await {
        error!("Failed to create notification partitions: {}", e);
    }

    if let Err(e) = manager.drop_old_partitions(DEFAULT_RETENTION_MONTHS).await {
        error!("Failed to drop old notification partitions: {}", e);
    }
}

async fn wait_for_initial_leader(
    leader_check: Arc<dyn LeaderCheck>,
    cancel: CancellationToken,
    task_name: &'static str,
) -> bool {
    let mut logged_wait = false;

    loop {
        if leader_check.is_leader() {
            return true;
        }

        if !logged_wait {
            info!("Delaying initial {task_name} run until cluster leadership is established");
            logged_wait = true;
        }

        tokio::select! {
            () = cancel.cancelled() => return false,
            _ = tokio::time::sleep(std::time::Duration::from_secs(INITIAL_LEADER_RETRY_INTERVAL_SECS)) => {}
        }
    }
}

async fn initialize_notification_partitions_on_startup(
    pool: &PgPool,
    run_retention_cleanup: bool,
) -> Result<()> {
    let manager = NotificationPartitionManager::new(pool.clone(), Arc::new(super::AlwaysLeader));

    // Step 1: Ensure future partitions exist
    manager
        .ensure_future_partitions(DEFAULT_MONTHS_AHEAD)
        .await?;

    // Startup initialization is per-replica readiness work only. Retention cleanup
    // stays leader-gated in the background task to avoid duplicate startup DDL.
    if run_retention_cleanup {
        manager
            .drop_old_partitions(DEFAULT_RETENTION_MONTHS)
            .await?;
    }

    info!(
        "Notification partition initialization completed (monthly granularity, {} months ahead, {} months retention)",
        DEFAULT_MONTHS_AHEAD, DEFAULT_RETENTION_MONTHS
    );

    Ok(())
}

/// Ensure notification partitions exist on application startup
///
/// Should be called during application bootstrap, after migrations.
///
/// Startup initialization runs on every node because partitions must exist
/// before any node can insert data. Retention cleanup remains leader-gated in
/// the background task, which performs an initial run as soon as leadership is
/// established instead of waiting a full check interval.
pub async fn ensure_notification_partitions_on_startup(pool: &PgPool) -> Result<()> {
    initialize_notification_partitions_on_startup(pool, STARTUP_RUNS_RETENTION_CLEANUP).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const _: () = assert!(
        !STARTUP_RUNS_RETENTION_CLEANUP,
        "per-replica startup initialization must avoid retention cleanup DDL"
    );

    #[tokio::test(start_paused = true)]
    async fn test_wait_for_initial_leader_completes_before_full_check_interval() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct ToggleLeader(AtomicBool);
        impl LeaderCheck for ToggleLeader {
            fn is_leader(&self) -> bool {
                self.0.load(Ordering::SeqCst)
            }
        }

        let leader = Arc::new(ToggleLeader(AtomicBool::new(false)));
        let cancel = CancellationToken::new();
        let wait_task = tokio::spawn(wait_for_initial_leader(
            leader.clone(),
            cancel,
            "notification partition management",
        ));

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(
            INITIAL_LEADER_RETRY_INTERVAL_SECS - 1,
        ))
        .await;
        assert!(
            !wait_task.is_finished(),
            "initial maintenance should still be waiting for leadership"
        );

        leader.0.store(true, Ordering::SeqCst);
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert!(
            wait_task.await.expect("wait task should complete"),
            "leader election should trigger the initial maintenance wait to finish"
        );
    }

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

}
