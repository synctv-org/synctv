use std::fs::{File, OpenOptions};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

use tokio::sync::{Semaphore, SemaphorePermit};

pub(crate) const TEST_RUN_LABEL: &str = "synctv.test.run_id";

pub(crate) struct ProcessLock(File);

pub(crate) struct DockerSlotGuard {
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
