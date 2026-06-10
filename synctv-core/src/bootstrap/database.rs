//! Database initialization

use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Postgres};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::Level;
use tracing::{debug, error, info};

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
    pub metrics_task: Option<JoinHandle<()>>,
}

/// Initialize database connection pool with an optional `CancellationToken`
/// for graceful shutdown of the background pool metrics task.
///
/// If `cancel` is `None`, the metrics task runs until the process exits.
pub async fn init_database_with_cancel(
    config: &Config,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<DatabaseInit> {
    let database_url = config.database_url();

    // Log only host/port, not credentials
    let masked_url = mask_database_url(&database_url);
    info!("Connecting to database: {}", masked_url);

    let statement_timeout_ms = DB_QUERY_TIMEOUT.as_millis();
    let pg_client_min_messages =
        pg_client_min_messages_for_level(crate::logging::effective_log_level(&config.logging)?);

    let pool: PgPool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .min_connections(config.database.min_connections)
        .acquire_timeout(Duration::from_secs(config.database.connect_timeout_seconds))
        .idle_timeout(Duration::from_secs(config.database.idle_timeout_seconds))
        .max_lifetime(Duration::from_secs(config.database.max_lifetime_seconds))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                apply_session_settings(conn, statement_timeout_ms, pg_client_min_messages).await?;
                Ok(())
            })
        })
        .after_release(move |conn, _meta| {
            Box::pin(async move {
                apply_session_settings(conn, statement_timeout_ms, pg_client_min_messages).await?;
                Ok(true)
            })
        })
        .connect(&database_url)
        .await
        .map_err(|e| {
            error!("Failed to connect to database: {}", e);
            anyhow::anyhow!("Database connection failed: {e}")
        })?;

    // Set database pool metrics
    crate::metrics::database::DB_POOL_SIZE_MAX.set(i64::from(config.database.max_connections));

    // Spawn periodic task to update pool usage metrics only when the caller
    // supplies a cancellation token and can therefore manage the task
    // lifecycle. Starting the task without any shutdown hook would leak it for
    // the rest of the process lifetime while it continues to hold the pool.
    let metrics_task = cancel.map(|token| {
        let pool_clone = pool.clone();
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
                let size = pool_clone.size();
                let idle = u32::try_from(pool_clone.num_idle()).unwrap_or(u32::MAX);
                let active = size.saturating_sub(idle);
                crate::metrics::database::DB_CONNECTIONS_ACTIVE.set(i64::from(active));
                crate::metrics::database::DB_CONNECTIONS_IDLE.set(i64::from(idle));
                let max = u32::try_from(crate::metrics::database::DB_POOL_SIZE_MAX.get())
                    .unwrap_or_default();
                if max > 0 {
                    crate::metrics::database::DB_POOL_UTILIZATION
                        .with_label_values(&["main"])
                        .set(f64::from(active) / f64::from(max));
                }
            }
        })
    });

    info!("Database connected successfully");

    Ok(DatabaseInit { pool, metrics_task })
}

async fn apply_session_settings(
    conn: &mut sqlx::PgConnection,
    statement_timeout_ms: u128,
    client_min_messages: &'static str,
) -> std::result::Result<(), sqlx::Error> {
    conn.execute(trusted_dynamic_sql(format!(
        "SET statement_timeout = {statement_timeout_ms}"
    )))
    .await?;
    conn.execute(trusted_dynamic_sql(format!(
        "SET client_min_messages = '{client_min_messages}'"
    )))
    .await?;
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
    // Early validation: check for URL scheme
    if !url.contains("://") {
        return "<url-missing-scheme>".to_string();
    }

    if let Ok(mut parsed) = url::Url::parse(url) {
        if !parsed.username().is_empty() {
            if let Err(()) = parsed.set_username("***") {
                debug!("Database URL username could not be masked with URL parser");
                return mask_database_url_manually(url);
            }
        }
        if parsed.password().is_some() {
            if let Err(()) = parsed.set_password(Some("***")) {
                debug!("Database URL password could not be masked with URL parser");
                return mask_database_url_manually(url);
            }
        }
        parsed.to_string()
    } else {
        mask_database_url_manually(url)
    }
}

fn mask_database_url_manually(url: &str) -> String {
    if let Some(at_pos) = url.rfind('@') {
        if let Some(scheme_end) = url.find("://") {
            let scheme = &url[..scheme_end + 3];
            let host_part = &url[at_pos..];
            return format!("{scheme}***:***{host_part}");
        }
    }
    "<invalid-url>".to_string()
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
