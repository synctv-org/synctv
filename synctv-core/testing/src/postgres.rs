//! `PostgreSQL` test container helpers

use std::process::Command;
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
pub struct TestContainer {
    inner: Option<ContainerAsync<Postgres>>,
    name: String,
}

impl TestContainer {
    fn new(inner: ContainerAsync<Postgres>, name: String) -> Self {
        Self {
            inner: Some(inner),
            name,
        }
    }

    pub async fn cleanup(mut self) {
        if let Some(container) = self.inner.take() {
            let _ = container.rm().await;
        }
    }

    pub fn raw(&self) -> &ContainerAsync<Postgres> {
        self.inner
            .as_ref()
            .expect("postgres test container should still be present")
    }
}

impl std::ops::Deref for TestContainer {
    type Target = ContainerAsync<Postgres>;

    fn deref(&self) -> &Self::Target {
        self.raw()
    }
}

impl Drop for TestContainer {
    fn drop(&mut self) {
        if let Some(container) = self.inner.take() {
            drop(container);
        }
        let _ = Command::new("docker")
            .args(["rm", "-f", self.name.as_str()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

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

fn sanitize_container_name(raw: &str) -> String {
    let mut name: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    name.truncate(48);
    while name.ends_with('-') {
        name.pop();
    }
    if name.is_empty() {
        "postgres-test".to_string()
    } else {
        name
    }
}

fn current_test_label() -> String {
    std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::thread::current().name().map(str::to_owned))
        .map(|value| sanitize_container_name(&value))
        .unwrap_or_else(|| "unknown-test".to_string())
}

fn postgres_container_name(label: &str) -> String {
    format!(
        "synctv-pg-{}-{}-{}",
        current_test_label(),
        sanitize_container_name(label),
        nanoid::nanoid!(6).to_lowercase()
    )
}

fn named_postgres_request(
    db_name: &str,
    container_name: &str,
) -> testcontainers::ContainerRequest<Postgres> {
    Postgres::default()
        .with_db_name(db_name)
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .with_container_name(container_name.to_string())
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
    create_test_pool_with_db_and_label("synctv_test", "pool").await
}

pub async fn create_test_pool_with_options_and_label(
    db_name: &str,
    label: &str,
    max_connections: u32,
    acquire_timeout: Duration,
) -> (TestContainer, PgPool) {
    let container_name = postgres_container_name(label);
    let postgres = tokio::time::timeout(
        docker_startup_timeout(),
        named_postgres_request(db_name, &container_name).start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Postgres container");

    let port = postgres
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");
    let connection_string = format!("postgresql://synctv:synctv_test@127.0.0.1:{port}/{db_name}");

    let pool = {
        let mut retries = 0u32;
        loop {
            match PgPoolOptions::new()
                .acquire_timeout(acquire_timeout)
                .max_connections(max_connections)
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

    (TestContainer::new(postgres, container_name), pool)
}

pub async fn create_test_pool_with_db_and_label(
    db_name: &str,
    label: &str,
) -> (TestContainer, PgPool) {
    create_test_pool_with_options_and_label(
        db_name,
        label,
        20,
        std::time::Duration::from_secs(5),
    )
    .await
}

/// Creates a `PostgreSQL` test pool with a custom database name
pub async fn create_test_pool_with_db(db_name: &str) -> (TestContainer, PgPool) {
    create_test_pool_with_db_and_label(db_name, db_name).await
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
