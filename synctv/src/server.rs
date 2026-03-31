//! Server lifecycle management
//!
//! Manages the startup and shutdown of all server components:
//! - unified API server (REST/gRPC)
//! - RTMP livestream server

use async_trait::async_trait;
use sqlx::PgPool;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use synctv_cluster::sync::ClusterEvent;
use synctv_core::{
    cache::UserCache,
    repository::UserProviderCredentialRepository,
    service::{RoomService, UserService},
    Config,
};

use crate::app::ClusterActivation;
use crate::shutdown::ShutdownCoordinator;

/// Livestream server state (held for graceful shutdown).
///
/// Dropping the handle stops the `StreamHub` event loop and all dependent tasks.
pub struct LivestreamState {
    pub handle: synctv_livestream::livestream::LivestreamHandle,
}

#[async_trait]
trait LivestreamShutdown {
    async fn shutdown_for_server(&mut self, timeout_secs: u64) -> bool;
}

#[async_trait]
impl LivestreamShutdown for LivestreamState {
    async fn shutdown_for_server(&mut self, timeout_secs: u64) -> bool {
        self.handle.shutdown_graceful(timeout_secs).await
    }
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
    pub user_cache: Arc<UserCache>,
    pub live_streaming_infrastructure:
        Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub stun_server: Option<Arc<synctv_core::service::StunServer>>,
    pub turn_health_checker: Option<Arc<synctv_core::service::TurnHealthChecker>>,
    pub node_registry: Option<Arc<synctv_cluster::discovery::NodeRegistry>>,
    pub health_monitor: Option<Arc<synctv_cluster::discovery::HealthMonitor>>,
    pub(crate) cluster_activation: Option<ClusterActivation>,
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
    api_handle: Option<JoinHandle<anyhow::Result<()>>>,
}

const STARTUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

fn build_proxy_slice_cache_config(
    _settings_registry: &Arc<synctv_core::service::SettingsRegistry>,
) -> synctv_proxy::slice_cache::SliceCacheConfig {
    synctv_proxy::slice_cache::SliceCacheConfig {
        // Runtime enablement is decided per request from SettingsRegistry.
        // Keep the cache engine itself available so toggling the setting does
        // not require a process restart.
        enabled: true,
        ..synctv_proxy::slice_cache::SliceCacheConfig::default()
    }
}

fn build_ws_ticket_service(
    redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
    redis_key_prefix: &str,
    is_cluster_mode: bool,
) -> anyhow::Result<Arc<synctv_core::service::WsTicketService>> {
    let svc = match (is_cluster_mode, redis_conn) {
        (_, Some(shared_conn)) => {
            synctv_core::service::WsTicketService::with_redis(shared_conn, redis_key_prefix, None)
        }
        (true, None) => {
            return Err(anyhow::anyhow!(
                "cluster.enabled=true requires Redis-backed WebSocket ticket service wiring"
            ));
        }
        (false, None) => synctv_core::service::WsTicketService::with_memory(None),
    };
    Ok(Arc::new(svc))
}

async fn await_task_shutdown(name: &'static str, mut handle: JoinHandle<()>, timeout: Duration) {
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(Ok(())) => info!("{name} stopped"),
        Ok(Err(e)) => warn!("{name} panicked during shutdown: {e}"),
        Err(_) => {
            warn!(
                "{name} did not stop within {}s, aborting task",
                timeout.as_secs()
            );
            handle.abort();
            match handle.await {
                Ok(()) => info!("{name} aborted cleanly"),
                Err(e) if e.is_cancelled() => info!("{name} aborted"),
                Err(e) => warn!("{name} failed after abort: {e}"),
            }
        }
    }
}

fn map_runtime_server_exit(
    name: &'static str,
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
) -> anyhow::Result<()> {
    match result {
        Ok(Ok(())) => Err(anyhow::anyhow!(
            "{name} stopped unexpectedly without an error"
        )),
        Ok(Err(err)) => Err(anyhow::anyhow!("{name} stopped unexpectedly: {err}")),
        Err(err) if err.is_cancelled() => Err(anyhow::anyhow!("{name} task was cancelled")),
        Err(err) => Err(anyhow::anyhow!("{name} task panicked: {err}")),
    }
}

fn map_background_task_exit(
    name: &'static str,
    result: Result<(), tokio::task::JoinError>,
) -> anyhow::Result<()> {
    match result {
        Ok(()) => Err(anyhow::anyhow!(
            "{name} stopped unexpectedly without an error"
        )),
        Err(err) if err.is_cancelled() => Err(anyhow::anyhow!("{name} task was cancelled")),
        Err(err) => Err(anyhow::anyhow!("{name} task panicked: {err}")),
    }
}

async fn await_runtime_server_shutdown(
    name: &'static str,
    handle: JoinHandle<anyhow::Result<()>>,
    timeout: Duration,
) {
    if timeout == Duration::ZERO {
        match handle.await {
            Ok(Ok(())) => info!("{name} stopped"),
            Ok(Err(err)) => warn!("{name} stopped with error during shutdown: {err}"),
            Err(err) if err.is_cancelled() => info!("{name} task cancelled during shutdown"),
            Err(err) => warn!("{name} panicked during shutdown: {err}"),
        }
        return;
    }

    let mut handle = handle;
    if let Ok(join_result) = tokio::time::timeout(timeout, &mut handle).await {
        match join_result {
            Ok(Ok(())) => info!("{name} stopped"),
            Ok(Err(err)) => warn!("{name} stopped with error during shutdown: {err}"),
            Err(err) if err.is_cancelled() => info!("{name} task cancelled during shutdown"),
            Err(err) => warn!("{name} panicked during shutdown: {err}"),
        }
    } else {
        warn!(
            "{name} did not stop within {}s, aborting task",
            timeout.as_secs()
        );
        handle.abort();
        match handle.await {
            Ok(Ok(())) => info!("{name} aborted cleanly"),
            Ok(Err(err)) => warn!("{name} returned error after abort: {err}"),
            Err(err) if err.is_cancelled() => info!("{name} aborted"),
            Err(err) => warn!("{name} failed after abort: {err}"),
        }
    }
}

