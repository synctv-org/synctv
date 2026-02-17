// Pull stream instance — single gRPC relay stream with lifecycle management
//
// Pulls RTMP data from a publisher node via gRPC and publishes it into
// the local StreamHub. GOP cache is handled by StreamHub internally.

use crate::{
    relay::registry_trait::StreamRegistryTrait,
    error::StreamResult,
    grpc::{GrpcConnectionPool, GrpcStreamPuller},
    livestream::managed_stream::{ManagedStream, StreamLifecycle},
};
use synctv_xiu::streamhub::define::{StreamHubEvent, StreamHubEventSender};
use synctv_xiu::streamhub::stream::StreamIdentifier;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Pull stream instance (pulls RTMP from publisher via gRPC, serves FLV to local clients)
///
/// GOP cache is handled by xiu's `StreamHub` — when the gRPC puller publishes
/// frames to the local `StreamHub`, and a new subscriber joins, `StreamHub`
/// automatically sends cached GOP frames via `send_prior_data`.
pub struct PullStream {
    pub(crate) room_id: String,
    pub(crate) media_id: String,
    pub(crate) publisher_node: String,
    local_node_id: String,
    registry: Arc<dyn StreamRegistryTrait>,
    stream_hub_event_sender: StreamHubEventSender,
    lifecycle: StreamLifecycle,
    /// Fencing token (epoch) from when the stream was created.
    /// Used to detect split-brain when publisher changes during network partition.
    epoch: u64,
    /// Cancellation token for graceful shutdown propagation.
    cancel_token: CancellationToken,
    /// Flag to prevent double UnPublish: set to `true` after `stop()` sends UnPublish.
    /// The `Drop` implementation checks this to skip its own UnPublish.
    stopped: AtomicBool,
    /// Shared gRPC connection pool for reusing HTTP/2 channels to publisher nodes.
    connection_pool: GrpcConnectionPool,
    /// Cluster authentication secret passed to `GrpcStreamPuller` for inter-node gRPC requests.
    cluster_secret: Option<String>,
}

impl ManagedStream for PullStream {
    fn lifecycle(&self) -> &StreamLifecycle {
        &self.lifecycle
    }

    fn stream_key(&self) -> String {
        format!("{}:{}", self.room_id, self.media_id)
    }
}

impl PullStream {
    pub fn new(
        room_id: String,
        media_id: String,
        publisher_node: String,
        local_node_id: String,
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        epoch: u64,
    ) -> Self {
        Self::with_pool(
            room_id, media_id, publisher_node, local_node_id,
            registry, stream_hub_event_sender, epoch,
            GrpcConnectionPool::with_defaults(),
        )
    }

    /// Create a new `PullStream` with a shared gRPC connection pool.
    ///
    /// Preferred over `new()` when a pool is available (from `PullStreamManager`).
    pub fn with_pool(
        room_id: String,
        media_id: String,
        publisher_node: String,
        local_node_id: String,
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        epoch: u64,
        connection_pool: GrpcConnectionPool,
    ) -> Self {
        Self {
            room_id,
            media_id,
            publisher_node,
            local_node_id,
            registry,
            stream_hub_event_sender,
            lifecycle: StreamLifecycle::new(),
            epoch,
            cancel_token: CancellationToken::new(),
            stopped: AtomicBool::new(false),
            connection_pool,
            cluster_secret: None,
        }
    }

    /// Set the cluster authentication secret for inter-node gRPC requests.
    #[must_use]
    pub fn with_cluster_secret(mut self, secret: Option<String>) -> Self {
        self.cluster_secret = secret;
        self
    }

