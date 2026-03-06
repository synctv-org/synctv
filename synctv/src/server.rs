//! Server lifecycle management
//!
//! Manages the startup and shutdown of all server components:
//! - gRPC API server
//! - HTTP/REST server
//! - RTMP livestream server

use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use synctv_cluster::sync::ClusterEvent;
use synctv_core::{
    repository::UserProviderCredentialRepository,
    service::{RoomService, UserService},
    Config,
};

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
    pub user_provider_credential_repository: Arc<UserProviderCredentialRepository>,
    pub providers: synctv_core::provider::ProviderSet,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub settings_service: Arc<synctv_core::service::SettingsService>,
    pub settings_registry: Arc<synctv_core::service::SettingsRegistry>,
    pub email_service: Option<Arc<synctv_core::service::EmailService>>,
    pub email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    pub publish_key_service: Arc<synctv_core::service::PublishKeyService>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub chat_service: Arc<synctv_core::service::ChatService>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub live_streaming_infrastructure:
        Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub stun_server: Option<Arc<synctv_core::service::StunServer>>,
    pub turn_health_checker: Option<Arc<synctv_core::service::TurnHealthChecker>>,
    pub node_registry: Option<Arc<synctv_cluster::discovery::NodeRegistry>>,
    pub health_monitor: Option<Arc<synctv_cluster::discovery::HealthMonitor>>,
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

