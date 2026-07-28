use std::fs::{File, OpenOptions};
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};
use testcontainers::core::client::docker_client_instance;
use testcontainers::runners::AsyncRunner;
use testcontainers::GenericImage;
use tokio::sync::{Semaphore, SemaphorePermit};

pub(crate) const TEST_RUN_LABEL: &str = "synctv.test.run_id";
const DOCKER_IMAGE_PULL_LOCK_PREFIX: &str = "docker-image-pull";
const IMAGE_PULL_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);
const IMAGE_PULL_RETRY_MAX_DELAY: Duration = Duration::from_secs(10);

pub struct ProcessLock(File);

pub struct DockerSlotGuard {
    _local_permit: SemaphorePermit<'static>,
    _process_lock: ProcessLock,
}

impl ProcessLock {
    pub(crate) fn try_acquire(name: &str) -> Option<Self> {
        let path = lock_file_path(name);
        Self::try_acquire_path(&path)
    }

    pub(crate) fn try_acquire_path(path: &Path) -> Option<Self> {
        let file = Self::open_lock_file(path);
        match file.try_lock() {
            Ok(()) => Some(Self(file)),
            Err(_) => None,
        }
    }

    pub(crate) fn open_lock_file(path: &Path) -> File {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| {
                panic!(
                    "failed to create lock file directory {}: {e}",
                    parent.display()
                )
            });
        }
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
            .expect("failed to release process lock for docker test startup");
    }
}

pub(crate) fn lock_file_path(name: &str) -> PathBuf {
    crate::test_temp_dir().join(format!("synctv-{name}.lock"))
}

pub(crate) fn sanitize_container_name(raw: &str, empty_fallback: &str) -> String {
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
        empty_fallback.to_string()
    } else {
        name
    }
}

pub(crate) fn current_process_id() -> u32 {
    std::process::id()
}

pub(crate) fn current_test_run_id(empty_fallback: &str) -> String {
    current_test_run_id_from(
        std::env::var("NEXTEST_RUN_ID").ok().as_deref(),
        empty_fallback,
    )
}

pub(crate) fn current_test_run_id_from(run_id: Option<&str>, empty_fallback: &str) -> String {
    run_id.filter(|value| !value.trim().is_empty()).map_or_else(
        || format!("pid-{}", current_process_id()),
        |value| sanitize_container_name(value, empty_fallback),
    )
}

pub(crate) fn run_lock_file_prefix(service: &str, run_id: &str) -> String {
    format!("synctv-{service}-run-{run_id}-")
}

pub(crate) fn acquire_run_lock(service: &str, run_id: &str) -> ProcessLock {
    let path = crate::test_temp_dir().join(format!(
        "{}{}.lock",
        run_lock_file_prefix(service, run_id),
        current_process_id()
    ));
    ProcessLock::try_acquire_path(&path)
        .unwrap_or_else(|| panic!("failed to acquire {service} run lock for {run_id}"))
}

pub(crate) fn run_has_active_lock(service: &str, run_id: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(crate::test_temp_dir()) else {
        return false;
    };

    let prefix = run_lock_file_prefix(service, run_id);
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with(&prefix) || !file_name.ends_with(".lock") {
            continue;
        }
        if ProcessLock::try_acquire_path(&path).is_none() {
            return true;
        }
    }

    false
}

pub(crate) fn cleanup_error_indicates_missing_container(err: &str) -> bool {
    let err = err.to_ascii_lowercase();
    err.contains("no such container") || err.contains("not found")
}

pub(crate) fn startup_error_is_retriable(err: &str) -> bool {
    let err = err.to_ascii_lowercase();
    err.contains("marked for removal") || err.contains("no such container")
}

pub(crate) fn startup_error_is_named_container_conflict(err: &str) -> bool {
    let err = err.to_ascii_lowercase();
    err.contains("already in use by container")
        || (err.contains("container name") && err.contains("already in use"))
}

pub(crate) fn docker_rm_force(container_ref: &str) -> Result<(), String> {
    docker_rm_force_with_program("docker", container_ref)
}

pub(crate) fn docker_rm_force_with_program(
    program: &str,
    container_ref: &str,
) -> Result<(), String> {
    let args = ["rm", "-v", "-f", container_ref];
    let output = Command::new(program).args(args).output().map_err(|err| {
        format!("failed to spawn `{program}` for `{container_ref}` cleanup: {err}")
    })?;

    if output.status.success() {
        return Ok(());
    }

    Err(format_command_failure(program, &args, &output))
}

