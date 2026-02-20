use anyhow::Result;
use sqlx::PgPool;
use std::time::Duration;
use tracing::{error, info, warn};

use synctv_core::service::MigrationLock;

const MIGRATION_LOCK_TTL: u64 = 300;
const MIGRATION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const MIGRATION_MAX_WAIT: Duration = Duration::from_mins(5);

/// Maximum time to wait for the PostgreSQL advisory lock in the Redis-fallback
/// path before giving up with an error.
const PG_ADVISORY_LOCK_MAX_WAIT: Duration = Duration::from_secs(60);
/// Initial backoff before the first retry of `pg_try_advisory_lock`.
const PG_ADVISORY_LOCK_INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// Maximum per-retry backoff for `pg_try_advisory_lock`.
const PG_ADVISORY_LOCK_MAX_BACKOFF: Duration = Duration::from_secs(8);
/// Stable integer key used with `pg_try_advisory_lock` / `pg_advisory_unlock`.
/// Hash of "synctv_migration" kept in sync with `PgAdvisoryMigrationLock`.
const PG_ADVISORY_LOCK_KEY: i64 = 0x73796E63_74766D69_u64 as i64;

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

/// Run migrations under a PostgreSQL advisory lock with exponential backoff.
///
/// Used as a fallback when Redis is unavailable at startup, to prevent
/// multiple replicas from running migrations concurrently and causing
/// conflicts.
///
/// Uses `pg_try_advisory_lock` (non-blocking) in a retry loop with exponential
/// backoff so that replicas that didn't win the initial race wait gracefully
/// instead of failing immediately. Once the lock is acquired:
/// - If another replica already completed migrations, skip re-running them.
/// - If migrations are not yet complete, run them.
/// - If the lock cannot be acquired within `PG_ADVISORY_LOCK_MAX_WAIT`, return
///   an error.
async fn run_migrations_with_pg_advisory_lock(pool: &PgPool) -> Result<()> {
    let start = tokio::time::Instant::now();
    let mut backoff = PG_ADVISORY_LOCK_INITIAL_BACKOFF;

    // Acquire a dedicated connection for the session-scoped advisory lock.
    // The same connection must be used for both acquire and release.
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to acquire DB connection for PG advisory lock: {e}"))?;

    loop {
        // pg_try_advisory_lock returns true if acquired, false if already held.
        let acquired: (bool,) = sqlx::query_as("SELECT pg_try_advisory_lock($1)")
            .bind(PG_ADVISORY_LOCK_KEY)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| anyhow::anyhow!("pg_try_advisory_lock query failed: {e}"))?;

        if acquired.0 {
            info!("PostgreSQL advisory lock acquired, checking migration state");

            // Another replica may have completed migrations while we waited.
            // Avoid re-running migrations if they are already applied.
            if migrations_already_applied(pool).await {
                info!("Migrations already applied by another replica, skipping (PG advisory lock path)");
                let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
                    .bind(PG_ADVISORY_LOCK_KEY)
                    .execute(&mut *conn)
                    .await;
                return Ok(());
            }

            info!("Running migrations under PostgreSQL advisory lock");
            let result = run_migrate(pool).await;

            // Always release the advisory lock on the same connection.
            let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(PG_ADVISORY_LOCK_KEY)
                .execute(&mut *conn)
                .await;

            return result;
        }

        // Lock is held by another replica.
        let elapsed = start.elapsed();
        if elapsed >= PG_ADVISORY_LOCK_MAX_WAIT {
            return Err(anyhow::anyhow!(
                "Timed out waiting for PostgreSQL advisory lock after {}s. \
                 Another replica may be running migrations.",
                elapsed.as_secs()
            ));
        }

        // Cap backoff at the maximum and ensure we do not exceed the remaining
        // wait budget.
        let remaining = PG_ADVISORY_LOCK_MAX_WAIT.saturating_sub(elapsed);
        let sleep_for = backoff.min(remaining);

        info!(
            "PostgreSQL advisory lock held by another replica, \
             retrying in {}ms (elapsed {}s / {}s)...",
            sleep_for.as_millis(),
            elapsed.as_secs(),
            PG_ADVISORY_LOCK_MAX_WAIT.as_secs(),
        );

        tokio::time::sleep(sleep_for).await;

        // Exponential backoff: double the interval up to the cap.
        backoff = (backoff * 2).min(PG_ADVISORY_LOCK_MAX_BACKOFF);
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
