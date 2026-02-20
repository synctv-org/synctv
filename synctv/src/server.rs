//! Server lifecycle management
//!
//! Manages the startup and shutdown of all server components:
//! - gRPC API server
//! - HTTP/REST server
//! - RTMP livestream server

use std::sync::Arc;
use std::time::Duration;
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use synctv_core::{
    service::{RoomService, UserService},
    repository::UserProviderCredentialRepository,
    provider::{AlistProvider, BilibiliProvider, EmbyProvider},
    Config,
};
use synctv_cluster::sync::ClusterEvent;

use crate::shutdown::ShutdownCoordinator;

/// Livestream server state (held for graceful shutdown).
///
/// Dropping the handle stops the `StreamHub` event loop and all dependent tasks.
pub struct LivestreamState {
    pub handle: synctv_livestream::livestream::LivestreamHandle,
}

/// Container for shared runtime services.
///
/// This struct holds only runtime service references. Shutdown-related resources
/// (cancellation tokens, background task handles, flush hooks) are managed by
/// `ShutdownCoordinator`.
#[derive(Clone)]
#[allow(dead_code)]
pub struct Services {
    pub user_service: Arc<UserService>,
    pub room_service: Arc<RoomService>,
    pub jwt_service: synctv_core::service::JwtService,
    pub cluster_manager: Option<Arc<synctv_cluster::sync::ClusterManager>>,
    pub redis_publish_tx: Option<tokio::sync::mpsc::Sender<synctv_cluster::sync::PublishRequest>>,
    pub rate_limiter: synctv_core::service::RateLimiter,
    pub rate_limit_config: synctv_core::service::RateLimitConfig,
    pub content_filter: synctv_core::service::ContentFilter,
    pub connection_manager: synctv_cluster::sync::ConnectionManager,
    pub providers_manager: Arc<synctv_core::service::ProvidersManager>,
    pub provider_instance_manager: Arc<synctv_core::service::RemoteProviderManager>,
    pub provider_instance_repository: Arc<synctv_core::repository::ProviderInstanceRepository>,
    pub user_provider_credential_repository: Arc<UserProviderCredentialRepository>,
    pub alist_provider: Arc<AlistProvider>,
    pub bilibili_provider: Arc<BilibiliProvider>,
    pub emby_provider: Arc<EmbyProvider>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub settings_service: Arc<synctv_core::service::SettingsService>,
    pub settings_registry: Arc<synctv_core::service::SettingsRegistry>,
    pub email_service: Option<Arc<synctv_core::service::EmailService>>,
    pub email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    pub publish_key_service: Arc<synctv_core::service::PublishKeyService>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub live_streaming_infrastructure: Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub stun_server: Option<Arc<synctv_core::service::StunServer>>,
    pub node_registry: Option<Arc<synctv_cluster::discovery::NodeRegistry>>,
    pub health_monitor: Option<Arc<synctv_cluster::discovery::HealthMonitor>>,
    pub load_balancer: Option<Arc<synctv_cluster::discovery::LoadBalancer>>,
    /// Shared Redis connection for playback caching (optional in standalone mode).
    pub redis_client: Option<redis::Client>,
    pub redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
    /// Credential encryption for protecting sensitive data (optional)
    pub credential_encryption: Option<synctv_core::service::CredentialEncryption>,
}

/// `SyncTV` server - manages all server components
pub struct SyncTvServer {
    config: Config,
    services: Services,
    livestream_state: Option<LivestreamState>,
    pool: PgPool,
    grpc_handle: Option<JoinHandle<()>>,
    http_handle: Option<JoinHandle<()>>,
}

impl SyncTvServer {
    /// Create a new server instance
    pub const fn new(
        config: Config,
        services: Services,
        livestream_state: Option<LivestreamState>,
        pool: PgPool,
    ) -> Self {
        Self {
            config,
            services,
            livestream_state,
            pool,
            grpc_handle: None,
            http_handle: None,
        }
    }

