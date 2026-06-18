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
    relay::{registry::HEARTBEAT_INTERVAL_SECS, registry_trait::StreamRegistryTrait},
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use synctv_common::ssrf::SsrfGuard;
use synctv_xiu::streamhub::define::{StreamHubEvent, StreamHubEventSender};
use synctv_xiu::streamhub::stream::StreamIdentifier;
use tracing::{debug, error, info, warn};

const DEFAULT_MAX_CONCURRENT_STREAMS: usize = 100;
const START_CONFIRM_TIMEOUT: Duration = Duration::from_secs(15);
const EXTERNAL_PUBLISHER_USER_ID: &str = "";

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
pub(crate) struct ExternalPublishManager {
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
    /// Explicit SSRF policy injected by the application layer.
    ssrf_guard: SsrfGuard,
    /// Maximum number of concurrent pull streams.
    /// Prevents memory exhaustion from unlimited stream creation.
    max_concurrent_streams: usize,
    creation_capacity_lock: tokio::sync::Mutex<()>,
    max_flv_tag_size_bytes: usize,
}

struct ExternalRegistration {
    registered: bool,
    epoch: Option<u64>,
}

impl ExternalRegistration {
    const fn new(registered: bool, epoch: Option<u64>) -> Self {
        Self { registered, epoch }
    }
}

impl ExternalPublishManager {
    pub(crate) fn new(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        stream_hub_event_sender: StreamHubEventSender,
        ssrf_guard: SsrfGuard,
    ) -> StreamResult<Self> {
        Self::with_timeouts(
            registry,
            local_node_id,
            String::new(),
            stream_hub_event_sender,
            ssrf_guard,
            60,
            300,
        )
    }

    #[must_use]
    pub(crate) const fn with_max_flv_tag_size_bytes(mut self, max: usize) -> Self {
        self.max_flv_tag_size_bytes = max;
        self
    }

