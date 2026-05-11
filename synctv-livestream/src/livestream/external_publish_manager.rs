// External Publish Manager
// Manages external pull-to-publish streams (RTMP / HTTP-FLV → local StreamHub).
// From the system's perspective this is a **publisher**: frames are pushed into
// the local StreamHub and the stream is registered in Redis so other nodes can
// discover and relay it via gRPC. The lifecycle mirrors PullStreamManager
// (lazy start on first viewer, idle cleanup after 5 min) but the two concerns
// are kept separate because external publish owns Redis registration/cleanup.

use crate::{
    error::StreamResult,
    livestream::external_puller::ExternalStreamPuller,
    livestream::managed_stream::{ManagedStream, StreamLifecycle, StreamPool},
    relay::registry_trait::StreamRegistryTrait,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use synctv_xiu::streamhub::define::{StreamHubEvent, StreamHubEventSender};
use synctv_xiu::streamhub::stream::StreamIdentifier;
use tracing::{debug, error, info, warn};

/// Default maximum number of concurrent external pull-to-publish streams.
///
/// Unlimited pull streams would exhaust memory on a heavily-loaded node.
/// This default can be overridden via `ExternalPublishManager::with_max_streams()`.
const DEFAULT_MAX_CONCURRENT_STREAMS: usize = 100;
const START_CONFIRM_TIMEOUT: Duration = Duration::from_secs(15);

async fn await_start_confirmation(
    confirm_rx: tokio::sync::oneshot::Receiver<Result<(), String>>,
    handle: &tokio::task::JoinHandle<anyhow::Result<()>>,
    timeout: Duration,
) -> StreamResult<()> {
    match tokio::time::timeout(timeout, confirm_rx).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(msg))) => {
            handle.abort();
            Err(crate::error::StreamError::ConnectionFailed(msg))
        }
        Ok(Err(_)) => {
            handle.abort();
            Err(crate::error::StreamError::ConnectionFailed(
                "Puller task exited before confirming connection".to_string(),
            ))
        }
        Err(_) => {
            handle.abort();
            Err(crate::error::StreamError::ConnectionFailed(format!(
                "Puller startup timed out after {}s",
                timeout.as_secs()
            )))
        }
    }
}

/// Manages external pull-to-publish streams.
///
/// Each stream is lazily started on the first viewer request. The manager
/// deduplicates concurrent requests (one puller per `room_id:media_id`),
/// registers the stream as a publisher in Redis, and automatically stops +
/// unregisters after 5 minutes with no subscribers.
pub struct ExternalPublishManager {
    pool: StreamPool<ExternalPublishStream>,
    registry: Arc<dyn StreamRegistryTrait>,
    local_node_id: String,
    /// Advertised shared API address of this node. Used when registering
    /// external publishers in Redis so other nodes can discover and relay
    /// streams via gRPC on the same listener.
    local_api_address: String,
    stream_hub_event_sender: StreamHubEventSender,
    /// Shared HTTP client for FLV connections. Built once with TLS (rustls) support
    /// and reused across all external publish streams to avoid per-stream TLS setup cost.
    http_client: reqwest::Client,
    /// Maximum number of concurrent pull streams.
    /// Prevents memory exhaustion from unlimited stream creation.
    max_concurrent_streams: usize,
    max_flv_tag_size_bytes: usize,
}