    /// Start all servers and wait for shutdown signal, using a `ShutdownCoordinator`
    /// for centralized shutdown orchestration.
    pub async fn start_with_coordinator(mut self, coordinator: ShutdownCoordinator) -> anyhow::Result<()> {
        info!("Starting SyncTV server...");

        // Create shutdown signal channel
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // Log infrastructure state
        if self.livestream_state.is_some() {
            info!("Livestream infrastructure: enabled");
        }
        if self.services.stun_server.is_some() {
            info!("STUN server: enabled");
        }

        // Start background connection cleanup (every 60 seconds)
        let cleanup_cancel = tokio_util::sync::CancellationToken::new();
        let _conn_cleanup = self.services.connection_manager.spawn_cleanup_task(
            Duration::from_mins(1),
            cleanup_cancel.clone(),
        );

        // Start gRPC server
        let grpc_handle = self.start_grpc_server(shutdown_rx.clone()).await?;
        self.grpc_handle = Some(grpc_handle);

        // Start HTTP server with graceful shutdown
        let http_handle = self.start_http_server(shutdown_rx.clone()).await?;
        self.http_handle = Some(http_handle);

        // Spawn streaming event listener for cluster-wide kicks
        let admin_event_handle: Option<JoinHandle<()>> = if let (Some(cluster_mgr), Some(infra)) = (&self.services.cluster_manager, &self.services.live_streaming_infrastructure) {
            let mut admin_rx = cluster_mgr.subscribe_admin_events();
            let infra = infra.clone();
            let handle = tokio::spawn(async move {
                loop {
                    match admin_rx.recv().await {
                        Ok(event) => {
                            match &event {
                                ClusterEvent::KickPublisher { room_id, media_id, reason, .. } => {
                                    info!(
                                        room_id = %room_id.as_str(),
                                        media_id = %media_id.as_str(),
                                        reason = %reason,
                                        "Received cluster-wide stream kick"
                                    );
                                    let _ = infra.kick_publisher(room_id.as_str(), media_id.as_str());
                                }
                                ClusterEvent::KickUser { user_id, reason, .. } => {
                                    info!(
                                        user_id = %user_id.as_str(),
                                        reason = %reason,
                                        "Received cluster-wide user kick"
                                    );
                                    infra.kick_user_publishers(user_id.as_str()).await;
                                }
                                _ => {}
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Admin event listener lagged by {} events", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("Admin event channel closed, stopping listener");
                            break;
                        }
                    }
                }
            });
            info!("Admin event listener spawned for cluster-wide stream kicks");
            Some(handle)
        } else {
            None
        };

        info!("All servers started successfully");

        // Wait for either a server to stop or a shutdown signal
        let mut grpc_handle = self.grpc_handle.take()
            .ok_or_else(|| anyhow::anyhow!("gRPC server handle missing after startup"))?;
        let mut http_handle = self.http_handle.take()
            .ok_or_else(|| anyhow::anyhow!("HTTP server handle missing after startup"))?;

        tokio::select! {
            _ = &mut grpc_handle => {
                error!("gRPC server stopped unexpectedly");
            }
            _ = &mut http_handle => {
                error!("HTTP server stopped unexpectedly");
            }
            () = shutdown_signal() => {
                info!("Shutdown signal received, starting graceful shutdown...");
            }
        }

        // Signal gRPC/HTTP servers to shut down
        let _ = shutdown_tx.send(true);
        cleanup_cancel.cancel();

        // Wait for gRPC and HTTP servers to finish with a timeout
        let drain_timeout = self.config.server.shutdown_drain_timeout_seconds;
        info!("Waiting up to {}s for gRPC and HTTP servers to shut down...", drain_timeout);
        let _ = tokio::time::timeout(
            Duration::from_secs(drain_timeout),
            async {
                let _ = grpc_handle.await;
                let _ = http_handle.await;
            },
        ).await;
        info!("gRPC and HTTP servers shut down");

        // Drain active connections BEFORE shutting down the cluster manager.
        // Events generated during drain (UserLeft, etc.) need the pub/sub
        // system to be alive so they can be broadcast to other replicas.
        {
            let drain_timeout = Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds);
            let drain_poll_interval = Duration::from_millis(500);
            let active = self.services.connection_manager.connection_count();
            if active > 0 {
                info!(
                    "Waiting up to {}s for {} active connection(s) to drain...",
                    drain_timeout.as_secs(),
                    active
                );
                let deadline = tokio::time::Instant::now() + drain_timeout;
                loop {
                    let remaining = self.services.connection_manager.connection_count();
                    if remaining == 0 {
                        info!("All connections drained");
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        warn!(
                            "Drain timeout reached with {} connection(s) still active, proceeding with shutdown",
                            remaining
                        );
                        break;
                    }
                    tokio::time::sleep(drain_poll_interval).await;
                }
            }
        }

        // Shut down the cluster manager so the admin event broadcast channel
        // closes, allowing the admin_event_handle listener to exit.
        if let Some(ref cluster_mgr) = self.services.cluster_manager {
            info!("Shutting down cluster manager (post-drain, closing admin event channel)...");
            cluster_mgr.shutdown().await;
            info!("Cluster manager shut down (admin event channel closed)");
        }

        // Wait for admin event listener
        if let Some(handle) = admin_event_handle {
            info!("Waiting for admin event listener to stop...");
            match tokio::time::timeout(Duration::from_secs(5), handle).await {
                Ok(_) => { info!("Admin event listener stopped"); }
                Err(_) => { warn!("Admin event listener did not stop within 5s, proceeding"); }
            }
        }

        // Shut down remaining infrastructure components
        self.shutdown_components().await;

        // Centralized shutdown: cancel tokens → drain tasks → run hooks
        coordinator.shutdown().await;

        // Close the database connection pool (after audit flush and settings task)
        info!("Closing database connection pool...");
        self.pool.close().await;
        info!("Database pool closed");

        info!("SyncTV server shut down complete");
        Ok(())
    }

