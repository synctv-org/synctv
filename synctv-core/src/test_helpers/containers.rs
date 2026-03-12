//! Testcontainers-based infrastructure for integration tests.
//!
//! Provides `TestInfra` which automatically starts Postgres and Redis containers,
//! runs migrations, and provides ready-to-use connections.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::Connection as _;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::core::wait::LogWaitStrategy;
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;
use tokio::sync::Semaphore;

/// Default `PostgreSQL` version for test containers
const POSTGRES_VERSION: &str = "16-alpine";
/// Default Redis version for test containers
const REDIS_VERSION: &str = "7-alpine";
const DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 120;
const MIN_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 30;
const DOCKER_STARTUP_TIMEOUT_ENV: &str = "SYNCTV_TEST_DOCKER_STARTUP_TIMEOUT_SECS";
const DEFAULT_DOCKER_STARTUP_PARALLELISM: usize = 4;
const MIN_DOCKER_STARTUP_PARALLELISM: usize = 1;
const DOCKER_STARTUP_PARALLELISM_ENV: &str = "SYNCTV_TEST_DOCKER_STARTUP_PARALLELISM";
static POSTGRES_START_SERIALIZER: std::sync::LazyLock<Semaphore> =
    std::sync::LazyLock::new(|| Semaphore::new(docker_startup_parallelism()));
static REDIS_START_SERIALIZER: std::sync::LazyLock<Semaphore> =
    std::sync::LazyLock::new(|| Semaphore::new(docker_startup_parallelism()));

struct ProcessLock(File);

impl ProcessLock {
    fn try_acquire(name: &str) -> Option<Self> {
        let mut path = PathBuf::from("/tmp");
        path.push(format!("synctv-{name}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("failed to open lock file {}: {e}", path.display()));
        match file.try_lock() {
            Ok(()) => Some(Self(file)),
            Err(_) => None,
        }
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        self.0
            .unlock()
            .expect("failed to release process lock for docker test startup");
    }
}

fn docker_startup_timeout() -> Duration {
    std::env::var(DOCKER_STARTUP_TIMEOUT_ENV)
        .ok()
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| secs.max(MIN_DOCKER_STARTUP_TIMEOUT_SECS)).map_or_else(|| Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS), Duration::from_secs)
}

fn docker_startup_parallelism() -> usize {
    std::env::var(DOCKER_STARTUP_PARALLELISM_ENV)
        .ok()
        .as_deref()
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(DEFAULT_DOCKER_STARTUP_PARALLELISM, |slots| slots.max(MIN_DOCKER_STARTUP_PARALLELISM))
}

