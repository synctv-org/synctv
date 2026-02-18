use anyhow::Result;
use sqlx::PgPool;
use std::time::Duration;
use tracing::{error, info, warn};

const MIGRATION_LOCK_KEY: &str = "synctv:migration";
const MIGRATION_LOCK_TTL: u64 = 300;
const MIGRATION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const MIGRATION_MAX_WAIT: Duration = Duration::from_mins(5);

/// Run database migrations, optionally using a Redis distributed lock for
/// multi-replica deployments.
///
/// When `redis_url` is empty, migrations run directly. When set, a distributed
/// lock ensures only one node runs migrations at a time. Nodes that wait for
/// the lock verify migrations are already applied before re-running them.
pub async fn run_migrations(pool: &PgPool, redis_url: &str) -> Result<()> {
    info!("Running database migrations...");

    if redis_url.is_empty() {
        run_migrate(pool).await?;
    } else {
        run_migrations_with_lock(pool, redis_url).await?;
    }

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

/// Run migrations under a Redis distributed lock so that only one replica in a
/// cluster performs the migration. Other replicas wait and verify completion.
async fn run_migrations_with_lock(pool: &PgPool, redis_url: &str) -> Result<()> {
    let redis_client = redis::Client::open(redis_url.to_owned())
        .map_err(|e| anyhow::anyhow!("Failed to open Redis for migration lock: {e}"))?;
    let redis_conn = redis_client
        .get_connection_manager()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to Redis for migration lock: {e}"))?;
    let lock = synctv_core::service::DistributedLock::new(redis_conn);

    match lock.acquire(MIGRATION_LOCK_KEY, MIGRATION_LOCK_TTL).await {
        Ok(Some(lock_value)) => {
            info!("Acquired migration lock, running migrations");
            let result = run_migrate(pool).await;
            release_lock(&lock, &lock_value).await;
            result
        }
        Ok(None) => wait_for_lock_and_migrate(pool, &lock).await,
        Err(e) => {
            warn!(
                "Failed to acquire migration lock (Redis error): {}, running migrations directly",
                e
            );
            run_migrate(pool).await
        }
    }
}

/// Another node holds the lock. Poll until it is released, then verify whether
/// migrations still need to run.
async fn wait_for_lock_and_migrate(
    pool: &PgPool,
    lock: &synctv_core::service::DistributedLock,
) -> Result<()> {
    info!("Another node is running migrations, waiting...");

    let max_attempts = (MIGRATION_MAX_WAIT.as_secs() / MIGRATION_POLL_INTERVAL.as_secs()) as u32;
    let mut attempts: u32 = 0;

    loop {
        tokio::time::sleep(MIGRATION_POLL_INTERVAL).await;
        attempts += 1;

        match lock.acquire(MIGRATION_LOCK_KEY, MIGRATION_LOCK_TTL).await {
            Ok(Some(lock_value)) => {
                // We got the lock. The previous holder likely finished. Check
                // whether migrations are already applied to avoid redundant work.
                if migrations_already_applied(pool).await {
                    info!("Migrations already applied by another node, skipping");
                    release_lock(lock, &lock_value).await;
                    return Ok(());
                }

                info!("Migration lock acquired after waiting, running migrations");
                let result = run_migrate(pool).await;
                release_lock(lock, &lock_value).await;
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
                    "Redis error while waiting for migration lock: {}, running migrations directly",
                    e
                );
                return run_migrate(pool).await;
            }
        }
    }
}

/// Best-effort lock release. Logs a warning on failure but never propagates
/// the error since migrations may have already succeeded.
async fn release_lock(lock: &synctv_core::service::DistributedLock, lock_value: &str) {
    if let Err(e) = lock.release(MIGRATION_LOCK_KEY, lock_value).await {
        warn!("Failed to release migration lock: {}", e);
    }
}