    /// Start the pull stream - connects to publisher via gRPC
    pub async fn start(&self) -> StreamResult<()> {
        // Validate epoch before starting to detect split-brain
        match self.registry.validate_epoch(&self.room_id, &self.media_id, self.epoch).await {
            Ok(true) => {
                debug!(
                    "Epoch {} validated for pull stream {}/{}",
                    self.epoch,
                    self.room_id,
                    self.media_id
                );
            }
            Ok(false) => {
                warn!(
                    "Epoch {} is stale for pull stream {}/{}, publisher may have changed. Stopping.",
                    self.epoch,
                    self.room_id,
                    self.media_id
                );
                return Err(crate::error::StreamError::StaleEpoch(format!(
                    "{} / {}",
                    self.room_id, self.media_id
                )));
            }
            Err(e) => {
                warn!(
                    "Failed to validate epoch for pull stream {}/{}: {}. Continuing optimistically.",
                    self.room_id,
                    self.media_id,
                    e
                );
                // Continue on error - fail open to avoid blocking streams during Redis issues
            }
        }

        self.lifecycle.set_running();
        self.lifecycle.update_last_active_time();

        let room_id = self.room_id.clone();
        let media_id = self.media_id.clone();
        let publisher_node = self.publisher_node.clone();
        let hub_sender = self.stream_hub_event_sender.clone();
        let pool = self.connection_pool.clone();
        let cluster_secret = self.cluster_secret.clone();
        // Clone the is_running flag to mark failure in the spawned task
        let is_running_flag = self.lifecycle.is_running_clone();

        let child_token = self.cancel_token.child_token();
        let handle = tokio::spawn(async move {
            info!("gRPC puller task started for {} / {}", room_id, media_id);

            /// Maximum number of puller rebuilds before giving up permanently.
            const MAX_REBUILDS: u32 = 3;
            /// Delay before rebuilding a puller after it exits with an error.
            const REBUILD_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

            let mut rebuild_count: u32 = 0;
            let result = loop {
                let grpc_puller = GrpcStreamPuller::with_pool(
                    room_id.clone(),
                    media_id.clone(),
                    publisher_node.clone(),
                    hub_sender.clone(),
                    pool.clone(),
                )
                .with_cluster_secret(cluster_secret.clone());

                // Track relay duration via histogram (stream_type = "rtmp" for gRPC RTMP relay)
                let timer = synctv_core::metrics::stream::STREAM_RELAY_DURATION
                    .with_label_values(&["rtmp"])
                    .start_timer();
                synctv_core::metrics::stream::ACTIVE_RELAY_STREAMS.inc();

                // Race the puller against cancellation for graceful shutdown
                let run_result = tokio::select! {
                    r = grpc_puller.run() => r,
                    _ = child_token.cancelled() => {
                        info!("gRPC puller task cancelled for {} / {}", room_id, media_id);
                        timer.observe_duration();
                        synctv_core::metrics::stream::ACTIVE_RELAY_STREAMS.dec();
                        break Ok(());
                    }
                };

                timer.observe_duration();
                synctv_core::metrics::stream::ACTIVE_RELAY_STREAMS.dec();

                match run_result {
                    Ok(()) => break Ok(()),
                    Err(e) => {
                        let error_type = if e.to_string().contains("timeout") {
                            "timeout"
                        } else if e.to_string().contains("connection") {
                            "connection"
                        } else {
                            "other"
                        };
                        synctv_core::metrics::stream::STREAM_ERRORS
                            .with_label_values(&["rtmp", error_type])
                            .inc();

                        rebuild_count += 1;
                        if rebuild_count > MAX_REBUILDS {
                            error!(
                                "gRPC puller exhausted all retries and {} rebuilds for {} / {}: {}",
                                MAX_REBUILDS, room_id, media_id, e
                            );
                            break Err(e);
                        }

                        warn!(
                            "gRPC puller exited for {} / {}, rebuilding ({}/{}): {}",
                            room_id, media_id, rebuild_count, MAX_REBUILDS, e
                        );

                        // Wait before rebuilding, but respect cancellation
                        tokio::select! {
                            _ = tokio::time::sleep(REBUILD_DELAY) => {}
                            _ = child_token.cancelled() => {
                                info!("gRPC puller rebuild cancelled for {} / {}", room_id, media_id);
                                break Ok(());
                            }
                        }
                    }
                }
            };

            if result.is_err() {
                // Mark is_running as false so is_healthy() returns false
                // This ensures the stream will be removed from the pool on next access
                is_running_flag.store(false, std::sync::atomic::Ordering::SeqCst);
            }
            result
        });

        self.lifecycle.set_task_handle(handle).await;

        info!("Pull stream started for room {} / media {}", self.room_id, self.media_id);
        Ok(())
    }

