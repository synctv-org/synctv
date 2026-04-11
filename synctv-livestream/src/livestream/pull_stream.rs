// Pull stream instance — single gRPC relay stream with lifecycle management
//
// Pulls RTMP data from a publisher node via gRPC and publishes it into
// the local StreamHub. GOP cache is handled by StreamHub internally.

use crate::{
    error::StreamResult,
    grpc::{GrpcConnectionPool, GrpcStreamPuller, HlsProxyClient},
    livestream::managed_stream::{ManagedStream, StreamLifecycle},
    relay::registry_trait::StreamRegistryTrait,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use synctv_xiu::streamhub::define::{StreamHubEvent, StreamHubEventSender};
use synctv_xiu::streamhub::stream::StreamIdentifier;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Pull stream instance (pulls RTMP from publisher via gRPC, serves FLV to local clients)
///
/// GOP cache is handled by xiu's `StreamHub` — when the gRPC puller publishes
/// frames to the local `StreamHub`, and a new subscriber joins, `StreamHub`
/// automatically sends cached GOP frames via `send_prior_data`.
pub struct PullStream {
    pub(crate) room_id: String,
    pub(crate) media_id: String,
    pub(crate) publisher_node: String,
    registry: Arc<dyn StreamRegistryTrait>,
    stream_hub_event_sender: StreamHubEventSender,
    lifecycle: StreamLifecycle,
    /// Fencing token (epoch) from when the stream was created.
    /// Used to detect split-brain when publisher changes during network partition.
    epoch: u64,
    /// Cancellation token for graceful shutdown propagation.
    cancel_token: CancellationToken,
    /// Flag to prevent double `UnPublish`: set to `true` after `stop()` sends `UnPublish`.
    /// The `Drop` implementation checks this to skip its own `UnPublish`.
    stopped: AtomicBool,
    /// Shared gRPC connection pool for reusing HTTP/2 channels to publisher nodes.
    connection_pool: GrpcConnectionPool,
    /// Cluster authentication secret passed to `GrpcStreamPuller` for inter-node gRPC requests.
    cluster_secret: Option<String>,
    /// Optional HLS proxy client for cache invalidation on stale epoch detection.
    hls_proxy: Option<HlsProxyClient>,
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
        _local_node_id: String,
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        epoch: u64,
    ) -> Self {
        Self::with_pool(
            room_id,
            media_id,
            publisher_node,
            registry,
            stream_hub_event_sender,
            epoch,
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
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        epoch: u64,
        connection_pool: GrpcConnectionPool,
    ) -> Self {
        Self {
            room_id,
            media_id,
            publisher_node,
            registry,
            stream_hub_event_sender,
            lifecycle: StreamLifecycle::new(),
            epoch,
            cancel_token: CancellationToken::new(),
            stopped: AtomicBool::new(false),
            connection_pool,
            cluster_secret: None,
            hls_proxy: None,
        }
    }

    /// Set the cluster authentication secret for inter-node gRPC requests.
    #[must_use]
    pub fn with_cluster_secret(mut self, secret: Option<String>) -> Self {
        self.cluster_secret = secret;
        self
    }

    /// Set the HLS proxy client for cache invalidation on stale epoch detection.
    #[must_use]
    pub fn with_hls_proxy(mut self, hls_proxy: Option<HlsProxyClient>) -> Self {
        self.hls_proxy = hls_proxy;
        self
    }

    /// Start the pull stream - connects to publisher via gRPC
    pub async fn start(&self) -> StreamResult<()> {
        // Validate epoch before starting to detect split-brain
        match self
            .registry
            .validate_epoch(&self.room_id, &self.media_id, self.epoch)
            .await
        {
            Ok(true) => {
                debug!(
                    "Epoch {} validated for pull stream {}/{}",
                    self.epoch, self.room_id, self.media_id
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
                // Issue #53: Fail-CLOSED on Redis error to prevent split-brain during
                // network partitions.  If we cannot validate the epoch, we cannot
                // confirm that our publisher record is still valid.  Optimistic
                // continuation ("fail-open") risks streaming stale data from the wrong
                // publisher node during a network partition scenario.
                //
                // The caller (ExternalPublishManager / PullStreamManager) treats this
                // as a failed start and will retry on the next viewer request.
                error!(
                    "Failed to validate epoch for pull stream {}/{}: {}. \
                     Failing closed to prevent potential split-brain. \
                     Stream will retry when Redis is available.",
                    self.room_id, self.media_id, e
                );
                return Err(crate::error::StreamError::RegistryError(format!(
                    "Epoch validation failed for {}/{}: {e}",
                    self.room_id, self.media_id
                )));
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
        let epoch = self.epoch;
        let registry = Arc::clone(&self.registry);
        let hls_proxy = self.hls_proxy.clone();
        // Clone the is_running flag to mark failure in the spawned task
        let is_running_flag = self.lifecycle.is_running_clone();

        let child_token = self.cancel_token.child_token();
        let handle = tokio::spawn(async move {
            info!("gRPC puller task started for {} / {}", room_id, media_id);
            let max_rebuilds: u32 = 3;
            let rebuild_delay = std::time::Duration::from_secs(5);
            let epoch_revalidation_interval = std::time::Duration::from_secs(30);
            // L-04: After this many Redis failures, terminate to avoid streaming with stale data.
            let max_consecutive_epoch_failures: u32 = 3;

            let mut rebuild_count: u32 = 0;
            let mut consecutive_epoch_failures: u32 = 0;
            let result = loop {
                let grpc_puller = GrpcStreamPuller::new(
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

                // Race the puller against cancellation and periodic epoch re-validation
                let mut epoch_interval = tokio::time::interval(epoch_revalidation_interval);
                // Skip the first immediate tick
                epoch_interval.tick().await;

                let run_result = tokio::select! {
                    r = grpc_puller.run() => r,
                    () = child_token.cancelled() => {
                        info!("gRPC puller task cancelled for {} / {}", room_id, media_id);
                        timer.observe_duration();
                        synctv_core::metrics::stream::ACTIVE_RELAY_STREAMS.dec();
                        break Ok(());
                    }
                    () = async {
                        loop {
                            epoch_interval.tick().await;
                            match registry.validate_epoch(&room_id, &media_id, epoch).await {
                                Ok(true) => {
                                    // L-04: Reset failure counter on success
                                    consecutive_epoch_failures = 0;
                                    debug!(
                                        "Periodic epoch {} still valid for {}/{}",
                                        epoch, room_id, media_id
                                    );
                                }
                                Ok(false) => {
                                    warn!(
                                        "Periodic epoch re-validation: epoch {} is stale for {}/{}, publisher changed",
                                        epoch, room_id, media_id
                                    );
                                    return;
                                }
                                Err(e) => {
                                    // L-04: Track consecutive failures instead of unconditional fail-open
                                    consecutive_epoch_failures += 1;
                                    if consecutive_epoch_failures >= max_consecutive_epoch_failures {
                                        error!(
                                            "Epoch validation failed {} consecutive times for {}/{}: {}. \
                                             Terminating pull stream (publisher may be stale). \
                                             Stream will reconnect when Redis is available.",
                                            consecutive_epoch_failures, room_id, media_id, e
                                        );
                                        return;
                                    }
                                    warn!(
                                        "Periodic epoch re-validation failed for {}/{}: {} ({}/{} consecutive failures). Continuing.",
                                        room_id, media_id, e, consecutive_epoch_failures, max_consecutive_epoch_failures
                                    );
                                }
                            }
                        }
                    } => {
                        // Epoch became stale during streaming — invalidate HLS cache
                        // so viewers don't see stale segments for up to 90s TTL.
                        // Use delayed invalidation to give the new publisher time to
                        // generate fresh HLS segments before we clear the old cache.
                        if let Some(ref hls_proxy) = hls_proxy {
                            hls_proxy.invalidate_stream_cache_delayed(
                                room_id.clone(),
                                media_id.clone(),
                                std::time::Duration::from_secs(3),
                            );
                        }
                        timer.observe_duration();
                        synctv_core::metrics::stream::ACTIVE_RELAY_STREAMS.dec();
                        break Err(anyhow::anyhow!(
                            "Stale epoch detected during streaming: publisher changed for {room_id} / {media_id}"
                        ));
                    }
                };

                timer.observe_duration();
                synctv_core::metrics::stream::ACTIVE_RELAY_STREAMS.dec();

                match run_result {
                    Ok(()) => break Ok(()),
                    Err(e) => {
                        let err_str = e.to_string();
                        let error_type = if err_str.contains("timeout") {
                            "timeout"
                        } else if err_str.contains("connection") {
                            "connection"
                        } else {
                            "other"
                        };
                        synctv_core::metrics::stream::STREAM_ERRORS
                            .with_label_values(&["rtmp", error_type])
                            .inc();

                        rebuild_count += 1;
                        if rebuild_count > max_rebuilds {
                            error!(
                                "gRPC puller exhausted all retries and {} rebuilds for {} / {}: {}",
                                max_rebuilds, room_id, media_id, e
                            );
                            break Err(e);
                        }

                        warn!(
                            "gRPC puller exited for {} / {}, rebuilding ({}/{}): {}",
                            room_id, media_id, rebuild_count, max_rebuilds, e
                        );

                        // Wait before rebuilding, but respect cancellation
                        tokio::select! {
                            () = tokio::time::sleep(rebuild_delay) => {}
                            () = child_token.cancelled() => {
                                info!("gRPC puller rebuild cancelled for {} / {}", room_id, media_id);
                                break Ok(());
                            }
                        }

                        // LS-7: Re-validate epoch before reconnecting to detect
                        // split-brain scenarios where the publisher has changed
                        // during the network disruption.
                        match registry.validate_epoch(&room_id, &media_id, epoch).await {
                            Ok(true) => {
                                // L-04: Reset failure counter on success
                                consecutive_epoch_failures = 0;
                                debug!(
                                    "Epoch {} still valid on reconnect for {}/{}",
                                    epoch, room_id, media_id
                                );
                            }
                            Ok(false) => {
                                warn!(
                                    "Epoch {} is stale on reconnect for {}/{}, publisher changed. Stopping pull stream.",
                                    epoch, room_id, media_id
                                );
                                if let Some(ref hls_proxy) = hls_proxy {
                                    hls_proxy
                                        .invalidate_stream_cache_sync(&room_id, &media_id)
                                        .await;
                                }
                                break Err(anyhow::anyhow!(
                                    "Stale epoch on reconnect: publisher changed for {room_id} / {media_id}"
                                ));
                            }
                            Err(e) => {
                                // L-04: Track consecutive failures instead of unconditional fail-open
                                consecutive_epoch_failures += 1;
                                if consecutive_epoch_failures >= max_consecutive_epoch_failures {
                                    error!(
                                        "Epoch validation on reconnect failed {} consecutive times for {}/{}: {}. \
                                         Terminating pull stream (publisher may be stale). \
                                         Stream will reconnect when Redis is available.",
                                        consecutive_epoch_failures, room_id, media_id, e
                                    );
                                    break Err(anyhow::anyhow!(
                                        "Epoch validation unreachable after {consecutive_epoch_failures} consecutive failures for {room_id} / {media_id}"
                                    ));
                                }
                                warn!(
                                    "Failed to validate epoch on reconnect for {}/{}: {} ({}/{} consecutive failures). Continuing.",
                                    room_id, media_id, e, consecutive_epoch_failures, max_consecutive_epoch_failures
                                );
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

        info!(
            "Pull stream started for room {} / media {}",
            self.room_id, self.media_id
        );
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
        let room_id = self.room_id.clone();
        let media_id = self.media_id.clone();
        match self
            .stream_hub_event_sender
            .try_send(StreamHubEvent::UnPublish { identifier })
        {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                let sender = self.stream_hub_event_sender.clone();
                warn!(
                    "PullStream stop: channel full, spawning async UnPublish for {}/{}",
                    room_id, media_id
                );
                if crate::util::try_spawn(async move {
                    if let Err(e) = sender.send(event).await {
                        warn!(
                            "PullStream stop: async UnPublish failed for {}/{}: {}",
                            room_id, media_id, e
                        );
                    }
                })
                .is_none()
                {
                    warn!(
                        "PullStream stop: no Tokio runtime available, skipping async UnPublish for {}/{}",
                        self.room_id, self.media_id
                    );
                }
            }
            Err(e) => {
                warn!(
                    "Failed to send UnPublish to StreamHub for {} / {}: {}",
                    self.room_id, self.media_id, e
                );
            }
        }

        self.lifecycle.abort_task().await;
        info!(
            "Pull stream stopped for room {} / media {}",
            self.room_id, self.media_id
        );
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
                let room_id_for_task = room_id.clone();
                let media_id_for_task = media_id.clone();
                warn!(
                    "PullStream drop: channel full, spawning async UnPublish for {}/{}",
                    room_id, media_id
                );
                if crate::util::try_spawn(async move {
                    if let Err(e) = sender.send(event).await {
                        warn!(
                            "PullStream drop: async UnPublish failed for {}/{}: {}",
                            room_id_for_task, media_id_for_task, e
                        );
                    }
                })
                .is_none()
                {
                    warn!(
                        "PullStream drop: no Tokio runtime available, skipping async UnPublish for {}/{}",
                        room_id, media_id
                    );
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::MockStreamRegistry;

    #[tokio::test]
    async fn test_stop_retries_unpublish_when_channel_full() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        sender
            .try_send(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "existing-room".to_string(),
                    stream_name: "existing-media".to_string(),
                },
            })
            .expect("fill channel");

        let stream = PullStream::new(
            "room-1".to_string(),
            "media-1".to_string(),
            "127.0.0.1:50051".to_string(),
            "node-1".to_string(),
            Arc::new(MockStreamRegistry::new()) as Arc<dyn crate::relay::StreamRegistryTrait>,
            sender,
            1,
        );

        stream.stop().await.expect("stop should succeed");

        let first = receiver.recv().await.expect("placeholder event");
        match first {
            StreamHubEvent::UnPublish { identifier } => match identifier {
                StreamIdentifier::Rtmp {
                    app_name,
                    stream_name,
                } => {
                    assert_eq!(app_name, "existing-room");
                    assert_eq!(stream_name, "existing-media");
                }
                other => panic!("unexpected identifier: {other:?}"),
            },
            _ => panic!("unexpected event variant"),
        }

        let retried = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .expect("async retry should enqueue unpublish")
            .expect("retried event should be present");
        match retried {
            StreamHubEvent::UnPublish { identifier } => match identifier {
                StreamIdentifier::Rtmp {
                    app_name,
                    stream_name,
                } => {
                    assert_eq!(app_name, "room-1");
                    assert_eq!(stream_name, "media-1");
                }
                other => panic!("unexpected identifier: {other:?}"),
            },
            _ => panic!("unexpected event variant"),
        }
    }
}