async fn force_abort_runtime_server(name: &'static str, handle: JoinHandle<anyhow::Result<()>>) {
    warn!("{name} exceeded the remaining shutdown budget, aborting task");
    handle.abort();
    match handle.await {
        Ok(Ok(())) => info!("{name} aborted cleanly"),
        Ok(Err(err)) => warn!("{name} returned error after forced abort: {err}"),
        Err(err) if err.is_cancelled() => info!("{name} aborted"),
        Err(err) => warn!("{name} failed after forced abort: {err}"),
    }
}

fn remaining_budget(deadline: tokio::time::Instant) -> Duration {
    deadline.saturating_duration_since(tokio::time::Instant::now())
}

async fn shutdown_livestream_state<T>(livestream_state: &mut Option<T>, timeout_secs: u64)
where
    T: LivestreamShutdown + Send,
{
    if let Some(state) = livestream_state.as_mut() {
        info!("Stopping livestream infrastructure...");
        let graceful = state.shutdown_for_server(timeout_secs).await;
        if !graceful {
            warn!("Livestream infrastructure required force-abort during shutdown");
        }
        info!("Livestream infrastructure shut down");
    }
}

async fn shutdown_runtime_phase(
    api_handle: Option<JoinHandle<anyhow::Result<()>>>,
    cleanup_handle: JoinHandle<()>,
    total_budget: Duration,
) {
    let deadline = tokio::time::Instant::now() + total_budget;

    info!(
        "Waiting up to {}s for API server and cleanup task to stop...",
        total_budget.as_secs()
    );

    if let Some(api_handle) = api_handle {
        let budget = remaining_budget(deadline);
        if budget.is_zero() {
            force_abort_runtime_server("API server", api_handle).await;
        } else {
            await_runtime_server_shutdown("API server", api_handle, budget).await;
        }
    }

    await_task_shutdown(
        "connection cleanup task",
        cleanup_handle,
        remaining_budget(deadline),
    )
    .await;
}

async fn cleanup_partial_startup(
    shutdown_tx: &watch::Sender<bool>,
    cleanup_cancel: &tokio_util::sync::CancellationToken,
    cleanup_handle: Option<JoinHandle<()>>,
    api_handle: Option<JoinHandle<anyhow::Result<()>>>,
    deadline: tokio::time::Instant,
) {
    let _ = shutdown_tx.send(true);
    cleanup_cancel.cancel();

    if let Some(handle) = cleanup_handle {
        await_task_shutdown(
            "connection cleanup task",
            handle,
            remaining_budget(deadline).min(STARTUP_CLEANUP_TIMEOUT),
        )
        .await;
    }

    if let Some(handle) = api_handle {
        let timeout = remaining_budget(deadline).min(STARTUP_CLEANUP_TIMEOUT);
        if timeout.is_zero() {
            force_abort_runtime_server("API server", handle).await;
        } else {
            await_runtime_server_shutdown("API server", handle, timeout).await;
        }
    }
}

async fn shutdown_after_startup_failure(
    shutdown_tx: &watch::Sender<bool>,
    cleanup_cancel: &tokio_util::sync::CancellationToken,
    cleanup_handle: Option<JoinHandle<()>>,
    api_handle: Option<JoinHandle<anyhow::Result<()>>>,
    deadline: tokio::time::Instant,
    component_cleanup: impl std::future::Future<Output = ()> + Send,
    coordinator: ShutdownCoordinator,
) {
    cleanup_partial_startup(
        shutdown_tx,
        cleanup_cancel,
        cleanup_handle,
        api_handle,
        deadline,
    )
    .await;
    component_cleanup.await;
    coordinator.shutdown_with_deadline(deadline).await;
}

async fn shutdown_after_cluster_activation_failure(
    server: &mut SyncTvServer,
    context: ClusterActivationFailureShutdown,
) {
    let ClusterActivationFailureShutdown {
        shutdown_tx,
        cleanup_cancel,
        cleanup_handle,
        api_handle,
        deadline,
        coordinator,
    } = context;

    let _ = shutdown_tx.send(true);
    cleanup_cancel.cancel();

    if let Some(handle) = cleanup_handle {
        await_task_shutdown(
            "connection cleanup task",
            handle,
            remaining_budget(deadline).min(STARTUP_CLEANUP_TIMEOUT),
        )
        .await;
    }

    if let Some(handle) = api_handle {
        let timeout = remaining_budget(deadline).min(STARTUP_CLEANUP_TIMEOUT);
        if timeout.is_zero() {
            force_abort_runtime_server("API server", handle).await;
        } else {
            await_runtime_server_shutdown("API server", handle, timeout).await;
        }
    }

    server.shutdown_startup_failure_components(deadline).await;
    coordinator.shutdown_with_deadline(deadline).await;
}

struct ClusterActivationFailureShutdown {
    shutdown_tx: watch::Sender<bool>,
    cleanup_cancel: tokio_util::sync::CancellationToken,
    cleanup_handle: Option<JoinHandle<()>>,
    api_handle: Option<JoinHandle<anyhow::Result<()>>>,
    deadline: tokio::time::Instant,
    coordinator: ShutdownCoordinator,
}

