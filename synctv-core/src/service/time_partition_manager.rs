//! Time partition management service
//!
//! Automatically manages time partition creation, retention cleanup,
//! and health monitoring with fixed daily granularity.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::Row as _;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::LeaderCheck;
use crate::repository::query_builder::trusted_dynamic_sql;
use crate::service::partitioning::{
    acquire_unbounded_ddl_connection, current_database_date, len_to_i32, len_to_i64, quote_ident,
    size_centi_mib, table_exists, wait_for_initial_leader, PartitionNameRow, PartitionSizeRow,
    STARTUP_RUNS_RETENTION_CLEANUP,
};
use crate::{Error, Result};

/// Minimum age before an empty partition can be dropped.
const EMPTY_PARTITION_MIN_AGE_DAYS: i32 = 90;

/// Default days to create ahead
const DEFAULT_PARTITION_DAYS_AHEAD: i32 = 30;

const TIME_PARTITIONED_TABLES: &[&str] = &["chat_messages", "room_playback_history"];

/// Health check result for time partitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimePartitionHealth {
    pub total_partitions: i32,
    pub total_size_centi_mib: i64,
    pub missing_partitions: Vec<String>,
    pub missing_count: i32,
    pub health_status: String,
}

/// Time partition manager (fixed daily granularity)
#[derive(Clone)]
pub struct TimePartitionManager {
    pool: PgPool,
    leader_check: Arc<dyn LeaderCheck>,
}

impl TimePartitionManager {
    /// Create a new partition manager with a leader check.
    ///
    /// Automatic partition management only runs on the leader node.
    #[must_use]
    pub fn new(pool: PgPool, leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self { pool, leader_check }
    }

    /// Ensure partitions exist for the next N days
    pub async fn ensure_future_partitions(&self, days_ahead: i32) -> Result<i32> {
        info!("Ensuring time partitions for next {} days", days_ahead);

        if days_ahead < 0 {
            return Err(Error::InvalidInput(
                "days_ahead must be greater than or equal to 0".to_string(),
            ));
        }

        let current_date = current_database_date(&self.pool).await?;
        let mut conn = acquire_unbounded_ddl_connection(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("Failed to acquire DDL connection: {e}")))?;

        for offset in 0..=days_ahead {
            let start_date = current_date + chrono::Duration::days(i64::from(offset));
            let end_date = start_date + chrono::Duration::days(1);
            for table_name in TIME_PARTITIONED_TABLES {
                let partition_name = format!("{}_{}", table_name, start_date.format("%Y_%m_%d"));
                let partition_ident = quote_ident(&partition_name);
                let table_ident = quote_ident(table_name);

                sqlx::query(trusted_dynamic_sql(format!(
                    "CREATE TABLE IF NOT EXISTS {partition_ident} PARTITION OF {table_ident} \
                     FOR VALUES FROM ('{start_date}') TO ('{end_date}')"
                )))
                .execute(&mut *conn)
                .await
                .map_err(|e| {
                    Error::Internal(format!(
                        "Failed to create {table_name} partition {partition_name}: {e}"
                    ))
                })?;
            }
        }

        let total_requested = days_ahead + 1;
        info!(
            "Time partitions created: {}/{} successful",
            total_requested, total_requested
        );

