//! Chat message partition management service
//!
//! Automatically manages chat message partition creation, retention cleanup,
//! and health monitoring with fixed daily granularity.

use std::sync::Arc;
use sqlx::PgPool;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn, error};

use crate::{Error, Result};
use crate::service::global_settings::SettingsRegistry;

/// Default retention period in days for chat messages
const DEFAULT_RETENTION_DAYS: i32 = 90;

/// Default days to create ahead
const DEFAULT_DAYS_AHEAD: i32 = 30;

/// Health check result for chat message partitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatPartitionHealth {
    pub total_partitions: i32,
    pub total_size_mb: f64,
    pub missing_partitions: Vec<String>,
    pub missing_count: i32,
    pub health_status: String,
}

/// Chat message partition manager (fixed daily granularity)
#[derive(Clone)]
pub struct ChatPartitionManager {
    pool: PgPool,
    /// Settings registry for future dynamic configuration (retention days, days ahead, etc.)
    _settings: Arc<SettingsRegistry>,
}

impl ChatPartitionManager {
    /// Create a new partition manager
    #[must_use]
    pub fn new(pool: PgPool, settings: Arc<SettingsRegistry>) -> Self {
        Self { pool, _settings: settings }
    }

    /// Ensure partitions exist for the next N days
    pub async fn ensure_future_partitions(&self, days_ahead: i32) -> Result<serde_json::Value> {
        info!("Ensuring chat message partitions for next {} days", days_ahead);

        let result = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT create_chat_message_partitions($1)"
        )
        .bind(days_ahead)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to create chat partitions: {e}")))?;

        let success_count = result["success_count"].as_i64().unwrap_or(0);
        let total_requested = result["total_requested"].as_i64().unwrap_or(0);
        info!("Chat partitions created: {}/{} successful", success_count, total_requested);

        Ok(result)
    }

    /// Drop partitions older than the configured retention period
    pub async fn drop_old_partitions(&self, keep_days: i32) -> Result<i64> {
        info!("Dropping chat message partitions older than {} days", keep_days);

        let result = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT drop_old_chat_message_partitions($1)"
        )
        .bind(keep_days)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to drop old chat partitions: {e}")))?;

        let dropped_count = result["dropped_count"].as_i64().unwrap_or(0);
        if dropped_count > 0 {
            info!("Dropped {} old chat message partitions", dropped_count);
        }

        Ok(dropped_count)
    }

    /// Check partition health status
    pub async fn check_health(&self, days_ahead: i32) -> Result<ChatPartitionHealth> {
        let result_json = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT check_chat_message_partitions($1)"
        )
        .bind(days_ahead)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to check chat partition health: {e}")))?;

        let health: ChatPartitionHealth = serde_json::from_value(result_json)
            .map_err(|e| Error::Internal(format!("Failed to parse chat partition health: {e}")))?;

        match health.health_status.as_str() {
            "healthy" => {
                info!("Chat message partitions are healthy: {} partitions", health.total_partitions);
            }
            "warning" => {
                warn!("Chat message partitions warning: {} missing", health.missing_count);
            }
            _ => {
                warn!("Unknown chat partition health status: {}", health.health_status);
            }
        }

        Ok(health)
    }

    /// Start background task for automatic partition management and retention cleanup.
    ///
    /// This task performs time-based partition operations (fixed daily granularity):
    /// 1. Ensures future partitions exist (default: 30 days ahead)
    /// 2. Drops old partitions (default: keep 90 days)
    ///
    /// The task will shut down gracefully when the provided `CancellationToken` is cancelled.
    ///
    /// Note: Per-room message limit cleanup is handled by ChatService.start_cleanup_task()
    /// which runs more frequently (every 60 seconds) for near real-time enforcement.
    #[must_use]
    pub fn start_auto_management(&self, check_interval_hours: u64, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(
                tokio::time::Duration::from_secs(check_interval_hours * 3600),
            );

            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    () = cancel.cancelled() => {
                        info!("Chat partition management task cancelled, shutting down");
                        return;
                    }
                }

                // 1. Ensure future partitions exist
                match manager.check_health(DEFAULT_DAYS_AHEAD).await {
                    Ok(health) => {
                        if health.missing_count > 0 {
                            warn!("Found {} missing chat partitions, creating now", health.missing_count);
                            if let Err(e) = manager.ensure_future_partitions(DEFAULT_DAYS_AHEAD).await {
                                error!("Failed to create missing chat partitions: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to check chat partition health: {}", e);
                    }
                }

                // 2. Drop old partitions (time-based retention)
                if let Err(e) = manager.drop_old_partitions(DEFAULT_RETENTION_DAYS).await {
                    error!("Failed to drop old chat partitions: {}", e);
                }
            }
        })
    }
}

/// Ensure chat message partitions exist on application startup
///
/// Should be called during application bootstrap, after migrations.
pub async fn ensure_chat_partitions_on_startup(
    pool: &PgPool,
    settings: Arc<SettingsRegistry>
) -> Result<()> {
    let manager = ChatPartitionManager::new(pool.clone(), settings);

    // Step 1: Ensure future partitions exist
    manager.ensure_future_partitions(DEFAULT_DAYS_AHEAD).await?;

    // Step 2: Check health status
    let health = manager.check_health(DEFAULT_DAYS_AHEAD).await?;
    if health.health_status != "healthy" {
        warn!("Chat partition health check: {}", health.health_status);
    }

    // Step 3: Drop old partitions (initial cleanup)
    manager.drop_old_partitions(DEFAULT_RETENTION_DAYS).await?;

    info!("Chat message partition initialization completed (daily granularity, {} days ahead, {} days retention)",
        DEFAULT_DAYS_AHEAD, DEFAULT_RETENTION_DAYS);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_partition_health_deserialization() {
        let json = r#"{
            "total_partitions": 7,
            "total_size_mb": 128.5,
            "missing_partitions": [],
            "missing_count": 0,
            "health_status": "healthy"
        }"#;

        let health: ChatPartitionHealth = serde_json::from_str(json).unwrap();
        assert_eq!(health.total_partitions, 7);
        assert_eq!(health.missing_count, 0);
        assert_eq!(health.health_status, "healthy");
    }

    #[test]
    fn test_chat_partition_health_warning() {
        let json = r#"{
            "total_partitions": 5,
            "total_size_mb": 64.0,
            "missing_partitions": ["chat_messages_2026_08"],
            "missing_count": 1,
            "health_status": "warning"
        }"#;

        let health: ChatPartitionHealth = serde_json::from_str(json).unwrap();
        assert_eq!(health.total_partitions, 5);
        assert_eq!(health.missing_count, 1);
        assert_eq!(health.health_status, "warning");
        assert_eq!(health.missing_partitions.len(), 1);
    }

    #[test]
    fn test_default_retention_days() {
        assert_eq!(DEFAULT_RETENTION_DAYS, 90);
    }
}
