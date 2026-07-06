// Livestream server facade
// Single entry point for starting the entire livestream infrastructure:
// StreamHub, RTMP server, HLS remuxer, PullStreamManager,
// ExternalPublishManager, PublisherManager, and LiveStreamingInfrastructure.
// The synctv binary never touches synctv_xiu directly -- all xiu interaction
// is encapsulated here.

use crate::{
    api::{livestream::LiveStreamingInfrastructure, tracker::StreamTracker},
    error::StreamResult,
    livestream::{
        external_publish_manager::ExternalPublishManager, pull_manager::PullStreamManager,
        CleanupConfig, SegmentManager,
    },
    relay::{publisher_manager::PublisherManager, registry_trait::StreamRegistryTrait},
};
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use synctv_common::ssrf::SsrfGuard;
use synctv_core::config::{HlsOssConfig, HlsStorageBackend};
use synctv_core::service::LeaderCheck;
use synctv_xiu::hls::{segment_manager::CleanupAuthority, CustomHlsRemuxer, StreamRegistry};
use synctv_xiu::rtmp::auth::AuthCallback;
use synctv_xiu::storage::{FileStorage, HlsStorage, MemoryStorage, OssConfig, OssStorage};
use synctv_xiu::streamhub::StreamsHub;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Maximum number of `StreamHub` automatic restart attempts before giving up.
const HUB_MAX_RESTARTS: u32 = 10;
const HUB_REREGISTER_TIMEOUT_SECS: u64 = 60;

type ReregisterRequest = oneshot::Sender<()>;

async fn abort_and_log_join(task_name: &'static str, handle: &mut JoinHandle<()>) {
    handle.abort();
    match handle.await {
        Ok(()) => {}
        Err(error) if error.is_cancelled() => {}
        Err(error) => {
            warn!(
                task = task_name,
                error = %error,
                "livestream background task returned join error after abort"
            );
        }
    }
}

fn notify_oneshot(sender: oneshot::Sender<()>, description: &'static str) {
    if sender.send(()).is_err() {
        tracing::debug!(
            description,
            "oneshot receiver dropped before livestream notification"
        );
    }
}

async fn request_publisher_reregistration(
    reregister_tx: &mpsc::Sender<ReregisterRequest>,
    is_restarting: &AtomicBool,
    timeout_duration: std::time::Duration,
) {
    let (done_tx, done_rx) = oneshot::channel::<()>();

    match reregister_tx.try_send(done_tx) {
        Ok(()) => match tokio::time::timeout(timeout_duration, done_rx).await {
            Ok(Ok(())) => {
                info!("StreamHub restart: publisher re-registration completed");
            }
            Ok(Err(_)) => {
                warn!(
                    "StreamHub restart: publisher re-registration task dropped completion signal"
                );
            }
            Err(_) => {
                warn!(
                    timeout_secs = timeout_duration.as_secs(),
                    "StreamHub restart: publisher re-registration timed out; clearing restart guard"
                );
            }
        },
        Err(mpsc::error::TrySendError::Full(_)) => {
            warn!("StreamHub restart: re-registration channel full; clearing restart guard");
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            warn!("StreamHub restart: re-registration task exited; clearing restart guard");
        }
    }

    is_restarting.store(false, Ordering::Release);
}

struct LeaderCleanupAuthority {
    leader_check: Arc<dyn LeaderCheck>,
}

impl LeaderCleanupAuthority {
    fn new(leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self { leader_check }
    }
}

impl CleanupAuthority for LeaderCleanupAuthority {
    fn should_cleanup(&self) -> bool {
        self.leader_check.is_leader()
    }
}

struct HubCycleTasks {
    rtmp_cancel_token: CancellationToken,
    rtmp_handle: Option<JoinHandle<()>>,
    forwarder_handle: Option<JoinHandle<()>>,
}

impl HubCycleTasks {
    fn new() -> Self {
        Self {
            rtmp_cancel_token: CancellationToken::new(),
            rtmp_handle: None,
            forwarder_handle: None,
        }
    }

    async fn replace(
        &mut self,
        rtmp_cancel_token: CancellationToken,
        rtmp_handle: JoinHandle<()>,
        forwarder_handle: JoinHandle<()>,
    ) {
        self.shutdown().await;
        self.rtmp_cancel_token = rtmp_cancel_token;
        self.rtmp_handle = Some(rtmp_handle);
        self.forwarder_handle = Some(forwarder_handle);
    }

    async fn shutdown(&mut self) {
        self.rtmp_cancel_token.cancel();
        if let Some(handle) = self.rtmp_handle.take() {
            let mut handle = handle;
            abort_and_log_join("hub cycle RTMP server", &mut handle).await;
        }
        if let Some(handle) = self.forwarder_handle.take() {
            let mut handle = handle;
            abort_and_log_join("hub cycle broadcast forwarder", &mut handle).await;
        }
    }
}

pub struct LivestreamConfig {
    pub rtmp_address: String,
    pub gop_cache_size: usize,
    pub node_id: String,
    pub cleanup_check_interval_seconds: u64,
    pub stream_timeout_seconds: u64,
    /// Whether multi-replica cluster runtime is enabled.
    pub distributed_enabled: bool,
    /// Cluster secret for authenticating gRPC HLS proxy calls
    pub cluster_secret: Option<String>,
    /// Maximum gRPC message size for cross-node HLS relay calls.
    pub grpc_max_message_size_bytes: usize,
    /// Whether cross-node HLS relay calls negotiate gzip compression.
    pub grpc_compression_enabled: bool,
    /// Maximum memory (in megabytes) for the GOP cache per stream.
    /// 0 means use the built-in default (500 MB).
    pub gop_cache_max_memory_mb: u64,
    /// Maximum FLV tag data size accepted from external HTTP-FLV sources.
    pub max_flv_tag_size_bytes: usize,
    /// Advertised shared API address of this node for cross-node proxying.
    /// Used by `PublisherManager` for re-registration after `StreamHub` restart.
    pub api_address: String,
    /// Maximum memory (in megabytes) for in-memory HLS segment storage.
    /// 0 means use the built-in default (512 MB).
    pub hls_memory_max_mb: u64,
    /// HLS segment storage backend.
    pub hls_storage_backend: HlsStorageBackend,
    /// Base path for file-backed HLS storage.
    pub hls_storage_path: String,
    /// S3-compatible object storage settings for the OSS backend.
    pub hls_oss: HlsOssConfig,
    /// Global SSRF policy for outbound livestream pull requests.
    pub ssrf_guard: SsrfGuard,
}

