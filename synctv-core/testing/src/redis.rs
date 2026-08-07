use std::net::IpAddr;
use std::process::Command;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use redis::AsyncCommands;
use testcontainers::core::{ImageExt, IntoContainerPort, ReuseDirective, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::redis::Redis;
use tokio::sync::{OnceCell, RwLock, Semaphore};

use crate::docker::{
    acquire_docker_slot, acquire_run_lock, candidate_endpoints_for_host,
    cleanup_error_indicates_missing_container, cleanup_orphaned_run_lock_files,
    cleanup_orphaned_testcontainers, current_process_id as docker_current_process_id,
    current_test_run_id as docker_current_test_run_id,
    current_test_run_id_from as docker_current_test_run_id_from,
    docker_named_container_belongs_to_current_run, docker_port_candidates, docker_rm_force,
    ensure_docker_image, host_address_family, sanitize_container_name,
    startup_error_is_named_container_conflict, startup_error_is_retriable, DockerSlotGuard,
    ProcessLock, TEST_RUN_LABEL,
};
use crate::postgres::{docker_startup_parallelism, docker_startup_timeout};

static REDIS_START_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(docker_startup_parallelism()));
const DEFAULT_REDIS_ACTIVE_PARALLELISM: usize = 32;
const MIN_REDIS_ACTIVE_PARALLELISM: usize = 1;
const REDIS_ACTIVE_PARALLELISM_ENV: &str = "SYNCTV_TEST_REDIS_ACTIVE_PARALLELISM";
static REDIS_ACTIVE_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(redis_active_parallelism()));
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
static REDIS_RUN_LOCK: OnceCell<Arc<ProcessLock>> = OnceCell::const_new();

struct SharedRedisServer {
    // Intentionally held but never dropped: the shared container survives
    // until the next test run's orphan cleanup removes it.  Using
    // ManuallyDrop prevents the Drop impl from calling `docker rm` when
    // any single nextest worker process exits while others are still running.
    //
    // Workers after the first one attach to the already-created named Docker
    // container via `docker port`, so they do not have a testcontainers handle.
    _container: Option<std::mem::ManuallyDrop<ContainerAsync<Redis>>>,
    name: String,
    host: String,
    port: u16,
    _run_lock: Arc<ProcessLock>,
}

pub struct RedisContainer {
    shared: Arc<SharedRedisServer>,
    cleaned_up: bool,
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
    docker_current_process_id()
}

fn current_test_run_id() -> String {
    docker_current_test_run_id("redis-test")
}

fn current_test_run_id_from(run_id: Option<&str>) -> String {
    docker_current_test_run_id_from(run_id, "redis-test")
}

fn shared_container_name() -> String {
    shared_container_name_from(std::env::var("NEXTEST_RUN_ID").ok().as_deref())
}

