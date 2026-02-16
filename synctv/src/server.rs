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
/// Livestream server state (held for graceful shutdown).
///
/// Dropping the handle stops the `StreamHub` event loop and all dependent tasks.
pub struct LivestreamState {
    pub handle: synctv_livestream::livestream::LivestreamHandle,
}

/// Container for shared services
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
    pub sfu_manager: Option<Arc<synctv_sfu::SfuManager>>,
    pub node_registry: Option<Arc<synctv_cluster::discovery::NodeRegistry>>,
    pub health_monitor: Option<Arc<synctv_cluster::discovery::HealthMonitor>>,
    pub load_balancer: Option<Arc<synctv_cluster::discovery::LoadBalancer>>,
    /// Token blacklist service for checking revoked tokens
    pub token_blacklist: synctv_core::service::TokenBlacklistService,
    /// Shared Redis connection for playback caching
    pub redis_conn: Option<redis::aio::ConnectionManager>,
    /// CancellationToken for settings listen task
    pub settings_cancel: tokio_util::sync::CancellationToken,
    /// CancellationToken for partition management tasks
    pub partition_cancel: tokio_util::sync::CancellationToken,
    /// Leader elector for singleton operations (None in single-node mode)
    pub leader_elector: Option<synctv_cluster::leader::AnyLeaderElector>,
    /// CancellationToken for leader election loop
    pub leader_cancel: tokio_util::sync::CancellationToken,
    /// K8s DNS refresh background task abort handle (aborted during shutdown)
    pub dns_refresh_abort: Option<tokio::task::AbortHandle>,
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