impl ExternalPublishManager {
    pub fn new(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> StreamResult<Self> {
        Self::with_timeouts(
            registry,
            local_node_id,
            String::new(),
            stream_hub_event_sender,
            60,
            300,
        )
    }

    /// Set the advertised shared API address used when registering external publishers in Redis.
    ///
    /// Other cluster nodes use this address to relay streams via gRPC.
    /// Should be called before the first `get_or_create` invocation.
    #[must_use]
    pub fn with_api_address(mut self, api_address: String) -> Self {
        self.local_api_address = api_address;
        self
    }

    /// Set the maximum number of concurrent pull streams.
    ///
    /// When this limit is reached, `get_or_create` returns an error instead of
    /// creating a new stream, preventing memory exhaustion from unlimited stream creation.
    ///
    /// Default: `DEFAULT_MAX_CONCURRENT_STREAMS` (100).
    #[must_use]
    pub const fn with_max_streams(mut self, max: usize) -> Self {
        self.max_concurrent_streams = max;
        self
    }

    #[must_use]
    pub const fn with_max_flv_tag_size_bytes(mut self, max: usize) -> Self {
        self.max_flv_tag_size_bytes = max;
        self
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
        local_api_address: String,
        stream_hub_event_sender: StreamHubEventSender,
        cleanup_check_interval_secs: u64,
        idle_timeout_secs: u64,
    ) -> StreamResult<Self> {
        // Build a shared reqwest::Client with TLS (rustls) support.
        // Reused across all HTTP-FLV pull streams to amortize TLS setup.
        // SSRF enforcement follows the active shared policy; with the current
        // runtime default this client does not inject a DNS resolver.
        let http_client = synctv_common::http::SsrfSafeClientBuilder::new()
            .connect_timeout(Duration::from_secs(10))
            .disable_request_timeout()
            .disable_read_timeout()
            .pool_max_idle_per_host(100)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| {
                crate::error::StreamError::Internal(format!(
                    "failed to build shared HTTP client: {e}"
                ))
            })?;

        let pool = StreamPool::new(
            Duration::from_secs(cleanup_check_interval_secs),
            Duration::from_secs(idle_timeout_secs),
        );
        Ok(Self {
            pool,
            registry,
            local_node_id,
            local_api_address,
            stream_hub_event_sender,
            http_client,
            max_concurrent_streams: DEFAULT_MAX_CONCURRENT_STREAMS,
            max_flv_tag_size_bytes: ExternalStreamPuller::DEFAULT_MAX_FLV_TAG_SIZE_BYTES,
        })
    }

