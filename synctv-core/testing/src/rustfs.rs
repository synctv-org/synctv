use std::net::IpAddr;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use synctv_core::service::{S3CompatibleFileStorageService, S3FileStorageConfig};
use testcontainers::core::{ImageExt, IntoContainerPort, ReuseDirective, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::rustfs::RustFS;
use tokio::sync::{OnceCell, Semaphore};

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

const RUSTFS_API_PORT: u16 = 9000;
pub const RUSTFS_ACCESS_KEY: &str = "rustfsadmin";
pub const RUSTFS_SECRET_KEY: &str = "rustfsadmin";
pub const RUSTFS_REGION: &str = "us-east-1";
const DEFAULT_RUSTFS_ACTIVE_PARALLELISM: usize = 32;
const MIN_RUSTFS_ACTIVE_PARALLELISM: usize = 1;
const RUSTFS_ACTIVE_PARALLELISM_ENV: &str = "SYNCTV_TEST_RUSTFS_ACTIVE_PARALLELISM";
const DEFAULT_RUSTFS_TAG: &str = "latest-glibc";

static RUSTFS_START_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(docker_startup_parallelism()));
static RUSTFS_ACTIVE_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(rustfs_active_parallelism()));
static SHARED_RUSTFS: OnceCell<Arc<SharedRustfsServer>> = OnceCell::const_new();
static RUSTFS_RUN_LOCK: OnceCell<Arc<ProcessLock>> = OnceCell::const_new();
static RUSTFS_BUCKET_COUNTER: AtomicU64 = AtomicU64::new(1);

struct SharedRustfsServer {
    _container: Option<std::mem::ManuallyDrop<ContainerAsync<RustFS>>>,
    name: String,
    host: String,
    port: u16,
    _run_lock: Arc<ProcessLock>,
}

pub struct RustfsContainer {
    shared: Arc<SharedRustfsServer>,
    bucket: String,
    cleaned_up: bool,
}

#[derive(Debug, Clone)]
pub struct RustfsS3Config {
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub region: String,
}

fn rustfs_active_parallelism() -> usize {
    rustfs_active_parallelism_from(std::env::var(RUSTFS_ACTIVE_PARALLELISM_ENV).ok().as_deref())
}

fn rustfs_active_parallelism_from(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(DEFAULT_RUSTFS_ACTIVE_PARALLELISM, |slots| {
            slots.max(MIN_RUSTFS_ACTIVE_PARALLELISM)
        })
}

fn current_process_id() -> u32 {
    docker_current_process_id()
}

fn current_test_run_id() -> String {
    docker_current_test_run_id("rustfs-test")
}

fn current_test_run_id_from(run_id: Option<&str>) -> String {
    docker_current_test_run_id_from(run_id, "rustfs-test")
}

fn shared_container_name() -> String {
    shared_container_name_from(std::env::var("NEXTEST_RUN_ID").ok().as_deref())
}

fn shared_container_name_from(run_id: Option<&str>) -> String {
    format!("synctv-rustfs-shared-{}", current_test_run_id_from(run_id))
}

fn named_rustfs_request(container_name: &str) -> testcontainers::ContainerRequest<RustFS> {
    RustFS::default()
        .with_tag(DEFAULT_RUSTFS_TAG)
        .with_container_name(container_name.to_string())
        .with_label(TEST_RUN_LABEL, current_test_run_id())
        .with_reuse(ReuseDirective::Always)
        .with_ready_conditions(Vec::<WaitFor>::new())
}