        Ok(total_requested)
    }

    /// Drop empty partitions older than the configured minimum age.
    pub async fn drop_empty_partitions_older_than(&self, min_age_days: i32) -> Result<i64> {
        info!("Dropping time partitions older than {} days", min_age_days);

        let mut conn = acquire_unbounded_ddl_connection(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("Failed to acquire DDL connection: {e}")))?;
        let current_date = current_database_date(&self.pool).await?;
        let cutoff_date = current_date - chrono::Duration::days(i64::from(min_age_days));
        let mut partitions = Vec::new();
        for table_name in TIME_PARTITIONED_TABLES {
            let cutoff_name = format!("{}_{}", table_name, cutoff_date.format("%Y_%m_%d"));
            let like_pattern = format!("{table_name}_%");
            let regex_pattern = format!(r"^{table_name}_[0-9]{{4}}_[0-9]{{2}}_[0-9]{{2}}$");
            let rows = sqlx::query_as::<_, PartitionNameRow>(
                r"SELECT tablename
                   FROM pg_tables
                   WHERE schemaname = 'public'
                     AND tablename LIKE $1
                     AND tablename ~ $2
                     AND tablename < $3
                   ORDER BY tablename",
            )
            .bind(like_pattern)
            .bind(regex_pattern)
            .bind(cutoff_name)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| Error::Internal(format!("Failed to list old partitions: {e}")))?;
            partitions.extend(rows.into_iter().map(|row| row.tablename));
        }

        let mut dropped_partitions = Vec::new();
        for partition in &partitions {
            if !partition_is_empty(&mut conn, partition).await? {
                warn!(
                    partition,
                    "Skipping non-empty old time partition; retention cleanup must delete rows before partition drop"
                );
                continue;
            }
            sqlx::query(trusted_dynamic_sql(format!(
                "DROP TABLE IF EXISTS {}",
                quote_ident(partition)
            )))
            .execute(&mut *conn)
            .await
            .map_err(|e| Error::Internal(format!("Failed to drop old time partition: {e}")))?;
            dropped_partitions.push(partition.clone());
        }

        let dropped_count = len_to_i64(dropped_partitions.len(), "dropped time partition count")?;
        if dropped_count > 0 {
            info!("Dropped {} old time partitions", dropped_count);
        }

        Ok(dropped_count)
    }

    /// Check partition health status
    pub async fn check_health(&self, days_ahead: i32) -> Result<TimePartitionHealth> {
        if days_ahead < 0 {
            return Err(Error::InvalidInput(
                "days_ahead must be greater than or equal to 0".to_string(),
            ));
        }

        let current_date = current_database_date(&self.pool).await?;
        let mut missing_partitions = Vec::new();
        for offset in 0..=days_ahead {
            let date = current_date + chrono::Duration::days(i64::from(offset));
            for table_name in TIME_PARTITIONED_TABLES {
                let partition_name = format!("{}_{}", table_name, date.format("%Y_%m_%d"));
                if !table_exists(&self.pool, &partition_name).await? {
                    missing_partitions.push(partition_name);
                }
            }
        }

        let rows = sqlx::query_as!(
            PartitionSizeRow,
            r#"
            SELECT pg_total_relation_size(format('%I.%I', schemaname, tablename))::BIGINT AS "size_bytes!"
             FROM pg_tables
             WHERE schemaname = 'public'
               AND (
                   tablename ~ '^chat_messages_[0-9]{4}_[0-9]{2}_[0-9]{2}$'
                   OR tablename ~ '^room_playback_history_[0-9]{4}_[0-9]{2}_[0-9]{2}$'
               )
             "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to check time partition health: {e}")))?;

        let total_partitions = len_to_i32(rows.len(), "time partition count")?;
        let total_size_bytes = rows.iter().map(|row| row.size_bytes).sum::<i64>();
        let missing_count = len_to_i32(missing_partitions.len(), "missing time partition count")?;
        let health = TimePartitionHealth {
            total_partitions,
            total_size_centi_mib: size_centi_mib(total_size_bytes),
            missing_partitions,
            missing_count,
            health_status: if missing_count == 0 {
                "healthy".to_string()
            } else {
                "warning".to_string()
            },
        };

        match health.health_status.as_str() {
            "healthy" => {
                info!(
                    "Time partitions are healthy: {} partitions",
                    health.total_partitions
                );
            }
            "warning" => {
                warn!("Time partitions warning: {} missing", health.missing_count);
            }
            _ => {
                warn!(
                    "Unknown time partition health status: {}",
                    health.health_status
                );
            }
        }

        Ok(health)
    }

    /// Start background task for automatic partition management.
    ///
    /// This task performs time-based partition operations (fixed daily granularity):
    /// 1. Ensures future partitions exist (default: 30 days ahead)
    /// 2. Drops empty partitions older than 90 days
    ///
    /// The task will shut down gracefully when the provided `CancellationToken` is cancelled.
    ///
    /// Row retention is handled by the cleanup services before this task drops empty partitions.
    #[must_use]
    pub fn start_auto_management(
        &self,
        check_interval_hours: u64,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();

        crate::spawn::spawn_monitored("time_partition_manager", async move {
            if !wait_for_initial_leader(
                manager.leader_check.clone(),
                cancel.clone(),
                "time partition management",
            )
            .await
            {
                info!("Time partition management task cancelled before leadership was established");
                return;
            }

            run_time_partition_maintenance(&manager).await;

            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
                check_interval_hours * 3600,
            ));
            interval.tick().await;

            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    () = cancel.cancelled() => {
                        info!("Time partition management task cancelled, shutting down");
                        return;
                    }
                }

                // Only run partition management on the leader node
                if !manager.leader_check.is_leader() {
                    info!("Skipping time partition management (not leader)");
                    continue;
                }

                run_time_partition_maintenance(&manager).await;
            }
        })
    }
}

