use anyhow::Result;
use sqlx::PgPool;
use std::time::Duration;
use tracing::{error, info, warn};

const MIGRATION_LOCK_TTL: u64 = 300;
const MIGRATION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const MIGRATION_MAX_WAIT: Duration = Duration::from_mins(5);

/// Run database migrations, optionally using a Redis distributed lock for
/// multi-replica deployments.
///
/// When `redis_url` is empty, migrations run directly. When set, a distributed
/// lock ensures only one node runs migrations at a time. Nodes that wait for
/// the lock verify migrations are already applied before re-running them.
///
/// `key_prefix` is the configured Redis key prefix (e.g., "synctv:") used to
/// namespace the migration lock key, avoiding conflicts when multiple SyncTV
/// instances share the same Redis.
pub async fn run_migrations(pool: &PgPool, redis_url: &str, key_prefix: &str) -> Result<()> {
    info!("Running database migrations...");

    if redis_url.is_empty() {
        run_migrate(pool).await?;
    } else {
        run_migrations_with_lock(pool, redis_url, key_prefix).await?;
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
async fn run_migrations_with_lock(pool: &PgPool, redis_url: &str, key_prefix: &str) -> Result<()> {
    let redis_client = redis::Client::open(redis_url.to_owned())
        .map_err(|e| anyhow::anyhow!("Failed to open Redis for migration lock: {e}"))?;
    let redis_conn = redis_client
        .get_connection_manager()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to Redis for migration lock: {e}"))?;
    let lock = synctv_core::service::DistributedLock::new(redis_conn);

    let migration_lock_key = format!("{}migration", key_prefix);

    match lock.acquire(&migration_lock_key, MIGRATION_LOCK_TTL).await {
        Ok(Some(lock_value)) => {
            info!("Acquired migration lock, running migrations");
            let result = run_migrate(pool).await;
            release_lock(&lock, &migration_lock_key, &lock_value).await;
            result
        }
        Ok(None) => wait_for_lock_and_migrate(pool, &lock, &migration_lock_key).await,
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

/// PostgreSQL advisory lock key for migration coordination.
///
/// This is a stable hash of the string "synctv_migration" converted to i64
/// for use with `pg_try_advisory_lock`. Value computed once at compile time.
const PG_ADVISORY_LOCK_KEY: i64 = 0x73796E63_74766D69_u64 as i64; // "synctvm i" prefix

/// Run migrations under a PostgreSQL advisory lock.
///
/// Used as a fallback when Redis is unavailable at startup, to prevent
/// multiple replicas from running migrations concurrently and causing
/// conflicts. PostgreSQL advisory locks are automatically released when
/// the session ends, providing crash safety.
///
/// Uses `pg_try_advisory_lock` (non-blocking) with a retry loop capped at
/// `MIGRATION_LOCK_TTL` seconds to avoid indefinite hangs if another replica
/// crashes while holding the lock.
async fn run_migrations_with_pg_advisory_lock(pool: &PgPool) -> Result<()> {
    use sqlx::Acquire;

    let mut conn = pool.acquire().await
        .map_err(|e| anyhow::anyhow!("Failed to acquire DB connection for PG advisory lock: {e}"))?;

    info!("Acquiring PostgreSQL advisory lock for migrations (key={})", PG_ADVISORY_LOCK_KEY);

    let max_wait = Duration::from_secs(MIGRATION_LOCK_TTL);
    let retry_interval = Duration::from_secs(5);
    let start = tokio::time::Instant::now();

    loop {
        let acquired: (bool,) = sqlx::query_as("SELECT pg_try_advisory_lock($1)")
            .bind(PG_ADVISORY_LOCK_KEY)
            .fetch_one(conn.acquire().await?)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to attempt PostgreSQL advisory lock: {e}"))?;

        if acquired.0 {
            break;
        }

        if start.elapsed() >= max_wait {
            return Err(anyhow::anyhow!(
                "Timed out waiting for PostgreSQL advisory lock after {}s. \
                 Another replica may have crashed while holding the migration lock.",
                max_wait.as_secs()
            ));
        }

        info!(
            "PostgreSQL advisory lock held by another connection, retrying in {}s (elapsed: {}s / {}s)...",
            retry_interval.as_secs(),
            start.elapsed().as_secs(),
            max_wait.as_secs()
        );
        tokio::time::sleep(retry_interval).await;
    }

    info!("PostgreSQL advisory lock acquired, running migrations");

    // Check if migrations already applied (another node may have just finished)
    if migrations_already_applied(pool).await {
        info!("Migrations already applied, skipping (PG advisory lock path)");
        // Advisory lock is released when `conn` is dropped (session ends)
        return Ok(());
    }

    let result = run_migrate(pool).await;
    // Advisory lock is released when `conn` is dropped at end of scope
    result
}

/// Another node holds the lock. Poll until it is released, then verify whether
/// migrations still need to run.
async fn wait_for_lock_and_migrate(
    pool: &PgPool,
    lock: &synctv_core::service::DistributedLock,
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
async fn release_lock(lock: &synctv_core::service::DistributedLock, lock_key: &str, lock_value: &str) {
    if let Err(e) = lock.release(lock_key, lock_value).await {
        warn!("Failed to release migration lock: {}", e);
    }
}
