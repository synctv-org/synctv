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
use crate::repository::query_builder::trusted_dynamic_sql;
use crate::service::partitioning::{
    add_months, current_database_date, len_to_i32, len_to_i64, partition_index_sql, quote_ident,
    size_centi_mib, start_of_month, table_exists, wait_for_initial_leader, PartitionIndexSpec,
    PartitionNameRow, PartitionSizeRow, STARTUP_RUNS_RETENTION_CLEANUP,
};
use crate::{Error, Result};

/// Default retention period in months for notifications
const DEFAULT_RETENTION_MONTHS: i32 = 6;

/// Default months to create ahead
const DEFAULT_MONTHS_AHEAD: i32 = 3;

const NOTIFICATION_PARTITION_INDEXES: &[PartitionIndexSpec] = &[
    PartitionIndexSpec {
        suffix: "idx_user_read_created",
        definition: "(user_id, is_read, created_at DESC)",
    },
    PartitionIndexSpec {
        suffix: "idx_user_unread",
        definition: "(user_id, created_at DESC) WHERE is_read = FALSE",
    },
    PartitionIndexSpec {
        suffix: "idx_user_type_created",
        definition: "(user_id, type, created_at DESC) WHERE is_read = FALSE",
    },
    PartitionIndexSpec {
        suffix: "idx_title_trgm",
        definition: "USING gin(title gin_trgm_ops)",
    },
    PartitionIndexSpec {
        suffix: "idx_content_trgm",
        definition: "USING gin(content gin_trgm_ops)",
    },
];

