// External Publish Manager
//
// Manages external pull-to-publish streams (RTMP / HTTP-FLV → local StreamHub).
//
// From the system's perspective this is a **publisher**: frames are pushed into
// the local StreamHub and the stream is registered in Redis so other nodes can
// discover and relay it via gRPC.  The lifecycle mirrors PullStreamManager
// (lazy start on first viewer, idle cleanup after 5 min) but the two concerns
// are kept separate because external publish owns Redis registration/cleanup.

use crate::{
    error::StreamResult,
    livestream::external_puller::ExternalStreamPuller,
    livestream::managed_stream::{ManagedStream, StreamLifecycle, StreamPool},
    relay::registry_trait::StreamRegistryTrait,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use synctv_xiu::streamhub::define::{StreamHubEvent, StreamHubEventSender};
use synctv_xiu::streamhub::stream::StreamIdentifier;
use tracing::{debug, error, info, warn};

/// Manages external pull-to-publish streams.
///
/// Each stream is lazily started on the first viewer request.  The manager
/// deduplicates concurrent requests (one puller per `room_id:media_id`),
/// registers the stream as a publisher in Redis, and automatically stops +
/// unregisters after 5 minutes with no subscribers.
pub struct ExternalPublishManager {
    pool: StreamPool<ExternalPublishStream>,
    registry: Arc<dyn StreamRegistryTrait>,
    local_node_id: String,
    stream_hub_event_sender: StreamHubEventSender,
    /// Shared HTTP client for FLV connections. Built once with TLS (rustls) support
    /// and reused across all external publish streams to avoid per-stream TLS setup cost.
    http_client: reqwest::Client,
    /// Handle for the background creation lock cleanup task.
    /// Auto-started in `with_timeouts()` to prevent memory leaks from failed stream creation attempts.
    _cleanup_handle: tokio::task::JoinHandle<()>,
}

impl ExternalPublishManager {
    pub fn new(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> Self {
        Self::with_timeouts(registry, local_node_id, stream_hub_event_sender, 60, 300)
    }

    /// Start the background cleanup task for stale creation locks.
    ///
    /// Should be called once after creating the manager to prevent memory leaks
    /// from failed stream creation attempts.
    #[must_use] 
    pub fn start_cleanup_task(&self) -> tokio::task::JoinHandle<()> {
        self.pool.start_creation_lock_cleanup()
    }

    pub fn with_timeouts(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        stream_hub_event_sender: StreamHubEventSender,
        cleanup_check_interval_secs: u64,
        idle_timeout_secs: u64,
    ) -> Self {
        // Build a shared reqwest::Client with TLS (rustls) support.
        // Reused across all HTTP-FLV pull streams to amortize TLS setup.
        let http_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build shared reqwest::Client");

        let pool = StreamPool::new(
            Duration::from_secs(cleanup_check_interval_secs),
            Duration::from_secs(idle_timeout_secs),
        );
        // Auto-start creation lock cleanup to prevent memory leaks
        let cleanup_handle = pool.start_creation_lock_cleanup();
        Self {
            pool,
            registry,
            local_node_id,
            stream_hub_event_sender,
            http_client,
            _cleanup_handle: cleanup_handle,
        }
    }

    /// Stop all managed external publish streams, aborting their tasks and clearing the pool.
    ///
    /// Called during StreamHub restart to ensure zombie streams (still connected
    /// to the old hub instance) are cleaned up before the new hub starts.
    pub async fn stop_all(&self) {
        self.pool.stop_all().await;
    }

    /// Get or create an external publish stream.
    ///
    /// If a healthy stream already exists for this `(room_id, media_id)` pair,
    /// the subscriber count is incremented and the existing stream is returned.
    /// Otherwise a new `ExternalStreamPuller` is spawned and the stream is
    /// registered as a publisher in Redis.
    ///
    /// ## Subscriber count contract
    ///
    /// Each call increments the subscriber count exactly once, regardless of
    /// which path is taken (fast-path reuse, post-lock reuse, or creation).
    /// The caller MUST call `decrement_subscriber_count()` exactly once when
    /// the viewer disconnects (typically via `StreamSubscriberGuard`).
    ///
    /// - **Fast path / post-lock reuse**: `pool.get_existing()` increments.
    /// - **Creation path**: explicit `increment_subscriber_count()`.
    pub async fn get_or_create(
        &self,
        room_id: &str,
        media_id: &str,
        source_url: &str,
    ) -> StreamResult<Arc<ExternalPublishStream>> {
        let stream_key = format!("{room_id}:{media_id}");

        // Fast path: reuse healthy stream. get_existing() increments subscriber count.
        if let Some(stream) = self.pool.get_existing(&stream_key).await {
            return Ok(stream);
        }

        // Acquire per-key creation lock
        let _guard = self.pool.acquire_creation_lock(&stream_key).await;

        // Re-check after lock. get_existing() increments subscriber count.
        if let Some(stream) = self.pool.get_existing(&stream_key).await {
            debug!(
                "Reusing external publish stream created by concurrent request for {}/{}",
                room_id,
                media_id,
            );
            return Ok(stream);
        }

        info!(
            "Lazy-load: Creating external publish stream for {}/{} from {}",
            room_id,
            media_id,
            source_url
        );

        let stream = Arc::new(ExternalPublishStream::new(
            room_id.to_string(),
            media_id.to_string(),
            source_url.to_string(),
            self.stream_hub_event_sender.clone(),
            self.http_client.clone(),
        ));

        // Register as publisher in Redis FIRST so other nodes can discover this stream.
        // If registration fails, don't start the stream at all to avoid orphaned frames.
        if let Err(e) = self
            .registry
            .try_register_publisher(room_id, media_id, &self.local_node_id, "external_puller", "")
            .await
        {
            error!("Failed to register external publisher in Redis: {e}");
            return Err(crate::error::StreamError::RegistryError(
                format!("Failed to register publisher in Redis: {e}"),
            ));
        }

        // Start the puller (pushes frames into local StreamHub)
        if let Err(e) = stream.start().await {
            error!("Failed to start external stream after registration, unregistering: {e}");
            // Roll back: unregister from Redis since stream didn't start
            if let Err(unreg_err) = self.registry.unregister_publisher(room_id, media_id).await {
                warn!("Failed to unregister after stream start failure: {unreg_err}");
            }
            return Err(e);
        }

        // Creation path: increment subscriber count exactly once for the viewer
        // that triggered creation. (Reuse paths increment inside get_existing().)
        stream.lifecycle().increment_subscriber_count();

        // Spawn idle-cleanup task with Redis unregistration hook
        let registry = Arc::clone(&self.registry);
        let local_node_id = self.local_node_id.clone();
        let hub_sender = self.stream_hub_event_sender.clone();

        self.pool.insert_and_cleanup(
            stream_key,
            Arc::clone(&stream),
            move |stream_key: &str| {
                let registry = Arc::clone(&registry);
                let local_node_id = local_node_id.clone();
                let hub_sender = hub_sender.clone();
                let stream_key = stream_key.to_string();
                Box::pin(async move {
                    // Unregister from Redis FIRST so other nodes stop routing
                    if let Some((room_id, media_id)) = stream_key.split_once(':') {
                        match registry.get_publisher(room_id, media_id).await {
                            Ok(Some(info)) if info.node_id == local_node_id => {
                                if let Err(e) = registry.unregister_publisher(room_id, media_id).await {
                                    warn!("Failed to unregister external publisher from Redis: {e}");
                                }
                            }
                            Ok(Some(_)) => {
                                info!("Skipping Redis unregister for {} — publisher owned by another node", stream_key);
                            }
                            _ => {}
                        }

                        // Send UnPublish to StreamHub
                        let identifier = StreamIdentifier::Rtmp {
                            app_name: room_id.to_string(),
                            stream_name: media_id.to_string(),
                        };
                        if let Err(e) = hub_sender.try_send(StreamHubEvent::UnPublish { identifier }) {
                            warn!("Failed to send UnPublish for {}: {}", stream_key, e);
                        }
                    }
                })
            },
        );

        Ok(stream)
    }
}

/// A single external publish stream instance.
///
/// Pulls from an external RTMP or HTTP-FLV source and publishes frames into
/// the local `StreamHub` under `live/{room_id}/{media_id}`.
pub struct ExternalPublishStream {
    room_id: String,
    media_id: String,
    source_url: String,
    stream_hub_event_sender: StreamHubEventSender,
    lifecycle: StreamLifecycle,
    /// Guard against sending duplicate UnPublish events (stop() + Drop)
    unpublish_sent: AtomicBool,
    /// Shared HTTP client for FLV connections (supports TLS via rustls).
    http_client: reqwest::Client,
}

impl ManagedStream for ExternalPublishStream {
    fn lifecycle(&self) -> &StreamLifecycle {
        &self.lifecycle
    }

    fn stream_key(&self) -> String {
        format!("{}:{}", self.room_id, self.media_id)
    }
}

impl ExternalPublishStream {
    #[must_use]
    pub fn new(
        room_id: String,
        media_id: String,
        source_url: String,
        stream_hub_event_sender: StreamHubEventSender,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            room_id,
            media_id,
            source_url,
            stream_hub_event_sender,
            lifecycle: StreamLifecycle::new(),
            unpublish_sent: AtomicBool::new(false),
            http_client,
        }
    }

    /// Start the external puller task.
    ///
    /// Spawns the puller in a background task and waits for it to confirm that
    /// the connection was established before returning. This prevents the caller
    /// from registering the stream in Redis before the source is actually reachable.
    pub async fn start(&self) -> StreamResult<()> {
        self.lifecycle.set_running();
        self.lifecycle.update_last_active_time();

        let room_id = self.room_id.clone();
        let media_id = self.media_id.clone();
        let source_url = self.source_url.clone();
        let stream_hub_sender = self.stream_hub_event_sender.clone();
        let http_client = self.http_client.clone();
        // Clone the is_running flag so the task can mark itself unhealthy on exit
        let is_running_flag = self.lifecycle.is_running_clone();

        let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

        let handle = tokio::spawn(async move {
            info!("External publish task started for {}/{}", room_id, media_id);
            let puller = match ExternalStreamPuller::new(
                room_id.clone(),
                media_id.clone(),
                source_url,
                stream_hub_sender,
            ) {
                Ok(p) => p.with_confirm(confirm_tx).with_http_client(http_client),
                Err(e) => {
                    let msg = format!("Failed to create puller for {room_id}/{media_id}: {e}");
                    error!("{}", msg);
                    let _ = confirm_tx.send(Err(msg));
                    is_running_flag.store(false, Ordering::SeqCst);
                    return Err(e);
                }
            };
            let result = puller.run().await;
            if let Err(ref e) = result {
                error!(
                    "External publish task failed for {}/{}: {}",
                    room_id,
                    media_id,
                    e
                );
            }
            // Mark as not running so is_healthy() returns false and the pool
            // can remove/replace this stream on next access.
            is_running_flag.store(false, Ordering::SeqCst);
            result
        });

        // Wait for the spawned task to confirm the connection was established
        match confirm_rx.await {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => {
                self.lifecycle.mark_stopping();
                return Err(crate::error::StreamError::ConnectionFailed(msg));
            }
            Err(_) => {
                self.lifecycle.mark_stopping();
                return Err(crate::error::StreamError::ConnectionFailed(
                    "Puller task exited before confirming connection".to_string(),
                ));
            }
        }

        self.lifecycle.set_task_handle(handle).await;
        info!(
            "External publish stream started for {}/{}",
            self.room_id,
            self.media_id
        );
        Ok(())
    }

    /// Stop the external puller task.
    ///
    /// Sends `UnPublish` to the local `StreamHub` BEFORE aborting, since the
    /// puller's own cleanup path won't run on abort.
    pub async fn stop(&self) -> StreamResult<()> {
        self.lifecycle.mark_stopping();

        // Only send UnPublish once (prevents duplicate when Drop also runs)
        if !self.unpublish_sent.swap(true, Ordering::AcqRel) {
            let identifier = StreamIdentifier::Rtmp {
                app_name: self.room_id.clone(),
                stream_name: self.media_id.clone(),
            };
            let room_id = self.room_id.clone();
            let media_id = self.media_id.clone();
            match self.stream_hub_event_sender.try_send(StreamHubEvent::UnPublish { identifier }) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                    // Channel full -- spawn async task to retry so UnPublish is not silently lost.
                    let sender = self.stream_hub_event_sender.clone();
                    warn!(
                        "ExternalPublishStream stop: channel full, spawning async UnPublish for {}/{}",
                        room_id, media_id
                    );
                    tokio::spawn(async move {
                        if let Err(e) = sender.send(event).await {
                            warn!(
                                "ExternalPublishStream stop: async UnPublish failed for {}/{}: {}",
                                room_id, media_id, e
                            );
                        }
                    });
                }
                Err(e) => {
                    warn!("Failed to send UnPublish for {}/{}: {}", room_id, media_id, e);
                }
            }
        }

        self.lifecycle.abort_task().await;
        info!(
            "External publish stream stopped for {}/{}",
            self.room_id,
            self.media_id
        );
        Ok(())
    }

    pub async fn is_healthy(&self) -> bool {
        self.lifecycle.is_healthy().await
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

    pub fn last_active_elapsed_secs(&self) -> u64 {
        self.lifecycle.last_active_elapsed_secs()
    }

    pub fn update_last_active_time(&self) {
        self.lifecycle.update_last_active_time();
    }
}

