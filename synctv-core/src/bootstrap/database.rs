//! Database initialization

use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Postgres};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::Level;
use tracing::{error, info};

use crate::repository::query_builder::trusted_dynamic_sql;
use crate::resilience::timeout::DB_QUERY_TIMEOUT;
use crate::Config;

/// Initialize database connection pool
///
/// Note: Migrations should be run separately by the binary crate.
///
/// This convenience entry point intentionally does not spawn the background
/// pool metrics task because it has no cancellation token to bind the task
/// lifetime to. Callers that need the metrics task must use
/// [`init_database_with_cancel`] and register the returned handle with their
/// shutdown coordinator.
pub async fn init_database(config: &Config) -> Result<DatabaseInit> {
    init_database_with_cancel(config, None).await
}

#[derive(Debug)]
pub struct DatabaseInit {
    pub pool: PgPool,
    pub pools: DatabasePools,
    pub metrics_task: Option<JoinHandle<()>>,
}

/// Database pools for primary and allowlisted eventually consistent reads.
///
/// Repository methods opt in to `read()` deliberately. Keep writes,
/// transactions, auth/security checks, cache-building inputs, post-write
/// fanout, cursor-coupled snapshots, playback worker claims, and file
/// finalization on `primary()`. The full contract is documented in
/// `docs/src/content/docs/en/develop/implementation-contracts.mdx`.
#[derive(Debug, Clone)]
pub struct DatabasePools {
    primary: PgPool,
    read: PgPool,
    dedicated_read_pool: bool,
}

impl DatabasePools {
    #[must_use]
    pub fn new(primary: PgPool, read: Option<PgPool>) -> Self {
        let dedicated_read_pool = read.is_some();
        let read = read.unwrap_or_else(|| primary.clone());
        Self {
            primary,
            read,
            dedicated_read_pool,
        }
    }

    #[must_use]
    pub const fn primary(&self) -> &PgPool {
        &self.primary
    }

    #[must_use]
    pub const fn read(&self) -> &PgPool {
        &self.read
    }

    #[must_use]
    pub fn primary_pool(&self) -> PgPool {
        self.primary.clone()
    }

    #[must_use]
    pub fn read_pool(&self) -> PgPool {
        self.read.clone()
    }

    #[must_use]
    pub fn has_dedicated_read_pool(&self) -> bool {
        self.dedicated_read_pool
    }

    pub async fn close(&self) {
        if self.has_dedicated_read_pool() {
            self.read.close().await;
        }
        self.primary.close().await;
    }
}

/// Initialize database connection pool with an optional `CancellationToken`
/// for graceful shutdown of the background pool metrics task.
///
/// If `cancel` is `None`, no metrics task is spawned.
pub async fn init_database_with_cancel(
    config: &Config,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<DatabaseInit> {
    init_database_inner(config, cancel, false).await
}

/// Initialize the primary database pool plus the configured read-replica pool.
///
/// This is the application startup path. Primary-only maintenance commands use
/// [`init_database_with_cancel`] so replica availability cannot block
/// migrations or status checks.
pub async fn init_database_with_read_pool_and_cancel(
    config: &Config,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<DatabaseInit> {
    init_database_inner(config, cancel, true).await
}

async fn init_database_inner(
    config: &Config,
    cancel: Option<tokio_util::sync::CancellationToken>,
    include_read_pool: bool,
) -> Result<DatabaseInit> {
    let statement_timeout_ms = DB_QUERY_TIMEOUT.as_millis();
    let pg_client_min_messages =
        pg_client_min_messages_for_level(crate::logging::effective_log_level(&config.logging)?);

    let database_url = config.database_url();
    let pool = connect_database_pool(
        &database_url,
        "primary",
        config,
        statement_timeout_ms,
        pg_client_min_messages,
        false,
    )
    .await?;
    let read_pool = match (include_read_pool, config.database_read_url()) {
        (true, Some(read_database_url)) => Some(
            connect_database_pool(
                &read_database_url,
                "read",
                config,
                statement_timeout_ms,
                pg_client_min_messages,
                true,
            )
            .await?,
        ),
        _ => None,
    };
    let pools = DatabasePools::new(pool.clone(), read_pool);

    let pool_count = if pools.has_dedicated_read_pool() {
        2
    } else {
        1
    };
    let max_connections_per_pool = config.database.max_connections;
    let max_connections_total = max_connections_per_pool.saturating_mul(pool_count);
    crate::metrics::database::DB_POOL_SIZE_MAX.set(i64::from(max_connections_total));

    // Spawn periodic task to update pool usage metrics only when the caller
    // supplies a cancellation token and can therefore manage the task
    // lifecycle. Starting the task without any shutdown hook would leak it for
    // the rest of the process lifetime while it continues to hold the pool.
    let metrics_task = cancel.map(|token| {
        let pools = pools.clone();
        crate::spawn::spawn_monitored("db_pool_metrics", async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(15));
            loop {
                tokio::select! {
                    () = token.cancelled() => {
                        tracing::debug!("DB pool metrics task cancelled");
                        break;
                    }
                    _ = ticker.tick() => {}
                }
                record_database_pool_metrics(&pools, max_connections_per_pool);
                if pools.has_dedicated_read_pool() {
                    record_pool_utilization("read", pools.read(), max_connections_per_pool);
                }
            }
        })
    });

    info!("Database connected successfully");

    Ok(DatabaseInit {
        pool,
        pools,
        metrics_task,
    })
}

