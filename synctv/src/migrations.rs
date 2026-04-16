use anyhow::Result;
use sqlx::{PgPool, Postgres};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use synctv_core::service::MigrationLock;

const MIGRATION_LOCK_TTL: u64 = 300;
const MIGRATION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const MIGRATION_MAX_WAIT: Duration = Duration::from_mins(5);

/// Maximum time to wait for the `PostgreSQL` advisory lock in the Redis-fallback
/// path before giving up with an error.
const PG_ADVISORY_LOCK_MAX_WAIT: Duration = MIGRATION_MAX_WAIT;
/// Initial backoff before the first retry of `pg_try_advisory_lock`.
const PG_ADVISORY_LOCK_INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// Maximum per-retry backoff for `pg_try_advisory_lock`.
const PG_ADVISORY_LOCK_MAX_BACKOFF: Duration = Duration::from_secs(8);
const MAX_CONSECUTIVE_REDIS_ERRORS: u32 = 5;
/// Stable integer key used with `pg_try_advisory_lock` / `pg_advisory_unlock`.
/// Hash of "`synctv_migration`" kept in sync with `PgAdvisoryMigrationLock`.
const PG_ADVISORY_LOCK_KEY: i64 = 0x7379_6E63_7476_6D69_i64;

/// Run database migrations using a distributed lock for multi-replica
/// deployments.
///
/// The caller supplies a `MigrationLock` implementation (e.g.
/// `DistributedLock` backed by Redis). If the lock cannot be acquired due
/// to an infrastructure error, falls back to a `PostgreSQL` advisory lock.
///
/// `key_prefix` is the configured Redis key prefix (e.g., "synctv:") used to
/// namespace the migration lock key, avoiding conflicts when multiple `SyncTV`
/// instances share the same Redis.
pub async fn run_migrations(
    pool: &PgPool,
    lock: std::sync::Arc<dyn MigrationLock>,
    key_prefix: &str,
    cluster_mode: bool,
) -> Result<()> {
    run_migrations_with_mode(pool, lock, key_prefix, cluster_mode).await
}

/// Inspect the current database against the embedded migration set and report
/// whether it is ready, pending, or broken.
pub async fn inspect_embedded_migrations(pool: &PgPool) -> Result<EmbeddedMigrationsStatus> {
    let mut conn = pool.acquire().await.map_err(|e| {
        anyhow::anyhow!("Failed to acquire DB connection for migration status inspection: {e}")
    })?;

    inspect_embedded_migrations_with_connection(&mut conn).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedMigrationsStatus {
    Ready,
    Pending,
    Broken(MigrationHistoryIssue),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationHistoryIssue {
    Dirty(i64),
    Drifted,
}

impl EmbeddedMigrationsStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Pending => "pending",
            Self::Broken(_) => "broken",
        }
    }

    pub fn detail(&self) -> Option<String> {
        match self {
            Self::Ready | Self::Pending => None,
            Self::Broken(MigrationHistoryIssue::Dirty(version)) => Some(format!(
                "migration {version} is marked dirty; resolve the partial migration state before retrying"
            )),
            Self::Broken(MigrationHistoryIssue::Drifted) => Some(
                "applied migration history does not match the embedded migration set".to_string(),
            ),
        }
    }
}

async fn run_migrations_with_mode(
    pool: &PgPool,
    lock: std::sync::Arc<dyn MigrationLock>,
    key_prefix: &str,
    cluster_mode: bool,
) -> Result<()> {
    run_migrations_with_runner(pool, lock, key_prefix, cluster_mode, true, &|pool| {
        Box::pin(run_migrate(pool))
    })
    .await
}

type MigrateFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;

async fn run_migrations_with_runner(
    pool: &PgPool,
    lock: std::sync::Arc<dyn MigrationLock>,
    key_prefix: &str,
    cluster_mode: bool,
    allow_pg_fallback: bool,
    migrate: &(dyn for<'a> Fn(&'a PgPool) -> MigrateFuture<'a> + Send + Sync),
) -> Result<()> {
    info!("Running database migrations...");

    run_migrations_with_lock(
        pool,
        lock,
        key_prefix,
        cluster_mode,
        allow_pg_fallback,
        migrate,
    )
    .await?;

    info!("Migrations completed");
    Ok(())
}

