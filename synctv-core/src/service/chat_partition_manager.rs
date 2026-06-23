//! Chat message partition management service
//!
//! Automatically manages chat message partition creation, retention cleanup,
//! and health monitoring with fixed daily granularity.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::LeaderCheck;
use crate::bootstrap::acquire_unbounded_ddl_connection;
use crate::repository::query_builder::trusted_dynamic_sql;
use crate::service::partitioning::{
    current_database_date, len_to_i32, len_to_i64, partition_index_sql, quote_ident,
    size_centi_mib, table_exists, wait_for_initial_leader, PartitionIndexSpec, PartitionNameRow,
    PartitionSizeRow, STARTUP_RUNS_RETENTION_CLEANUP,
};
use crate::{Error, Result};

/// Default retention period in days for chat messages
const DEFAULT_RETENTION_DAYS: i32 = 90;

/// Default days to create ahead
const DEFAULT_DAYS_AHEAD: i32 = 30;

const CHAT_PARTITION_INDEXES: &[PartitionIndexSpec] = &[
    PartitionIndexSpec {
        suffix: "idx_room_pagination",
        definition: "(room_id, created_at DESC, id DESC)",
    },
    PartitionIndexSpec {
        suffix: "idx_user_created",
        definition: "(user_id, created_at DESC)",
    },
    PartitionIndexSpec {
        suffix: "idx_created_at",
        definition: "(created_at DESC)",
    },
    PartitionIndexSpec {
        suffix: "idx_status",
        definition: "(room_id, status, created_at DESC, id DESC)",
    },
    PartitionIndexSpec {
        suffix: "idx_reply_target",
        definition: "(reply_to_message_id, reply_to_message_created_at)",
    },
    PartitionIndexSpec {
        suffix: "idx_content_search",
        definition: "USING gin(content_search)",
    },
    PartitionIndexSpec {
        suffix: "idx_content_trgm",
        definition: "USING gin(content gin_trgm_ops)",
    },
    PartitionIndexSpec {
        suffix: "idx_playback_media",
        definition: r"(
            room_id,
            ((metadata #>> '{playback,media_id}')),
            (
                CASE
                    WHEN jsonb_typeof(metadata #> '{playback,position_seconds}') = 'number'
                    THEN (metadata #>> '{playback,position_seconds}')::double precision
                    ELSE NULL
                END
            ),
            created_at,
            id
        )
        WHERE metadata ? 'playback'",
    },
    PartitionIndexSpec {
        suffix: "idx_playback_playlist_target",
        definition: r"(
            room_id,
            ((metadata #>> '{playback,playlist_id}')),
            ((metadata #>> '{playback,target_hex}')),
            (
                CASE
                    WHEN jsonb_typeof(metadata #> '{playback,position_seconds}') = 'number'
                    THEN (metadata #>> '{playback,position_seconds}')::double precision
                    ELSE NULL
                END
            ),
            created_at,
            id
        )
        WHERE metadata ? 'playback'",
    },
];

/// Health check result for chat message partitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatPartitionHealth {
    pub total_partitions: i32,
    pub total_size_centi_mib: i64,
    pub missing_partitions: Vec<String>,
    pub missing_count: i32,
    pub health_status: String,
}

/// Chat message partition manager (fixed daily granularity)
#[derive(Clone)]
pub struct ChatPartitionManager {
    pool: PgPool,
    leader_check: Arc<dyn LeaderCheck>,
}

impl ChatPartitionManager {
    /// Create a new partition manager with a leader check.
    ///
    /// Automatic partition management only runs on the leader node.
    #[must_use]
    pub fn new(pool: PgPool, leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self { pool, leader_check }
    }

    /// Ensure partitions exist for the next N days
    pub async fn ensure_future_partitions(&self, days_ahead: i32) -> Result<i32> {
        info!(
            "Ensuring chat message partitions for next {} days",
            days_ahead
        );

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
            let partition_name = format!("chat_messages_{}", start_date.format("%Y_%m_%d"));
            let partition_ident = quote_ident(&partition_name);

            sqlx::query(trusted_dynamic_sql(format!(
                "CREATE TABLE IF NOT EXISTS {partition_ident} PARTITION OF chat_messages \
                 FOR VALUES FROM ('{start_date}') TO ('{end_date}')"
            )))
            .execute(&mut *conn)
            .await
            .map_err(|e| Error::Internal(format!("Failed to create chat partition: {e}")))?;

            for spec in CHAT_PARTITION_INDEXES {
                sqlx::query(trusted_dynamic_sql(partition_index_sql(
                    &partition_name,
                    &partition_ident,
                    *spec,
                )))
                .execute(&mut *conn)
                .await
                .map_err(|e| {
                    Error::Internal(format!("Failed to create chat partition index: {e}"))
                })?;
            }
        }

        let total_requested = days_ahead + 1;
        info!(
            "Chat partitions created: {}/{} successful",
            total_requested, total_requested
        );

        Ok(total_requested)
    }

    /// Drop partitions older than the configured retention period
    pub async fn drop_old_partitions(&self, keep_days: i32) -> Result<i64> {
        info!(
            "Dropping chat message partitions older than {} days",
            keep_days
        );

        let mut conn = acquire_unbounded_ddl_connection(&self.pool)
            .await
            .map_err(|e| Error::Internal(format!("Failed to acquire DDL connection: {e}")))?;
        let current_date = current_database_date(&self.pool).await?;
        let cutoff_date = current_date - chrono::Duration::days(i64::from(keep_days));
        let cutoff_name = format!("chat_messages_{}", cutoff_date.format("%Y_%m_%d"));
        let partitions = sqlx::query_as!(
            PartitionNameRow,
            r#"
            SELECT tablename AS "tablename!"
             FROM pg_tables
             WHERE schemaname = 'public'
               AND tablename LIKE 'chat_messages_%'
               AND tablename ~ '^chat_messages_[0-9]{4}_[0-9]{2}_[0-9]{2}$'
               AND tablename < $1
             ORDER BY tablename
             "#,
            cutoff_name
        )
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| Error::Internal(format!("Failed to drop old chat partitions: {e}")))?
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
            .map_err(|e| Error::Internal(format!("Failed to drop old chat partition: {e}")))?;
        }

        let dropped_count = len_to_i64(partitions.len(), "dropped chat partition count")?;
        if dropped_count > 0 {
            info!("Dropped {} old chat message partitions", dropped_count);
        }

        Ok(dropped_count)
    }

    /// Check partition health status
    pub async fn check_health(&self, days_ahead: i32) -> Result<ChatPartitionHealth> {
        if days_ahead < 0 {
            return Err(Error::InvalidInput(
                "days_ahead must be greater than or equal to 0".to_string(),
            ));
        }

        let current_date = current_database_date(&self.pool).await?;
        let mut missing_partitions = Vec::new();
        for offset in 0..=days_ahead {
            let date = current_date + chrono::Duration::days(i64::from(offset));
            let partition_name = format!("chat_messages_{}", date.format("%Y_%m_%d"));
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
               AND tablename LIKE 'chat_messages_%'
               AND tablename ~ '^chat_messages_[0-9]{4}_[0-9]{2}_[0-9]{2}$'
             "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Error::Internal(format!("Failed to check chat partition health: {e}")))?;

        let total_partitions = len_to_i32(rows.len(), "chat partition count")?;
        let total_size_bytes = rows.iter().map(|row| row.size_bytes).sum::<i64>();
        let missing_count = len_to_i32(missing_partitions.len(), "missing chat partition count")?;
        let health = ChatPartitionHealth {
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
                    "Chat message partitions are healthy: {} partitions",
                    health.total_partitions
                );
            }
            "warning" => {
                warn!(
                    "Chat message partitions warning: {} missing",
                    health.missing_count
                );
            }
            _ => {
                warn!(
                    "Unknown chat partition health status: {}",
                    health.health_status
                );
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
    /// Note: Per-room message limit cleanup is handled by `ChatService.start_cleanup_task()`
    /// which runs more frequently (every 60 seconds) for near real-time enforcement.
    #[must_use]
    pub fn start_auto_management(
        &self,
        check_interval_hours: u64,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();

        crate::spawn::spawn_monitored("chat_partition_manager", async move {
            if !wait_for_initial_leader(
                manager.leader_check.clone(),
                cancel.clone(),
                "chat partition management",
            )
            .await
            {
                info!("Chat partition management task cancelled before leadership was established");
                return;
            }

            run_chat_partition_maintenance(&manager).await;

            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
                check_interval_hours * 3600,
            ));
            interval.tick().await;

            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    () = cancel.cancelled() => {
                        info!("Chat partition management task cancelled, shutting down");
                        return;
                    }
                }

                // Only run partition management on the leader node
                if !manager.leader_check.is_leader() {
                    info!("Skipping chat partition management (not leader)");
                    continue;
                }

                run_chat_partition_maintenance(&manager).await;
            }
        })
    }
}

async fn run_chat_partition_maintenance(manager: &ChatPartitionManager) {
    match manager.check_health(DEFAULT_DAYS_AHEAD).await {
        Ok(health) => {
            if health.missing_count > 0 {
                warn!(
                    "Found {} missing chat partitions, creating now",
                    health.missing_count
                );
                if let Err(e) = manager.ensure_future_partitions(DEFAULT_DAYS_AHEAD).await {
                    error!("Failed to create missing chat partitions: {}", e);
                }
            }
        }
        Err(e) => {
            error!("Failed to check chat partition health: {}", e);
        }
    }

    if let Err(e) = manager.drop_old_partitions(DEFAULT_RETENTION_DAYS).await {
        error!("Failed to drop old chat partitions: {}", e);
    }
}

async fn initialize_chat_partitions_on_startup(
    pool: &PgPool,
    run_retention_cleanup: bool,
) -> Result<()> {
    let manager = ChatPartitionManager::new(pool.clone(), Arc::new(super::AlwaysLeader));

    // Step 1: Ensure future partitions exist
    manager.ensure_future_partitions(DEFAULT_DAYS_AHEAD).await?;

    // Step 2: Check health status
    let health = manager.check_health(DEFAULT_DAYS_AHEAD).await?;
    if health.health_status != "healthy" {
        warn!("Chat partition health check: {}", health.health_status);
    }

    // Startup initialization is per-replica readiness work only. Retention cleanup
    // stays leader-gated in the background task to avoid duplicate startup DDL.
    if run_retention_cleanup {
        manager.drop_old_partitions(DEFAULT_RETENTION_DAYS).await?;
    }

    info!("Chat message partition initialization completed (daily granularity, {} days ahead, {} days retention)",
        DEFAULT_DAYS_AHEAD, DEFAULT_RETENTION_DAYS);

    Ok(())
}

/// Ensure chat message partitions exist on application startup
///
/// Should be called during application bootstrap, after migrations.
///
/// Startup initialization runs on every node because partitions must exist
/// before any node can insert data. Retention cleanup remains leader-gated in
/// the background task, which performs an initial run as soon as leadership is
/// established instead of waiting a full check interval.
pub async fn ensure_chat_partitions_on_startup(pool: &PgPool) -> Result<()> {
    initialize_chat_partitions_on_startup(pool, STARTUP_RUNS_RETENTION_CLEANUP).await
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

        let health: ChatPartitionHealth =
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

        let health: ChatPartitionHealth = ok(
            serde_json::from_str(json),
            "warning health JSON should deserialize",
        );
        assert_eq!(health.total_partitions, 5);
        assert_eq!(health.missing_count, 1);
        assert_eq!(health.health_status, "warning");
        assert_eq!(health.missing_partitions.len(), 1);
    }

    #[test]
    fn chat_partition_indexes_match_search_and_playback_paths() {
        let partition_name = "chat_messages_2026_06_23";
        let partition_ident = quote_ident(partition_name);
        let sql = CHAT_PARTITION_INDEXES
            .iter()
            .map(|spec| partition_index_sql(partition_name, &partition_ident, *spec))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(CHAT_PARTITION_INDEXES.len(), 9);
        assert!(sql.contains(
            r#"CREATE INDEX IF NOT EXISTS "chat_messages_2026_06_23_idx_content_search" ON "chat_messages_2026_06_23" USING gin(content_search)"#
        ));
        assert!(sql.contains(
            r#"CREATE INDEX IF NOT EXISTS "chat_messages_2026_06_23_idx_content_trgm" ON "chat_messages_2026_06_23" USING gin(content gin_trgm_ops)"#
        ));
        assert!(sql.contains(
            r#"CREATE INDEX IF NOT EXISTS "chat_messages_2026_06_23_idx_reply_target" ON "chat_messages_2026_06_23" (reply_to_message_id, reply_to_message_created_at)"#
        ));
        assert!(sql.contains(
            r#"CREATE INDEX IF NOT EXISTS "chat_messages_2026_06_23_idx_playback_media" ON "chat_messages_2026_06_23""#
        ));
        assert!(sql.contains(
            r#"CREATE INDEX IF NOT EXISTS "chat_messages_2026_06_23_idx_playback_playlist_target" ON "chat_messages_2026_06_23""#
        ));
    }
}
