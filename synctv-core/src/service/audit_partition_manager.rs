//! Audit log partition management service
//!
//! Automatically manages audit log partition creation and maintenance

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::LeaderCheck;
use crate::bootstrap::acquire_unbounded_ddl_connection;
use crate::repository::query_builder::trusted_dynamic_sql;
use crate::service::partitioning::{
    add_months, current_database_date, quote_ident, size_centi_gib, size_centi_mib, start_of_month,
    table_exists,
};
use crate::{Error, InternalExt, Result};

/// Maximum retry attempts for partition operations
const MAX_PARTITION_RETRIES: u32 = 3;
/// Base backoff in milliseconds (exponential: 1s, 2s, 4s)
const PARTITION_RETRY_BASE_MS: u64 = 1_000;
/// Default number of months of audit log partitions to retain
const DEFAULT_RETENTION_MONTHS: i32 = 12;
const INITIAL_LEADER_RETRY_INTERVAL_SECS: u64 = 5;

fn len_to_i32(len: usize, field: &'static str) -> Result<i32> {
    i32::try_from(len).map_err(|_| Error::Internal(format!("{field} exceeds i32::MAX")))
}

/// Health check result for audit log partitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionHealth {
    pub total_partitions: i32,
    pub total_size_centi_mib: i64,
    pub total_size_centi_gib: i64,
    pub missing_partitions: Vec<String>,
    pub missing_count: i32,
    pub health_status: String,
}

/// Statistics for audit log partitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionStats {
    pub total_partitions: usize,
    pub total_records: i64,
    pub partitions: Vec<PartitionInfo>,
}

/// Information about a single partition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionInfo {
    pub partition: String,
    pub row_count: i64,
    pub size_centi_mib: i64,
}

/// Result of partition creation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionCreationResult {
    pub total_requested: i32,
    pub success_count: i32,
    pub partitions: Vec<PartitionCreationDetail>,
}

/// Details of a single partition creation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionCreationDetail {
    pub partition_name: String,
    pub start_date: String,
    pub end_date: String,
    pub indexes_created: i32,
}

#[derive(sqlx::FromRow)]
struct PartitionNameRow {
    tablename: String,
}

#[derive(sqlx::FromRow)]
struct PartitionSizeRow {
    size_bytes: i64,
}

#[derive(sqlx::FromRow)]
struct PartitionStatsRow {
    tablename: String,
    row_count: i64,
    size_bytes: i64,
}

/// Audit log partition manager
pub struct AuditPartitionManager {
    pool: PgPool,
    leader_check: Arc<dyn LeaderCheck>,
}

impl AuditPartitionManager {
    /// Create a new partition manager with a leader check.
    ///
    /// Automatic partition management only runs on the leader node.
    #[must_use]
    pub fn new(pool: PgPool, leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self { pool, leader_check }
    }

    /// Ensure partitions exist for the next N months
    ///
    /// Should be called on application startup to ensure partitions are available
    pub async fn ensure_future_partitions(
        &self,
        months_ahead: i32,
    ) -> Result<PartitionCreationResult> {
        info!(
            "Ensuring audit log partitions for next {} months",
            months_ahead
        );

        if months_ahead < 0 {
            return Err(Error::InvalidInput(
                "months_ahead must be greater than or equal to 0".to_string(),
            ));
        }

        let current_month = start_of_month(current_database_date(&self.pool).await?)?;
        let mut partitions = Vec::new();
        for offset in 0..=months_ahead {
            partitions.push(
                self.create_partition_detail(add_months(current_month, offset)?)
                    .await?,
            );
        }

        let success_count = len_to_i32(partitions.len(), "created audit partition count")?;
        let result = PartitionCreationResult {
            total_requested: months_ahead + 1,
            success_count,
            partitions,
        };

        info!(
            "Partitions created: {}/{} successful",
            result.success_count, result.total_requested
        );

        Ok(result)
    }

    /// Create a partition for a specific date
    pub async fn create_partition(&self, date: chrono::NaiveDate) -> Result<PartitionInfo> {
        info!("Creating audit log partition for date: {}", date);

        let partition_name = self.create_partition_detail(date).await?.partition_name;

        Ok(PartitionInfo {
            partition: partition_name,
            row_count: 0,
            size_centi_mib: 0,
        })
    }