impl Services {}

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

    /// Start all servers and wait for shutdown signal
    pub async fn start(mut self) -> anyhow::Result<()> {
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
        if self.services.sfu_manager.is_some() {
            info!("SFU manager: enabled");
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
                                    infra.kick_user_publishers(user_id.as_str());
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
        let grpc_handle = self.grpc_handle.take()
            .ok_or_else(|| anyhow::anyhow!("gRPC server handle missing after startup"))?;
        let http_handle = self.http_handle.take()
            .ok_or_else(|| anyhow::anyhow!("HTTP server handle missing after startup"))?;

        tokio::select! {
            _ = grpc_handle => {
                error!("gRPC server stopped unexpectedly");
            }
            _ = http_handle => {
                error!("HTTP server stopped unexpectedly");
            }
            () = shutdown_signal() => {
                info!("Shutdown signal received, starting graceful shutdown...");
            }
        }

        // Signal all components to shut down
        let _ = shutdown_tx.send(true);
        cleanup_cancel.cancel();
        self.services.settings_cancel.cancel();
        self.services.partition_cancel.cancel();
        self.services.leader_cancel.cancel();

        // Wait for admin event listener to finish before shutting down
        // the cluster manager (which owns the broadcast channel it reads from).
        if let Some(handle) = admin_event_handle {
            info!("Waiting for admin event listener to stop...");
            let _ = handle.await;
            info!("Admin event listener stopped");
        }

        // Run graceful shutdown
        self.shutdown().await;

        Ok(())
    }

    /// Gracefully shut down all server components
    ///
    /// **Production Enhancement (#72)**: Implements graceful shutdown with connection draining:
    /// - Waits for active connections to complete before shutting down (configurable timeout)
    /// - Polls connection count every 500ms to track drainage progress
    /// - Deregisters node from cluster registry for immediate peer discovery of departure
    /// - Sequentially shuts down components: SFU → STUN → livestream → cluster → database
    /// - Ensures zero-downtime deployments in Kubernetes (readiness probe fails immediately)
    async fn shutdown(&self) {
        info!("Shutting down SyncTV server...");

        // 1. Wait for active connections to drain (with configurable timeout)
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

        // 1.5. Deregister this node from the cluster registry so other nodes
        // discover the departure immediately instead of waiting for heartbeat timeout.
        if let Some(ref registry) = self.services.node_registry {
            info!("Deregistering node from cluster registry...");
            if let Err(e) = registry.unregister().await {
                warn!("Failed to deregister node from cluster registry: {}", e);
            } else {
                info!("Node deregistered from cluster registry");
            }
        }

        // 2. Shutdown SFU manager (close all SFU rooms)
        if let Some(ref sfu) = self.services.sfu_manager {
            info!("Shutting down SFU manager...");
            sfu.shutdown().await;
        }

        // 3. Shut down STUN server
        if let Some(ref stun) = self.services.stun_server {
            info!("Shutting down STUN server...");
            stun.shutdown().await;
            info!("STUN server shut down");
        }

        // 4. Stop livestream: abort the StreamHub event loop
        if let Some(ref state) = self.livestream_state {
            info!("Stopping livestream infrastructure...");
            state.handle.shutdown();
            info!("Livestream infrastructure shut down");
        }

        // 4.5. Shut down health monitor (await the monitoring task)
        if let Some(ref health_monitor) = self.services.health_monitor {
            info!("Shutting down health monitor...");
            health_monitor.shutdown().await;
            info!("Health monitor shut down");
        }

        // 4.6. Abort K8s DNS refresh loop (if active)
        if let Some(ref abort_handle) = self.services.dns_refresh_abort {
            info!("Aborting K8s DNS refresh loop...");
            abort_handle.abort();
            info!("K8s DNS refresh loop aborted");
        }

        // 5. Shut down cluster manager (cancels Redis Pub/Sub + heartbeat + deduplicator tasks)
        if let Some(ref cluster_mgr) = self.services.cluster_manager {
            info!("Shutting down cluster manager...");
            cluster_mgr.shutdown().await;
            info!("Cluster manager shut down");
        }

        // 6. Redis publish channel closes when sender is dropped
        if self.services.redis_publish_tx.is_some() {
            info!("Closing Redis publish channel");
        }

        // 6. Close the database connection pool
        info!("Closing database connection pool...");
        self.pool.close().await;
        info!("Database pool closed");

        info!("SyncTV server shut down complete");
    }

    /// Start gRPC server
    async fn start_grpc_server(&self, shutdown_rx: watch::Receiver<bool>) -> anyhow::Result<JoinHandle<()>> {
        let config = self.config.clone();
        // If no ClusterManager (Redis unavailable), create a default single-node one
        // so gRPC server can still start without Redis.
        let cluster_manager = if let Some(cm) = self.services.cluster_manager.clone() {
            cm
        } else {
            warn!("ClusterManager not available, creating default single-node instance for gRPC server");
            Arc::new(
                synctv_cluster::sync::ClusterManager::with_defaults()
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to create default ClusterManager: {e}"))?
            )
        };

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
                provider_instance_repository: services.provider_instance_repository,
                user_provider_credential_repository: services.user_provider_credential_repository,
                settings_service: services.settings_service,
                settings_registry: Some(services.settings_registry),
                email_service: services.email_service,
                email_token_service: services.email_token_service,
                sfu_manager: services.sfu_manager,
                live_streaming_infrastructure: services.live_streaming_infrastructure,
                publish_key_service: Some(services.publish_key_service),
                notification_service: services.notification_service,
                oauth2_service: services.oauth2_service,
                audit_service: services.audit_service,
                node_registry: services.node_registry,
                token_blacklist_service: services.token_blacklist,
                redis_conn: services.redis_conn,
                shutdown_rx: Some(shutdown_rx),
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
        let sfu_manager = self.services.sfu_manager.clone();

        // Create WebSocket ticket service if Redis is configured
        let ws_ticket_service = if self.config.redis.url.is_empty() {
            None
        } else {
            match redis::Client::open(self.config.redis.url.clone()) {
                Ok(redis_client) => {
                    match redis::aio::ConnectionManager::new(redis_client).await {
                        Ok(conn) => {
                            Some(Arc::new(synctv_core::service::WsTicketService::new(
                                Some(conn),
                                None,
                            )))
                        }
                        Err(e) => {
                            tracing::warn!("Failed to create Redis ConnectionManager for ws_ticket_service: {}", e);
                            None
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to create Redis client for ws_ticket_service: {}", e);
                    None
                }
            }
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
                publish_key_service: Some(publish_key_service),
                notification_service,
                audit_service: self.services.audit_service.clone(),
                live_streaming_infrastructure,
                sfu_manager,
                rate_limiter: self.services.rate_limiter.clone(),
                token_blacklist_service: self.services.token_blacklist.clone(),
                ws_ticket_service,
                redis_conn: self.services.redis_conn.clone(),
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

            if let Err(e) = axum::serve(listener, http_router)
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
