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
use testcontainers::core::wait::LogWaitStrategy;
use testcontainers::core::ImageExt;
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;
use tokio::sync::{Semaphore, SemaphorePermit};

/// Default `PostgreSQL` version for test containers
const POSTGRES_VERSION: &str = "16-alpine";
/// Default Redis version for test containers
const REDIS_VERSION: &str = "7-alpine";
const DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 300;
const MIN_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 30;
const DOCKER_STARTUP_TIMEOUT_ENV: &str = "SYNCTV_TEST_DOCKER_STARTUP_TIMEOUT_SECS";
const DEFAULT_DOCKER_STARTUP_PARALLELISM: usize = 4;
const MIN_DOCKER_STARTUP_PARALLELISM: usize = 1;
const DOCKER_STARTUP_PARALLELISM_ENV: &str = "SYNCTV_TEST_DOCKER_STARTUP_PARALLELISM";
const DEFAULT_POSTGRES_ACTIVE_PARALLELISM: usize = 4;
const MIN_POSTGRES_ACTIVE_PARALLELISM: usize = 1;
const POSTGRES_ACTIVE_PARALLELISM_ENV: &str = "SYNCTV_TEST_POSTGRES_ACTIVE_PARALLELISM";
const DEFAULT_REDIS_ACTIVE_PARALLELISM: usize = 4;
const MIN_REDIS_ACTIVE_PARALLELISM: usize = 1;
const REDIS_ACTIVE_PARALLELISM_ENV: &str = "SYNCTV_TEST_REDIS_ACTIVE_PARALLELISM";
static POSTGRES_START_SERIALIZER: std::sync::LazyLock<Semaphore> =
    std::sync::LazyLock::new(|| Semaphore::new(docker_startup_parallelism()));
static REDIS_START_SERIALIZER: std::sync::LazyLock<Semaphore> =
    std::sync::LazyLock::new(|| Semaphore::new(docker_startup_parallelism()));
static POSTGRES_ACTIVE_SERIALIZER: std::sync::LazyLock<Semaphore> =
    std::sync::LazyLock::new(|| Semaphore::new(postgres_active_parallelism()));
static REDIS_ACTIVE_SERIALIZER: std::sync::LazyLock<Semaphore> =
    std::sync::LazyLock::new(|| Semaphore::new(redis_active_parallelism()));
const TEST_CONTAINER_OWNER_LABEL: &str = "synctv.test.owner_pid";

struct ProcessLock(File);
struct DockerSlotGuard {
    _local_permit: SemaphorePermit<'static>,
    _process_lock: ProcessLock,
}

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
        .map_or_else(
            || Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS),
            Duration::from_secs,
        )
}

fn docker_startup_parallelism() -> usize {
    std::env::var(DOCKER_STARTUP_PARALLELISM_ENV)
        .ok()
        .as_deref()
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(DEFAULT_DOCKER_STARTUP_PARALLELISM, |slots| {
            slots.max(MIN_DOCKER_STARTUP_PARALLELISM)
        })
}

fn postgres_active_parallelism() -> usize {
    std::env::var(POSTGRES_ACTIVE_PARALLELISM_ENV)
        .ok()
        .as_deref()
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(DEFAULT_POSTGRES_ACTIVE_PARALLELISM, |slots| {
            slots.max(MIN_POSTGRES_ACTIVE_PARALLELISM)
        })
}

fn redis_active_parallelism() -> usize {
    std::env::var(REDIS_ACTIVE_PARALLELISM_ENV)
        .ok()
        .as_deref()
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(DEFAULT_REDIS_ACTIVE_PARALLELISM, |slots| {
            slots.max(MIN_REDIS_ACTIVE_PARALLELISM)
        })
}

fn current_process_id() -> u32 {
    std::process::id()
}

fn process_is_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn cleanup_orphaned_testcontainers(prefix: &str) {
    let output = Command::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("name=^{prefix}"),
            "--filter",
            "label=org.testcontainers.managed-by=testcontainers",
        ])
        .output();

    let Ok(output) = output else {
        return;
    };
    if !output.status.success() {
        return;
    }

    let ids = String::from_utf8_lossy(&output.stdout);
    for container_id in ids.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let inspect = Command::new("docker")
            .args([
                "inspect",
                container_id,
                "--format",
                &format!("{{{{index .Config.Labels \"{TEST_CONTAINER_OWNER_LABEL}\"}}}}"),
            ])
            .output();

        let Ok(inspect) = inspect else {
            continue;
        };
        if !inspect.status.success() {
            continue;
        }

        let owner_pid = String::from_utf8_lossy(&inspect.stdout).trim().to_string();
        if owner_pid.is_empty() || process_is_alive(&owner_pid) {
            continue;
        }

        let _ = Command::new("docker")
            .args(["rm", "-f", container_id])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

