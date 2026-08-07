use anyhow::Result;
use sqlx::{PgPool, Postgres};
use tracing::{error, info};

/// Run embedded database migrations.
///
/// Concurrency control is intentionally delegated to SQLx's migrator. For
/// PostgreSQL, SQLx acquires its own advisory lock and records version/checksum
/// state in `_sqlx_migrations`.
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    info!("Running database migrations...");

    let mut conn = pool.acquire().await.map_err(|e| {
        anyhow::anyhow!("Failed to acquire DB connection for running migrations: {e}")
    })?;

    run_migrate_with_connection(&mut conn).await?;

    info!("Migrations completed");
    Ok(())
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

async fn run_migrate_with_connection(
    conn: &mut sqlx::pool::PoolConnection<Postgres>,
) -> Result<()> {
    sqlx::query!("SET statement_timeout = 0")
        .execute(&mut **conn)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to disable statement_timeout for migrations: {e}"))?;

    sqlx::migrate!("../migrations")
        .run_direct(None, &mut **conn, false)
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
             in the current binary. Recreate the database or Docker volumes from scratch instead \
             of editing _sqlx_migrations by hand."
        ),
        sqlx::migrate::MigrateError::Dirty(version) => format!(
            "Migration failed: migration {version} is marked dirty. Resolve the partial migration \
             state before retrying startup."
        ),
        _ => format!("Migration failed: {err}"),
    }
}

async fn inspect_embedded_migrations_with_connection(
    conn: &mut sqlx::pool::PoolConnection<Postgres>,
) -> Result<EmbeddedMigrationsStatus> {
    let dirty_version: Option<i64> = match sqlx::query_scalar!(
        "SELECT version FROM _sqlx_migrations WHERE success = false ORDER BY version LIMIT 1"
    )
    .fetch_optional(&mut **conn)
    .await
    {
        Ok(version) => version,
        Err(err) if is_missing_migrations_table(&err) => {
            return Ok(EmbeddedMigrationsStatus::Pending);
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
    let applied = match sqlx::query!(
        "SELECT version, checksum FROM _sqlx_migrations WHERE success = true ORDER BY version"
    )
    .fetch_all(&mut **conn)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| (row.version, row.checksum))
            .collect::<Vec<_>>(),
        Err(err) if is_missing_migrations_table(&err) => {
            return Ok(EmbeddedMigrationsStatus::Pending);
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

#[cfg(test)]
mod tests {
    use super::{
        classify_embedded_migrations, describe_migration_error, inspect_embedded_migrations,
        EmbeddedMigrationsStatus, MigrationHistoryIssue,
    };

    #[test]
    fn version_mismatch_error_includes_rebuild_guidance() {
        let message = describe_migration_error(&sqlx::migrate::MigrateError::VersionMismatch(
            20_260_426_004,
        ));

        assert!(message.contains("20260426004"));
        assert!(message.contains("Recreate the database or Docker volumes"));
        assert!(message.contains("_sqlx_migrations"));
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
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn embedded_migration_status_surfaces_pool_acquire_failures() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        pool.close().await;

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

        sqlx::query!("SET statement_timeout = '250ms'")
            .execute(&mut *conn)
            .await
            .expect("should set a short timeout for the session");

        sqlx::query!("SET statement_timeout = 0")
            .execute(&mut *conn)
            .await
            .expect("migrations must be able to disable session statement timeout");

        sqlx::query!("SELECT pg_sleep(0.3)")
            .execute(&mut *conn)
            .await
            .expect("disabled statement_timeout should allow long migration statements");

        let timeout = sqlx::query_scalar!(
            r#"SELECT current_setting('statement_timeout') as "statement_timeout!""#
        )
        .fetch_one(&mut *conn)
        .await
        .expect("should read statement_timeout");

        assert_eq!(
            timeout, "0",
            "migration connection should run with statement_timeout disabled"
        );
    }
}
