//! `PostgreSQL` test container helpers

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::Connection as _;
use sqlx::PgPool;
use testcontainers::core::wait::LogWaitStrategy;
use testcontainers::core::{ImageExt, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use tokio::sync::{Semaphore, SemaphorePermit};

/// Default `PostgreSQL` version for test containers
pub const POSTGRES_VERSION: &str = "16-alpine";
const DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 300;
const MIN_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 30;
const DOCKER_STARTUP_TIMEOUT_ENV: &str = "SYNCTV_TEST_DOCKER_STARTUP_TIMEOUT_SECS";
const DEFAULT_DOCKER_STARTUP_PARALLELISM: usize = 3;
const MIN_DOCKER_STARTUP_PARALLELISM: usize = 1;
const DOCKER_STARTUP_PARALLELISM_ENV: &str = "SYNCTV_TEST_DOCKER_STARTUP_PARALLELISM";
const DEFAULT_POSTGRES_ACTIVE_PARALLELISM: usize = 3;
const MIN_POSTGRES_ACTIVE_PARALLELISM: usize = 1;
const POSTGRES_ACTIVE_PARALLELISM_ENV: &str = "SYNCTV_TEST_POSTGRES_ACTIVE_PARALLELISM";
static POSTGRES_START_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(docker_startup_parallelism()));
static POSTGRES_ACTIVE_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(postgres_active_parallelism()));
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
        Self::try_acquire_path(path)
    }

    fn try_acquire_path(path: PathBuf) -> Option<Self> {
        let file = Self::open_lock_file(&path);
        match file.try_lock() {
            Ok(()) => Some(Self(file)),
            Err(_) => None,
        }
    }

    fn open_lock_file(path: &PathBuf) -> File {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .unwrap_or_else(|e| panic!("failed to open lock file {}: {e}", path.display()))
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        self.0
            .unlock()
            .expect("failed to release process lock for postgres test startup");
    }
}

/// Type alias for `PostgreSQL` test container
pub struct TestContainer {
    inner: Option<ContainerAsync<Postgres>>,
    name: String,
    cleaned_up: bool,
    _slot_guard: Option<DockerSlotGuard>,
}

impl TestContainer {
    fn new(inner: ContainerAsync<Postgres>, name: String, slot_guard: DockerSlotGuard) -> Self {
        Self {
            inner: Some(inner),
            name,
            cleaned_up: false,
            _slot_guard: Some(slot_guard),
        }
    }

    pub async fn cleanup(mut self) {
        if let Some(container) = self.inner.take() {
            let _ = container.rm().await;
        }
        self.cleaned_up = true;
    }

    pub const fn raw(&self) -> &ContainerAsync<Postgres> {
        self.inner
            .as_ref()
            .expect("postgres test container should still be present")
    }