    /// Shut down infrastructure components (STUN, livestream, health monitor, node registry, connection manager).
    ///
    /// This is separate from the `ShutdownCoordinator` because these components
    /// have custom shutdown protocols (not just cancellation tokens or join handles).
    async fn shutdown_components(&self) {
        // Shut down connection manager (stops TTL refresh background task)
        info!("Shutting down connection manager...");
        self.services.connection_manager.shutdown();
        info!("Connection manager shut down");

        // Deregister node from cluster registry
        if let Some(ref registry) = self.services.node_registry {
            info!("Deregistering node from cluster registry...");
            if let Err(e) = registry.unregister().await {
                warn!("Failed to deregister node from cluster registry: {}", e);
            } else {
                info!("Node deregistered from cluster registry");
            }
        }

        // Shut down STUN server
        if let Some(ref stun) = self.services.stun_server {
            info!("Shutting down STUN server...");
            stun.shutdown().await;
            info!("STUN server shut down");
        }

        // Stop livestream
        if let Some(ref state) = self.livestream_state {
            info!("Stopping livestream infrastructure...");
            state.handle.shutdown();
            info!("Livestream infrastructure shut down");
        }

        // Shut down health monitor
        if let Some(ref health_monitor) = self.services.health_monitor {
            info!("Shutting down health monitor...");
            health_monitor.shutdown().await;
            info!("Health monitor shut down");
        }

        // Redis publish channel closes when sender is dropped
        if self.services.redis_publish_tx.is_some() {
            info!("Closing Redis publish channel");
        }
    }

    /// Start gRPC server
    async fn start_grpc_server(&self, shutdown_rx: watch::Receiver<bool>) -> anyhow::Result<JoinHandle<()>> {
        let config = self.config.clone();
        let cluster_manager = self.services.cluster_manager.clone();

        let services = self.services.clone();
        let handle = tokio::spawn(async move {
            info!("Starting gRPC server on {}...", config.grpc_address());
            let grpc_config = synctv_api::grpc::GrpcServerConfig {
                config: &config,
                jwt_service: services.jwt_service,
                user_service: services.user_service,
                room_service: services.room_service,
                cluster_manager,
                redis_publish_tx: services.redis_publish_tx,
                rate_limiter: services.rate_limiter,
                rate_limit_config: services.rate_limit_config,
                content_filter: services.content_filter,
                connection_manager: services.connection_manager,
                providers_manager: Some(services.providers_manager),
                provider_instance_manager: services.provider_instance_manager,
                user_provider_credential_repository: services.user_provider_credential_repository,
                settings_service: services.settings_service,
                settings_registry: Some(services.settings_registry),
                email_service: services.email_service,
                email_token_service: services.email_token_service,
                live_streaming_infrastructure: services.live_streaming_infrastructure,
                publish_key_service: Some(services.publish_key_service),
                notification_service: services.notification_service,
                oauth2_service: services.oauth2_service,
                audit_service: services.audit_service,
                node_registry: services.node_registry,
                redis_client: services.redis_client.clone(),
                redis_conn: services.redis_conn.clone(),
                shutdown_rx: Some(shutdown_rx),
                builtin_stun_url: services.stun_server.as_ref().map(|s| {
                    let addr = s.external_addr();
                    format!("stun:{}:{}", addr.ip(), addr.port())
                }),
            };
            if let Err(e) = synctv_api::grpc::serve(grpc_config).await {
                error!("gRPC server error: {}", e);
            }
        });

        Ok(handle)
    }

