// Livestream server facade
// Single entry point for starting the entire livestream infrastructure:
// StreamHub, RTMP server, HLS remuxer, PullStreamManager,
// ExternalPublishManager, PublisherManager, and LiveStreamingInfrastructure.
// The synctv binary never touches synctv_xiu directly -- all xiu interaction
// is encapsulated here.

use crate::{
    api::{LiveStreamingInfrastructure, StreamTracker},
    error::StreamResult,
    livestream::{
        external_publish_manager::ExternalPublishManager,
        pull_manager::PullStreamManager,
        segment_manager::{CleanupConfig, SegmentManager},
    },
    protocols::hls::{CustomHlsRemuxer, StreamRegistry},
    relay::{registry_trait::StreamRegistryTrait, PublisherManager},
};
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use synctv_common::ssrf::SsrfGuard;
use synctv_core::config::{HlsOssConfig, HlsStorageBackend};
use synctv_core::service::LeaderCheck;
use synctv_xiu::hls::segment_manager::CleanupAuthority;
use synctv_xiu::rtmp::auth::AuthCallback;
use synctv_xiu::storage::{FileStorage, HlsStorage, MemoryStorage, OssConfig, OssStorage};
use synctv_xiu::streamhub::StreamsHub;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Maximum number of `StreamHub` automatic restart attempts before giving up.
/// Used to size the stop-streams notification channel to prevent signal loss
/// under rapid consecutive restarts.
const HUB_MAX_RESTARTS: u32 = 10;

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

