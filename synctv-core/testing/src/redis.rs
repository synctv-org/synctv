use std::fs::{File, OpenOptions};
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use redis::AsyncCommands;
use testcontainers::core::{ImageExt, IntoContainerPort, ReuseDirective, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::redis::Redis;
use tokio::sync::{OnceCell, RwLock, Semaphore, SemaphorePermit};

use crate::postgres::{docker_startup_parallelism, docker_startup_timeout};

pub type RedisConnectionManager = redis::aio::ConnectionManager;
pub type RedisConnectionHandle = Arc<RwLock<redis::aio::ConnectionManager>>;
static REDIS_START_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(docker_startup_parallelism()));
const DEFAULT_REDIS_ACTIVE_PARALLELISM: usize = 32;
const MIN_REDIS_ACTIVE_PARALLELISM: usize = 1;
const REDIS_ACTIVE_PARALLELISM_ENV: &str = "SYNCTV_TEST_REDIS_ACTIVE_PARALLELISM";
static REDIS_ACTIVE_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(redis_active_parallelism()));
const TEST_RUN_LABEL: &str = "synctv.test.run_id";
pub const REDIS_VERSION: &str = "8";
const REDIS_EPHEMERAL_TUNING_ARGS: &[&str] = &[
    "--save",
    "",
    "--appendonly",
    "no",
    "--stop-writes-on-bgsave-error",
    "no",
    "--maxclients",
    "100000",
    "--maxmemory",
    "1gb",
    "--maxmemory-policy",
    "noeviction",
    "--io-threads",
    "8",
    "--io-threads-do-reads",
    "yes",
    "--loglevel",
    "warning",
    "--slowlog-log-slower-than",
    "-1",
    "--slowlog-max-len",
    "0",
    "--latency-monitor-threshold",
    "0",
    "--activerehashing",
    "no",
    "--activedefrag",
    "no",
];

fn redis_ephemeral_tuning_args() -> impl Iterator<Item = &'static str> {
    REDIS_EPHEMERAL_TUNING_ARGS.iter().copied()
}

static SHARED_REDIS: OnceCell<Arc<SharedRedisServer>> = OnceCell::const_new();

struct ProcessLock(File);
struct DockerSlotGuard {
    _local_permit: SemaphorePermit<'static>,
    _process_lock: ProcessLock,
}

struct SharedRedisServer {
    // Intentionally held but never dropped: the shared container survives
    // until the next test run's orphan cleanup removes it.  Using
    // ManuallyDrop prevents the Drop impl from calling `docker rm` when
    // any single nextest worker process exits while others are still running.
    _container: std::mem::ManuallyDrop<ContainerAsync<Redis>>,
    name: String,
    host: String,
    port: u16,
}

pub struct RedisContainer {
    shared: Arc<SharedRedisServer>,
    cleaned_up: bool,
}

impl ProcessLock {
    fn try_acquire(name: &str) -> Option<Self> {
        let mut path = PathBuf::from("/tmp");
        path.push(format!("synctv-{name}.lock"));
        Self::try_acquire_path(&path)
    }

    fn try_acquire_path(path: &Path) -> Option<Self> {
        let file = Self::open_lock_file(path);
        match file.try_lock() {
            Ok(()) => Some(Self(file)),
            Err(_) => None,
        }
    }

    fn open_lock_file(path: &Path) -> File {
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
            .expect("failed to release process lock for redis test startup");
    }
}

fn redis_active_parallelism() -> usize {
    redis_active_parallelism_from(std::env::var(REDIS_ACTIVE_PARALLELISM_ENV).ok().as_deref())
}

fn redis_active_parallelism_from(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(DEFAULT_REDIS_ACTIVE_PARALLELISM, |slots| {
            slots.max(MIN_REDIS_ACTIVE_PARALLELISM)
        })
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
        "redis-test".to_string()
    } else {
        name
    }
}

fn sanitize_key_prefix_component(raw: &str) -> String {
    let mut value: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while value.ends_with('-') {
        value.pop();
    }
    if value.is_empty() {
        "test".to_string()
    } else {
        value
    }
}