fn shared_container_name_from(run_id: Option<&str>) -> String {
    format!("synctv-redis-shared-{}", current_test_run_id_from(run_id))
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

async fn resolve_existing_named_redis_endpoint(container_name: &str) -> Option<(String, u16)> {
    if !docker_named_container_belongs_to_current_run(container_name, &current_test_run_id()) {
        return None;
    }

    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut last_error = String::from("docker port has not returned a Redis endpoint yet");

    while std::time::Instant::now() < deadline {
        if let Some(candidates) = docker_port_candidates(container_name, 6379) {
            for (host, port) in &candidates {
                let redis_url = redis_connection_url(host, *port);
                match redis::Client::open(redis_url.clone()) {
                    Ok(client) => match client.get_multiplexed_async_connection().await {
                        Ok(mut conn) => {
                            let ping_result: redis::RedisResult<String> =
                                redis::cmd("PING").query_async(&mut conn).await;
                            if ping_result.is_ok() {
                                return Some((host.clone(), *port));
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
        }

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    panic!(
        "Existing Redis container {container_name} did not become reachable within {:?}: {last_error}",
        docker_startup_timeout()
    );
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

async fn redis_run_lock(run_id: &str) -> Arc<ProcessLock> {
    let run_id = run_id.to_string();
    Arc::clone(
        REDIS_RUN_LOCK
            .get_or_init(|| async move { Arc::new(acquire_run_lock("redis", &run_id)) })
            .await,
    )
}

impl Drop for RedisContainer {
    fn drop(&mut self) {
        self.cleaned_up = true;
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
    let run_id = current_test_run_id();
    let run_lock = redis_run_lock(&run_id).await;
    cleanup_orphaned_testcontainers("synctv-redis-", "redis", &run_id);
    cleanup_orphaned_run_lock_files("synctv-redis-run-");

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
    let (container, host, port) = {
        let _redis_process_lock = acquire_docker_start_slot("redis-start").await;
        if let Some((host, port)) = resolve_existing_named_redis_endpoint(&container_name).await {
            (None, host, port)
        } else {
            let image_descriptor = named_redis_request(&container_name).descriptor();
            ensure_docker_image(&image_descriptor, docker_startup_timeout())
                .await
                .unwrap_or_else(|error| panic!("Failed to prepare Redis image: {error}"));
            let start_deadline = std::time::Instant::now() + docker_startup_timeout();
            let mut last_start_error;
            loop {
                match tokio::time::timeout(
                    docker_startup_timeout(),
                    named_redis_request(&container_name).start(),
                )
                .await
                {
                    Ok(Ok(c)) => {
                        let (host, port) = resolve_host_port(&c, 6379).await;
                        break (Some(std::mem::ManuallyDrop::new(c)), host, port);
                    }
                    Ok(Err(e)) => {
                        let err_str = format!("{e}");
                        if startup_error_is_named_container_conflict(&err_str) {
                            if let Some((host, port)) =
                                resolve_existing_named_redis_endpoint(&container_name).await
                            {
                                break (None, host, port);
                            }
                        }
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
        }
    };
    let client = redis::Client::open(redis_connection_url(&host, port))
        .expect("Failed to create Redis client");
    wait_for_redis_ready(&client).await;
    drop(active_slot);

    SharedRedisServer {
        _container: container,
        name: container_name,
        host,
        port,
        _run_lock: run_lock,
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

pub async fn start_redis_client_url_with_label(
    label: &str,
) -> (RedisContainer, redis::Client, String) {
    let (container, redis_url, client) = start_redis_inner(label).await;
    (container, client, redis_url)
}

pub async fn start_redis_client_manager_with_label(
    label: &str,
) -> (RedisContainer, redis::Client, redis::aio::ConnectionManager) {
    let (container, _redis_url, client) = start_redis_inner(label).await;
    let manager = redis_connection_manager(&client).await;
    (container, client, manager)
}

pub async fn start_redis_client_manager(
) -> (RedisContainer, redis::Client, redis::aio::ConnectionManager) {
    start_redis_client_manager_with_label("client-manager").await
}

/// Start a shared Redis container and return a `ConnectionManager`.
///
/// This reuses the shared Redis container across processes in the same test run.
pub async fn start_redis() -> (RedisContainer, redis::aio::ConnectionManager) {
    let (container, redis_url, _client) = start_redis_inner("conn-mgr").await;
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let manager = redis_connection_manager(&client).await;
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
pub async fn start_dedicated_redis() -> (RedisContainer, redis::aio::ConnectionManager) {
    let run_lock = redis_run_lock(&current_test_run_id()).await;
    let container_name = format!(
        "synctv-redis-dedicated-{}-{}",
        current_process_id(),
        sanitize_container_name(
            &std::env::var("NEXTEST_TEST_NAME")
                .ok()
                .or_else(|| std::thread::current().name().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_string()),
            "redis-test"
        )
    );
    let image_descriptor = named_redis_request(&container_name).descriptor();
    ensure_docker_image(&image_descriptor, docker_startup_timeout())
        .await
        .unwrap_or_else(|error| panic!("Failed to prepare Redis image: {error}"));
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
        _container: Some(std::mem::ManuallyDrop::new(container)),
        name: container_name,
        host,
        port,
        _run_lock: run_lock,
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

pub async fn start_redis_handle() -> (RedisContainer, Arc<RwLock<redis::aio::ConnectionManager>>) {
    let (container, redis_url, _client) = start_redis_inner("handle").await;
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client for handle");
    let manager = redis_connection_manager(&client).await;
    (container, Arc::new(RwLock::new(manager)))
}

pub async fn start_redis_url() -> (RedisContainer, String) {
    let (container, redis_url, _client) = start_redis_inner("url").await;
    (container, redis_url)
}

pub async fn redis_connection_manager(client: &redis::Client) -> redis::aio::ConnectionManager {
    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut last_error = String::from("redis connection manager probe has not run yet");
    while std::time::Instant::now() < deadline {
        match redis::aio::ConnectionManager::new(client.clone()).await {
            Ok(mut conn) => {
                let ping: redis::RedisResult<String> =
                    redis::cmd("PING").query_async(&mut conn).await;
                if ping.is_ok() {
                    return conn;
                }
                last_error = format!("connection manager ping failed: {ping:?}");
            }
            Err(err) => {
                last_error = format!("connection manager init failed: {err}");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    panic!(
        "Redis connection manager did not become ready within {:?}: {}",
        docker_startup_timeout(),
        last_error
    );
}

pub async fn redis_multiplexed_connection(
    client: &redis::Client,
) -> redis::aio::MultiplexedConnection {
    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut last_error = String::from("redis multiplexed connection probe has not run yet");
    while std::time::Instant::now() < deadline {
        match client.get_multiplexed_async_connection().await {
            Ok(mut conn) => {
                let ping: redis::RedisResult<String> =
                    redis::cmd("PING").query_async(&mut conn).await;
                if ping.is_ok() {
                    return conn;
                }
                last_error = format!("multiplexed ping failed: {ping:?}");
            }
            Err(err) => {
                last_error = format!("multiplexed init failed: {err}");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    panic!(
        "Redis multiplexed connection did not become ready within {:?}: {}",
        docker_startup_timeout(),
        last_error
    );
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
    use std::net::Ipv4Addr;

    use crate::docker::{
        detect_primary_ipv4_address, docker_rm_force_with_program, is_viable_host_ipv4,
        lock_file_path, run_has_active_lock,
    };

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
        let name = shared_container_name_from(Some("Run.Id/42"));

        assert_eq!(name, "synctv-redis-shared-run-id-42");
    }

    #[tokio::test]
    async fn redis_run_lock_marks_current_run_active() {
        let run_id = current_test_run_id();
        let lock = redis_run_lock(&run_id).await;

        assert!(
            run_has_active_lock("redis", &run_id),
            "held redis run lock must prevent orphan cleanup from treating the run as dead"
        );

        drop(lock);
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
            cmd.windows(2)
                .any(|pair| pair == ["--maxclients", "100000"]),
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
        let candidates = candidate_endpoints_for_host("[::1]", Some(6379), Some(16379));

        assert!(
            candidates.contains(&("[::1]".to_string(), 16379)),
            "IPv6 host candidates should preserve the IPv6 endpoint: {candidates:?}"
        );
        assert!(
            candidates.contains(&("127.0.0.1".to_string(), 6379)),
            "IPv6 host candidates should include loopback IPv4 fallback: {candidates:?}"
        );
        if let Some(local_ipv4) = detect_primary_ipv4_address() {
            assert!(
                candidates.contains(&(local_ipv4, 6379)),
                "IPv6 host candidates should include the primary IPv4 fallback when available: {candidates:?}"
            );
        }
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
    fn lock_file_path_uses_platform_temp_directory() {
        let path = lock_file_path("redis-startup-test");

        assert!(
            path.starts_with(crate::test_temp_dir()),
            "redis lock files should live under the platform temp directory: {path:?}"
        );
    }

    #[test]
    fn open_lock_file_creates_missing_parent_directories() {
        let nested_dir = crate::test_temp_dir().join(format!(
            "synctv-redis-lock-test-{}",
            synctv_common::snanoid!(8).to_lowercase()
        ));
        let path = nested_dir.join("nested").join("redis.lock");

        let file = ProcessLock::open_lock_file(&path);
        drop(file);

        assert!(
            path.exists(),
            "opening a redis lock file should create missing parent directories: {path:?}"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(nested_dir);
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