    async fn create_partition_detail(
        &self,
        date: chrono::NaiveDate,
    ) -> Result<PartitionCreationDetail> {
        let start_date = start_of_month(date)?;
        let end_date = add_months(start_date, 1)?;
        let partition_name = format!("audit_logs_{}", start_date.format("%Y_%m"));
        let partition_ident = quote_ident(&partition_name);
        let mut conn = acquire_unbounded_ddl_connection(&self.pool)
            .await
            .internal_with_err("Failed to acquire DDL connection for single partition creation")?;

        sqlx::query(trusted_dynamic_sql(format!(
            "CREATE TABLE IF NOT EXISTS {partition_ident} PARTITION OF audit_logs \
             FOR VALUES FROM ('{start_date}') TO ('{end_date}')"
        )))
        .execute(&mut *conn)
        .await
        .internal_with_err("Failed to create audit partition")?;

        sqlx::query(trusted_dynamic_sql(format!(
            "CREATE INDEX IF NOT EXISTS {} ON {partition_ident}(actor_id, created_at DESC) WHERE actor_id IS NOT NULL",
            quote_ident(&format!("{partition_name}_idx_actor_created"))
        )))
        .execute(&mut *conn)
        .await
        .internal_with_err("Failed to create audit partition index")?;

        sqlx::query(trusted_dynamic_sql(format!(
            "CREATE INDEX IF NOT EXISTS {} ON {partition_ident}(action, created_at DESC)",
            quote_ident(&format!("{partition_name}_idx_action_created"))
        )))
        .execute(&mut *conn)
        .await
        .internal_with_err("Failed to create audit partition index")?;

        sqlx::query(trusted_dynamic_sql(format!(
            "CREATE INDEX IF NOT EXISTS {} ON {partition_ident}(target_type, target_id, created_at DESC) WHERE target_type IS NOT NULL",
            quote_ident(&format!("{partition_name}_idx_target_created"))
        )))
        .execute(&mut *conn)
        .await
        .internal_with_err("Failed to create audit partition index")?;

        sqlx::query(trusted_dynamic_sql(format!(
            "CREATE INDEX IF NOT EXISTS {} ON {partition_ident}(ip_address) WHERE ip_address IS NOT NULL",
            quote_ident(&format!("{partition_name}_idx_ip_address"))
        )))
        .execute(&mut *conn)
        .await
        .internal_with_err("Failed to create audit partition index")?;

        Ok(PartitionCreationDetail {
            partition_name,
            start_date: start_date.to_string(),
            end_date: end_date.to_string(),
            indexes_created: 4,
        })
    }

    /// Drop old partitions
    ///
    /// Removes partitions older than the specified number of months
    pub async fn drop_old_partitions(&self, keep_months: i32) -> Result<Vec<String>> {
        info!(
            "Dropping audit log partitions older than {} months",
            keep_months
        );

        let mut conn = acquire_unbounded_ddl_connection(&self.pool)
            .await
            .internal_with_err("Failed to acquire DDL connection for dropping partitions")?;
        let current_month = start_of_month(current_database_date(&self.pool).await?)?;
        let cutoff_name = format!(
            "audit_logs_{}",
            add_months(current_month, -keep_months)?.format("%Y_%m")
        );
        let dropped_partitions = sqlx::query_as!(
            PartitionNameRow,
            r#"
            SELECT tablename AS "tablename!"
             FROM pg_tables
             WHERE schemaname = 'public'
               AND tablename ~ '^audit_logs_[0-9]{4}_[0-9]{2}$'
               AND tablename < $1
             ORDER BY tablename
             "#,
            cutoff_name
        )
        .fetch_all(&mut *conn)
        .await
        .internal_with_err("Failed to list old audit partitions")?
        .into_iter()
        .map(|row| row.tablename)
        .collect::<Vec<_>>();

        for partition in &dropped_partitions {
            sqlx::query(trusted_dynamic_sql(format!(
                "DROP TABLE IF EXISTS {}",
                quote_ident(partition)
            )))
            .execute(&mut *conn)
            .await
            .internal_with_err("Failed to drop audit partition")?;
        }

        let dropped_count = len_to_i32(dropped_partitions.len(), "dropped audit partition count")?;

        info!("Successfully dropped {} old partitions", dropped_count);

        Ok(dropped_partitions)
    }

