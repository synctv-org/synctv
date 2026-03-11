//! Testcontainers-based infrastructure for integration tests.
//!
//! Provides `TestInfra` which automatically starts Postgres and Redis containers,
//! runs migrations, and provides ready-to-use connections.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

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
        .map(|secs| secs.max(MIN_DOCKER_STARTUP_TIMEOUT_SECS))
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS))
}

fn docker_startup_parallelism() -> usize {
    std::env::var(DOCKER_STARTUP_PARALLELISM_ENV)
        .ok()
        .as_deref()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|slots| slots.max(MIN_DOCKER_STARTUP_PARALLELISM))
        .unwrap_or(DEFAULT_DOCKER_STARTUP_PARALLELISM)
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
            Postgres::default()
                .with_db_name("synctv_test")
                .with_user("synctv")
                .with_password("synctv_test")
                .with_tag(POSTGRES_VERSION)
                .with_container_name(postgres_name.clone())
                .start()
                .await
                .expect("Failed to start Postgres container")
        };
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