/// Execute `sqlx::migrate!` against the pool. This is the single place that
/// calls the migration macro so it is never duplicated.
async fn run_migrate(pool: &PgPool) -> Result<()> {
    let mut conn = pool.acquire().await.map_err(|e| {
        anyhow::anyhow!("Failed to acquire DB connection for running migrations: {e}")
    })?;

    run_migrate_with_connection(&mut conn).await
}

async fn run_migrate_with_connection(
    conn: &mut sqlx::pool::PoolConnection<Postgres>,
) -> Result<()> {
    sqlx::query("SET statement_timeout = 0")
        .execute(&mut **conn)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to disable statement_timeout for migrations: {e}"))?;

    sqlx::migrate!("../migrations")
        .run_direct(&mut **conn)
        .await
        .map_err(|e| {
            error!("Failed to run migrations: {}", e);
            anyhow::anyhow!("{}", describe_migration_error(&e))
        })
}

fn describe_migration_error(err: &sqlx::migrate::MigrateError) -> String {
    match err {
        sqlx::migrate::MigrateError::VersionMismatch(version) => format!(
            "Migration failed: migration {version} was previously applied but has been modified. \
             This database was initialized from a different migration history than the one embedded \
             in the current binary. For disposable local/dev databases, recreate the database or \
             Docker volumes from scratch instead of editing _sqlx_migrations by hand. For persistent \
             environments, restore the original migration file and add a new forward-only corrective migration."
        ),
        sqlx::migrate::MigrateError::Dirty(version) => format!(
            "Migration failed: migration {version} is marked dirty. Resolve the partial migration \
             state before retrying startup."
        ),
        _ => format!("Migration failed: {err}"),
    }
}

/// Check whether all known migrations have already been applied by comparing
/// the migrator's list against the `_sqlx_migrations` table.
async fn migrations_already_applied(pool: &PgPool) -> bool {
    let Ok(mut conn) = pool.acquire().await else {
        return false;
    };

    migrations_already_applied_with_connection(&mut conn).await
}

async fn inspect_embedded_migrations_with_connection(
    conn: &mut sqlx::pool::PoolConnection<Postgres>,
) -> Result<EmbeddedMigrationsStatus> {
    let dirty_version: Option<i64> = match sqlx::query_scalar(
        "SELECT version FROM _sqlx_migrations WHERE success = false ORDER BY version LIMIT 1",
    )
    .fetch_optional(&mut **conn)
    .await
    {
        Ok(version) => version,
        Err(err) if is_missing_migrations_table(&err) => {
            return Ok(EmbeddedMigrationsStatus::Pending)
        }
        Err(err) => {
            return Err(anyhow::anyhow!(
                "Failed to inspect dirty migration metadata in _sqlx_migrations: {err}"
            ));
        }
    };

    if let Some(version) = dirty_version {
        return Ok(EmbeddedMigrationsStatus::Broken(
            MigrationHistoryIssue::Dirty(version),
        ));
    }

    let migrator = sqlx::migrate!("../migrations");
    let applied: Vec<(i64, Vec<u8>)> = match sqlx::query_as(
        "SELECT version, checksum FROM _sqlx_migrations WHERE success = true ORDER BY version",
    )
    .fetch_all(&mut **conn)
    .await
    {
        Ok(rows) => rows,
        Err(err) if is_missing_migrations_table(&err) => {
            return Ok(EmbeddedMigrationsStatus::Pending)
        }
        Err(err) => {
            return Err(anyhow::anyhow!(
                "Failed to inspect applied migration metadata in _sqlx_migrations: {err}"
            ));
        }
    };

    Ok(classify_embedded_migrations(
        migrator
            .migrations
            .iter()
            .map(|migration| (migration.version, migration.checksum.as_ref())),
        applied,
    ))
}

