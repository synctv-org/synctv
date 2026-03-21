//! `PostgreSQL` test container helpers

use std::fs::{File, OpenOptions};
use std::net::IpAddr;
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
            log_cleanup_warning_if_needed(handle_cleanup_result(
                &mut self.cleaned_up,
                &self.name,
                container.rm().await.map_err(|err| err.to_string()),
                "postgres",
                docker_rm_force,
            ));
        } else {
            self.cleaned_up = true;
        }
    }

    pub const fn raw(&self) -> &ContainerAsync<Postgres> {
        self.inner
            .as_ref()
            .expect("postgres test container should still be present")
    }

    pub async fn host(&self) -> String {
        self.host_port(5432).await.0
    }

    pub async fn port_ipv4(&self, internal_port: u16) -> u16 {
        self.host_port(internal_port).await.1
    }

    pub async fn host_port(&self, internal_port: u16) -> (String, u16) {
        let host = self
            .raw()
            .get_host()
            .await
            .expect("Failed to get Postgres host")
            .to_string();
        let ports = self
            .raw()
            .ports()
            .await
            .expect("Failed to inspect Postgres port mappings");
        candidate_endpoints_for_host(
            &host,
            ports.map_to_host_port_ipv4(internal_port),
            ports.map_to_host_port_ipv6(internal_port),
        )
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("Failed to resolve Postgres endpoint for host {host}"))
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
            if let Err(err) = docker_rm_force(&self.name) {
                eprintln!(
                    "warning: failed to force-remove postgres test container {} during Drop: {err}",
                    self.name
                );
            }
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

fn handle_cleanup_result<F>(
    cleaned_up: &mut bool,
    container_name: &str,
    result: Result<(), String>,
    kind: &str,
    fallback_remove: F,
) -> Option<String>
where
    F: FnOnce(&str) -> Result<(), String>,
{
    match result {
        Ok(()) => {
            *cleaned_up = true;
            None
        }
        Err(err) if cleanup_error_indicates_missing_container(&err) => {
            *cleaned_up = true;
            Some(format!(
                "warning: {kind} test container {container_name} was already removed before explicit cleanup completed: {err}"
            ))
        }
        Err(err) => match fallback_remove(container_name) {
            Ok(()) => {
                *cleaned_up = true;
                Some(format!(
                    "warning: explicit removal for {kind} test container {container_name} failed; fallback `docker rm -f` succeeded: {err}"
                ))
            }
            Err(fallback_err) if cleanup_error_indicates_missing_container(&fallback_err) => {
                *cleaned_up = true;
                Some(format!(
                    "warning: explicit removal for {kind} test container {container_name} reported an error, but fallback confirmed it was already gone: {err}; fallback: {fallback_err}"
                ))
            }
            Err(fallback_err) => Some(format!(
                "warning: failed to remove {kind} test container {container_name} during explicit cleanup: {err}; fallback `docker rm -f` also failed: {fallback_err}"
            )),
        }
    }
}

fn log_cleanup_warning_if_needed(warning: Option<String>) {
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
}

fn cleanup_error_indicates_missing_container(err: &str) -> bool {
    let err = err.to_ascii_lowercase();
    err.contains("no such container") || err.contains("not found")
}

fn docker_rm_force(container_ref: &str) -> Result<(), String> {
    docker_rm_force_with_program("docker", container_ref)
}

fn docker_rm_force_with_program(program: &str, container_ref: &str) -> Result<(), String> {
    let args = ["rm", "-f", container_ref];
    let output = Command::new(program).args(args).output().map_err(|err| {
        format!("failed to spawn `{program}` for `{container_ref}` cleanup: {err}")
    })?;

    if output.status.success() {
        return Ok(());
    }

    Err(format_command_failure(program, &args, &output))
}

fn format_command_failure(program: &str, args: &[&str], output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let mut details = Vec::new();
    if !stdout.is_empty() {
        details.push(format!("stdout={stdout}"));
    }
    if !stderr.is_empty() {
        details.push(format!("stderr={stderr}"));
    }
    let details = if details.is_empty() {
        "no command output".to_string()
    } else {
        details.join(" ")
    };

    format!(
        "command `{}` exited with status {}: {details}",
        std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" "),
        output.status
    )
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

        if let Err(err) = docker_rm_force(container_id) {
            eprintln!(
                "warning: failed to remove orphaned postgres test container {container_id}: {err}"
            );
        }
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

fn postgres_connection_url(host: &str, port: u16, db_name: &str) -> String {
    format!(
        "postgresql://synctv:synctv_test@{}:{port}/{db_name}",
        format_socket_host(host)
    )
}

