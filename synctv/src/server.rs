//! Server lifecycle management
//!
//! Manages the startup and shutdown of all server components:
//! - unified API server (REST/gRPC)
//! - RTMP livestream server

use anyhow::Context;
use async_trait::async_trait;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use reqwest::Client;
use sqlx::PgPool;
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tower::ServiceExt;
use tracing::{error, info, warn};

use synctv_api::impls::{AdminApiImpl, ClientApiImpl};
use synctv_api::realtime_fanout::RealtimeFanoutService;
use synctv_api::runtime::{RealtimeConnectionService, RealtimeEventService};
use synctv_core::{
    cache::UserCache,
    config::absolute_display_path,
    repository::UserProviderCredentialRepository,
    service::{RoomService, UserService},
    Config, RedisConnectionRuntime,
};
use synctv_management::lifecycle::{ManagementLifecycleController, ShutdownMode};
use synctv_management::server::{spawn_management_server, ManagementServerConfig};
use synctv_realtime::sync::RealtimeEvent;

use crate::bootstrap::cluster::ClusterNodeActivator;
use crate::shutdown::ShutdownCoordinator;

#[cfg(test)]
async fn complete_test_unpublish(
    registry: &Arc<dyn synctv_livestream::relay::StreamRegistryTrait>,
    tracker: &Arc<synctv_livestream::api::StreamTracker>,
    room_id: &str,
    media_id: &str,
) {
    registry
        .unregister_publisher(room_id, media_id)
        .await
        .expect("test unpublish completion should unregister publisher");
    let _ = tracker.remove_stream(room_id, media_id);
}

/// Livestream server state (held for graceful shutdown).
///
/// Dropping the handle stops the `StreamHub` event loop and all dependent tasks.
pub struct LivestreamState {
    pub handle: synctv_livestream::livestream::LivestreamHandle,
    pub infrastructure: Arc<synctv_livestream::api::LiveStreamingInfrastructure>,
}

#[async_trait]
trait LivestreamShutdown {
    async fn cleanup_local_publishers_for_server(&mut self, timeout: Duration);
    fn force_shutdown_for_server(&mut self);
    async fn shutdown_for_server(&mut self, timeout_secs: u64) -> bool;
}

#[async_trait]
impl LivestreamShutdown for LivestreamState {
    async fn cleanup_local_publishers_for_server(&mut self, timeout: Duration) {
        let node_id = self.infrastructure.local_node_id.clone();
        if node_id.is_empty() {
            self.infrastructure.user_stream_tracker.clear();
            return;
        }

        if timeout.is_zero() {
            warn!(
                node_id = %node_id,
                "Skipping local publisher cleanup before livestream shutdown because no shutdown budget remains"
            );
            self.infrastructure.user_stream_tracker.clear();
            return;
        }

        let cleanup_timeout = timeout.min(Duration::from_secs(2));

        match tokio::time::timeout(
            cleanup_timeout,
            self.infrastructure
                .registry
                .cleanup_all_publishers_for_node(&node_id),
        )
        .await
        {
            Ok(Ok(())) => {
                info!(
                    node_id = %node_id,
                    "Cleaned up local publisher registrations before livestream shutdown"
                );
            }
            Ok(Err(error)) => {
                warn!(
                    node_id = %node_id,
                    error = %error,
                    "Failed to cleanup local publisher registrations before livestream shutdown"
                );
            }
            Err(_) => {
                warn!(
                    node_id = %node_id,
                    "Timed out cleaning local publisher registrations before livestream shutdown"
                );
            }
        }

        self.infrastructure.user_stream_tracker.clear();
    }

    fn force_shutdown_for_server(&mut self) {
        self.handle.shutdown();
    }

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
    pub realtime_fanout_service: Arc<dyn RealtimeFanoutService>,
    pub rate_limiter: Arc<dyn synctv_core::service::RequestRateLimiterService>,
    pub rate_limit_config: synctv_core::service::RateLimitConfig,
    pub content_filter: synctv_core::service::ContentFilter,
    pub realtime_connection_service: Arc<dyn RealtimeConnectionService>,
    pub realtime_event_service: Arc<dyn RealtimeEventService>,
    pub providers_manager: Arc<synctv_core::service::ProvidersManager>,
    pub provider_instance_manager: Arc<synctv_core::service::RemoteProviderManager>,
    pub user_provider_credential_repository: Arc<UserProviderCredentialRepository>,
    pub providers: synctv_core::provider::ProviderSet,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub passkey_service: Option<Arc<synctv_core::service::PasskeyService>>,
    pub settings_service: Arc<synctv_core::service::SettingsService>,
    pub settings_registry: Arc<synctv_core::service::SettingsRegistry>,
    pub email_service: Option<Arc<synctv_core::service::EmailService>>,
    pub email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    pub ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
    pub publish_key_service: Arc<dyn synctv_core::service::StreamingPublishKeyService>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub chat_service: Arc<synctv_core::service::ChatService>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub user_cache: Arc<UserCache>,
    pub live_streaming_infrastructure:
        Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub stun_server: Option<Arc<synctv_core::service::StunServer>>,
    pub webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    pub node_registry: Option<Arc<dyn synctv_cluster::discovery::ClusterNodeDirectory>>,
    pub health_monitor: Option<Arc<dyn synctv_cluster::discovery::ClusterHealthRuntime>>,
    pub(crate) cluster_activation: Option<Arc<dyn ClusterNodeActivator>>,
    pub redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    /// Credential encryption for protecting sensitive data (optional)
    pub credential_encryption: Option<synctv_core::credential_encryption::CredentialEncryption>,
}

/// `SyncTV` server - manages all server components
pub struct SyncTvServer {
    config: Config,
    services: Services,
    livestream_state: Option<LivestreamState>,
    pool: PgPool,
    lifecycle_controller: Arc<ManagementLifecycleController>,
    api_handle: Option<JoinHandle<anyhow::Result<()>>>,
    metrics_handle: Option<JoinHandle<anyhow::Result<()>>>,
    management_handle: Option<JoinHandle<anyhow::Result<()>>>,
    playback_lifecycle_event_source_handle: Option<JoinHandle<()>>,
}

const STARTUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const METRICS_CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const METRICS_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
const FORCE_SHUTDOWN_COORDINATOR_BUDGET: Duration = Duration::from_secs(1);

#[derive(Clone)]
struct SharedProviderPlaybackRuntime {
    provider_stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver>,
    signing_key: Arc<synctv_core::proxy_signature::ProxySigningKey>,
}

impl SharedProviderPlaybackRuntime {
    fn new(
        config: &Config,
        redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            provider_stores:
                synctv_core::provider::store::build_provider_store_resolver_from_profile(
                    &synctv_core::SharedStateProfile::best_effort(
                        redis_runtime,
                        config.redis.key_prefix.clone(),
                    ),
                ),
            signing_key: Arc::new(
                synctv_core::proxy_signature::ProxySigningKey::try_derive_from(
                    config.jwt.secret.as_bytes(),
                )
                .map_err(|error| anyhow::anyhow!("Failed to derive proxy signing key: {error}"))?,
            ),
        })
    }
}

async fn build_proxy_slice_cache(
    config: &Config,
    proxy_http_client: Client,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
) -> anyhow::Result<Arc<synctv_proxy::slice_cache::SliceCache>> {
    let slice_cache_config =
        synctv_api::config_adapters::proxy_slice_cache_config_from_app_config(config);
    let cache = synctv_proxy::slice_cache::SliceCache::try_new_with_client_and_ssrf_guard(
        slice_cache_config,
        proxy_http_client,
        ssrf_guard,
    )
    .await
    .context("failed to initialize proxy slice cache backend")?;
    Ok(Arc::new(cache))
}

struct ManagementProxySliceCacheRuntime {
    cache: Arc<synctv_proxy::slice_cache::SliceCache>,
}

impl ManagementProxySliceCacheRuntime {
    fn new(cache: Arc<synctv_proxy::slice_cache::SliceCache>) -> Self {
        Self { cache }
    }
}

#[async_trait]
impl synctv_management::service::ManagementSliceCacheRuntime for ManagementProxySliceCacheRuntime {
    fn stats(&self) -> synctv_management::service::ManagementSliceCacheStats {
        self.cache.stats()
    }

    async fn purge_all(&self) -> synctv_management::service::ManagementSliceCachePurgeResult {
        self.cache.purge_all().await
    }

    async fn evict_expired_entries(&self) -> u64 {
        self.cache.evict_expired_entries().await
    }
}

struct ManagementApiHandles {
    client: Arc<ClientApiImpl>,
    admin: Arc<AdminApiImpl>,
    provider_common: Arc<synctv_api::impls::ProviderCommonApiImpl>,
    alist: Arc<synctv_api::impls::AlistApiImpl>,
    bilibili: Arc<synctv_api::impls::BilibiliApiImpl>,
    emby: Arc<synctv_api::impls::EmbyApiImpl>,
}

fn management_apis_from_http_state(
    state: &synctv_api::http::AppState,
) -> anyhow::Result<ManagementApiHandles> {
    let shared_runtime = &state.shared_api_runtime;
    let admin_api = shared_runtime
        .admin_api
        .clone()
        .ok_or_else(|| anyhow::anyhow!("management server requires shared admin API wiring"))?;
    Ok(ManagementApiHandles {
        client: shared_runtime.client_api.clone(),
        admin: admin_api,
        provider_common: shared_runtime.provider_common_api.clone(),
        alist: shared_runtime.alist_api.clone(),
        bilibili: shared_runtime.bilibili_api.clone(),
        emby: shared_runtime.emby_api.clone(),
    })
}

