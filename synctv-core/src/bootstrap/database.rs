//! Database initialization

use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Postgres};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{error, info};

use crate::resilience::timeout::DB_QUERY_TIMEOUT;
use crate::Config;

/// Initialize database connection pool
///
/// Note: Migrations should be run separately by the binary crate.
pub async fn init_database(config: &Config) -> Result<PgPool> {
    Ok(init_database_with_cancel(config, None).await?.pool)
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
    let masked_url = mask_database_url(database_url);
    info!("Connecting to database: {}", masked_url);

    let statement_timeout_ms = DB_QUERY_TIMEOUT.as_millis();

    let pool: PgPool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .min_connections(config.database.min_connections)
        .acquire_timeout(Duration::from_secs(config.database.connect_timeout_seconds))
        .idle_timeout(Duration::from_secs(config.database.idle_timeout_seconds))
        .max_lifetime(Duration::from_secs(config.database.max_lifetime_seconds))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                conn.execute(format!("SET statement_timeout = {statement_timeout_ms}").as_str())
                    .await?;
                Ok(())
            })
        })
        .after_release(move |conn, _meta| {
            Box::pin(async move {
                conn.execute(format!("SET statement_timeout = {statement_timeout_ms}").as_str())
                    .await?;
                Ok(true)
            })
        })
        .connect(database_url)
        .await
        .map_err(|e| {
            error!("Failed to connect to database: {}", e);
            anyhow::anyhow!("Database connection failed: {e}")
        })?;

    // Set database pool metrics
    crate::metrics::database::DB_POOL_SIZE_MAX.set(i64::from(config.database.max_connections));

    // Spawn periodic task to update pool usage metrics.
    // Respects the CancellationToken for graceful shutdown.
    let pool_clone = pool.clone();
    let metrics_task = crate::spawn::spawn_monitored("db_pool_metrics", async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(15));
        loop {
            if let Some(ref token) = cancel {
                tokio::select! {
                    () = token.cancelled() => {
                        tracing::debug!("DB pool metrics task cancelled");
                        break;
                    }
                    _ = ticker.tick() => {}
                }
            } else {
                ticker.tick().await;
            }
            let size = i64::from(pool_clone.size());
            let idle = pool_clone.num_idle() as i64;
            crate::metrics::database::DB_CONNECTIONS_ACTIVE.set(size - idle);
            crate::metrics::database::DB_CONNECTIONS_IDLE.set(idle);
            let max = crate::metrics::database::DB_POOL_SIZE_MAX.get();
            if max > 0 {
                crate::metrics::database::DB_POOL_UTILIZATION
                    .with_label_values(&["main"])
                    .set((size - idle) as f64 / max as f64);
            }
        }
    });

    info!("Database connected successfully");

    Ok(DatabaseInit {
        pool,
        metrics_task: Some(metrics_task),
    })
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
            let _ = parsed.set_username("***");
        }
        if parsed.password().is_some() {
            let _ = parsed.set_password(Some("***"));
        }
        parsed.to_string()
    } else {
        // Parsing failed - attempt manual masking to prevent credential leakage
        // This handles edge cases where URL is malformed but contains credentials
        if let Some(at_pos) = url.rfind('@') {
            if let Some(scheme_end) = url.find("://") {
                // Found scheme and @ symbol, reconstruct safely masked URL
                let scheme = &url[..scheme_end + 3];
                let host_part = &url[at_pos..];
                return format!("{scheme}***:***{host_part}");
            }
        }
        // Completely unparseable URL - return safe placeholder
        "<invalid-url>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgRow;
    use sqlx::Row;

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn acquire_unbounded_ddl_connection_disables_statement_timeout() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;

        let mut conn = acquire_unbounded_ddl_connection(&pool)
            .await
            .expect("should acquire dedicated ddl connection");

        let row: PgRow = sqlx::query("SHOW statement_timeout")
            .fetch_one(&mut *conn)
            .await
            .expect("should query session statement timeout");
        let timeout: String = row
            .try_get(0)
            .expect("SHOW statement_timeout should return a string");

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
            },
            ..crate::Config::default()
        };
        let pool = init_database_with_cancel(&config, None)
            .await
            .expect("production pool initialization should succeed")
            .pool;

        {
            let mut conn = acquire_unbounded_ddl_connection(&pool)
                .await
                .expect("should acquire dedicated ddl connection");
            sqlx::query("SELECT 1")
                .execute(&mut *conn)
                .await
                .expect("ddl connection should stay usable");
        }

        let mut conn = pool
            .acquire()
            .await
            .expect("should reacquire pooled connection");
        let row: PgRow = sqlx::query("SHOW statement_timeout")
            .fetch_one(&mut *conn)
            .await
            .expect("should query reset statement timeout");
        let timeout: String = row
            .try_get(0)
            .expect("SHOW statement_timeout should return a string");

        assert_ne!(
            timeout, "0",
            "connections returned to the OLTP pool must not retain unlimited statement_timeout"
        );
    }
}