async fn acquire_docker_start_slot(
    serializer: &'static std::sync::LazyLock<Semaphore>,
    slots: usize,
    prefix: &'static str,
) -> DockerSlotGuard {
    let local_permit = serializer
        .acquire()
        .await
        .expect("docker startup guard should not be closed");

    let process_lock = tokio::task::spawn_blocking(move || loop {
        for slot in 0..slots {
            let slot_name = format!("{prefix}-slot-{slot}");
            if let Some(lock) = ProcessLock::try_acquire(&slot_name) {
                return lock;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    })
    .await
    .expect("docker process slot task should not panic");

    DockerSlotGuard {
        _local_permit: local_permit,
        _process_lock: process_lock,
    }
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
        .map_or_else(
            || "unknown-test".to_string(),
            |value| sanitize_container_name(&value),
        )
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
    let owner_pid = current_process_id().to_string();
    Postgres::default()
        .with_db_name(db_name)
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .with_container_name(container_name.to_string())
        .with_label(TEST_CONTAINER_OWNER_LABEL, owner_pid)
        .with_ready_conditions(postgres_ready_conditions())
}

fn named_redis_request(container_name: &str) -> testcontainers::ContainerRequest<Redis> {
    Redis::default()
        .with_tag(REDIS_VERSION)
        .with_container_name(container_name.to_string())
        .with_label(TEST_CONTAINER_OWNER_LABEL, current_process_id().to_string())
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
    let mut last_error = String::from("redis readiness probe has not run yet");
    while std::time::Instant::now() < deadline {
        let manager_ready = match redis::aio::ConnectionManager::new(client.clone()).await {
            Ok(mut conn) => match redis::cmd("PING").query_async::<String>(&mut conn).await {
                Ok(_) => true,
                Err(err) => {
                    last_error = format!("connection manager ping failed: {err}");
                    false
                }
            },
            Err(err) => {
                last_error = format!("connection manager init failed: {err}");
                false
            }
        };

        let multiplexed_ready = match client.get_multiplexed_async_connection().await {
            Ok(mut conn) => {
                let ping_result: redis::RedisResult<String> =
                    redis::cmd("PING").query_async(&mut conn).await;
                let set_result: redis::RedisResult<()> =
                    redis::AsyncCommands::set_ex(&mut conn, "synctv:test:ping", "pong", 5).await;
                let get_result: redis::RedisResult<String> =
                    redis::AsyncCommands::get(&mut conn, "synctv:test:ping").await;
                match (ping_result, set_result, get_result) {
                    (Ok(_), Ok(()), Ok(value)) if value == "pong" => true,
                    (ping_result, set_result, get_result) => {
                        last_error = format!(
                            "multiplexed probe failed: ping={ping_result:?} set={set_result:?} get={get_result:?}"
                        );
                        false
                    }
                }
            }
            Err(err) => {
                last_error = format!("multiplexed init failed: {err}");
                false
            }
        };

        if manager_ready && multiplexed_ready {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!(
        "Redis container did not become ready within {:?}: {}",
        docker_startup_timeout(),
        last_error
    );
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
    cleaned_up: bool,
    _slot_guard: Option<DockerSlotGuard>,
}

impl ManagedPostgres {
    fn new(inner: ContainerAsync<Postgres>, name: String, slot_guard: DockerSlotGuard) -> Self {
        Self {
            inner: Some(inner),
            name,
            cleaned_up: false,
            _slot_guard: Some(slot_guard),
        }
    }

    async fn cleanup(&mut self) {
        if let Some(container) = self.inner.take() {
            let _ = container.rm().await;
        }
        self.cleaned_up = true;
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
        if !self.cleaned_up {
            force_remove_container(&self.name);
        }
    }
}

struct ManagedRedis {
    inner: Option<ContainerAsync<Redis>>,
    name: String,
    cleaned_up: bool,
    _slot_guard: Option<DockerSlotGuard>,
}

impl ManagedRedis {
    fn new(inner: ContainerAsync<Redis>, name: String, slot_guard: DockerSlotGuard) -> Self {
        Self {
            inner: Some(inner),
            name,
            cleaned_up: false,
            _slot_guard: Some(slot_guard),
        }
    }

    async fn cleanup(&mut self) {
        if let Some(container) = self.inner.take() {
            let _ = container.rm().await;
        }
        self.cleaned_up = true;
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
        if !self.cleaned_up {
            force_remove_container(&self.name);
        }
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
        let postgres_container_slot = acquire_docker_start_slot(
            &POSTGRES_ACTIVE_SERIALIZER,
            postgres_active_parallelism(),
            "postgres-active",
        )
        .await;
        let redis_container_slot = acquire_docker_start_slot(
            &REDIS_ACTIVE_SERIALIZER,
            redis_active_parallelism(),
            "redis-active",
        )
        .await;
        cleanup_orphaned_testcontainers("synctv-core-pg-");
        cleanup_orphaned_testcontainers("synctv-core-redis-");
        let (pg_container, redis_container) = {
            let _postgres_start_slot = acquire_docker_start_slot(
                &POSTGRES_START_SERIALIZER,
                docker_startup_parallelism(),
                "postgres-start",
            )
            .await;
            let _redis_start_slot = acquire_docker_start_slot(
                &REDIS_START_SERIALIZER,
                docker_startup_parallelism(),
                "redis-start",
            )
            .await;
            tokio::join!(
                tokio::time::timeout(
                    docker_startup_timeout(),
                    named_postgres_request("synctv_test", &postgres_name).start()
                ),
                tokio::time::timeout(
                    docker_startup_timeout(),
                    named_redis_request(&redis_name).start()
                ),
            )
        };

        let pg_container = ManagedPostgres::new(
            pg_container
                .expect("Docker container startup timed out (is Docker running?)")
                .expect("Failed to start Postgres container"),
            postgres_name,
            postgres_container_slot,
        );
        let redis_container = ManagedRedis::new(
            redis_container
                .expect("Docker container startup timed out (is Docker running?)")
                .expect("Failed to start Redis container"),
            redis_name,
            redis_container_slot,
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

    pub async fn cleanup(mut self) {
        self.pool.close().await;
        self.postgres.cleanup().await;
        self.redis.cleanup().await;
    }

    /// Start only Postgres (no Redis). Useful for DB-only tests.
    pub async fn postgres_only() -> TestPostgres {
        let postgres_name = postgres_container_name("postgres-only");
        let postgres_container_slot = acquire_docker_start_slot(
            &POSTGRES_ACTIVE_SERIALIZER,
            postgres_active_parallelism(),
            "postgres-active",
        )
        .await;
        cleanup_orphaned_testcontainers("synctv-core-pg-");
        let pg_container = {
            let _postgres_start_slot = acquire_docker_start_slot(
                &POSTGRES_START_SERIALIZER,
                docker_startup_parallelism(),
                "postgres-start",
            )
            .await;
            tokio::time::timeout(
                docker_startup_timeout(),
                named_postgres_request("synctv_test", &postgres_name).start(),
            )
            .await
            .expect("Docker container startup timed out (is Docker running?)")
            .expect("Failed to start Postgres container")
        };
        let pg_container =
            ManagedPostgres::new(pg_container, postgres_name, postgres_container_slot);
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
        let redis_container_slot = acquire_docker_start_slot(
            &REDIS_ACTIVE_SERIALIZER,
            redis_active_parallelism(),
            "redis-active",
        )
        .await;
        cleanup_orphaned_testcontainers("synctv-core-redis-");
        let redis_container = {
            let _redis_start_slot = acquire_docker_start_slot(
                &REDIS_START_SERIALIZER,
                docker_startup_parallelism(),
                "redis-start",
            )
            .await;
            tokio::time::timeout(
                docker_startup_timeout(),
                named_redis_request(&redis_name).start(),
            )
            .await
            .expect("Docker container startup timed out (is Docker running?)")
            .expect("Failed to start Redis container")
        };
        let redis_container = ManagedRedis::new(redis_container, redis_name, redis_container_slot);
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

impl TestPostgres {
    pub async fn cleanup(mut self) {
        self.pool.close().await;
        self.postgres.cleanup().await;
    }
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

    pub async fn cleanup(mut self) {
        self.redis.cleanup().await;
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

    #[tokio::test]
    async fn explicit_cleanup_marks_managed_containers_as_cleaned_up() {
        let mut postgres = ManagedPostgres {
            inner: None,
            name: "synctv-core-pg-test".to_string(),
            cleaned_up: false,
            _slot_guard: None,
        };
        postgres.cleanup().await;
        assert!(postgres.cleaned_up);

        let mut redis = ManagedRedis {
            inner: None,
            name: "synctv-core-redis-test".to_string(),
            cleaned_up: false,
            _slot_guard: None,
        };
        redis.cleanup().await;
        assert!(redis.cleaned_up);
    }

    #[test]
    fn docker_startup_parallelism_defaults_to_workspace_throughput() {
        assert_eq!(
            docker_startup_parallelism(),
            DEFAULT_DOCKER_STARTUP_PARALLELISM
        );
        assert_eq!(DEFAULT_DOCKER_STARTUP_PARALLELISM, 4);
    }

    #[test]
    fn docker_startup_timeout_defaults_to_extended_budget() {
        assert_eq!(
            docker_startup_timeout(),
            Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS)
        );
        assert_eq!(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS, 300);
    }

    #[test]
    fn postgres_active_parallelism_defaults_to_conservative_live_limit() {
        assert_eq!(
            postgres_active_parallelism(),
            DEFAULT_POSTGRES_ACTIVE_PARALLELISM
        );
        assert_eq!(DEFAULT_POSTGRES_ACTIVE_PARALLELISM, 4);
    }

    #[test]
    fn redis_active_parallelism_defaults_to_conservative_live_limit() {
        assert_eq!(redis_active_parallelism(), DEFAULT_REDIS_ACTIVE_PARALLELISM);
        assert_eq!(DEFAULT_REDIS_ACTIVE_PARALLELISM, 4);
    }
}