fn current_test_key_namespace() -> String {
    std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::thread::current().name().map(str::to_owned))
        .map_or_else(
            || "unknown-test".to_string(),
            |value| sanitize_key_prefix_component(&value),
        )
}

fn current_process_id() -> u32 {
    std::process::id()
}

fn current_test_run_id() -> String {
    std::env::var("NEXTEST_RUN_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map_or_else(
            || format!("pid-{}", current_process_id()),
            |value| sanitize_container_name(&value),
        )
}

fn shared_container_name() -> String {
    format!("synctv-redis-shared-{}", current_test_run_id())
}

pub fn test_redis_key_prefix(label: &str) -> String {
    format!(
        "synctv-test:{}:{}:pid{}:",
        current_test_run_id(),
        sanitize_key_prefix_component(&format!("{}-{}", label, current_test_key_namespace())),
        current_process_id()
    )
}

fn named_redis_request(container_name: &str) -> testcontainers::ContainerRequest<Redis> {
    Redis::default()
        .with_container_name(container_name.to_string())
        .with_label(TEST_RUN_LABEL, current_test_run_id())
        .with_reuse(ReuseDirective::Always)
        .with_tag(REDIS_VERSION)
        .with_cmd(redis_ephemeral_tuning_args())
        // The Redis 8 image no longer emits the legacy "Ready to accept
        // connections" stdout line that testcontainers-modules waits for.
        // We intentionally skip image-level log readiness and rely on the
        // explicit TCP + PING readiness probes in resolve_host_port /
        // wait_for_redis_ready instead.
        .with_ready_conditions(Vec::<WaitFor>::new())
        .with_ulimit("nofile", 200_000, Some(200_000))
}

fn redis_connection_url(host: &str, port: u16) -> String {
    format!("redis://{}:{port}", format_socket_host(host))
}