async fn spawn_admin_event_listener(
    cluster_mgr: Arc<synctv_cluster::sync::ClusterManager>,
    infra: Arc<synctv_livestream::api::LiveStreamingInfrastructure>,
    cancel: tokio_util::sync::CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut admin_rx = cluster_mgr.subscribe_admin_events();
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    info!("Admin event listener cancelled");
                    break;
                }
                recv = admin_rx.recv() => {
                    match recv {
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
                                    infra.kick_stream(room_id.as_str(), media_id.as_str()).await
                                {
                                    warn!(
                                        room_id = %room_id.as_str(),
                                        media_id = %media_id.as_str(),
                                        error = %e,
                                        "Failed to kick publisher from cluster admin event"
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
                            ClusterEvent::KickUserFromRoom {
                                room_id,
                                user_id,
                                reason,
                                ..
                            } => {
                                info!(
                                    room_id = %room_id.as_str(),
                                    user_id = %user_id.as_str(),
                                    reason = %reason,
                                    "Received room-scoped user kick"
                                );
                                infra
                                    .kick_user_room_publishers(room_id.as_str(), user_id.as_str())
                                    .await;
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
            }
        }
    })
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
            api_handle: None,
        }
    }

    /// Start all servers and wait for shutdown signal, using a `ShutdownCoordinator`
    /// for centralized shutdown orchestration.
    pub async fn start_with_coordinator(
        self,
        coordinator: ShutdownCoordinator,
    ) -> anyhow::Result<()> {
        self.start_with_coordinator_and_shutdown_signal(coordinator, shutdown_signal())
            .await
    }

    /// Start all servers and wait for an externally supplied shutdown signal.
    ///
    /// This is primarily used by integration tests that need to start the full
    /// process in-process and stop it deterministically without sending OS signals.
    pub async fn start_with_coordinator_and_shutdown_signal<F>(
        mut self,
        coordinator: ShutdownCoordinator,
        shutdown_signal: F,
    ) -> anyhow::Result<()>
    where
        F: Future<Output = ()> + Send,
    {
        info!("Starting SyncTV server...");

        // Create shutdown signal channel
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        tokio::pin!(shutdown_signal);

        // Log infrastructure state
        if self.livestream_state.is_some() {
            info!("Livestream infrastructure: enabled");
        }
        if self.services.stun_server.is_some() {
            info!("STUN server: enabled");
        }

        // Start background connection cleanup (every 60 seconds)
        let cleanup_cancel = tokio_util::sync::CancellationToken::new();
        let cleanup_handle = self
            .services
            .connection_manager
            .spawn_cleanup_task(Duration::from_mins(1), cleanup_cancel.clone());

        // Start unified API server (single listener for REST + gRPC)
        let api_handle = match self.start_api_server(shutdown_rx.clone()).await {
            Ok(handle) => handle,
            Err(err) => {
                let startup_cleanup_budget =
                    Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds)
                        .min(STARTUP_CLEANUP_TIMEOUT);
                let startup_cleanup_deadline = tokio::time::Instant::now() + startup_cleanup_budget;
                shutdown_after_startup_failure(
                    &shutdown_tx,
                    &cleanup_cancel,
                    Some(cleanup_handle),
                    None,
                    startup_cleanup_deadline,
                    self.shutdown_startup_failure_components(startup_cleanup_deadline),
                    coordinator,
                )
                .await;
                info!("Closing database connection pool after startup failure...");
                self.pool.close().await;
                info!("Database pool closed after startup failure");
                return Err(err);
            }
        };
        self.api_handle = Some(api_handle);

        if let Some(cluster_activation) = &self.services.cluster_activation {
            if let Err(err) = crate::bootstrap::cluster::activate_cluster_node(
                &cluster_activation.config,
                &cluster_activation.cluster_manager,
                &cluster_activation.connection_manager,
                &cluster_activation.node_registry,
                &cluster_activation.health_monitor,
            )
            .await
            {
                let api_handle = self.api_handle.take();
                let startup_cleanup_budget =
                    Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds)
                        .min(STARTUP_CLEANUP_TIMEOUT);
                let startup_cleanup_deadline = tokio::time::Instant::now() + startup_cleanup_budget;
                shutdown_after_cluster_activation_failure(
                    &mut self,
                    ClusterActivationFailureShutdown {
                        shutdown_tx: shutdown_tx.clone(),
                        cleanup_cancel: cleanup_cancel.clone(),
                        cleanup_handle: Some(cleanup_handle),
                        api_handle,
                        deadline: startup_cleanup_deadline,
                        coordinator,
                    },
                )
                .await;
                info!("Closing database connection pool after startup failure...");
                self.pool.close().await;
                info!("Database pool closed after startup failure");
                return Err(err);
            }
        }

        // Spawn streaming event listener for cluster-wide kicks
        let admin_event_cancel = tokio_util::sync::CancellationToken::new();
        let admin_event_handle: Option<JoinHandle<()>> = if let (Some(cluster_mgr), Some(infra)) = (
            &self.services.cluster_manager,
            &self.services.live_streaming_infrastructure,
        ) {
            let handle = spawn_admin_event_listener(
                Arc::clone(cluster_mgr),
                Arc::clone(infra),
                admin_event_cancel.clone(),
            )
            .await;
            info!("Admin event listener spawned for cluster-wide stream kicks");
            Some(handle)
        } else {
            None
        };

        info!("All servers started successfully");

        // Wait for either a server to stop or a shutdown signal
        let mut api_handle = Some(
            self.api_handle
                .take()
                .ok_or_else(|| anyhow::anyhow!("API server handle missing after startup"))?,
        );

        let (unexpected_exit, api_handle) = tokio::select! {
            result = async {
                api_handle
                    .as_mut()
                    .expect("API server handle should be present before select")
                    .await
            } => {
                let _ = api_handle.take();
                (Some(map_runtime_server_exit("API server", result)), None)
            },
            () = &mut shutdown_signal => {
                info!("External shutdown signal received, starting graceful shutdown...");
                (None, api_handle.take())
            }
        };

        // Signal API server to shut down
        let _ = shutdown_tx.send(true);
        cleanup_cancel.cancel();

        // D6 fix: Track total shutdown start time to compute remaining budget for
        // each phase. The total drain budget is `shutdown_drain_timeout_seconds`.
        // Previously, both HTTP drain and connection drain each used the full
        // timeout, potentially exceeding K8s grace period (2x the configured value).
        let shutdown_start = tokio::time::Instant::now();
        let total_drain_budget =
            Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds);

        // Phase 1: Wait for unified API server to finish (use 60% of budget).
        let http_drain_budget = total_drain_budget * 60 / 100;
        info!(
            "Waiting up to {}s for API server to shut down...",
            http_drain_budget.as_secs()
        );
        shutdown_runtime_phase(api_handle, cleanup_handle, http_drain_budget).await;
        info!("API server shut down");

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
            admin_event_cancel.cancel();
            info!("Waiting for admin event listener to stop...");
            await_task_shutdown(
                "admin event listener",
                handle,
                total_drain_budget.saturating_sub(shutdown_start.elapsed()),
            )
            .await;
        }

        // Shut down remaining infrastructure components
        self.shutdown_components(total_drain_budget.saturating_sub(shutdown_start.elapsed()))
            .await;

        // Centralized shutdown: cancel tokens -> drain tasks -> run hooks
        coordinator
            .shutdown_with_deadline(shutdown_start + total_drain_budget)
            .await;

        // Close the database connection pool (after audit flush and settings task)
        info!("Closing database connection pool...");
        self.pool.close().await;
        info!("Database pool closed");

        info!("SyncTV server shut down complete");
        if let Some(result) = unexpected_exit {
            return result;
        }
        Ok(())
    }

    /// Shut down infrastructure components (STUN, livestream, health monitor, node registry, connection manager).
    ///
    /// This is separate from the `ShutdownCoordinator` because these components
    /// have custom shutdown protocols (not just cancellation tokens or join handles).
    async fn shutdown_components(&mut self, budget_remaining: Duration) {
        let deadline = tokio::time::Instant::now() + budget_remaining;

        // Shut down connection manager (stops TTL refresh background task)
        info!("Shutting down connection manager...");
        self.services.connection_manager.shutdown().await;
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
        let livestream_budget = remaining_budget(deadline);
        shutdown_livestream_state(&mut self.livestream_state, livestream_budget.as_secs()).await;

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

    async fn shutdown_startup_failure_components(&mut self, deadline: tokio::time::Instant) {
        if let Some(ref cluster_mgr) = self.services.cluster_manager {
            info!("Shutting down cluster manager during startup rollback...");
            let timeout = remaining_budget(deadline);
            if timeout.is_zero() {
                warn!("Skipping cluster manager shutdown during startup rollback: no budget left");
            } else if tokio::time::timeout(timeout, cluster_mgr.shutdown())
                .await
                .is_ok()
            {
                info!("Cluster manager shut down during startup rollback");
            } else {
                warn!("Cluster manager shutdown exceeded startup rollback budget");
            }
        }

        self.shutdown_components(remaining_budget(deadline)).await;
    }

    /// Start unified REST + gRPC API server with graceful shutdown support
    async fn start_api_server(
        &self,
        shutdown_rx: watch::Receiver<bool>,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
        let api_address = self.config.api_address();
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

        let is_cluster_mode = self.config.cluster_runtime_enabled();
        let ws_ticket_service = build_ws_ticket_service(
            self.services.redis_conn.clone(),
            &self.config.redis.key_prefix,
            is_cluster_mode,
        )?;
        let proxy_http_client = synctv_proxy::build_proxy_http_client()?;
        let proxy_slice_cache = Arc::new(synctv_proxy::slice_cache::SliceCache::new_with_client(
            build_proxy_slice_cache_config(&self.services.settings_registry),
            proxy_http_client.clone(),
        ));

        let (http_router, http_state) = synctv_api::http::create_router_with_state_from_config(
            synctv_api::http::RouterConfig {
                config: Arc::new(self.config.clone()),
                user_service,
                user_cache: self.services.user_cache.clone(),
                room_service,
                content_filter: self.services.content_filter.clone(),
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
                proxy_slice_cache: proxy_slice_cache.clone(),
                proxy_http_client: proxy_http_client.clone(),
                messaging_rate_limit_config: synctv_core::service::RateLimitConfig {
                    chat_per_second: self.config.messaging_rate_limits.chat_per_second,
                    danmaku_per_second: self.config.messaging_rate_limits.danmaku_per_second,
                    window_seconds: self.config.messaging_rate_limits.window_seconds,
                },
                heartbeat_schedule: synctv_api::impls::HeartbeatSchedule::production(),
                providers_manager: Some(self.services.providers_manager.clone()),
            },
        )?;
        let grpc_router = synctv_api::grpc::build_axum_router(synctv_api::grpc::GrpcServerConfig {
            config: &self.config,
            jwt_service: self.services.jwt_service.clone(),
            user_service: self.services.user_service.clone(),
            user_cache: self.services.user_cache.clone(),
            room_service: self.services.room_service.clone(),
            cluster_manager: self.services.cluster_manager.clone(),
            redis_publish_tx: self.services.redis_publish_tx.clone(),
            rate_limiter: self.services.rate_limiter.clone(),
            rate_limit_config: self.services.rate_limit_config.clone(),
            content_filter: self.services.content_filter.clone(),
            connection_manager: self.services.connection_manager.clone(),
            providers_manager: Some(self.services.providers_manager.clone()),
            provider_instance_manager: self.services.provider_instance_manager.clone(),
            user_provider_credential_repository: self
                .services
                .user_provider_credential_repository
                .clone(),
            settings_service: self.services.settings_service.clone(),
            settings_registry: Some(self.services.settings_registry.clone()),
            email_service: self.services.email_service.clone(),
            email_token_service: self.services.email_token_service.clone(),
            live_streaming_infrastructure: self.services.live_streaming_infrastructure.clone(),
            publish_key_service: Some(self.services.publish_key_service.clone()),
            notification_service: self.services.notification_service.clone(),
            chat_service: Some(self.services.chat_service.clone()),
            oauth2_service: self.services.oauth2_service.clone(),
            audit_service: self.services.audit_service.clone(),
            node_registry: self.services.node_registry.clone(),
            redis_client: self.services.redis_client.clone(),
            redis_conn: self.services.redis_conn.clone(),
            shutdown_rx: Some(shutdown_rx.clone()),
            builtin_stun_url: self.services.stun_server.as_ref().map(|s| {
                let addr = s.external_addr();
                format!("stun:{}:{}", addr.ip(), addr.port())
            }),
            turn_health_checker: self.services.turn_health_checker.clone(),
            credential_encryption: self.services.credential_encryption.clone(),
            grpc_listener: None,
        })
        .await?;

        // Parse and bind unified API address before spawning the task to propagate errors properly
        let http_addr: std::net::SocketAddr = api_address
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid API address '{api_address}': {e}"))?;

        let listener = tokio::net::TcpListener::bind(http_addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind HTTP address {http_addr}: {e}"))?;

        info!("API server listening on {}", http_addr);

        let handle = tokio::spawn(async move {
            let mut rx = shutdown_rx;
            let proxy_cache_lifecycle =
                synctv_api::http::start_proxy_cache_lifecycle(http_state.proxy_slice_cache.clone());
            let graceful = async move {
                let _ = rx.changed().await;
            };

            let server = axum::serve(
                listener,
                http_router
                    .merge(grpc_router)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(graceful);

            let server_result = if let Some(lifecycle) = proxy_cache_lifecycle {
                let mut lifecycle_handle = lifecycle.handle;
                let lifecycle_cancel = lifecycle.cancel;

                let result = tokio::select! {
                    server_result = server => {
                        lifecycle_cancel.cancel();
                        let _ = lifecycle_handle.await;
                        server_result
                    }
                    lifecycle_result = &mut lifecycle_handle => {
                            lifecycle_cancel.cancel();
                            return map_background_task_exit(
                            "API proxy cache lifecycle",
                            lifecycle_result,
                        );
                    }
                };

                result
            } else {
                server.await
            };

            server_result.map_err(|e| anyhow::anyhow!("API server error: {e}"))?;

            info!("API server shut down gracefully");
            Ok(())
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
    use super::{
        await_runtime_server_shutdown, await_task_shutdown, build_proxy_slice_cache_config,
        build_ws_ticket_service, cleanup_partial_startup, map_background_task_exit,
        map_runtime_server_exit, shutdown_after_startup_failure, shutdown_livestream_state,
        shutdown_runtime_phase, spawn_admin_event_listener, LivestreamShutdown,
    };
    use crate::shutdown::ShutdownCoordinator;
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::Duration;
    use tokio::sync::{oneshot, watch};
    use tokio_util::sync::CancellationToken;

    fn test_settings_registry() -> Arc<synctv_core::service::SettingsRegistry> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");
        let settings_service = Arc::new(synctv_core::service::SettingsService::new(
            synctv_core::repository::SettingsRepository::new(pool.clone()),
            pool,
        ));
        Arc::new(synctv_core::service::SettingsRegistry::new(
            settings_service,
        ))
    }

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
        let addr: std::net::SocketAddr = "127.0.0.1:0"
            .parse()
            .expect("test socket address literal must parse");
        let listener1 = match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!(
                    "skipping bind test: local TCP listen is not permitted in this environment"
                );
                return;
            }
            Err(error) => panic!("expected initial bind to succeed, got: {error}"),
        };
        let bound_addr = listener1
            .local_addr()
            .expect("bound TCP listener must expose its local address");

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
        let addr: std::net::SocketAddr = "127.0.0.1:0"
            .parse()
            .expect("test socket address literal must parse");
        match tokio::net::TcpListener::bind(addr).await {
            Ok(_listener) => {}
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!(
                    "skipping bind test: local TCP listen is not permitted in this environment"
                );
            }
            Err(error) => {
                panic!("Expected binding to port 0 (OS-assigned) to succeed, got: {error}")
            }
        }
    }

    #[test]
    fn test_ws_ticket_service_uses_memory_in_standalone_without_redis() {
        let service = build_ws_ticket_service(None, "synctv:", false)
            .expect("standalone mode should allow memory-backed ws tickets");

        assert_eq!(service.backend_name(), "memory");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ws_ticket_service_uses_redis_backend_when_available_in_standalone() {
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::redis::Redis;

        let redis = Redis::default()
            .start()
            .await
            .expect("redis container should start for ws ticket redis backend test");
        let port = redis
            .get_host_port_ipv4(6379)
            .await
            .expect("redis port should be exposed");
        let client = redis::Client::open(format!("redis://127.0.0.1:{port}"))
            .expect("redis client should be created");
        let manager = redis::aio::ConnectionManager::new(client)
            .await
            .expect("redis connection manager should be created");
        let shared = Arc::new(tokio::sync::RwLock::new(manager));

        let service = build_ws_ticket_service(Some(shared), "synctv:", false)
            .expect("standalone with redis should succeed");

        assert_eq!(service.backend_name(), "redis");
    }

    #[test]
    fn test_ws_ticket_service_rejects_memory_backend_in_cluster_mode() {
        let error = build_ws_ticket_service(None, "synctv:", true)
            .expect_err("cluster mode must not fall back to memory-backed ws tickets");

        assert!(
            error.to_string().contains(
                "cluster.enabled=true requires Redis-backed WebSocket ticket service wiring"
            ),
            "Unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_proxy_slice_cache_runtime_toggle_keeps_engine_available() {
        let registry = test_settings_registry();
        let config = build_proxy_slice_cache_config(&registry);

        assert!(
            config.enabled,
            "proxy slice cache engine must stay available so runtime settings can enable caching without restart"
        );
    }

    #[tokio::test]
    async fn test_cleanup_partial_startup_signals_and_joins_tasks() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let cleanup_cancel = CancellationToken::new();

        let cleanup_stopped = Arc::new(AtomicBool::new(false));
        let cleanup_stopped_clone = Arc::clone(&cleanup_stopped);
        let cleanup_cancel_for_task = cleanup_cancel.clone();
        let cleanup_handle = tokio::spawn(async move {
            cleanup_cancel_for_task.cancelled().await;
            cleanup_stopped_clone.store(true, Ordering::SeqCst);
        });

        let grpc_stopped = Arc::new(AtomicBool::new(false));
        let grpc_stopped_clone = Arc::clone(&grpc_stopped);
        let grpc_handle = tokio::spawn(async move {
            let _ = shutdown_rx.changed().await;
            grpc_stopped_clone.store(true, Ordering::SeqCst);
            Ok::<(), anyhow::Error>(())
        });

        cleanup_partial_startup(
            &shutdown_tx,
            &cleanup_cancel,
            Some(cleanup_handle),
            Some(grpc_handle),
            tokio::time::Instant::now() + Duration::from_secs(5),
        )
        .await;

        assert!(cleanup_cancel.is_cancelled());
        assert!(cleanup_stopped.load(Ordering::SeqCst));
        assert!(grpc_stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_shutdown_after_startup_failure_runs_coordinator_hooks() {
        use crate::shutdown::ShutdownHook;
        use std::future::Future;
        use std::pin::Pin;

        struct FlagHook(Arc<AtomicBool>);

        impl ShutdownHook for FlagHook {
            fn name(&self) -> &str {
                "flag_hook"
            }

            fn timeout(&self) -> Duration {
                Duration::from_secs(1)
            }

            fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
                let flag = Arc::clone(&self.0);
                Box::pin(async move {
                    flag.store(true, Ordering::SeqCst);
                })
            }
        }

        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let cleanup_cancel = CancellationToken::new();
        let cleanup_handle = tokio::spawn(async move {});
        let hook_called = Arc::new(AtomicBool::new(false));
        let component_cleanup_called = Arc::new(AtomicBool::new(false));
        let mut coordinator = ShutdownCoordinator::new(Duration::from_secs(1));
        coordinator.register_hook(FlagHook(Arc::clone(&hook_called)));

        shutdown_after_startup_failure(
            &shutdown_tx,
            &cleanup_cancel,
            Some(cleanup_handle),
            None,
            tokio::time::Instant::now() + Duration::from_secs(1),
            {
                let component_cleanup_called = Arc::clone(&component_cleanup_called);
                async move {
                    component_cleanup_called.store(true, Ordering::SeqCst);
                }
            },
            coordinator,
        )
        .await;

        assert!(
            component_cleanup_called.load(Ordering::SeqCst),
            "startup failure cleanup must run component-specific shutdown before coordinator hooks"
        );
        assert!(
            hook_called.load(Ordering::SeqCst),
            "startup failure cleanup must run the centralized shutdown coordinator"
        );
    }

    #[tokio::test]
    async fn test_shutdown_after_startup_failure_shares_single_deadline() {
        use crate::shutdown::ShutdownHook;
        use std::future::Future;
        use std::pin::Pin;

        struct PendingHook;

        impl ShutdownHook for PendingHook {
            fn name(&self) -> &str {
                "pending_hook"
            }

            fn timeout(&self) -> Duration {
                Duration::from_secs(30)
            }

            fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
                Box::pin(async move {
                    std::future::pending::<()>().await;
                })
            }
        }

        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let cleanup_cancel = CancellationToken::new();
        let cleanup_handle = tokio::spawn(async move {
            std::future::pending::<()>().await;
        });
        let mut coordinator = ShutdownCoordinator::new(Duration::from_secs(30));
        coordinator.register_hook(PendingHook);

        let start = tokio::time::Instant::now();
        shutdown_after_startup_failure(
            &shutdown_tx,
            &cleanup_cancel,
            Some(cleanup_handle),
            None,
            start + Duration::from_millis(50),
            async {},
            coordinator,
        )
        .await;

        assert!(
            start.elapsed() < Duration::from_secs(1),
            "startup rollback must respect a shared absolute deadline"
        );
    }

    #[tokio::test]
    async fn test_await_task_shutdown_aborts_timed_out_task() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_clone = Arc::clone(&dropped);
        let handle = tokio::spawn(async move {
            let _guard = DropFlag(dropped_clone);
            std::future::pending::<()>().await;
        });

        await_task_shutdown("pending task", handle, Duration::from_millis(10)).await;

        assert!(
            dropped.load(Ordering::SeqCst),
            "timed-out task should be aborted rather than detached"
        );
    }

    #[tokio::test]
    async fn test_await_runtime_server_shutdown_zero_timeout_waits_for_graceful_stop() {
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_clone = Arc::clone(&stopped);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            stopped_clone.store(true, Ordering::SeqCst);
            Ok::<(), anyhow::Error>(())
        });

        await_runtime_server_shutdown("graceful server", handle, Duration::ZERO).await;

        assert!(
            stopped.load(Ordering::SeqCst),
            "zero timeout should defer to the outer shutdown budget instead of aborting immediately"
        );
    }

    #[tokio::test]
    async fn test_shutdown_runtime_phase_aborts_stuck_tasks_within_budget() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let api_dropped = Arc::new(AtomicBool::new(false));
        let api_dropped_clone = Arc::clone(&api_dropped);
        let api_handle = tokio::spawn(async move {
            let _guard = DropFlag(api_dropped_clone);
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        });

        let (cleanup_tx, cleanup_rx) = oneshot::channel::<()>();
        let cleanup_handle = tokio::spawn(async move {
            let _ = cleanup_rx.await;
        });

        shutdown_runtime_phase(Some(api_handle), cleanup_handle, Duration::from_millis(60)).await;

        assert!(
            api_dropped.load(Ordering::SeqCst),
            "API task should be aborted within the phase budget"
        );
        assert!(
            cleanup_tx.send(()).is_err(),
            "cleanup task should no longer be running after shutdown phase returns"
        );
    }

    #[tokio::test]
    async fn test_shutdown_livestream_state_uses_graceful_shutdown() {
        struct FakeLivestreamState {
            called: Arc<AtomicBool>,
            timeout_seen: Arc<std::sync::atomic::AtomicU64>,
        }

        #[async_trait]
        impl LivestreamShutdown for FakeLivestreamState {
            async fn shutdown_for_server(&mut self, timeout_secs: u64) -> bool {
                self.called.store(true, Ordering::SeqCst);
                self.timeout_seen.store(timeout_secs, Ordering::SeqCst);
                true
            }
        }

        let called = Arc::new(AtomicBool::new(false));
        let timeout_seen = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut livestream_state = Some(FakeLivestreamState {
            called: Arc::clone(&called),
            timeout_seen: Arc::clone(&timeout_seen),
        });

        shutdown_livestream_state(&mut livestream_state, 17).await;

        assert!(
            called.load(Ordering::SeqCst),
            "server shutdown should invoke graceful livestream shutdown"
        );
        assert_eq!(
            timeout_seen.load(Ordering::SeqCst),
            17,
            "server shutdown must pass through the configured drain timeout"
        );
    }

    #[tokio::test]
    async fn test_admin_event_listener_stops_on_cancel() {
        use synctv_cluster::sync::{ClusterConfig, ClusterManager};
        use synctv_livestream::api::StreamTracker;
        use synctv_livestream::livestream::{ExternalPublishManager, PullStreamManager};
        use synctv_livestream::relay::InMemoryStreamRegistry;
        use tokio::sync::mpsc;

        let cluster_manager = ClusterManager::new(
            ClusterConfig {
                redis_client: None,
                redis_conn: None,
                cluster_enabled: false,
                node_id: "test-node".to_string(),
                dedup_window: Duration::from_mins(1),
                critical_channel_capacity: 8,
                publish_channel_capacity: 8,
                key_prefix: "test:".to_string(),
                catchup_window_secs: 60,
                stream_max_length: 100,
                shared_redis_conn: None,
                parent_cancel_token: None,
            },
            None,
            None,
        )
        .await
        .expect("cluster manager should be created");

        let registry = Arc::new(InMemoryStreamRegistry::new());
        let (event_sender, _event_receiver) = mpsc::channel(8);
        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "test-node".to_string(),
                event_sender.clone(),
            )
            .expect("failed to create ExternalPublishManager"),
        );
        let infra = Arc::new(synctv_livestream::api::LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            pull_manager,
            external_publish_manager,
            Arc::new(StreamTracker::new()),
        ));
        let cancel = CancellationToken::new();
        let handle =
            spawn_admin_event_listener(Arc::new(cluster_manager), infra, cancel.clone()).await;

        cancel.cancel();
        await_task_shutdown("admin event listener", handle, Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn test_admin_event_listener_kick_publisher_removes_registry_entry() {
        use chrono::Utc;
        use synctv_cluster::sync::{ClusterConfig, ClusterEvent, ClusterManager};
        use synctv_core::models::{MediaId, RoomId};
        use synctv_livestream::api::StreamTracker;
        use synctv_livestream::livestream::{ExternalPublishManager, PullStreamManager};
        use synctv_livestream::relay::{InMemoryStreamRegistry, StreamRegistryTrait};
        use tokio::sync::mpsc;

        let cluster_manager = ClusterManager::new(
            ClusterConfig {
                redis_client: None,
                redis_conn: None,
                cluster_enabled: false,
                node_id: "test-node".to_string(),
                dedup_window: Duration::from_mins(1),
                critical_channel_capacity: 8,
                publish_channel_capacity: 8,
                key_prefix: "test:".to_string(),
                catchup_window_secs: 60,
                stream_max_length: 100,
                shared_redis_conn: None,
                parent_cancel_token: None,
            },
            None,
            None,
        )
        .await
        .expect("cluster manager should be created");

        let registry = Arc::new(InMemoryStreamRegistry::new());
        registry
            .try_register_publisher(
                "room-1",
                "media-1",
                "test-node",
                "publisher-user",
                "127.0.0.1:50051",
            )
            .await
            .expect("publisher should register");

        let (event_sender, event_receiver) = mpsc::channel(8);
        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "test-node".to_string(),
                event_sender.clone(),
            )
            .expect("failed to create ExternalPublishManager"),
        );
        let infra = Arc::new(synctv_livestream::api::LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            pull_manager,
            external_publish_manager,
            Arc::new(StreamTracker::new()),
        ));
        let cancel = CancellationToken::new();
        let cluster_manager = Arc::new(cluster_manager);
        let handle =
            spawn_admin_event_listener(cluster_manager.clone(), infra, cancel.clone()).await;

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if cluster_manager
                    .admin_event_tx()
                    .send(ClusterEvent::KickPublisher {
                        event_id: nanoid::nanoid!(16),
                        room_id: RoomId::from_string("room-1".to_string()),
                        media_id: MediaId::from_string("media-1".to_string()),
                        reason: "room_deleted".to_string(),
                        timestamp: Utc::now(),
                    })
                    .is_ok()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("kick publisher event should reach the listener");

        tokio::time::timeout(Duration::from_secs(1), async move {
            let mut rx = event_receiver;
            rx.recv().await
        })
        .await
        .expect("listener should enqueue an unpublish event")
        .expect("streamhub event channel should receive unpublish");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if registry
                    .get_publisher("room-1", "media-1")
                    .await
                    .expect("registry lookup should succeed")
                    .is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("kick listener should remove registry entry after processing");

        cancel.cancel();
        await_task_shutdown("admin event listener", handle, Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn test_admin_event_listener_kick_user_from_room_only_removes_room_local_publishers() {
        use chrono::Utc;
        use synctv_cluster::sync::{ClusterConfig, ClusterEvent, ClusterManager};
        use synctv_core::models::{RoomId, UserId};
        use synctv_livestream::api::StreamTracker;
        use synctv_livestream::livestream::{ExternalPublishManager, PullStreamManager};
        use synctv_livestream::relay::{InMemoryStreamRegistry, StreamRegistryTrait};
        use tokio::sync::mpsc;

        let cluster_manager = ClusterManager::new(
            ClusterConfig {
                redis_client: None,
                redis_conn: None,
                cluster_enabled: false,
                node_id: "test-node".to_string(),
                dedup_window: Duration::from_mins(1),
                critical_channel_capacity: 8,
                publish_channel_capacity: 8,
                key_prefix: "test:".to_string(),
                catchup_window_secs: 60,
                stream_max_length: 100,
                shared_redis_conn: None,
                parent_cancel_token: None,
            },
            None,
            None,
        )
        .await
        .expect("cluster manager should be created");

        let registry = Arc::new(InMemoryStreamRegistry::new());
        registry
            .try_register_publisher(
                "room-1",
                "media-1",
                "test-node",
                "publisher-user",
                "127.0.0.1:50051",
            )
            .await
            .expect("room-1 publisher should register");
        registry
            .try_register_publisher(
                "room-2",
                "media-2",
                "test-node",
                "publisher-user",
                "127.0.0.1:50051",
            )
            .await
            .expect("room-2 publisher should register");

        let tracker = Arc::new(StreamTracker::new());
        tracker.insert(
            "publisher-user".to_string(),
            "room-1".to_string(),
            "media-1".to_string(),
            "room-1",
            "media-1",
        );
        tracker.insert(
            "publisher-user".to_string(),
            "room-2".to_string(),
            "media-2".to_string(),
            "room-2",
            "media-2",
        );

        let (event_sender, mut event_receiver) = mpsc::channel(8);
        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "test-node".to_string(),
                event_sender.clone(),
            )
            .expect("failed to create ExternalPublishManager"),
        );
        let infra = Arc::new(synctv_livestream::api::LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            pull_manager,
            external_publish_manager,
            tracker.clone(),
        ));
        let cancel = CancellationToken::new();
        let cluster_manager = Arc::new(cluster_manager);
        let handle =
            spawn_admin_event_listener(cluster_manager.clone(), infra, cancel.clone()).await;

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if cluster_manager
                    .admin_event_tx()
                    .send(ClusterEvent::KickUserFromRoom {
                        event_id: nanoid::nanoid!(16),
                        room_id: RoomId::from_string("room-1".to_string()),
                        user_id: UserId::from_string("publisher-user".to_string()),
                        reason: "removed".to_string(),
                        timestamp: Utc::now(),
                    })
                    .is_ok()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("kick user from room event should reach the listener");

        tokio::time::timeout(Duration::from_secs(1), async move {
            event_receiver
                .recv()
                .await
                .expect("streamhub event channel should receive unpublish");
        })
        .await
        .expect("listener should enqueue one room-scoped unpublish event");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let room1_missing = registry
                    .get_publisher("room-1", "media-1")
                    .await
                    .expect("registry lookup should succeed")
                    .is_none();
                let room2_present = registry
                    .get_publisher("room-2", "media-2")
                    .await
                    .expect("registry lookup should succeed")
                    .is_some();
                if room1_missing && room2_present {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("room-scoped kick listener should only remove the targeted publisher");

        assert!(
            tracker.get_stream_user("room-1", "media-1").is_none(),
            "target room publisher must be removed from tracker"
        );
        assert_eq!(
            tracker.get_stream_user("room-2", "media-2").as_deref(),
            Some("publisher-user"),
            "publisher in another room must remain tracked"
        );

        cancel.cancel();
        await_task_shutdown("admin event listener", handle, Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn test_select_completion_must_consume_join_result_directly() {
        let handle = tokio::spawn(async { Ok::<(), anyhow::Error>(()) });

        let first = tokio::select! {
            result = handle => result,
        };

        let err = map_runtime_server_exit("HTTP server", first)
            .expect_err("select-completed join result must be handled directly");

        assert!(
            err.to_string()
                .contains("HTTP server stopped unexpectedly without an error"),
            "Unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_runtime_server_exit_ok_is_treated_as_failure() {
        let handle = tokio::spawn(async { Ok::<(), anyhow::Error>(()) });

        let err = map_runtime_server_exit("HTTP server", handle.await)
            .expect_err("unexpected task completion must fail closed");

        assert!(
            err.to_string()
                .contains("HTTP server stopped unexpectedly without an error"),
            "Unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_runtime_server_exit_propagates_inner_error() {
        let handle = tokio::spawn(async {
            Err::<(), anyhow::Error>(anyhow::anyhow!("listener accept loop failed"))
        });

        let err = map_runtime_server_exit("gRPC server", handle.await)
            .expect_err("server task errors must bubble up");

        assert!(
            err.to_string()
                .contains("gRPC server stopped unexpectedly: listener accept loop failed"),
            "Unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_runtime_server_exit_propagates_panic() {
        let handle = tokio::spawn(async move {
            panic!("boom");
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        });

        let err = map_runtime_server_exit("gRPC server", handle.await)
            .expect_err("panics must be surfaced as startup failures");

        assert!(
            err.to_string().contains("gRPC server task panicked"),
            "Unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_background_task_exit_ok_is_treated_as_failure() {
        let handle = tokio::spawn(async {});

        let err = map_background_task_exit("HTTP proxy cache lifecycle", handle.await)
            .expect_err("unexpected background task completion must fail closed");

        assert!(
            err.to_string()
                .contains("HTTP proxy cache lifecycle stopped unexpectedly without an error"),
            "Unexpected error: {err}"
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