async fn partition_is_empty(
    conn: &mut sqlx::pool::PoolConnection<sqlx::Postgres>,
    partition: &str,
) -> Result<bool> {
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {} LIMIT 1)",
        quote_ident(partition)
    );
    let row = sqlx::query(trusted_dynamic_sql(sql))
        .fetch_one(&mut **conn)
        .await
        .map_err(|e| Error::Internal(format!("Failed to inspect time partition rows: {e}")))?;
    let has_rows: bool = row
        .try_get(0)
        .map_err(|e| Error::Internal(format!("Failed to read time partition row probe: {e}")))?;
    Ok(!has_rows)
}

async fn run_time_partition_maintenance(manager: &TimePartitionManager) {
    match manager.check_health(DEFAULT_PARTITION_DAYS_AHEAD).await {
        Ok(health) => {
            if health.missing_count > 0 {
                warn!(
                    "Found {} missing time partitions, creating now",
                    health.missing_count
                );
                if let Err(e) = manager
                    .ensure_future_partitions(DEFAULT_PARTITION_DAYS_AHEAD)
                    .await
                {
                    error!("Failed to create missing time partitions: {}", e);
                }
            }
        }
        Err(e) => {
            error!("Failed to check time partition health: {}", e);
        }
    }

    if let Err(e) = manager
        .drop_empty_partitions_older_than(EMPTY_PARTITION_MIN_AGE_DAYS)
        .await
    {
        error!("Failed to drop old time partitions: {}", e);
    }
}

async fn initialize_time_partitions_on_startup(
    pool: &PgPool,
    drop_old_empty_partitions: bool,
) -> Result<()> {
    let manager = TimePartitionManager::new(pool.clone(), Arc::new(super::AlwaysLeader));

    // Step 1: Ensure future partitions exist
    manager
        .ensure_future_partitions(DEFAULT_PARTITION_DAYS_AHEAD)
        .await?;

    // Step 2: Check health status
    let health = manager.check_health(DEFAULT_PARTITION_DAYS_AHEAD).await?;
    if health.health_status != "healthy" {
        warn!("Time partition health check: {}", health.health_status);
    }

    // Startup initialization is per-replica readiness work only. Retention cleanup
    // stays leader-gated in the background task to avoid duplicate startup DDL.
    if drop_old_empty_partitions {
        manager
            .drop_empty_partitions_older_than(EMPTY_PARTITION_MIN_AGE_DAYS)
            .await?;
    }

    info!("Time partition initialization completed (daily granularity, {} days ahead, {} days minimum empty-partition age)",
        DEFAULT_PARTITION_DAYS_AHEAD, EMPTY_PARTITION_MIN_AGE_DAYS);

    Ok(())
}

/// Ensure time partitions exist on application startup
///
/// Should be called during application bootstrap, after migrations.
///
/// Startup initialization runs on every node because partitions must exist
/// before any node can insert data. Retention cleanup remains leader-gated in
/// the background task, which performs an initial run as soon as leadership is
/// established instead of waiting a full check interval.
pub async fn ensure_time_partitions_on_startup(pool: &PgPool) -> Result<()> {
    initialize_time_partitions_on_startup(pool, STARTUP_RUNS_RETENTION_CLEANUP).await
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
    fn test_chat_partition_health_deserialization() {
        let json = r#"{
            "total_partitions": 7,
            "total_size_centi_mib": 12850,
            "missing_partitions": [],
            "missing_count": 0,
            "health_status": "healthy"
        }"#;

        let health: TimePartitionHealth =
            ok(serde_json::from_str(json), "health JSON should deserialize");
        assert_eq!(health.total_partitions, 7);
        assert_eq!(health.missing_count, 0);
        assert_eq!(health.health_status, "healthy");
    }

    #[test]
    fn test_chat_partition_health_warning() {
        let json = r#"{
            "total_partitions": 5,
            "total_size_centi_mib": 6400,
            "missing_partitions": ["chat_messages_2026_08"],
            "missing_count": 1,
            "health_status": "warning"
        }"#;

        let health: TimePartitionHealth = ok(
            serde_json::from_str(json),
            "warning health JSON should deserialize",
        );
        assert_eq!(health.total_partitions, 5);
        assert_eq!(health.missing_count, 1);
        assert_eq!(health.health_status, "warning");
        assert_eq!(health.missing_partitions.len(), 1);
    }

    #[test]
    fn chat_partition_names_use_daily_ranges() {
        assert_eq!(
            format!("chat_messages_{}", "2026_06_23"),
            "chat_messages_2026_06_23"
        );
    }
}