    pub(crate) fn with_timeouts(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        local_api_address: String,
        stream_hub_event_sender: StreamHubEventSender,
        ssrf_guard: SsrfGuard,
        cleanup_check_interval_secs: u64,
        idle_timeout_secs: u64,
    ) -> StreamResult<Self> {
        // Build a shared reqwest::Client with TLS (rustls) support.
        // Reused across all HTTP-FLV pull streams to amortize TLS setup.
        let http_client = synctv_common::http::SsrfSafeClientBuilder::new()
            .ssrf_guard(ssrf_guard.clone())
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
            ssrf_guard,
            max_concurrent_streams: DEFAULT_MAX_CONCURRENT_STREAMS,
            creation_capacity_lock: tokio::sync::Mutex::new(()),
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
    pub(crate) async fn get_or_create(
        &self,
        room_id: &str,
        media_id: &str,
        source_url: &str,
    ) -> StreamResult<Arc<ExternalPublishStream>> {
        let stream_key = format!("{room_id}:{media_id}");

        // Fast path: reuse healthy stream. get_existing() increments subscriber count.
        if let Some(stream) = self.pool.get_existing(&stream_key).await {
            if let Err(error) = self
                .ensure_external_registration(room_id, media_id, &stream)
                .await
            {
                stream.decrement_subscriber_count();
                return Err(error);
            }
            return Ok(stream);
        }

        // Acquire per-key creation lock
        let _guard = self.pool.acquire_creation_lock(&stream_key).await;

        // Re-check after lock. get_existing() increments subscriber count.
        if let Some(stream) = self.pool.get_existing(&stream_key).await {
            if let Err(error) = self
                .ensure_external_registration(room_id, media_id, &stream)
                .await
            {
                stream.decrement_subscriber_count();
                return Err(error);
            }
            debug!(
                "Reusing external publish stream created by concurrent request for {}/{}",
                room_id, media_id,
            );
            return Ok(stream);
        }

        let _capacity_guard = self.creation_capacity_lock.lock().await;
        let current_count = self.pool.streams.len();
        if current_count >= self.max_concurrent_streams {
            warn!(
                "Max concurrent pull streams ({}) reached for {}/{}. \
                 Rejecting new stream request to prevent memory exhaustion.",
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
            super::external_puller::redact_source_url_for_logs(source_url),
            current_count + 1,
            self.max_concurrent_streams,
        );

        let stream = Arc::new(ExternalPublishStream::new(
            room_id.to_string(),
            media_id.to_string(),
            source_url.to_string(),
            self.stream_hub_event_sender.clone(),
            self.http_client.clone(),
            self.ssrf_guard.clone(),
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
            self.register_external_publisher(room_id, media_id),
        )
        .await
        .map_err(|_| {
            crate::error::StreamError::RegistryError(format!(
                "Registry registration timed out after {}s for {room_id}/{media_id}",
                registry_timeout.as_secs()
            ))
        })
        .and_then(std::convert::identity);

        match register_result {
            Ok(registration) if registration.registered => {
                stream.set_registration_epoch(registration.epoch);
            }
            Ok(_) => {
                if self
                    .clear_stale_local_external_registration(room_id, media_id)
                    .await?
                {
                    let retry_result = self.register_external_publisher(room_id, media_id).await;
                    match retry_result {
                        Ok(registration) if registration.registered => {
                            stream.set_registration_epoch(registration.epoch);
                        }
                        Ok(_) => {
                            Self::stop_unregistered_stream(&stream, room_id, media_id).await;
                            return Err(crate::error::StreamError::InvalidState(
                                "Another publisher already registered".into(),
                            ));
                        }
                        Err(error) => {
                            Self::stop_unregistered_stream(&stream, room_id, media_id).await;
                            return Err(error);
                        }
                    }
                } else {
                    Self::stop_unregistered_stream(&stream, room_id, media_id).await;
                    return Err(crate::error::StreamError::InvalidState(
                        "Another publisher already registered".into(),
                    ));
                }
            }
            Err(e) => {
                error!("Failed to register external publisher in Redis after stream start, stopping stream: {e}");
                Self::stop_unregistered_stream(&stream, room_id, media_id).await;
                return Err(e);
            }
        }

        // Creation path: increment subscriber count exactly once for the viewer
        // that triggered creation. (Reuse paths increment inside get_existing().)
        stream.lifecycle().increment_subscriber_count();

        // External pullers are long-lived publishers owned by this manager, not
        // user RTMP publishers tracked by PublisherManager. Keep their Redis
        // ownership entry alive for as long as the local puller is healthy so
        // FLV/HLS requests can discover the stream after the original viewer
        // request has completed.
        self.start_registration_heartbeat(&stream);

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
                    // Unregister from Redis first so other nodes stop routing.
                    if let Some((room_id, media_id)) = stream_key.split_once(':') {
                        match registry.get_publisher(room_id, media_id).await {
                            Ok(Some(info)) if info.node_id == local_node_id => {
                                if let Err(e) = registry.unregister_publisher(room_id, media_id).await
                                {
                                    warn!("Failed to unregister external publisher from Redis: {e}");
                                }
                            }
                            Ok(Some(_)) => {
                                info!(
                                    "Skipping Redis unregister for {} because publisher is owned by another node",
                                    stream_key
                                );
                            }
                            _ => {}
                        }

                        let identifier = StreamIdentifier::Rtmp {
                            app_name: room_id.to_string(),
                            stream_name: media_id.to_string(),
                        };
                        if let Err(e) = hub_sender.send(StreamHubEvent::UnPublish { identifier }).await
                        {
                            warn!("Failed to send UnPublish for {}: {}", stream_key, e);
                        }
                    }
                })
            },
        );