fn format_socket_host(host: &str) -> String {
    if matches!(host_address_family(host), Some(IpAddr::V6(_))) && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

async fn resolve_host_port(
    container: &ContainerAsync<Postgres>,
    internal_port: u16,
    db_name: &str,
) -> (String, u16) {
    let host = container
        .get_host()
        .await
        .expect("Failed to get Postgres host")
        .to_string();
    let ports = container
        .ports()
        .await
        .expect("Failed to inspect Postgres port mappings");
    let endpoints = candidate_endpoints_for_host(
        &host,
        ports.map_to_host_port_ipv4(internal_port),
        ports.map_to_host_port_ipv6(internal_port),
    );

    assert!(
        !endpoints.is_empty(),
        "Failed to resolve Postgres endpoint for host {host}"
    );

    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut retries = 0u32;
    let mut last_error = None;

    while std::time::Instant::now() < deadline {
        for (candidate_host, candidate_port) in &endpoints {
            let connect_options = PgConnectOptions::new()
                .host(candidate_host)
                .port(*candidate_port)
                .username("synctv")
                .password("synctv_test")
                .database(db_name)
                .ssl_mode(PgSslMode::Disable);

            match sqlx::postgres::PgConnection::connect_with(&connect_options).await {
                Ok(mut conn) => {
                    sqlx::query_scalar::<_, i32>("SELECT 1")
                        .fetch_one(&mut conn)
                        .await
                        .expect("PostgreSQL readiness probe should succeed once connected");
                    drop(conn);
                    return (candidate_host.clone(), *candidate_port);
                }
                Err(err) => last_error = Some(err),
            }
        }

        retries += 1;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    panic!(
        "PostgreSQL not ready within {:?} after {retries} retries across endpoints {:?}: {}",
        docker_startup_timeout(),
        endpoints,
        last_error
            .map(|err| err.to_string())
            .unwrap_or_else(|| "connection attempts did not yield an error".to_string())
    );
}

fn host_address_family(host: &str) -> Option<IpAddr> {
    let normalized = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    normalized.parse::<IpAddr>().ok()
}

fn candidate_endpoints_for_host(
    host: &str,
    ipv4_port: Option<u16>,
    ipv6_port: Option<u16>,
) -> Vec<(String, u16)> {
    let mut candidates = Vec::new();

    match host_address_family(host) {
        Some(IpAddr::V4(_)) => {
            if let Some(port) = ipv4_port {
                candidates.push((host.to_string(), port));
            }
            if let Some(port) = ipv6_port.filter(|port| Some(*port) != ipv4_port) {
                candidates.push(("::1".to_string(), port));
            }
        }
        Some(IpAddr::V6(_)) => {
            if let Some(port) = ipv6_port {
                candidates.push((host.to_string(), port));
            }
            if let Some(port) = ipv4_port.filter(|port| Some(*port) != ipv6_port) {
                candidates.push(("127.0.0.1".to_string(), port));
            }
        }
        None => {
            if let Some(port) = ipv6_port.filter(|_| host == "localhost") {
                candidates.push(("::1".to_string(), port));
            }
            if let Some(port) = ipv4_port {
                let ipv4_host = if host == "localhost" {
                    "127.0.0.1".to_string()
                } else {
                    host.to_string()
                };
                candidates.push((ipv4_host, port));
            }
            if let Some(port) =
                ipv6_port.filter(|port| Some(*port) != ipv4_port && host != "localhost")
            {
                candidates.push((host.to_string(), port));
            }
        }
    }

    candidates
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

    let (host, port) = resolve_host_port(&postgres, 5432, db_name).await;
    let connect_options = PgConnectOptions::new()
        .host(&host)
        .port(port)
        .username("synctv")
        .password("synctv_test")
        .database(db_name)
        .ssl_mode(PgSslMode::Disable);

    let pool = PgPoolOptions::new()
        .acquire_timeout(acquire_timeout)
        .max_connections(max_connections)
        .connect_with(connect_options.clone())
        .await
        .expect("PostgreSQL pool creation should succeed after readiness probe");

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

    let (host, port) = resolve_host_port(&postgres, 5432, db_name).await;
    let database_url = postgres_connection_url(&host, port, db_name);

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
    fn cleanup_marks_container_as_cleaned_up_on_success() {
        let mut cleaned_up = false;

        let warning = handle_cleanup_result(
            &mut cleaned_up,
            "synctv-pg-test",
            Ok(()),
            "postgres",
            |_| Ok(()),
        );

        assert!(warning.is_none());
        assert!(cleaned_up);
    }

    #[test]
    fn cleanup_uses_fallback_when_explicit_container_removal_fails() {
        let mut cleaned_up = false;
        let mut fallback_called = false;
        let warning = handle_cleanup_result(
            &mut cleaned_up,
            "synctv-pg-test",
            Err("docker rm failed".to_string()),
            "postgres",
            |_| {
                fallback_called = true;
                Ok(())
            },
        )
        .expect("fallback success should emit a warning");

        assert!(
            warning.contains("fallback `docker rm -f` succeeded"),
            "warning should explain that cleanup fell back to force remove: {warning}"
        );
        assert!(
            fallback_called,
            "explicit cleanup failure must try fallback removal"
        );
        assert!(
            cleaned_up,
            "successful fallback should mark the container as cleaned up"
        );
    }

    #[test]
    fn cleanup_treats_missing_container_as_already_cleaned_up() {
        let mut cleaned_up = false;

        let warning = handle_cleanup_result(
            &mut cleaned_up,
            "synctv-pg-test",
            Err("Error response from daemon: No such container: synctv-pg-test".to_string()),
            "postgres",
            |_| panic!("fallback should not run when the container is already gone"),
        )
        .expect("missing container should still surface a warning");

        assert!(
            warning.contains("already removed"),
            "warning should explain that the container was already gone: {warning}"
        );
        assert!(
            cleaned_up,
            "missing container should still be treated as cleaned up"
        );
    }

    #[test]
    fn cleanup_leaves_container_uncleaned_when_explicit_and_fallback_removal_fail() {
        let mut cleaned_up = false;
        let warning = handle_cleanup_result(
            &mut cleaned_up,
            "synctv-pg-test",
            Err("docker rm failed".to_string()),
            "postgres",
            |_| Err("docker rm -f failed".to_string()),
        )
        .expect("double failure should emit a warning");

        assert!(
            warning.contains("fallback `docker rm -f` also failed"),
            "warning should include the fallback failure: {warning}"
        );
        assert!(
            !cleaned_up,
            "cleanup should leave Drop fallback enabled when both removal attempts fail"
        );
    }

    #[test]
    fn docker_rm_force_reports_command_failure() {
        let err = docker_rm_force_with_program("false", "synctv-pg-test")
            .expect_err("failed command must surface as an error");

        assert!(
            err.contains("command `false rm -f synctv-pg-test` exited with status"),
            "error should include the failing command line: {err}"
        );
    }

    #[test]
    fn docker_rm_force_reports_spawn_failure() {
        let err =
            docker_rm_force_with_program("synctv-command-that-should-not-exist", "synctv-pg-test")
                .expect_err("spawn failure must surface as an error");

        assert!(
            err.contains("failed to spawn `synctv-command-that-should-not-exist`"),
            "error should include the missing program: {err}"
        );
    }

    #[test]
    fn postgres_connection_url_uses_resolved_host() {
        let url = postgres_connection_url("docker.internal", 5432, "synctv_test");

        assert_eq!(
            url,
            "postgresql://synctv:synctv_test@docker.internal:5432/synctv_test"
        );
        assert!(
            !url.contains("@127.0.0.1:"),
            "connection URL must not hardcode localhost"
        );
    }

    #[test]
    fn postgres_connection_url_brackets_ipv6_literals() {
        let url = postgres_connection_url("::1", 5432, "synctv_test");

        assert_eq!(
            url,
            "postgresql://synctv:synctv_test@[::1]:5432/synctv_test"
        );
    }

    #[test]
    fn resolve_host_port_uses_ipv4_port_for_domain_hosts() {
        assert_eq!(
            candidate_endpoints_for_host("docker.internal", Some(5432), Some(15433)),
            vec![
                ("docker.internal".to_string(), 5432),
                ("docker.internal".to_string(), 15433)
            ]
        );
    }

    #[test]
    fn resolve_host_port_keeps_reported_host_for_ipv4_domain_mappings() {
        assert_eq!(
            candidate_endpoints_for_host("10.0.0.8", Some(5432), None),
            vec![("10.0.0.8".to_string(), 5432)]
        );
    }

    #[test]
    fn resolve_host_port_uses_ipv6_port_for_ipv6_hosts() {
        assert_eq!(
            candidate_endpoints_for_host("[::1]", Some(5432), Some(15433)),
            vec![
                ("[::1]".to_string(), 15433),
                ("127.0.0.1".to_string(), 5432)
            ]
        );
    }

    #[test]
    fn resolve_host_port_uses_ipv6_when_domain_only_has_ipv6_mapping() {
        assert_eq!(
            candidate_endpoints_for_host("docker.internal", None, Some(15433)),
            vec![("docker.internal".to_string(), 15433)]
        );
    }

    #[test]
    fn resolve_host_port_rewrites_localhost_to_ipv6_literal_when_needed() {
        assert_eq!(
            candidate_endpoints_for_host("localhost", Some(5432), Some(15433)),
            vec![("::1".to_string(), 15433), ("127.0.0.1".to_string(), 5432)]
        );
    }

    fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
        if let Some(message) = payload.downcast_ref::<String>() {
            return message.clone();
        }
        if let Some(message) = payload.downcast_ref::<&str>() {
            return (*message).to_string();
        }
        "<non-string panic payload>".to_string()
    }
}