pub(crate) fn format_command_failure(
    program: &str,
    args: &[&str],
    output: &std::process::Output,
) -> String {
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

fn docker_image_missing_error(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    normalized.contains("no such image") || normalized.contains("no such object")
}

async fn docker_image_inspect(descriptor: &str) -> Result<bool, String> {
    let client = docker_client_instance()
        .await
        .map_err(|error| format!("failed to connect to Docker: {error}"))?;

    match client.inspect_image(descriptor).await {
        Ok(_) => Ok(true),
        Err(error) if docker_image_missing_error(&error.to_string()) => Ok(false),
        Err(error) => Err(format!(
            "failed to inspect Docker image `{descriptor}`: {error}"
        )),
    }
}

fn image_descriptor_parts(descriptor: &str) -> Result<(&str, &str), String> {
    let Some((name, tag)) = descriptor.rsplit_once(':') else {
        return Err(format!("Docker image descriptor `{descriptor}` has no tag"));
    };
    if name.is_empty() || tag.is_empty() || tag.contains('/') {
        return Err(format!("Docker image descriptor `{descriptor}` is invalid"));
    }
    Ok((name, tag))
}

async fn docker_pull_image(descriptor: &str) -> Result<(), String> {
    let (name, tag) = image_descriptor_parts(descriptor)?;
    GenericImage::new(name, tag)
        .pull_image()
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to pull Docker image `{descriptor}`: {error}"))
}

async fn run_until_deadline<T>(
    deadline: Instant,
    operation: impl Future<Output = Result<T, String>>,
    timeout_message: String,
) -> Result<T, String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(timeout_message);
    }
    tokio::time::timeout(remaining, operation)
        .await
        .map_err(|_| timeout_message)?
}

fn image_pull_error_is_rate_limited(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    normalized.contains("toomanyrequests")
        || normalized.contains("too many requests")
        || (normalized.contains("oauth token") && normalized.contains("429"))
}

fn docker_image_pull_lock_name(descriptor: &str) -> String {
    let digest = Sha256::digest(descriptor.as_bytes());
    format!("{DOCKER_IMAGE_PULL_LOCK_PREFIX}-{}", hex::encode(digest))
}

fn acquire_image_pull_lock(
    name: &str,
    deadline: Instant,
    timeout: Duration,
) -> Result<ProcessLock, String> {
    loop {
        if let Some(lock) = ProcessLock::try_acquire(name) {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {timeout:?} waiting to prepare Docker image"
            ));
        }
        std::thread::sleep(IMAGE_PULL_LOCK_POLL_INTERVAL);
    }
}