async fn connect_database_pool(
    database_url: &str,
    role: &'static str,
    config: &Config,
    statement_timeout_ms: u128,
    pg_client_min_messages: &'static str,
    read_only: bool,
) -> Result<PgPool> {
    let masked_url = mask_database_url(database_url);
    info!(database_role = role, "Connecting to database: {masked_url}");

    let pool: PgPool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .min_connections(config.database.min_connections)
        .acquire_timeout(Duration::from_secs(config.database.connect_timeout_seconds))
        .idle_timeout(Duration::from_secs(config.database.idle_timeout_seconds))
        .max_lifetime(Duration::from_secs(config.database.max_lifetime_seconds))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                apply_session_settings(
                    conn,
                    statement_timeout_ms,
                    pg_client_min_messages,
                    read_only,
                )
                .await?;
                Ok(())
            })
        })
        .after_release(move |conn, _meta| {
            Box::pin(async move {
                apply_session_settings(
                    conn,
                    statement_timeout_ms,
                    pg_client_min_messages,
                    read_only,
                )
                .await?;
                Ok(true)
            })
        })
        .connect(database_url)
        .await
        .map_err(|e| {
            error!(database_role = role, "Failed to connect to database: {}", e);
            anyhow::anyhow!("{role} database connection failed: {e}")
        })?;
    Ok(pool)
}

fn record_database_pool_metrics(pools: &DatabasePools, max_connections_per_pool: u32) {
    let primary = pool_connection_counts(pools.primary());
    let read = if pools.has_dedicated_read_pool() {
        pool_connection_counts(pools.read())
    } else {
        PoolConnectionCounts::default()
    };

    crate::metrics::database::DB_CONNECTIONS_ACTIVE
        .set(i64::from(primary.active.saturating_add(read.active)));
    crate::metrics::database::DB_CONNECTIONS_IDLE
        .set(i64::from(primary.idle.saturating_add(read.idle)));
    record_pool_utilization("primary", pools.primary(), max_connections_per_pool);
}

#[derive(Debug, Clone, Copy, Default)]
struct PoolConnectionCounts {
    active: u32,
    idle: u32,
}

fn pool_connection_counts(pool: &PgPool) -> PoolConnectionCounts {
    let size = pool.size();
    let idle = u32::try_from(pool.num_idle()).unwrap_or(u32::MAX);
    let active = size.saturating_sub(idle);
    PoolConnectionCounts { active, idle }
}

