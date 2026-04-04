use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tonic::transport::Server;
use tracing::{info, warn};

use synctv_api::impls::{AdminApiImpl, ClientApiImpl};
use synctv_core::{
    config::absolute_display_path, config::ManagementTransport, service::UserService, Config,
};

use crate::lifecycle::ManagementLifecycleController;
use crate::proto::management_service_server::ManagementServiceServer;
use crate::service::ManagementServiceImpl;
use crate::FILE_DESCRIPTOR_SET;

pub struct ManagementServerConfig {
    pub config: Arc<Config>,
    pub user_service: Arc<UserService>,
    pub admin_api: Arc<AdminApiImpl>,
    pub client_api: Arc<ClientApiImpl>,
    pub lifecycle_controller: Arc<ManagementLifecycleController>,
    pub shutdown_rx: watch::Receiver<bool>,
}

pub async fn spawn_management_server(
    config: ManagementServerConfig,
) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
    match config.config.management.transport {
        ManagementTransport::Tcp => spawn_management_tcp_server(config).await,
        ManagementTransport::Unix => spawn_management_unix_server(config).await,
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

async fn spawn_management_unix_server(
    config: ManagementServerConfig,
) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
    #[cfg(not(unix))]
    {
        let _ = config;
        Err(anyhow::anyhow!(
            "management.transport=unix is not supported on this platform"
        ))
    }

    #[cfg(unix)]
    {
        let socket_path = config.config.management.unix_socket_path.clone();
        prepare_management_unix_socket(&socket_path)?;
        let listener = tokio::net::UnixListener::bind(&socket_path).with_context(|| {
            format!(
                "failed to bind management unix socket {}",
                absolute_display_path(Path::new(&socket_path))
            )
        })?;
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
    let management_service = ManagementServiceServer::new(ManagementServiceImpl::new(
        config.user_service,
        config.admin_api,
        config.client_api,
        config.lifecycle_controller,
    ))
    .max_decoding_message_size(config.config.server.grpc_max_message_size_bytes)
    .max_encoding_message_size(config.config.server.grpc_max_message_size_bytes);

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
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
    use std::os::unix::fs::FileTypeExt;

    let socket_path = Path::new(path);
    let parent = socket_path.parent().ok_or_else(|| {
        anyhow::anyhow!("management unix socket path '{path}' must have a parent directory")
    })?;
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create management unix socket parent directory {}",
            absolute_display_path(parent)
        )
    })?;

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