fn format_socket_host(host: &str) -> String {
    if matches!(host_address_family(host), Some(IpAddr::V6(_))) && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

async fn resolve_host_port(container: &ContainerAsync<Redis>, internal_port: u16) -> (String, u16) {
    let host = container
        .get_host()
        .await
        .expect("Failed to get Redis host")
        .to_string();

    // Retry port resolution: Docker may not have finished mapping ports
    // immediately after container start, especially under heavy concurrent load.
    // Use a tighter 30-second deadline for port resolution (not the full
    // docker_startup_timeout) since port mapping should appear quickly.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut _last_port_error = String::from("port resolution has not been attempted yet");

    let endpoints = loop {
        let ports = container
            .ports()
            .await
            .expect("Failed to inspect Redis port mappings");
        let eps = candidate_endpoints_for_host(
            &host,
            ports.map_to_host_port_ipv4(internal_port.tcp()),
            ports.map_to_host_port_ipv6(internal_port.tcp()),
        );
        if !eps.is_empty() {
            break eps;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Failed to resolve Redis endpoint for host {host} within 30 seconds"
        );
        _last_port_error = format!(
            "no port mapping for internal port {internal_port} (ipv4={:?}, ipv6={:?})",
            ports.map_to_host_port_ipv4(internal_port.tcp()),
            ports.map_to_host_port_ipv6(internal_port.tcp()),
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };

    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut last_error = String::from("redis endpoint probe has not run yet");

    while std::time::Instant::now() < deadline {
        for (candidate_host, candidate_port) in &endpoints {
            let redis_url = redis_connection_url(candidate_host, *candidate_port);
            match redis::Client::open(redis_url.clone()) {
                Ok(client) => match client.get_multiplexed_async_connection().await {
                    Ok(mut conn) => {
                        let ping_result: redis::RedisResult<String> =
                            redis::cmd("PING").query_async(&mut conn).await;
                        if ping_result.is_ok() {
                            return (candidate_host.clone(), *candidate_port);
                        }
                        last_error = format!("ping failed for {redis_url}: {ping_result:?}");
                    }
                    Err(err) => {
                        last_error = format!("connect failed for {redis_url}: {err}");
                    }
                },
                Err(err) => {
                    last_error = format!("client open failed for {redis_url}: {err}");
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    panic!(
        "Redis container did not become reachable within {:?} across endpoints {:?}: {}",
        docker_startup_timeout(),
        endpoints,
        last_error
    );
}

fn host_address_family(host: &str) -> Option<IpAddr> {
    let normalized = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    normalized.parse::<IpAddr>().ok()
}

fn detect_primary_ipv4_address() -> Option<String> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if is_viable_host_ipv4(ip) => Some(ip.to_string()),
        _ => None,
    }
}

const fn is_viable_host_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        // RFC 2544 benchmarking / many local proxy virtual interfaces.
        || (a == 198 && matches!(b, 18 | 19)))
}

fn push_candidate(candidates: &mut Vec<(String, u16)>, host: String, port: u16) {
    if !candidates
        .iter()
        .any(|(existing_host, existing_port)| existing_host == &host && *existing_port == port)
    {
        candidates.push((host, port));
    }
}

fn candidate_endpoints_for_host(
    host: &str,
    ipv4_port: Option<u16>,
    ipv6_port: Option<u16>,
) -> Vec<(String, u16)> {
    let mut candidates = Vec::new();
    let local_ipv4 = detect_primary_ipv4_address();

    match host_address_family(host) {
        Some(IpAddr::V4(_)) => {
            if let Some(port) = ipv4_port {
                push_candidate(&mut candidates, host.to_string(), port);
                if host.starts_with("127.") {
                    if let Some(local_ipv4) = local_ipv4.as_ref() {
                        push_candidate(&mut candidates, local_ipv4.clone(), port);
                    }
                }
            }
            if let Some(port) = ipv6_port.filter(|port| Some(*port) != ipv4_port) {
                push_candidate(&mut candidates, "::1".to_string(), port);
            }
        }
        Some(IpAddr::V6(_)) => {
            if let Some(port) = ipv6_port {
                push_candidate(&mut candidates, host.to_string(), port);
            }
            if let Some(port) = ipv4_port.filter(|port| Some(*port) != ipv6_port) {
                push_candidate(&mut candidates, "127.0.0.1".to_string(), port);
                if let Some(local_ipv4) = local_ipv4.as_ref() {
                    push_candidate(&mut candidates, local_ipv4.clone(), port);
                }
            }
        }
        None => {
            if let Some(port) = ipv6_port.filter(|_| host == "localhost") {
                push_candidate(&mut candidates, "::1".to_string(), port);
            }
            if let Some(port) = ipv4_port {
                let ipv4_host = if host == "localhost" {
                    "127.0.0.1".to_string()
                } else {
                    host.to_string()
                };
                push_candidate(&mut candidates, ipv4_host, port);
                if host == "localhost" {
                    if let Some(local_ipv4) = local_ipv4.as_ref() {
                        push_candidate(&mut candidates, local_ipv4.clone(), port);
                    }
                }
            }
            if let Some(port) =
                ipv6_port.filter(|port| Some(*port) != ipv4_port && host != "localhost")
            {
                push_candidate(&mut candidates, host.to_string(), port);
            }
        }
    }

    candidates
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
        },
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

fn startup_error_is_retriable(err: &str) -> bool {
    let err = err.to_ascii_lowercase();
    err.contains("marked for removal") || err.contains("no such container")
}

fn docker_rm_force_with_program(program: &str, container_ref: &str) -> Result<(), String> {
    let args = ["rm", "-v", "-f", container_ref];
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
    let current_run_id = current_test_run_id();
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
                &format!("{{{{index .Config.Labels \"{TEST_RUN_LABEL}\"}}}}"),
            ])
            .output();

        let Ok(inspect) = inspect else {
            continue;
        };
        if !inspect.status.success() {
            continue;
        }

        let run_id = String::from_utf8_lossy(&inspect.stdout).trim().to_string();
        if run_id == current_run_id {
            continue;
        }

        if let Err(err) = docker_rm_force(container_id) {
            eprintln!(
                "warning: failed to remove orphaned redis test container {container_id}: {err}"
            );
        }
    }
}

