//! Testcontainers-based infrastructure for integration tests.
//!
//! Provides `TestInfra` which automatically starts Postgres and Redis containers,
//! runs migrations, and provides ready-to-use connections.

use std::process::Command;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;
use tokio::sync::Semaphore;

/// Default `PostgreSQL` version for test containers
const POSTGRES_VERSION: &str = "16-alpine";
/// Default Redis version for test containers
const REDIS_VERSION: &str = "7-alpine";
static POSTGRES_START_SERIALIZER: std::sync::LazyLock<Semaphore> =
    std::sync::LazyLock::new(|| Semaphore::new(1));

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
        "test".to_string()
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
        "synctv-core-pg-{}-{}-{}",
        current_test_label(),
        sanitize_container_name(label),
        nanoid::nanoid!(6).to_lowercase()
    )
}

fn redis_container_name(label: &str) -> String {
    format!(
        "synctv-core-redis-{}-{}-{}",
        current_test_label(),
        sanitize_container_name(label),
        nanoid::nanoid!(6).to_lowercase()
    )
}

fn force_remove_container(name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "-f", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

struct ManagedPostgres {
    inner: Option<ContainerAsync<Postgres>>,
    name: String,
}

impl ManagedPostgres {
    fn new(inner: ContainerAsync<Postgres>, name: String) -> Self {
        Self {
            inner: Some(inner),
            name,
        }
    }

    async fn host_port(&self) -> (String, u16) {
        let inner = self
            .inner
            .as_ref()
            .expect("postgres test container should still exist");
        let host = inner.get_host().await.expect("Failed to get Postgres host");
        let port = inner
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get Postgres port");
        (host.to_string(), port)
    }
}

impl Drop for ManagedPostgres {
    fn drop(&mut self) {
        if let Some(container) = self.inner.take() {
            drop(container);
        }
        force_remove_container(&self.name);
    }
}

struct ManagedRedis {
    inner: Option<ContainerAsync<Redis>>,
    name: String,
}

impl ManagedRedis {
    fn new(inner: ContainerAsync<Redis>, name: String) -> Self {
        Self {
            inner: Some(inner),
            name,
        }
    }

    async fn host_port(&self) -> (String, u16) {
        let inner = self
            .inner
            .as_ref()
            .expect("redis test container should still exist");
        let host = inner.get_host().await.expect("Failed to get Redis host");
        let port = inner
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Redis port");
        (host.to_string(), port)
    }
}

impl Drop for ManagedRedis {
    fn drop(&mut self) {
        if let Some(container) = self.inner.take() {
            drop(container);
        }
        force_remove_container(&self.name);
    }
}

/// Test infrastructure that manages Postgres and Redis containers.
///
/// Containers are automatically stopped when this struct is dropped.
///
/// # Example
///
/// ```text
/// let infra = TestInfra::new().await;
/// let pool = &infra.pool;
/// // use pool for database operations...
/// ```
pub struct TestInfra {
    pub pool: PgPool,
    pub redis_client: redis::Client,
    pub redis_url: String,
    #[allow(dead_code)]
    postgres: ManagedPostgres,
    #[allow(dead_code)]
    redis: ManagedRedis,
}

impl TestInfra {
    /// Start Postgres and Redis containers, run migrations, and return connections.
    pub async fn new() -> Self {
        let _postgres_start_permit = POSTGRES_START_SERIALIZER
            .acquire()
            .await
            .expect("Postgres startup guard should not be closed");
        let postgres_name = postgres_container_name("infra");
        let redis_name = redis_container_name("infra");
        let (pg_container, redis_container) = tokio::join!(
            Postgres::default()
                .with_db_name("synctv_test")
                .with_user("synctv")
                .with_password("synctv_test")
                .with_tag(POSTGRES_VERSION)
                .with_container_name(postgres_name.clone())
                .start(),
            Redis::default()
                .with_tag(REDIS_VERSION)
                .with_container_name(redis_name.clone())
                .start(),
        );

        let pg_container = ManagedPostgres::new(
            pg_container.expect("Failed to start Postgres container"),
            postgres_name,
        );
        let redis_container = ManagedRedis::new(
            redis_container.expect("Failed to start Redis container"),
            redis_name,
        );
        let (pg_host, pg_port) = pg_container.host_port().await;
        let (redis_host, redis_port) = redis_container.host_port().await;

        // Build connection URLs
        let database_url =
            format!("postgresql://synctv:synctv_test@{pg_host}:{pg_port}/synctv_test");
        let redis_url = format!("redis://{redis_host}:{redis_port}");

        // Connect to Postgres with retry (container port may be mapped before
        // the server accepts connections)
        let pool = {
            let mut retries = 0u32;
            loop {
                match PgPoolOptions::new()
                    .acquire_timeout(std::time::Duration::from_secs(2))
                    .max_connections(5)
                    .connect(&database_url)
                    .await
                {
                    Ok(p) => break p,
                    Err(_) if retries < 60 => {
                        retries += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    Err(e) => panic!("Failed to connect to Postgres container: {e}"),
                }
            }
        };

        // Run migrations
        sqlx::migrate!("../migrations")
            .run(&pool)
            .await
            .expect("Failed to run migrations");

        // Create Redis client
        let redis_client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

        // Verify Redis connectivity
        let _: () = redis::cmd("PING")
            .query_async(
                &mut redis_client
                    .get_multiplexed_async_connection()
                    .await
                    .expect("Failed to connect to Redis container"),
            )
            .await
            .expect("Redis PING failed");

        Self {
            pool,
            redis_client,
            redis_url,
            postgres: pg_container,
            redis: redis_container,
        }
    }

    /// Create a Redis `ConnectionManager` for use with services that require one.
    pub async fn redis_connection_manager(&self) -> redis::aio::ConnectionManager {
        redis::aio::ConnectionManager::new(self.redis_client.clone())
            .await
            .expect("Failed to create Redis ConnectionManager")
    }

    /// Start only Postgres (no Redis). Useful for DB-only tests.
    pub async fn postgres_only() -> TestPostgres {
        let _postgres_start_permit = POSTGRES_START_SERIALIZER
            .acquire()
            .await
            .expect("Postgres startup guard should not be closed");
        let postgres_name = postgres_container_name("postgres-only");
        let pg_container = Postgres::default()
            .with_db_name("synctv_test")
            .with_user("synctv")
            .with_password("synctv_test")
            .with_tag(POSTGRES_VERSION)
            .with_container_name(postgres_name.clone())
            .start()
            .await
            .expect("Failed to start Postgres container");
        let pg_container = ManagedPostgres::new(pg_container, postgres_name);
        let (pg_host, pg_port) = pg_container.host_port().await;

        let database_url =
            format!("postgresql://synctv:synctv_test@{pg_host}:{pg_port}/synctv_test");

        let pool = {
            let mut retries = 0u32;
            loop {
                match PgPoolOptions::new()
                    .acquire_timeout(std::time::Duration::from_secs(2))
                    .max_connections(5)
                    .connect(&database_url)
                    .await
                {
                    Ok(p) => break p,
                    Err(_) if retries < 60 => {
                        retries += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    Err(e) => panic!("Failed to connect to Postgres container: {e}"),
                }
            }
        };

        sqlx::migrate!("../migrations")
            .run(&pool)
            .await
            .expect("Failed to run migrations");

        TestPostgres {
            pool,
            postgres: pg_container,
        }
    }

    /// Start only Redis (no Postgres). Useful for Redis-only tests.
    pub async fn redis_only() -> TestRedis {
        let redis_name = redis_container_name("redis-only");
        let redis_container = Redis::default()
            .with_tag(REDIS_VERSION)
            .with_container_name(redis_name.clone())
            .start()
            .await
            .expect("Failed to start Redis container");
        let redis_container = ManagedRedis::new(redis_container, redis_name);
        let (redis_host, redis_port) = redis_container.host_port().await;

        let redis_url = format!("redis://{redis_host}:{redis_port}");

        let redis_client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

        TestRedis {
            redis_client,
            redis_url,
            redis: redis_container,
        }
    }
}

/// Postgres-only test infrastructure.
pub struct TestPostgres {
    pub pool: PgPool,
    #[allow(dead_code)]
    postgres: ManagedPostgres,
}

/// Redis-only test infrastructure.
pub struct TestRedis {
    pub redis_client: redis::Client,
    pub redis_url: String,
    #[allow(dead_code)]
    redis: ManagedRedis,
}

impl TestRedis {
    /// Start only Redis (no Postgres). Alias for `TestInfra::redis_only()`.
    pub async fn new() -> Self {
        TestInfra::redis_only().await
    }

    /// Create a Redis `ConnectionManager` for use with services that require one.
    pub async fn connection_manager(&self) -> redis::aio::ConnectionManager {
        redis::aio::ConnectionManager::new(self.redis_client.clone())
            .await
            .expect("Failed to create Redis ConnectionManager")
    }
}
