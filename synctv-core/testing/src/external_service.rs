use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::{collections::BTreeMap, iter};

use testcontainers::core::{CmdWaitFor, ExecCommand, ImageExt, IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage};
use tokio::sync::Semaphore;

use crate::docker::{
    acquire_docker_slot, acquire_run_lock, cleanup_orphaned_run_lock_files,
    cleanup_orphaned_testcontainers, current_test_run_id as docker_current_test_run_id,
    ensure_docker_image, sanitize_container_name, TEST_RUN_LABEL,
};
use crate::postgres::{docker_startup_parallelism, docker_startup_timeout};

static EXTERNAL_SERVICE_START_SERIALIZER: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(docker_startup_parallelism()));
static EXTERNAL_SERVICE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Configuration for a generic external-service test container.
pub struct ExternalServiceRequest {
    service: String,
    container_prefix: String,
    image: String,
    tag: String,
    internal_port: u16,
    additional_ports: Vec<u16>,
    stdout_ready_message: Option<String>,
    user: Option<String>,
    env: Vec<(String, String)>,
    copied_files: Vec<(String, Vec<u8>)>,
    post_start_shell_commands: Vec<String>,
}

impl ExternalServiceRequest {
    #[must_use]
    pub fn new(
        service: impl Into<String>,
        container_prefix: impl Into<String>,
        image: impl Into<String>,
        tag: impl Into<String>,
        internal_port: u16,
    ) -> Self {
        Self {
            service: service.into(),
            container_prefix: container_prefix.into(),
            image: image.into(),
            tag: tag.into(),
            internal_port,
            additional_ports: Vec::new(),
            stdout_ready_message: None,
            user: None,
            env: Vec::new(),
            copied_files: Vec::new(),
            post_start_shell_commands: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_exposed_port(mut self, port: u16) -> Self {
        if port != self.internal_port && !self.additional_ports.contains(&port) {
            self.additional_ports.push(port);
        }
        self
    }

    #[must_use]
    pub fn with_stdout_ready_message(mut self, message: impl Into<String>) -> Self {
        self.stdout_ready_message = Some(message.into());
        self
    }

    #[must_use]
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn with_copy_to(mut self, path: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
        self.copied_files.push((path.into(), contents.into()));
        self
    }

    #[must_use]
    pub fn with_post_start_shell_command(mut self, command: impl Into<String>) -> Self {
        self.post_start_shell_commands.push(command.into());
        self
    }
}

/// Running external-service test container.
pub struct ExternalServiceContainer {
    container: ContainerAsync<GenericImage>,
    _run_lock: crate::docker::ProcessLock,
    host: String,
    port: u16,
    mapped_ports: BTreeMap<u16, u16>,
}

impl ExternalServiceContainer {
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn mapped_port(&self, internal_port: u16) -> Option<u16> {
        self.mapped_ports.get(&internal_port).copied()
    }

    #[must_use]
    pub fn http_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub async fn logs(&self) -> anyhow::Result<String> {
        let mut output = self.container.stdout_to_vec().await?;
        output.extend_from_slice(&self.container.stderr_to_vec().await?);
        String::from_utf8(output).map_err(Into::into)
    }
}

fn current_test_run_id(service: &str) -> String {
    docker_current_test_run_id(&format!("{service}-test"))
}

fn next_container_name(prefix: &str, service: &str, run_id: &str) -> String {
    let sequence = EXTERNAL_SERVICE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let thread = std::thread::current()
        .name()
        .map_or_else(|| "unknown-test".to_string(), ToString::to_string);
    let component = sanitize_container_name(&thread, service);
    format!(
        "{prefix}{run_id}-pid{}-{sequence}-{component}",
        std::process::id()
    )
}

async fn acquire_external_service_start_slot(service: &str) -> crate::docker::DockerSlotGuard {
    acquire_docker_slot(
        &EXTERNAL_SERVICE_START_SERIALIZER,
        docker_startup_parallelism(),
        &format!("{service}-external-service-start"),
        "external-service startup guard should not be closed",
        "external-service process slot task should not panic",
    )
    .await
}

/// Start a generic external-service test container with the shared Docker test policy.
pub async fn start_external_service(request: ExternalServiceRequest) -> ExternalServiceContainer {
    let service = sanitize_container_name(&request.service, "external-service");
    let container_prefix = sanitize_container_name(&request.container_prefix, &service);
    let run_id = current_test_run_id(&service);
    let run_lock = acquire_run_lock(&service, &run_id);
    cleanup_orphaned_testcontainers(&container_prefix, &service, &run_id);
    cleanup_orphaned_run_lock_files(&format!("synctv-{service}-run-"));
    cleanup_orphaned_run_lock_files(&format!("synctv-{service}-startup-"));

    let container_name = next_container_name(&container_prefix, &service, &run_id);
    let _slot = acquire_external_service_start_slot(&service).await;

    let image_descriptor = format!("{}:{}", request.image, request.tag);
    ensure_docker_image(&image_descriptor, docker_startup_timeout())
        .await
        .unwrap_or_else(|error| panic!("Failed to prepare {service} image: {error}"));

    let mut image = GenericImage::new(request.image, request.tag);

    if let Some(message) = request.stdout_ready_message {
        image = image.with_wait_for(WaitFor::message_on_stdout(message));
    }

    let internal_ports = iter::once(request.internal_port)
        .chain(request.additional_ports.iter().copied())
        .collect::<Vec<_>>();
    image = image.with_exposed_port(request.internal_port.tcp());
    for port in &request.additional_ports {
        image = image.with_exposed_port(port.tcp());
    }
    let mut container_request = image
        .with_container_name(container_name.clone())
        .with_label(TEST_RUN_LABEL, run_id);
    if let Some(user) = request.user {
        container_request = container_request.with_user(user);
    }
    for (key, value) in request.env {
        container_request = container_request.with_env_var(key, value);
    }
    for (path, contents) in request.copied_files {
        container_request = container_request.with_copy_to(path, contents);
    }

    let container = tokio::time::timeout(docker_startup_timeout(), container_request.start())
        .await
        .unwrap_or_else(|elapsed| {
            panic!(
                "Docker container startup timed out after {:?}: {elapsed} (is Docker running?)",
                docker_startup_timeout(),
            )
        })
        .unwrap_or_else(|error| {
            panic!("Failed to start {service} container {container_name}: {error}")
        });

    for command in request.post_start_shell_commands {
        container
            .exec(
                ExecCommand::new(["sh", "-c", command.as_str()])
                    .with_cmd_ready_condition(CmdWaitFor::exit_code(0)),
            )
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "Failed to run post-start command in {service} container {container_name}: {error}"
                )
            });
    }

    let mut mapped_ports = BTreeMap::new();
    for internal_port in internal_ports {
        let mapped_port = container
            .get_host_port_ipv4(internal_port.tcp())
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "Failed to resolve {service} container {container_name} port {internal_port}: {error}"
                )
            });
        mapped_ports.insert(internal_port, mapped_port);
    }
    let port = mapped_ports[&request.internal_port];

    ExternalServiceContainer {
        container,
        _run_lock: run_lock,
        host: "127.0.0.1".to_string(),
        port,
        mapped_ports,
    }
}