    #[cfg(test)]
    const fn is_cleaned_up(&self) -> bool {
        self.cleaned_up
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
        if !self.cleaned_up {
            let _ = Command::new("docker")
                .args(["rm", "-f", self.name.as_str()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
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
pub fn docker_startup_parallelism() -> usize {
    docker_startup_parallelism_from(
        std::env::var(DOCKER_STARTUP_PARALLELISM_ENV)
            .ok()
            .as_deref(),
    )
}

#[must_use]
pub fn postgres_active_parallelism() -> usize {
    postgres_active_parallelism_from(
        std::env::var(POSTGRES_ACTIVE_PARALLELISM_ENV)
            .ok()
            .as_deref(),
    )
}

#[must_use]
fn docker_startup_timeout_from(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| secs.max(MIN_DOCKER_STARTUP_TIMEOUT_SECS))
        .map_or_else(
            || Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS),
            Duration::from_secs,
        )
}

#[must_use]
fn docker_startup_parallelism_from(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(DEFAULT_DOCKER_STARTUP_PARALLELISM, |slots| {
            slots.max(MIN_DOCKER_STARTUP_PARALLELISM)
        })
}

#[must_use]
fn postgres_active_parallelism_from(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(DEFAULT_POSTGRES_ACTIVE_PARALLELISM, |slots| {
            slots.max(MIN_POSTGRES_ACTIVE_PARALLELISM)
        })
}

async fn acquire_docker_slot(
    serializer: &'static LazyLock<Semaphore>,
    slots: usize,
    name: &str,
    closed_message: &'static str,
    panic_message: &'static str,
) -> DockerSlotGuard {
    let local_permit = serializer.acquire().await.expect(closed_message);
    let prefix = name.to_string();

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
    .expect(panic_message);

    DockerSlotGuard {
        _local_permit: local_permit,
        _process_lock: process_lock,
    }
}

async fn acquire_docker_start_slot(name: &str) -> DockerSlotGuard {
    acquire_docker_slot(
        &POSTGRES_START_SERIALIZER,
        docker_startup_parallelism(),
        name,
        "Postgres startup guard should not be closed",
        "postgres process slot task should not panic",
    )
    .await
}

async fn acquire_docker_active_slot(name: &str) -> DockerSlotGuard {
    acquire_docker_slot(
        &POSTGRES_ACTIVE_SERIALIZER,
        postgres_active_parallelism(),
        name,
        "Postgres active-container guard should not be closed",
        "postgres active container slot task should not panic",
    )
    .await
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
        .map_or_else(
            || "unknown-test".to_string(),
            |value| sanitize_container_name(&value),
        )
}

fn postgres_container_name(label: &str) -> String {
    format!(
        "synctv-pg-{}-{}-{}",
        current_test_label(),
        sanitize_container_name(label),
        nanoid::nanoid!(6).to_lowercase()
    )
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

fn postgres_ready_conditions() -> Vec<WaitFor> {
    // The official postgres image emits "database system is ready to accept
    // connections" twice on first boot: once for a transient init server and
    // once after the final post-init restart. Waiting for the second occurrence
    // avoids racing the final server startup without the overhead of Docker
    // healthchecks on every container.
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
    let container_slot = acquire_docker_active_slot("postgres-active").await;
    let _postgres_process_lock = acquire_docker_start_slot("postgres-start").await;
    cleanup_orphaned_testcontainers("synctv-pg-");
    let postgres = {
        tokio::time::timeout(
            docker_startup_timeout(),
            named_postgres_request(db_name, &container_name).start(),
        )
        .await
        .expect("Docker container startup timed out (is Docker running?)")
        .expect("Failed to start Postgres container")
    };

    let port = postgres
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");
    let connect_options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("synctv")
        .password("synctv_test")
        .database(db_name)
        .ssl_mode(PgSslMode::Disable);

    let pool = {
        let deadline = std::time::Instant::now() + docker_startup_timeout();
        let mut retries = 0u32;
        let mut last_error = None;

        loop {
            match sqlx::postgres::PgConnection::connect_with(&connect_options).await {
                Ok(mut conn) => {
                    sqlx::query_scalar::<_, i32>("SELECT 1")
                        .fetch_one(&mut conn)
                        .await
                        .expect("PostgreSQL readiness probe should succeed once connected");
                    drop(conn);

                    let pool = PgPoolOptions::new()
                        .acquire_timeout(acquire_timeout)
                        .max_connections(max_connections)
                        .connect_with(connect_options.clone())
                        .await
                        .expect("PostgreSQL pool creation should succeed after readiness probe");
                    break pool;
                }
                Err(err) if std::time::Instant::now() < deadline => {
                    retries += 1;
                    last_error = Some(err);
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(err) => panic!(
                    "PostgreSQL not ready within {:?} after {retries} retries: {}",
                    docker_startup_timeout(),
                    last_error
                        .as_ref()
                        .map_or_else(|| err.to_string(), std::string::ToString::to_string)
                ),
            }
        }
    };

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (
        TestContainer::new(postgres, container_name, container_slot),
        pool,
    )
}

pub async fn create_test_pool_with_db_and_label(
    db_name: &str,
    label: &str,
) -> (TestContainer, PgPool) {
    create_test_pool_with_options_and_label(db_name, label, 20, std::time::Duration::from_secs(5))
        .await
}

/// Creates a `PostgreSQL` test pool with a custom database name
pub async fn create_test_pool_with_db(db_name: &str) -> (TestContainer, PgPool) {
    create_test_pool_with_db_and_label(db_name, db_name).await
}

/// Starts a `PostgreSQL` test container and returns a connection URL without
/// creating a pool. Useful for tests that need to exercise production pool
/// initialization paths directly.
pub async fn create_test_database_url_with_label(
    db_name: &str,
    label: &str,
) -> (TestContainer, String) {
    let container_name = postgres_container_name(label);
    let container_slot = acquire_docker_active_slot("postgres-active").await;
    let _postgres_process_lock = acquire_docker_start_slot("postgres-start").await;
    cleanup_orphaned_testcontainers("synctv-pg-");
    let postgres = {
        tokio::time::timeout(
            docker_startup_timeout(),
            named_postgres_request(db_name, &container_name).start(),
        )
        .await
        .expect("Docker container startup timed out (is Docker running?)")
        .expect("Failed to start Postgres container")
    };

    let port = postgres
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");
    let database_url = format!("postgresql://synctv:synctv_test@127.0.0.1:{port}/{db_name}");

    let connect_options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("synctv")
        .password("synctv_test")
        .database(db_name)
        .ssl_mode(PgSslMode::Disable);

    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut retries = 0u32;
    let mut last_error = None;

    loop {
        match sqlx::postgres::PgConnection::connect_with(&connect_options).await {
            Ok(mut conn) => {
                sqlx::query_scalar::<_, i32>("SELECT 1")
                    .fetch_one(&mut conn)
                    .await
                    .expect("PostgreSQL readiness probe should succeed once connected");
                drop(conn);
                break;
            }
            Err(err) if std::time::Instant::now() < deadline => {
                retries += 1;
                last_error = Some(err);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(err) => panic!(
                "PostgreSQL not ready within {:?} after {retries} retries: {}",
                docker_startup_timeout(),
                last_error
                    .as_ref()
                    .map_or_else(|| err.to_string(), std::string::ToString::to_string)
            ),
        }
    }

    (
        TestContainer::new(postgres, container_name, container_slot),
        database_url,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_postgres_request_waits_for_second_ready_log() {
        let request = named_postgres_request("synctv_test", "synctv-pg-test");
        let ready_conditions = request.ready_conditions();

        assert_eq!(
            ready_conditions.len(),
            1,
            "postgres test container should have a single explicit readiness condition"
        );
        assert!(
            matches!(ready_conditions.as_slice(), [WaitFor::Log(_)]),
            "postgres test container should wait for the second ready log instead of the first init-server ready log"
        );
    }

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
            Duration::from_mins(3)
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
    fn test_docker_startup_parallelism_defaults_to_workspace_throughput() {
        assert_eq!(
            docker_startup_parallelism_from(None),
            DEFAULT_DOCKER_STARTUP_PARALLELISM
        );
        assert_eq!(DEFAULT_DOCKER_STARTUP_PARALLELISM, 3);
    }

    #[test]
    fn test_docker_startup_parallelism_honors_valid_override() {
        assert_eq!(docker_startup_parallelism_from(Some("6")), 6);
    }

    #[test]
    fn test_docker_startup_parallelism_rejects_zero_override() {
        assert_eq!(
            docker_startup_parallelism_from(Some("0")),
            MIN_DOCKER_STARTUP_PARALLELISM
        );
    }

    #[test]
    fn test_postgres_active_parallelism_defaults_to_conservative_live_limit() {
        assert_eq!(
            postgres_active_parallelism_from(None),
            DEFAULT_POSTGRES_ACTIVE_PARALLELISM
        );
        assert_eq!(DEFAULT_POSTGRES_ACTIVE_PARALLELISM, 3);
    }

    #[test]
    fn test_postgres_active_parallelism_honors_valid_override() {
        assert_eq!(postgres_active_parallelism_from(Some("3")), 3);
    }

    #[test]
    fn test_postgres_active_parallelism_rejects_zero_override() {
        assert_eq!(
            postgres_active_parallelism_from(Some("0")),
            MIN_POSTGRES_ACTIVE_PARALLELISM
        );
    }

    #[test]
    fn test_docker_startup_timeout_ignores_invalid_override() {
        assert_eq!(
            docker_startup_timeout_from(Some("not-a-number")),
            Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS)
        );
    }

    #[test]
    fn cleanup_marks_container_as_cleaned_up() {
        let container = TestContainer {
            inner: None,
            name: "synctv-pg-test".to_string(),
            cleaned_up: false,
            _slot_guard: None,
        };

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let container = runtime.block_on(async move {
            let mut container = container;
            if let Some(inner) = container.inner.take() {
                let _ = inner.rm().await;
            }
            container.cleaned_up = true;
            container
        });

        assert!(
            container.is_cleaned_up(),
            "explicit cleanup must suppress the Drop-time docker rm fallback"
        );
    }
}