fn record_pool_utilization(role: &'static str, pool: &PgPool, max_connections_per_pool: u32) {
    let active = pool_connection_counts(pool).active;
    if max_connections_per_pool > 0 {
        crate::metrics::database::DB_POOL_UTILIZATION
            .with_label_values(&[role])
            .set(f64::from(active) / f64::from(max_connections_per_pool));
    }
}

async fn apply_session_settings(
    conn: &mut sqlx::PgConnection,
    statement_timeout_ms: u128,
    client_min_messages: &'static str,
    read_only: bool,
) -> std::result::Result<(), sqlx::Error> {
    conn.execute(trusted_dynamic_sql(format!(
        "SET statement_timeout = {statement_timeout_ms}"
    )))
    .await?;
    conn.execute(trusted_dynamic_sql(format!(
        "SET client_min_messages = '{client_min_messages}'"
    )))
    .await?;
    if read_only {
        conn.execute("SET default_transaction_read_only = on")
            .await?;
    }
    Ok(())
}

const fn pg_client_min_messages_for_level(level: Level) -> &'static str {
    match level {
        Level::TRACE => "notice",
        Level::DEBUG | Level::INFO | Level::WARN => "warning",
        Level::ERROR => "error",
    }
}

/// Acquire a dedicated connection for migration/DDL style work with
/// `statement_timeout` disabled for the lifetime of that session.
///
/// Normal OLTP queries should continue using the main pool with bounded
/// `statement_timeout`. This helper is only for startup/schema management work
/// that can legitimately exceed the request-path timeout budget.
pub async fn acquire_unbounded_ddl_connection(pool: &PgPool) -> Result<PoolConnection<Postgres>> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to acquire DB connection for DDL: {e}"))?;

    conn.execute("SET statement_timeout = 0")
        .await
        .map_err(|e| anyhow::anyhow!("Failed to disable statement_timeout for DDL: {e}"))?;

    Ok(conn)
}

