use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tonic::transport::Server;
use tracing::info;
#[cfg(unix)]
use tracing::warn;

use synctv_api::impls::{
    AdminApiImpl, AlistApiImpl, BilibiliApiImpl, ClientApiImpl, EmbyApiImpl, ProviderCommonApiImpl,
};
use synctv_core::{
    config::absolute_display_path, config::ManagementTransport, service::UserService, Config,
};

use crate::lifecycle::ManagementLifecycleController;
use crate::proto::management_service_server::ManagementServiceServer;
use crate::service::{ManagementServiceDependencies, ManagementServiceImpl};
use crate::FILE_DESCRIPTOR_SET;

pub struct ManagementServerConfig {
    pub config: Arc<Config>,
    pub user_service: Arc<UserService>,
    pub admin_api: Arc<AdminApiImpl>,
    pub provider_common_api: Arc<ProviderCommonApiImpl>,
    pub client_api: Arc<ClientApiImpl>,
    pub alist_api: Arc<AlistApiImpl>,
    pub bilibili_api: Arc<BilibiliApiImpl>,
    pub emby_api: Arc<EmbyApiImpl>,
    pub proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    pub cluster_client: Option<Arc<synctv_cluster::grpc::ClusterClient>>,
    pub node_id: String,
    pub lifecycle_controller: Arc<ManagementLifecycleController>,
    pub shutdown_rx: watch::Receiver<bool>,
}

pub async fn spawn_management_server(
    config: ManagementServerConfig,
) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
    match config.config.management.transport {
        ManagementTransport::Tcp => spawn_management_tcp_server(config).await,
        ManagementTransport::Unix => {
            #[cfg(unix)]
            {
                spawn_management_unix_server(config)
            }

            #[cfg(not(unix))]
            {
                spawn_management_unix_server(&config)
            }
        }
    }
}

async fn spawn_management_tcp_server(
    config: ManagementServerConfig,
) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
    let bind_target = config.config.management_bind_target();
    let listener = tokio::net::TcpListener::bind(&bind_target)
        .await
        .with_context(|| format!("failed to bind management TCP address {bind_target}"))?;
    info!("Management gRPC server listening on {}", bind_target);

    let handle = tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        serve_management(config, incoming, None).await
    });

    Ok(handle)
}

#[cfg(unix)]
fn spawn_management_unix_server(
    config: ManagementServerConfig,
) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
    let socket_path = config.config.management.unix_socket_path.clone();
    prepare_management_unix_socket(&socket_path)?;
    let listener = tokio::net::UnixListener::bind(&socket_path).with_context(|| {
        format!(
            "failed to bind management unix socket {}",
            absolute_display_path(Path::new(&socket_path))
        )
    })?;
    restrict_management_unix_socket_permissions(Path::new(&socket_path))?;
    info!(
        "Management gRPC server listening on unix://{}",
        absolute_display_path(Path::new(&socket_path))
    );

    let handle = tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::UnixListenerStream::new(listener);
        let result = serve_management(config, incoming, Some(socket_path.clone())).await;
        cleanup_management_unix_socket(&socket_path);
        result
    });

    Ok(handle)
}

#[cfg(not(unix))]
fn spawn_management_unix_server(
    _: &ManagementServerConfig,
) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
    Err(anyhow::anyhow!(
        "management.transport=unix is not supported on this platform"
    ))
}

