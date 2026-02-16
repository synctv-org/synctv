//! Database initialization

use anyhow::Result;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::Duration;
use tracing::{error, info};

use crate::Config;

/// Initialize database connection pool
///
/// Note: Migrations should be run separately by the binary crate.
pub async fn init_database(config: &Config) -> Result<PgPool> {
    let database_url = config.database_url();

    // Log only host/port, not credentials
    let masked_url = mask_database_url(database_url);
    info!("Connecting to database: {}", masked_url);

    let pool: PgPool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .min_connections(config.database.min_connections)
        .acquire_timeout(Duration::from_secs(config.database.connect_timeout_seconds))
        .idle_timeout(Duration::from_secs(config.database.idle_timeout_seconds))
        .max_lifetime(Duration::from_secs(config.database.max_lifetime_seconds))
        .connect(database_url)
        .await
        .map_err(|e| {
            error!("Failed to connect to database: {}", e);
            anyhow::anyhow!("Database connection failed: {e}")
        })?;

    // Set database pool metrics
    crate::metrics::database::DB_POOL_SIZE_MAX.set(config.database.max_connections as i64);

    // Spawn periodic task to update pool usage metrics
    let pool_clone = pool.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(15));
        loop {
            ticker.tick().await;
            let size = pool_clone.size() as i64;
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

    Ok(pool)
}

/// Mask credentials in a database URL for safe logging.
/// Turns `postgres://user:pass@host:5432/db` into `postgres://***:***@host:5432/db`
fn mask_database_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            if !parsed.username().is_empty() {
                let _ = parsed.set_username("***");
            }
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some("***"));
            }
            parsed.to_string()
        }
        Err(_) => "<invalid-url>".to_string(),
    }
}
