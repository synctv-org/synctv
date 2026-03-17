use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use redis::AsyncCommands;
use testcontainers::core::ImageExt;
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
            let _ = container.rm().await;
        }
        self.cleaned_up = true;
    }

    pub const fn raw(&self) -> &ContainerAsync<Redis> {
        self.inner
            .as_ref()
            .expect("redis container should still be present")
    }

    #[cfg(test)]
    const fn is_cleaned_up(&self) -> bool {
        self.cleaned_up
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
            let _ = Command::new("docker")
                .args(["rm", "-f", self.name.as_str()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
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
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get port");
    let redis_url = format!("redis://127.0.0.1:{port}");
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
    fn cleanup_marks_container_as_cleaned_up() {
        let container = RedisContainer {
            inner: None,
            name: "synctv-redis-test".to_string(),
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