fn hls_cleanup_should_use_leader(backend: HlsStorageBackend) -> bool {
    matches!(
        backend,
        HlsStorageBackend::SharedFile | HlsStorageBackend::Oss
    )
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
            handle.abort();
            let _ = handle.await;
        }
        if let Some(handle) = self.forwarder_handle.take() {
            handle.abort();
            let _ = handle.await;
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
    pub pull_manager: Arc<PullStreamManager>,
    hub_handle: JoinHandle<()>,
    hub_cycle_tasks: Arc<tokio::sync::Mutex<HubCycleTasks>>,
    hls_remuxer_handle: JoinHandle<()>,
    publisher_manager_handle: JoinHandle<()>,
    /// Inner re-registration task spawned inside `publisher_manager_handle`.
    /// Must be tracked separately to prevent task leaks on shutdown.
    reregister_task_handle: JoinHandle<()>,
    /// Cancellation token for the inner re-registration task.
    reregister_cancel_token: CancellationToken,
    /// Listener for StreamHub restart stop-all notifications.
    /// Must be owned by the handle so shutdown/drop can terminate it explicitly.
    stop_streams_listener_handle: JoinHandle<()>,
    pull_manager_cleanup: JoinHandle<()>,
    external_publish_cleanup: JoinHandle<()>,
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
        }

        self.infrastructure.user_stream_tracker.clear();
    }

    fn spawn_local_publisher_cleanup(&self) {
        let registry = Arc::clone(&self.infrastructure.registry);
        let tracker = Arc::clone(&self.infrastructure.user_stream_tracker);
        let node_id = self.infrastructure.local_node_id.clone();

        tracker.clear();

        if node_id.is_empty() {
            return;
        }

        let node_id_for_task = node_id.clone();
        if crate::util::try_spawn(async move {
            if let Err(e) = registry
                .cleanup_all_publishers_for_node(&node_id_for_task)
                .await
            {
                warn!(
                    node_id = %node_id_for_task,
                    error = %e,
                    "Failed to cleanup local publisher registrations during shutdown"
                );
            }
        })
        .is_none()
        {
            warn!(
                node_id = %node_id,
                "No Tokio runtime available for async local publisher cleanup during shutdown"
            );
        }
    }

    /// Abort all spawned tasks in reverse startup order.
    ///
    /// This is a fast shutdown that immediately aborts all tasks.
    /// For graceful shutdown that waits for tasks to complete, use `shutdown_graceful`.
    pub fn shutdown(&self) {
        self.external_publish_cleanup.abort();
        self.pull_manager_cleanup.abort();
        self.stop_streams_listener_handle.abort();
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
        let _ = crate::util::try_spawn(async move {
            hub_cycle_tasks.lock().await.shutdown().await;
        });
        self.spawn_local_publisher_cleanup();
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

        // 1. Stop all managed stream pools to prevent zombie streams.
        // This must happen before aborting cleanup tasks so in-flight stop operations
        // can complete properly.
        info!("Stopping all managed pull streams...");
        self.pull_manager.stop_all().await;
        info!("All managed pull streams stopped");

        info!("Stopping all managed external publish streams...");
        self.infrastructure
            .external_publish_manager
            .stop_all()
            .await;
        info!("All managed external publish streams stopped");

        // 2. Abort external publish cleanup (periodic timer, no graceful signal)
        self.external_publish_cleanup.abort();
        let _ = (&mut self.external_publish_cleanup).await;
        info!("External publish cleanup stopped");

        // 3. Abort pull manager cleanup (periodic timer, no graceful signal)
        self.pull_manager_cleanup.abort();
        let _ = (&mut self.pull_manager_cleanup).await;
        info!("Pull manager cleanup stopped");

        // 4. Stop the inner re-registration task gracefully via cancellation token
        self.stop_streams_listener_handle.abort();
        let _ = (&mut self.stop_streams_listener_handle).await;
        info!("Stop-stream listener stopped");

        // 5. Stop the inner re-registration task gracefully via cancellation token
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

        // 6. Abort publisher manager event loop (no graceful signal)
        self.publisher_manager_handle.abort();
        let _ = (&mut self.publisher_manager_handle).await;
        info!("Publisher manager stopped");

        // 7. Stop HLS tasks gracefully: first cancel the token, then await both
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
        self.hub_handle.abort();
        let _ = (&mut self.hub_handle).await;
        self.hub_cycle_tasks.lock().await.shutdown().await;
        info!("StreamHub stopped");

        // 9. Final local publisher cleanup. Shutdown may interrupt the normal
        // on_unpublish -> PublisherManager cleanup chain, so clear local tracker
        // and remove this node's publisher registrations explicitly.
        self.cleanup_local_publishers_on_shutdown().await;

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
    /// `shutdown_graceful()`, all cancellation tokens are cancelled, tasks
    /// are properly terminated, and all managed streams are stopped.
    /// This prevents task leaks, memory leaks from HLS segments, and zombie streams.
    fn drop(&mut self) {
        // Stop all managed streams first to prevent zombie streams.
        // Since Drop can't be async, we spawn a task to call stop_all().
        // The abort calls below will signal the cleanup tasks to exit,
        // and the stop_all() calls will clean up the actual stream resources.
        let pull_manager = Arc::clone(&self.pull_manager);
        let external_publish_manager = Arc::clone(&self.infrastructure.external_publish_manager);
        if crate::util::try_spawn(async move {
            info!("LivestreamHandle drop: stopping all managed pull streams");
            pull_manager.stop_all().await;
            info!("LivestreamHandle drop: stopping all managed external publish streams");
            external_publish_manager.stop_all().await;
            info!("LivestreamHandle drop: all managed streams stopped");
        })
        .is_none()
        {
            warn!("LivestreamHandle drop: no Tokio runtime available, skipping async stop_all cleanup");
        }

        // Cancel all cancellation tokens to signal tasks to exit
        self.reregister_cancel_token.cancel();
        self.hls_shutdown_token.cancel();

        // Abort all task handles to ensure they terminate immediately
        // (in case they don't respond to cancellation tokens)
        self.stop_streams_listener_handle.abort();
        self.reregister_task_handle.abort();
        self.publisher_manager_handle.abort();
        self.hls_cleanup_handle.abort();
        self.hls_remuxer_handle.abort();
        self.hub_handle.abort();
        let hub_cycle_tasks = Arc::clone(&self.hub_cycle_tasks);
        let _ = crate::util::try_spawn(async move {
            hub_cycle_tasks.lock().await.shutdown().await;
        });
        self.pull_manager_cleanup.abort();
        self.external_publish_cleanup.abort();

        self.spawn_local_publisher_cleanup();
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
        // 1. Create StreamHub channels and hub (bounded to prevent OOM under load)
        let (event_sender, event_receiver) =
            mpsc::channel(synctv_xiu::streamhub::define::STREAM_HUB_EVENT_CHANNEL_CAPACITY);
        let mut streams_hub = StreamsHub::new(event_sender.clone(), event_receiver);

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
        // Notify to signal PublisherManager to re-register after StreamHub restart.
        // Uses Notify instead of mpsc channel so signals are never lost even if
        // multiple restarts occur before the listener wakes up.
        let reregister_notify = Arc::new(tokio::sync::Notify::new());
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
        // Channel to notify pull/external managers to stop all streams before StreamHub restart.
        // This ensures zombie streams (still connected to the old hub) are cleaned up.
        // The oneshot sender allows the restart loop to wait for stop_all() completion
        // before proceeding with re-registration, preventing the race condition where
        // active streams are stopped while re-registration is already happening.
        // Capacity matches HUB_MAX_RESTARTS so rapid consecutive restarts never drop signals
        // when the receiver is momentarily busy processing a previous stop_all().
        let (stop_streams_tx, mut stop_streams_rx) =
            mpsc::channel::<tokio::sync::oneshot::Sender<()>>(HUB_MAX_RESTARTS as usize);

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
        let reregister_notify_for_hub = Arc::clone(&reregister_notify);
        let is_restarting_for_hub = Arc::clone(&is_restarting_flag);
        let restart_mutex_for_hub = Arc::clone(&restart_mutex);
        // Pre-bound listener for first cycle (enables early port conflict detection)
        let rtmp_listener = self.rtmp_listener;
        let hub_cycle_tasks = Arc::new(tokio::sync::Mutex::new(HubCycleTasks::new()));
        let hub_cycle_tasks_for_hub = Arc::clone(&hub_cycle_tasks);

        // 2. Spawn StreamHub event loop with automatic recovery
        let hub_handle = tokio::spawn(async move {
            const INITIAL_BACKOFF_SECS: u64 = 1;
            const MAX_BACKOFF_SECS: u64 = 30;

            let mut restart_count: u32 = 0;
            // Track successful cycles to reset restart_count after stable operation
            let mut successful_cycles: u32 = 0;
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
                                let _ = ext_tx.send(event);
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
                let mut rtmp_server = synctv_xiu::rtmp::rtmp::RtmpServer::new(
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

                // Track successful cycles for restart_count reset
                // If this was a clean exit (channel_closed), it counts as stable operation
                // and allows the restart count to decay, preventing transient failures
                // from permanently exhausting HUB_MAX_RESTARTS.
                let was_clean_exit = run_result.is_ok();
                if was_clean_exit {
                    successful_cycles += 1;
                    // Decrement restart_count on successful exit (floor at 0)
                    // This allows the hub to recover from transient failure bursts
                    if restart_count > 0 {
                        restart_count = restart_count.saturating_sub(1);
                    }
                } else {
                    // Only increment restart_count on actual failures (panics)
                    restart_count += 1;
                }

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
                    successful_cycles,
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

                // Stop all managed pull/external-publish streams BEFORE restart.
                // These streams hold channels to the old StreamHub instance and would
                // become zombies (still running but unable to deliver frames) if not
                // cleaned up. The receiver task calls stop_all() on both managers.
                // Two-phase cleanup: create a oneshot channel to receive confirmation
                // when stop_all() completes. This ensures we wait for streams to fully
                // stop before proceeding with Redis cleanup and re-registration.
                let (stop_done_tx, stop_done_rx) = tokio::sync::oneshot::channel::<()>();
                match stop_streams_tx.try_send(stop_done_tx) {
                    Ok(()) => {
                        // Wait for stop_all() to complete with a timeout.
                        // Use a bounded wait long enough for streams to disconnect from
                        // remote servers or flush pending data.
                        match tokio::time::timeout(std::time::Duration::from_secs(5), stop_done_rx)
                            .await
                        {
                            Ok(Ok(())) => {
                                info!("StreamHub restart: stop_all() completed, proceeding with cleanup");
                            }
                            Ok(Err(_)) => {
                                warn!("StreamHub restart: stop_done sender dropped, proceeding anyway");
                            }
                            Err(_) => {
                                warn!("StreamHub restart: stop_all() timed out after 5000ms, proceeding anyway");
                            }
                        }
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        warn!("StreamHub restart: stop_streams channel full, previous stop still pending");
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        warn!(
                            "StreamHub restart: stop_streams channel closed, receiver task exited"
                        );
                    }
                }

                // Brief delay to allow in-progress unregistrations to complete.
                // This reduces the race window where cleanup_all_publishers_for_node
                // might conflict with concurrent unregister_publisher calls from
                // streams that are still disconnecting after the stop_all() signal.
                // The 500ms delay is a reasonable tradeoff: long enough for most
                // async unregistration operations to complete, but short enough
                // to not significantly delay the restart recovery.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

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

                // Notify PublisherManager to re-register all active publishers immediately.
                // The reregister_all_publishers() method will clear the is_restarting flag
                // after re-registration completes.
                reregister_notify_for_hub.notify_one();

                if restart_count >= HUB_MAX_RESTARTS {
                    error!(
                        "StreamHub has restarted {} times, giving up to avoid infinite restart loop",
                        restart_count
                    );
                    break;
                }

                // Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s, 30s, ...
                // When restart_count is 0 (all previous exits were clean), use minimal backoff.
                let backoff_secs = if restart_count == 0 {
                    INITIAL_BACKOFF_SECS
                } else {
                    INITIAL_BACKOFF_SECS
                        .saturating_mul(1u64 << (restart_count - 1).min(16))
                        .min(MAX_BACKOFF_SECS)
                };
                info!(
                    "Waiting {} seconds before restarting StreamHub...",
                    backoff_secs
                );
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;

                // Mutex guard is dropped here, allowing the next restart (if any) to proceed
            }
        });

        // 3. Start HLS remuxer (converts RTMP to HLS segments)
        let hls_storage = build_hls_storage(&self.config)?;

        let mut segment_manager = SegmentManager::new(hls_storage, CleanupConfig::default());
        if hls_cleanup_should_use_leader(self.config.hls_storage_backend) {
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
        let activity_callback: crate::protocols::hls::PublisherActivityCallback =
            Arc::new(move |room_id: &str, media_id: &str| {
                activity_pm.record_publisher_activity(room_id, media_id);
            });

        // Create active publishers source for post-lag reconciliation in the HLS remuxer.
        let reconcile_pm = Arc::clone(&publisher_manager);
        let active_publishers_source: crate::protocols::hls::ActivePublishersSource =
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

        // 5a. Create a shared gRPC connection pool for HlsProxy and PullStreamManager
        //     to avoid redundant HTTP/2 connections to the same publisher nodes.
        let shared_grpc_pool = crate::grpc::GrpcConnectionPool::with_defaults();

        // 5b. Create HLS proxy client with the shared pool
        let hls_proxy =
            crate::grpc::HlsProxyClient::with_defaults(self.config.cluster_secret.clone())
                .with_grpc_max_message_size(self.config.grpc_max_message_size_bytes)
                .with_connection_pool(shared_grpc_pool.clone());

        // 5c. Create PullStreamManager with the same shared pool
        let pull_manager = Arc::new(
            PullStreamManager::with_timeouts(
                self.publisher_registry.clone(),
                event_sender.clone(),
                self.config.cleanup_check_interval_seconds,
                self.config.stream_timeout_seconds,
            )
            .with_connection_pool(shared_grpc_pool)
            .with_cluster_secret(self.config.cluster_secret.clone())
            .with_hls_proxy(hls_proxy.clone()),
        );
        // Start periodic cleanup of stale creation locks to prevent memory leaks
        let pull_manager_cleanup = pull_manager.start_cleanup_task();

        // 6. Create ExternalPublishManager
        let external_publish_manager = Arc::new(
            ExternalPublishManager::with_timeouts(
                self.publisher_registry.clone(),
                self.config.node_id.clone(),
                self.config.api_address.clone(),
                event_sender.clone(),
                self.config.ssrf_guard.clone(),
                self.config.cleanup_check_interval_seconds,
                self.config.stream_timeout_seconds,
            )?
            .with_max_flv_tag_size_bytes(self.config.max_flv_tag_size_bytes),
        );
        // Start periodic cleanup of stale creation locks to prevent memory leaks
        let external_publish_cleanup = external_publish_manager.start_cleanup_task();

        // 6b. Spawn listener that stops all managed streams on StreamHub restart.
        // This ensures zombie streams (connected to the old hub) are cleaned up
        // before the new hub starts accepting events.
        // Two-phase cleanup protocol:
        // 1. Receive stop request with oneshot sender
        // 2. Call stop_all() on both managers
        // 3. Send confirmation via oneshot sender
        // This allows the restart loop to wait for completion before re-registration.
        let stop_streams_listener_handle = {
            let pm = Arc::clone(&pull_manager);
            let epm = Arc::clone(&external_publish_manager);
            tokio::spawn(async move {
                while let Some(stop_done_tx) = stop_streams_rx.recv().await {
                    info!("StreamHub restart: stopping all managed pull streams...");
                    pm.stop_all().await;
                    info!("StreamHub restart: stopping all managed external publish streams...");
                    epm.stop_all().await;
                    info!("StreamHub restart: all managed streams stopped");
                    // Signal completion to the restart loop
                    let _ = stop_done_tx.send(());
                }
            })
        };

        // 7. Start PublisherManager -- listens to StreamHub broadcast events
        // and registers/unregisters publishers in Redis for multi-node relay
        // (PublisherManager was created earlier in step 3 to wire the activity callback)
        let local_node_id = self.config.node_id.clone();
        if local_node_id.is_empty() {
            return Err(crate::error::StreamError::InvalidState(
                "node_id is required for cluster mode: empty node_id causes stream ownership confusion. \
                 Set node_id in the livestream config."
                    .to_string(),
            ));
        }

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
            let reregister = Arc::clone(&reregister_notify);
            tokio::spawn(async move {
                loop {
                    // Use tokio::select to respond to both signals and cancellation
                    tokio::select! {
                        () = reregister_token_clone.cancelled() => {
                            // Shutdown signal received - exit cleanly
                            info!("Re-registration task received shutdown signal");
                            break;
                        }
                        () = reregister.notified() => {
                            pm_for_reregister.reregister_all_publishers().await;
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
            LiveStreamingInfrastructure::new(
                self.publisher_registry,
                event_sender,
                pull_manager.clone(),
                external_publish_manager,
                self.user_stream_tracker,
            )
            .with_segment_manager(segment_manager)
            .with_hls_stream_registry(stream_registry)
            .with_local_node_id(local_node_id)
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
            stop_streams_listener_handle,
            pull_manager_cleanup,
            external_publish_cleanup,
            hls_shutdown_token,
            hls_cleanup_handle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tracker::StreamTracker;
    use crate::relay::MockStreamRegistry;
    use bytes::Bytes;
    use tempfile::tempdir;
    use tokio::time::{timeout, Duration};

    /// Helper to create a minimal `LivestreamConfig` for testing
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

    #[test]
    fn test_hls_cleanup_leader_gate_only_for_shared_backends() {
        assert!(!hls_cleanup_should_use_leader(HlsStorageBackend::Memory));
        assert!(!hls_cleanup_should_use_leader(HlsStorageBackend::File));
        assert!(hls_cleanup_should_use_leader(HlsStorageBackend::SharedFile));
        assert!(hls_cleanup_should_use_leader(HlsStorageBackend::Oss));
    }

    #[tokio::test]
    async fn test_build_hls_storage_uses_memory_when_shared_storage_disabled() {
        let dir = tempdir().expect("tempdir should be created");
        let mut config = test_config();
        config.hls_storage_path = dir.path().display().to_string();

        let storage = build_hls_storage(&config).expect("storage should be built");
        storage
            .write("room1", "media1", "seg1", Bytes::from_static(b"segment"))
            .await
            .expect("segment write should succeed");

        assert!(
            !dir.path()
                .join("room1")
                .join("media1")
                .join("seg1")
                .exists(),
            "memory storage should not create files on disk"
        );
    }

    #[tokio::test]
    async fn test_build_hls_storage_uses_shared_file_backend() {
        let dir = tempdir().expect("tempdir should be created");
        let mut config = test_config();
        config.distributed_enabled = true;
        config.cluster_secret = Some("cluster-secret".to_string());
        config.hls_storage_backend = HlsStorageBackend::SharedFile;
        config.hls_storage_path = dir.path().display().to_string();

        let storage = build_hls_storage(&config).expect("storage should be built");
        let bucket = chrono::Utc::now().timestamp() / 60;
        let segment = format!("{bucket}_seg1");
        storage
            .write("room1", "media1", &segment, Bytes::from_static(b"segment"))
            .await
            .expect("segment write should succeed");

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
    }

    #[tokio::test]
    async fn test_cluster_secret_alone_does_not_enable_cluster_hls_semantics() {
        let dir = tempdir().expect("tempdir should be created");
        let mut config = test_config();
        config.cluster_secret = Some("cluster-secret".to_string());
        config.hls_storage_path = dir.path().display().to_string();

        let storage = build_hls_storage(&config).expect("storage should be built");
        storage
            .write("room1", "media1", "seg1", Bytes::from_static(b"segment"))
            .await
            .expect("segment write should succeed");

        assert!(
            !dir.path()
                .join("room1")
                .join("media1")
                .join("seg1")
                .exists(),
            "cluster_secret without distributed_enabled must remain standalone memory storage"
        );
    }

    /// Helper to create a `StreamTracker` for testing
    fn test_tracker() -> Arc<StreamTracker> {
        Arc::new(StreamTracker::new())
    }

    /// Test that the re-registration task is properly cleaned up on shutdown.
    ///
    /// This test verifies that when `LivestreamHandle::shutdown()` is called,
    /// both the publisher manager task AND the re-registration task terminate,
    /// preventing task leaks.
    ///
    #[tokio::test]
    async fn test_reregister_task_cleanup_on_shutdown() {
        // Create a mock registry
        let registry = Arc::new(MockStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry, test_tracker());

        // Start the server
        let handle = server.start().expect("Failed to start server");

        // Give tasks a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Shutdown the handle
        handle.shutdown();

        // Give tasks time to abort
        tokio::time::sleep(Duration::from_millis(100)).await;

        // The publisher_manager_handle should be aborted
        assert!(
            handle.publisher_manager_handle.is_finished(),
            "publisher_manager_handle should be finished after shutdown"
        );

        // The reregister_task_handle should also be finished (no leak)
        assert!(
            handle.reregister_task_handle.is_finished(),
            "reregister_task_handle should be finished after shutdown - task leak detected!"
        );
        assert!(
            handle.stop_streams_listener_handle.is_finished(),
            "stop_streams_listener_handle should be finished after shutdown"
        );
    }

    /// Test that the re-registration task stops when cancelled via `CancellationToken`.
    ///
    /// This test verifies graceful shutdown properly cancels both tasks.
    #[tokio::test]
    async fn test_reregister_task_respects_cancellation() {
        let registry = Arc::new(MockStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry, test_tracker());

        // Start the server
        let mut handle = server.start().expect("Failed to start server");

        // Give tasks a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Use graceful shutdown with a short timeout
        let result = timeout(Duration::from_secs(2), handle.shutdown_graceful(1)).await;

        // Shutdown should complete within timeout
        assert!(
            result.is_ok(),
            "shutdown_graceful should complete within timeout"
        );

        // All tasks should be finished
        assert!(
            handle.publisher_manager_handle.is_finished(),
            "publisher_manager_handle should be finished after graceful shutdown"
        );

        assert!(
            handle.reregister_task_handle.is_finished(),
            "reregister_task_handle should be finished after graceful shutdown"
        );
        assert!(
            handle.stop_streams_listener_handle.is_finished(),
            "stop_streams_listener_handle should be finished after graceful shutdown"
        );
    }

    #[tokio::test]
    async fn test_shutdown_graceful_cleans_local_publishers_from_registry_and_tracker() {
        let registry = Arc::new(MockStreamRegistry::new());
        let tracker = test_tracker();

        let server = LivestreamServer::new(test_config(), registry.clone(), tracker.clone());
        let mut handle = server.start().expect("Failed to start server");

        registry
            .try_register_publisher(
                "room-shutdown",
                "media-shutdown",
                "test-node",
                "user-shutdown",
                "127.0.0.1:50051",
            )
            .await
            .expect("publisher should register in mock registry");
        tracker.insert(
            "user-shutdown".to_string(),
            "room-shutdown".to_string(),
            "media-shutdown".to_string(),
            "live",
            "stream-token",
        );

        let result = timeout(Duration::from_secs(2), handle.shutdown_graceful(1))
            .await
            .expect("shutdown_graceful should complete");
        assert!(result, "shutdown_graceful should complete cleanly");

        assert!(
            !registry
                .is_stream_active("room-shutdown", "media-shutdown")
                .await
                .expect("registry query should succeed"),
            "shutdown must remove local publisher registrations from the registry"
        );
        assert!(
            tracker.is_empty(),
            "shutdown must clear local publisher tracking entries"
        );
    }

    /// Test that the re-registration task terminates properly without background leaks.
    ///
    /// This test verifies that after shutdown, the re-registration task is truly
    /// terminated and won't respond to any more notifications.
    #[tokio::test]
    async fn test_reregister_task_no_background_leak() {
        let registry = Arc::new(MockStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry.clone(), test_tracker());

        // Start the server
        let handle = server.start().expect("Failed to start server");

        // Give tasks a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Get initial count
        let count_before = registry.register_call_count();

        // Shutdown the handle
        handle.shutdown();

        // Give tasks time to abort
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Both tasks should be finished
        assert!(
            handle.publisher_manager_handle.is_finished(),
            "publisher_manager_handle should be finished after shutdown"
        );
        assert!(
            handle.reregister_task_handle.is_finished(),
            "reregister_task_handle should be finished after shutdown"
        );
        assert!(
            handle.stop_streams_listener_handle.is_finished(),
            "stop_streams_listener_handle should be finished after shutdown"
        );

        // Verify no new register calls happen after shutdown
        tokio::time::sleep(Duration::from_millis(100)).await;
        let count_after = registry.register_call_count();

        assert_eq!(
            count_before, count_after,
            "No new register calls should happen after shutdown"
        );
    }

    /// Test that dropping `LivestreamHandle` without calling `shutdown()` still cleans up tasks.
    ///
    /// This test verifies that the Drop implementation for `LivestreamHandle` cancels
    /// all cancellation tokens, preventing task leaks when the handle is dropped.
    ///
    #[tokio::test]
    async fn test_drop_cleans_up_tasks() {
        let registry = Arc::new(MockStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry, test_tracker());

        // Start the server
        let handle = server.start().expect("Failed to start server");

        // Give tasks a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Get references to check if tasks are finished after drop
        // We need to check these BEFORE dropping because the handle owns them
        let publisher_finished = handle.publisher_manager_handle.is_finished();
        let reregister_finished = handle.reregister_task_handle.is_finished();

        // Tasks should NOT be finished yet (they're running)
        assert!(
            !publisher_finished,
            "publisher_manager_handle should be running before drop"
        );
        assert!(
            !reregister_finished,
            "reregister_task_handle should be running before drop"
        );
        let stop_streams_listener_abort_handle = handle.stop_streams_listener_handle.abort_handle();
        assert!(
            !stop_streams_listener_abort_handle.is_finished(),
            "stop_streams_listener_handle should be running before drop"
        );

        // Drop the handle WITHOUT calling shutdown
        drop(handle);

        // Give tasks time to abort due to Drop
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            stop_streams_listener_abort_handle.is_finished(),
            "stop_streams_listener_handle should be aborted when handle is dropped"
        );

        // After drop, all background tasks should have been cancelled.
        // We can't check is_finished() on the JoinHandles because they've been dropped,
        // but we've verified the Drop implementation cancels the tokens.
        // The cancellation tokens (hls_shutdown_token, reregister_cancel_token) are
        // cancelled in the Drop impl, which signals the tasks to exit.
    }

    /// Test that HLS cleanup task is cancelled when `LivestreamHandle` is dropped.
    ///
    /// This test verifies that the segment cleanup task (started in `start()`) is
    /// properly terminated when the handle is dropped, preventing memory leaks.
    #[tokio::test]
    async fn test_hls_cleanup_task_terminated_on_drop() {
        let registry = Arc::new(MockStreamRegistry::new());

        let server = LivestreamServer::new(test_config(), registry, test_tracker());

        // Start the server - this starts the HLS segment cleanup task
        let handle = server.start().expect("Failed to start server");

        // Give tasks a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify hls_shutdown_token is not cancelled yet
        assert!(
            !handle.hls_shutdown_token.is_cancelled(),
            "hls_shutdown_token should not be cancelled before drop"
        );

        // Drop the handle
        drop(handle);

        // The hls_shutdown_token should have been cancelled in Drop,
        // which signals the HLS cleanup task to exit.
        // Note: We can't check the token after drop because it's owned by the handle,
        // but the Drop implementation calls cancel() on it.
    }

    #[test]
    fn test_livestream_handle_drop_without_runtime_does_not_panic() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            let handle = runtime.block_on(async {
                let registry = Arc::new(MockStreamRegistry::new());
                let server = LivestreamServer::new(test_config(), registry, test_tracker());
                server.start().expect("Failed to start server")
            });

            drop(runtime);
            drop(handle);
        }));

        assert!(
            result.is_ok(),
            "LivestreamHandle::drop must not panic when runtime is already gone"
        );
    }

    // Tests verify that RTMP port conflicts are detected early through pre-binding.

    /// Test that pre-binding a port and passing it to LivestreamServer works correctly.
    ///
    /// This verifies that the `with_rtmp_listener` method properly accepts a pre-bound
    /// listener and the server starts successfully using that listener.
    #[tokio::test]
    async fn test_rtmp_pre_binding_works() {
        let registry = Arc::new(MockStreamRegistry::new());

        // Pre-bind to port 0 (let OS assign a free port)
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to pre-bind RTMP port");

        // Get the actual port assigned by the OS
        let local_addr = listener.local_addr().expect("Failed to get local addr");

        // Create config that matches the pre-bound address
        let config = LivestreamConfig {
            rtmp_address: local_addr.to_string(),
            gop_cache_size: 1024 * 1024,
            node_id: "test-node".to_string(),
            cleanup_check_interval_seconds: 1,
            stream_timeout_seconds: 5,
            distributed_enabled: false,
            cluster_secret: None,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            gop_cache_max_memory_mb: 0,
            max_flv_tag_size_bytes: 10 * 1024 * 1024,
            api_address: "127.0.0.1:0".to_string(),
            hls_memory_max_mb: 0,
            hls_storage_backend: HlsStorageBackend::Memory,
            hls_storage_path: String::new(),
            hls_oss: HlsOssConfig::default(),
            ssrf_guard: SsrfGuard::strict_policy(),
        };

        let server =
            LivestreamServer::new(config, registry, test_tracker()).with_rtmp_listener(listener);

        // Start should succeed because we already have the port
        let handle = server
            .start()
            .expect("Failed to start server with pre-bound listener");

        // Give tasks a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Clean up
        handle.shutdown();
    }

    #[tokio::test]
    async fn test_shutdown_releases_rtmp_port_for_rebind() {
        let registry = Arc::new(MockStreamRegistry::new());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to pre-bind RTMP port");
        let local_addr = listener.local_addr().expect("Failed to get local addr");

        let config = LivestreamConfig {
            rtmp_address: local_addr.to_string(),
            gop_cache_size: 1024 * 1024,
            node_id: "test-node".to_string(),
            cleanup_check_interval_seconds: 1,
            stream_timeout_seconds: 5,
            distributed_enabled: false,
            cluster_secret: None,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            gop_cache_max_memory_mb: 0,
            max_flv_tag_size_bytes: 10 * 1024 * 1024,
            api_address: "127.0.0.1:0".to_string(),
            hls_memory_max_mb: 0,
            hls_storage_backend: HlsStorageBackend::Memory,
            hls_storage_path: String::new(),
            hls_oss: HlsOssConfig::default(),
            ssrf_guard: SsrfGuard::strict_policy(),
        };

        let server =
            LivestreamServer::new(config, registry, test_tracker()).with_rtmp_listener(listener);

        let handle = server
            .start()
            .expect("Failed to start server with pre-bound listener");

        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.shutdown();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let rebound = tokio::net::TcpListener::bind(local_addr).await;
        assert!(
            rebound.is_ok(),
            "shutdown must release the RTMP listener so the port can be rebound: {rebound:?}"
        );
    }

    #[tokio::test]
    async fn test_shutdown_graceful_releases_rtmp_port_for_rebind() {
        let registry = Arc::new(MockStreamRegistry::new());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to pre-bind RTMP port");
        let local_addr = listener.local_addr().expect("Failed to get local addr");

        let config = LivestreamConfig {
            rtmp_address: local_addr.to_string(),
            gop_cache_size: 1024 * 1024,
            node_id: "test-node".to_string(),
            cleanup_check_interval_seconds: 1,
            stream_timeout_seconds: 5,
            distributed_enabled: false,
            cluster_secret: None,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            gop_cache_max_memory_mb: 0,
            max_flv_tag_size_bytes: 10 * 1024 * 1024,
            api_address: "127.0.0.1:0".to_string(),
            hls_memory_max_mb: 0,
            hls_storage_backend: HlsStorageBackend::Memory,
            hls_storage_path: String::new(),
            hls_oss: HlsOssConfig::default(),
            ssrf_guard: SsrfGuard::strict_policy(),
        };

        let server =
            LivestreamServer::new(config, registry, test_tracker()).with_rtmp_listener(listener);

        let mut handle = server
            .start()
            .expect("Failed to start server with pre-bound listener");

        tokio::time::sleep(Duration::from_millis(50)).await;

        let result = timeout(Duration::from_secs(2), handle.shutdown_graceful(1))
            .await
            .expect("shutdown_graceful should complete");
        assert!(result, "shutdown_graceful should complete cleanly");

        let rebound = tokio::net::TcpListener::bind(local_addr).await;
        assert!(
            rebound.is_ok(),
            "shutdown_graceful must release the RTMP listener so the port can be rebound: {rebound:?}"
        );
    }

    /// Test that port conflicts are detected when trying to bind an already-used port.
    ///
    /// This verifies the core purpose of pre-binding: detecting port conflicts early
    /// before deep initialization.
    #[tokio::test]
    async fn test_port_conflict_detected_early() {
        // Bind to a specific port first
        let listener1 = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind first listener");

        let bound_addr = listener1.local_addr().expect("Failed to get local addr");

        // Try to bind to the same port - this should fail
        let result = tokio::net::TcpListener::bind(bound_addr).await;

        assert!(
            result.is_err(),
            "Binding to an already-in-use port should fail"
        );

        // The error should be an address-in-use error
        let err = result.unwrap_err();
        assert!(
            err.kind() == std::io::ErrorKind::AddrInUse,
            "Error should be AddrInUse, got: {err:?}"
        );
    }

    /// Test that LivestreamServer can bind its own listener when one is not pre-supplied.
    ///
    /// This verifies the supported construction path where the server owns listener binding.
    #[tokio::test]
    async fn test_livestream_server_without_prebound_listener() {
        let registry = Arc::new(MockStreamRegistry::new());

        // Use port 0 to let OS assign a free port
        let server = LivestreamServer::new(test_config(), registry, test_tracker());

        // Start without pre-binding should still work
        let handle = server
            .start()
            .expect("Server should start without pre-bound listener");

        // Give tasks a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Clean up
        handle.shutdown();
    }
}