fn cleanup_orphaned_run_lock_files(prefix: &str) {
    let Ok(entries) = std::fs::read_dir("/tmp") else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with(prefix) || !file_name.ends_with(".lock") {
            continue;
        }

        let Some(lock) = ProcessLock::try_acquire_path(&path) else {
            continue;
        };
        drop(lock);

        let _ = std::fs::remove_file(path);
    }
}

impl SharedRedisServer {
    fn url(&self) -> String {
        redis_connection_url(&self.host, self.port)
    }
}

impl RedisContainer {
    const fn new(shared: Arc<SharedRedisServer>) -> Self {
        Self {
            shared,
            cleaned_up: false,
        }
    }

    pub fn cleanup(mut self) {
        self.cleaned_up = true;
    }

    pub fn terminate(mut self) {
        // The container is intentionally ManuallyDrop'd so it survives
        // individual worker process exits.  Use force-removal via Docker CLI
        // as fallback when explicit termination is requested.
        let result = docker_rm_force(&self.shared.name);
        let warning = handle_cleanup_result(
            &mut self.cleaned_up,
            &self.shared.name,
            result,
            "redis",
            docker_rm_force,
        );
        log_cleanup_warning_if_needed(warning);
    }

    pub fn id(&self) -> String {
        // The container is kept alive via ManuallyDrop; access the ID
        // through the Docker CLI since we no longer hold a direct reference.
        let output = Command::new("docker")
            .args(["inspect", &self.shared.name, "--format", "{{.Id}}"])
            .output();
        match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => self.shared.name.clone(),
        }
    }

    pub fn host(&self) -> String {
        self.shared.host.clone()
    }

    pub fn port_ipv4(&self, _internal_port: u16) -> u16 {
        self.shared.port
    }

    pub fn host_port(&self, _internal_port: u16) -> (String, u16) {
        (self.shared.host.clone(), self.shared.port)
    }

    pub fn connection_url(&self) -> String {
        self.shared.url()
    }
}

impl Drop for RedisContainer {
    fn drop(&mut self) {
        self.cleaned_up = true;
    }
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
        &REDIS_START_SERIALIZER,
        docker_startup_parallelism(),
        name,
        "Redis startup guard should not be closed",
        "redis process slot task should not panic",
    )
    .await
}

async fn acquire_docker_active_slot(name: &str) -> DockerSlotGuard {
    acquire_docker_slot(
        &REDIS_ACTIVE_SERIALIZER,
        redis_active_parallelism(),
        name,
        "Redis active-container guard should not be closed",
        "redis active container slot task should not panic",
    )
    .await
}