        Ok(stream)
    }

    async fn register_external_publisher(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> StreamResult<ExternalRegistration> {
        let registered = self
            .registry
            .try_register_publisher(
                room_id,
                media_id,
                &self.local_node_id,
                EXTERNAL_PUBLISHER_USER_ID,
                &self.local_api_address,
            )
            .await
            .map_err(|e| {
                crate::error::StreamError::RegistryError(format!(
                    "Failed to register publisher in Redis: {e}"
                ))
            })?;

        if !registered {
            return Ok(ExternalRegistration::new(false, None));
        }

        let Some(publisher) = self
            .load_local_external_publisher(room_id, media_id)
            .await?
        else {
            return Err(crate::error::StreamError::RegistryError(format!(
                "Publisher registration for {room_id}/{media_id} could not be read back"
            )));
        };

        Ok(ExternalRegistration::new(true, Some(publisher.epoch)))
    }

    async fn load_local_external_publisher(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> StreamResult<Option<crate::relay::PublisherInfo>> {
        let publisher = self
            .registry
            .get_publisher(room_id, media_id)
            .await
            .map_err(|e| {
                crate::error::StreamError::RegistryError(format!(
                    "Failed to load publisher registration: {e}"
                ))
            })?;

        Ok(publisher.filter(|info| {
            info.node_id == self.local_node_id && info.user_id == EXTERNAL_PUBLISHER_USER_ID
        }))
    }

    async fn ensure_external_registration(
        &self,
        room_id: &str,
        media_id: &str,
        stream: &Arc<ExternalPublishStream>,
    ) -> StreamResult<()> {
        let current_epoch = stream.registration_epoch();
        if let Some(epoch) = current_epoch {
            match self
                .registry
                .refresh_publisher_ttl(
                    room_id,
                    media_id,
                    EXTERNAL_PUBLISHER_USER_ID,
                    &self.local_node_id,
                    epoch,
                )
                .await
            {
                Ok(crate::relay::PublisherRefreshOutcome::Refreshed) => return Ok(()),
                Ok(crate::relay::PublisherRefreshOutcome::Missing) => {
                    warn!(
                        room_id,
                        media_id, epoch, "External publisher registration is missing; restoring it"
                    );
                }
                Ok(crate::relay::PublisherRefreshOutcome::OwnershipChanged) => {
                    warn!(
                        room_id,
                        media_id,
                        epoch,
                        "External publisher registration ownership changed; revalidating before reuse"
                    );
                }
                Err(error) => {
                    return Err(crate::error::StreamError::RegistryError(format!(
                        "Failed to refresh external publisher registration: {error}"
                    )));
                }
            }
        }

        if let Some(existing) = self
            .load_local_external_publisher(room_id, media_id)
            .await?
        {
            stream.set_registration_epoch(Some(existing.epoch));
            return Ok(());
        }

        let registration = self.register_external_publisher(room_id, media_id).await?;
        if registration.registered {
            stream.set_registration_epoch(registration.epoch);
            return Ok(());
        }

        Err(crate::error::StreamError::InvalidState(
            "Another publisher already registered".to_string(),
        ))
    }

    fn start_registration_heartbeat(&self, stream: &Arc<ExternalPublishStream>) {
        let registry = Arc::clone(&self.registry);
        let local_node_id = self.local_node_id.clone();
        let stream = Arc::downgrade(stream);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                let Some(stream) = stream.upgrade() else {
                    break;
                };

                if !stream.lifecycle().is_healthy().await {
                    break;
                }

                let Some(epoch) = stream.registration_epoch() else {
                    continue;
                };

                match registry
                    .refresh_publisher_ttl(
                        &stream.room_id,
                        &stream.media_id,
                        EXTERNAL_PUBLISHER_USER_ID,
                        &local_node_id,
                        epoch,
                    )
                    .await
                {
                    Ok(crate::relay::PublisherRefreshOutcome::Refreshed) => {}
                    Ok(crate::relay::PublisherRefreshOutcome::Missing) => {
                        warn!(
                            room_id = %stream.room_id,
                            media_id = %stream.media_id,
                            epoch,
                            "External publisher registration disappeared during heartbeat"
                        );
                        stream.set_registration_epoch(None);
                    }
                    Ok(crate::relay::PublisherRefreshOutcome::OwnershipChanged) => {
                        warn!(
                            room_id = %stream.room_id,
                            media_id = %stream.media_id,
                            epoch,
                            "External publisher registration ownership changed during heartbeat"
                        );
                        stream.set_registration_epoch(None);
                    }
                    Err(error) => {
                        warn!(
                            room_id = %stream.room_id,
                            media_id = %stream.media_id,
                            epoch,
                            %error,
                            "Failed to refresh external publisher registration"
                        );
                    }
                }
            }
        });
    }

    async fn stop_unregistered_stream(
        stream: &Arc<ExternalPublishStream>,
        room_id: &str,
        media_id: &str,
    ) {
        if let Err(stop_error) = stream.stop().await {
            warn!(
                room_id,
                media_id,
                error = %stop_error,
                "Failed to stop unregistered external publish stream"
            );
        }
    }

    async fn clear_stale_local_external_registration(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> StreamResult<bool> {
        let existing = self
            .registry
            .get_publisher(room_id, media_id)
            .await
            .map_err(|e| {
                crate::error::StreamError::RegistryError(format!(
                    "Failed to inspect publisher registration: {e}"
                ))
            })?;
        let Some(existing) = existing else {
            return Ok(false);
        };
        if existing.node_id != self.local_node_id || !existing.user_id.is_empty() {
            return Ok(false);
        }

        self.registry
            .unregister_publisher_if_epoch_matches(room_id, media_id, existing.epoch)
            .await
            .map_err(|e| {
                crate::error::StreamError::RegistryError(format!(
                    "Failed to clear stale local external publisher registration: {e}"
                ))
            })?;
        Ok(true)
    }
}