fn build_ws_ticket_service(
    redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
    is_cluster_mode: bool,
) -> anyhow::Result<Option<Arc<synctv_core::service::WsTicketService>>> {
    let svc = synctv_core::service::WsTicketService::new(redis_conn, None, is_cluster_mode)
        .map_err(|e| anyhow::anyhow!("Failed to initialize WebSocket ticket service: {e}"))?;
    Ok(Some(Arc::new(svc)))
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
    pub async fn start_with_coordinator(
        mut self,
        coordinator: ShutdownCoordinator,
    ) -> anyhow::Result<()> {
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
        let _conn_cleanup = self
            .services
            .connection_manager
            .spawn_cleanup_task(Duration::from_mins(1), cleanup_cancel.clone());

        // Start gRPC server
        let grpc_handle = self.start_grpc_server(shutdown_rx.clone()).await?;
        self.grpc_handle = Some(grpc_handle);

        // Start HTTP server with graceful shutdown
        let http_handle = self.start_http_server(shutdown_rx.clone()).await?;
        self.http_handle = Some(http_handle);

        // Spawn streaming event listener for cluster-wide kicks
        let admin_event_handle: Option<JoinHandle<()>> = if let (Some(cluster_mgr), Some(infra)) = (
            &self.services.cluster_manager,
            &self.services.live_streaming_infrastructure,
        ) {
            let mut admin_rx = cluster_mgr.subscribe_admin_events();
            let infra = infra.clone();
            let handle = tokio::spawn(async move {
                loop {
                    match admin_rx.recv().await {
                        Ok(event) => match &event {
                            ClusterEvent::KickPublisher {
                                room_id,
                                media_id,
                                reason,
                                ..
                            } => {
                                info!(
                                    room_id = %room_id.as_str(),
                                    media_id = %media_id.as_str(),
                                    reason = %reason,
                                    "Received cluster-wide stream kick"
                                );
                                if let Err(e) =
                                    infra.kick_publisher(room_id.as_str(), media_id.as_str())
                                {
                                    warn!(
                                        room_id = %room_id.as_str(),
                                        media_id = %media_id.as_str(),
                                        error = %e,
                                        "Failed to kick publisher from StreamHub"
                                    );
                                }
                            }
                            ClusterEvent::KickUser {
                                user_id, reason, ..
                            } => {
                                info!(
                                    user_id = %user_id.as_str(),
                                    reason = %reason,
                                    "Received cluster-wide user kick"
                                );
                                infra.kick_user_publishers(user_id.as_str()).await;
                            }
                            _ => {}
                        },
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
        let mut grpc_handle = self
            .grpc_handle
            .take()
            .ok_or_else(|| anyhow::anyhow!("gRPC server handle missing after startup"))?;
        let mut http_handle = self
            .http_handle
            .take()
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

        // D6 fix: Track total shutdown start time to compute remaining budget for
        // each phase. The total drain budget is `shutdown_drain_timeout_seconds`.
        // Previously, both HTTP drain and connection drain each used the full
        // timeout, potentially exceeding K8s grace period (2x the configured value).
        let shutdown_start = tokio::time::Instant::now();
        let total_drain_budget =
            Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds);

        // Phase 1: Wait for gRPC and HTTP servers to finish (use 60% of budget).
        let http_drain_budget = total_drain_budget * 60 / 100;
        info!(
            "Waiting up to {}s for gRPC and HTTP servers to shut down...",
            http_drain_budget.as_secs()
        );
        let _ = tokio::time::timeout(http_drain_budget, async {
            let _ = grpc_handle.await;
            let _ = http_handle.await;
        })
        .await;
        info!("gRPC and HTTP servers shut down");

        // Phase 2: Drain active connections BEFORE shutting down the cluster manager.
        // Events generated during drain (UserLeft, etc.) need the pub/sub
        // system to be alive so they can be broadcast to other replicas.
        //
        // D6 fix: Use the REMAINING time from the total budget instead of a
        // separate full timeout, ensuring total shutdown stays within K8s grace period.
        {
            let elapsed = shutdown_start.elapsed();
            let remaining_budget = total_drain_budget.saturating_sub(elapsed);
            let drain_poll_interval = Duration::from_millis(500);
            let active = self.services.connection_manager.connection_count();
            if active > 0 && remaining_budget > Duration::ZERO {
                info!(
                    "Waiting up to {}s for {} active connection(s) to drain ({}s elapsed)...",
                    remaining_budget.as_secs(),
                    active,
                    elapsed.as_secs()
                );
                let deadline = tokio::time::Instant::now() + remaining_budget;
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
            } else if active > 0 {
                warn!(
                    "No remaining drain budget for {} active connection(s) (HTTP drain consumed full budget)",
                    active
                );
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
                Ok(_) => {
                    info!("Admin event listener stopped");
                }
                Err(_) => {
                    warn!("Admin event listener did not stop within 5s, proceeding");
                }
            }
        }

        // Shut down remaining infrastructure components
        self.shutdown_components().await;

        // Centralized shutdown: cancel tokens -> drain tasks -> run hooks
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

        // Minor fix: Removed redundant `registry.unregister()` call.
        // `ClusterManager::shutdown()` already calls `registry.unregister()` during
        // heartbeat state cleanup. Calling it again here was a no-op (the node is
        // already deregistered) but added unnecessary Redis round-trip and log noise.

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
    async fn start_grpc_server(
        &self,
        shutdown_rx: watch::Receiver<bool>,
    ) -> anyhow::Result<JoinHandle<()>> {
        let config = self.config.clone();
        let cluster_manager = self.services.cluster_manager.clone();

        // Pre-bind gRPC listener to catch port-in-use errors before spawning the task
        let grpc_address = config.grpc_address();
        let grpc_addr: std::net::SocketAddr = grpc_address
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid gRPC address '{grpc_address}': {e}"))?;
        let grpc_listener = tokio::net::TcpListener::bind(grpc_addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind gRPC address {grpc_addr}: {e}"))?;
        info!("gRPC server listening on {}", grpc_addr);

        let services = self.services.clone();
        let handle = tokio::spawn(async move {
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
                chat_service: Some(services.chat_service),
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
                turn_health_checker: services.turn_health_checker.clone(),
                credential_encryption: services.credential_encryption.clone(),
                grpc_listener: Some(grpc_listener),
            };
            if let Err(e) = synctv_api::grpc::serve(grpc_config).await {
                error!("gRPC server error: {}", e);
            }
        });

        Ok(handle)
    }

    /// Start HTTP server with graceful shutdown support
    async fn start_http_server(
        &self,
        shutdown_rx: watch::Receiver<bool>,
    ) -> anyhow::Result<JoinHandle<()>> {
        let http_address = self.config.http_address();
        let user_service = self.services.user_service.clone();
        let room_service = self.services.room_service.clone();
        let provider_instance_manager = self.services.provider_instance_manager.clone();
        let user_provider_credential_repository =
            self.services.user_provider_credential_repository.clone();
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

        let is_cluster_mode = self.config.cluster.enabled;
        let ws_ticket_service =
            build_ws_ticket_service(self.services.redis_conn.clone(), is_cluster_mode)?;

        let http_router =
            synctv_api::http::create_router_from_config(synctv_api::http::RouterConfig {
                config: Arc::new(self.config.clone()),
                user_service,
                room_service,
                provider_instance_manager,
                user_provider_credential_repository,
                providers: self.services.providers.clone(),
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
                chat_service: Some(self.services.chat_service.clone()),
                audit_service: self.services.audit_service.clone(),
                live_streaming_infrastructure,
                rate_limiter: self.services.rate_limiter.clone(),
                ws_ticket_service,
                redis_conn: self.services.redis_conn.clone(),
                builtin_stun_url: self.services.stun_server.as_ref().map(|s| {
                    let addr = s.external_addr();
                    format!("stun:{}:{}", addr.ip(), addr.port())
                }),
                turn_health_checker: self.services.turn_health_checker.clone(),
                credential_encryption: self.services.credential_encryption.clone(),
                messaging_rate_limit_config: self.services.rate_limit_config.clone(),
                providers_manager: Some(self.services.providers_manager.clone()),
            });

        // Parse and bind HTTP address before spawning the task to propagate errors properly
        let http_addr: std::net::SocketAddr = http_address
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid HTTP address '{http_address}': {e}"))?;

        let listener = tokio::net::TcpListener::bind(http_addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind HTTP address {http_addr}: {e}"))?;

        info!("HTTP server listening on {}", http_addr);

        let handle = tokio::spawn(async move {
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
///
/// On Unix systems, also handles SIGHUP for log rotation support.
/// SIGHUP does NOT trigger shutdown - it is logged for awareness only,
/// allowing external log rotation tools (logrotate, etc.) to work correctly.
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

    // SIGHUP handler for log rotation support (Unix only)
    // This does NOT trigger shutdown - it just logs the signal for awareness.
    // External tools like logrotate send SIGHUP after rotating log files.
    #[cfg(unix)]
    let sighup = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
            Ok(mut signal) => {
                // Keep receiving SIGHUP signals without shutting down
                loop {
                    signal.recv().await;
                    info!("Received SIGHUP signal (log rotation notification)");
                }
            }
            Err(e) => {
                error!("Failed to install SIGHUP handler: {}", e);
            }
        }
    };

    #[cfg(not(unix))]
    let sighup = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => { info!("Received Ctrl+C"); }
        () = terminate => { info!("Received SIGTERM"); }
        () = sighup => { /* SIGHUP never completes - it loops forever */ }
    }
}

#[cfg(test)]
mod tests {
    use super::build_ws_ticket_service;

    /// Test that invalid HTTP address format returns an error
    #[test]
    fn test_invalid_http_address_returns_error() {
        // Test various invalid address formats
        let invalid_addresses = vec![
            "not a valid address",
            "256.256.256.256:8080", // Invalid IP
            ":invalid_port",
            "localhost:notaport",
            "",
        ];

        for addr in invalid_addresses {
            let result: Result<std::net::SocketAddr, _> = addr.parse();
            assert!(
                result.is_err(),
                "Expected '{addr}' to be invalid, but it parsed successfully"
            );
        }
    }

    /// Test that valid HTTP address formats parse correctly
    #[test]
    fn test_valid_http_address_parses() {
        let valid_addresses = vec![
            "127.0.0.1:8080",
            "0.0.0.0:80",
            "[::1]:8080",
            "[::]:80",
            "192.168.1.1:3000",
        ];

        for addr in valid_addresses {
            let result: Result<std::net::SocketAddr, _> = addr.parse();
            assert!(
                result.is_ok(),
                "Expected '{addr}' to be valid, but it failed to parse"
            );
        }
    }

    /// Test binding to an already-bound port fails
    #[tokio::test]
    async fn test_bind_to_already_bound_port_fails() {
        // Bind to a port first
        let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener1 = tokio::net::TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener1.local_addr().unwrap();

        // Attempting to bind to the same address should fail
        let result = tokio::net::TcpListener::bind(bound_addr).await;
        assert!(
            result.is_err(),
            "Expected binding to already-bound port {bound_addr} to fail"
        );

        // Clean up
        drop(listener1);
    }

    /// Test that binding to an available port succeeds
    #[tokio::test]
    async fn test_bind_to_available_port_succeeds() {
        // Binding to port 0 lets the OS assign an available port
        let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let result = tokio::net::TcpListener::bind(addr).await;
        assert!(
            result.is_ok(),
            "Expected binding to port 0 (OS-assigned) to succeed"
        );
    }

    #[test]
    fn test_ws_ticket_service_uses_memory_in_standalone_without_redis() {
        let service = build_ws_ticket_service(None, false)
            .expect("standalone mode should allow memory-backed ws tickets")
            .expect("ws ticket service should be configured");

        assert_eq!(service.backend_name(), "memory");
    }

    #[test]
    fn test_ws_ticket_service_rejects_memory_backend_in_cluster_mode() {
        let error = build_ws_ticket_service(None, true)
            .expect_err("cluster mode must not fall back to memory-backed ws tickets");

        assert!(
            error.to_string().contains("Redis is required"),
            "Unexpected error: {error}"
        );
    }

    /// Test that SIGHUP signal handler can be installed on Unix systems
    #[cfg(unix)]
    #[tokio::test]
    async fn test_sighup_handler_can_be_installed() {
        // Verify that we can successfully register a SIGHUP handler
        let result = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup());
        assert!(
            result.is_ok(),
            "Failed to install SIGHUP handler: {:?}",
            result.err()
        );
    }

    /// Test that SIGTERM signal handler can be installed on Unix systems
    #[cfg(unix)]
    #[tokio::test]
    async fn test_sigterm_handler_can_be_installed() {
        // Verify that we can successfully register a SIGTERM handler
        let result = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        assert!(
            result.is_ok(),
            "Failed to install SIGTERM handler: {:?}",
            result.err()
        );
    }

    /// Test that SIGHUP does not immediately complete (it loops forever)
    /// This test uses tokio::time::timeout to verify the handler keeps running
    #[cfg(unix)]
    #[tokio::test]
    async fn test_sighup_handler_does_not_complete_immediately() {
        use tokio::time::{timeout, Duration};

        let mut sighup_signal =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .expect("Failed to install SIGHUP handler");

        // The signal.recv() should not complete without an actual SIGHUP being sent
        let result = timeout(Duration::from_millis(100), sighup_signal.recv()).await;
        assert!(
            result.is_err(),
            "SIGHUP handler should not complete without receiving a signal"
        );
    }
}