async fn serve_metrics_connection<S>(stream: S, app: axum::Router) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |request| {
        let app = app.clone();
        async move {
            let app = app.into_service();
            let response = match app.oneshot(request).await {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(
                        error = ?error,
                        "metrics router failed to handle request"
                    );
                    axum::http::Response::builder()
                        .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                        .body(axum::body::Body::from("metrics router error"))
                        .unwrap_or_else(|_| axum::http::Response::new(axum::body::Body::empty()))
                }
            };
            Ok::<_, std::convert::Infallible>(response)
        }
    });

    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(METRICS_HEADER_READ_TIMEOUT)
        .keep_alive(false);

    builder
        .serve_connection(TokioIo::new(stream), service)
        .await
        .map_err(|error| anyhow::anyhow!("metrics connection error: {error}"))?;

    Ok(())
}

async fn load_metrics_tls_server_config(
    tls: &synctv_core::config::MetricsTlsConfig,
) -> anyhow::Result<rustls::ServerConfig> {
    let cert_pem = tokio::fs::read(&tls.cert_path).await.with_context(|| {
        format!(
            "failed to read metrics TLS certificate {}",
            absolute_display_path(std::path::Path::new(&tls.cert_path))
        )
    })?;
    let key_pem = tokio::fs::read(&tls.key_path).await.with_context(|| {
        format!(
            "failed to read metrics TLS private key {}",
            absolute_display_path(std::path::Path::new(&tls.key_path))
        )
    })?;

    let cert_chain = rustls_pemfile::certs(&mut BufReader::new(cert_pem.as_slice()))
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| {
            format!(
                "failed to parse metrics TLS certificate {}",
                absolute_display_path(std::path::Path::new(&tls.cert_path))
            )
        })?;
    if cert_chain.is_empty() {
        anyhow::bail!(
            "metrics TLS certificate {} did not contain any PEM certificates",
            absolute_display_path(std::path::Path::new(&tls.cert_path))
        );
    }

    let private_key = rustls_pemfile::private_key(&mut BufReader::new(key_pem.as_slice()))
        .with_context(|| {
            format!(
                "failed to parse metrics TLS private key {}",
                absolute_display_path(std::path::Path::new(&tls.key_path))
            )
        })?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "metrics TLS private key {} did not contain a supported PEM private key",
                absolute_display_path(std::path::Path::new(&tls.key_path))
            )
        })?;

    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .context("failed to build metrics TLS server config")
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