/// A single external publish stream instance.
///
/// Pulls from an external RTMP or HTTP-FLV source and publishes frames into
/// the local `StreamHub` under `live/{room_id}/{media_id}`.
pub(crate) struct ExternalPublishStream {
    room_id: String,
    media_id: String,
    source_url: String,
    stream_hub_event_sender: StreamHubEventSender,
    lifecycle: StreamLifecycle,
    /// Guard against sending duplicate `UnPublish` events (`stop()` + Drop)
    unpublish_sent: AtomicBool,
    /// Redis publisher epoch for the system-owned external puller.
    registration_epoch: std::sync::atomic::AtomicU64,
    /// Shared HTTP client for FLV connections (supports TLS via rustls).
    http_client: reqwest::Client,
    ssrf_guard: SsrfGuard,
    max_flv_tag_size_bytes: usize,
}

#[async_trait::async_trait]
impl ManagedStream for ExternalPublishStream {
    fn lifecycle(&self) -> &StreamLifecycle {
        &self.lifecycle
    }

    async fn stop_managed(&self) {
        if let Err(error) = self.stop().await {
            warn!(
                room_id = %self.room_id,
                media_id = %self.media_id,
                %error,
                "Failed to stop external publish stream during managed cleanup"
            );
        }
    }
}

impl ExternalPublishStream {
    #[must_use]
    pub(crate) fn new(
        room_id: String,
        media_id: String,
        source_url: String,
        stream_hub_event_sender: StreamHubEventSender,
        http_client: reqwest::Client,
        ssrf_guard: SsrfGuard,
        max_flv_tag_size_bytes: usize,
    ) -> Self {
        Self {
            room_id,
            media_id,
            source_url,
            stream_hub_event_sender,
            lifecycle: StreamLifecycle::new(),
            unpublish_sent: AtomicBool::new(false),
            registration_epoch: std::sync::atomic::AtomicU64::new(0),
            http_client,
            ssrf_guard,
            max_flv_tag_size_bytes,
        }
    }