    /// Check partition health status
    ///
    /// Returns missing partitions and overall health status
    pub async fn check_health(&self) -> Result<PartitionHealth> {
        let current_month = start_of_month(current_database_date(&self.pool).await?)?;
        let mut missing_partitions = Vec::new();
        for offset in 0..=6 {
            let partition_name = format!(
                "audit_logs_{}",
                add_months(current_month, offset)?.format("%Y_%m")
            );
            if !table_exists(&self.pool, &partition_name).await? {
                missing_partitions.push(partition_name);
            }
        }

        let rows = sqlx::query_as!(
            PartitionSizeRow,
            r#"
            SELECT pg_total_relation_size(c.oid)::BIGINT AS "size_bytes!"
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = 'public'
               AND c.relkind = 'r'
               AND c.relname ~ '^audit_logs_[0-9]{4}_[0-9]{2}$'
             "#,
        )
        .fetch_all(&self.pool)
        .await
        .internal_with_err("Failed to check partition health")?;

        let total_partitions = len_to_i32(rows.len(), "audit partition count")?;
        let total_size = rows.iter().map(|row| row.size_bytes).sum::<i64>();
        let missing_count = len_to_i32(missing_partitions.len(), "missing audit partition count")?;
        let health = PartitionHealth {
            total_partitions,
            total_size_centi_mib: size_centi_mib(total_size),
            total_size_centi_gib: size_centi_gib(total_size),
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
                    "Audit log partitions are healthy: {} partitions",
                    health.total_partitions
                );
            }
            "warning" => {
                warn!(
                    "Audit log partitions warning: {} missing partitions",
                    health.missing_count
                );
            }
            _ => {
                warn!("Unknown partition health status: {}", health.health_status);
            }
        }

        Ok(health)
    }

    /// Get partition statistics
    ///
    /// Returns detailed statistics for all partitions
    pub async fn get_stats(&self) -> Result<PartitionStats> {
        let rows = sqlx::query_as!(
            PartitionStatsRow,
            r#"
            SELECT
                c.relname AS "tablename!",
                GREATEST(c.reltuples, 0)::BIGINT AS "row_count!",
                pg_total_relation_size(c.oid)::BIGINT AS "size_bytes!"
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = 'public'
               AND c.relkind = 'r'
               AND c.relname ~ '^audit_logs_[0-9]{4}_[0-9]{2}$'
             ORDER BY c.relname DESC
             "#,
        )
        .fetch_all(&self.pool)
        .await
        .internal_with_err("Failed to get partition stats")?;

        let total_records = rows.iter().map(|row| row.row_count).sum();
        let partitions = rows
            .into_iter()
            .map(|row| PartitionInfo {
                partition: row.tablename,
                row_count: row.row_count,
                size_centi_mib: size_centi_mib(row.size_bytes),
            })
            .collect::<Vec<_>>();
        let stats = PartitionStats {
            total_partitions: partitions.len(),
            total_records,
            partitions,
        };

        info!(
            "Audit log stats: {} partitions, {} total records",
            stats.total_partitions, stats.total_records
        );

        Ok(stats)
    }

    /// Ensure future partitions with exponential backoff retry.
    ///
    /// Retries up to `MAX_PARTITION_RETRIES` times with exponential backoff
    /// (1s, 2s, 4s) on transient database failures.
    pub async fn ensure_future_partitions_with_retry(
        &self,
        months_ahead: i32,
    ) -> Result<PartitionCreationResult> {
        let mut last_err = None;

        for attempt in 0..MAX_PARTITION_RETRIES {
            match self.ensure_future_partitions(months_ahead).await {
                Ok(result) => {
                    if attempt > 0 {
                        info!(
                            "Partition creation succeeded on retry attempt {}",
                            attempt + 1
                        );
                    }
                    return Ok(result);
                }
                Err(e) => {
                    let backoff_ms = PARTITION_RETRY_BASE_MS * (1 << attempt);
                    warn!(
                        attempt = attempt + 1,
                        max_attempts = MAX_PARTITION_RETRIES,
                        backoff_ms = backoff_ms,
                        error = %e,
                        "Partition creation failed, retrying with exponential backoff"
                    );
                    last_err = Some(e);

                    if attempt + 1 < MAX_PARTITION_RETRIES {
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            Error::Internal("Partition creation failed after all retries".to_string())
        }))
    }

    /// Start automatic partition management task
    ///
    /// Spawns a background task that periodically checks and creates partitions.
    /// The task will shut down gracefully when the provided `CancellationToken` is cancelled.
    ///
    /// Partition creation uses exponential backoff retry on transient failures.
    #[must_use]
    pub fn start_auto_management(
        &self,
        check_interval_hours: u64,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();

        crate::spawn::spawn_monitored("audit_partition_manager", async move {
            if !wait_for_initial_leader(
                manager.leader_check.clone(),
                cancel.clone(),
                "audit partition management",
            )
            .await
            {
                info!(
                    "Audit partition management task cancelled before leadership was established"
                );
                return;
            }

            run_audit_partition_maintenance(&manager).await;

            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
                check_interval_hours * 3600,
            ));
            interval.tick().await;

            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    () = cancel.cancelled() => {
                        info!("Audit partition management task cancelled, shutting down");
                        return;
                    }
                }

                // Only run partition management on the leader node
                if !manager.leader_check.is_leader() {
                    info!("Skipping audit partition management (not leader)");
                    continue;
                }

                run_audit_partition_maintenance(&manager).await;
            }
        })
    }
}

