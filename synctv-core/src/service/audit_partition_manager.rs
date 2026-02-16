//! Audit log partition management service
//!
//! Automatically manages audit log partition creation and maintenance

use sqlx::PgPool;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{Error, Result};

/// Maximum retry attempts for partition operations
const MAX_PARTITION_RETRIES: u32 = 3;
/// Base backoff in milliseconds (exponential: 1s, 2s, 4s)
const PARTITION_RETRY_BASE_MS: u64 = 1_000;

/// Health check result for audit log partitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionHealth {
    pub total_partitions: i32,
    pub total_size_mb: f64,
    pub total_size_gb: f64,
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
    pub size_mb: f64,
}

/// Result of partition creation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionCreationResult {
    pub status: String,
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
    pub status: String,
}

/// Result of index ensure operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEnsureResult {
    pub status: String,
    pub partitions_updated: i64,
    pub total_indexes_created: i64,
    pub partitions: Vec<PartitionInfo>,
}

/// Audit log partition manager
pub struct AuditPartitionManager {
    pool: PgPool,
}

impl AuditPartitionManager {
    /// Create a new partition manager
    #[must_use] 
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Ensure existing partitions have indexes
    ///
    /// Adds missing indexes to existing partitions (idempotent operation)
    pub async fn ensure_existing_indexes(&self, partition_count: i32) -> Result<IndexEnsureResult> {
        info!("Ensuring indexes for last {} partitions", partition_count);

        let result_json = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT ensure_existing_partitions_indexes($1)"
        )
        .bind(partition_count)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to ensure indexes: {e}")))?;

        let result: IndexEnsureResult = serde_json::from_value(result_json)
            .map_err(|e| Error::Internal(format!("Failed to parse index result: {e}")))?;

        info!(
            "Indexes ensured: {} partitions, {} indexes created",
            result.partitions_updated, result.total_indexes_created
        );

        Ok(result)
    }

    /// Ensure partitions exist for the next N months
    ///
    /// Should be called on application startup to ensure partitions are available
    pub async fn ensure_future_partitions(&self, months_ahead: i32) -> Result<PartitionCreationResult> {
        info!("Ensuring audit log partitions for next {} months", months_ahead);

        let result_json = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT create_audit_logs_partitions($1)"
        )
        .bind(months_ahead)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to create partitions: {e}")))?;

        let result: PartitionCreationResult = serde_json::from_value(result_json)
            .map_err(|e| Error::Internal(format!("Failed to parse partition result: {e}")))?;

        info!(
            "Partitions created: {}/{} successful",
            result.success_count, result.total_requested
        );

        Ok(result)
    }

    /// Create a partition for a specific date
    pub async fn create_partition(&self, date: chrono::NaiveDate) -> Result<PartitionInfo> {
        info!("Creating audit log partition for date: {}", date);

        let result_json = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT create_audit_logs_partition($1)"
        )
        .bind(date)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to create partition: {e}")))?;

        let partition_name = result_json["partition_name"]
            .as_str()
            .ok_or_else(|| Error::Internal("Invalid partition result".to_string()))?;

        Ok(PartitionInfo {
            partition: partition_name.to_string(),
            row_count: 0,
            size_mb: 0.0,
        })
    }

    /// Drop old partitions
    ///
    /// Removes partitions older than the specified number of months
    pub async fn drop_old_partitions(&self, keep_months: i32) -> Result<Vec<String>> {
        info!("Dropping audit log partitions older than {} months", keep_months);

        let result_json = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT drop_old_audit_logs_partitions($1)"
        )
        .bind(keep_months)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to drop partitions: {e}")))?;

        let dropped_count = result_json["dropped_count"]
            .as_i64()
            .unwrap_or(0) as i32;

        info!("Successfully dropped {} old partitions", dropped_count);

        let dropped_partitions = result_json["dropped_partitions"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v["partition"].as_str())
                    .map(std::string::ToString::to_string)
                    .collect()
            })
            .unwrap_or_default();

        Ok(dropped_partitions)
    }

    /// Check partition health status
    ///
    /// Returns missing partitions and overall health status
    pub async fn check_health(&self) -> Result<PartitionHealth> {
        let result_json = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT check_audit_logs_partitions()"
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to check partition health: {e}")))?;

        let health: PartitionHealth = serde_json::from_value(result_json)
            .map_err(|e| Error::Internal(format!("Failed to parse health result: {e}")))?;

        match health.health_status.as_str() {
            "healthy" => {
                info!("Audit log partitions are healthy: {} partitions", health.total_partitions);
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
        let result_json = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT get_audit_logs_stats()"
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to get partition stats: {e}")))?;

        let stats: PartitionStats = serde_json::from_value(result_json)
            .map_err(|e| Error::Internal(format!("Failed to parse stats result: {e}")))?;

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
    pub async fn ensure_future_partitions_with_retry(&self, months_ahead: i32) -> Result<PartitionCreationResult> {
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
    pub fn start_auto_management(&self, check_interval_hours: u64, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(check_interval_hours * 3600));

            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    () = cancel.cancelled() => {
                        info!("Audit partition management task cancelled, shutting down");
                        return;
                    }
                }

                // Check health status
                match manager.check_health().await {
                    Ok(health) => {
                        // Create missing partitions if any (with retry)
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
            }
        })
    }
}