async fn shutdown_metrics_connection_tasks(connections: &mut JoinSet<()>, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        if connections.is_empty() {
            return;
        }

        let remaining = remaining_budget(deadline);
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(remaining, connections.join_next()).await {
            Ok(Some(Ok(()))) => {}
            Ok(Some(Err(error))) => {
                if error.is_panic() {
                    std::panic::resume_unwind(error.into_panic());
                }
                warn!(error = %error, "metrics connection task ended unexpectedly");
            }
            Ok(None) => return,
            Err(_) => break,
        }
    }

    if connections.is_empty() {
        return;
    }

    warn!(
        timeout_secs = timeout.as_secs_f64(),
        remaining_connections = connections.len(),
        "metrics server still has active connections after drain timeout; aborting remaining tasks"
    );
    connections.abort_all();

    while let Some(join_result) = connections.join_next().await {
        if let Err(error) = join_result {
            if error.is_panic() {
                std::panic::resume_unwind(error.into_panic());
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

fn signal_server_shutdown(sender: &watch::Sender<bool>, reason: &'static str) {
    if sender.send(true).is_err() {
        warn!(reason, "Server shutdown signal had no active receivers");
    }
}

async fn await_optional_runtime_server(
    handle: &mut Option<JoinHandle<anyhow::Result<()>>>,
) -> Result<anyhow::Result<()>, tokio::task::JoinError> {
    match handle.as_mut() {
        Some(handle) => handle.await,
        None => std::future::pending().await,
    }
}

async fn await_runtime_server_shutdown(
    name: &'static str,
    handle: JoinHandle<anyhow::Result<()>>,
    timeout: Duration,
) {
    if timeout == Duration::ZERO {
        force_abort_runtime_server(name, handle).await;
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
    let mut handle = handle;
    handle.abort();
    if let Ok(join_result) =
        tokio::time::timeout(FORCE_SHUTDOWN_COORDINATOR_BUDGET, &mut handle).await
    {
        match join_result {
            Ok(Ok(())) => info!("{name} aborted cleanly"),
            Ok(Err(err)) => warn!("{name} returned error after forced abort: {err}"),
            Err(err) if err.is_cancelled() => info!("{name} aborted"),
            Err(err) => warn!("{name} failed after forced abort: {err}"),
        }
    } else {
        warn!("{name} did not join after forced abort");
    }
}

async fn force_abort_background_task(name: &'static str, handle: JoinHandle<()>) {
    warn!("{name} exceeded the remaining shutdown budget, aborting task");
    let mut handle = handle;
    handle.abort();
    if let Ok(join_result) =
        tokio::time::timeout(FORCE_SHUTDOWN_COORDINATOR_BUDGET, &mut handle).await
    {
        match join_result {
            Ok(()) => info!("{name} aborted cleanly"),
            Err(err) if err.is_cancelled() => info!("{name} aborted"),
            Err(err) => warn!("{name} failed after forced abort: {err}"),
        }
    } else {
        warn!("{name} did not join after forced abort");
    }
}

fn remaining_budget(deadline: tokio::time::Instant) -> Duration {
    deadline.saturating_duration_since(tokio::time::Instant::now())
}

fn coordinator_shutdown_deadline(
    shutdown_start: tokio::time::Instant,
    total_drain_budget: Duration,
    force_shutdown: bool,
) -> tokio::time::Instant {
    if force_shutdown {
        tokio::time::Instant::now() + FORCE_SHUTDOWN_COORDINATOR_BUDGET
    } else {
        shutdown_start + total_drain_budget
    }
}

fn livestream_shutdown_timeout_secs(timeout: Duration) -> u64 {
    if timeout.is_zero() {
        0
    } else {
        timeout.as_secs() + u64::from(timeout.subsec_nanos() > 0)
    }
}

async fn shutdown_livestream_state<T>(livestream_state: &mut Option<T>, budget: Duration)
where
    T: LivestreamShutdown + Send,
{
    if let Some(mut state) = livestream_state.take() {
        info!("Stopping livestream infrastructure...");
        let started = tokio::time::Instant::now();
        let cleanup_result =
            tokio::time::timeout(budget, state.cleanup_local_publishers_for_server(budget)).await;
        if cleanup_result.is_err() {
            warn!("Local publisher cleanup consumed the remaining livestream shutdown budget");
        }

        let remaining_budget = budget.saturating_sub(started.elapsed());
        let timeout_secs = livestream_shutdown_timeout_secs(remaining_budget);
        let graceful = if remaining_budget.is_zero() {
            warn!(
                "No livestream shutdown budget remains after publisher cleanup; force-aborting livestream infrastructure"
            );
            state.force_shutdown_for_server();
            false
        } else if let Ok(graceful) =
            tokio::time::timeout(remaining_budget, state.shutdown_for_server(timeout_secs)).await
        {
            graceful
        } else {
            warn!(
                "Livestream infrastructure exceeded the remaining shutdown budget before graceful shutdown could complete"
            );
            state.force_shutdown_for_server();
            false
        };
        if !graceful {
            warn!("Livestream infrastructure required force-abort or timed out during shutdown");
        }
        info!("Livestream infrastructure shut down");
    }
}

async fn shutdown_runtime_phase(
    api_handle: Option<JoinHandle<anyhow::Result<()>>>,
    metrics_handle: Option<JoinHandle<anyhow::Result<()>>>,
    management_handle: Option<JoinHandle<anyhow::Result<()>>>,
    cleanup_handle: JoinHandle<()>,
    playback_lifecycle_event_source_handle: Option<JoinHandle<()>>,
    total_budget: Duration,
    defer_management_wait: bool,
) -> Option<JoinHandle<anyhow::Result<()>>> {
    let deadline = tokio::time::Instant::now() + total_budget;
    let mut management_handle = management_handle;

    info!(
        "Waiting up to {}s for API server, management server, and cleanup task to stop...",
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

    if let Some(metrics_handle) = metrics_handle {
        let budget = remaining_budget(deadline);
        if budget.is_zero() {
            force_abort_runtime_server("Metrics server", metrics_handle).await;
        } else {
            await_runtime_server_shutdown("Metrics server", metrics_handle, budget).await;
        }
    }

    if defer_management_wait {
        info!("Deferring management server shutdown wait until stop-stream consumers disconnect");
    } else if let Some(management_handle) = management_handle.take() {
        let budget = remaining_budget(deadline);
        if budget.is_zero() {
            force_abort_runtime_server("Management server", management_handle).await;
        } else {
            await_runtime_server_shutdown("Management server", management_handle, budget).await;
        }
    }

    await_task_shutdown(
        "connection cleanup task",
        cleanup_handle,
        remaining_budget(deadline),
    )
    .await;

    if let Some(playback_lifecycle_event_source_handle) = playback_lifecycle_event_source_handle {
        await_task_shutdown(
            "observed playback lifecycle event source",
            playback_lifecycle_event_source_handle,
            remaining_budget(deadline),
        )
        .await;
    }

    if defer_management_wait {
        management_handle
    } else {
        None
    }
}

async fn cleanup_partial_startup(
    shutdown_tx: &watch::Sender<bool>,
    cleanup_cancel: &tokio_util::sync::CancellationToken,
    cleanup_handle: Option<JoinHandle<()>>,
    api_handle: Option<JoinHandle<anyhow::Result<()>>>,
    metrics_handle: Option<JoinHandle<anyhow::Result<()>>>,
    management_handle: Option<JoinHandle<anyhow::Result<()>>>,
    deadline: tokio::time::Instant,
) {
    signal_server_shutdown(shutdown_tx, "partial startup cleanup");
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

    if let Some(handle) = metrics_handle {
        let timeout = remaining_budget(deadline).min(STARTUP_CLEANUP_TIMEOUT);
        if timeout.is_zero() {
            force_abort_runtime_server("Metrics server", handle).await;
        } else {
            await_runtime_server_shutdown("Metrics server", handle, timeout).await;
        }
    }

    if let Some(handle) = management_handle {
        let timeout = remaining_budget(deadline).min(STARTUP_CLEANUP_TIMEOUT);
        if timeout.is_zero() {
            force_abort_runtime_server("Management gRPC server", handle).await;
        } else {
            await_runtime_server_shutdown("Management gRPC server", handle, timeout).await;
        }
    }
}

async fn shutdown_after_startup_failure(
    context: StartupFailureShutdownContext,
    component_cleanup: impl std::future::Future<Output = ()> + Send,
    coordinator: ShutdownCoordinator,
) {
    let StartupFailureShutdownContext {
        shutdown_tx,
        cleanup_cancel,
        cleanup_handle,
        api_handle,
        metrics_handle,
        management_handle,
        deadline,
    } = context;
    cleanup_partial_startup(
        &shutdown_tx,
        &cleanup_cancel,
        cleanup_handle,
        api_handle,
        metrics_handle,
        management_handle,
        deadline,
    )
    .await;
    component_cleanup.await;
    coordinator.shutdown_with_deadline(deadline).await;
}

struct StartupFailureShutdownContext {
    shutdown_tx: watch::Sender<bool>,
    cleanup_cancel: tokio_util::sync::CancellationToken,
    cleanup_handle: Option<JoinHandle<()>>,
    api_handle: Option<JoinHandle<anyhow::Result<()>>>,
    metrics_handle: Option<JoinHandle<anyhow::Result<()>>>,
    management_handle: Option<JoinHandle<anyhow::Result<()>>>,
    deadline: tokio::time::Instant,
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
        metrics_handle,
        management_handle,
        playback_lifecycle_event_source_handle,
        deadline,
        coordinator,
    } = context;

    signal_server_shutdown(&shutdown_tx, "cluster activation failure cleanup");
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

    if let Some(handle) = metrics_handle {
        let timeout = remaining_budget(deadline).min(STARTUP_CLEANUP_TIMEOUT);
        if timeout.is_zero() {
            force_abort_runtime_server("Metrics server", handle).await;
        } else {
            await_runtime_server_shutdown("Metrics server", handle, timeout).await;
        }
    }

    if let Some(handle) = management_handle {
        let timeout = remaining_budget(deadline).min(STARTUP_CLEANUP_TIMEOUT);
        if timeout.is_zero() {
            force_abort_runtime_server("Management gRPC server", handle).await;
        } else {
            await_runtime_server_shutdown("Management gRPC server", handle, timeout).await;
        }
    }

    if let Some(handle) = playback_lifecycle_event_source_handle {
        let timeout = remaining_budget(deadline).min(STARTUP_CLEANUP_TIMEOUT);
        if timeout.is_zero() {
            force_abort_background_task("observed playback lifecycle event source", handle).await;
        } else {
            await_task_shutdown("observed playback lifecycle event source", handle, timeout).await;
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
    metrics_handle: Option<JoinHandle<anyhow::Result<()>>>,
    management_handle: Option<JoinHandle<anyhow::Result<()>>>,
    playback_lifecycle_event_source_handle: Option<JoinHandle<()>>,
    deadline: tokio::time::Instant,
    coordinator: ShutdownCoordinator,
}

fn spawn_admin_event_listener(
    event_service: Arc<dyn RealtimeEventService>,
    infra: Arc<synctv_livestream::api::LiveStreamingInfrastructure>,
    cancel: tokio_util::sync::CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut admin_rx = event_service.subscribe_admin_events();
        let mut lifecycle_rx = event_service.subscribe_lifecycle_events();
        let mut recent_event_ids = HashSet::new();
        let mut recent_event_order = VecDeque::new();
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    info!("Admin event listener cancelled");
                    break;
                }
                recv = admin_rx.recv() => {
                    match recv {
                        Ok(event) => {
                            handle_admin_lifecycle_event(
                                &infra,
                                event,
                                &mut recent_event_ids,
                                &mut recent_event_order,
                            )
                            .await;
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
                recv = lifecycle_rx.recv() => {
                    match recv {
                        Ok(event) => {
                            handle_admin_lifecycle_event(
                                &infra,
                                event,
                                &mut recent_event_ids,
                                &mut recent_event_order,
                            )
                            .await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Lifecycle event listener lagged by {} events", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("Lifecycle event channel closed, stopping listener");
                            break;
                        }
                    }
                }
            }
        }
    })
}

async fn handle_admin_lifecycle_event(
    infra: &Arc<synctv_livestream::api::LiveStreamingInfrastructure>,
    event: RealtimeEvent,
    recent_event_ids: &mut HashSet<String>,
    recent_event_order: &mut VecDeque<String>,
) {
    const RECENT_ADMIN_LIFECYCLE_EVENTS: usize = 1024;

    let event_id = event.event_id().to_string();
    if !recent_event_ids.insert(event_id.clone()) {
        return;
    }
    recent_event_order.push_back(event_id);
    while recent_event_order.len() > RECENT_ADMIN_LIFECYCLE_EVENTS {
        if let Some(expired) = recent_event_order.pop_front() {
            recent_event_ids.remove(&expired);
        }
    }

    match &event {
        RealtimeEvent::KickPublisher {
            room_id,
            media_id,
            reason,
            ..
        } => {
            info!(
                room_id = %room_id,
                media_id = %media_id,
                reason = %reason,
                "Received replica-wide stream kick"
            );
            let room_id_string = room_id.to_string();
            let media_id_string = media_id.to_string();
            if let Err(e) = infra.kick_stream(&room_id_string, &media_id_string).await {
                warn!(
                    room_id = %room_id,
                    media_id = %media_id,
                    error = %e,
                    "Failed to kick publisher from cluster admin event"
                );
            }
        }
        RealtimeEvent::KickUser {
            user_id, reason, ..
        } => {
            info!(
                user_id = %user_id,
                reason = %reason,
                "Received replica-wide user kick"
            );
            let user_id_string = user_id.to_string();
            infra.kick_user_publishers(&user_id_string).await;
        }
        RealtimeEvent::KickUserFromRoom {
            room_id,
            user_id,
            reason,
            ..
        } => {
            info!(
                room_id = %room_id,
                user_id = %user_id,
                reason = %reason,
                "Received room-scoped user kick"
            );
            let room_id_string = room_id.to_string();
            let user_id_string = user_id.to_string();
            infra
                .kick_user_room_publishers(&room_id_string, &user_id_string)
                .await;
        }
        RealtimeEvent::RoomDeleted { room_id, .. } => {
            info!(
                room_id = %room_id,
                "Received replica-wide room deletion"
            );
            infra.kick_room_publishers(&room_id.to_string()).await;
        }
        RealtimeEvent::RoomBanned { room_id, .. } => {
            info!(
                room_id = %room_id,
                "Received replica-wide room ban"
            );
            infra.kick_room_publishers(&room_id.to_string()).await;
        }
        RealtimeEvent::RoomOwnerInactive { room_id, .. } => {
            info!(
                room_id = %room_id,
                "Received replica-wide inactive-owner room lifecycle event"
            );
            infra.kick_room_publishers(&room_id.to_string()).await;
        }
        _ => {}
    }
}

impl SyncTvServer {
    fn builtin_stun_url(&self) -> Option<String> {
        self.services.stun_server.as_ref().map(|stun| {
            let addr = stun.external_addr();
            format!("stun:{}:{}", addr.ip(), addr.port())
        })
    }

    fn current_webrtc_status(&self) -> synctv_core::service::WebRtcRuntimeStatus {
        self.services.webrtc_status.clone().with_task_running(
            self.services
                .stun_server
                .as_ref()
                .is_some_and(|stun| stun.is_running()),
        )
    }

    /// Create a new server instance
    pub const fn new(
        config: Config,
        services: Services,
        livestream_state: Option<LivestreamState>,
        pool: PgPool,
        lifecycle_controller: Arc<ManagementLifecycleController>,
    ) -> Self {
        Self {
            config,
            services,
            livestream_state,
            pool,
            lifecycle_controller,
            api_handle: None,
            metrics_handle: None,
            management_handle: None,
            playback_lifecycle_event_source_handle: None,
        }
    }

    /// Start all servers and wait for shutdown signal, using a `ShutdownCoordinator`
    /// for centralized shutdown orchestration.
    pub async fn start_with_coordinator(
        self,
        coordinator: ShutdownCoordinator,
    ) -> anyhow::Result<()> {
        Box::pin(self.start_with_coordinator_and_shutdown_signal(coordinator, shutdown_signal()))
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
        let mut lifecycle_shutdown_rx = self.lifecycle_controller.shutdown_receiver();
        tokio::pin!(shutdown_signal);

        // Log infrastructure state
        if self.livestream_state.is_some() {
            info!("Livestream infrastructure: enabled");
        }
        info!("WebRTC runtime: {}", self.current_webrtc_status().summary());

        // Start background connection cleanup (every 60 seconds)
        let cleanup_cancel = tokio_util::sync::CancellationToken::new();
        let cleanup_handle = self
            .services
            .realtime_connection_service
            .spawn_cleanup_task(Duration::from_mins(1), cleanup_cancel.clone());
        let shared_provider_runtime =
            SharedProviderPlaybackRuntime::new(&self.config, self.services.redis_runtime.clone())?;
        let (http_router, shared_http_app_state) = match self
            .build_shared_http_runtime(&shared_provider_runtime)
            .await
        {
            Ok(runtime) => runtime,
            Err(err) => {
                let startup_cleanup_budget =
                    Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds)
                        .min(STARTUP_CLEANUP_TIMEOUT);
                let startup_cleanup_deadline = tokio::time::Instant::now() + startup_cleanup_budget;
                shutdown_after_startup_failure(
                    StartupFailureShutdownContext {
                        shutdown_tx: shutdown_tx.clone(),
                        cleanup_cancel: cleanup_cancel.clone(),
                        cleanup_handle: Some(cleanup_handle),
                        api_handle: None,
                        metrics_handle: None,
                        management_handle: None,
                        deadline: startup_cleanup_deadline,
                    },
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

        // Start unified API server (single listener for REST + gRPC)
        let api_handle = match self
            .start_api_server(
                shutdown_rx.clone(),
                http_router,
                shared_http_app_state.clone(),
            )
            .await
        {
            Ok(handle) => handle,
            Err(err) => {
                let startup_cleanup_budget =
                    Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds)
                        .min(STARTUP_CLEANUP_TIMEOUT);
                let startup_cleanup_deadline = tokio::time::Instant::now() + startup_cleanup_budget;
                shutdown_after_startup_failure(
                    StartupFailureShutdownContext {
                        shutdown_tx: shutdown_tx.clone(),
                        cleanup_cancel: cleanup_cancel.clone(),
                        cleanup_handle: Some(cleanup_handle),
                        api_handle: None,
                        metrics_handle: None,
                        management_handle: None,
                        deadline: startup_cleanup_deadline,
                    },
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

        if self.config.metrics.enabled {
            let metrics_handle = match self
                .start_metrics_server(shutdown_rx.clone(), shared_http_app_state.clone())
                .await
            {
                Ok(handle) => handle,
                Err(err) => {
                    let api_handle = self.api_handle.take();
                    let startup_cleanup_budget =
                        Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds)
                            .min(STARTUP_CLEANUP_TIMEOUT);
                    let startup_cleanup_deadline =
                        tokio::time::Instant::now() + startup_cleanup_budget;
                    shutdown_after_startup_failure(
                        StartupFailureShutdownContext {
                            shutdown_tx: shutdown_tx.clone(),
                            cleanup_cancel: cleanup_cancel.clone(),
                            cleanup_handle: Some(cleanup_handle),
                            api_handle,
                            metrics_handle: None,
                            management_handle: None,
                            deadline: startup_cleanup_deadline,
                        },
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
            self.metrics_handle = Some(metrics_handle);
        }

        if self.config.management.enabled {
            let management_handle = match self
                .start_management_server(shutdown_rx.clone(), shared_http_app_state.clone())
                .await
            {
                Ok(handle) => handle,
                Err(err) => {
                    let api_handle = self.api_handle.take();
                    let metrics_handle = self.metrics_handle.take();
                    let startup_cleanup_budget =
                        Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds)
                            .min(STARTUP_CLEANUP_TIMEOUT);
                    let startup_cleanup_deadline =
                        tokio::time::Instant::now() + startup_cleanup_budget;
                    shutdown_after_startup_failure(
                        StartupFailureShutdownContext {
                            shutdown_tx: shutdown_tx.clone(),
                            cleanup_cancel: cleanup_cancel.clone(),
                            cleanup_handle: Some(cleanup_handle),
                            api_handle,
                            metrics_handle,
                            management_handle: None,
                            deadline: startup_cleanup_deadline,
                        },
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
            self.management_handle = Some(management_handle);
        }

        let playback_service = shared_http_app_state.shared_api_runtime.client_api.clone();
        self.playback_lifecycle_event_source_handle = Some(
            synctv_api::impls::messaging::spawn_observed_playback_lifecycle_event_source(
                playback_service.clone(),
                vec![Arc::new(
                    synctv_api::impls::messaging::ProviderPlaybackProgressSubscriber::new(
                        playback_service,
                    ),
                )],
                shutdown_rx.clone(),
            ),
        );

        if let Some(cluster_activation) = &self.services.cluster_activation {
            if let Err(err) = cluster_activation.activate().await {
                let api_handle = self.api_handle.take();
                let metrics_handle = self.metrics_handle.take();
                let management_handle = self.management_handle.take();
                let startup_cleanup_budget =
                    Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds)
                        .min(STARTUP_CLEANUP_TIMEOUT);
                let startup_cleanup_deadline = tokio::time::Instant::now() + startup_cleanup_budget;
                let playback_lifecycle_event_source_handle =
                    self.playback_lifecycle_event_source_handle.take();
                shutdown_after_cluster_activation_failure(
                    &mut self,
                    ClusterActivationFailureShutdown {
                        shutdown_tx: shutdown_tx.clone(),
                        cleanup_cancel: cleanup_cancel.clone(),
                        cleanup_handle: Some(cleanup_handle),
                        api_handle,
                        metrics_handle,
                        management_handle,
                        playback_lifecycle_event_source_handle,
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

        // Spawn streaming event listener for replica-wide kicks
        let admin_event_cancel = tokio_util::sync::CancellationToken::new();
        let admin_event_handle: Option<JoinHandle<()>> =
            if let Some(infra) = &self.services.live_streaming_infrastructure {
                let handle = spawn_admin_event_listener(
                    Arc::clone(&self.services.realtime_event_service),
                    Arc::clone(infra),
                    admin_event_cancel.clone(),
                );
                info!("Admin event listener spawned for replica-wide stream kicks");
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
        let mut metrics_handle = self.metrics_handle.take();
        let mut management_handle = self.management_handle.take();

        let (
            shutdown_mode,
            unexpected_exit,
            api_handle,
            metrics_handle,
            management_handle,
            defer_management_shutdown_wait,
        ) = tokio::select! {
            result = await_optional_runtime_server(&mut api_handle) => {
                let _ = api_handle.take();
                (
                    ShutdownMode::Graceful,
                    Some(map_runtime_server_exit("API server", result)),
                    None,
                    metrics_handle.take(),
                    management_handle.take(),
                    false,
                )
            },
            result = await_optional_runtime_server(&mut metrics_handle), if metrics_handle.is_some() => {
                let _ = metrics_handle.take();
                (
                    ShutdownMode::Graceful,
                    Some(map_runtime_server_exit("Metrics server", result)),
                    api_handle.take(),
                    None,
                    management_handle.take(),
                    false,
                )
            },
            result = await_optional_runtime_server(&mut management_handle), if management_handle.is_some() => {
                let _ = management_handle.take();
                (
                    ShutdownMode::Graceful,
                    Some(map_runtime_server_exit("Management server", result)),
                    api_handle.take(),
                    metrics_handle.take(),
                    None,
                    false,
                )
            },
            lifecycle_mode = async {
                if lifecycle_shutdown_rx.changed().await.is_err() {
                    ShutdownMode::Graceful
                } else {
                    (*lifecycle_shutdown_rx.borrow()).unwrap_or(ShutdownMode::Graceful)
                }
            }, if self.config.management.enabled => {
                (
                    lifecycle_mode,
                    None,
                    api_handle.take(),
                    metrics_handle.take(),
                    management_handle.take(),
                    true,
                )
            },
            () = &mut shutdown_signal => {
                info!("External shutdown signal received, starting graceful shutdown...");
                self.lifecycle_controller
                    .request_shutdown(ShutdownMode::Graceful);
                (
                    ShutdownMode::Graceful,
                    None,
                    api_handle.take(),
                    metrics_handle.take(),
                    management_handle.take(),
                    false,
                )
            }
        };

        // Signal API server to shut down
        signal_server_shutdown(&shutdown_tx, "runtime shutdown");
        cleanup_cancel.cancel();
        self.lifecycle_controller.publish_runtime_draining();

        // Track total shutdown start time to compute remaining budget for each
        // phase. The total drain budget is `shutdown_drain_timeout_seconds`.
        let shutdown_start = tokio::time::Instant::now();
        let total_drain_budget =
            Duration::from_secs(self.config.server.shutdown_drain_timeout_seconds);
        let force_shutdown = matches!(shutdown_mode, ShutdownMode::Force);

        // Phase 1: Wait for unified API server to finish (use 60% of budget).
        let http_drain_budget = total_drain_budget * 60 / 100;
        info!(
            "Waiting up to {}s for API server and management server to shut down...",
            http_drain_budget.as_secs()
        );
        let deferred_management_handle = shutdown_runtime_phase(
            api_handle,
            metrics_handle,
            management_handle,
            cleanup_handle,
            self.playback_lifecycle_event_source_handle.take(),
            http_drain_budget,
            defer_management_shutdown_wait,
        )
        .await;
        if deferred_management_handle.is_some() {
            info!("API and metrics servers shut down; management server wait deferred");
        } else {
            info!("API, metrics, and management servers shut down");
        }

        // Phase 2: Drain active connections BEFORE shutting down the realtime manager.
        // Events generated during drain (UserLeft, etc.) need the pub/sub
        // system to be alive so they can be broadcast to other replicas.
        // Use the remaining time from the total budget instead of a separate
        // full timeout, keeping total shutdown within K8s grace period.
        {
            self.lifecycle_controller.publish_connection_draining();
            let elapsed = shutdown_start.elapsed();
            let remaining_budget = total_drain_budget.saturating_sub(elapsed);
            let drain_poll_interval = Duration::from_millis(500);
            let active = self.services.realtime_connection_service.connection_count();
            if force_shutdown {
                info!("Force shutdown requested, skipping connection drain wait");
            } else if active > 0 && remaining_budget > Duration::ZERO {
                info!(
                    "Waiting up to {}s for {} active connection(s) to drain ({}s elapsed)...",
                    remaining_budget.as_secs(),
                    active,
                    elapsed.as_secs()
                );
                let deadline = tokio::time::Instant::now() + remaining_budget;
                loop {
                    let remaining = self.services.realtime_connection_service.connection_count();
                    if remaining == 0 {
                        info!("All connections drained");
                        break;
                    }
                    if matches!(*lifecycle_shutdown_rx.borrow(), Some(ShutdownMode::Force)) {
                        warn!(
                            "Force shutdown requested during connection drain, stopping wait with {} connection(s) still active",
                            remaining
                        );
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

        // Stop livestream publishers before shutting down the realtime manager.
        // Realtime shutdown can wait on Redis pub/sub tasks; shared livestream
        // registry cleanup must run while Redis is still available and before
        // that wait consumes the remaining process drain budget.
        self.lifecycle_controller.publish_components_shutting_down();
        shutdown_livestream_state(
            &mut self.livestream_state,
            if matches!(*lifecycle_shutdown_rx.borrow(), Some(ShutdownMode::Force)) {
                Duration::ZERO
            } else {
                total_drain_budget.saturating_sub(shutdown_start.elapsed())
            },
        )
        .await;

        info!("Shutting down realtime event service (post-drain, closing admin event channel)...");
        self.services.realtime_event_service.shutdown().await;
        info!("Realtime event service shut down (admin event channel closed)");

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

        // Shut down remaining infrastructure components. Livestream was already
        // stopped above; this remains a no-op for livestream because the state
        // is consumed by `shutdown_livestream_state`.
        self.shutdown_components(
            if matches!(*lifecycle_shutdown_rx.borrow(), Some(ShutdownMode::Force)) {
                Duration::ZERO
            } else {
                total_drain_budget.saturating_sub(shutdown_start.elapsed())
            },
        )
        .await;

        // Centralized shutdown: cancel tokens -> drain/abort tasks -> run hooks.
        // Force shutdown intentionally grants a short bounded cleanup window
        // so local hooks can close listeners and flush critical state after
        // skipping the normal connection/component drains.
        self.lifecycle_controller.publish_finalizing();
        let force_shutdown = matches!(*lifecycle_shutdown_rx.borrow(), Some(ShutdownMode::Force));
        let coordinator_deadline =
            coordinator_shutdown_deadline(shutdown_start, total_drain_budget, force_shutdown);
        if force_shutdown {
            coordinator
                .shutdown_force_with_deadline(coordinator_deadline)
                .await;
        } else {
            coordinator
                .shutdown_with_deadline(coordinator_deadline)
                .await;
        }

        // Close the database connection pool (after audit flush and settings task)
        info!("Closing database connection pool...");
        self.pool.close().await;
        info!("Database pool closed");

        info!("SyncTV server shut down complete");
        if let Some(result) = unexpected_exit {
            if let Err(error) = &result {
                self.lifecycle_controller
                    .publish_failure(format!("shutdown failed: {error}"));
            }
            return result;
        }
        self.lifecycle_controller.publish_completed();
        if let Some(handle) = deferred_management_handle {
            let timeout = if matches!(*lifecycle_shutdown_rx.borrow(), Some(ShutdownMode::Force)) {
                Duration::ZERO
            } else {
                total_drain_budget.saturating_sub(shutdown_start.elapsed())
            };
            await_runtime_server_shutdown("Management server", handle, timeout).await;
        }
        Ok(())
    }

    /// Shut down infrastructure components (STUN, livestream, health monitor, node registry, realtime connections).
    ///
    /// This is separate from the `ShutdownCoordinator` because these components
    /// have custom shutdown protocols (not just cancellation tokens or join handles).
    async fn shutdown_components(&mut self, budget_remaining: Duration) {
        let deadline = tokio::time::Instant::now() + budget_remaining;

        // Shut down realtime connection service (stops TTL refresh/background maintenance).
        info!("Shutting down realtime connection service...");
        self.services.realtime_connection_service.shutdown().await;
        info!("Realtime connection service shut down");

        // Minor fix: Removed redundant `registry.unregister()` call.
        // `RealtimeManager::shutdown()` already calls `registry.unregister()` during
        // heartbeat state cleanup. Calling it again here was a no-op (the node is
        // already deregistered) but added unnecessary Redis round-trip and log noise.

        // Shut down STUN server
        if let Some(ref stun) = self.services.stun_server {
            info!("Shutting down STUN server...");
            stun.shutdown();
            info!("STUN server shut down");
        }

        // Stop livestream
        let livestream_budget = remaining_budget(deadline);
        shutdown_livestream_state(&mut self.livestream_state, livestream_budget).await;

        // Shut down health monitor
        if let Some(ref health_monitor) = self.services.health_monitor {
            info!("Shutting down health monitor...");
            health_monitor.shutdown().await;
            info!("Health monitor shut down");
        }
    }

    async fn shutdown_startup_failure_components(&mut self, deadline: tokio::time::Instant) {
        info!("Shutting down realtime event service during startup rollback...");
        let timeout = remaining_budget(deadline);
        if timeout.is_zero() {
            warn!(
                "Skipping realtime event service shutdown during startup rollback: no budget left"
            );
        } else if tokio::time::timeout(timeout, self.services.realtime_event_service.shutdown())
            .await
            .is_ok()
        {
            info!("Realtime event service shut down during startup rollback");
        } else {
            warn!("Realtime event service shutdown exceeded startup rollback budget");
        }

        self.shutdown_components(remaining_budget(deadline)).await;
    }

    async fn build_grpc_router(
        &self,
        config: &Config,
        shutdown_rx: watch::Receiver<bool>,
        shared_http_app_state: Arc<synctv_api::http::AppState>,
    ) -> anyhow::Result<axum::Router> {
        synctv_api::grpc::build_axum_router(synctv_api::grpc::GrpcServerConfig {
            config,
            jwt_service: self.services.jwt_service.clone(),
            user_service: self.services.user_service.clone(),
            user_cache: self.services.user_cache.clone(),
            room_service: self.services.room_service.clone(),
            event_service: self.services.realtime_event_service.clone(),
            realtime_fanout_service: self.services.realtime_fanout_service.clone(),
            rate_limiter: self.services.rate_limiter.clone(),
            rate_limit_config: self.services.rate_limit_config.clone(),
            content_filter: self.services.content_filter.clone(),
            connection_service: self.services.realtime_connection_service.clone(),
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
            ws_ticket_service: self.services.ws_ticket_service.clone(),
            live_streaming_infrastructure: self.services.live_streaming_infrastructure.clone(),
            publish_key_service: Some(self.services.publish_key_service.clone()),
            notification_service: self.services.notification_service.clone(),
            chat_service: Some(self.services.chat_service.clone()),
            oauth2_service: self.services.oauth2_service.clone(),
            passkey_service: self.services.passkey_service.clone(),
            audit_service: self.services.audit_service.clone(),
            node_registry: self.services.node_registry.clone(),
            redis_runtime: self.services.redis_runtime.clone(),
            shared_http_app_state: Some(shared_http_app_state),
            shutdown_rx: Some(shutdown_rx),
            builtin_stun_url: self.builtin_stun_url(),
            webrtc_status: self.current_webrtc_status(),
            credential_encryption: self.services.credential_encryption.clone(),
            grpc_listener: None,
        })
        .await
    }

    async fn build_shared_http_runtime(
        &self,
        shared_provider_runtime: &SharedProviderPlaybackRuntime,
    ) -> anyhow::Result<(axum::Router, Arc<synctv_api::http::AppState>)> {
        let ssrf_guard = self.config.security.ssrf_guard();
        let proxy_http_client = synctv_proxy::build_proxy_http_client(ssrf_guard.clone())?;
        let proxy_slice_cache =
            build_proxy_slice_cache(&self.config, proxy_http_client.clone(), ssrf_guard.clone())
                .await?;

        let (http_router, http_state) = synctv_api::http::create_router_with_state_from_config(
            synctv_api::http::RouterConfig {
                config: Arc::new(self.config.clone()),
                user_service: self.services.user_service.clone(),
                user_cache: self.services.user_cache.clone(),
                room_service: self.services.room_service.clone(),
                content_filter: self.services.content_filter.clone(),
                provider_instance_manager: self.services.provider_instance_manager.clone(),
                user_provider_credential_repository: self
                    .services
                    .user_provider_credential_repository
                    .clone(),
                providers: self.services.providers.clone(),
                event_service: self.services.realtime_event_service.clone(),
                connection_manager: self.services.realtime_connection_service.clone(),
                jwt_service: self.services.jwt_service.clone(),
                realtime_fanout_service: self.services.realtime_fanout_service.clone(),
                oauth2_service: self.services.oauth2_service.clone(),
                passkey_service: self.services.passkey_service.clone(),
                settings_service: Some(self.services.settings_service.clone()),
                settings_registry: Some(self.services.settings_registry.clone()),
                email_service: self.services.email_service.clone(),
                email_token_service: self.services.email_token_service.clone(),
                publish_key_service: Some(self.services.publish_key_service.clone()),
                notification_service: self.services.notification_service.clone(),
                chat_service: Some(self.services.chat_service.clone()),
                audit_service: self.services.audit_service.clone(),
                live_streaming_infrastructure: self.services.live_streaming_infrastructure.clone(),
                rate_limiter: self.services.rate_limiter.clone(),
                ws_ticket_service: self.services.ws_ticket_service.clone(),
                redis_runtime: self.services.redis_runtime.clone(),
                shared_provider_stores: Some(shared_provider_runtime.provider_stores.clone()),
                shared_proxy_signing_key: Some(shared_provider_runtime.signing_key.clone()),
                builtin_stun_url: self.builtin_stun_url(),
                webrtc_status: self.current_webrtc_status(),
                credential_encryption: self.services.credential_encryption.clone(),
                proxy_slice_cache,
                ssrf_guard,
                proxy_http_client,
                messaging_rate_limit_config: synctv_core::service::RateLimitConfig {
                    chat_per_second: self.config.messaging_rate_limits.chat_per_second,
                    window_seconds: self.config.messaging_rate_limits.window_seconds,
                },
                heartbeat_schedule: synctv_api::impls::HeartbeatSchedule::production(),
                providers_manager: Some(self.services.providers_manager.clone()),
            },
        )?;

        Ok((http_router, Arc::new(http_state)))
    }

    /// Start unified REST + gRPC API server with graceful shutdown support
    async fn start_api_server(
        &self,
        shutdown_rx: watch::Receiver<bool>,
        http_router: axum::Router,
        shared_http_app_state: Arc<synctv_api::http::AppState>,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
        let api_address = self.config.api_address();
        let grpc_router = self
            .build_grpc_router(
                &self.config,
                shutdown_rx.clone(),
                shared_http_app_state.clone(),
            )
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
            let proxy_cache_lifecycle = synctv_api::http::start_proxy_cache_lifecycle(
                &shared_http_app_state.proxy_slice_cache,
            );
            let graceful = async move {
                if rx.changed().await.is_err() {
                    warn!("API server shutdown signal channel closed");
                }
            };

            let server = axum::serve(
                listener,
                http_router
                    .merge(grpc_router)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(graceful);

            let mut lifecycle_handle = proxy_cache_lifecycle.handle;
            let lifecycle_cancel = proxy_cache_lifecycle.cancel;
            let server_result = tokio::select! {
                server_result = server => {
                    lifecycle_cancel.cancel();
                    match lifecycle_handle.await {
                        Ok(()) => info!("API proxy cache lifecycle stopped after API server exit"),
                        Err(error) if error.is_cancelled() => {
                            info!("API proxy cache lifecycle task cancelled during shutdown");
                        }
                        Err(error) => {
                            warn!(
                                error = %error,
                                "API proxy cache lifecycle task failed during shutdown"
                            );
                        }
                    }
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

            server_result.map_err(|e| anyhow::anyhow!("API server error: {e}"))?;

            info!("API server shut down gracefully");
            Ok(())
        });

        Ok(handle)
    }

    async fn start_metrics_server(
        &self,
        shutdown_rx: watch::Receiver<bool>,
        shared_http_app_state: Arc<synctv_api::http::AppState>,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
        let metrics_address = self.config.metrics_address();
        let listener_addr: std::net::SocketAddr = metrics_address
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid metrics address '{metrics_address}': {e}"))?;
        let listener = tokio::net::TcpListener::bind(listener_addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind metrics address {listener_addr}: {e}"))?;

        let metrics_app = synctv_api::http::health::create_metrics_router()
            .with_state(shared_http_app_state.as_ref().clone());
        let tls_acceptor = if self.config.metrics.tls.enabled {
            Some(tokio_rustls::TlsAcceptor::from(Arc::new(
                load_metrics_tls_server_config(&self.config.metrics.tls).await?,
            )))
        } else {
            None
        };

        info!(
            "Metrics server listening on {}://{}",
            if tls_acceptor.is_some() {
                "https"
            } else {
                "http"
            },
            listener_addr
        );

        let handle = tokio::spawn(async move {
            let mut rx = shutdown_rx;
            let mut connections = JoinSet::new();

            loop {
                tokio::select! {
                    biased;
                    _ = rx.changed() => {
                        break;
                    }
                    accept_result = listener.accept() => {
                        let (stream, peer_addr) = accept_result
                            .map_err(|error| anyhow::anyhow!("Metrics server accept error: {error}"))?;
                        let app = metrics_app.clone();
                        if let Some(acceptor) = tls_acceptor.clone() {
                            connections.spawn(async move {
                                match acceptor.accept(stream).await {
                                    Ok(tls_stream) => {
                                        if let Err(error) = serve_metrics_connection(tls_stream, app).await {
                                            warn!(peer = %peer_addr, error = %error, "metrics TLS connection failed");
                                        }
                                    }
                                    Err(error) => {
                                        warn!(peer = %peer_addr, error = %error, "metrics TLS handshake failed");
                                    }
                                }
                            });
                        } else {
                            connections.spawn(async move {
                                if let Err(error) = serve_metrics_connection(stream, app).await {
                                    warn!(peer = %peer_addr, error = %error, "metrics connection failed");
                                }
                            });
                        }
                    }
                }
            }

            shutdown_metrics_connection_tasks(&mut connections, METRICS_CONNECTION_DRAIN_TIMEOUT)
                .await;

            info!("Metrics server shut down gracefully");
            Ok(())
        });

        Ok(handle)
    }

    async fn start_management_server(
        &self,
        shutdown_rx: watch::Receiver<bool>,
        shared_http_app_state: Arc<synctv_api::http::AppState>,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
        let management_apis = management_apis_from_http_state(shared_http_app_state.as_ref())?;
        let node_id = self.services.realtime_event_service.node_id().to_string();
        let cluster_client = self
            .services
            .node_registry
            .as_ref()
            .filter(|_| self.config.cluster_runtime_enabled())
            .filter(|_| !self.config.cluster.secret.is_empty())
            .map(|node_registry| {
                Arc::new(synctv_cluster::grpc::ClusterClient::from_runtime(
                    node_registry.clone(),
                    synctv_cluster::grpc::ClusterClientConfig {
                        self_node_id: node_id.clone(),
                    },
                ))
            });

        spawn_management_server(ManagementServerConfig {
            config: Arc::new(self.config.clone()),
            user_service: self.services.user_service.clone(),
            admin_api: management_apis.admin,
            provider_common_api: management_apis.provider_common,
            client_api: management_apis.client,
            alist_api: management_apis.alist,
            bilibili_api: management_apis.bilibili,
            emby_api: management_apis.emby,
            slice_cache_runtime: Arc::new(ManagementProxySliceCacheRuntime::new(
                shared_http_app_state.proxy_slice_cache.clone(),
            )),
            cluster_client,
            node_id,
            lifecycle_controller: self.lifecycle_controller.clone(),
            shutdown_rx,
        })
        .await
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
        await_runtime_server_shutdown, await_task_shutdown, cleanup_partial_startup,
        complete_test_unpublish, coordinator_shutdown_deadline, livestream_shutdown_timeout_secs,
        management_apis_from_http_state, map_background_task_exit, map_runtime_server_exit,
        shutdown_after_startup_failure, shutdown_livestream_state,
        shutdown_metrics_connection_tasks, shutdown_runtime_phase, spawn_admin_event_listener,
        LivestreamShutdown, SharedProviderPlaybackRuntime, StartupFailureShutdownContext,
        FORCE_SHUTDOWN_COORDINATOR_BUDGET,
    };
    use crate::shutdown::ShutdownCoordinator;
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::Duration;
    use synctv_core::{
        cache::UsernameCache,
        repository::{ProviderInstanceRepository, UserProviderCredentialRepository},
        service::{JwtService, ProvidersManager, RemoteProviderManager, RoomService, UserService},
        Config,
    };
    use synctv_core_testing::{
        create_test_brute_force_protection_service, create_test_token_blacklist_store_service,
    };
    use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};
    use tokio::sync::{oneshot, watch};
    use tokio::task::JoinSet;
    use tokio_util::sync::CancellationToken;

    fn test_user_service(pool: &sqlx::PgPool) -> UserService {
        let jwt_service =
            JwtService::new("test-jwt-secret-key-for-testing-minimum-length").expect("jwt");
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 64, 60);

        UserService::new_for_tests(
            pool,
            jwt_service,
            username_cache,
            create_test_token_blacklist_store_service(),
            synctv_core::cache::KeyBuilder::new("test"),
            create_test_brute_force_protection_service(),
        )
    }

    #[tokio::test]
    async fn management_server_reuses_shared_http_app_state_instances() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");
        let config = Arc::new(Config::default());
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(pool.clone()));
        let shared_runtime = SharedProviderPlaybackRuntime::new(&config, None)
            .expect("test proxy signing key should derive");
        let user_service = Arc::new(test_user_service(&pool));
        let room_service = Arc::new(
            RoomService::new_for_tests(pool.clone(), (*user_service).clone())
                .expect("room service should build"),
        );
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
            ProviderInstanceRepository::new(pool.clone()),
        )));
        let providers_manager = Arc::new(
            ProvidersManager::new(provider_instance_manager.clone())
                .expect("providers manager should build"),
        );
        let providers = synctv_core::provider::ProviderSet::new_with_ssrf_guard(
            provider_instance_manager.clone(),
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .expect("provider set should build");
        let settings_service = Arc::new(synctv_core::service::SettingsService::new(
            synctv_core::repository::SettingsRepository::new(pool.clone()),
            pool.clone(),
        ));
        let settings_registry = Arc::new(synctv_core::service::SettingsRegistry::new(
            settings_service.clone(),
        ));
        let (audit_service, _audit_handle) = synctv_core::service::AuditService::new(pool.clone());
        let http_state =
            synctv_api::http::create_app_state_from_config(synctv_api::http::RouterConfig {
                config: config.clone(),
                user_service,
                user_cache: Arc::new(synctv_core::cache::UserCache::local_only(
                    128,
                    60,
                    300,
                    "test:user:".to_string(),
                )),
                room_service,
                content_filter: synctv_core::service::ContentFilter::new(),
                provider_instance_manager,
                user_provider_credential_repository: credential_repo.clone(),
                providers,
                event_service: Arc::new(synctv_api::runtime::LocalNoopRealtimeEventService::new()),
                connection_manager: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
                jwt_service: JwtService::new("test-jwt-secret-key-for-testing-minimum-length")
                    .expect("jwt"),
                realtime_fanout_service:
                    synctv_api::realtime_fanout::disabled_realtime_fanout_service(),
                oauth2_service: None,
                passkey_service: None,
                settings_service: Some(settings_service),
                settings_registry: Some(settings_registry),
                email_service: None,
                email_token_service: None,
                publish_key_service: None,
                notification_service: None,
                chat_service: None,
                audit_service: Arc::new(audit_service),
                live_streaming_infrastructure: None,
                rate_limiter: Arc::new(synctv_core::service::RateLimiter::local_only(
                    "test:".to_string(),
                )),
                ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(
                    None,
                )),
                redis_runtime: None,
                shared_provider_stores: Some(shared_runtime.provider_stores.clone()),
                shared_proxy_signing_key: Some(shared_runtime.signing_key.clone()),
                builtin_stun_url: None,
                webrtc_status:
                    synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
                credential_encryption: None,
                proxy_slice_cache: Arc::new(synctv_proxy::slice_cache::SliceCache::new(
                    synctv_proxy::slice_cache::SliceCacheConfig::default(),
                )),
                ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
                proxy_http_client: synctv_proxy::build_proxy_http_client(
                    synctv_common::ssrf::SsrfGuard::strict_policy(),
                )
                .expect("proxy HTTP client should build for tests"),
                messaging_rate_limit_config: synctv_core::service::RateLimitConfig::default(),
                heartbeat_schedule: synctv_api::impls::HeartbeatSchedule::production(),
                providers_manager: Some(providers_manager),
            })
            .expect("test HTTP app state should build");
        let management_apis =
            management_apis_from_http_state(&http_state).expect("shared management APIs");

        assert!(
            management_apis.client.signing_key.is_some(),
            "management client API must carry proxy signing key for signed playback"
        );
        assert!(
            management_apis.client.provider_stores.is_some(),
            "management client API must carry provider stores for versioned playback mappings"
        );
        assert!(Arc::ptr_eq(
            &management_apis.alist,
            &http_state.shared_api_runtime.alist_api
        ));
        assert!(Arc::ptr_eq(
            &management_apis.bilibili,
            &http_state.shared_api_runtime.bilibili_api
        ));
        assert!(Arc::ptr_eq(
            &management_apis.emby,
            &http_state.shared_api_runtime.emby_api
        ));
        assert!(
            Arc::ptr_eq(
                management_apis
                    .client
                    .signing_key
                    .as_ref()
                    .expect("management client API should set signing key"),
                &shared_runtime.signing_key
            ),
            "management client API must reuse the configured proxy signing key"
        );
        assert!(
            Arc::ptr_eq(
                management_apis
                    .client
                    .provider_stores
                    .as_ref()
                    .expect("management client API should set provider stores"),
                &shared_runtime.provider_stores
            ),
            "shared HTTP state must reuse the shared provider store registry"
        );
        assert!(
            Arc::ptr_eq(
                management_apis
                    .client
                    .credential_repo
                    .as_ref()
                    .expect("management client API should keep credential repo"),
                &credential_repo
            ),
            "shared HTTP state must keep the credential repository wiring"
        );
        assert!(
            Arc::ptr_eq(
                &management_apis.client,
                &http_state.shared_api_runtime.client_api
            ),
            "management server must reuse the shared client API instance"
        );
        assert!(
            Arc::ptr_eq(
                &management_apis.admin,
                http_state
                    .shared_api_runtime
                    .admin_api
                    .as_ref()
                    .expect("shared runtime should include admin API when settings are wired")
            ),
            "management server must reuse the shared admin API instance"
        );
    }

    #[test]
    fn shared_provider_runtime_uses_exact_configured_key_prefix() {
        let config = Config::default();
        let shared_runtime = SharedProviderPlaybackRuntime::new(&config, None)
            .expect("test proxy signing key should derive");

        assert_eq!(
            shared_runtime.provider_stores.key_prefix(),
            config.redis.key_prefix,
            "shared provider runtime must preserve the configured Redis key prefix exactly"
        );
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
            None,
            None,
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
            fn name(&self) -> &'static str {
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
            StartupFailureShutdownContext {
                shutdown_tx,
                cleanup_cancel,
                cleanup_handle: Some(cleanup_handle),
                api_handle: None,
                metrics_handle: None,
                management_handle: None,
                deadline: tokio::time::Instant::now() + Duration::from_secs(1),
            },
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
            fn name(&self) -> &'static str {
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
            StartupFailureShutdownContext {
                shutdown_tx,
                cleanup_cancel,
                cleanup_handle: Some(cleanup_handle),
                api_handle: None,
                metrics_handle: None,
                management_handle: None,
                deadline: start + Duration::from_millis(50),
            },
            async {},
            coordinator,
        )
        .await;

        assert!(
            start.elapsed() < Duration::from_secs(1),
            "startup rollback must respect a shared absolute deadline"
        );
    }

    #[test]
    fn test_force_shutdown_keeps_short_coordinator_budget() {
        let before = tokio::time::Instant::now();
        let shutdown_start = before - Duration::from_secs(10);
        let deadline = coordinator_shutdown_deadline(shutdown_start, Duration::from_secs(30), true);
        let after = tokio::time::Instant::now();

        assert!(
            deadline > before,
            "force shutdown must still give coordinator hooks a chance to run"
        );
        assert!(
            deadline <= after + FORCE_SHUTDOWN_COORDINATOR_BUDGET + Duration::from_millis(100),
            "force shutdown coordinator budget should stay short, deadline drifted too far"
        );
    }

    #[test]
    fn test_graceful_shutdown_uses_original_coordinator_deadline() {
        let shutdown_start = tokio::time::Instant::now();
        let total_drain_budget = Duration::from_secs(30);

        assert_eq!(
            coordinator_shutdown_deadline(shutdown_start, total_drain_budget, false),
            shutdown_start + total_drain_budget
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
    async fn test_shutdown_metrics_connection_tasks_aborts_stuck_connections_after_timeout() {
        struct DropFlag(Arc<AtomicBool>);

        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_clone = Arc::clone(&dropped);
        let mut connections = JoinSet::new();
        connections.spawn(async move {
            let _guard = DropFlag(dropped_clone);
            std::future::pending::<()>().await;
        });

        shutdown_metrics_connection_tasks(&mut connections, Duration::from_millis(20)).await;

        assert!(
            dropped.load(Ordering::SeqCst),
            "metrics shutdown must abort stuck per-connection tasks after the drain timeout"
        );
        assert!(
            connections.is_empty(),
            "metrics shutdown must fully drain aborted connection tasks"
        );
    }

    #[tokio::test]
    async fn test_await_runtime_server_shutdown_zero_timeout_aborts_pending_task() {
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_clone = Arc::clone(&stopped);
        let handle = tokio::spawn(async move {
            std::future::pending::<()>().await;
            stopped_clone.store(true, Ordering::SeqCst);
            Ok::<(), anyhow::Error>(())
        });

        await_runtime_server_shutdown("graceful server", handle, Duration::ZERO).await;

        assert!(
            !stopped.load(Ordering::SeqCst),
            "zero timeout should abort immediately instead of waiting without a budget"
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
            std::future::pending::<Result<(), anyhow::Error>>().await
        });

        let (cleanup_tx, cleanup_rx) = oneshot::channel::<()>();
        let cleanup_handle = tokio::spawn(async move {
            let _ = cleanup_rx.await;
        });

        shutdown_runtime_phase(
            Some(api_handle),
            None,
            None,
            cleanup_handle,
            None,
            Duration::from_millis(60),
            false,
        )
        .await;

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
    async fn test_shutdown_runtime_phase_can_defer_management_wait() {
        let (management_tx, management_rx) = oneshot::channel::<()>();
        let management_handle = tokio::spawn(async move {
            let _ = management_rx.await;
            Ok::<(), anyhow::Error>(())
        });

        let (cleanup_tx, cleanup_rx) = oneshot::channel::<()>();
        let cleanup_handle = tokio::spawn(async move {
            let _ = cleanup_rx.await;
        });

        let deferred_management = shutdown_runtime_phase(
            None,
            None,
            Some(management_handle),
            cleanup_handle,
            None,
            Duration::from_millis(60),
            true,
        )
        .await;

        assert!(
            cleanup_tx.send(()).is_err(),
            "cleanup task should no longer be running after shutdown phase returns"
        );

        let management_handle =
            deferred_management.expect("management handle should be returned when deferred");
        await_runtime_server_shutdown("Management server", management_handle, Duration::ZERO).await;
        assert!(
            management_tx.send(()).is_err(),
            "deferred management handle should be aborted when no shutdown budget remains"
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
            async fn cleanup_local_publishers_for_server(&mut self, _timeout: Duration) {}

            fn force_shutdown_for_server(&mut self) {}

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

        shutdown_livestream_state(&mut livestream_state, Duration::from_secs(17)).await;

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

    #[test]
    fn test_livestream_shutdown_timeout_secs_rounds_subsecond_budget_up() {
        assert_eq!(
            livestream_shutdown_timeout_secs(Duration::from_millis(500)),
            1,
            "sub-second shutdown budgets should still grant a 1s graceful livestream drain"
        );
        assert_eq!(
            livestream_shutdown_timeout_secs(Duration::ZERO),
            0,
            "zero shutdown budget should remain zero"
        );
    }

    #[tokio::test]
    async fn test_shutdown_livestream_state_does_not_exceed_budget() {
        struct SlowLivestreamState;

        #[async_trait]
        impl LivestreamShutdown for SlowLivestreamState {
            async fn cleanup_local_publishers_for_server(&mut self, _timeout: Duration) {}

            fn force_shutdown_for_server(&mut self) {}

            async fn shutdown_for_server(&mut self, _timeout_secs: u64) -> bool {
                tokio::time::sleep(Duration::from_millis(50)).await;
                true
            }
        }

        let mut livestream_state = Some(SlowLivestreamState);

        let result = tokio::time::timeout(
            Duration::from_millis(20),
            shutdown_livestream_state(&mut livestream_state, Duration::from_millis(20)),
        )
        .await;

        assert!(
            result.is_ok(),
            "livestream shutdown must respect the caller's remaining shutdown budget"
        );
    }

    #[tokio::test]
    async fn test_shutdown_livestream_state_bounds_cleanup_by_budget() {
        struct SlowCleanupLivestreamState {
            shutdown_called: Arc<AtomicBool>,
            force_shutdown_called: Arc<AtomicBool>,
            cleanup_timeout_seen: Arc<std::sync::Mutex<Option<Duration>>>,
        }

        #[async_trait]
        impl LivestreamShutdown for SlowCleanupLivestreamState {
            async fn cleanup_local_publishers_for_server(&mut self, timeout: Duration) {
                *self
                    .cleanup_timeout_seen
                    .lock()
                    .expect("cleanup timeout mutex poisoned") = Some(timeout);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }

            fn force_shutdown_for_server(&mut self) {
                self.force_shutdown_called.store(true, Ordering::SeqCst);
            }

            async fn shutdown_for_server(&mut self, _timeout_secs: u64) -> bool {
                self.shutdown_called.store(true, Ordering::SeqCst);
                true
            }
        }

        let shutdown_called = Arc::new(AtomicBool::new(false));
        let force_shutdown_called = Arc::new(AtomicBool::new(false));
        let cleanup_timeout_seen = Arc::new(std::sync::Mutex::new(None));
        let mut livestream_state = Some(SlowCleanupLivestreamState {
            shutdown_called: Arc::clone(&shutdown_called),
            force_shutdown_called: Arc::clone(&force_shutdown_called),
            cleanup_timeout_seen: Arc::clone(&cleanup_timeout_seen),
        });

        let result = tokio::time::timeout(
            Duration::from_millis(30),
            shutdown_livestream_state(&mut livestream_state, Duration::from_millis(20)),
        )
        .await;

        assert!(
            result.is_ok(),
            "publisher cleanup must be bounded by the livestream shutdown budget"
        );
        assert_eq!(
            *cleanup_timeout_seen
                .lock()
                .expect("cleanup timeout mutex poisoned"),
            Some(Duration::from_millis(20)),
            "cleanup should receive the caller's remaining budget"
        );
        assert!(
            !shutdown_called.load(Ordering::SeqCst),
            "graceful livestream shutdown must not be polled with a zero remaining budget"
        );
        assert!(
            force_shutdown_called.load(Ordering::SeqCst),
            "livestream handle must still be force-aborted after cleanup consumes the budget"
        );
    }

    #[tokio::test]
    async fn test_admin_event_listener_stops_on_cancel() {
        use synctv_livestream::api::StreamTracker;
        use synctv_livestream::livestream::{ExternalPublishManager, PullStreamManager};
        use synctv_realtime::sync::{RealtimeConfig, RealtimeManager, RoomMessageHub};
        use tokio::sync::mpsc;

        let realtime_manager = RealtimeManager::new(RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: "test-node".to_string(),
            dedup_window: Duration::from_mins(1),
            critical_channel_capacity: 8,
            publish_channel_capacity: 8,
            key_prefix: "test:".to_string(),
            catchup_window_secs: 60,
            stream_max_length: 100,
            event_handler: None,
            parent_cancel_token: None,
        })
        .await
        .expect("realtime manager should be created");

        let registry = synctv_livestream::relay::local_stream_registry();
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
                synctv_common::ssrf::SsrfGuard::disabled(),
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
        let handle = spawn_admin_event_listener(Arc::new(realtime_manager), infra, cancel.clone());

        cancel.cancel();
        await_task_shutdown("admin event listener", handle, Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn test_admin_event_listener_kick_publisher_removes_registry_entry() {
        use chrono::Utc;
        use synctv_core::models::{MediaId, RoomId};
        use synctv_livestream::api::StreamTracker;
        use synctv_livestream::livestream::{ExternalPublishManager, PullStreamManager};
        use synctv_realtime::sync::{
            RealtimeConfig, RealtimeEvent, RealtimeManager, RoomMessageHub,
        };
        use tokio::sync::mpsc;

        let realtime_manager = RealtimeManager::new(RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: "test-node".to_string(),
            dedup_window: Duration::from_mins(1),
            critical_channel_capacity: 8,
            publish_channel_capacity: 8,
            key_prefix: "test:".to_string(),
            catchup_window_secs: 60,
            stream_max_length: 100,
            event_handler: None,
            parent_cancel_token: None,
        })
        .await
        .expect("realtime manager should be created");

        let room_id = RoomId::expect_positive(112_001);
        let media_id = MediaId::expect_positive(112_002);
        let room_id_string = room_id.to_string();
        let media_id_string = media_id.to_string();
        let registry = synctv_livestream::relay::local_stream_registry();
        registry
            .try_register_publisher(
                &room_id_string,
                &media_id_string,
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
                synctv_common::ssrf::SsrfGuard::disabled(),
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
        let realtime_manager = Arc::new(realtime_manager);
        let handle = spawn_admin_event_listener(realtime_manager.clone(), infra, cancel.clone());

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if realtime_manager
                    .admin_event_tx()
                    .send(RealtimeEvent::KickPublisher {
                        event_id: synctv_common::snanoid!(16),
                        room_id,
                        media_id,
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

        assert!(
            registry
                .get_publisher(&room_id_string, &media_id_string)
                .await
                .expect("registry lookup should succeed")
                .is_some(),
            "kick listener must not remove registry ownership before StreamHub processes unpublish"
        );

        complete_test_unpublish(
            &registry,
            &Arc::new(StreamTracker::new()),
            &room_id_string,
            &media_id_string,
        )
        .await;

        assert!(
            registry
                .get_publisher(&room_id_string, &media_id_string)
                .await
                .expect("registry lookup should succeed")
                .is_none(),
            "actual unpublish completion should remove registry entry"
        );

        cancel.cancel();
        await_task_shutdown("admin event listener", handle, Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn test_admin_event_listener_kick_user_from_room_only_removes_room_local_publishers() {
        use chrono::Utc;
        use synctv_core::models::{MediaId, RoomId, UserId};
        use synctv_livestream::api::StreamTracker;
        use synctv_livestream::livestream::{ExternalPublishManager, PullStreamManager};
        use synctv_realtime::sync::{
            RealtimeConfig, RealtimeEvent, RealtimeManager, RoomMessageHub,
        };
        use tokio::sync::mpsc;

        let realtime_manager = RealtimeManager::new(RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: "test-node".to_string(),
            dedup_window: Duration::from_mins(1),
            critical_channel_capacity: 8,
            publish_channel_capacity: 8,
            key_prefix: "test:".to_string(),
            catchup_window_secs: 60,
            stream_max_length: 100,
            event_handler: None,
            parent_cancel_token: None,
        })
        .await
        .expect("realtime manager should be created");

        let room_id = RoomId::expect_positive(112_001);
        let other_room_id = RoomId::expect_positive(112_004);
        let media_id = MediaId::expect_positive(112_002);
        let other_media_id = MediaId::expect_positive(112_005);
        let user_id = UserId::expect_positive(112_003);
        let room_id_string = room_id.to_string();
        let other_room_id_string = other_room_id.to_string();
        let media_id_string = media_id.to_string();
        let other_media_id_string = other_media_id.to_string();
        let user_id_string = user_id.to_string();
        let registry = synctv_livestream::relay::local_stream_registry();
        registry
            .try_register_publisher(
                &room_id_string,
                &media_id_string,
                "test-node",
                "publisher-user",
                "127.0.0.1:50051",
            )
            .await
            .expect("room-1 publisher should register");
        registry
            .try_register_publisher(
                &other_room_id_string,
                &other_media_id_string,
                "test-node",
                &user_id_string,
                "127.0.0.1:50051",
            )
            .await
            .expect("room-2 publisher should register");

        let tracker = Arc::new(StreamTracker::new());
        tracker.insert(
            user_id_string.clone(),
            room_id_string.clone(),
            media_id_string.clone(),
            &room_id_string,
            &media_id_string,
        );
        tracker.insert(
            user_id_string.clone(),
            other_room_id_string.clone(),
            other_media_id_string.clone(),
            &other_room_id_string,
            &other_media_id_string,
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
                synctv_common::ssrf::SsrfGuard::disabled(),
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
        let realtime_manager = Arc::new(realtime_manager);
        let handle = spawn_admin_event_listener(realtime_manager.clone(), infra, cancel.clone());

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if realtime_manager
                    .admin_event_tx()
                    .send(RealtimeEvent::KickUserFromRoom {
                        event_id: synctv_common::snanoid!(16),
                        room_id,
                        user_id,
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

        assert!(
            registry
                .get_publisher(&room_id_string, &media_id_string)
                .await
                .expect("registry lookup should succeed")
                .is_some(),
            "room-scoped kick must not remove registry ownership before StreamHub processes unpublish"
        );
        assert!(
            registry
                .get_publisher(&other_room_id_string, &other_media_id_string)
                .await
                .expect("registry lookup should succeed")
                .is_some(),
            "publisher in another room must remain registered"
        );

        complete_test_unpublish(&registry, &tracker, &room_id_string, &media_id_string).await;

        assert!(
            registry
                .get_publisher(&room_id_string, &media_id_string)
                .await
                .expect("registry lookup should succeed")
                .is_none(),
            "actual unpublish completion should remove targeted publisher"
        );
        assert!(
            registry
                .get_publisher(&other_room_id_string, &other_media_id_string)
                .await
                .expect("registry lookup should succeed")
                .is_some(),
            "publisher in another room must remain registered after targeted unpublish"
        );

        assert!(
            tracker
                .get_stream_user(&room_id_string, &media_id_string)
                .is_none(),
            "target room publisher must be removed from tracker"
        );
        assert_eq!(
            tracker
                .get_stream_user(&other_room_id_string, &other_media_id_string)
                .as_deref(),
            Some(user_id_string.as_str()),
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