fn classify_embedded_migrations<'a, M, A>(migrations: M, applied: A) -> EmbeddedMigrationsStatus
where
    M: IntoIterator<Item = (i64, &'a [u8])>,
    A: IntoIterator<Item = (i64, Vec<u8>)>,
{
    let expected: Vec<(i64, &'a [u8])> = migrations.into_iter().collect();
    let applied: Vec<(i64, Vec<u8>)> = applied.into_iter().collect();

    if applied.len() > expected.len() {
        return EmbeddedMigrationsStatus::Broken(MigrationHistoryIssue::Drifted);
    }

    for ((expected_version, expected_checksum), (applied_version, applied_checksum)) in
        expected.iter().zip(applied.iter())
    {
        if expected_version != applied_version || *expected_checksum != applied_checksum.as_slice()
        {
            return EmbeddedMigrationsStatus::Broken(MigrationHistoryIssue::Drifted);
        }
    }

    if applied.len() == expected.len() {
        EmbeddedMigrationsStatus::Ready
    } else {
        EmbeddedMigrationsStatus::Pending
    }
}

fn is_missing_migrations_table(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Database(database_error)
            if database_error.code().as_deref() == Some("42P01")
    )
}

async fn migrations_already_applied_with_connection(
    conn: &mut sqlx::pool::PoolConnection<Postgres>,
) -> bool {
    let migrator = sqlx::migrate!("../migrations");
    let applied: Vec<(i64, Vec<u8>)> = match sqlx::query_as(
        "SELECT version, checksum FROM _sqlx_migrations WHERE success = true ORDER BY version",
    )
    .fetch_all(&mut **conn)
    .await
    {
        Ok(rows) => rows,
        Err(_) => return false, // table may not exist yet
    };

    migrations_match_applied_set(
        migrator
            .migrations
            .iter()
            .map(|migration| (migration.version, migration.checksum.as_ref())),
        applied,
    )
}

fn migrations_match_applied_set<'a, M, A>(migrations: M, applied: A) -> bool
where
    M: IntoIterator<Item = (i64, &'a [u8])>,
    A: IntoIterator<Item = (i64, Vec<u8>)>,
{
    let expected_versions: std::collections::HashMap<i64, &'a [u8]> =
        migrations.into_iter().collect();
    let applied_versions: std::collections::HashMap<i64, Vec<u8>> = applied.into_iter().collect();

    if expected_versions.len() != applied_versions.len() {
        return false;
    }

    expected_versions
        .into_iter()
        .all(|(version, expected_checksum)| {
            applied_versions
                .get(&version)
                .is_some_and(|actual_checksum| actual_checksum.as_slice() == expected_checksum)
        })
}

/// Run migrations under a distributed lock so that only one replica in a
/// cluster performs the migration. Other replicas wait and verify completion.
async fn run_migrations_with_lock(
    pool: &PgPool,
    lock: std::sync::Arc<dyn MigrationLock>,
    key_prefix: &str,
    cluster_mode: bool,
    allow_pg_fallback: bool,
    migrate: &(dyn for<'a> Fn(&'a PgPool) -> MigrateFuture<'a> + Send + Sync),
) -> Result<()> {
    // Fast path: if migrations are already applied, skip lock acquisition
    // entirely.  This avoids thundering-herd pressure on Redis when many
    // test processes start simultaneously against databases cloned from a
    // pre-migrated template.  Use a short timeout so that pools with lazy
    // connections to unreachable hosts do not block startup.
    let already_applied =
        tokio::time::timeout(Duration::from_millis(500), migrations_already_applied(pool))
            .await
            .unwrap_or(false);
    if already_applied {
        info!("Migrations already applied, skipping lock acquisition");
        return Ok(());
    }

    let migration_lock_key = format!("{key_prefix}migration");

    match lock.acquire(&migration_lock_key, MIGRATION_LOCK_TTL).await {
        Ok(Some(lock_value)) => {
            info!("Acquired migration lock, running migrations");
            let result = run_migration_under_lock(
                lock.clone(),
                migration_lock_key.clone(),
                lock_value.clone(),
                migrate(pool),
            )
            .await;
            release_lock(lock.as_ref(), &migration_lock_key, &lock_value).await;
            result
        }
        Ok(None) => {
            wait_for_lock_and_migrate(pool, lock, &migration_lock_key, cluster_mode, migrate).await
        }
        Err(e) => {
            if !allow_pg_fallback {
                return Err(anyhow::anyhow!(
                    "Migration lock acquisition failed without Redis fallback enabled: {e}"
                ));
            }
            if cluster_mode {
                return Err(anyhow::anyhow!(
                    "cluster.enabled=true requires the Redis migration lock to remain healthy; \
                     refusing PostgreSQL advisory lock fallback after Redis lock acquisition error: {e}"
                ));
            }
            warn!(
                "Failed to acquire migration lock (Redis error): {}. \
                 Falling back to PostgreSQL advisory lock to prevent concurrent migrations.",
                e
            );
            run_migrations_with_pg_advisory_lock(pool).await
        }
    }
}

/// Run migrations under a `PostgreSQL` advisory lock with exponential backoff.
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
    let mut conn = pool.acquire().await.map_err(|e| {
        anyhow::anyhow!("Failed to acquire DB connection for PG advisory lock: {e}")
    })?;

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
            if migrations_already_applied_with_connection(&mut conn).await {
                info!("Migrations already applied by another replica, skipping (PG advisory lock path)");
                let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
                    .bind(PG_ADVISORY_LOCK_KEY)
                    .execute(&mut *conn)
                    .await;
                return Ok(());
            }

            info!("Running migrations under PostgreSQL advisory lock");
            let result = run_migrate_with_connection(&mut conn).await;

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

/// Check whether a Redis error is transient (e.g. broken pipe, connection
/// reset) and worth retrying rather than failing immediately.
fn is_transient_redis_error(err: &anyhow::Error) -> bool {
    // Try to inspect the typed redis error for a reliable classification.
    if let Some(redis_err) = err.downcast_ref::<redis::RedisError>() {
        use redis::ServerErrorKind;
        return match redis_err.kind() {
            // Network / IO errors (broken pipe, connection reset, refused, etc.)
            // Cluster connection not yet established — retryable.
            // Server asked us to retry (TRYAGAIN / BUSYLOADING).
            redis::ErrorKind::Io
            | redis::ErrorKind::ClusterConnectionNotFound
            | redis::ErrorKind::Server(ServerErrorKind::TryAgain | ServerErrorKind::BusyLoading) => {
                true
            }
            _ => false,
        };
    }

    // Fallback: the MigrationLock implementation may wrap the redis error
    // with additional context that obscures the original type.  Fall back to
    // string matching for the patterns we know indicate a transient fault.
    let s = err.to_string();
    s.contains("broken pipe")
        || s.contains("Connection reset")
        || s.contains("connection refused")
        || s.contains("timed out")
        || s.contains("Unexpected EOF")
}

/// Another node holds the lock. Poll until it is released, then verify whether
/// migrations still need to run.
async fn wait_for_lock_and_migrate(
    pool: &PgPool,
    lock: std::sync::Arc<dyn MigrationLock>,
    lock_key: &str,
    cluster_mode: bool,
    migrate: &(dyn for<'a> Fn(&'a PgPool) -> MigrateFuture<'a> + Send + Sync),
) -> Result<()> {
    info!("Another node is running migrations, waiting...");

    let max_attempts = MIGRATION_MAX_WAIT.as_secs() / MIGRATION_POLL_INTERVAL.as_secs();
    let mut attempts = 0_u64;
    let mut consecutive_redis_errors: u32 = 0;

    loop {
        tokio::time::sleep(MIGRATION_POLL_INTERVAL).await;
        attempts += 1;

        match lock.acquire(lock_key, MIGRATION_LOCK_TTL).await {
            Ok(Some(lock_value)) => {
                // We got the lock. The previous holder likely finished. Check
                // whether migrations are already applied to avoid redundant work.
                if migrations_already_applied(pool).await {
                    info!("Migrations already applied by another node, skipping");
                    release_lock(lock.as_ref(), lock_key, &lock_value).await;
                    return Ok(());
                }

                info!("Migration lock acquired after waiting, running migrations");
                let result = run_migration_under_lock(
                    lock.clone(),
                    lock_key.to_string(),
                    lock_value.clone(),
                    migrate(pool),
                )
                .await;
                release_lock(lock.as_ref(), lock_key, &lock_value).await;
                return result;
            }
            Ok(None) if attempts < max_attempts => {
                consecutive_redis_errors = 0;
            }
            Ok(None) => {
                return Err(anyhow::anyhow!(
                    "Timed out waiting for migration lock after {}s",
                    attempts * MIGRATION_POLL_INTERVAL.as_secs()
                ));
            }
            Err(e) => {
                consecutive_redis_errors += 1;
                if is_transient_redis_error(&e)
                    && consecutive_redis_errors < MAX_CONSECUTIVE_REDIS_ERRORS
                    && attempts < max_attempts
                {
                    warn!(
                        "Transient Redis error while waiting for migration lock (attempt {consecutive_redis_errors}/{MAX_CONSECUTIVE_REDIS_ERRORS}): {e}"
                    );
                    continue;
                }
                let mode_message = if cluster_mode {
                    "cluster.enabled=true requires Redis migration locking"
                } else {
                    "standalone startup requires the same Redis migration lock to remain healthy once another node already owns it"
                };
                return Err(anyhow::anyhow!(
                    "{mode_message}; refusing PostgreSQL advisory lock fallback while waiting for Redis migration lock: {e}"
                ));
            }
        }
    }
}

async fn run_migration_under_lock<Fut>(
    lock: std::sync::Arc<dyn MigrationLock>,
    lock_key: String,
    lock_value: String,
    migrate: Fut,
) -> Result<()>
where
    Fut: std::future::Future<Output = Result<()>>,
{
    run_migration_under_lock_with_refresh_interval(
        lock,
        lock_key,
        lock_value,
        Duration::from_secs((MIGRATION_LOCK_TTL / 3).max(1)),
        migrate,
    )
    .await
}

async fn run_migration_under_lock_with_refresh_interval<Fut>(
    lock: std::sync::Arc<dyn MigrationLock>,
    lock_key: String,
    lock_value: String,
    refresh_interval: Duration,
    migrate: Fut,
) -> Result<()>
where
    Fut: std::future::Future<Output = Result<()>>,
{
    let keepalive_cancel = CancellationToken::new();
    let mut keepalive = spawn_lock_keepalive(
        lock,
        lock_key,
        lock_value,
        refresh_interval,
        keepalive_cancel.clone(),
    );
    tokio::pin!(migrate);

    let migrate_result = tokio::select! {
        result = &mut migrate => {
            keepalive_cancel.cancel();
            Some(result)
        }
        keepalive_result = &mut keepalive => {
            return match keepalive_result {
                Ok(Ok(())) => Err(anyhow::anyhow!(
                    "Migration lock keepalive stopped before migrations completed"
                )),
                Ok(Err(err)) => Err(err),
                Err(join_err) if join_err.is_cancelled() => Err(anyhow::anyhow!(
                    "Migration lock keepalive task was cancelled before migrations completed"
                )),
                Err(join_err) => Err(anyhow::anyhow!(
                    "Migration lock keepalive task panicked: {join_err}"
                )),
            };
        }
    };

    let result = migrate_result.expect("migrate branch must produce a result");
    match keepalive.await {
        Ok(Err(err)) if result.is_ok() => Err(err),
        Err(join_err) if result.is_ok() => Err(anyhow::anyhow!(
            "Migration lock keepalive task panicked: {join_err}"
        )),
        _ => result,
    }
}

fn spawn_lock_keepalive(
    lock: std::sync::Arc<dyn MigrationLock>,
    lock_key: String,
    lock_value: String,
    refresh_interval: Duration,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(refresh_interval);
        ticker.tick().await;
        loop {
            tokio::select! {
                () = cancel.cancelled() => return Ok(()),
                _ = ticker.tick() => {
                    match lock.extend(&lock_key, &lock_value, MIGRATION_LOCK_TTL).await {
                        Ok(true) => {}
                        Ok(false) => {
                            warn!(
                                lock_key = %lock_key,
                                "Migration lock keepalive lost ownership while migrations are still running"
                            );
                            return Err(anyhow::anyhow!(
                                "Migration lock '{lock_key}' expired or was stolen while migrations were still running"
                            ));
                        }
                        Err(err) => {
                            warn!(
                                lock_key = %lock_key,
                                error = %err,
                                "Migration lock keepalive failed"
                            );
                            return Err(anyhow::anyhow!(
                                "Migration lock keepalive failed for '{lock_key}': {err}"
                            ));
                        }
                    }
                }
            }
        }
    })
}