async fn serve_management<I, IO>(
    config: ManagementServerConfig,
    incoming: I,
    unix_socket_path: Option<String>,
) -> anyhow::Result<()>
where
    I: futures::Stream<Item = Result<IO, std::io::Error>> + Send + 'static,
    IO: tonic::transport::server::Connected + AsyncReadWrite + Unpin + Send + 'static,
{
    let management_service =
        ManagementServiceServer::new(ManagementServiceImpl::new(ManagementServiceDependencies {
            config: Arc::clone(&config.config),
            user_service: config.user_service,
            admin_api: config.admin_api,
            provider_common_api: config.provider_common_api,
            client_api: config.client_api,
            alist_api: config.alist_api,
            bilibili_api: config.bilibili_api,
            emby_api: config.emby_api,
            proxy_slice_cache: config.proxy_slice_cache,
            cluster_client: config.cluster_client,
            node_id: config.node_id,
            lifecycle_controller: config.lifecycle_controller,
            management_auth_token: config.config.management.auth_token.clone(),
        }))
        .max_decoding_message_size(config.config.server.grpc_max_message_size_bytes)
        .max_encoding_message_size(config.config.server.grpc_max_message_size_bytes);

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_service_status("", tonic_health::ServingStatus::Serving)
        .await;
    health_reporter
        .set_serving::<ManagementServiceServer<ManagementServiceImpl>>()
        .await;

    let reflection_service = if config.config.management.enable_reflection {
        Some(
            tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
                .build_v1()
                .context("failed to build management gRPC reflection service")?,
        )
    } else {
        None
    };

    let mut shutdown_rx = config.shutdown_rx;
    let graceful = async move {
        let _ = shutdown_rx.changed().await;
    };

    let mut server = Server::builder();
    let result = if let Some(reflection_service) = reflection_service {
        server
            .add_service(health_service)
            .add_service(reflection_service)
            .add_service(management_service)
            .serve_with_incoming_shutdown(incoming, graceful)
            .await
    } else {
        server
            .add_service(health_service)
            .add_service(management_service)
            .serve_with_incoming_shutdown(incoming, graceful)
            .await
    };

    result.map_err(|error| anyhow::anyhow!("management gRPC server error: {error}"))?;

    if let Some(socket_path) = unix_socket_path {
        info!(
            "Management gRPC server on unix://{} shut down gracefully",
            absolute_display_path(Path::new(&socket_path))
        );
    } else {
        info!("Management gRPC server shut down gracefully");
    }

    Ok(())
}

trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static {}

impl<T> AsyncReadWrite for T where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static
{
}

#[cfg(unix)]
fn prepare_management_unix_socket(path: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let socket_path = Path::new(path);
    let parent = socket_path.parent().ok_or_else(|| {
        anyhow::anyhow!("management unix socket path '{path}' must have a parent directory")
    })?;
    let parent_preexisting = match std::fs::metadata(parent) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(anyhow::anyhow!(
                    "management unix socket parent path {} already exists and is not a directory",
                    absolute_display_path(parent)
                ));
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(anyhow::anyhow!(
                "failed to inspect management unix socket parent directory {}: {error}",
                absolute_display_path(parent)
            ));
        }
    };

    if !parent_preexisting {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create management unix socket parent directory {}",
                absolute_display_path(parent)
            )
        })?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).with_context(
            || {
                format!(
                    "failed to restrict management unix socket parent directory permissions {}",
                    absolute_display_path(parent)
                )
            },
        )?;
    }

    match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            std::fs::remove_file(socket_path).with_context(|| {
                format!(
                    "failed to remove stale management unix socket {}",
                    absolute_display_path(socket_path)
                )
            })?;
        }
        Ok(_) => {
            return Err(anyhow::anyhow!(
                "management unix socket path {} already exists and is not a socket",
                absolute_display_path(socket_path)
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(anyhow::anyhow!(
                "failed to inspect management unix socket path {}: {error}",
                absolute_display_path(socket_path)
            ));
        }
    }

    Ok(())
}

#[cfg(unix)]
fn restrict_management_unix_socket_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).with_context(|| {
        format!(
            "failed to restrict management unix socket permissions {}",
            absolute_display_path(path)
        )
    })
}