fn build_hls_storage(config: &LivestreamConfig) -> StreamResult<Arc<dyn HlsStorage>> {
    match config.hls_storage_backend {
        HlsStorageBackend::File | HlsStorageBackend::SharedFile => {
            let path = config.hls_storage_path.trim();
            if path.is_empty() {
                return Err(crate::error::StreamError::InvalidState(
                    "hls_storage_backend=file/shared_file requires a non-empty hls_storage_path"
                        .to_string(),
                ));
            }

            info!(
                hls_storage_path = %path,
                hls_storage_backend = ?config.hls_storage_backend,
                "HLS storage backend: filesystem"
            );
            Ok(Arc::new(FileStorage::new(path)))
        }
        HlsStorageBackend::Oss => {
            let oss = &config.hls_oss;
            let storage = OssStorage::new(OssConfig {
                endpoint: oss.endpoint.clone(),
                access_key_id: oss.access_key_id.clone(),
                secret_access_key: oss.secret_access_key.clone(),
                bucket: oss.bucket.clone(),
                region: oss.region.clone(),
                base_path: oss.base_path.clone(),
                public_url_prefix: String::new(),
                presign_expires_in: 3600,
            })
            .map_err(|error| {
                crate::error::StreamError::InvalidState(format!(
                    "failed to initialize HLS OSS storage: {error}"
                ))
            })?;

            info!(
                endpoint = %oss.endpoint,
                bucket = %oss.bucket,
                base_path = %oss.base_path,
                "HLS storage backend: OSS/S3-compatible object storage"
            );
            Ok(Arc::new(storage))
        }
        HlsStorageBackend::Memory => {
            let storage: Arc<dyn HlsStorage> = if config.hls_memory_max_mb > 0 {
                let max_bytes_mb = usize::try_from(config.hls_memory_max_mb).map_err(|_| {
                    crate::error::StreamError::InvalidState(format!(
                        "hls_memory_max_mb={} exceeds platform usize",
                        config.hls_memory_max_mb
                    ))
                })?;
                let max_bytes = max_bytes_mb.checked_mul(1024 * 1024).ok_or_else(|| {
                    crate::error::StreamError::InvalidState(format!(
                        "hls_memory_max_mb={} overflows byte capacity",
                        config.hls_memory_max_mb
                    ))
                })?;
                info!(
                    "HLS memory storage max set to {} MB",
                    config.hls_memory_max_mb,
                );
                Arc::new(MemoryStorage::with_limits(max_bytes, 0))
            } else {
                Arc::new(MemoryStorage::new())
            };

            if config.distributed_enabled {
                warn!(
                    "HLS storage is using in-memory backend in cluster mode. \
                     Each segment request will require gRPC proxy to the publisher node. \
                     Use shared_file or OSS storage for production multi-replica HLS."
                );
            }

            Ok(storage)
        }
    }
}

/// Handle returned by [`LivestreamServer::start`].
///
/// Owns the spawned tasks (`StreamHub` event loop, RTMP server, HLS remuxer,
/// `PublisherManager`) and exposes the shared infrastructure components.
pub struct LivestreamHandle {
    pub infrastructure: Arc<LiveStreamingInfrastructure>,
    pub(crate) pull_manager: Arc<PullStreamManager>,
    hub_handle: JoinHandle<()>,
    hub_cycle_tasks: Arc<tokio::sync::Mutex<HubCycleTasks>>,
    hls_remuxer_handle: JoinHandle<()>,
    publisher_manager_handle: JoinHandle<()>,
    /// Inner re-registration task spawned inside `publisher_manager_handle`.
    /// Must be tracked separately to prevent task leaks on shutdown.
    reregister_task_handle: JoinHandle<()>,
    /// Cancellation token for the inner re-registration task.
    reregister_cancel_token: CancellationToken,
    hls_shutdown_token: CancellationToken,
    /// HLS segment cleanup task handle.
    /// Must be tracked to prevent task leaks when `LivestreamHandle` is dropped.
    hls_cleanup_handle: JoinHandle<()>,
}

impl LivestreamHandle {
    async fn cleanup_local_publishers_on_shutdown(&self) {
        if self.infrastructure.local_node_id.is_empty() {
            self.infrastructure.user_stream_tracker.clear();
            return;
        }

        if let Err(e) = self
            .infrastructure
            .registry
            .cleanup_all_publishers_for_node(&self.infrastructure.local_node_id)
            .await
        {
            warn!(
                node_id = %self.infrastructure.local_node_id,
                error = %e,
                "Failed to cleanup local publisher registrations during shutdown"
            );
        } else {
            info!(
                node_id = %self.infrastructure.local_node_id,
                "Cleaned up local publisher registrations during shutdown"
            );
        }

        self.infrastructure.user_stream_tracker.clear();
    }

    /// Abort all spawned tasks in reverse startup order.
    ///
    /// This is a fast shutdown that immediately aborts all tasks.
    /// For graceful shutdown that waits for tasks to complete, use `shutdown_graceful`.
    pub fn shutdown(&self) {
        // Cancel the inner re-registration task first
        self.reregister_cancel_token.cancel();
        self.reregister_task_handle.abort();
        self.publisher_manager_handle.abort();
        // Cancel HLS tasks (remuxer and cleanup)
        self.hls_shutdown_token.cancel();
        self.hls_cleanup_handle.abort();
        self.hls_remuxer_handle.abort();
        self.hub_handle.abort();
        let hub_cycle_tasks = Arc::clone(&self.hub_cycle_tasks);
        if crate::util::try_spawn(async move {
            hub_cycle_tasks.lock().await.shutdown().await;
        })
        .is_none()
        {
            warn!("No Tokio runtime available for StreamHub cycle task shutdown");
        }
        self.infrastructure.user_stream_tracker.clear();
    }