impl Drop for ExternalPublishStream {
    fn drop(&mut self) {
        // Only send UnPublish if stop() hasn't already sent it
        if !self.unpublish_sent.swap(true, Ordering::AcqRel) {
            let identifier = StreamIdentifier::Rtmp {
                app_name: self.room_id.clone(),
                stream_name: self.media_id.clone(),
            };
            if let Err(e) = self
                .stream_hub_event_sender
                .try_send(StreamHubEvent::UnPublish { identifier })
            {
                // During runtime shutdown, the channel may already be closed.
                // This is best-effort cleanup; Redis TTL will eventually expire the
                // publisher entry if this UnPublish is lost.
                warn!(
                    "ExternalPublishStream drop: failed to send UnPublish for {}/{}: {} \
                     (best-effort cleanup; Redis TTL will expire stale entry)",
                    self.room_id, self.media_id, e
                );
            }
        }

        debug!(
            "ExternalPublishStream dropped for {}/{}",
            self.room_id, self.media_id
        );
        // StreamLifecycle's Drop will abort the task handle (best-effort:
        // if the Tokio runtime is shutting down, the abort may not complete).
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::MockStreamRegistry;

    #[tokio::test]
    async fn test_external_publish_manager_creation() {
        let registry = Arc::new(MockStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let manager = ExternalPublishManager::new(registry, "node-1".to_string(), sender);
        assert_eq!(manager.pool.streams.len(), 0);
    }

    #[tokio::test]
    async fn test_external_publish_stream_subscriber_count() {
        let (sender, _) = tokio::sync::mpsc::channel(64);
        let stream = ExternalPublishStream::new(
            "room-1".to_string(),
            "media-1".to_string(),
            "rtmp://example.com/live/stream".to_string(),
            sender,
            reqwest::Client::new(),
        );

        assert_eq!(stream.subscriber_count(), 0);
        stream.increment_subscriber_count();
        assert_eq!(stream.subscriber_count(), 1);
        stream.decrement_subscriber_count();
        assert_eq!(stream.subscriber_count(), 0);
    }
}