/// Mask credentials in a database URL for safe logging.
/// Turns `postgres://user:pass@host:5432/db` into `postgres://***:***@host:5432/db`
///
/// This function uses multiple strategies to ensure credentials are never leaked:
/// 1. Standard URL parsing and masking when possible
/// 2. Manual fallback for malformed URLs
/// 3. Safe placeholders for completely invalid URLs
fn mask_database_url(url: &str) -> String {
    synctv_common::redaction::mask_url_credentials(url, "<url-missing-scheme>", "<invalid-url>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoggingConfig;
    use crate::test_helpers::{TestOptionExt, TestResultExt};
    use tokio::time::timeout;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn pg_client_min_messages_matches_application_log_level_policy() {
        assert_eq!(pg_client_min_messages_for_level(Level::TRACE), "notice");
        assert_eq!(pg_client_min_messages_for_level(Level::DEBUG), "warning");
        assert_eq!(pg_client_min_messages_for_level(Level::INFO), "warning");
        assert_eq!(pg_client_min_messages_for_level(Level::WARN), "warning");
        assert_eq!(pg_client_min_messages_for_level(Level::ERROR), "error");
    }

    #[test]
    fn effective_log_level_uses_synctv_config_for_database_policy() {
        let config = crate::Config {
            logging: LoggingConfig {
                level: "debug".to_string(),
                ..LoggingConfig::default()
            },
            ..crate::Config::default()
        };

        let effective = crate::logging::effective_log_level(&config.logging)
            .checked("effective log level should resolve");

        assert_eq!(effective, Level::DEBUG);
        assert_eq!(pg_client_min_messages_for_level(effective), "warning");
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn acquire_unbounded_ddl_connection_disables_statement_timeout() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;

        let mut conn = acquire_unbounded_ddl_connection(&pool)
            .await
            .checked("should acquire dedicated ddl connection");

        let timeout = sqlx::query_scalar!(
            r#"SELECT current_setting('statement_timeout') as "statement_timeout!""#
        )
        .fetch_one(&mut *conn)
        .await
        .checked("should query session statement timeout");

        assert_eq!(
            timeout, "0",
            "DDL connection must not inherit the main pool statement_timeout"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn ddl_connection_statement_timeout_is_reset_when_returned_to_pool() {
        let (_postgres, database_url) =
            synctv_core_testing::create_test_database_url_with_label("synctv_test", "ddl-reset")
                .await;

        let config = crate::Config {
            database: crate::config::DatabaseConfig {
                url: database_url,
                max_connections: 5,
                min_connections: 1,
                connect_timeout_seconds: 5,
                idle_timeout_seconds: 600,
                max_lifetime_seconds: 1800,
                ..crate::config::DatabaseConfig::default()
            },
            ..crate::Config::default()
        };
        let pool = init_database_with_cancel(&config, None)
            .await
            .checked("production pool initialization should succeed")
            .pool;

        {
            let mut conn = acquire_unbounded_ddl_connection(&pool)
                .await
                .checked("should acquire dedicated ddl connection");
            sqlx::query_scalar!(r#"SELECT 1 as "one!""#)
                .fetch_one(&mut *conn)
                .await
                .checked("ddl connection should stay usable");
        }

        let mut conn = pool
            .acquire()
            .await
            .checked("should reacquire pooled connection");
        let timeout = sqlx::query_scalar!(
            r#"SELECT current_setting('statement_timeout') as "statement_timeout!""#
        )
        .fetch_one(&mut *conn)
        .await
        .checked("should query reset statement timeout");

        assert_ne!(
            timeout, "0",
            "connections returned to the OLTP pool must not retain unlimited statement_timeout"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn init_database_sets_client_min_messages_from_info_log_level() {
        let (_postgres, database_url) = synctv_core_testing::create_test_database_url_with_label(
            "synctv_test",
            "client-min-messages-info",
        )
        .await;

        let config = crate::Config {
            database: crate::config::DatabaseConfig {
                url: database_url,
                max_connections: 5,
                min_connections: 1,
                connect_timeout_seconds: 5,
                idle_timeout_seconds: 600,
                max_lifetime_seconds: 1800,
                ..crate::config::DatabaseConfig::default()
            },
            logging: LoggingConfig {
                level: "info".to_string(),
                ..LoggingConfig::default()
            },
            ..crate::Config::default()
        };
        let pool = init_database_with_cancel(&config, None)
            .await
            .checked("database init should succeed")
            .pool;

        let mut conn = pool
            .acquire()
            .await
            .checked("should acquire pooled connection");
        let level = sqlx::query_scalar!(
            r#"SELECT current_setting('client_min_messages') as "client_min_messages!""#
        )
        .fetch_one(&mut *conn)
        .await
        .checked("should query session client_min_messages");

        assert_eq!(
            level, "warning",
            "info-level app logging should suppress PostgreSQL NOTICE output at the session level"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn ddl_connection_client_min_messages_is_reset_when_returned_to_pool() {
        let (_postgres, database_url) = synctv_core_testing::create_test_database_url_with_label(
            "synctv_test",
            "client-min-reset",
        )
        .await;

        let config = crate::Config {
            database: crate::config::DatabaseConfig {
                url: database_url,
                max_connections: 5,
                min_connections: 1,
                connect_timeout_seconds: 5,
                idle_timeout_seconds: 600,
                max_lifetime_seconds: 1800,
                ..crate::config::DatabaseConfig::default()
            },
            logging: LoggingConfig {
                level: "info".to_string(),
                ..LoggingConfig::default()
            },
            ..crate::Config::default()
        };
        let pool = init_database_with_cancel(&config, None)
            .await
            .checked("database init should succeed")
            .pool;

        {
            let mut conn = acquire_unbounded_ddl_connection(&pool)
                .await
                .checked("should acquire dedicated ddl connection");
            sqlx::query!("SET client_min_messages = 'notice'")
                .execute(&mut *conn)
                .await
                .checked(
                    "ddl connection should allow session-level override during migration work",
                );
        }

        let mut conn = pool
            .acquire()
            .await
            .checked("should reacquire pooled connection");
        let level = sqlx::query_scalar!(
            r#"SELECT current_setting('client_min_messages') as "client_min_messages!""#
        )
        .fetch_one(&mut *conn)
        .await
        .checked("should query reset client_min_messages");

        assert_eq!(
            level, "warning",
            "connections returned to the OLTP pool must restore the configured PostgreSQL message threshold"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn init_database_without_cancel_does_not_spawn_unmanaged_metrics_task() {
        let (_postgres, database_url) = synctv_core_testing::create_test_database_url_with_label(
            "synctv_test",
            "init-no-metrics",
        )
        .await;

        let config = crate::Config {
            database: crate::config::DatabaseConfig {
                url: database_url,
                max_connections: 5,
                min_connections: 1,
                connect_timeout_seconds: 5,
                idle_timeout_seconds: 600,
                max_lifetime_seconds: 1800,
                ..crate::config::DatabaseConfig::default()
            },
            ..crate::Config::default()
        };

        let db_init = init_database(&config)
            .await
            .checked("database init should succeed without background task");
        assert!(
            db_init.metrics_task.is_none(),
            "init_database must not spawn an unmanaged metrics task"
        );
        db_init.pool.close().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn init_database_uses_primary_only_even_when_read_url_is_configured() {
        let (_postgres, database_url) = synctv_core_testing::create_test_database_url_with_label(
            "synctv_test",
            "init-primary-only",
        )
        .await;

        let config = crate::Config {
            database: crate::config::DatabaseConfig {
                url: database_url,
                read_url: "postgresql://synctv:wrong@127.0.0.1:1/synctv".to_string(),
                max_connections: 5,
                min_connections: 1,
                connect_timeout_seconds: 1,
                idle_timeout_seconds: 600,
                max_lifetime_seconds: 1800,
                ..crate::config::DatabaseConfig::default()
            },
            ..crate::Config::default()
        };

        let db_init = init_database(&config)
            .await
            .checked("primary-only init should ignore unavailable read pool");
        assert!(!db_init.pools.has_dedicated_read_pool());
        db_init.pool.close().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn init_database_with_read_pool_requires_configured_read_pool() {
        let (_postgres, database_url) = synctv_core_testing::create_test_database_url_with_label(
            "synctv_test",
            "init-with-read",
        )
        .await;

        let config = crate::Config {
            database: crate::config::DatabaseConfig {
                url: database_url,
                read_url: "postgresql://synctv:wrong@127.0.0.1:1/synctv".to_string(),
                max_connections: 5,
                min_connections: 1,
                connect_timeout_seconds: 1,
                idle_timeout_seconds: 600,
                max_lifetime_seconds: 1800,
                ..crate::config::DatabaseConfig::default()
            },
            ..crate::Config::default()
        };

        let error = init_database_with_read_pool_and_cancel(&config, None)
            .await
            .failed("application init should surface unavailable read pool");
        assert!(
            error
                .to_string()
                .contains("read database connection failed"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn init_database_with_cancel_returns_stoppable_metrics_task() {
        let (_postgres, database_url) = synctv_core_testing::create_test_database_url_with_label(
            "synctv_test",
            "init-with-metrics",
        )
        .await;

        let config = crate::Config {
            database: crate::config::DatabaseConfig {
                url: database_url,
                max_connections: 5,
                min_connections: 1,
                connect_timeout_seconds: 5,
                idle_timeout_seconds: 600,
                max_lifetime_seconds: 1800,
                ..crate::config::DatabaseConfig::default()
            },
            ..crate::Config::default()
        };

        let cancel = CancellationToken::new();
        let db_init = init_database_with_cancel(&config, Some(cancel.clone()))
            .await
            .checked("database init with cancellation should succeed");

        let handle = db_init
            .metrics_task
            .checked("cancellable init must return a managed metrics task");

        cancel.cancel();
        timeout(Duration::from_secs(1), handle)
            .await
            .checked("metrics task should stop after cancellation")
            .checked("metrics task should exit cleanly");
        db_init.pool.close().await;
    }
}
