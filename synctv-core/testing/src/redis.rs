use std::fs::{File, OpenOptions};
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use redis::AsyncCommands;
use testcontainers::core::{ImageExt, IntoContainerPort};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::redis::Redis;
use tokio::sync::{RwLock, Semaphore, SemaphorePermit};

use crate::postgres::{docker_startup_parallelism, docker_startup_timeout};

pub type RedisConnectionManager = redis::aio::ConnectionManager;
pub type RedisConnectionHandle = Arc<RwLock<redis::aio::ConnectionManager>>;
static REDIS_START_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(docker_startup_parallelism()));
const DEFAULT_REDIS_ACTIVE_PARALLELISM: usize = 4;
const MIN_REDIS_ACTIVE_PARALLELISM: usize = 1;
const REDIS_ACTIVE_PARALLELISM_ENV: &str = "SYNCTV_TEST_REDIS_ACTIVE_PARALLELISM";
static REDIS_ACTIVE_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(redis_active_parallelism()));
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

fn redis_container_name(label: &str) -> String {
    format!(
        "synctv-redis-{}-{}-{}",
        current_test_label(),
        sanitize_container_name(label),
        nanoid::nanoid!(6).to_lowercase()
    )
}

fn named_redis_request(container_name: &str) -> testcontainers::ContainerRequest<Redis> {
    Redis::default()
        .with_container_name(container_name.to_string())
        .with_label(TEST_CONTAINER_OWNER_LABEL, std::process::id().to_string())
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
    let ports = container
        .ports()
        .await
        .expect("Failed to inspect Redis port mappings");
    let endpoints = candidate_endpoints_for_host(
        &host,
        ports.map_to_host_port_ipv4(internal_port.tcp()),
        ports.map_to_host_port_ipv6(internal_port.tcp()),
    );

    assert!(
        !endpoints.is_empty(),
        "Failed to resolve Redis endpoint for host {host}"
    );

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
                "warning: failed to remove orphaned redis test container {container_id}: {err}"
            );
        }
    }
}

pub struct RedisContainer {
    inner: Option<ContainerAsync<Redis>>,
    name: String,
    cleaned_up: bool,
    _slot_guard: Option<DockerSlotGuard>,
}

impl RedisContainer {
    fn new(inner: ContainerAsync<Redis>, name: String, slot_guard: DockerSlotGuard) -> Self {
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
                "redis",
                docker_rm_force,
            ));
        } else {
            self.cleaned_up = true;
        }
    }

    pub const fn raw(&self) -> &ContainerAsync<Redis> {
        self.inner
            .as_ref()
            .expect("redis container should still be present")
    }

    pub async fn host(&self) -> String {
        self.host_port(6379).await.0
    }

    pub async fn port_ipv4(&self, internal_port: u16) -> u16 {
        self.host_port(internal_port).await.1
    }

    pub async fn host_port(&self, internal_port: u16) -> (String, u16) {
        resolve_host_port(self.raw(), internal_port).await
    }
}

impl std::ops::Deref for RedisContainer {
    type Target = ContainerAsync<Redis>;

    fn deref(&self) -> &Self::Target {
        self.raw()
    }
}

impl Drop for RedisContainer {
    fn drop(&mut self) {
        if let Some(container) = self.inner.take() {
            drop(container);
        }
        if !self.cleaned_up {
            if let Err(err) = docker_rm_force(&self.name) {
                eprintln!(
                    "warning: failed to force-remove redis test container {} during Drop: {err}",
                    self.name
                );
            }
        }
    }
}

async fn start_redis_inner(label: &str) -> (RedisContainer, String, redis::Client) {
    let container_name = redis_container_name(label);
    let container_slot = acquire_docker_active_slot("redis-active").await;
    cleanup_orphaned_testcontainers("synctv-redis-");
    let container = {
        let _redis_process_lock = acquire_docker_start_slot("redis-start").await;
        tokio::time::timeout(
            docker_startup_timeout(),
            named_redis_request(&container_name).start(),
        )
        .await
        .expect("Docker container startup timed out (is Docker running?)")
        .expect("Failed to start Redis")
    };
    let (host, port) = resolve_host_port(&container, 6379).await;
    let redis_url = redis_connection_url(&host, port);
    let client = redis::Client::open(redis_url.clone()).expect("Failed to create Redis client");
    wait_for_redis_ready(&client).await;
    (
        RedisContainer::new(container, container_name, container_slot),
        redis_url,
        client,
    )
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

pub async fn start_redis_with_client() -> (RedisContainer, redis::Client) {
    let (container, _redis_url, client) = start_redis_inner("client").await;
    (container, client)
}

pub async fn start_redis_url_with_label(label: &str) -> (RedisContainer, String) {
    let (container, redis_url, _client) = start_redis_inner(label).await;
    (container, redis_url)
}

pub async fn start_redis() -> (RedisContainer, RedisConnectionManager) {
    let (container, client) = start_redis_with_client().await;
    let manager = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create Redis connection manager");
    (container, manager)
}

pub async fn start_redis_handle() -> (RedisContainer, RedisConnectionHandle) {
    let (container, manager) = start_redis().await;
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
        assert_eq!(DEFAULT_REDIS_ACTIVE_PARALLELISM, 4);
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
        assert!(fallback_called, "explicit cleanup failure must try fallback removal");
        assert!(cleaned_up, "successful fallback should mark the container as cleaned up");
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
        assert!(cleaned_up, "missing container should still be treated as cleaned up");
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
            err.contains("command `false rm -f synctv-redis-test` exited with status"),
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
    fn redis_connection_url_uses_resolved_host() {
        let url = redis_connection_url("docker.internal", 6379);

        assert_eq!(url, "redis://docker.internal:6379");
        assert!(
            !url.contains("127.0.0.1"),
            "redis URL must not hardcode localhost"
        );
    }

    #[test]
    fn redis_connection_url_brackets_ipv6_literals() {
        let url = redis_connection_url("::1", 6379);

        assert_eq!(url, "redis://[::1]:6379");
    }

    #[test]
    fn resolve_host_port_uses_ipv4_port_for_domain_hosts() {
        assert_eq!(
            candidate_endpoints_for_host("docker.internal", Some(6379), Some(16380)),
            vec![
                ("docker.internal".to_string(), 6379),
                ("docker.internal".to_string(), 16380)
            ]
        );
    }

    #[test]
    fn resolve_host_port_keeps_reported_host_for_ipv4_domain_mappings() {
        assert_eq!(
            candidate_endpoints_for_host("10.0.0.8", Some(6379), None),
            vec![("10.0.0.8".to_string(), 6379)]
        );
    }

    #[test]
    fn resolve_host_port_uses_ipv6_port_for_ipv6_hosts() {
        assert_eq!(
            candidate_endpoints_for_host("[::1]", Some(6379), Some(16380)),
            vec![
                ("[::1]".to_string(), 16380),
                ("127.0.0.1".to_string(), 6379)
            ]
        );
    }

    #[test]
    fn resolve_host_port_uses_ipv6_when_domain_only_has_ipv6_mapping() {
        assert_eq!(
            candidate_endpoints_for_host("docker.internal", None, Some(16380)),
            vec![("docker.internal".to_string(), 16380)]
        );
    }

    #[test]
    fn resolve_host_port_rewrites_localhost_to_ipv6_literal_when_needed() {
        assert_eq!(
            candidate_endpoints_for_host("localhost", Some(6379), Some(16380)),
            vec![("::1".to_string(), 16380), ("127.0.0.1".to_string(), 6379)]
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
