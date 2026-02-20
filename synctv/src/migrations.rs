use anyhow::Result;
use sqlx::PgPool;
use std::time::Duration;
use tracing::{error, info, warn};

use synctv_core::service::MigrationLock;

const MIGRATION_LOCK_TTL: u64 = 300;
const MIGRATION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const MIGRATION_MAX_WAIT: Duration = Duration::from_mins(5);

/// Run database migrations using a distributed lock for multi-replica
/// deployments.
///
/// The caller supplies a `MigrationLock` implementation (e.g.
/// `DistributedLock` backed by Redis). If the lock cannot be acquired due
/// to an infrastructure error, falls back to a PostgreSQL advisory lock.
///
/// `key_prefix` is the configured Redis key prefix (e.g., "synctv:") used to
/// namespace the migration lock key, avoiding conflicts when multiple SyncTV
/// instances share the same Redis.
pub async fn run_migrations(pool: &PgPool, lock: &dyn MigrationLock, key_prefix: &str) -> Result<()> {
    info!("Running database migrations...");

    run_migrations_with_lock(pool, lock, key_prefix).await?;

    info!("Migrations completed");
    Ok(())
}

/// Execute `sqlx::migrate!` against the pool. This is the single place that
/// calls the migration macro so it is never duplicated.
async fn run_migrate(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("../migrations")
        .run(pool)
        .await
        .map_err(|e| {
            error!("Failed to run migrations: {}", e);
            anyhow::anyhow!("Migration failed: {e}")
        })
}

/// Check whether all known migrations have already been applied by comparing
/// the migrator's list against the `_sqlx_migrations` table.
async fn migrations_already_applied(pool: &PgPool) -> bool {
    let migrator = sqlx::migrate!("../migrations");
    let applied: Vec<(i64,)> = match sqlx::query_as(
        "SELECT version FROM _sqlx_migrations ORDER BY version",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(_) => return false, // table may not exist yet
    };

    let applied_versions: std::collections::HashSet<i64> =
        applied.into_iter().map(|(v,)| v).collect();

    migrator
        .migrations
        .iter()
        .all(|m| applied_versions.contains(&m.version))
}

/// Run migrations under a distributed lock so that only one replica in a
/// cluster performs the migration. Other replicas wait and verify completion.
async fn run_migrations_with_lock(pool: &PgPool, lock: &dyn MigrationLock, key_prefix: &str) -> Result<()> {
    let migration_lock_key = format!("{}migration", key_prefix);

    match lock.acquire(&migration_lock_key, MIGRATION_LOCK_TTL).await {
        Ok(Some(lock_value)) => {
            info!("Acquired migration lock, running migrations");
            let result = run_migrate(pool).await;
            release_lock(lock, &migration_lock_key, &lock_value).await;
            result
        }
        Ok(None) => wait_for_lock_and_migrate(pool, lock, &migration_lock_key).await,
        Err(e) => {
            warn!(
                "Failed to acquire migration lock (Redis error): {}. \
                 Falling back to PostgreSQL advisory lock to prevent concurrent migrations.",
                e
            );
            run_migrations_with_pg_advisory_lock(pool).await
        }
    }
}

/// Run migrations under a PostgreSQL advisory lock.
///
/// Used as a fallback when Redis is unavailable at startup, to prevent
/// multiple replicas from running migrations concurrently and causing
/// conflicts.
async fn run_migrations_with_pg_advisory_lock(pool: &PgPool) -> Result<()> {
    let pg_lock = synctv_core::service::PgAdvisoryMigrationLock::new(pool.clone());
    let lock_key = "pg_advisory_migration";

    match pg_lock.acquire(lock_key, MIGRATION_LOCK_TTL).await {
        Ok(Some(_lock_value)) => {
            info!("PostgreSQL advisory lock acquired, running migrations");

            // Check if migrations already applied (another node may have just finished)
            if migrations_already_applied(pool).await {
                info!("Migrations already applied, skipping (PG advisory lock path)");
                let _ = pg_lock.release(lock_key, "pg_advisory").await;
                return Ok(());
            }

            let result = run_migrate(pool).await;
            let _ = pg_lock.release(lock_key, "pg_advisory").await;
            result
        }
        Ok(None) => {
            Err(anyhow::anyhow!("Failed to acquire PostgreSQL advisory lock (already held)"))
        }
        Err(e) => {
            Err(anyhow::anyhow!("Failed to acquire PostgreSQL advisory lock: {e}"))
        }
    }
}

/// Another node holds the lock. Poll until it is released, then verify whether
/// migrations still need to run.
async fn wait_for_lock_and_migrate(
    pool: &PgPool,
    lock: &dyn MigrationLock,
    lock_key: &str,
) -> Result<()> {
    info!("Another node is running migrations, waiting...");

    let max_attempts = (MIGRATION_MAX_WAIT.as_secs() / MIGRATION_POLL_INTERVAL.as_secs()) as u32;
    let mut attempts: u32 = 0;

    loop {
        tokio::time::sleep(MIGRATION_POLL_INTERVAL).await;
        attempts += 1;

        match lock.acquire(lock_key, MIGRATION_LOCK_TTL).await {
            Ok(Some(lock_value)) => {
                // We got the lock. The previous holder likely finished. Check
                // whether migrations are already applied to avoid redundant work.
                if migrations_already_applied(pool).await {
                    info!("Migrations already applied by another node, skipping");
                    release_lock(lock, lock_key, &lock_value).await;
                    return Ok(());
                }

                info!("Migration lock acquired after waiting, running migrations");
                let result = run_migrate(pool).await;
                release_lock(lock, lock_key, &lock_value).await;
                return result;
            }
            Ok(None) if attempts < max_attempts => continue,
            Ok(None) => {
                return Err(anyhow::anyhow!(
                    "Timed out waiting for migration lock after {}s",
                    u64::from(attempts) * MIGRATION_POLL_INTERVAL.as_secs()
                ));
            }
            Err(e) => {
                warn!(
                    "Redis error while waiting for migration lock: {}. \
                     Falling back to PostgreSQL advisory lock.",
                    e
                );
                return run_migrations_with_pg_advisory_lock(pool).await;
            }
        }
    }
}

/// Best-effort lock release. Logs a warning on failure but never propagates
/// the error since migrations may have already succeeded.
async fn release_lock(lock: &dyn MigrationLock, lock_key: &str, lock_value: &str) {
    if let Err(e) = lock.release(lock_key, lock_value).await {
        warn!("Failed to release migration lock: {}", e);
    }
}
