// External Publish Manager
// Manages external RTMP, RTSP, and HTTP-FLV pull-to-publish streams.
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
use futures::StreamExt as _;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use synctv_common::ssrf::SsrfGuard;
use synctv_xiu::streamhub::define::StreamHubEventSender;
use tracing::{debug, error, info, warn};

const DEFAULT_MAX_CONCURRENT_STREAMS: usize = 100;
const STOP_UNREGISTER_CONCURRENCY: usize = 16;
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
    /// Advertised cluster listener address of this node. Used when registering
    /// external publishers in Redis so other nodes can discover and relay streams.
    local_cluster_address: String,
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

pub(crate) struct ExternalStreamActivityGuard(Arc<ExternalPublishStream>);

impl Drop for ExternalStreamActivityGuard {
    fn drop(&mut self) {
        self.0.decrement_subscriber_count();
    }
}

struct ExternalRegistration {
    registered: bool,
    lease_epoch: Option<u64>,
}

impl ExternalRegistration {
    const fn new(registered: bool, lease_epoch: Option<u64>) -> Self {
        Self {
            registered,
            lease_epoch,
        }
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
        local_cluster_address: String,
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
            local_cluster_address,
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
        let streams: Vec<_> = self
            .pool
            .streams
            .iter()
            .map(|entry| {
                (
                    entry.key().clone(),
                    entry.value().generation_id.to_string(),
                    entry.value().registration_lease_epoch(),
                )
            })
            .collect();
        // Close the pool admission gate before touching registry ownership.
        // This prevents a viewer from re-registering the stream between the
        // owner snapshot and the stream stop operation.
        self.pool.stop_all().await;

        futures::stream::iter(&streams)
            .for_each_concurrent(
                STOP_UNREGISTER_CONCURRENCY,
                |(key, generation_id, lease_epoch)| async move {
                    let Some((room_id, media_id)) = key.split_once(':') else {
                        return;
                    };
                    match self.registry.get_active_generation(room_id, media_id).await {
                        Ok(Some(current))
                            if current.generation_id == *generation_id
                                && *lease_epoch == Some(current.lease_epoch) =>
                        {
                            if let Err(error) = self
                                .registry
                                .deactivate_generation_preserving_hls_if_lease_matches(
                                    room_id,
                                    media_id,
                                    generation_id,
                                    current.lease_epoch,
                                )
                                .await
                            {
                                warn!(
                                    "Failed to unregister external publisher {room_id}/{media_id} during stop_all: {error}"
                                );
                            }
                        }
                        Ok(_) => {}
                        Err(error) => warn!(
                            "Failed to inspect external publisher {room_id}/{media_id} during stop_all: {error}"
                        ),
                    }
                },
            )
            .await;
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
        source: &synctv_core::models::ExternalLiveSourceConfig,
    ) -> StreamResult<Arc<ExternalPublishStream>> {
        let stream_key = format!("{room_id}:{media_id}");

        // Fast path: reuse healthy stream. get_existing() increments subscriber count.
        if let Some(stream) = self.pool.get_existing(&stream_key).await {
            if stream.source == *source {
                if let Err(error) = self
                    .ensure_external_registration(room_id, media_id, &stream)
                    .await
                {
                    stream.decrement_subscriber_count();
                    return Err(error);
                }
                return Ok(stream);
            }
            stream.decrement_subscriber_count();
        }

        // Acquire per-key creation lock
        let _guard = self.pool.acquire_creation_lock(&stream_key).await;

        // Re-check after lock. get_existing() increments subscriber count.
        if let Some(stream) = self.pool.get_existing(&stream_key).await {
            if stream.source == *source {
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
            stream.decrement_subscriber_count();
            info!(
                room_id,
                media_id,
                old_source = %super::external_puller::redact_source_url_for_logs(stream.source.url()),
                new_source = %super::external_puller::redact_source_url_for_logs(source.url()),
                "Replacing external pull after source configuration changed"
            );
            self.pool
                .remove_if_same_and_stop(&stream_key, &stream)
                .await;
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
            super::external_puller::redact_source_url_for_logs(source.url()),
            current_count + 1,
            self.max_concurrent_streams,
        );

        let stream = Arc::new(ExternalPublishStream::new(ExternalPublishStreamParams {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            source: source.clone(),
            stream_hub_event_sender: self.stream_hub_event_sender.clone(),
            http_client: self.http_client.clone(),
            ssrf_guard: self.ssrf_guard.clone(),
            max_flv_tag_size_bytes: self.max_flv_tag_size_bytes,
        }));

        // Validate that we have a cluster address before registering. Other nodes need this
        // address to relay the stream via gRPC; registering with an empty address means
        // cross-node relay will fail silently.
        if self.local_cluster_address.is_empty() {
            error!(
                "Cannot register external publisher for {}/{}: local_cluster_address is empty. \
                 Other nodes will be unable to relay this stream. \
                 Set cluster_address in LivestreamConfig.",
                room_id, media_id
            );
            return Err(crate::error::StreamError::InvalidState(
                "local_cluster_address is empty; cannot register external publisher without a valid cluster address".to_string(),
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
            self.register_external_publisher(room_id, media_id, stream.generation_id),
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
                stream.set_registration_lease_epoch(registration.lease_epoch);
            }
            Ok(_) => {
                if self
                    .clear_stale_local_external_registration(room_id, media_id)
                    .await?
                {
                    let retry_result = self
                        .register_external_publisher(room_id, media_id, stream.generation_id)
                        .await;
                    match retry_result {
                        Ok(registration) if registration.registered => {
                            stream.set_registration_lease_epoch(registration.lease_epoch);
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
        let expected_generation_id = stream.generation_id.to_string();
        let expected_lease_epoch = stream.registration_lease_epoch();

        self.pool.insert_and_cleanup(
            stream_key,
            Arc::clone(&stream),
            move |stream_key: &str| {
                let registry = Arc::clone(&registry);
                let local_node_id = local_node_id.clone();
                let expected_generation_id = expected_generation_id.clone();
                let stream_key = stream_key.to_string();
                Box::pin(async move {
                    // Release active ownership while the HLS remuxer finalizes
                    // and retains this generation for existing viewers.
                    if let Some((room_id, media_id)) = stream_key.split_once(':') {
                        match registry.get_active_generation(room_id, media_id).await {
                            Ok(Some(info))
                                if info.node_id == local_node_id
                                    && info.generation_id == expected_generation_id
                                    && expected_lease_epoch == Some(info.lease_epoch) =>
                            {
                                if let Err(e) = registry
                                    .deactivate_generation_preserving_hls_if_lease_matches(
                                        room_id,
                                        media_id,
                                        &expected_generation_id,
                                        info.lease_epoch,
                                    )
                                    .await
                                {
                                    warn!("Failed to unregister external publisher from Redis: {e}");
                                }
                            }
                            Ok(Some(_)) => {
                                info!(
                                    "Skipping Redis unregister for {} because publisher generation ownership changed",
                                    stream_key
                                );
                            }
                            _ => {}
                        }
                    }
                })
            },
        );

        Ok(stream)
    }

    /// Subscribe to an external stream that this manager already owns locally.
    ///
    /// Local external streams are also registered as publishers, so callers
    /// reach the local-publisher branch before the normal creation path. This
    /// lookup preserves the managed-stream subscriber and activity lifecycle
    /// for every subsequent FLV or HLS request.
    pub(crate) async fn subscribe_existing(
        &self,
        room_id: &str,
        media_id: &str,
        source: &synctv_core::models::ExternalLiveSourceConfig,
    ) -> StreamResult<Option<Arc<ExternalPublishStream>>> {
        let stream_key = format!("{room_id}:{media_id}");
        let Some(stream) = self.pool.get_existing(&stream_key).await else {
            return Ok(None);
        };

        if stream.source != *source {
            stream.decrement_subscriber_count();
            return Ok(None);
        }

        if let Err(error) = self
            .ensure_external_registration(room_id, media_id, &stream)
            .await
        {
            stream.decrement_subscriber_count();
            return Err(error);
        }

        Ok(Some(stream))
    }

    pub(crate) async fn subscribe_active_generation(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
    ) -> Option<ExternalStreamActivityGuard> {
        let stream_key = format!("{room_id}:{media_id}");
        let stream = self.pool.get_existing(&stream_key).await?;
        if stream.generation_id.to_string() == generation_id {
            Some(ExternalStreamActivityGuard(stream))
        } else {
            stream.decrement_subscriber_count();
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn install_running_test_stream(
        &self,
        room_id: &str,
        media_id: &str,
        source: synctv_core::models::ExternalLiveSourceConfig,
    ) -> (
        Arc<ExternalPublishStream>,
        synctv_xiu::streamhub::utils::Uuid,
    ) {
        let stream = Arc::new(ExternalPublishStream::new(ExternalPublishStreamParams {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            source,
            stream_hub_event_sender: self.stream_hub_event_sender.clone(),
            http_client: self.http_client.clone(),
            ssrf_guard: self.ssrf_guard.clone(),
            max_flv_tag_size_bytes: self.max_flv_tag_size_bytes,
        }));
        stream.lifecycle().set_running();
        stream.lifecycle().update_last_active_time();
        self.pool.insert_and_cleanup(
            format!("{room_id}:{media_id}"),
            Arc::clone(&stream),
            |_stream_key| Box::pin(async {}),
        );
        let generation_id = stream.generation_id;
        (stream, generation_id)
    }

    async fn register_external_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: synctv_xiu::streamhub::utils::Uuid,
    ) -> StreamResult<ExternalRegistration> {
        let registered = self
            .registry
            .try_activate_generation(
                room_id,
                media_id,
                &self.local_node_id,
                EXTERNAL_PUBLISHER_USER_ID,
                &self.local_cluster_address,
                &generation_id.to_string(),
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
            .load_local_external_publisher(room_id, media_id, &generation_id.to_string())
            .await?
        else {
            return Err(crate::error::StreamError::RegistryError(format!(
                "Publisher registration for {room_id}/{media_id} could not be read back"
            )));
        };

        Ok(ExternalRegistration::new(true, Some(publisher.lease_epoch)))
    }

    async fn load_local_external_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
    ) -> StreamResult<Option<crate::relay::StreamGeneration>> {
        let publisher = self
            .registry
            .get_active_generation(room_id, media_id)
            .await
            .map_err(|e| {
                crate::error::StreamError::RegistryError(format!(
                    "Failed to load publisher registration: {e}"
                ))
            })?;

        Ok(publisher.filter(|info| {
            info.node_id == self.local_node_id
                && info.user_id == EXTERNAL_PUBLISHER_USER_ID
                && info.generation_id == generation_id
        }))
    }

    async fn ensure_external_registration(
        &self,
        room_id: &str,
        media_id: &str,
        stream: &Arc<ExternalPublishStream>,
    ) -> StreamResult<()> {
        let current_epoch = stream.registration_lease_epoch();
        if let Some(lease_epoch) = current_epoch {
            match self
                .registry
                .refresh_generation_lease(
                    room_id,
                    media_id,
                    &stream.generation_id.to_string(),
                    EXTERNAL_PUBLISHER_USER_ID,
                    &self.local_node_id,
                    lease_epoch,
                )
                .await
            {
                Ok(crate::relay::LeaseRefreshOutcome::Refreshed) => return Ok(()),
                Ok(crate::relay::LeaseRefreshOutcome::Missing) => {
                    warn!(
                        room_id,
                        media_id,
                        lease_epoch,
                        "External publisher registration is missing; restoring it"
                    );
                }
                Ok(crate::relay::LeaseRefreshOutcome::OwnershipChanged) => {
                    warn!(
                        room_id,
                        media_id,
                        lease_epoch,
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
            .load_local_external_publisher(room_id, media_id, &stream.generation_id.to_string())
            .await?
        {
            stream.set_registration_lease_epoch(Some(existing.lease_epoch));
            return Ok(());
        }

        let registration = self
            .register_external_publisher(room_id, media_id, stream.generation_id)
            .await?;
        if registration.registered {
            stream.set_registration_lease_epoch(registration.lease_epoch);
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

                let Some(lease_epoch) = stream.registration_lease_epoch() else {
                    continue;
                };

                match registry
                    .refresh_generation_lease(
                        &stream.room_id,
                        &stream.media_id,
                        &stream.generation_id.to_string(),
                        EXTERNAL_PUBLISHER_USER_ID,
                        &local_node_id,
                        lease_epoch,
                    )
                    .await
                {
                    Ok(crate::relay::LeaseRefreshOutcome::Refreshed) => {}
                    Ok(crate::relay::LeaseRefreshOutcome::Missing) => {
                        warn!(
                            room_id = %stream.room_id,
                            media_id = %stream.media_id,
                            lease_epoch,
                            "External publisher registration disappeared during heartbeat"
                        );
                        stream.set_registration_lease_epoch(None);
                        if let Err(error) = stream.stop().await {
                            warn!(
                                room_id = %stream.room_id,
                                media_id = %stream.media_id,
                                %error,
                                "Failed to stop external publisher after registry ownership loss"
                            );
                        }
                        break;
                    }
                    Ok(crate::relay::LeaseRefreshOutcome::OwnershipChanged) => {
                        warn!(
                            room_id = %stream.room_id,
                            media_id = %stream.media_id,
                            lease_epoch,
                            "External publisher registration ownership changed during heartbeat"
                        );
                        stream.set_registration_lease_epoch(None);
                        if let Err(error) = stream.stop().await {
                            warn!(
                                room_id = %stream.room_id,
                                media_id = %stream.media_id,
                                %error,
                                "Failed to stop external publisher after ownership change"
                            );
                        }
                        break;
                    }
                    Err(error) => {
                        warn!(
                            room_id = %stream.room_id,
                            media_id = %stream.media_id,
                            lease_epoch,
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
            .get_active_generation(room_id, media_id)
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
            .deactivate_generation_if_lease_matches(
                room_id,
                media_id,
                &existing.generation_id,
                existing.lease_epoch,
            )
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
/// Pulls from an external RTMP, RTSP, or HTTP-FLV source and publishes frames into
/// the local `StreamHub` under `live/{room_id}/{media_id}`.
pub(crate) struct ExternalPublishStream {
    room_id: String,
    media_id: String,
    source: synctv_core::models::ExternalLiveSourceConfig,
    stream_hub_event_sender: StreamHubEventSender,
    lifecycle: StreamLifecycle,
    generation_id: synctv_xiu::streamhub::utils::Uuid,
    /// Redis publisher lease_epoch for the system-owned external puller.
    registration_lease_epoch: std::sync::atomic::AtomicU64,
    /// Shared HTTP client for FLV connections (supports TLS via rustls).
    http_client: reqwest::Client,
    ssrf_guard: SsrfGuard,
    max_flv_tag_size_bytes: usize,
}

struct ExternalPublishStreamParams {
    room_id: String,
    media_id: String,
    source: synctv_core::models::ExternalLiveSourceConfig,
    stream_hub_event_sender: StreamHubEventSender,
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
    fn new(params: ExternalPublishStreamParams) -> Self {
        let ExternalPublishStreamParams {
            room_id,
            media_id,
            source,
            stream_hub_event_sender,
            http_client,
            ssrf_guard,
            max_flv_tag_size_bytes,
        } = params;
        Self {
            room_id,
            media_id,
            source,
            stream_hub_event_sender,
            lifecycle: StreamLifecycle::new(),
            generation_id: synctv_xiu::streamhub::utils::Uuid::new(),
            registration_lease_epoch: std::sync::atomic::AtomicU64::new(0),
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
        let source = self.source.clone();
        let stream_hub_sender = self.stream_hub_event_sender.clone();
        let http_client = self.http_client.clone();
        let ssrf_guard = self.ssrf_guard.clone();
        let max_flv_tag_size_bytes = self.max_flv_tag_size_bytes;
        let generation_id = self.generation_id;
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
                source,
                stream_hub_sender,
                ssrf_guard,
            )
            .await
            {
                Ok(p) => p
                    .with_generation_id(generation_id)
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
    /// The puller's generation-aware drop guard owns StreamHub cleanup, including
    /// cancellation while this task is being aborted.
    async fn stop(&self) -> StreamResult<()> {
        self.lifecycle.mark_stopping();
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

    fn set_registration_lease_epoch(&self, lease_epoch: Option<u64>) {
        self.registration_lease_epoch
            .store(lease_epoch.unwrap_or(0), Ordering::Release);
    }

    fn registration_lease_epoch(&self) -> Option<u64> {
        match self.registration_lease_epoch.load(Ordering::Acquire) {
            0 => None,
            lease_epoch => Some(lease_epoch),
        }
    }
}

impl Drop for ExternalPublishStream {
    fn drop(&mut self) {
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
    use crate::util::TEST_GENERATION_ID;
    use std::future::pending;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    fn rtmp_source(url: &str) -> synctv_core::models::ExternalLiveSourceConfig {
        synctv_core::models::ExternalLiveSourceConfig::Rtmp {
            url: url.to_string(),
            mode: synctv_core::models::RtmpStreamMode::Default,
        }
    }

    fn http_flv_source(url: &str) -> synctv_core::models::ExternalLiveSourceConfig {
        synctv_core::models::ExternalLiveSourceConfig::HttpFlv {
            url: url.to_string(),
        }
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

        let source = http_flv_source("http://127.0.0.1:8080/live.flv");
        let Err(error) = manager.get_or_create("room1", "media1", &source).await else {
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
    async fn startup_failure_does_not_leave_pool_or_registry_ghost() -> TestResult {
        // A source can accept the HTTP request and still fail before yielding a
        // valid FLV header. The manager must unwind the local StreamHub
        // publication and leave no managed stream or Redis ownership behind.
        use synctv_xiu::streamhub::define::{FrameDataSender, StreamHubEvent};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::TcpListener;

        let source_listener = TcpListener::bind("127.0.0.1:0").await?;
        let source_address = source_listener.local_addr()?;
        let source_task = tokio::spawn(async move {
            let Ok((mut socket, _)) = source_listener.accept().await else {
                return;
            };
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nbroken-flv!!",
                )
                .await;
        });

        let registry = Arc::new(TestStreamRegistry::new());
        let (event_sender, mut event_receiver) = tokio::sync::mpsc::channel(32);
        let (unpublish_tx, unpublish_rx) = tokio::sync::oneshot::channel();
        let event_task = tokio::spawn(async move {
            let mut unpublish_tx = Some(unpublish_tx);
            while let Some(event) = event_receiver.recv().await {
                if let StreamHubEvent::Publish { result_sender, .. } = event {
                    let (data_sender, _data_receiver) = tokio::sync::mpsc::channel(4);
                    let _ = result_sender.send(Ok((
                        Some(FrameDataSender::bounded(data_sender)),
                        None,
                        None,
                    )));
                } else if matches!(event, StreamHubEvent::UnPublish { .. }) {
                    if let Some(sender) = unpublish_tx.take() {
                        let _ = sender.send(());
                    }
                }
            }
        });

        let manager = ExternalPublishManager::with_timeouts(
            registry.clone(),
            "node-1".to_string(),
            "127.0.0.1:50051".to_string(),
            event_sender,
            SsrfGuard::disabled(),
            1,
            300,
        )?;
        let source_url = format!("http://{source_address}/stream.flv");
        let source = http_flv_source(&source_url);
        let Err(error) = manager
            .get_or_create("room-startup-failure", "media-startup-failure", &source)
            .await
        else {
            return Err(test_error(
                "invalid FLV startup should fail the viewer request",
            ));
        };
        assert!(
            error.to_string().contains("FLV"),
            "unexpected error: {error}"
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), unpublish_rx)
            .await
            .map_err(|_| test_error("failed external startup must unpublish local stream"))??;
        assert!(manager.pool.streams.is_empty());
        assert!(registry
            .get_active_generation("room-startup-failure", "media-startup-failure")
            .await?
            .is_none());

        source_task.await?;
        event_task.abort();
        let _ = event_task.await;
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
            .try_activate_generation(
                "room1",
                "media1",
                "node-1",
                "",
                "127.0.0.1:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        registry
            .try_activate_generation(
                "room1",
                "media2",
                "node-1",
                "user-1",
                "127.0.0.1:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        registry
            .try_activate_generation(
                "room1",
                "media3",
                "node-2",
                "",
                "127.0.0.2:50051",
                TEST_GENERATION_ID,
            )
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
            registry
                .get_active_generation("room1", "media1")
                .await?
                .is_none(),
            "same-node system-owned external publisher registration should be cleared"
        );
        assert!(
            !manager
                .clear_stale_local_external_registration("room1", "media2")
                .await?
        );
        assert!(
            registry
                .get_active_generation("room1", "media2")
                .await?
                .is_some(),
            "same-node user-owned RTMP publisher registration should be preserved"
        );
        assert!(
            !manager
                .clear_stale_local_external_registration("room1", "media3")
                .await?
        );
        assert!(
            registry
                .get_active_generation("room1", "media3")
                .await?
                .is_some(),
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

        let stream = Arc::new(ExternalPublishStream::new(ExternalPublishStreamParams {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            source: rtmp_source("rtmp://example.com/live/stream"),
            stream_hub_event_sender: sender,
            http_client: reqwest::Client::new(),
            ssrf_guard: SsrfGuard::disabled(),
            max_flv_tag_size_bytes: ExternalStreamPuller::DEFAULT_MAX_FLV_TAG_SIZE_BYTES,
        }));
        stream.lifecycle().set_running();

        let registered = manager
            .register_external_publisher("room1", "media1", stream.generation_id)
            .await?;
        assert!(registered.registered);
        stream.set_registration_lease_epoch(registered.lease_epoch);
        registry
            .deactivate_current_generation("room1", "media1")
            .await?;

        manager.pool.insert_and_cleanup(
            "room1:media1".to_string(),
            stream.clone(),
            |_stream_key| Box::pin(async {}),
        );

        let source = rtmp_source("rtmp://example.com/live/stream");
        let reused = manager.get_or_create("room1", "media1", &source).await?;

        assert!(Arc::ptr_eq(&reused, &stream));
        let restored = registry
            .get_active_generation("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher registration should be restored"))?;
        assert_eq!(restored.node_id, "node-1");
        assert_eq!(restored.user_id, "");
        assert_eq!(
            stream.registration_lease_epoch(),
            Some(restored.lease_epoch)
        );
        assert_eq!(
            stream.lifecycle().subscriber_count(),
            1,
            "reuse path must keep exactly one subscriber for the caller"
        );
        reused.decrement_subscriber_count();

        let subscribed = manager
            .subscribe_existing("room1", "media1", &source)
            .await?
            .ok_or_else(|| anyhow::anyhow!("local external stream should remain subscribable"))?;
        assert!(Arc::ptr_eq(&subscribed, &stream));
        assert_eq!(
            stream.lifecycle().subscriber_count(),
            1,
            "local publisher reuse must refresh the managed subscriber lifecycle"
        );
        subscribed.decrement_subscriber_count();
        assert_eq!(stream.lifecycle().subscriber_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn hls_generation_polling_extends_external_stream_lifetime() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        let (sender, _) = tokio::sync::mpsc::channel(8);
        let manager = ExternalPublishManager::with_timeouts(
            registry,
            "node-1".to_string(),
            "127.0.0.1:50051".to_string(),
            sender,
            SsrfGuard::disabled(),
            1,
            2,
        )?;
        let source = http_flv_source("http://127.0.0.1/live.flv");
        let (stream, generation_id) =
            manager.install_running_test_stream("room1", "media1", source);
        let generation_id = generation_id.to_string();

        for _ in 0..6 {
            let guard = manager
                .subscribe_active_generation("room1", "media1", &generation_id)
                .await
                .ok_or_else(|| anyhow::anyhow!("active generation should remain subscribable"))?;
            assert_eq!(stream.lifecycle().subscriber_count(), 1);
            drop(guard);
            assert_eq!(stream.lifecycle().subscriber_count(), 0);
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        assert!(manager.pool.streams.contains_key("room1:media1"));

        tokio::time::timeout(Duration::from_secs(5), async {
            while manager.pool.streams.contains_key("room1:media1") {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await?;
        assert!(!stream.lifecycle().is_healthy().await);
        Ok(())
    }

    #[tokio::test]
    async fn stop_all_does_not_unregister_same_node_replacement_generation() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        let (sender, _) = tokio::sync::mpsc::channel(8);
        let manager = ExternalPublishManager::with_timeouts(
            registry.clone(),
            "node-1".to_string(),
            "127.0.0.1:50051".to_string(),
            sender,
            SsrfGuard::disabled(),
            60,
            300,
        )?;
        let (stream, generation_id) = manager.install_running_test_stream(
            "room1",
            "media1",
            http_flv_source("http://127.0.0.1/live.flv"),
        );
        assert!(
            registry
                .try_activate_generation(
                    "room1",
                    "media1",
                    "node-1",
                    "",
                    "127.0.0.1:50051",
                    &generation_id.to_string(),
                )
                .await?
        );
        let old_epoch = registry
            .get_active_generation("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("old publisher should exist"))?
            .lease_epoch;
        stream.set_registration_lease_epoch(Some(old_epoch));
        registry
            .deactivate_current_generation("room1", "media1")
            .await?;

        let replacement_id = synctv_xiu::streamhub::utils::Uuid::new();
        assert!(
            registry
                .try_activate_generation(
                    "room1",
                    "media1",
                    "node-1",
                    "",
                    "127.0.0.1:50051",
                    &replacement_id.to_string(),
                )
                .await?
        );

        manager.stop_all().await;

        let current = registry
            .get_active_generation("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("replacement publisher should remain"))?;
        assert_eq!(current.generation_id, replacement_id.to_string());
        Ok(())
    }

    #[tokio::test]
    async fn stop_all_preserves_matching_external_hls_route() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        let (sender, _) = tokio::sync::mpsc::channel(8);
        let manager = ExternalPublishManager::with_timeouts(
            registry.clone(),
            "node-1".to_string(),
            "127.0.0.1:50051".to_string(),
            sender,
            SsrfGuard::disabled(),
            60,
            300,
        )?;
        let (stream, generation_id) = manager.install_running_test_stream(
            "room1",
            "media1",
            http_flv_source("http://127.0.0.1/live.flv"),
        );
        assert!(
            registry
                .try_activate_generation(
                    "room1",
                    "media1",
                    "node-1",
                    "",
                    "127.0.0.1:50051",
                    &generation_id.to_string(),
                )
                .await?
        );
        let lease_epoch = registry
            .get_active_generation("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher should exist"))?
            .lease_epoch;
        stream.set_registration_lease_epoch(Some(lease_epoch));

        manager.stop_all().await;

        assert!(registry
            .get_active_generation("room1", "media1")
            .await?
            .is_none());
        let ended = registry
            .get_generation("room1", "media1", &generation_id.to_string())
            .await?
            .ok_or_else(|| anyhow::anyhow!("matching ended HLS route should remain"))?;
        assert_eq!(ended.lease_epoch, lease_epoch);
        Ok(())
    }

    #[tokio::test]
    async fn existing_pool_stream_cannot_adopt_replacement_generation() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        let (sender, _) = tokio::sync::mpsc::channel(8);
        let manager = ExternalPublishManager::with_timeouts(
            registry.clone(),
            "node-1".to_string(),
            "127.0.0.1:50051".to_string(),
            sender,
            SsrfGuard::disabled(),
            60,
            300,
        )?;
        let source = http_flv_source("http://127.0.0.1/live.flv");
        let (stream, old_generation_id) =
            manager.install_running_test_stream("room1", "media1", source.clone());
        let replacement_id = synctv_xiu::streamhub::utils::Uuid::new();
        assert_ne!(old_generation_id, replacement_id);
        assert!(
            registry
                .try_activate_generation(
                    "room1",
                    "media1",
                    "node-1",
                    "",
                    "127.0.0.1:50051",
                    &replacement_id.to_string(),
                )
                .await?
        );

        let Err(error) = manager.subscribe_existing("room1", "media1", &source).await else {
            return Err(anyhow::anyhow!(
                "old pool stream must reject replacement registry generation"
            )
            .into());
        };
        assert!(error
            .to_string()
            .contains("Another publisher already registered"));
        assert_eq!(stream.lifecycle().subscriber_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_external_publish_stream_subscriber_count() {
        let (sender, _) = tokio::sync::mpsc::channel(64);
        let stream = ExternalPublishStream::new(ExternalPublishStreamParams {
            room_id: "room-1".to_string(),
            media_id: "media-1".to_string(),
            source: rtmp_source("rtmp://example.com/live/stream"),
            stream_hub_event_sender: sender,
            http_client: reqwest::Client::new(),
            ssrf_guard: SsrfGuard::disabled(),
            max_flv_tag_size_bytes: ExternalStreamPuller::DEFAULT_MAX_FLV_TAG_SIZE_BYTES,
        });

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