    /// Stop the pull stream
    ///
    /// Sends `UnPublish` to the local `StreamHub` BEFORE aborting the puller task,
    /// because the puller's own cleanup path won't run on abort.
    pub async fn stop(&self) -> StreamResult<()> {
        self.lifecycle.mark_stopping();

        // Mark as stopped so Drop does not send a duplicate UnPublish
        self.stopped.store(true, Ordering::SeqCst);

        // Cancel the puller task gracefully first
        self.cancel_token.cancel();

        let identifier = StreamIdentifier::Rtmp {
            app_name: self.room_id.clone(),
            stream_name: self.media_id.clone(),
        };
        if let Err(e) = self.stream_hub_event_sender.try_send(StreamHubEvent::UnPublish { identifier }) {
            warn!("Failed to send UnPublish to StreamHub for {} / {}: {}", self.room_id, self.media_id, e);
        }

        self.lifecycle.abort_task().await;
        info!("Pull stream stopped for room {} / media {}", self.room_id, self.media_id);
        Ok(())
    }

    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.lifecycle.subscriber_count()
    }

    pub fn increment_subscriber_count(&self) {
        self.lifecycle.increment_subscriber_count();
    }

    pub fn decrement_subscriber_count(&self) {
        self.lifecycle.decrement_subscriber_count();
    }

    pub async fn is_healthy(&self) -> bool {
        self.lifecycle.is_healthy().await
    }

    pub fn last_active_elapsed_secs(&self) -> u64 {
        self.lifecycle.last_active_elapsed_secs()
    }

    pub fn update_last_active_time(&self) {
        self.lifecycle.update_last_active_time();
    }

    pub fn mark_stopping(&self) {
        self.lifecycle.mark_stopping();
    }

    pub fn restore_running(&self) {
        self.lifecycle.restore_running();
    }
}

impl Drop for PullStream {
    fn drop(&mut self) {
        // Cancel the puller task gracefully via token
        self.cancel_token.cancel();

        // Skip UnPublish if stop() was already called (prevents double-send)
        if self.stopped.load(Ordering::SeqCst) {
            debug!(
                "PullStream dropped for {}/{} (stop() already called, skipping UnPublish)",
                self.room_id, self.media_id
            );
            return;
        }

        // Send UnPublish to StreamHub so the local stream entry is removed.
        // Use try_send first for the fast path; if the channel is full, spawn
        // an async task that awaits `.send()` so the event is not silently lost.
        let identifier = StreamIdentifier::Rtmp {
            app_name: self.room_id.clone(),
            stream_name: self.media_id.clone(),
        };
        let room_id = self.room_id.clone();
        let media_id = self.media_id.clone();
        match self
            .stream_hub_event_sender
            .try_send(StreamHubEvent::UnPublish { identifier })
        {
            Ok(()) => {
                debug!(
                    "PullStream drop: sent UnPublish for {}/{}",
                    room_id, media_id
                );
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                // Channel full -- spawn a task that awaits capacity so the
                // UnPublish is not silently dropped.
                let sender = self.stream_hub_event_sender.clone();
                warn!(
                    "PullStream drop: channel full, spawning async UnPublish for {}/{}",
                    room_id, media_id
                );
                tokio::spawn(async move {
                    if let Err(e) = sender.send(event).await {
                        warn!(
                            "PullStream drop: async UnPublish failed for {}/{}: {}",
                            room_id, media_id, e
                        );
                    }
                });
            }
            Err(e) => {
                warn!(
                    "PullStream drop: failed to send UnPublish for {}/{}: {}",
                    room_id, media_id, e
                );
            }
        }
        // StreamLifecycle's Drop will abort the task handle
    }
}