#[cfg(unix)]
fn cleanup_management_unix_socket(path: &str) {
    if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            warn!(
                "Failed to remove management unix socket {}: {error}",
                absolute_display_path(Path::new(path))
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::{prepare_management_unix_socket, restrict_management_unix_socket_permissions};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tonic::transport::Server;
    use tonic_health::pb::health_client::HealthClient;
    use tonic_health::pb::HealthCheckRequest;

    #[cfg(unix)]
    struct TempDirGuard {
        path: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl TempDirGuard {
        fn new(label: &str) -> Self {
            let base_dir = std::path::Path::new("/tmp");
            let path = base_dir.join(format!(
                "stv-m-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock should be after unix epoch")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("temp dir should be created");
            Self { path }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    #[cfg(unix)]
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[cfg(unix)]
    #[test]
    fn prepare_management_unix_socket_restricts_parent_directory_permissions() {
        let temp_dir = TempDirGuard::new("parent-perms");
        let runtime_dir = temp_dir.path().join("management-runtime");
        let socket_path = runtime_dir.join("synctv.sock");

        prepare_management_unix_socket(socket_path.to_str().expect("socket path should be utf-8"))
            .expect("prepare should succeed");

        let metadata = std::fs::metadata(&runtime_dir).expect("runtime dir metadata");
        let mode = metadata.permissions().mode() & 0o777;

        assert_eq!(
            mode, 0o700,
            "management runtime directory must be owner-only, got {mode:o}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepare_management_unix_socket_preserves_existing_parent_directory_permissions() {
        let temp_dir = TempDirGuard::new("existing-parent-perms");
        let runtime_dir = temp_dir.path().join("preexisting-runtime");
        std::fs::create_dir_all(&runtime_dir).expect("existing runtime dir should be created");
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o755))
            .expect("existing runtime dir permissions should be set");
        let socket_path = runtime_dir.join("synctv.sock");

        prepare_management_unix_socket(socket_path.to_str().expect("socket path should be utf-8"))
            .expect("prepare should succeed");

        let metadata = std::fs::metadata(&runtime_dir).expect("runtime dir metadata");
        let mode = metadata.permissions().mode() & 0o777;

        assert_eq!(
            mode, 0o755,
            "prepare must not rewrite permissions for an existing parent directory, got {mode:o}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn management_unix_socket_is_owner_only_after_bind() {
        let temp_dir = TempDirGuard::new("socket-perms");
        let socket_path = temp_dir.path().join("management.sock");

        prepare_management_unix_socket(socket_path.to_str().expect("socket path should be utf-8"))
            .expect("prepare should succeed");

        let listener =
            std::os::unix::net::UnixListener::bind(&socket_path).expect("socket should bind");
        restrict_management_unix_socket_permissions(&socket_path)
            .expect("socket permissions should be restricted");
        let metadata = std::fs::metadata(&socket_path).expect("socket metadata");
        let mode = metadata.permissions().mode() & 0o777;

        drop(listener);
        std::fs::remove_file(&socket_path).expect("socket cleanup");

        assert_eq!(
            mode, 0o600,
            "management unix socket must be owner-only, got {mode:o}"
        );
    }

    #[tokio::test]
    async fn management_health_service_remains_accessible_without_management_auth() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("health test listener should bind");
        let addr = listener
            .local_addr()
            .expect("health test listener should expose local address");

        let (reporter, health_service) = tonic_health::server::health_reporter();
        reporter
            .set_service_status("", tonic_health::ServingStatus::Serving)
            .await;

        let serve_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(health_service)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .expect("health-only management server should serve");
        });

        let endpoint = format!("http://{addr}");
        let channel = tonic::transport::Endpoint::from_shared(endpoint)
            .expect("health test endpoint should be valid")
            .connect()
            .await
            .expect("health test channel should connect");

        let mut unauthenticated_client = HealthClient::new(channel);
        let response = unauthenticated_client
            .check(HealthCheckRequest {
                service: String::new(),
            })
            .await
            .expect("health check should stay available without management auth")
            .into_inner();
        assert_eq!(
            response.status,
            tonic_health::pb::health_check_response::ServingStatus::Serving as i32
        );

        serve_handle.abort();
        let _ = serve_handle.await;
    }
}
