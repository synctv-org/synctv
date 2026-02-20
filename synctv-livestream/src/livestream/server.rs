// Livestream server facade
//
// Single entry point for starting the entire livestream infrastructure:
// StreamHub, RTMP server, HLS remuxer, PullStreamManager,
// ExternalPublishManager, PublisherManager, and LiveStreamingInfrastructure.
//
// The synctv binary never touches synctv_xiu directly -- all xiu interaction
// is encapsulated here.

use crate::{
    relay::{registry_trait::StreamRegistryTrait, PublisherManager},
    livestream::{
        pull_manager::PullStreamManager,
        external_publish_manager::ExternalPublishManager,
        segment_manager::{SegmentManager, CleanupConfig},
    },
    protocols::hls::{CustomHlsRemuxer, StreamRegistry},
    api::{LiveStreamingInfrastructure, UserStreamTracker},
    error::StreamResult,
};
use synctv_xiu::rtmp::auth::AuthCallback;
use synctv_xiu::storage::MemoryStorage;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use synctv_xiu::streamhub::StreamsHub;

pub struct LivestreamConfig {
    pub rtmp_address: String,
    pub gop_cache_size: usize,
    pub node_id: String,
    pub cleanup_check_interval_seconds: u64,
    pub stream_timeout_seconds: u64,
    /// Cluster secret for authenticating gRPC HLS proxy calls
    pub cluster_secret: Option<String>,
    /// Maximum memory (in megabytes) for the GOP cache per stream.
    /// 0 means use the built-in default (50 MB).
    pub gop_cache_max_memory_mb: u64,
    /// Advertised gRPC address of this node for cross-node proxying.
    /// Used by `PublisherManager` for re-registration after `StreamHub` restart.
    pub grpc_address: String,
    /// Maximum memory (in megabytes) for in-memory HLS segment storage.
    /// 0 means use the built-in default (512 MB).
    pub hls_memory_max_mb: u64,
}

/// Handle returned by [`LivestreamServer::start`].
///
/// Owns the spawned tasks (`StreamHub` event loop, RTMP server, HLS remuxer,
/// `PublisherManager`) and exposes the shared infrastructure components.
pub struct LivestreamHandle {
    pub infrastructure: Arc<LiveStreamingInfrastructure>,
    pub pull_manager: Arc<PullStreamManager>,
    hub_handle: JoinHandle<()>,
    hls_remuxer_handle: JoinHandle<()>,
    publisher_manager_handle: JoinHandle<()>,
    pull_manager_cleanup: JoinHandle<()>,
    external_publish_cleanup: JoinHandle<()>,
    hls_shutdown_token: CancellationToken,
}

impl LivestreamHandle {
    /// Abort all spawned tasks in reverse startup order.
    ///
    /// This is a fast shutdown that immediately aborts all tasks.
    /// For graceful shutdown that waits for tasks to complete, use `shutdown_graceful`.
    pub fn shutdown(&self) {
        self.external_publish_cleanup.abort();
        self.pull_manager_cleanup.abort();
        self.publisher_manager_handle.abort();
        self.hls_shutdown_token.cancel();
        self.hls_remuxer_handle.abort();
        self.hub_handle.abort();
    }

    /// Gracefully shutdown all spawned tasks.
    ///
    /// This method waits for each task to complete (with timeout) before
    /// proceeding to the next. This ensures proper cleanup of resources.
    ///
    /// # Arguments
    /// * `timeout_secs` - Maximum seconds to wait for each task to complete.
    ///
    /// # Returns
    /// `true` if all tasks shut down gracefully, `false` if any task was aborted due to timeout.
    pub async fn shutdown_graceful(&mut self, timeout_secs: u64) -> bool {
        use tokio::time::{timeout, Duration};
        let timeout_duration = Duration::from_secs(timeout_secs);
        let mut all_graceful = true;

        // Shutdown in reverse startup order
        info!("Starting graceful shutdown of livestream components...");

        // 1. Stop external publish cleanup
        self.external_publish_cleanup.abort();
        if timeout(timeout_duration, &mut self.external_publish_cleanup).await.is_ok() { info!("External publish cleanup stopped") } else {
            warn!("External publish cleanup shutdown timed out");
            all_graceful = false;
        }

        // 2. Stop pull manager cleanup
        self.pull_manager_cleanup.abort();
        if timeout(timeout_duration, &mut self.pull_manager_cleanup).await.is_ok() { info!("Pull manager cleanup stopped") } else {
            warn!("Pull manager cleanup shutdown timed out");
            all_graceful = false;
        }

        // 3. Stop publisher manager
        self.publisher_manager_handle.abort();
        if timeout(timeout_duration, &mut self.publisher_manager_handle).await.is_ok() { info!("Publisher manager stopped") } else {
            warn!("Publisher manager shutdown timed out");
            all_graceful = false;
        }

        // 4. Stop HLS remuxer (cancel token triggers graceful drain)
        self.hls_shutdown_token.cancel();
        if timeout(timeout_duration, &mut self.hls_remuxer_handle).await.is_ok() { info!("HLS remuxer stopped") } else {
            warn!("HLS remuxer shutdown timed out");
            self.hls_remuxer_handle.abort();
            all_graceful = false;
        }

        // 5. Stop StreamHub (last, as other components depend on it)
        // The RTMP server is now managed inside the hub loop and will be
        // cancelled automatically when the hub task is aborted.
        self.hub_handle.abort();
        if timeout(timeout_duration, &mut self.hub_handle).await.is_ok() { info!("StreamHub stopped") } else {
            warn!("StreamHub shutdown timed out");
            all_graceful = false;
        }

        if all_graceful {
            info!("Graceful shutdown completed successfully");
        } else {
            warn!("Graceful shutdown completed with some timeouts");
        }

        all_graceful
    }
}