    /// Shutdown all spawned tasks.
    ///
    /// Tasks with cancellation tokens (HLS remuxer, re-registration task) are signaled
    /// gracefully first, then waited on with a timeout. Other tasks (cleanup loops,
    /// publisher manager event loop, `StreamHub`) are aborted directly since they lack
    /// graceful shutdown signals.
    ///
    /// # Arguments
    /// * `timeout_secs` - Maximum seconds to wait for graceful tasks to complete
    ///   before falling back to abort.
    ///
    /// # Returns
    /// `true` if all token-based tasks shut down within the timeout,
    /// `false` if any had to be force-aborted.
    pub async fn shutdown_graceful(&mut self, timeout_secs: u64) -> bool {
        use tokio::time::{timeout, Duration};
        let timeout_duration = Duration::from_secs(timeout_secs);
        let mut all_graceful = true;

        // Shutdown in reverse startup order
        info!("Starting shutdown of livestream components...");

        // Remove this node from the shared publisher registry first. Later
        // shutdown phases can be force-aborted when the process budget is low,
        // but other cluster nodes must stop routing viewers to this node.
        self.cleanup_local_publishers_on_shutdown().await;

        // 1. Stop all managed stream pools to prevent zombie streams.
        info!("Stopping all managed pull streams...");
        self.pull_manager.stop_all().await;
        info!("All managed pull streams stopped");

        info!("Stopping all managed external publish streams...");
        self.infrastructure
            .external_publish_manager
            .stop_all()
            .await;
        info!("All managed external publish streams stopped");

        // 2. Stop the inner re-registration task gracefully via cancellation token
        self.reregister_cancel_token.cancel();
        if timeout(timeout_duration, &mut self.reregister_task_handle)
            .await
            .is_ok()
        {
            info!("Re-registration task stopped gracefully");
        } else {
            warn!("Re-registration task shutdown timed out, aborting");
            self.reregister_task_handle.abort();
            all_graceful = false;
        }

        // 3. Abort publisher manager event loop (no graceful signal)
        abort_and_log_join("publisher manager", &mut self.publisher_manager_handle).await;
        info!("Publisher manager stopped");

        // 4. Stop HLS tasks gracefully: first cancel the token, then await both
        // the remuxer and the cleanup task.
        self.hls_shutdown_token.cancel();
        if timeout(timeout_duration, &mut self.hls_remuxer_handle)
            .await
            .is_ok()
        {
            info!("HLS remuxer stopped gracefully");
        } else {
            warn!("HLS remuxer shutdown timed out, aborting");
            self.hls_remuxer_handle.abort();
            all_graceful = false;
        }

        // Await HLS cleanup task during graceful shutdown.
        if timeout(timeout_duration, &mut self.hls_cleanup_handle)
            .await
            .is_ok()
        {
            info!("HLS cleanup task stopped gracefully");
        } else {
            warn!("HLS cleanup task shutdown timed out, aborting");
            self.hls_cleanup_handle.abort();
            all_graceful = false;
        }

        // 8. Abort StreamHub (last, as other components depend on it).
        // The RTMP server is managed inside the hub loop and will be
        // shut down explicitly because aborting the hub task skips the loop's
        // per-cycle cleanup path.
        abort_and_log_join("StreamHub", &mut self.hub_handle).await;
        self.hub_cycle_tasks.lock().await.shutdown().await;
        info!("StreamHub stopped");

        if all_graceful {
            info!("Shutdown completed successfully");
        } else {
            warn!("Shutdown completed with some force-aborted tasks");
        }

        all_graceful
    }
}

impl Drop for LivestreamHandle {
    /// Clean up all background tasks when the handle is dropped.
    ///
    /// This ensures that even if the caller forgets to call `shutdown()` or
    /// `shutdown_graceful()`, cancellation tokens are cancelled and background
    /// task handles are aborted.
    fn drop(&mut self) {
        // Cancel all cancellation tokens to signal tasks to exit
        self.reregister_cancel_token.cancel();
        self.hls_shutdown_token.cancel();

        // Abort all task handles to ensure they terminate immediately
        // (in case they don't respond to cancellation tokens)
        self.reregister_task_handle.abort();
        self.publisher_manager_handle.abort();
        self.hls_cleanup_handle.abort();
        self.hls_remuxer_handle.abort();
        self.hub_handle.abort();
        let hub_cycle_tasks = Arc::clone(&self.hub_cycle_tasks);
        if crate::util::try_spawn(async move {
            hub_cycle_tasks.lock().await.shutdown().await;
        })
        .is_none()
        {
            warn!("LivestreamHandle drop: no Tokio runtime available for StreamHub cycle task shutdown");
        }
        self.infrastructure.user_stream_tracker.clear();
    }
}

pub struct LivestreamServer {
    config: LivestreamConfig,
    publisher_registry: Arc<dyn StreamRegistryTrait>,
    user_stream_tracker: Arc<StreamTracker>,
    auth: Option<Arc<dyn AuthCallback>>,
    hls_cleanup_leader: Arc<dyn LeaderCheck>,
    /// Pre-bound RTMP listener for early port conflict detection.
    rtmp_listener: Option<tokio::net::TcpListener>,
    /// Shared flag to reject publications during StreamHub restart.
    /// Created early so it can be shared with auth callback before start().
    is_restarting_flag: Arc<AtomicBool>,
}