/// Best-effort lock release. Logs a warning on failure but never propagates
/// the error since migrations may have already succeeded.
async fn release_lock(lock: &dyn MigrationLock, lock_key: &str, lock_value: &str) {
    if let Err(e) = lock.release(lock_key, lock_value).await {
        warn!("Failed to release migration lock: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_embedded_migrations, describe_migration_error, inspect_embedded_migrations,
        migrations_match_applied_set, run_migrations_with_mode, run_migrations_with_runner,
        EmbeddedMigrationsStatus, MigrationHistoryIssue, MIGRATION_LOCK_TTL, MIGRATION_MAX_WAIT,
        PG_ADVISORY_LOCK_MAX_WAIT,
    };
    use anyhow::anyhow;
    use sqlx::Row;
    use sqlx::{postgres::PgPoolOptions, PgPool};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;

    #[derive(Clone)]
    struct FailingMigrationLock {
        acquire_called: Arc<AtomicBool>,
    }

    #[derive(Clone)]
    struct WaitThenFailMigrationLock {
        acquire_calls: Arc<AtomicUsize>,
    }

    #[derive(Clone, Default)]
    struct ExtendTrackingMigrationLock {
        acquire_calls: Arc<AtomicUsize>,
        extend_calls: Arc<AtomicUsize>,
        release_calls: Arc<AtomicUsize>,
        allow_release: Arc<AtomicBool>,
        fail_extend: Arc<AtomicBool>,
        notify_extend: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl synctv_core::service::MigrationLock for FailingMigrationLock {
        async fn acquire(&self, _key: &str, _ttl_secs: u64) -> anyhow::Result<Option<String>> {
            self.acquire_called.store(true, Ordering::SeqCst);
            Err(anyhow!("redis unavailable"))
        }

        async fn release(&self, _key: &str, _lock_value: &str) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    #[async_trait::async_trait]
    impl synctv_core::service::MigrationLock for WaitThenFailMigrationLock {
        async fn acquire(&self, _key: &str, _ttl_secs: u64) -> anyhow::Result<Option<String>> {
            let call = self.acquire_calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                Ok(None)
            } else {
                Err(anyhow!("redis unavailable while waiting"))
            }
        }

        async fn release(&self, _key: &str, _lock_value: &str) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    #[async_trait::async_trait]
    impl synctv_core::service::MigrationLock for ExtendTrackingMigrationLock {
        async fn acquire(&self, _key: &str, _ttl_secs: u64) -> anyhow::Result<Option<String>> {
            self.acquire_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some("lock-value".to_string()))
        }

        async fn extend(
            &self,
            _key: &str,
            _lock_value: &str,
            _ttl_secs: u64,
        ) -> anyhow::Result<bool> {
            self.extend_calls.fetch_add(1, Ordering::SeqCst);
            self.notify_extend.notify_waiters();
            Ok(!self.fail_extend.load(Ordering::SeqCst))
        }

        async fn release(&self, _key: &str, _lock_value: &str) -> anyhow::Result<bool> {
            self.release_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.allow_release.load(Ordering::SeqCst))
        }
    }

    #[tokio::test]
    async fn cluster_mode_rejects_pg_advisory_fallback_after_redis_lock_error() {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
            .expect("connect_lazy should succeed");
        let lock = FailingMigrationLock {
            acquire_called: Arc::new(AtomicBool::new(false)),
        };

        let err = run_migrations_with_mode(&pool, Arc::new(lock.clone()), "test:", true)
            .await
            .expect_err("cluster mode must refuse PG advisory fallback after Redis lock errors");

        assert!(lock.acquire_called.load(Ordering::SeqCst));
        assert!(
            err.to_string()
                .contains("requires the Redis migration lock to remain healthy")
                || err
                    .to_string()
                    .contains("refusing PostgreSQL advisory lock fallback"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn standalone_mode_rejects_pg_advisory_fallback_after_waiting_lock_error() {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
            .expect("connect_lazy should succeed");
        let lock = WaitThenFailMigrationLock {
            acquire_calls: Arc::new(AtomicUsize::new(0)),
        };

        let err = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            run_migrations_with_mode(&pool, Arc::new(lock.clone()), "test:", false),
        )
        .await
        .expect("wait path should complete within a single poll interval")
        .expect_err("must not fall back to PG advisory lock after Redis wait-path error");

        assert_eq!(lock.acquire_calls.load(Ordering::SeqCst), 2);
        assert!(
            err.to_string()
                .contains("refusing PostgreSQL advisory lock fallback")
                || err
                    .to_string()
                    .contains("requires the same Redis migration lock to remain healthy"),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string()
                .contains("while waiting for Redis migration lock"),
            "wait-path error should clearly describe the failing phase: {err}"
        );
    }

    #[tokio::test]
    async fn standalone_pg_lock_path_does_not_attempt_nested_pg_fallback() {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
            .expect("connect_lazy should succeed");
        let lock = FailingMigrationLock {
            acquire_called: Arc::new(AtomicBool::new(false)),
        };

        let err = run_migrations_with_runner(
            &pool,
            Arc::new(lock.clone()),
            "test:",
            false,
            false,
            &|_pool: &PgPool| Box::pin(async { Ok(()) }),
        )
        .await
        .expect_err("standalone PG advisory path must fail directly without nested fallback");

        assert!(lock.acquire_called.load(Ordering::SeqCst));
        assert!(
            err.to_string().contains("without Redis fallback enabled"),
            "unexpected error: {err}"
        );
        assert!(
            !err.to_string()
                .contains("Falling back to PostgreSQL advisory lock"),
            "PG advisory lock path should not recursively fall back to itself: {err}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn acquired_migration_lock_is_extended_while_migrations_run() {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
            .expect("connect_lazy should succeed");
        let lock = Arc::new(ExtendTrackingMigrationLock {
            allow_release: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        });
        let lock_for_runner = Arc::clone(&lock);
        let extend_notified = lock.notify_extend.notified();
        tokio::pin!(extend_notified);

        let task = tokio::spawn(async move {
            run_migrations_with_runner(
                &pool,
                lock_for_runner,
                "test:",
                false,
                true,
                &|_pool: &PgPool| {
                    Box::pin(async {
                        tokio::time::sleep(Duration::from_secs(MIGRATION_LOCK_TTL / 3 + 1)).await;
                        Ok(())
                    })
                },
            )
            .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(MIGRATION_LOCK_TTL / 3 + 2)).await;
        extend_notified.await;

        let result = task.await.expect("migration task should join");
        assert!(
            result.is_ok(),
            "migration runner should succeed: {result:?}"
        );
        assert_eq!(lock.acquire_calls.load(Ordering::SeqCst), 1);
        assert!(
            lock.extend_calls.load(Ordering::SeqCst) >= 1,
            "migration lock must be periodically extended while migrations run"
        );
        assert_eq!(lock.release_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pg_advisory_lock_wait_budget_matches_primary_migration_wait_budget() {
        assert_eq!(
            PG_ADVISORY_LOCK_MAX_WAIT, MIGRATION_MAX_WAIT,
            "PostgreSQL advisory lock fallback must wait as long as the primary migration lock path"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn migration_runner_fails_when_keepalive_loses_lock() {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
            .expect("connect_lazy should succeed");
        let lock = Arc::new(ExtendTrackingMigrationLock {
            allow_release: Arc::new(AtomicBool::new(true)),
            fail_extend: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        });
        let lock_for_runner = Arc::clone(&lock);
        let extend_notified = lock.notify_extend.notified();
        tokio::pin!(extend_notified);

        let task = tokio::spawn(async move {
            run_migrations_with_runner(
                &pool,
                lock_for_runner,
                "test:",
                false,
                true,
                &|_pool: &PgPool| {
                    Box::pin(async {
                        tokio::time::sleep(Duration::from_secs(MIGRATION_LOCK_TTL / 3 + 1)).await;
                        Ok(())
                    })
                },
            )
            .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(MIGRATION_LOCK_TTL / 3 + 2)).await;
        extend_notified.await;

        let err = task
            .await
            .expect("migration task should join")
            .expect_err("keepalive loss must fail the migration runner");
        assert!(
            err.to_string().contains("expired or was stolen"),
            "unexpected error: {err}"
        );
        assert_eq!(lock.release_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn migration_match_requires_checksum_match() {
        let expected = vec![(1_i64, b"checksum-a".as_slice())];
        let applied = vec![(1_i64, b"checksum-b".to_vec())];

        assert!(
            !migrations_match_applied_set(expected, applied),
            "changed SQL checksum must force migrations to run again instead of being treated as already applied"
        );
    }

    #[test]
    fn version_mismatch_error_includes_rebuild_guidance() {
        let message = describe_migration_error(&sqlx::migrate::MigrateError::VersionMismatch(
            20_240_101_000_004,
        ));

        assert!(message.contains("20240101000004"));
        assert!(message.contains("recreate the database or Docker volumes"));
        assert!(message.contains("forward-only corrective migration"));
    }

    #[test]
    fn migration_match_requires_every_version() {
        let expected = vec![
            (1_i64, b"checksum-a".as_slice()),
            (2_i64, b"checksum-b".as_slice()),
        ];
        let applied = vec![(1_i64, b"checksum-a".to_vec())];

        assert!(
            !migrations_match_applied_set(expected, applied),
            "missing applied versions must not be treated as complete migration state"
        );
    }

    #[test]
    fn migration_match_accepts_exact_version_and_checksum_set() {
        let expected = vec![
            (1_i64, b"checksum-a".as_slice()),
            (2_i64, b"checksum-b".as_slice()),
        ];
        let applied = vec![
            (1_i64, b"checksum-a".to_vec()),
            (2_i64, b"checksum-b".to_vec()),
        ];

        assert!(
            migrations_match_applied_set(expected, applied),
            "matching version/checksum pairs should be treated as fully applied"
        );
    }

    #[test]
    fn migration_match_rejects_extra_applied_versions() {
        let expected = vec![(1_i64, b"checksum-a".as_slice())];
        let applied = vec![
            (1_i64, b"checksum-a".to_vec()),
            (2_i64, b"checksum-b".to_vec()),
        ];

        assert!(
            !migrations_match_applied_set(expected, applied),
            "extra applied versions must not be treated as the exact embedded migration state"
        );
    }

    #[test]
    fn embedded_migration_status_marks_exact_prefix_as_pending() {
        let expected = vec![
            (1_i64, b"checksum-a".as_slice()),
            (2_i64, b"checksum-b".as_slice()),
        ];
        let applied = vec![(1_i64, b"checksum-a".to_vec())];

        assert_eq!(
            classify_embedded_migrations(expected, applied),
            EmbeddedMigrationsStatus::Pending
        );
    }

    #[test]
    fn embedded_migration_status_marks_checksum_drift_as_broken() {
        let expected = vec![(1_i64, b"checksum-a".as_slice())];
        let applied = vec![(1_i64, b"checksum-b".to_vec())];

        assert_eq!(
            classify_embedded_migrations(expected, applied),
            EmbeddedMigrationsStatus::Broken(MigrationHistoryIssue::Drifted)
        );
    }

    #[test]
    fn embedded_migration_status_marks_gaps_as_broken() {
        let expected = vec![
            (1_i64, b"checksum-a".as_slice()),
            (2_i64, b"checksum-b".as_slice()),
        ];
        let applied = vec![(2_i64, b"checksum-b".to_vec())];

        assert_eq!(
            classify_embedded_migrations(expected, applied),
            EmbeddedMigrationsStatus::Broken(MigrationHistoryIssue::Drifted)
        );
    }

    #[tokio::test]
    async fn embedded_migration_status_surfaces_pool_acquire_failures() {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
            .expect("connect_lazy should succeed");

        let err = inspect_embedded_migrations(&pool)
            .await
            .expect_err("inspect should surface acquisition failures");

        assert!(
            err.to_string()
                .contains("Failed to acquire DB connection for migration status inspection"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn migrations_disable_statement_timeout_on_migration_connection() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let mut conn = pool.acquire().await.expect("should acquire connection");

        sqlx::query("SET statement_timeout = 1")
            .execute(&mut *conn)
            .await
            .expect("should set an aggressive timeout for the session");

        sqlx::query("SET statement_timeout = 0")
            .execute(&mut *conn)
            .await
            .expect("migrations must be able to disable session statement timeout");

        let row = sqlx::query("SHOW statement_timeout")
            .fetch_one(&mut *conn)
            .await
            .expect("should read statement_timeout");
        let timeout: String = row
            .try_get(0)
            .expect("SHOW statement_timeout should return a string value");

        assert_eq!(
            timeout, "0",
            "migration connection should run with statement_timeout disabled"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn standalone_pg_advisory_lock_path_reuses_the_lock_connection_for_migrations() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool_with_options_and_label(
            "synctv_test",
            "migration-pg-lock-single-conn",
            1,
            Duration::from_secs(30),
        )
        .await;
        let lock = Arc::new(FailingMigrationLock {
            acquire_called: Arc::new(AtomicBool::new(false)),
        });

        run_migrations_with_mode(&pool, lock, "test:", false)
            .await
            .expect(
                "PG advisory fallback should not deadlock when the pool has a single connection",
            );
    }
}