    /// Stop all managed external publish streams, unregistering from Redis and
    /// aborting their tasks before clearing the pool.
    ///
    /// Called during `StreamHub` restart to ensure zombie streams (still connected
    /// to the old hub instance) are cleaned up before the new hub starts.
    pub async fn stop_all(&self) {
        // Unregister each stream from Redis before stopping, so entries don't
        // persist for the full TTL (up to 5 minutes) after graceful shutdown.
        let keys: Vec<String> = self.pool.streams.iter().map(|e| e.key().clone()).collect();
        for key in &keys {
            if let Some((room_id, media_id)) = key.split_once(':') {
                if let Err(e) = self.registry.unregister_publisher(room_id, media_id).await {
                    warn!(
                        "Failed to unregister external publisher {}/{} from Redis during stop_all: {}",
                        room_id, media_id, e
                    );
                }
            }
        }
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

        // Pre-check max concurrent streams BEFORE acquiring creation lock.
        // This provides fast rejection when the limit is already reached, avoiding
        // unnecessary lock contention. The check is repeated inside the lock for correctness.
        if self.pool.streams.len() >= self.max_concurrent_streams {
            warn!(
                "Max concurrent pull streams ({}) reached for {}/{}. \
                 Rejecting new stream request to prevent memory exhaustion (pre-check).",
                self.max_concurrent_streams, room_id, media_id
            );
            return Err(crate::error::StreamError::ResourceExhausted(format!(
                "Max concurrent pull streams ({}) reached. Try again later.",
                self.max_concurrent_streams
            )));
        }

        // Acquire per-key creation lock
        let _guard = self.pool.acquire_creation_lock(&stream_key).await;

        // Re-check after lock. get_existing() increments subscriber count.
        if let Some(stream) = self.pool.get_existing(&stream_key).await {
            debug!(
                "Reusing external publish stream created by concurrent request for {}/{}",
                room_id, media_id,
            );
            return Ok(stream);
        }

        // Second check inside the lock to prevent race condition.
        // Multiple requests for DIFFERENT streams may have passed the pre-check
        // concurrently, but only one can create at a time per-key. Re-check here
        // ensures we don't exceed the limit when multiple creations race.
        let current_count = self.pool.streams.len();
        if current_count >= self.max_concurrent_streams {
            warn!(
                "Max concurrent pull streams ({}) reached for {}/{}. \
                 Rejecting new stream request to prevent memory exhaustion (lock-internal check).",
                self.max_concurrent_streams, room_id, media_id
            );
            return Err(crate::error::StreamError::ResourceExhausted(format!(
                "Max concurrent pull streams ({}) reached. Try again later.",
                self.max_concurrent_streams
            )));
        }

        info!(
            "Lazy-load: Creating external publish stream for {}/{} from {} ({}/{} active streams)",
            room_id,
            media_id,
            source_url,
            current_count + 1,
            self.max_concurrent_streams,
        );

        let stream = Arc::new(ExternalPublishStream::new(
            room_id.to_string(),
            media_id.to_string(),
            source_url.to_string(),
            self.stream_hub_event_sender.clone(),
            self.http_client.clone(),
            self.max_flv_tag_size_bytes,
        ));

        // Validate that we have an API address before registering. Other nodes need this
        // address to relay the stream via gRPC; registering with an empty address means
        // cross-node relay will fail silently.
        if self.local_api_address.is_empty() {
            error!(
                "Cannot register external publisher for {}/{}: local_api_address is empty. \
                 Other nodes will be unable to relay this stream. \
                 Set api_address in LivestreamConfig.",
                room_id, media_id
            );
            return Err(crate::error::StreamError::InvalidState(
                "local_api_address is empty; cannot register external publisher without a valid API address".to_string(),
            ));
        }

        // Start the stream BEFORE registering in Redis to eliminate the
        // register-before-start race condition. If registration happened first and
        // the process crashed between registration and stream startup, a stale phantom
        // entry would remain in Redis until TTL expiry, preventing any other node from
        // taking over the stream.
        // New ordering:
        // 1. Start the puller (connect to source, begin frame ingestion)
        // 2. Register in Redis only after startup succeeds
        // Residual risk: if the process crashes between step 1 and step 2, the stream
        // runs locally but isn't discoverable by other nodes until it re-registers on
        // the next viewer request. This is a much smaller window than the reverse order
        // and self-heals on the next request without manual cleanup.

        // Start the puller (pushes frames into local StreamHub)
        if let Err(e) = stream.start().await {
            error!("Failed to start external stream, not registering: {e}");
            return Err(e);
        }

        // Register as publisher in Redis now that the stream is confirmed running.
        // If registration fails, stop the stream to avoid running unregistered.
        let registry_timeout = std::time::Duration::from_secs(5);
        let register_result = tokio::time::timeout(
            registry_timeout,
            self.registry.try_register_publisher(
                room_id,
                media_id,
                &self.local_node_id,
                "external_puller",
                &self.local_api_address,
            ),
        )
        .await
        .map_err(|_| {
            crate::error::StreamError::RegistryError(format!(
                "Registry registration timed out after {}s for {room_id}/{media_id}",
                registry_timeout.as_secs()
            ))
        })
        .and_then(|r| {
            r.map_err(|e| {
                crate::error::StreamError::RegistryError(format!(
                    "Failed to register publisher in Redis: {e}"
                ))
            })
        });

        match register_result {
            Ok(true) => { /* success, continue */ }
            Ok(false) => {
                // Another publisher already registered for this stream.
                // Stop the local stream to avoid a duplicate publisher.
                let _ = stream.stop().await;
                return Err(crate::error::StreamError::InvalidState(
                    "Another publisher already registered".into(),
                ));
            }
            Err(e) => {
                error!("Failed to register external publisher in Redis after stream start, stopping stream: {e}");
                // Roll back: stop the stream since we can't register it
                let _ = stream.stop().await;
                return Err(e);
            }
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

 // Send UnPublish to StreamHub (use send().await to avoid
 // silently dropping the event if the channel is momentarily full)
                        let identifier = StreamIdentifier::Rtmp {
                            app_name: room_id.to_string(),
                            stream_name: media_id.to_string(),
                        };
                        if let Err(e) = hub_sender.send(StreamHubEvent::UnPublish { identifier }).await {
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
    /// Guard against sending duplicate `UnPublish` events (`stop()` + Drop)
    unpublish_sent: AtomicBool,
    /// Shared HTTP client for FLV connections (supports TLS via rustls).
    http_client: reqwest::Client,
    max_flv_tag_size_bytes: usize,
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
        max_flv_tag_size_bytes: usize,
    ) -> Self {
        Self {
            room_id,
            media_id,
            source_url,
            stream_hub_event_sender,
            lifecycle: StreamLifecycle::new(),
            unpublish_sent: AtomicBool::new(false),
            http_client,
            max_flv_tag_size_bytes,
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
        let max_flv_tag_size_bytes = self.max_flv_tag_size_bytes;
        // Clone the is_running flag so the task can mark itself unhealthy on exit
        let is_running_flag = self.lifecycle.is_running_clone();

        let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

        let handle = tokio::spawn(async move {
            info!("External publish task started for {}/{}", room_id, media_id);
            // Use new_async() for proper async DNS resolution and SSRF validation.
            // This resolves the hostname at creation time and pins the resolved IP address,
            // preventing DNS rebinding attacks during the connection phase.
            let puller = match ExternalStreamPuller::new_async(
                room_id.clone(),
                media_id.clone(),
                source_url,
                stream_hub_sender,
            )
            .await
            {
                Ok(p) => p
                    .with_confirm(confirm_tx)
                    .with_http_client(http_client)
                    .with_max_flv_tag_size_bytes(max_flv_tag_size_bytes),
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
                    room_id, media_id, e
                );
            }
            // Mark as not running so is_healthy() returns false and the pool
            // can remove/replace this stream on next access.
            is_running_flag.store(false, Ordering::SeqCst);
            result
        });

        // Wait for the spawned task to confirm the connection was established.
        // Abort the task on timeout / startup failure so transient first-attempt
        // failures do not leave a retry loop running detached in the background.
        if let Err(error) =
            await_start_confirmation(confirm_rx, &handle, START_CONFIRM_TIMEOUT).await
        {
            self.lifecycle.mark_stopping();
            return Err(error);
        }

        self.lifecycle.set_task_handle(handle).await;
        info!(
            "External publish stream started for {}/{}",
            self.room_id, self.media_id
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
            match self
                .stream_hub_event_sender
                .try_send(StreamHubEvent::UnPublish { identifier })
            {
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
                    warn!(
                        "Failed to send UnPublish for {}/{}: {}",
                        room_id, media_id, e
                    );
                }
            }
        }

        self.lifecycle.abort_task().await;
        info!(
            "External publish stream stopped for {}/{}",
            self.room_id, self.media_id
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
            let room_id = self.room_id.clone();
            let media_id = self.media_id.clone();
            match self
                .stream_hub_event_sender
                .try_send(StreamHubEvent::UnPublish { identifier })
            {
                Ok(()) => {
                    debug!(
                        "ExternalPublishStream drop: sent UnPublish for {}/{}",
                        room_id, media_id
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                    // Channel full — spawn an async task to await capacity so the
                    // UnPublish is not silently dropped, leaving the stream registered.
                    let sender = self.stream_hub_event_sender.clone();
                    let room_id_for_task = room_id.clone();
                    let media_id_for_task = media_id.clone();
                    warn!(
                        "ExternalPublishStream drop: channel full, spawning async UnPublish for {}/{}",
                        room_id, media_id
                    );
                    if crate::util::try_spawn(async move {
                        if let Err(e) = sender.send(event).await {
                            warn!(
                                "ExternalPublishStream drop: async UnPublish failed for {}/{}: {} \
                                 (best-effort cleanup; Redis TTL will expire stale entry)",
                                room_id_for_task, media_id_for_task, e
                            );
                        }
                    })
                    .is_none()
                    {
                        warn!(
                            "ExternalPublishStream drop: no Tokio runtime available, skipping async UnPublish for {}/{}",
                            room_id, media_id
                        );
                    }
                }
                Err(e) => {
                    // During runtime shutdown, the channel may already be closed.
                    // This is best-effort cleanup; Redis TTL will eventually expire the
                    // publisher entry if this UnPublish is lost.
                    warn!(
                        "ExternalPublishStream drop: failed to send UnPublish for {}/{}: {} \
                         (best-effort cleanup; Redis TTL will expire stale entry)",
                        room_id, media_id, e
                    );
                }
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
    use std::future::pending;

    #[tokio::test]
    async fn test_external_publish_manager_creation() {
        let registry = Arc::new(MockStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let manager = ExternalPublishManager::new(registry, "node-1".to_string(), sender).unwrap();
        assert_eq!(manager.pool.streams.len(), 0);
    }

    #[tokio::test]
    async fn test_external_publish_manager_shared_http_client_disables_inherited_read_timeout() {
        let registry = Arc::new(MockStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let manager = ExternalPublishManager::new(registry, "node-1".to_string(), sender).unwrap();
        let request = manager
            .http_client
            .get("http://192.168.1.10:8080/stream.flv")
            .build()
            .expect("request should build");

        assert_eq!(
            request.timeout(),
            None,
            "shared HTTP-FLV client must not inherit a total request timeout"
        );

        let debug_repr = format!("{:?}", manager.http_client);
        assert!(
            !debug_repr.contains("read_timeout: Some(30s)"),
            "shared HTTP-FLV client must not inherit the proxy preset read timeout: {debug_repr}"
        );
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
            ExternalStreamPuller::DEFAULT_MAX_FLV_TAG_SIZE_BYTES,
        );

        assert_eq!(stream.subscriber_count(), 0);
        stream.increment_subscriber_count();
        assert_eq!(stream.subscriber_count(), 1);
        stream.decrement_subscriber_count();
        assert_eq!(stream.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn test_await_start_confirmation_aborts_task_on_error_signal() {
        let handle = tokio::spawn(async { pending::<anyhow::Result<()>>().await });
        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send(Err("startup failed".to_string()))
            .expect("sender should still be open");

        let result = await_start_confirmation(rx, &handle, Duration::from_secs(1)).await;

        assert!(matches!(
            result,
            Err(crate::error::StreamError::ConnectionFailed(message))
            if message == "startup failed"
        ));
        let join = handle
            .await
            .expect_err("aborted task should not complete normally");
        assert!(
            join.is_cancelled(),
            "task should be aborted on startup failure"
        );
    }

    #[tokio::test]
    async fn test_await_start_confirmation_aborts_task_on_timeout() {
        let handle = tokio::spawn(async { pending::<anyhow::Result<()>>().await });
        let (_tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

        let result = await_start_confirmation(rx, &handle, Duration::from_millis(10)).await;

        assert!(matches!(
            result,
            Err(crate::error::StreamError::ConnectionFailed(message))
            if message.contains("timed out")
        ));
        let join = handle.await.expect_err("timed out startup must abort task");
        assert!(join.is_cancelled(), "timed out task should be aborted");
    }

    #[tokio::test]
    async fn test_await_start_confirmation_succeeds_without_aborting_task() {
        let handle = tokio::spawn(async { Ok(()) });
        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send(Ok(())).expect("sender should still be open");

        let result = await_start_confirmation(rx, &handle, Duration::from_secs(1)).await;

        assert!(result.is_ok(), "successful confirmation should pass");
        let join = handle.await.expect("join should succeed");
        assert!(join.is_ok(), "task should continue normally after success");
    }
}