impl Clone for AuditPartitionManager {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

/// Ensure audit partitions exist on application startup
///
/// Should be called during application bootstrap.
/// Uses exponential backoff retry for partition creation.
pub async fn ensure_audit_partitions_on_startup(pool: &PgPool) -> Result<()> {
    let manager = AuditPartitionManager::new(pool.clone());

    // Step 1: Ensure existing partitions have indexes (idempotent)
    manager.ensure_existing_indexes(4).await?;

    // Step 2: Ensure next 6 months have partitions (with retry)
    manager.ensure_future_partitions_with_retry(6).await?;

    // Step 3: Check health status
    let health = manager.check_health().await?;
    if health.health_status != "healthy" {
        warn!("Partition health check: {}", health.health_status);
    }

    info!("Audit log partition initialization completed");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partition_health_deserialization() {
        let json = r#"{
            "total_partitions": 10,
            "total_size_mb": 1024.5,
            "total_size_gb": 1.0,
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
            "status": "completed",
            "total_requested": 6,
            "success_count": 6,
            "partitions": [
                {
                    "partition_name": "audit_logs_2026_05",
                    "start_date": "2026-05-01",
                    "end_date": "2026-06-01",
                    "indexes_created": 4,
                    "status": "success"
                }
            ]
        }"#;

        let result: PartitionCreationResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.total_requested, 6);
        assert_eq!(result.success_count, 6);
        assert_eq!(result.partitions.len(), 1);
        assert_eq!(result.partitions[0].indexes_created, 4);
    }

    #[test]
    fn test_index_ensure_result_deserialization() {
        let json = r#"{
            "status": "completed",
            "partitions_updated": 4,
            "total_indexes_created": 16,
            "partitions": [
                {
                    "partition": "audit_logs_2024_01",
                    "row_count": 0,
                    "size_mb": 0.0
                }
            ]
        }"#;

        let result: IndexEnsureResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.partitions_updated, 4);
        assert_eq!(result.total_indexes_created, 16);
        assert_eq!(result.partitions.len(), 1);
    }

    #[test]
    fn test_partition_info_serialization() {
        let info = PartitionInfo {
            partition: "audit_logs_2024_01".to_string(),
            row_count: 1000,
            size_mb: 256.5,
        };

        let json = serde_json::to_string(&info).unwrap();
        let deserialized: PartitionInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.partition, info.partition);
        assert_eq!(deserialized.row_count, info.row_count);
        assert_eq!(deserialized.size_mb, info.size_mb);
    }

    #[test]
    fn test_partition_creation_detail_serialization() {
        let detail = PartitionCreationDetail {
            partition_name: "audit_logs_2024_01".to_string(),
            start_date: "2024-01-01".to_string(),
            end_date: "2024-02-01".to_string(),
            indexes_created: 4,
            status: "success".to_string(),
        };

        let json = serde_json::to_string(&detail).unwrap();
        let deserialized: PartitionCreationDetail = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.partition_name, detail.partition_name);
        assert_eq!(deserialized.indexes_created, detail.indexes_created);
        assert_eq!(deserialized.status, detail.status);
    }

    #[test]
    fn test_all_statuses() {
        // Test all possible health statuses
        let statuses = vec!["healthy", "warning", "unknown"];
        for status in statuses {
            let health = PartitionHealth {
                total_partitions: 10,
                total_size_mb: 1024.5,
                total_size_gb: 1.0,
                missing_partitions: vec![],
                missing_count: 0,
                health_status: status.to_string(),
            };
            assert_eq!(health.health_status, status);
        }
    }

    // ========== Retry Constants ==========

    #[test]
    fn test_retry_constants() {
        assert_eq!(MAX_PARTITION_RETRIES, 3);
        assert_eq!(PARTITION_RETRY_BASE_MS, 1_000);
    }

    #[test]
    fn test_exponential_backoff_sequence() {
        // Verify the backoff values: 1s, 2s, 4s
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
                {"partition": "audit_logs_2025_01", "row_count": 2000, "size_mb": 50.0},
                {"partition": "audit_logs_2025_02", "row_count": 3000, "size_mb": 75.0}
            ]
        }"#;

        let stats: PartitionStats = serde_json::from_str(json).unwrap();
        assert_eq!(stats.total_partitions, 3);
        assert_eq!(stats.total_records, 5000);
        assert_eq!(stats.partitions.len(), 2);
    }
}