    /// Start HTTP server with graceful shutdown support
    async fn start_http_server(&self, shutdown_rx: watch::Receiver<bool>) -> anyhow::Result<JoinHandle<()>> {
        let http_address = self.config.http_address();
        let user_service = self.services.user_service.clone();
        let room_service = self.services.room_service.clone();
        let provider_instance_manager = self.services.provider_instance_manager.clone();
        let user_provider_credential_repository = self.services.user_provider_credential_repository.clone();
        let cluster_manager = self.services.cluster_manager.clone();
        let jwt_service = self.services.jwt_service.clone();
        let redis_publish_tx = self.services.redis_publish_tx.clone();
        let oauth2_service = self.services.oauth2_service.clone();
        let settings_service = self.services.settings_service.clone();
        let settings_registry = self.services.settings_registry.clone();
        let email_service = self.services.email_service.clone();
        let publish_key_service = self.services.publish_key_service.clone();
        let notification_service = self.services.notification_service.clone();
        let connection_manager = self.services.connection_manager.clone();

        let live_streaming_infrastructure = self.services.live_streaming_infrastructure.clone();

        let is_cluster_mode = !self.config.server.cluster_secret.is_empty();
        let ws_ticket_service = if let Some(ref redis_conn) = self.services.redis_conn {
            let redis_conn_snapshot = redis_conn.read().await.clone();
            match synctv_core::service::WsTicketService::new(
                Some(redis_conn_snapshot),
                None,
                is_cluster_mode,
            ) {
                Ok(svc) => Some(Arc::new(svc)),
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Failed to initialize WebSocket ticket service: {e}"
                    ));
                }
            }
        } else {
            Some(Arc::new(synctv_core::service::WsTicketService::with_memory(None)))
        };

        let http_router = synctv_api::http::create_router_from_config(
            synctv_api::http::RouterConfig {
                config: Arc::new(self.config.clone()),
                user_service,
                room_service,
                provider_instance_manager,
                user_provider_credential_repository,
                alist_provider: self.services.alist_provider.clone(),
                bilibili_provider: self.services.bilibili_provider.clone(),
                emby_provider: self.services.emby_provider.clone(),
                cluster_manager,
                connection_manager: Arc::new(connection_manager),
                jwt_service,
                redis_publish_tx,
                oauth2_service,
                settings_service: Some(settings_service),
                settings_registry: Some(settings_registry),
                email_service,
                email_token_service: self.services.email_token_service.clone(),
                publish_key_service: Some(publish_key_service),
                notification_service,
                audit_service: self.services.audit_service.clone(),
                live_streaming_infrastructure,
                rate_limiter: self.services.rate_limiter.clone(),
                ws_ticket_service,
                redis_conn: self.services.redis_conn.clone(),
                builtin_stun_url: self.services.stun_server.as_ref().map(|s| {
                    let addr = s.external_addr();
                    format!("stun:{}:{}", addr.ip(), addr.port())
                }),
                credential_encryption: self.services.credential_encryption.clone(),
            },
        );

        let handle = tokio::spawn(async move {
            let http_addr: std::net::SocketAddr = match http_address.parse() {
                Ok(addr) => addr,
                Err(e) => {
                    error!("Invalid HTTP address '{}': {}", http_address, e);
                    return;
                }
            };

            let listener = match tokio::net::TcpListener::bind(http_addr).await {
                Ok(listener) => listener,
                Err(e) => {
                    error!("Failed to bind HTTP address {}: {}", http_addr, e);
                    return;
                }
            };

            info!("HTTP server listening on {}", http_addr);

            let mut rx = shutdown_rx;
            let graceful = async move {
                let _ = rx.changed().await;
            };

            if let Err(e) = axum::serve(
                listener,
                http_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
                .with_graceful_shutdown(graceful)
                .await
            {
                error!("HTTP server error: {}", e);
            }

            info!("HTTP server shut down gracefully");
        });

        Ok(handle)
    }
}

/// Wait for a shutdown signal (SIGTERM or SIGINT/Ctrl+C)
async fn shutdown_signal() {
    let ctrl_c = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                info!("Received Ctrl+C signal");
            }
            Err(e) => {
                error!("Failed to install Ctrl+C handler: {}", e);
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
                info!("Received SIGTERM signal");
            }
            Err(e) => {
                error!("Failed to install SIGTERM handler: {}", e);
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => { info!("Received Ctrl+C"); }
        () = terminate => { info!("Received SIGTERM"); }
    }
}