async fn acquire_docker_start_slot(
    serializer: &'static std::sync::LazyLock<Semaphore>,
    prefix: &'static str,
) -> ProcessLock {
    let slots = docker_startup_parallelism();
    let _local_permit = serializer
        .acquire()
        .await
        .expect("docker startup guard should not be closed");

    tokio::task::spawn_blocking(move || loop {
        for slot in 0..slots {
            let slot_name = format!("{prefix}-slot-{slot}");
            if let Some(lock) = ProcessLock::try_acquire(&slot_name) {
                return lock;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    })
    .await
    .expect("docker process slot task should not panic")
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
        "test".to_string()
    } else {
        name
    }
}

fn current_test_label() -> String {
    std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::thread::current().name().map(str::to_owned)).map_or_else(|| "unknown-test".to_string(), |value| sanitize_container_name(&value))
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

fn postgres_ready_conditions() -> Vec<WaitFor> {
    vec![WaitFor::log(
        LogWaitStrategy::stdout_or_stderr("database system is ready to accept connections")
            .with_times(2),
    )]
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
        .with_ready_conditions(postgres_ready_conditions())
}

fn force_remove_container(name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "-f", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

async fn wait_for_redis_ready(client: &redis::Client) {
    let deadline = std::time::Instant::now() + docker_startup_timeout();
    while std::time::Instant::now() < deadline {
        if let Ok(mut conn) = client.get_multiplexed_async_connection().await {
            let ping_result: redis::RedisResult<String> =
                redis::cmd("PING").query_async(&mut conn).await;
            let set_result: redis::RedisResult<()> =
                redis::AsyncCommands::set_ex(&mut conn, "synctv:test:ping", "pong", 5).await;
            let get_result: redis::RedisResult<String> =
                redis::AsyncCommands::get(&mut conn, "synctv:test:ping").await;
            if ping_result.is_ok() && set_result.is_ok() && get_result.as_deref() == Ok("pong") {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("Redis container did not become ready in time");
}

async fn connect_postgres_pool(
    host: &str,
    port: u16,
    db_name: &str,
    max_connections: u32,
    acquire_timeout: Duration,
) -> PgPool {
    let connect_options = PgConnectOptions::new()
        .host(host)
        .port(port)
        .username("synctv")
        .password("synctv_test")
        .database(db_name)
        .ssl_mode(PgSslMode::Disable);
    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut last_error = None;

    while std::time::Instant::now() < deadline {
        match sqlx::postgres::PgConnection::connect_with(&connect_options).await {
            Ok(mut conn) => {
                sqlx::query_scalar::<_, i32>("SELECT 1")
                    .fetch_one(&mut conn)
                    .await
                    .expect("PostgreSQL readiness probe should succeed once connected");
                drop(conn);

                return PgPoolOptions::new()
                    .acquire_timeout(acquire_timeout)
                    .max_connections(max_connections)
                    .connect_with(connect_options.clone())
                    .await
                    .expect("PostgreSQL pool creation should succeed after readiness probe");
            }
            Err(err) => {
                last_error = Some(err);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }

    panic!(
        "Failed to connect to Postgres container within {:?}: {}",
        docker_startup_timeout(),
        last_error
            .map(|err| err.to_string())
            .unwrap_or_else(|| "connection attempts did not yield an error".to_string())
    );
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
        let postgres_name = postgres_container_name("infra");
        let redis_name = redis_container_name("infra");
        let (pg_container, redis_container) = {
            let _postgres_start_slot =
                acquire_docker_start_slot(&POSTGRES_START_SERIALIZER, "postgres-start").await;
            let _redis_start_slot =
                acquire_docker_start_slot(&REDIS_START_SERIALIZER, "redis-start").await;
            tokio::join!(
                named_postgres_request("synctv_test", &postgres_name).start(),
                Redis::default()
                    .with_tag(REDIS_VERSION)
                    .with_container_name(redis_name.clone())
                    .start(),
            )
        };

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

        let redis_url = format!("redis://{redis_host}:{redis_port}");

        let pool =
            connect_postgres_pool(&pg_host, pg_port, "synctv_test", 5, Duration::from_secs(2))
                .await;

        // Run migrations
        sqlx::migrate!("../migrations")
            .run(&pool)
            .await
            .expect("Failed to run migrations");

        // Create Redis client
        let redis_client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

        wait_for_redis_ready(&redis_client).await;

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
        let postgres_name = postgres_container_name("postgres-only");
        let pg_container = {
            let _postgres_start_slot =
                acquire_docker_start_slot(&POSTGRES_START_SERIALIZER, "postgres-start").await;
            named_postgres_request("synctv_test", &postgres_name)
                .start()
                .await
                .expect("Failed to start Postgres container")
        };
        let pg_container = ManagedPostgres::new(pg_container, postgres_name);
        let (pg_host, pg_port) = pg_container.host_port().await;
        let pool =
            connect_postgres_pool(&pg_host, pg_port, "synctv_test", 5, Duration::from_secs(2))
                .await;

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
        let redis_container = {
            let _redis_start_slot =
                acquire_docker_start_slot(&REDIS_START_SERIALIZER, "redis-start").await;
            Redis::default()
                .with_tag(REDIS_VERSION)
                .with_container_name(redis_name.clone())
                .start()
                .await
                .expect("Failed to start Redis container")
        };
        let redis_container = ManagedRedis::new(redis_container, redis_name);
        let (redis_host, redis_port) = redis_container.host_port().await;

        let redis_url = format!("redis://{redis_host}:{redis_port}");

        let redis_client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");
        wait_for_redis_ready(&redis_client).await;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_postgres_request_waits_for_second_ready_log() {
        let request = named_postgres_request("synctv_test", "synctv-core-pg-test");
        let ready_conditions = request.ready_conditions();

        assert_eq!(
            ready_conditions.len(),
            1,
            "postgres test container should have a single explicit readiness condition"
        );
        assert!(
            matches!(ready_conditions.as_slice(), [WaitFor::Log(_)]),
            "postgres test container should wait for the second ready log instead of racing the init server"
        );
    }
}