    /// Start the external puller task.
    ///
    /// Spawns the puller in a background task and waits for it to confirm that
    /// the connection was established before returning. This prevents the caller
    /// from registering the stream in Redis before the source is actually reachable.
    async fn start(&self) -> StreamResult<()> {
        self.lifecycle.set_running();
        self.lifecycle.update_last_active_time();

        let room_id = self.room_id.clone();
        let media_id = self.media_id.clone();
        let source_url = self.source_url.clone();
        let stream_hub_sender = self.stream_hub_event_sender.clone();
        let http_client = self.http_client.clone();
        let ssrf_guard = self.ssrf_guard.clone();
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
                ssrf_guard,
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
                    if confirm_tx.send(Err(msg)).is_err() {
                        debug!(
                            room_id,
                            media_id,
                            "external publish startup receiver dropped before puller creation failure"
                        );
                    }
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
    async fn stop(&self) -> StreamResult<()> {
        self.lifecycle.mark_stopping();

        // Only send UnPublish once (prevents duplicate when Drop also runs)
        if !self.unpublish_sent.swap(true, Ordering::AcqRel) {
            let identifier = StreamIdentifier::Rtmp {
                app_name: self.room_id.clone(),
                stream_name: self.media_id.clone(),
            };
            let room_id = self.room_id.clone();
            let media_id = self.media_id.clone();
            if let Err(e) = self
                .stream_hub_event_sender
                .try_send(StreamHubEvent::UnPublish { identifier })
            {
                warn!(
                    "Failed to send UnPublish for {}/{}: {}",
                    room_id, media_id, e
                );
            }
        }

        self.lifecycle.abort_task().await;
        info!(
            "External publish stream stopped for {}/{}",
            self.room_id, self.media_id
        );
        Ok(())
    }

    pub(crate) fn decrement_subscriber_count(&self) {
        self.lifecycle.decrement_subscriber_count();
    }

    fn set_registration_epoch(&self, epoch: Option<u64>) {
        self.registration_epoch
            .store(epoch.unwrap_or(0), Ordering::Release);
    }

    fn registration_epoch(&self) -> Option<u64> {
        match self.registration_epoch.load(Ordering::Acquire) {
            0 => None,
            epoch => Some(epoch),
        }
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
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    warn!(
                        "ExternalPublishStream drop: channel full, skipped UnPublish for {}/{}",
                        room_id, media_id
                    );
                }
                Err(e) => {
                    warn!(
                        "ExternalPublishStream drop: failed to send UnPublish for {}/{}: {}",
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
    use crate::relay::TestStreamRegistry;
    use std::future::pending;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    #[tokio::test]
    async fn test_external_publish_manager_creation() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let manager = ExternalPublishManager::new(
            registry,
            "node-1".to_string(),
            sender,
            SsrfGuard::disabled(),
        )?;
        assert_eq!(manager.pool.streams.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_external_publish_manager_rejects_when_capacity_full() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let mut manager = ExternalPublishManager::new(
            registry,
            "node-1".to_string(),
            sender,
            SsrfGuard::disabled(),
        )?;
        manager.max_concurrent_streams = 0;

        let Err(error) = manager
            .get_or_create("room1", "media1", "http://127.0.0.1:8080/live.flv")
            .await
        else {
            return Err(test_error(
                "capacity limit should reject before starting puller",
            ));
        };

        assert!(
            matches!(error, crate::error::StreamError::ResourceExhausted(_)),
            "expected ResourceExhausted, got {error:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_external_publish_manager_shared_http_client_disables_inherited_read_timeout(
    ) -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new()) as Arc<dyn StreamRegistryTrait>;
        let (sender, _) = tokio::sync::mpsc::channel(64);

        let manager = ExternalPublishManager::new(
            registry,
            "node-1".to_string(),
            sender,
            SsrfGuard::disabled(),
        )?;
        let request = manager
            .http_client
            .get("http://192.168.1.10:8080/stream.flv")
            .build()?;

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
        Ok(())
    }

    #[tokio::test]
    async fn test_clear_stale_local_external_registration_only_clears_system_owned_local_entry(
    ) -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        registry
            .try_register_publisher("room1", "media1", "node-1", "", "127.0.0.1:50051")
            .await?;
        registry
            .try_register_publisher("room1", "media2", "node-1", "user-1", "127.0.0.1:50051")
            .await?;
        registry
            .try_register_publisher("room1", "media3", "node-2", "", "127.0.0.2:50051")
            .await?;

        let (sender, _) = tokio::sync::mpsc::channel(64);
        let manager = ExternalPublishManager::new(
            registry.clone(),
            "node-1".to_string(),
            sender,
            SsrfGuard::disabled(),
        )?;

        assert!(
            manager
                .clear_stale_local_external_registration("room1", "media1")
                .await?
        );
        assert!(
            registry.get_publisher("room1", "media1").await?.is_none(),
            "same-node system-owned external publisher registration should be cleared"
        );
        assert!(
            !manager
                .clear_stale_local_external_registration("room1", "media2")
                .await?
        );
        assert!(
            registry.get_publisher("room1", "media2").await?.is_some(),
            "same-node user-owned RTMP publisher registration should be preserved"
        );
        assert!(
            !manager
                .clear_stale_local_external_registration("room1", "media3")
                .await?
        );
        assert!(
            registry.get_publisher("room1", "media3").await?.is_some(),
            "remote publisher registration should be preserved"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_reused_external_stream_restores_missing_registry_registration() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        let (sender, _) = tokio::sync::mpsc::channel(64);
        let manager = ExternalPublishManager::with_timeouts(
            registry.clone(),
            "node-1".to_string(),
            "127.0.0.1:50051".to_string(),
            sender.clone(),
            SsrfGuard::disabled(),
            60,
            300,
        )?;

        let stream = Arc::new(ExternalPublishStream::new(
            "room1".to_string(),
            "media1".to_string(),
            "rtmp://example.com/live/stream".to_string(),
            sender,
            reqwest::Client::new(),
            SsrfGuard::disabled(),
            ExternalStreamPuller::DEFAULT_MAX_FLV_TAG_SIZE_BYTES,
        ));
        stream.lifecycle().set_running();

        let registered = manager
            .register_external_publisher("room1", "media1")
            .await?;
        assert!(registered.registered);
        stream.set_registration_epoch(registered.epoch);
        registry.unregister_publisher("room1", "media1").await?;

        manager.pool.insert_and_cleanup(
            "room1:media1".to_string(),
            stream.clone(),
            |_stream_key| Box::pin(async {}),
        );

        let reused = manager
            .get_or_create("room1", "media1", "rtmp://example.com/live/stream")
            .await?;

        assert!(Arc::ptr_eq(&reused, &stream));
        let restored = registry
            .get_publisher("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher registration should be restored"))?;
        assert_eq!(restored.node_id, "node-1");
        assert_eq!(restored.user_id, "");
        assert_eq!(stream.registration_epoch(), Some(restored.epoch));
        assert_eq!(
            stream.lifecycle().subscriber_count(),
            1,
            "reuse path must keep exactly one subscriber for the caller"
        );
        Ok(())
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
            SsrfGuard::disabled(),
            ExternalStreamPuller::DEFAULT_MAX_FLV_TAG_SIZE_BYTES,
        );

        assert_eq!(stream.lifecycle().subscriber_count(), 0);
        stream.lifecycle().increment_subscriber_count();
        assert_eq!(stream.lifecycle().subscriber_count(), 1);
        stream.decrement_subscriber_count();
        assert_eq!(stream.lifecycle().subscriber_count(), 0);
    }

    #[tokio::test]
    async fn test_await_start_confirmation_aborts_task_on_error_signal() -> TestResult {
        let handle = tokio::spawn(async { pending::<anyhow::Result<()>>().await });
        let (tx, rx) = tokio::sync::oneshot::channel();
        assert!(tx.send(Err("startup failed".to_string())).is_ok());

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
        Ok(())
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
    async fn test_await_start_confirmation_succeeds_without_aborting_task() -> TestResult {
        let handle = tokio::spawn(async { Ok(()) });
        let (tx, rx) = tokio::sync::oneshot::channel();
        assert!(tx.send(Ok(())).is_ok());

        let result = await_start_confirmation(rx, &handle, Duration::from_secs(1)).await;

        assert!(result.is_ok(), "successful confirmation should pass");
        let join = handle.await?;
        assert!(join.is_ok(), "task should continue normally after success");
        Ok(())
    }
}