pub struct LivestreamServer {
    config: LivestreamConfig,
    publisher_registry: Arc<dyn StreamRegistryTrait>,
    user_stream_tracker: UserStreamTracker,
    auth: Option<Arc<dyn AuthCallback>>,
}

impl LivestreamServer {
    pub fn new(
        config: LivestreamConfig,
        publisher_registry: Arc<dyn StreamRegistryTrait>,
        user_stream_tracker: UserStreamTracker,
    ) -> Self {
        Self {
            config,
            publisher_registry,
            user_stream_tracker,
            auth: None,
        }
    }

    /// Set RTMP auth callback
    #[must_use]
    pub fn with_auth(mut self, auth: Arc<dyn AuthCallback>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Start the entire livestream infrastructure.
    ///
    /// Creates `StreamHub`, RTMP server, HLS remuxer, `PullStreamManager`,
    /// `ExternalPublishManager`, `PublisherManager`, and `LiveStreamingInfrastructure`.
    /// Returns a handle with public components.
    pub async fn start(self) -> StreamResult<LivestreamHandle> {
        // 1. Create StreamHub channels and hub (bounded to prevent OOM under load)
        let (event_sender, event_receiver) =
            mpsc::channel(synctv_xiu::streamhub::define::STREAM_HUB_EVENT_CHANNEL_CAPACITY);
        let mut streams_hub = StreamsHub::new(
            event_sender.clone(),
            event_receiver,
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
        // Notify to signal PublisherManager to re-register after StreamHub restart.
        // Uses Notify instead of mpsc channel so signals are never lost even if
        // multiple restarts occur before the listener wakes up.
        let reregister_notify = Arc::new(tokio::sync::Notify::new());
        // Channel to notify pull/external managers to stop all streams before StreamHub restart.
        // This ensures zombie streams (still connected to the old hub) are cleaned up.
        let (stop_streams_tx, mut stop_streams_rx) = mpsc::channel::<()>(4);

        // Compute per-stream GOP cache memory limit from config (0 means use default).
        let per_stream_max_bytes: Option<usize> = if self.config.gop_cache_max_memory_mb > 0 {
            let max_bytes = self.config.gop_cache_max_memory_mb as usize * 1024 * 1024;
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

        // 2. Spawn StreamHub event loop with automatic recovery
        let hub_handle = tokio::spawn(async move {
            const MAX_RESTARTS: u32 = 10;
            const INITIAL_BACKOFF_SECS: u64 = 1;
            const MAX_BACKOFF_SECS: u64 = 30;

            let mut restart_count: u32 = 0;

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
                                continue;
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
                .with_cancellation_token(rtmp_session_token.clone());
                let rtmp_handle = tokio::spawn(async move {
                    if let Err(e) = rtmp_server.run().await {
                        error!("RTMP server error: {}", e);
                    }
                });

                info!("Starting StreamHub event loop...");
                let run_result = streams_hub.run().await;

                // Hub exited -- stop the RTMP server and forwarder for this cycle
                rtmp_session_token.cancel();
                rtmp_handle.abort();
                forwarder_handle.abort();

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
                    max_restarts = MAX_RESTARTS,
                    reason,
                    "StreamHub event loop exited unexpectedly, cleaning up local state before restart..."
                );

                info!("Cancelled all active RTMP sessions due to StreamHub restart");

                // Stop all managed pull/external-publish streams BEFORE restart.
                // These streams hold channels to the old StreamHub instance and would
                // become zombies (still running but unable to deliver frames) if not
                // cleaned up. The receiver task calls stop_all() on both managers.
                let _ = stop_streams_tx.try_send(());

                // Clean up all local publisher registrations from Redis
                // This ensures stale state doesn't persist after restart
                if let Err(e) = registry_for_cleanup.cleanup_all_publishers_for_node(&node_id_for_cleanup).await {
                    error!("Failed to cleanup publishers on StreamHub restart: {}", e);
                }

                // Notify PublisherManager to re-register all active publishers immediately
                reregister_notify_for_hub.notify_one();

                if restart_count >= MAX_RESTARTS {
                    error!(
                        "StreamHub has restarted {} times, giving up to avoid infinite restart loop",
                        restart_count
                    );
                    break;
                }

                // Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s, 30s, ...
                let backoff_secs = INITIAL_BACKOFF_SECS
                    .saturating_mul(1u64 << (restart_count - 1).min(16))
                    .min(MAX_BACKOFF_SECS);
                info!("Waiting {} seconds before restarting StreamHub...", backoff_secs);
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
            }
        });

        // 3. Start HLS remuxer (converts RTMP to HLS segments)
        let hls_storage = if self.config.hls_memory_max_mb > 0 {
            let max_bytes = self.config.hls_memory_max_mb as usize * 1024 * 1024;
            info!(
                "HLS memory storage max set to {} MB",
                self.config.hls_memory_max_mb,
            );
            Arc::new(MemoryStorage::with_limits(max_bytes, 0)) as Arc<dyn synctv_xiu::storage::HlsStorage>
        } else {
            Arc::new(MemoryStorage::new()) as Arc<dyn synctv_xiu::storage::HlsStorage>
        };

        // Warn if using in-memory HLS storage in cluster mode (cluster_secret is set).
        // In cluster mode, each HLS segment request for a remote publisher requires a
        // gRPC proxy call to the publisher node, which is inefficient at scale.
        if self.config.cluster_secret.is_some() {
            warn!(
                "HLS storage is using in-memory backend in cluster mode. \
                 Each segment request will require gRPC proxy to the publisher node. \
                 Consider using OSS or shared filesystem storage for better performance."
            );
        }

        let segment_manager = Arc::new(SegmentManager::new(hls_storage, CleanupConfig::default()));
        let stream_registry: StreamRegistry = Arc::new(DashMap::new());
        let hls_shutdown_token = CancellationToken::new();

        // Start segment cleanup background task
        segment_manager.clone().start_cleanup_task(hls_shutdown_token.clone());

        // Create PublisherManager early so the activity callback can be wired to the HLS remuxer.
        // The manager itself is started later (step 7) after all components are created.
        let publisher_manager = Arc::new(PublisherManager::new(
            self.publisher_registry.clone(),
            self.config.node_id.clone(),
            event_sender.clone(),
        ).with_grpc_address(self.config.grpc_address.clone()));

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

                let error_type = if e.to_string().contains("timeout") {
                    "timeout"
                } else if e.to_string().contains("connection") {
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
        let hls_proxy = crate::grpc::HlsProxyClient::with_defaults(self.config.cluster_secret.clone())
            .with_connection_pool(shared_grpc_pool.clone());

        // 5c. Create PullStreamManager with the same shared pool
        let pull_manager = Arc::new(PullStreamManager::with_timeouts(
            self.publisher_registry.clone(),
            event_sender.clone(),
            self.config.cleanup_check_interval_seconds,
            self.config.stream_timeout_seconds,
        )
        .with_connection_pool(shared_grpc_pool)
        .with_cluster_secret(self.config.cluster_secret.clone())
        .with_hls_proxy(hls_proxy.clone()));
        // Start periodic cleanup of stale creation locks to prevent memory leaks
        let pull_manager_cleanup = pull_manager.start_cleanup_task();

        // 6. Create ExternalPublishManager
        let external_publish_manager = Arc::new(ExternalPublishManager::with_timeouts(
            self.publisher_registry.clone(),
            self.config.node_id.clone(),
            self.config.grpc_address.clone(),
            event_sender.clone(),
            self.config.cleanup_check_interval_seconds,
            self.config.stream_timeout_seconds,
        ));
        // Start periodic cleanup of stale creation locks to prevent memory leaks
        let external_publish_cleanup = external_publish_manager.start_cleanup_task();

        // 6b. Spawn listener that stops all managed streams on StreamHub restart.
        // This ensures zombie streams (connected to the old hub) are cleaned up
        // before the new hub starts accepting events.
        {
            let pm = Arc::clone(&pull_manager);
            let epm = Arc::clone(&external_publish_manager);
            tokio::spawn(async move {
                while stop_streams_rx.recv().await.is_some() {
                    info!("StreamHub restart: stopping all managed pull streams...");
                    pm.stop_all().await;
                    info!("StreamHub restart: stopping all managed external publish streams...");
                    epm.stop_all().await;
                    info!("StreamHub restart: all managed streams stopped");
                }
            });
        }

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
        let publisher_manager_handle = tokio::spawn({
            let pm = Arc::clone(&publisher_manager);
            let reregister = Arc::clone(&reregister_notify);
            async move {
                // Spawn a task to listen for re-registration signals from StreamHub restart
                let pm_for_reregister = Arc::clone(&pm);
                tokio::spawn(async move {
                    loop {
                        reregister.notified().await;
                        pm_for_reregister.reregister_all_publishers().await;
                    }
                });
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
            .with_hls_proxy(hls_proxy)
        );

        info!(
            "Livestream infrastructure initialized, RTMP server listening on rtmp://{}",
            self.config.rtmp_address,
        );

        Ok(LivestreamHandle {
            infrastructure,
            pull_manager,
            hub_handle,
            hls_remuxer_handle,
            publisher_manager_handle,
            pull_manager_cleanup,
            external_publish_cleanup,
            hls_shutdown_token,
        })
    }
}