impl LivestreamServer {
    pub fn new(
        config: LivestreamConfig,
        publisher_registry: Arc<dyn StreamRegistryTrait>,
        user_stream_tracker: Arc<StreamTracker>,
    ) -> Self {
        Self {
            config,
            publisher_registry,
            user_stream_tracker,
            auth: None,
            hls_cleanup_leader: Arc::new(synctv_core::service::AlwaysLeader),
            rtmp_listener: None,
            is_restarting_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Use a pre-bound TCP listener for RTMP instead of binding internally.
    /// This allows early port conflict detection before spawning the server.
    #[must_use]
    pub fn with_rtmp_listener(mut self, listener: tokio::net::TcpListener) -> Self {
        self.rtmp_listener = Some(listener);
        self
    }

    /// Set RTMP auth callback
    #[must_use]
    pub fn with_auth(mut self, auth: Arc<dyn AuthCallback>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Set the leader check used for shared HLS storage cleanup.
    ///
    /// Only `shared_file` and `oss` use this gate. Local `file` and `memory`
    /// backends still clean on every replica because their data is per-process
    /// or per-node.
    #[must_use]
    pub fn with_hls_cleanup_leader(mut self, leader_check: Arc<dyn LeaderCheck>) -> Self {
        self.hls_cleanup_leader = leader_check;
        self
    }

    /// Get a clone of the is_restarting flag for sharing with auth callback.
    /// This allows external auth implementations to check if StreamHub is restarting
    /// and reject new publications during the restart window.
    #[must_use]
    pub fn restarting_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.is_restarting_flag)
    }

    /// Start the entire livestream infrastructure.
    ///
    /// Creates `StreamHub`, RTMP server, HLS remuxer, `PullStreamManager`,
    /// `ExternalPublishManager`, `PublisherManager`, and `LiveStreamingInfrastructure`.
    /// Returns a handle with public components.
    pub fn start(self) -> StreamResult<LivestreamHandle> {
        let local_node_id = self.config.node_id.clone();
        if local_node_id.is_empty() {
            return Err(crate::error::StreamError::InvalidState(
                "livestream node_id is required: empty node_id causes stream ownership confusion. \
                 Set node_id in the livestream config."
                    .to_string(),
            ));
        }

        // Build all fallible local resources before spawning background tasks.
        // This keeps start() transactional: validation failures cannot leave an
        // RTMP/StreamHub/HLS task running without a returned handle to shut it down.
        let hls_storage = build_hls_storage(&self.config)?;

        // 1. Create StreamHub channels and hub (bounded to prevent OOM under load)
        let (event_sender, event_receiver) =
            mpsc::channel(synctv_xiu::streamhub::define::STREAM_HUB_EVENT_CHANNEL_CAPACITY);
        let mut streams_hub = StreamsHub::new(event_sender.clone(), event_receiver);

        let external_publish_manager = Arc::new(
            ExternalPublishManager::with_timeouts(
                self.publisher_registry.clone(),
                local_node_id.clone(),
                self.config.api_address.clone(),
                event_sender.clone(),
                self.config.ssrf_guard.clone(),
                self.config.cleanup_check_interval_seconds,
                self.config.stream_timeout_seconds,
            )?
            .with_max_flv_tag_size_bytes(self.config.max_flv_tag_size_bytes),
        );

        // Create a shared gRPC connection pool for HlsProxy and PullStreamManager
        // to avoid redundant HTTP/2 connections to the same publisher nodes.
        let shared_grpc_pool = crate::grpc::GrpcConnectionPool::with_defaults();

        let hls_proxy =
            crate::grpc::HlsProxyClient::with_defaults(self.config.cluster_secret.clone())
                .with_grpc_max_message_size(self.config.grpc_max_message_size_bytes)
                .with_grpc_compression(self.config.grpc_compression_enabled)
                .with_connection_pool(shared_grpc_pool.clone());

        let pull_manager = Arc::new(
            PullStreamManager::with_timeouts(
                self.publisher_registry.clone(),
                event_sender.clone(),
                self.config.cleanup_check_interval_seconds,
                self.config.stream_timeout_seconds,
            )
            .with_connection_pool(shared_grpc_pool)
            .with_grpc_max_message_size(self.config.grpc_max_message_size_bytes)
            .with_grpc_compression(self.config.grpc_compression_enabled)
            .with_cluster_secret(self.config.cluster_secret.clone()),
        );

        // Create a long-lived external broadcast channel that survives StreamHub restarts.
        // The StreamHub's internal broadcast channel is recreated on each restart, which
        // would leave existing receivers (PublisherManager, HLS remuxer) stale.
        // Instead, we subscribe to the internal broadcast and forward events to this
        // external channel on each hub (re)start cycle.
        let (external_broadcast_tx, _) =
            tokio::sync::broadcast::channel::<synctv_xiu::streamhub::define::BroadcastEvent>(1000);
        let broadcast_receiver = external_broadcast_tx.subscribe();
        let hls_broadcast_receiver = external_broadcast_tx.subscribe();
        let hls_hub_event_sender = streams_hub.get_hub_event_sender();

        // Clone registry for cleanup on StreamHub restart
        let registry_for_cleanup = self.publisher_registry.clone();
        let node_id_for_cleanup = self.config.node_id.clone();
        // Clone user_stream_tracker for cleanup on StreamHub restart
        // This ensures stale local entries are cleared when Redis entries are cleaned
        let user_stream_tracker_for_cleanup = self.user_stream_tracker.clone();
        // Request channel used by the StreamHub restart loop to ask
        // PublisherManager to re-register tracked publishers. Each request
        // carries an ack so the restart loop owns the restarting flag lifetime.
        let (reregister_tx, mut reregister_rx) =
            mpsc::channel::<ReregisterRequest>(HUB_MAX_RESTARTS as usize);
        // Shared flag to suppress silent-publisher detection during StreamHub restart.
        // Set before cleanup begins, cleared after re-registration completes.
        // Also checked by auth callback (via restarting_flag()) to reject new publications.
        // Use the flag created in LivestreamServer::new so it can be shared with auth.
        let is_restarting_flag = Arc::clone(&self.is_restarting_flag);
        // Mutex to serialize restart operations and prevent race conditions.
        // This ensures only one restart flow executes at a time, preventing:
        // - Corrupted state from parallel cleanup_all_publishers_for_node calls
        // - Lost re-registration signals
        // - Inconsistent is_restarting flag state
        let restart_mutex = Arc::new(Mutex::new(()));
        // Compute per-stream GOP cache memory limit from config (0 means use default).
        let per_stream_max_bytes: Option<usize> = if self.config.gop_cache_max_memory_mb > 0 {
            let max_bytes_mb =
                usize::try_from(self.config.gop_cache_max_memory_mb).map_err(|_| {
                    crate::error::StreamError::InvalidState(format!(
                        "gop_cache_max_memory_mb={} exceeds platform usize",
                        self.config.gop_cache_max_memory_mb
                    ))
                })?;
            let max_bytes = max_bytes_mb.checked_mul(1024 * 1024).ok_or_else(|| {
                crate::error::StreamError::InvalidState(format!(
                    "gop_cache_max_memory_mb={} overflows byte capacity",
                    self.config.gop_cache_max_memory_mb
                ))
            })?;
            info!(
                "GOP cache max memory set to {} MB per stream",
                self.config.gop_cache_max_memory_mb,
            );
            Some(max_bytes)
        } else {
            None
        };

        // RTMP server config -- cloned into the hub restart loop so we can
        // recreate the RTMP server with a fresh CancellationToken on each cycle.
        let rtmp_address = self.config.rtmp_address.clone();
        let rtmp_gop_cache_size = self.config.gop_cache_size;
        let rtmp_auth = self.auth.clone();
        let rtmp_event_sender = event_sender.clone();
        let reregister_tx_for_hub = reregister_tx.clone();
        let is_restarting_for_hub = Arc::clone(&is_restarting_flag);
        let restart_mutex_for_hub = Arc::clone(&restart_mutex);
        let pull_manager_for_hub = Arc::clone(&pull_manager);
        let external_publish_manager_for_hub = Arc::clone(&external_publish_manager);
        // Pre-bound listener for first cycle (enables early port conflict detection)
        let rtmp_listener = self.rtmp_listener;
        let hub_cycle_tasks = Arc::new(tokio::sync::Mutex::new(HubCycleTasks::new()));
        let hub_cycle_tasks_for_hub = Arc::clone(&hub_cycle_tasks);

        // 2. Spawn StreamHub event loop with automatic recovery
        let hub_handle = tokio::spawn(async move {
            const INITIAL_BACKOFF_SECS: u64 = 1;
            const MAX_BACKOFF_SECS: u64 = 30;

            let mut restart_count: u32 = 0;
            // Pre-bound listener for first cycle only (enables early port conflict detection)
            let mut first_cycle_listener = rtmp_listener;

            loop {
                // Subscribe to the hub's internal broadcast and forward to the
                // external channel. A new subscription is needed on each restart
                // because the hub recreates its internal broadcast::Sender.
                let mut internal_rx = streams_hub.get_client_event_consumer();
                let ext_tx = external_broadcast_tx.clone();
                let forwarder_handle = tokio::spawn(async move {
                    loop {
                        match internal_rx.recv().await {
                            Ok(event) => {
                                // Best-effort forward; if no external receivers, that's fine
                                if ext_tx.send(event).is_err() {
                                    tracing::debug!(
                                        "internal-to-external broadcast forwarder has no receivers"
                                    );
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!(
                                    "Internal-to-external broadcast forwarder lagged by {n} events"
                                );
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                break;
                            }
                        }
                    }
                });

                // Create a fresh cancellation token and RTMP server for each cycle.
                // CancellationToken is single-use: once cancelled it stays cancelled,
                // so we must create a new one on every restart to keep the RTMP server
                // functional.
                let rtmp_session_token = CancellationToken::new();
                let stream_callbacks = synctv_xiu::rtmp::callbacks::StreamEventCallbacks {
                    on_publisher_start: Some(Arc::new(|| {
                        synctv_core::metrics::livestream::LIVESTREAM_ACTIVE_PUBLISHERS.inc();
                    })),
                    on_publisher_stop: Some(Arc::new(|| {
                        synctv_core::metrics::livestream::LIVESTREAM_ACTIVE_PUBLISHERS.dec();
                    })),
                    on_viewer_join: Some(Arc::new(|| {
                        synctv_core::metrics::livestream::LIVESTREAM_ACTIVE_VIEWERS.inc();
                    })),
                    on_viewer_leave: Some(Arc::new(|| {
                        synctv_core::metrics::livestream::LIVESTREAM_ACTIVE_VIEWERS.dec();
                    })),
                };
                let mut rtmp_server = synctv_xiu::rtmp::server::RtmpServer::new(
                    rtmp_address.clone(),
                    rtmp_event_sender.clone(),
                    rtmp_gop_cache_size,
                    rtmp_auth.clone(),
                    per_stream_max_bytes,
                )
                .with_callbacks(stream_callbacks)
                .with_cancellation_token(&rtmp_session_token.clone());

                // Use pre-bound listener on first cycle for early port conflict detection.
                // On subsequent cycles (after StreamHub restart), the RTMP server binds internally.
                if let Some(listener) = first_cycle_listener.take() {
                    rtmp_server = rtmp_server.with_listener(listener);
                }

                let rtmp_handle = tokio::spawn(async move {
                    if let Err(e) = rtmp_server.run().await {
                        error!("RTMP server error: {}", e);
                    }
                });
                hub_cycle_tasks_for_hub
                    .lock()
                    .await
                    .replace(rtmp_session_token.clone(), rtmp_handle, forwarder_handle)
                    .await;

                info!("Starting StreamHub event loop...");
                let run_result = streams_hub.run().await;

                // Hub exited -- stop the RTMP server and forwarder for this cycle
                hub_cycle_tasks_for_hub.lock().await.shutdown().await;

                // Aborted sessions may not call their stop callbacks, leaving
                // gauges permanently inflated. Reset to 0 since all sessions
                // are terminated on restart.
                synctv_core::metrics::livestream::LIVESTREAM_ACTIVE_PUBLISHERS.set(0);
                synctv_core::metrics::livestream::LIVESTREAM_ACTIVE_VIEWERS.set(0);

                restart_count += 1;

                // Record restart metrics with exit reason
                let reason = match &run_result {
                    Ok(()) => "channel_closed",
                    Err(_) => "panic",
                };
                synctv_core::metrics::streamhub::STREAMHUB_RESTARTS_TOTAL
                    .with_label_values(&[reason])
                    .inc();

                warn!(
                    restart_count,
                    max_restarts = HUB_MAX_RESTARTS,
                    reason,
                    "StreamHub event loop exited unexpectedly, cleaning up local state before restart..."
                );

                info!("Cancelled all active RTMP sessions due to StreamHub restart");

                // Acquire the restart mutex to serialize cleanup operations.
                // This prevents race conditions when multiple restarts happen in quick succession:
                // - Prevents parallel cleanup_all_publishers_for_node calls
                // - Ensures is_restarting flag state is consistent
                // - Serializes re-registration notifications
                let _restart_guard = restart_mutex_for_hub.lock().await;

                // Set the restarting flag BEFORE cleanup to suppress silent-publisher
                // detection during the restart window. This prevents false cleanup of
                // publishers that are temporarily missing from Redis during the
                // cleanup -> re-register cycle.
                // Also checked by the application RTMP auth callback to reject new publications.
                is_restarting_for_hub.store(true, Ordering::Release);

                info!("StreamHub restart: stopping all managed pull streams...");
                pull_manager_for_hub.stop_all().await;
                info!("StreamHub restart: stopping all managed external publish streams...");
                external_publish_manager_for_hub.stop_all().await;
                info!("StreamHub restart: all managed streams stopped");

                // Clean up all local publisher registrations from Redis
                // This ensures stale state doesn't persist after restart
                if let Err(e) = registry_for_cleanup
                    .cleanup_all_publishers_for_node(&node_id_for_cleanup)
                    .await
                {
                    error!("Failed to cleanup publishers on StreamHub restart: {}", e);
                }

                // Clear the local user_stream_tracker to remove stale entries
                // After Redis cleanup, the tracker entries no longer have corresponding
                // publishers in Redis, so they must be cleared to prevent incorrect lookups
                user_stream_tracker_for_cleanup.clear();

                // Re-register all active publishers immediately. The restart loop
                // waits for completion and then clears the shared restart guard,
                // so publication blocking is not owned by a detached background task.
                request_publisher_reregistration(
                    &reregister_tx_for_hub,
                    &is_restarting_for_hub,
                    std::time::Duration::from_secs(HUB_REREGISTER_TIMEOUT_SECS),
                )
                .await;

                if restart_count >= HUB_MAX_RESTARTS {
                    error!(
                        "StreamHub has restarted {} times, giving up to avoid infinite restart loop",
                        restart_count
                    );
                    break;
                }

                let backoff_secs = INITIAL_BACKOFF_SECS
                    .saturating_mul(1u64 << (restart_count - 1).min(16))
                    .min(MAX_BACKOFF_SECS);
                info!(
                    "Waiting {} seconds before restarting StreamHub...",
                    backoff_secs
                );
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;

                // Mutex guard is dropped here, allowing the next restart (if any) to proceed
            }
        });

        let mut segment_manager = SegmentManager::new(hls_storage, CleanupConfig::default());
        if matches!(
            self.config.hls_storage_backend,
            HlsStorageBackend::SharedFile | HlsStorageBackend::Oss
        ) {
            info!(
                hls_storage_backend = ?self.config.hls_storage_backend,
                "HLS shared storage cleanup will run only on the cluster leader"
            );
            segment_manager = segment_manager.with_cleanup_authority(Arc::new(
                LeaderCleanupAuthority::new(self.hls_cleanup_leader.clone()),
            ));
        }
        let segment_manager = Arc::new(segment_manager);
        let stream_registry: StreamRegistry = Arc::new(DashMap::new());
        let hls_shutdown_token = CancellationToken::new();

        // Start segment cleanup background task and track the handle
        let hls_cleanup_handle = segment_manager
            .clone()
            .start_cleanup_task(hls_shutdown_token.clone());

        // Create PublisherManager early so the activity callback can be wired to the HLS remuxer.
        // The manager itself is started later (step 7) after all components are created.
        // Use with_restarting_flag to share the is_restarting flag with the StreamHub restart loop.
        let publisher_manager = Arc::new(
            PublisherManager::with_restarting_flag(
                self.publisher_registry.clone(),
                self.config.node_id.clone(),
                event_sender.clone(),
                Arc::clone(&is_restarting_flag),
            )
            .with_api_address(self.config.api_address.clone()),
        );

        // Create activity callback for the HLS remuxer to record publisher data activity.
        // This prevents the silent publisher detection from incorrectly timing out active publishers.
        let activity_pm = Arc::clone(&publisher_manager);
        let activity_callback: synctv_xiu::hls::PublisherActivityCallback =
            Arc::new(move |room_id: &str, media_id: &str| {
                activity_pm.record_publisher_activity(room_id, media_id);
            });

        // Create active publishers source for post-lag reconciliation in the HLS remuxer.
        let reconcile_pm = Arc::clone(&publisher_manager);
        let active_publishers_source: synctv_xiu::hls::ActivePublishersSource =
            Arc::new(move || reconcile_pm.active_publisher_streams());

        // Start the HLS remuxer
        let hls_segment_manager = segment_manager.clone();
        let hls_stream_registry = stream_registry.clone();
        let hls_cancel = hls_shutdown_token.clone();
        let hls_remuxer_handle = tokio::spawn(async move {
            let mut remuxer = CustomHlsRemuxer::new(
                hls_broadcast_receiver,
                hls_hub_event_sender,
                hls_segment_manager,
                hls_stream_registry,
                hls_cancel,
            )
            .with_activity_callback(activity_callback)
            .with_active_publishers_source(active_publishers_source);

            let timer = synctv_core::metrics::stream::STREAM_RELAY_DURATION
                .with_label_values(&["hls"])
                .start_timer();
            synctv_core::metrics::stream::ACTIVE_RELAY_STREAMS.inc();

            if let Err(e) = remuxer.run().await {
                error!("HLS remuxer error: {}", e);

                let err_str = e.to_string();
                let error_type = if err_str.contains("timeout") {
                    "timeout"
                } else if err_str.contains("connection") {
                    "connection"
                } else {
                    "other"
                };
                synctv_core::metrics::stream::STREAM_ERRORS
                    .with_label_values(&["hls", error_type])
                    .inc();
            }

            timer.observe_duration();
            synctv_core::metrics::stream::ACTIVE_RELAY_STREAMS.dec();
        });

        info!("HLS remuxer started (in-process, no standalone HTTP server)");

        // Create cancellation token for the re-registration task.
        // This allows graceful shutdown to prevent task leaks.
        let reregister_cancel_token = CancellationToken::new();
        let reregister_token_clone = reregister_cancel_token.clone();

        // Spawn the re-registration task as a separate top-level task.
        // This task listens for re-registration signals from StreamHub restart.
        // Previously, this was spawned inside the publisher_manager task, causing
        // a task leak because tokio::spawn detaches child tasks.
        let reregister_task_handle = {
            let pm_for_reregister = Arc::clone(&publisher_manager);
            tokio::spawn(async move {
                loop {
                    // Use tokio::select to respond to both signals and cancellation
                    tokio::select! {
                        () = reregister_token_clone.cancelled() => {
                            // Shutdown signal received - exit cleanly
                            info!("Re-registration task received shutdown signal");
                            break;
                        }
                        request = reregister_rx.recv() => {
                            let Some(done_tx) = request else {
                                break;
                            };
                            pm_for_reregister.reregister_all_publishers().await;
                            notify_oneshot(done_tx, "publisher re-registration completion");
                        }
                    }
                }
                info!("Re-registration task exited");
            })
        };

        let publisher_manager_handle = tokio::spawn({
            let pm = Arc::clone(&publisher_manager);
            async move {
                pm.start(broadcast_receiver).await;
            }
        });

        // 8. Wire HLS proxy client (created in step 5a) into infrastructure
        // 9. Create LiveStreamingInfrastructure with HLS components wired in
        let infrastructure = Arc::new(
            LiveStreamingInfrastructure::from_parts(
                self.publisher_registry,
                event_sender,
                pull_manager.clone(),
                external_publish_manager,
                self.user_stream_tracker,
                local_node_id,
            )
            .with_segment_manager(segment_manager)
            .with_hls_stream_registry(stream_registry)
            .with_hls_storage_backend(self.config.hls_storage_backend)
            .with_hls_proxy(hls_proxy),
        );

        info!(
            "Livestream infrastructure initialized, RTMP server listening on rtmp://{}",
            self.config.rtmp_address,
        );

        Ok(LivestreamHandle {
            infrastructure,
            pull_manager,
            hub_handle,
            hub_cycle_tasks,
            hls_remuxer_handle,
            publisher_manager_handle,
            reregister_task_handle,
            reregister_cancel_token,
            hls_shutdown_token,
            hls_cleanup_handle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tracker::StreamTracker;
    use crate::relay::TestStreamRegistry;
    use bytes::Bytes;
    use tempfile::tempdir;
    use tokio::time::{timeout, Duration};

    type TestResult = anyhow::Result<()>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn test_config() -> LivestreamConfig {
        LivestreamConfig {
            rtmp_address: "127.0.0.1:0".to_string(),
            gop_cache_size: 1024 * 1024,
            node_id: "test-node".to_string(),
            cleanup_check_interval_seconds: 1,
            stream_timeout_seconds: 5,
            distributed_enabled: false,
            cluster_secret: None,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            gop_cache_max_memory_mb: 0,
            max_flv_tag_size_bytes: 10 * 1024 * 1024,
            api_address: "127.0.0.1:0".to_string(),
            hls_memory_max_mb: 0,
            hls_storage_backend: HlsStorageBackend::Memory,
            hls_storage_path: String::new(),
            hls_oss: HlsOssConfig::default(),
            ssrf_guard: SsrfGuard::strict_policy(),
        }
    }

    async fn bind_local_listener() -> anyhow::Result<(tokio::net::TcpListener, std::net::SocketAddr)>
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let local_addr = listener.local_addr()?;
        Ok((listener, local_addr))
    }

    fn config_for_rtmp_addr(local_addr: std::net::SocketAddr) -> LivestreamConfig {
        let mut config = test_config();
        config.rtmp_address = local_addr.to_string();
        config
    }

    #[tokio::test]
    async fn test_build_hls_storage_uses_memory_when_shared_storage_disabled() -> TestResult {
        let dir = tempdir()?;
        let mut config = test_config();
        config.hls_storage_path = dir.path().display().to_string();

        let storage = build_hls_storage(&config)?;
        storage
            .write("room1", "media1", "seg1", Bytes::from_static(b"segment"))
            .await?;

        assert!(
            !dir.path()
                .join("room1")
                .join("media1")
                .join("seg1")
                .exists(),
            "memory storage should not create files on disk"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_reregister_request_clears_restart_flag_after_ack() -> TestResult {
        let is_restarting = AtomicBool::new(true);
        let (tx, mut rx) = mpsc::channel::<ReregisterRequest>(1);

        let request = request_publisher_reregistration(
            &tx,
            &is_restarting,
            std::time::Duration::from_secs(1),
        );
        tokio::pin!(request);

        let done_tx = tokio::select! {
            done_tx = rx.recv() => done_tx.ok_or_else(|| test_error("request should be queued"))?,
            () = &mut request => return Err(test_error("re-registration request completed before ack")),
        };
        assert!(
            is_restarting.load(Ordering::Acquire),
            "restart flag must remain set until re-registration is acknowledged"
        );
        notify_oneshot(done_tx, "test re-registration acknowledgement");

        (&mut request).await;
        assert!(
            !is_restarting.load(Ordering::Acquire),
            "restart flag must be cleared after acknowledged re-registration"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_reregister_request_clears_restart_flag_when_channel_closed() {
        let is_restarting = AtomicBool::new(true);
        let (tx, rx) = mpsc::channel::<ReregisterRequest>(1);
        drop(rx);

        request_publisher_reregistration(&tx, &is_restarting, std::time::Duration::from_millis(10))
            .await;

        assert!(
            !is_restarting.load(Ordering::Acquire),
            "restart flag must not stay set forever when re-registration task is unavailable"
        );
    }

    #[tokio::test]
    async fn test_reregister_request_clears_restart_flag_when_channel_full() -> TestResult {
        let is_restarting = AtomicBool::new(true);
        let (tx, _rx) = mpsc::channel::<ReregisterRequest>(1);
        let (queued_tx, _queued_rx) = oneshot::channel::<()>();
        tx.try_send(queued_tx)
            .map_err(|_| test_error("test precondition: channel should accept first request"))?;

        request_publisher_reregistration(&tx, &is_restarting, std::time::Duration::from_millis(10))
            .await;

        assert!(
            !is_restarting.load(Ordering::Acquire),
            "restart flag must not stay set forever when re-registration queue is saturated"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_build_hls_storage_uses_shared_file_backend() -> TestResult {
        let dir = tempdir()?;
        let mut config = test_config();
        config.distributed_enabled = true;
        config.cluster_secret = Some("cluster-secret".to_string());
        config.hls_storage_backend = HlsStorageBackend::SharedFile;
        config.hls_storage_path = dir.path().display().to_string();

        let storage = build_hls_storage(&config)?;
        let bucket = synctv_core::SystemClock.now().timestamp() / 60;
        let segment = format!("{bucket}_seg1");
        storage
            .write("room1", "media1", &segment, Bytes::from_static(b"segment"))
            .await?;

        assert!(
            dir.path()
                .join("segments")
                .join(bucket.to_string())
                .join("room1")
                .join("media1")
                .join(segment)
                .exists(),
            "shared storage should persist HLS segments on disk"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_cluster_secret_alone_does_not_enable_cluster_hls_semantics() -> TestResult {
        let dir = tempdir()?;
        let mut config = test_config();
        config.cluster_secret = Some("cluster-secret".to_string());
        config.hls_storage_path = dir.path().display().to_string();

        let storage = build_hls_storage(&config)?;
        storage
            .write("room1", "media1", "seg1", Bytes::from_static(b"segment"))
            .await?;

        assert!(
            !dir.path()
                .join("room1")
                .join("media1")
                .join("seg1")
                .exists(),
            "cluster_secret without distributed_enabled must remain standalone memory storage"
        );
        Ok(())
    }

    fn test_tracker() -> Arc<StreamTracker> {
        Arc::new(StreamTracker::new())
    }

    #[tokio::test]
    async fn test_reregister_task_cleanup_on_shutdown() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry, test_tracker());

        let handle = server.start()?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.shutdown();

        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            handle.publisher_manager_handle.is_finished(),
            "publisher_manager_handle should be finished after shutdown"
        );

        assert!(
            handle.reregister_task_handle.is_finished(),
            "reregister_task_handle should be finished after shutdown - task leak detected!"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_reregister_task_respects_cancellation() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry, test_tracker());

        let mut handle = server.start()?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        let result = timeout(Duration::from_secs(2), handle.shutdown_graceful(1)).await;

        assert!(
            result.is_ok(),
            "shutdown_graceful should complete within timeout"
        );

        assert!(
            handle.publisher_manager_handle.is_finished(),
            "publisher_manager_handle should be finished after graceful shutdown"
        );

        assert!(
            handle.reregister_task_handle.is_finished(),
            "reregister_task_handle should be finished after graceful shutdown"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_shutdown_graceful_cleans_local_publishers_from_registry_and_tracker() -> TestResult
    {
        let registry = Arc::new(TestStreamRegistry::new());
        let tracker = test_tracker();

        let server = LivestreamServer::new(test_config(), registry.clone(), tracker.clone());
        let mut handle = server.start()?;

        registry
            .try_register_publisher(
                "room-shutdown",
                "media-shutdown",
                "test-node",
                "user-shutdown",
                "127.0.0.1:50051",
            )
            .await?;
        tracker.insert(
            "user-shutdown".to_string(),
            "room-shutdown".to_string(),
            "media-shutdown".to_string(),
        );

        let result = timeout(Duration::from_secs(2), handle.shutdown_graceful(1)).await?;
        assert!(result, "shutdown_graceful should complete cleanly");

        assert!(
            !registry
                .is_stream_active("room-shutdown", "media-shutdown")
                .await?,
            "shutdown must remove local publisher registrations from the registry"
        );
        assert!(
            tracker.get_user_streams("user-shutdown").is_empty()
                && tracker.get_room_streams("room-shutdown").is_empty()
                && tracker
                    .get_stream_user("room-shutdown", "media-shutdown")
                    .is_none(),
            "shutdown must clear local publisher tracking entries"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_reregister_task_no_background_leak() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry.clone(), test_tracker());

        let handle = server.start()?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        let count_before = registry.register_call_count();

        handle.shutdown();

        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            handle.publisher_manager_handle.is_finished(),
            "publisher_manager_handle should be finished after shutdown"
        );
        assert!(
            handle.reregister_task_handle.is_finished(),
            "reregister_task_handle should be finished after shutdown"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let count_after = registry.register_call_count();

        assert_eq!(
            count_before, count_after,
            "No new register calls should happen after shutdown"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_drop_cleans_up_tasks() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry, test_tracker());

        let handle = server.start()?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        let publisher_finished = handle.publisher_manager_handle.is_finished();
        let reregister_finished = handle.reregister_task_handle.is_finished();

        assert!(
            !publisher_finished,
            "publisher_manager_handle should be running before drop"
        );
        assert!(
            !reregister_finished,
            "reregister_task_handle should be running before drop"
        );
        drop(handle);
        Ok(())
    }

    #[tokio::test]
    async fn test_hls_cleanup_task_terminated_on_drop() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry, test_tracker());

        let handle = server.start()?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            !handle.hls_shutdown_token.is_cancelled(),
            "hls_shutdown_token should not be cancelled before drop"
        );

        drop(handle);
        Ok(())
    }

    #[test]
    fn test_livestream_handle_drop_without_runtime_does_not_panic() -> TestResult {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let runtime = tokio::runtime::Runtime::new().map_err(|err| err.to_string())?;
            let handle = runtime.block_on(async {
                let registry = Arc::new(TestStreamRegistry::new());
                let server = LivestreamServer::new(test_config(), registry, test_tracker());
                server.start().map_err(|err| err.to_string())
            });

            drop(runtime);
            drop(handle?);
            Ok::<(), String>(())
        }));

        assert!(
            result
                .map_err(|_| test_error("drop should not panic"))?
                .is_ok(),
            "LivestreamHandle::drop must not panic when runtime is already gone"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_rtmp_pre_binding_works() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());

        let (listener, local_addr) = bind_local_listener().await?;
        let config = config_for_rtmp_addr(local_addr);

        let server =
            LivestreamServer::new(config, registry, test_tracker()).with_rtmp_listener(listener);

        let handle = server.start()?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn test_start_failure_on_hls_storage_validation_does_not_spawn_rtmp() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        let (listener, local_addr) = bind_local_listener().await?;

        let mut config = config_for_rtmp_addr(local_addr);
        config.hls_storage_backend = HlsStorageBackend::SharedFile;
        config.hls_storage_path = String::new();

        let server =
            LivestreamServer::new(config, registry, test_tracker()).with_rtmp_listener(listener);
        let err = match server.start() {
            Ok(handle) => {
                handle.shutdown();
                return Err(test_error(
                    "empty shared_file path should fail before spawning RTMP",
                ));
            }
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("hls_storage_path"),
            "unexpected error: {err}"
        );

        let rebound = tokio::net::TcpListener::bind(local_addr).await;
        assert!(
            rebound.is_ok(),
            "failed start must not leave an RTMP task bound to the prebound port: {rebound:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_start_failure_on_empty_node_id_does_not_spawn_rtmp() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        let (listener, local_addr) = bind_local_listener().await?;

        let mut config = config_for_rtmp_addr(local_addr);
        config.node_id = String::new();

        let server =
            LivestreamServer::new(config, registry, test_tracker()).with_rtmp_listener(listener);
        let err = match server.start() {
            Ok(handle) => {
                handle.shutdown();
                return Err(test_error("empty node_id should fail before spawning RTMP"));
            }
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("node_id"),
            "unexpected error: {err}"
        );

        let rebound = tokio::net::TcpListener::bind(local_addr).await;
        assert!(
            rebound.is_ok(),
            "failed start must not leave an RTMP task bound to the prebound port: {rebound:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_shutdown_releases_rtmp_port_for_rebind() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());

        let (listener, local_addr) = bind_local_listener().await?;
        let config = config_for_rtmp_addr(local_addr);

        let server =
            LivestreamServer::new(config, registry, test_tracker()).with_rtmp_listener(listener);

        let handle = server.start()?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.shutdown();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let rebound = tokio::net::TcpListener::bind(local_addr).await;
        assert!(
            rebound.is_ok(),
            "shutdown must release the RTMP listener so the port can be rebound: {rebound:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_shutdown_graceful_releases_rtmp_port_for_rebind() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());

        let (listener, local_addr) = bind_local_listener().await?;
        let config = config_for_rtmp_addr(local_addr);

        let server =
            LivestreamServer::new(config, registry, test_tracker()).with_rtmp_listener(listener);

        let mut handle = server.start()?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        let result = timeout(Duration::from_secs(2), handle.shutdown_graceful(1)).await?;
        assert!(result, "shutdown_graceful should complete cleanly");

        let rebound = tokio::net::TcpListener::bind(local_addr).await;
        assert!(
            rebound.is_ok(),
            "shutdown_graceful must release the RTMP listener so the port can be rebound: {rebound:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_port_conflict_detected_early() -> TestResult {
        let (_listener1, bound_addr) = bind_local_listener().await?;

        let result = tokio::net::TcpListener::bind(bound_addr).await;

        assert!(
            result.is_err(),
            "Binding to an already-in-use port should fail"
        );

        let err = match result {
            Ok(listener) => {
                drop(listener);
                return Err(test_error("binding to an already-in-use port should fail"));
            }
            Err(err) => err,
        };
        assert!(
            err.kind() == std::io::ErrorKind::AddrInUse,
            "Error should be AddrInUse, got: {err:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_livestream_server_without_prebound_listener() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry, test_tracker());

        let handle = server.start()?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.shutdown();
        Ok(())
    }
}