/// Health check result for notification partitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPartitionHealth {
    pub total_partitions: i32,
    pub total_size_centi_mib: i64,
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
    pub async fn ensure_future_partitions(&self, months_ahead: i32) -> Result<i32> {
        info!(
            "Ensuring notification partitions for next {} months",
            months_ahead
        );

        if months_ahead < 0 {
            return Err(Error::InvalidInput(
                "months_ahead must be greater than or equal to 0".to_string(),
            ));
        }

        let current_month = start_of_month(current_database_date(&self.pool).await?)?;
        let mut conn = acquire_unbounded_ddl_connection(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("Failed to acquire DDL connection: {e}")))?;

        for offset in 0..=months_ahead {
            let start_date = add_months(current_month, offset)?;
            let end_date = add_months(start_date, 1)?;
            let partition_name = format!("notifications_{}", start_date.format("%Y_%m"));
            let partition_ident = quote_ident(&partition_name);

            sqlx::query(trusted_dynamic_sql(format!(
                "CREATE TABLE IF NOT EXISTS {partition_ident} PARTITION OF notifications \
                 FOR VALUES FROM ('{start_date}') TO ('{end_date}')"
            )))
            .execute(&mut *conn)
            .await
            .map_err(|e| {
                Error::Internal(format!("Failed to create notification partition: {e}"))
            })?;

            for spec in NOTIFICATION_PARTITION_INDEXES {
                sqlx::query(trusted_dynamic_sql(partition_index_sql(
                    &partition_name,
                    &partition_ident,
                    *spec,
                )))
                .execute(&mut *conn)
                .await
                .map_err(|e| {
                    Error::Internal(format!(
                        "Failed to create notification partition index: {e}"
                    ))
                })?;
            }
        }

        let total_requested = months_ahead + 1;
        info!(
            "Notification partitions created: {}/{} successful",
            total_requested, total_requested
        );

        Ok(total_requested)
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
        let current_month = start_of_month(current_database_date(&self.pool).await?)?;
        let cutoff_name = format!(
            "notifications_{}",
            add_months(current_month, -retain_months)?.format("%Y_%m")
        );
        let partitions = sqlx::query_as!(
            PartitionNameRow,
            r#"
            SELECT tablename AS "tablename!"
             FROM pg_tables
             WHERE schemaname = 'public'
               AND tablename LIKE 'notifications_%'
               AND tablename != 'notifications_default'
               AND tablename ~ '^notifications_[0-9]{4}_[0-9]{2}$'
               AND tablename < $1
             ORDER BY tablename
             "#,
            cutoff_name
        )
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| Error::Internal(format!("Failed to drop old notification partitions: {e}")))?
        .into_iter()
        .map(|row| row.tablename)
        .collect::<Vec<_>>();

        for partition in &partitions {
            sqlx::query(trusted_dynamic_sql(format!(
                "DROP TABLE IF EXISTS {}",
                quote_ident(partition)
            )))
            .execute(&mut *conn)
            .await
            .map_err(|e| {
                Error::Internal(format!("Failed to drop old notification partition: {e}"))
            })?;
        }

        let dropped_count = len_to_i64(partitions.len(), "dropped notification partition count")?;
        if dropped_count > 0 {
            info!("Dropped {} old notification partitions", dropped_count);
        }

        Ok(dropped_count)
    }

    /// Check partition health status.
    pub async fn check_health(&self, months_ahead: i32) -> Result<NotificationPartitionHealth> {
        if months_ahead < 0 {
            return Err(Error::InvalidInput(
                "months_ahead must be greater than or equal to 0".to_string(),
            ));
        }

        let current_month = start_of_month(current_database_date(&self.pool).await?)?;
        let mut missing_partitions = Vec::new();
        for offset in 0..=months_ahead {
            let partition_name = format!(
                "notifications_{}",
                add_months(current_month, offset)?.format("%Y_%m")
            );
            if !table_exists(&self.pool, &partition_name).await? {
                missing_partitions.push(partition_name);
            }
        }

        let rows = sqlx::query_as!(
            PartitionSizeRow,
            r#"
            SELECT pg_total_relation_size(format('%I.%I', schemaname, tablename))::BIGINT AS "size_bytes!"
             FROM pg_tables
             WHERE schemaname = 'public'
               AND tablename LIKE 'notifications_%'
               AND tablename != 'notifications_default'
               AND tablename ~ '^notifications_[0-9]{4}_[0-9]{2}$'
             "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to check notification partitions: {e}")))?;

        let total_partitions = len_to_i32(rows.len(), "notification partition count")?;
        let total_size_bytes = rows.iter().map(|row| row.size_bytes).sum::<i64>();
        let missing_count = len_to_i32(
            missing_partitions.len(),
            "missing notification partition count",
        )?;
        Ok(NotificationPartitionHealth {
            total_partitions,
            total_size_centi_mib: size_centi_mib(total_size_bytes),
            missing_partitions,
            missing_count,
            health_status: if missing_count == 0 {
                "healthy".to_string()
            } else {
                "warning".to_string()
            },
        })
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

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn test_notification_partition_health_deserialization() {
        let json = r#"{
            "total_partitions": 4,
            "total_size_centi_mib": 3250,
            "missing_partitions": [],
            "missing_count": 0,
            "health_status": "healthy"
        }"#;

        let health: NotificationPartitionHealth =
            ok(serde_json::from_str(json), "health JSON should deserialize");
        assert_eq!(health.total_partitions, 4);
        assert_eq!(health.missing_count, 0);
        assert_eq!(health.health_status, "healthy");
    }

    #[test]
    fn test_notification_partition_health_warning() {
        let json = r#"{
            "total_partitions": 3,
            "total_size_centi_mib": 1600,
            "missing_partitions": ["notifications_2026_09"],
            "missing_count": 1,
            "health_status": "warning"
        }"#;

        let health: NotificationPartitionHealth = ok(
            serde_json::from_str(json),
            "warning health JSON should deserialize",
        );
        assert_eq!(health.total_partitions, 3);
        assert_eq!(health.missing_count, 1);
        assert_eq!(health.health_status, "warning");
        assert_eq!(health.missing_partitions.len(), 1);
    }

    #[test]
    fn notification_partition_indexes_match_search_paths() {
        let partition_name = "notifications_2026_06";
        let partition_ident = quote_ident(partition_name);
        let sql = NOTIFICATION_PARTITION_INDEXES
            .iter()
            .map(|spec| partition_index_sql(partition_name, &partition_ident, *spec))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(NOTIFICATION_PARTITION_INDEXES.len(), 5);
        assert!(sql.contains(
            r#"CREATE INDEX IF NOT EXISTS "notifications_2026_06_idx_title_trgm" ON "notifications_2026_06" USING gin(title gin_trgm_ops)"#
        ));
        assert!(sql.contains(
            r#"CREATE INDEX IF NOT EXISTS "notifications_2026_06_idx_content_trgm" ON "notifications_2026_06" USING gin(content gin_trgm_ops)"#
        ));
        assert!(sql.contains(
            r#"CREATE INDEX IF NOT EXISTS "notifications_2026_06_idx_user_read_created" ON "notifications_2026_06" (user_id, is_read, created_at DESC)"#
        ));
    }
}