async fn init_shared_redis_server() -> SharedRedisServer {
    cleanup_orphaned_testcontainers("synctv-redis-");
    cleanup_orphaned_run_lock_files("synctv-redis-run-");
    let run_id = current_test_run_id();

    let lock_name = format!("redis-run-{run_id}");
    let _startup_lock = tokio::task::spawn_blocking(move || loop {
        if let Some(lock) = ProcessLock::try_acquire(&lock_name) {
            return lock;
        }
        std::thread::sleep(Duration::from_millis(100));
    })
    .await
    .expect("redis startup lock task should not panic");

    let container_name = shared_container_name();
    // Acquire active + start slots only during initialization so the file locks
    // are released once the shared container is confirmed ready.  Holding them
    // for the process lifetime (as _slot_guard previously did) limits total
    // concurrency to `redis_active_parallelism` (default 8) across all nextest
    // workers – which is far too low for the 20+ ignored Docker tests that run
    // in parallel.
    let active_slot = acquire_docker_active_slot("redis-active").await;
    let container = {
        let _redis_process_lock = acquire_docker_start_slot("redis-start").await;
        let start_deadline = std::time::Instant::now() + docker_startup_timeout();
        let mut last_start_error;
        loop {
            match tokio::time::timeout(
                docker_startup_timeout(),
                named_redis_request(&container_name).start(),
            )
            .await
            {
                Ok(Ok(c)) => break c,
                Ok(Err(e)) => {
                    let err_str = format!("{e}");
                    // Retry known Docker container lifecycle races while a named
                    // shared container is being cleaned up or recreated.
                    if startup_error_is_retriable(&err_str) {
                        last_start_error = err_str;
                        assert!(
                            std::time::Instant::now() < start_deadline,
                            "Failed to start Redis after retries: {last_start_error}"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        continue;
                    }
                    panic!("Failed to start Redis: {e}");
                }
                Err(elapsed) => {
                    panic!(
                        "Docker container startup timed out after {:?}: {elapsed} (is Docker running?)",
                        docker_startup_timeout(),
                    );
                }
            }
        }
    };
    let (host, port) = resolve_host_port(&container, 6379).await;
    let client = redis::Client::open(redis_connection_url(&host, port))
        .expect("Failed to create Redis client");
    wait_for_redis_ready(&client).await;
    drop(active_slot);

    SharedRedisServer {
        _container: std::mem::ManuallyDrop::new(container),
        name: container_name,
        host,
        port,
    }
}

async fn shared_redis_server() -> Arc<SharedRedisServer> {
    Arc::clone(
        SHARED_REDIS
            .get_or_init(|| async { Arc::new(init_shared_redis_server().await) })
            .await,
    )
}

async fn start_redis_inner(_label: &str) -> (RedisContainer, String, redis::Client) {
    let shared = shared_redis_server().await;
    let redis_url = shared.url();
    let client = redis::Client::open(redis_url.clone()).expect("Failed to create Redis client");
    wait_for_redis_ready(&client).await;

    (RedisContainer::new(shared), redis_url, client)
}

pub async fn start_redis_with_client() -> (RedisContainer, redis::Client) {
    let (container, _redis_url, client) = start_redis_inner("client").await;
    (container, client)
}

/// Start a shared Redis container and return a `ConnectionManager`.
///
/// This reuses the shared Redis container across processes in the same test run.
pub async fn start_redis() -> (RedisContainer, RedisConnectionManager) {
    let (container, redis_url, _client) = start_redis_inner("conn-mgr").await;
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let manager = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create Redis connection manager");
    (container, manager)
}

pub async fn start_redis_url_with_label(label: &str) -> (RedisContainer, String) {
    let (container, redis_url, _client) = start_redis_inner(label).await;
    (container, redis_url)
}

/// Start a **dedicated** Redis container that is NOT shared with other tests.
///
/// Use this for tests that need to terminate or otherwise destroy their Redis
/// instance (e.g. fail-closed tests).  The shared container must never be
/// terminated because other concurrent test processes depend on it.
pub async fn start_dedicated_redis() -> (RedisContainer, RedisConnectionManager) {
    let container_name = format!(
        "synctv-redis-dedicated-{}-{}",
        current_process_id(),
        sanitize_container_name(
            &std::env::var("NEXTEST_TEST_NAME")
                .ok()
                .or_else(|| std::thread::current().name().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_string())
        )
    );
    let container = tokio::time::timeout(
        docker_startup_timeout(),
        named_redis_request(&container_name).start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start dedicated Redis");

    let (host, port) = resolve_host_port(&container, 6379).await;
    let redis_url = redis_connection_url(&host, port);
    let client = redis::Client::open(redis_url.clone()).expect("Failed to create Redis client");
    wait_for_redis_ready(&client).await;
    let manager = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create Redis connection manager");

    let shared = Arc::new(SharedRedisServer {
        _container: std::mem::ManuallyDrop::new(container),
        name: container_name,
        host,
        port,
    });

    (RedisContainer::new(shared), manager)
}

/// Start a **dedicated** Redis container (not shared) and return its URL.
///
/// Use for tests that terminate or destroy their Redis instance.
/// The label is used for the container name; each invocation creates a
/// separate container.
pub async fn start_dedicated_redis_url_with_label(_label: &str) -> (RedisContainer, String) {
    let (container, _manager) = start_dedicated_redis().await;
    let redis_url = container.connection_url();
    (container, redis_url)
}

pub async fn start_redis_handle() -> (RedisContainer, RedisConnectionHandle) {
    let (container, redis_url, _client) = start_redis_inner("handle").await;
    let manager = redis::aio::ConnectionManager::new(
        redis::Client::open(redis_url).expect("Failed to create Redis client for handle"),
    )
    .await
    .expect("Failed to create Redis connection manager");
    (container, Arc::new(RwLock::new(manager)))
}

pub async fn start_redis_url() -> (RedisContainer, String) {
    let (container, redis_url, _client) = start_redis_inner("url").await;
    (container, redis_url)
}

pub async fn wait_for_redis_ready(client: &redis::Client) {
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
                    conn.set_ex("synctv:test:ping", "pong", 5).await;
                let get_result: redis::RedisResult<String> = conn.get("synctv:test:ping").await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_active_parallelism_defaults_to_conservative_live_limit() {
        assert_eq!(
            redis_active_parallelism_from(None),
            DEFAULT_REDIS_ACTIVE_PARALLELISM
        );
        assert_eq!(DEFAULT_REDIS_ACTIVE_PARALLELISM, 32);
    }

    #[test]
    fn redis_active_parallelism_honors_valid_override() {
        assert_eq!(redis_active_parallelism_from(Some("7")), 7);
    }

    #[test]
    fn redis_active_parallelism_rejects_zero_override() {
        assert_eq!(
            redis_active_parallelism_from(Some("0")),
            MIN_REDIS_ACTIVE_PARALLELISM
        );
    }

    #[test]
    fn shared_container_name_uses_nextest_run_id_when_present() {
        unsafe {
            std::env::set_var("NEXTEST_RUN_ID", "Run.Id/42");
        }
        let name = shared_container_name();
        unsafe {
            std::env::remove_var("NEXTEST_RUN_ID");
        }

        assert_eq!(name, "synctv-redis-shared-run-id-42");
    }

    #[test]
    fn redis_connection_url_brackets_ipv6_literals() {
        let url = redis_connection_url("::1", 6379);

        assert_eq!(url, "redis://[::1]:6379");
    }

    #[test]
    fn named_redis_request_uses_high_concurrency_ephemeral_tuning() {
        let request = named_redis_request("synctv-redis-test");
        let cmd: Vec<_> = request.cmd().map(std::borrow::Cow::into_owned).collect();

        assert!(
            cmd.windows(2).any(|pair| pair == ["--appendonly", "no"]),
            "test redis should disable AOF persistence: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["--stop-writes-on-bgsave-error", "no"]),
            "test redis should not fail closed on snapshot persistence errors when persistence is disabled: {cmd:?}"
        );
        assert!(
            cmd.windows(2).any(|pair| pair == ["--maxclients", "100000"]),
            "test redis should expose a very high client limit for nextest-scale concurrency: {cmd:?}"
        );
        assert!(
            cmd.windows(2).any(|pair| pair == ["--io-threads", "8"]),
            "test redis should enable additional IO threads for high concurrency: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["--slowlog-log-slower-than", "-1"]),
            "test redis should disable slowlog collection overhead: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["--latency-monitor-threshold", "0"]),
            "test redis should disable latency monitor overhead: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["--activerehashing", "no"]),
            "test redis should disable active rehashing overhead for ephemeral workloads: {cmd:?}"
        );
        assert!(
            cmd.windows(2).any(|pair| pair == ["--activedefrag", "no"]),
            "test redis should disable active defragmentation overhead for ephemeral workloads: {cmd:?}"
        );
        assert!(
            format!("{request:?}").contains("nofile"),
            "test redis should raise the container nofile ulimit for high maxclients settings"
        );
        assert!(
            request.ready_conditions().is_empty(),
            "test redis should bypass stale image-level log readiness and use explicit ping readiness"
        );
    }

    #[test]
    fn resolve_host_port_uses_ipv4_port_for_domain_hosts() {
        assert_eq!(
            candidate_endpoints_for_host("docker.internal", Some(6379), Some(16379)),
            vec![
                ("docker.internal".to_string(), 6379),
                ("docker.internal".to_string(), 16379)
            ]
        );
    }

    #[test]
    fn resolve_host_port_uses_ipv6_port_for_ipv6_hosts() {
        assert_eq!(
            candidate_endpoints_for_host("[::1]", Some(6379), Some(16379)),
            vec![
                ("[::1]".to_string(), 16379),
                ("127.0.0.1".to_string(), 6379)
            ]
        );
    }

    #[test]
    fn localhost_candidates_include_primary_ipv4_fallback_when_available() {
        let candidates = candidate_endpoints_for_host("localhost", Some(6379), Some(16379));

        assert!(
            candidates.contains(&("127.0.0.1".to_string(), 6379)),
            "localhost candidates should include loopback IPv4: {candidates:?}"
        );
        if let Some(local_ipv4) = detect_primary_ipv4_address() {
            assert!(
                candidates.contains(&(local_ipv4, 6379)),
                "localhost candidates should include the primary IPv4 fallback: {candidates:?}"
            );
        }
    }

    #[test]
    fn viable_host_ipv4_rejects_proxy_benchmark_range() {
        assert!(!is_viable_host_ipv4(Ipv4Addr::new(198, 18, 0, 1)));
        assert!(!is_viable_host_ipv4(Ipv4Addr::new(198, 19, 255, 254)));
        assert!(is_viable_host_ipv4(Ipv4Addr::new(192, 168, 0, 40)));
    }

    #[test]
    fn cleanup_marks_container_as_cleaned_up_on_success() {
        let mut cleaned_up = false;

        let warning = handle_cleanup_result(
            &mut cleaned_up,
            "synctv-redis-test",
            Ok(()),
            "redis",
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
            "synctv-redis-test",
            Err("docker rm failed".to_string()),
            "redis",
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
            "synctv-redis-test",
            Err("Error response from daemon: No such container: synctv-redis-test".to_string()),
            "redis",
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
            "synctv-redis-test",
            Err("docker rm failed".to_string()),
            "redis",
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
        let err = docker_rm_force_with_program("false", "synctv-redis-test")
            .expect_err("failed command must surface as an error");

        assert!(
            err.contains("command `false rm -v -f synctv-redis-test` exited with status"),
            "error should include the failing command line: {err}"
        );
    }

    #[test]
    fn docker_rm_force_reports_spawn_failure() {
        let err = docker_rm_force_with_program(
            "synctv-command-that-should-not-exist",
            "synctv-redis-test",
        )
        .expect_err("spawn failure must surface as an error");

        assert!(
            err.contains("failed to spawn `synctv-command-that-should-not-exist`"),
            "error should include the missing program: {err}"
        );
    }

    #[test]
    fn startup_error_is_retriable_for_known_container_lifecycle_races() {
        assert!(startup_error_is_retriable("No such container: abc123"));
        assert!(startup_error_is_retriable(
            "DockerResponseServerError { status_code: 404, message: \"No such container\" }"
        ));
        assert!(startup_error_is_retriable(
            "container is marked for removal"
        ));
        assert!(startup_error_is_retriable(
            "DockerResponseServerError { status_code: 409, message: \"container is marked for removal\" }"
        ));
        assert!(!startup_error_is_retriable(
            "404 gateway from registry mirror"
        ));
        assert!(!startup_error_is_retriable("409 conflict during start"));
        assert!(!startup_error_is_retriable("authentication failed"));
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed Redis"]
    async fn start_redis_reuses_shared_container_within_process() {
        let (redis_one, url_one) = start_redis_url_with_label("shared-a").await;
        let (redis_two, url_two) = start_redis_url_with_label("shared-b").await;

        assert_eq!(
            redis_one.host_port(6379),
            redis_two.host_port(6379),
            "redis leases in the same process should reuse one shared container"
        );
        assert_eq!(
            url_one, url_two,
            "shared redis leases should point at the same endpoint"
        );

        redis_one.cleanup();
        redis_two.cleanup();
    }
}