pub(crate) async fn ensure_docker_image(descriptor: &str, timeout: Duration) -> Result<(), String> {
    let descriptor = descriptor.to_string();
    let deadline = Instant::now() + timeout;
    let timeout_message =
        format!("timed out after {timeout:?} preparing Docker image '{descriptor}'");

    if run_until_deadline(
        deadline,
        docker_image_inspect(&descriptor),
        timeout_message.clone(),
    )
    .await?
    {
        return Ok(());
    }

    let pull_lock_name = docker_image_pull_lock_name(&descriptor);
    let lock_timeout = timeout;
    let lock_deadline = deadline;
    let pull_lock_task = tokio::task::spawn_blocking(move || {
        acquire_image_pull_lock(&pull_lock_name, lock_deadline, lock_timeout)
    });
    let _pull_lock = run_until_deadline(
        deadline,
        async move {
            pull_lock_task
                .await
                .map_err(|error| format!("Docker image pull lock task failed: {error}"))?
        },
        timeout_message.clone(),
    )
    .await?;

    // Another cargo/nextest process may have populated the image while this
    // process waited for the cross-process pull lock.
    if run_until_deadline(
        deadline,
        docker_image_inspect(&descriptor),
        timeout_message.clone(),
    )
    .await?
    {
        return Ok(());
    }

    let mut retry_delay = Duration::from_secs(1);
    loop {
        match run_until_deadline(
            deadline,
            docker_pull_image(&descriptor),
            timeout_message.clone(),
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) if image_pull_error_is_rate_limited(&error) && Instant::now() < deadline => {
                eprintln!(
                    "warning: Docker registry rate-limited image pull for {descriptor}, retrying: {error}"
                );
                let remaining = deadline.saturating_duration_since(Instant::now());
                tokio::time::sleep(retry_delay.min(remaining)).await;
                retry_delay = (retry_delay * 2).min(IMAGE_PULL_RETRY_MAX_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn cleanup_orphaned_testcontainers(
    container_prefix: &str,
    service: &str,
    current_run_id: &str,
) {
    let output = Command::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("name=^{container_prefix}"),
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
        if run_has_active_lock(service, &run_id) {
            continue;
        }

        if let Err(err) = docker_rm_force(container_id) {
            eprintln!(
                "warning: failed to remove orphaned {service} test container {container_id}: {err}"
            );
        }
    }
}

pub(crate) fn cleanup_orphaned_run_lock_files(prefix: &str) {
    let Ok(entries) = std::fs::read_dir(crate::test_temp_dir()) else {
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

pub(crate) fn docker_named_container_belongs_to_current_run(
    container_name: &str,
    current_run_id: &str,
) -> bool {
    let output = Command::new("docker")
        .args([
            "inspect",
            container_name,
            "--format",
            &format!("{{{{index .Config.Labels \"{TEST_RUN_LABEL}\"}}}}"),
        ])
        .output();

    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    String::from_utf8_lossy(&output.stdout).trim() == current_run_id
}

pub(crate) fn host_address_family(host: &str) -> Option<IpAddr> {
    let normalized = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    normalized.parse::<IpAddr>().ok()
}

pub(crate) fn detect_primary_ipv4_address() -> Option<String> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if is_viable_host_ipv4(ip) => Some(ip.to_string()),
        _ => None,
    }
}

pub(crate) const fn is_viable_host_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || (a == 198 && matches!(b, 18 | 19)))
}

pub(crate) fn push_candidate(candidates: &mut Vec<(String, u16)>, host: String, port: u16) {
    if !candidates
        .iter()
        .any(|(existing_host, existing_port)| existing_host == &host && *existing_port == port)
    {
        candidates.push((host, port));
    }
}

pub(crate) fn candidate_endpoints_for_host(
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

pub(crate) fn docker_port_line_candidates(line: &str) -> Vec<(String, u16)> {
    let Some((raw_host, raw_port)) = line.trim().rsplit_once(':') else {
        return Vec::new();
    };
    let Ok(port) = raw_port.parse::<u16>() else {
        return Vec::new();
    };
    let host = raw_host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(raw_host);

    let mut candidates = Vec::new();
    match host {
        "0.0.0.0" => {
            push_candidate(&mut candidates, "127.0.0.1".to_string(), port);
            if let Some(local_ipv4) = detect_primary_ipv4_address() {
                push_candidate(&mut candidates, local_ipv4, port);
            }
        }
        "::" => {
            push_candidate(&mut candidates, "::1".to_string(), port);
            push_candidate(&mut candidates, "127.0.0.1".to_string(), port);
            if let Some(local_ipv4) = detect_primary_ipv4_address() {
                push_candidate(&mut candidates, local_ipv4, port);
            }
        }
        "" => {}
        _ => push_candidate(&mut candidates, host.to_string(), port),
    }
    candidates
}

pub(crate) fn docker_port_candidates(
    container_name: &str,
    internal_port: u16,
) -> Option<Vec<(String, u16)>> {
    let output = Command::new("docker")
        .args(["port", container_name, &format!("{internal_port}/tcp")])
        .output();

    let Ok(output) = output else {
        return None;
    };
    if !output.status.success() {
        return None;
    }

    let mut candidates = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        for (host, port) in docker_port_line_candidates(line) {
            push_candidate(&mut candidates, host, port);
        }
    }

    if candidates.is_empty() {
        None
    } else {
        Some(candidates)
    }
}

pub(crate) async fn acquire_docker_slot(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_pull_rate_limit_detection_matches_docker_hub_errors() {
        assert!(image_pull_error_is_rate_limited(
            "failed to fetch oauth token: unexpected status 429 Too Many Requests"
        ));
        assert!(image_pull_error_is_rate_limited(
            "error from registry: TOOMANYREQUESTS"
        ));
    }

    #[test]
    fn image_pull_rate_limit_detection_rejects_authentication_errors() {
        assert!(!image_pull_error_is_rate_limited(
            "failed to fetch oauth token: unexpected status 401 Unauthorized"
        ));
    }

    #[test]
    fn image_pull_lock_name_is_stable_for_the_same_descriptor() {
        let first = docker_image_pull_lock_name("postgres:18");
        let second = docker_image_pull_lock_name("postgres:18");

        assert_eq!(first, second);
        assert!(first.starts_with("docker-image-pull-"));
    }

    #[test]
    fn image_pull_lock_name_distinguishes_images_and_tags() {
        let postgres = docker_image_pull_lock_name("postgres:18");
        let redis = docker_image_pull_lock_name("redis:8");
        let older_postgres = docker_image_pull_lock_name("postgres:17");

        assert_ne!(postgres, redis);
        assert_ne!(postgres, older_postgres);
    }

    #[test]
    fn image_descriptor_parts_support_registry_ports() {
        assert_eq!(
            image_descriptor_parts("registry.example:5000/synctv/postgres:18"),
            Ok(("registry.example:5000/synctv/postgres", "18"))
        );
    }

    #[test]
    fn image_descriptor_parts_reject_missing_or_invalid_tags() {
        assert!(image_descriptor_parts("postgres").is_err());
        assert!(image_descriptor_parts("postgres:").is_err());
        assert!(image_descriptor_parts(":18").is_err());
    }
}
