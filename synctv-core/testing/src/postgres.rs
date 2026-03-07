//! `PostgreSQL` test container helpers

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

/// Default `PostgreSQL` version for test containers
pub const POSTGRES_VERSION: &str = "16-alpine";
const DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 120;
const MIN_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 30;
const DOCKER_STARTUP_TIMEOUT_ENV: &str = "SYNCTV_TEST_DOCKER_STARTUP_TIMEOUT_SECS";

/// Type alias for `PostgreSQL` test container
pub type TestContainer = ContainerAsync<Postgres>;

/// Returns the timeout budget used for Docker-backed integration tests.
///
/// The default is intentionally higher than 30 seconds because workspace-scale
/// `cargo nextest -j20` runs can cold-pull images or contend on Docker daemon
/// resources, making a 30s cap spuriously fail healthy tests.
#[must_use]
pub fn docker_startup_timeout() -> Duration {
    docker_startup_timeout_from(std::env::var(DOCKER_STARTUP_TIMEOUT_ENV).ok().as_deref())
}

#[must_use]
fn docker_startup_timeout_from(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| secs.max(MIN_DOCKER_STARTUP_TIMEOUT_SECS))
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS))
}

/// Creates a `PostgreSQL` test container and connection pool
///
/// This function:
/// 1. Starts a `PostgreSQL` Docker container
/// 2. Creates a connection pool
/// 3. Runs database migrations
///
/// # Returns
///
/// A tuple of (container, pool). The container is kept alive
/// to prevent database connection loss during tests.
///
/// # Example
///
/// ```text
/// use synctv_core_testing::create_test_pool;
///
/// #[tokio::test]
/// async fn my_test() {
///     let (_container, pool) = create_test_pool().await;
///     // Use pool for database operations...
/// }
/// ```
pub async fn create_test_pool() -> (TestContainer, PgPool) {
    let postgres = tokio::time::timeout(
        docker_startup_timeout(),
        Postgres::default()
            .with_db_name("synctv_test")
            .with_user("synctv")
            .with_password("synctv_test")
            .with_tag(POSTGRES_VERSION)
            .start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get port")
    );

    // Retry connection until PG is fully ready (container port may be mapped
    // before the server accepts connections).  Use a short acquire timeout so
    // each attempt fails fast instead of blocking 30s (the default), which
    // prevents 60 retries × 30s = 30-minute hangs under Docker pressure.
    let pool = {
        let mut retries = 0u32;
        loop {
            match PgPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_secs(5))
                .max_connections(20)
                .connect(&connection_string)
                .await
            {
                Ok(p) => break p,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!("PostgreSQL not ready after {retries} retries: {e}"),
            }
        }
    };

    // Run migrations from the parent crate
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

/// Creates a `PostgreSQL` test pool with a custom database name
///
/// # Arguments
///
/// * `db_name` - Custom database name
///
/// # Example
///
/// ```text
/// let (_container, pool) = create_test_pool_with_db("my_test_db").await;
/// ```
pub async fn create_test_pool_with_db(db_name: &str) -> (TestContainer, PgPool) {
    let postgres = tokio::time::timeout(
        docker_startup_timeout(),
        Postgres::default()
            .with_db_name(db_name)
            .with_user("synctv")
            .with_password("synctv_test")
            .with_tag(POSTGRES_VERSION)
            .start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/{}",
        postgres
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get port"),
        db_name
    );

    let pool = {
        let mut retries = 0u32;
        loop {
            match PgPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_secs(5))
                .max_connections(20)
                .connect(&connection_string)
                .await
            {
                Ok(p) => break p,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!("PostgreSQL not ready after {retries} retries: {e}"),
            }
        }
    };

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_docker_startup_timeout_defaults_to_extended_budget() {
        assert_eq!(
            docker_startup_timeout_from(None),
            Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS)
        );
    }

    #[test]
    fn test_docker_startup_timeout_honors_valid_override() {
        assert_eq!(
            docker_startup_timeout_from(Some("180")),
            Duration::from_secs(180)
        );
    }

    #[test]
    fn test_docker_startup_timeout_rejects_too_small_override() {
        assert_eq!(
            docker_startup_timeout_from(Some("5")),
            Duration::from_secs(MIN_DOCKER_STARTUP_TIMEOUT_SECS)
        );
    }

    #[test]
    fn test_docker_startup_timeout_ignores_invalid_override() {
        assert_eq!(
            docker_startup_timeout_from(Some("not-a-number")),
            Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS)
        );
    }
}
