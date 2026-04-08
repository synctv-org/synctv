//! Integration tests for audit partition management against real PostgreSQL.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use synctv_core::service::{AlwaysLeader, AuditPartitionManager};
use synctv_core_testing::create_test_pool;

#[tokio::test]
#[ignore = "Requires Docker-backed PostgreSQL"]
async fn audit_partition_manager_get_stats_returns_partition_stats() {
    let (_postgres, pool) = create_test_pool().await;
    let manager = AuditPartitionManager::new(pool, Arc::new(AlwaysLeader));

    let stats = manager
        .get_stats()
        .await
        .expect("audit partition stats function should exist and return JSON");

    assert!(stats.total_partitions >= 1);
    assert!(stats.total_records >= 0);
}