fn format_socket_host(host: &str) -> String {
    if matches!(host_address_family(host), Some(IpAddr::V6(_))) && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn endpoint_url(host: &str, port: u16) -> String {
    format!("http://{}:{port}", format_socket_host(host))
}

async fn resolve_host_port(
    container: &ContainerAsync<RustFS>,
    internal_port: u16,
) -> (String, u16) {
    let host = container
        .get_host()
        .await
        .expect("Failed to get RustFS host")
        .to_string();

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let endpoints = loop {
        let ports = container
            .ports()
            .await
            .expect("Failed to inspect RustFS port mappings");
        let endpoints = candidate_endpoints_for_host(
            &host,
            ports.map_to_host_port_ipv4(internal_port.tcp()),
            ports.map_to_host_port_ipv6(internal_port.tcp()),
        );
        if !endpoints.is_empty() {
            break endpoints;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Failed to resolve RustFS endpoint for host {host} within 30 seconds"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };

    wait_for_any_rustfs_endpoint(&endpoints).await
}

async fn wait_for_any_rustfs_endpoint(endpoints: &[(String, u16)]) -> (String, u16) {
    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut last_error = String::from("RustFS readiness probe has not run yet");
    while std::time::Instant::now() < deadline {
        for (host, port) in endpoints {
            match wait_for_rustfs_endpoint_once(host, *port).await {
                Ok(()) => return (host.clone(), *port),
                Err(error) => last_error = error,
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    panic!(
        "RustFS container did not become reachable within {:?} across endpoints {:?}: {}",
        docker_startup_timeout(),
        endpoints,
        last_error
    );
}

async fn wait_for_rustfs_endpoint_once(host: &str, port: u16) -> Result<(), String> {
    tokio::net::TcpStream::connect((host, port))
        .await
        .map_err(|error| {
            format!(
                "TCP connect failed for {}: {error}",
                endpoint_url(host, port)
            )
        })?;
    Ok(())
}

async fn resolve_existing_named_rustfs_endpoint(container_name: &str) -> Option<(String, u16)> {
    if !docker_named_container_belongs_to_current_run(container_name, &current_test_run_id()) {
        return None;
    }

    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut last_error = String::from("docker port has not returned a RustFS endpoint yet");
    while std::time::Instant::now() < deadline {
        if let Some(candidates) = docker_port_candidates(container_name, RUSTFS_API_PORT) {
            return Some(wait_for_any_rustfs_endpoint(&candidates).await);
        }
        last_error = format!("docker port returned no endpoint for {container_name}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    panic!(
        "Existing RustFS container {container_name} did not become reachable within {:?}: {last_error}",
        docker_startup_timeout()
    );
}

impl SharedRustfsServer {
    fn endpoint(&self) -> String {
        endpoint_url(&self.host, self.port)
    }

    fn s3_config(&self, bucket: &str) -> RustfsS3Config {
        RustfsS3Config {
            endpoint: self.endpoint(),
            access_key_id: RUSTFS_ACCESS_KEY.to_string(),
            secret_access_key: RUSTFS_SECRET_KEY.to_string(),
            bucket: bucket.to_string(),
            region: RUSTFS_REGION.to_string(),
        }
    }
}

impl RustfsContainer {
    fn new(shared: Arc<SharedRustfsServer>, bucket: String) -> Self {
        Self {
            shared,
            bucket,
            cleaned_up: false,
        }
    }

    pub fn cleanup(mut self) {
        self.cleaned_up = true;
    }

    pub fn terminate(mut self) {
        let warning = handle_cleanup_result(
            &mut self.cleaned_up,
            &self.shared.name,
            docker_rm_force(&self.shared.name),
            "rustfs",
            docker_rm_force,
        );
        log_cleanup_warning_if_needed(warning);
    }

    pub fn id(&self) -> String {
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

    pub fn endpoint(&self) -> String {
        self.shared.endpoint()
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }
}

impl Drop for RustfsContainer {
    fn drop(&mut self) {
        self.cleaned_up = true;
    }
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
        Err(error) if cleanup_error_indicates_missing_container(&error) => {
            *cleaned_up = true;
            Some(format!(
                "warning: {kind} test container {container_name} was already removed before explicit cleanup completed: {error}"
            ))
        }
        Err(error) => match fallback_remove(container_name) {
            Ok(()) => {
                *cleaned_up = true;
                Some(format!(
                    "warning: explicit removal for {kind} test container {container_name} failed; fallback `docker rm -f` succeeded: {error}"
                ))
            }
            Err(fallback_error) if cleanup_error_indicates_missing_container(&fallback_error) => {
                *cleaned_up = true;
                Some(format!(
                    "warning: explicit removal for {kind} test container {container_name} reported an error, but fallback confirmed it was already gone: {error}; fallback: {fallback_error}"
                ))
            }
            Err(fallback_error) => Some(format!(
                "warning: failed to remove {kind} test container {container_name} during explicit cleanup: {error}; fallback `docker rm -f` also failed: {fallback_error}"
            )),
        },
    }
}

fn log_cleanup_warning_if_needed(warning: Option<String>) {
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
}

async fn rustfs_run_lock(run_id: &str) -> Arc<ProcessLock> {
    let run_id = run_id.to_string();
    Arc::clone(
        RUSTFS_RUN_LOCK
            .get_or_init(|| async move { Arc::new(acquire_run_lock("rustfs", &run_id)) })
            .await,
    )
}

async fn acquire_docker_start_slot(name: &str) -> DockerSlotGuard {
    acquire_docker_slot(
        &RUSTFS_START_SERIALIZER,
        docker_startup_parallelism(),
        name,
        "RustFS startup guard should not be closed",
        "RustFS process slot task should not panic",
    )
    .await
}

async fn acquire_docker_active_slot(name: &str) -> DockerSlotGuard {
    acquire_docker_slot(
        &RUSTFS_ACTIVE_SERIALIZER,
        rustfs_active_parallelism(),
        name,
        "RustFS active-container guard should not be closed",
        "RustFS active container slot task should not panic",
    )
    .await
}

async fn init_shared_rustfs_server() -> SharedRustfsServer {
    let run_id = current_test_run_id();
    let run_lock = rustfs_run_lock(&run_id).await;
    cleanup_orphaned_testcontainers("synctv-rustfs-", "rustfs", &run_id);
    cleanup_orphaned_run_lock_files("synctv-rustfs-run-");

    let lock_name = format!("rustfs-run-{run_id}");
    let _startup_lock = tokio::task::spawn_blocking(move || loop {
        if let Some(lock) = ProcessLock::try_acquire(&lock_name) {
            return lock;
        }
        std::thread::sleep(Duration::from_millis(100));
    })
    .await
    .expect("RustFS startup lock task should not panic");

    let container_name = shared_container_name();
    let active_slot = acquire_docker_active_slot("rustfs-active").await;
    let (container, host, port) = {
        let _rustfs_process_lock = acquire_docker_start_slot("rustfs-start").await;
        if let Some((host, port)) = resolve_existing_named_rustfs_endpoint(&container_name).await {
            (None, host, port)
        } else {
            let image_descriptor = named_rustfs_request(&container_name).descriptor();
            ensure_docker_image(&image_descriptor, docker_startup_timeout())
                .await
                .unwrap_or_else(|error| panic!("Failed to prepare RustFS image: {error}"));
            let start_deadline = std::time::Instant::now() + docker_startup_timeout();
            let mut last_start_error;
            loop {
                match tokio::time::timeout(
                    docker_startup_timeout(),
                    named_rustfs_request(&container_name).start(),
                )
                .await
                {
                    Ok(Ok(container)) => {
                        let (host, port) = resolve_host_port(&container, RUSTFS_API_PORT).await;
                        break (Some(std::mem::ManuallyDrop::new(container)), host, port);
                    }
                    Ok(Err(error)) => {
                        let error = format!("{error}");
                        if startup_error_is_named_container_conflict(&error) {
                            if let Some((host, port)) =
                                resolve_existing_named_rustfs_endpoint(&container_name).await
                            {
                                break (None, host, port);
                            }
                        }
                        if startup_error_is_retriable(&error) {
                            last_start_error = error;
                            assert!(
                                std::time::Instant::now() < start_deadline,
                                "Failed to start RustFS after retries: {last_start_error}"
                            );
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            continue;
                        }
                        panic!("Failed to start RustFS: {error}");
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
    let server = SharedRustfsServer {
        _container: container,
        name: container_name,
        host,
        port,
        _run_lock: run_lock,
    };
    drop(active_slot);
    server
}

async fn shared_rustfs_server() -> Arc<SharedRustfsServer> {
    synctv_core::install_process_crypto_provider();
    Arc::clone(
        SHARED_RUSTFS
            .get_or_init(|| async { Arc::new(init_shared_rustfs_server().await) })
            .await,
    )
}

pub async fn start_rustfs() -> (RustfsContainer, RustfsS3Config) {
    let bucket = test_rustfs_bucket_name("default");
    start_rustfs_bucket(&bucket).await
}

async fn start_rustfs_bucket(bucket: &str) -> (RustfsContainer, RustfsS3Config) {
    let shared = shared_rustfs_server().await;
    let bucket = bucket.trim();
    ensure_bucket_name(&shared, bucket).await;
    let config = shared.s3_config(bucket);
    (RustfsContainer::new(shared, bucket.to_string()), config)
}

pub fn test_rustfs_base_path(label: &str) -> String {
    format!(
        "files/{}/{}/pid{}",
        current_test_run_id(),
        sanitize_label(label),
        current_process_id()
    )
}

pub fn test_rustfs_bucket_name(label: &str) -> String {
    let counter = RUSTFS_BUCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
    let test_label = current_test_bucket_label();
    let middle = sanitize_bucket_component(
        &format!("{}-{}-{}", current_test_run_id(), test_label, label),
        "test",
    );
    let suffix = format!("p{}-{counter}", current_process_id());
    let max_middle_len = 63usize.saturating_sub("synctv".len() + suffix.len() + 2);
    let middle = truncate_bucket_component(&middle, max_middle_len);
    sanitize_bucket_name(&format!("synctv-{middle}-{suffix}"))
}

fn current_test_bucket_label() -> String {
    std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::thread::current().name().map(str::to_owned))
        .unwrap_or_else(|| "unknown-test".to_string())
}

fn sanitize_label(raw: &str) -> String {
    sanitize_container_name(raw, "rustfs-test")
}

fn sanitize_bucket_component(raw: &str, fallback: &str) -> String {
    let mut value = String::with_capacity(raw.len());
    let mut last_dash = false;
    for ch in raw.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if normalized == '-' {
            if !last_dash {
                value.push(normalized);
            }
            last_dash = true;
        } else {
            value.push(normalized);
            last_dash = false;
        }
    }
    let value = value.trim_matches('-').to_string();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

fn truncate_bucket_component(value: &str, max_len: usize) -> String {
    let mut truncated = value
        .chars()
        .take(max_len.max(1))
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if truncated.is_empty() {
        truncated.push_str("test");
    }
    truncated
}

fn sanitize_bucket_name(raw: &str) -> String {
    let mut name = sanitize_bucket_component(raw, "synctv-test");
    name.truncate(63);
    name = name.trim_matches('-').to_string();
    while name.len() < 3 {
        name.push('0');
    }
    name
}

async fn ensure_bucket_name(server: &SharedRustfsServer, bucket: &str) {
    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut last_error = String::from("bucket creation has not run yet");
    while std::time::Instant::now() < deadline {
        match create_bucket(server, bucket).await {
            Ok(()) => return,
            Err(error) => {
                last_error = error;
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
    panic!(
        "RustFS bucket {bucket} was not ready within {:?}: {last_error}",
        docker_startup_timeout()
    );
}

async fn create_bucket(server: &SharedRustfsServer, bucket: &str) -> Result<(), String> {
    let bucket = bucket.trim();
    if bucket.is_empty() {
        return Err("bucket name must not be empty".to_string());
    }
    S3CompatibleFileStorageService::ensure_test_bucket(&S3FileStorageConfig {
        endpoint: server.endpoint(),
        access_key_id: RUSTFS_ACCESS_KEY.to_string(),
        secret_access_key: RUSTFS_SECRET_KEY.to_string(),
        bucket: bucket.to_string(),
        region: RUSTFS_REGION.to_string(),
        base_path: String::new(),
        public_base_url: None,
        upload_expires_seconds: 900,
        storage_backend: "rustfs_test_setup".to_string(),
        upload_token_secret: "rustfs-test-setup".to_string(),
    })
    .await
    .map_err(|error| format!("failed to create RustFS bucket {bucket}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustfs_active_parallelism_defaults_to_shared_container_limit() {
        assert_eq!(
            rustfs_active_parallelism_from(None),
            DEFAULT_RUSTFS_ACTIVE_PARALLELISM
        );
        assert_eq!(DEFAULT_RUSTFS_ACTIVE_PARALLELISM, 32);
    }

    #[test]
    fn rustfs_active_parallelism_honors_valid_override() {
        assert_eq!(rustfs_active_parallelism_from(Some("7")), 7);
    }

    #[test]
    fn rustfs_active_parallelism_rejects_zero_override() {
        assert_eq!(
            rustfs_active_parallelism_from(Some("0")),
            MIN_RUSTFS_ACTIVE_PARALLELISM
        );
    }

    #[test]
    fn shared_container_name_uses_nextest_run_id_when_present() {
        let name = shared_container_name_from(Some("Run.Id/42"));

        assert_eq!(name, "synctv-rustfs-shared-run-id-42");
    }

    #[test]
    fn named_rustfs_request_uses_fixed_glibc_image() {
        let request = named_rustfs_request("synctv-rustfs-test");

        assert_eq!(request.descriptor(), "rustfs/rustfs:latest-glibc");
        assert!(
            request.ready_conditions().is_empty(),
            "RustFS tests use explicit TCP and S3 bucket readiness probes"
        );
    }

    #[test]
    fn test_rustfs_bucket_name_is_unique_and_s3_compatible() {
        let first = test_rustfs_bucket_name("bucket isolation with a long label");
        let second = test_rustfs_bucket_name("bucket isolation with a long label");

        assert_ne!(first, second);
        for name in [first, second] {
            assert!(name.starts_with("synctv-"));
            assert!(name.len() <= 63, "bucket name is too long: {name}");
            assert!(
                name.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'),
                "bucket name contains invalid characters: {name}"
            );
        }
    }
}