impl Clone for AuditPartitionManager {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            leader_check: self.leader_check.clone(),
        }
    }
}

async fn run_audit_partition_maintenance(manager: &AuditPartitionManager) {
    match manager.check_health().await {
        Ok(health) => {
            if health.missing_count > 0 {
                warn!(
                    "Found {} missing partitions, creating now",
                    health.missing_count
                );
                if let Err(e) = manager.ensure_future_partitions_with_retry(6).await {
                    tracing::error!(
                        error = %e,
                        "Failed to create missing partitions after {} retries",
                        MAX_PARTITION_RETRIES
                    );
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to check partition health: {}", e);
        }
    }

    if let Err(e) = manager.drop_old_partitions(DEFAULT_RETENTION_MONTHS).await {
        tracing::error!(error = %e, "Failed to drop old audit log partitions");
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
            () = tokio::time::sleep(std::time::Duration::from_secs(INITIAL_LEADER_RETRY_INTERVAL_SECS)) => {}
        }
    }
}

const STARTUP_RUNS_RETENTION_CLEANUP: bool = false;

async fn initialize_audit_partitions_on_startup(
    pool: &PgPool,
    run_retention_cleanup: bool,
) -> Result<()> {
    let manager = AuditPartitionManager::new(pool.clone(), Arc::new(super::AlwaysLeader));

    // Step 1: Ensure next 6 months have partitions (with retry)
    manager.ensure_future_partitions_with_retry(6).await?;

    // Step 2: Check health status
    let health = manager.check_health().await?;
    if health.health_status != "healthy" {
        warn!("Partition health check: {}", health.health_status);
    }

    // Startup initialization is per-replica readiness work only. Retention cleanup
    // stays leader-gated in the background task to avoid duplicate startup DDL.
    if run_retention_cleanup {
        manager
            .drop_old_partitions(DEFAULT_RETENTION_MONTHS)
            .await?;
    }

    info!("Audit log partition initialization completed");

    Ok(())
}

/// Ensure audit partitions exist on application startup
///
/// Should be called during application bootstrap.
/// Uses exponential backoff retry for partition creation.
///
/// Startup initialization runs on every node because partitions must exist
/// before any node can insert data. Retention cleanup remains leader-gated in
/// the background task, which performs an initial run as soon as leadership is
/// established instead of waiting a full check interval.
pub async fn ensure_audit_partitions_on_startup(pool: &PgPool) -> Result<()> {
    initialize_audit_partitions_on_startup(pool, STARTUP_RUNS_RETENTION_CLEANUP).await
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
            "audit partition management",
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
    fn test_partition_health_deserialization() {
        let json = r#"{
            "total_partitions": 10,
            "total_size_centi_mib": 102450,
            "total_size_centi_gib": 100,
            "missing_partitions": [
                "audit_logs_2026_06"
            ],
            "missing_count": 1,
            "health_status": "warning"
        }"#;

        let health: PartitionHealth = serde_json::from_str(json).unwrap();
        assert_eq!(health.total_partitions, 10);
        assert_eq!(health.missing_count, 1);
        assert_eq!(health.health_status, "warning");
    }

    #[test]
    fn test_partition_creation_result_deserialization() {
        let json = r#"{
            "total_requested": 7,
            "success_count": 7,
            "partitions": [
                {
                    "partition_name": "audit_logs_2026_05",
                    "start_date": "2026-05-01",
                    "end_date": "2026-06-01",
                    "indexes_created": 4
                }
            ]
        }"#;

        let result: PartitionCreationResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.total_requested, 7);
        assert_eq!(result.success_count, 7);
        assert_eq!(result.partitions.len(), 1);
        assert_eq!(result.partitions[0].indexes_created, 4);
    }

    #[test]
    fn test_exponential_backoff_sequence() {
        let backoffs: Vec<u64> = (0..MAX_PARTITION_RETRIES)
            .map(|attempt| PARTITION_RETRY_BASE_MS * (1 << attempt))
            .collect();
        assert_eq!(backoffs, vec![1_000, 2_000, 4_000]);
    }

    #[test]
    fn test_partition_stats_deserialization() {
        let json = r#"{
            "total_partitions": 3,
            "total_records": 5000,
            "partitions": [
                {"partition": "audit_logs_2025_01", "row_count": 2000, "size_centi_mib": 5000},
                {"partition": "audit_logs_2025_02", "row_count": 3000, "size_centi_mib": 7500}
            ]
        }"#;

        let stats: PartitionStats = serde_json::from_str(json).unwrap();
        assert_eq!(stats.total_partitions, 3);
        assert_eq!(stats.total_records, 5000);
        assert_eq!(stats.partitions.len(), 2);
    }
}
