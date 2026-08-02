// Live streaming API helpers used by synctv-api HTTP endpoints.

use crate::livestream::server::HlsStorageBackend;
use crate::{
    error::StreamError,
    grpc::{HlsProxyClient, StreamRelayServiceImpl},
    livestream::{
        external_publish_manager::ExternalPublishManager, managed_stream::ManagedStream,
        pull_manager::PullStreamManager, SegmentManager,
    },
    relay::{ActiveStreamGeneration, StreamGeneration, StreamRegistryTrait},
};
use anyhow::{Context, Result};
use bytes::Bytes;
use std::collections::BTreeSet;
use std::sync::Arc;
use synctv_common::ssrf::SsrfGuard;
use synctv_xiu::hls::remuxer::StreamRegistry as HlsStreamRegistry;
use synctv_xiu::httpflv::HttpFlvSession;
use synctv_xiu::streamhub::{
    define::{StreamHubEvent, StreamHubEventSender},
    send_event_with_backpressure_timeout_for,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

pub use super::tracker::{StreamSubscriberGuard, StreamTracker};

const KICK_PUBLISHER_EVENT_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const HLS_GENERATION_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const HLS_GENERATION_READY_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(25);

pub(crate) async fn find_hls_generation_state(
    hls_registry: &HlsStreamRegistry,
    room_id: &str,
    media_id: &str,
    generation_id: &str,
    wait_for_ready: bool,
) -> Option<Arc<parking_lot::RwLock<synctv_xiu::hls::StreamProcessorState>>> {
    let stream_key = synctv_xiu::hls::generation_registry_key(room_id, media_id, generation_id);
    let deadline = tokio::time::Instant::now() + HLS_GENERATION_READY_TIMEOUT;

    loop {
        if let Some(state) = hls_registry
            .get(&stream_key)
            .map(|entry| Arc::clone(entry.value()))
        {
            return Some(state);
        }
        if !wait_for_ready || tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(HLS_GENERATION_READY_POLL_INTERVAL).await;
    }
}

#[derive(Clone)]
pub struct LiveStreamingInfrastructure {
    /// Registry for finding publishers (Redis)
    pub(crate) registry: Arc<dyn StreamRegistryTrait>,
    /// `StreamHub` event sender for subscribing to streams
    pub(crate) stream_hub_event_sender: StreamHubEventSender,
    /// Pull stream manager for gRPC relay (cross-node pull)
    pub(crate) pull_manager: Arc<PullStreamManager>,
    /// External publish manager for RTMP, RTSP, and HTTP-FLV pull-to-publish streams.
    pub(crate) external_publish_manager: Arc<ExternalPublishManager>,
    /// Segment manager for HLS storage
    pub(crate) segment_manager: Option<Arc<SegmentManager>>,
    /// HLS stream registry for M3U8 generation
    pub(crate) hls_stream_registry: Option<HlsStreamRegistry>,
    /// Tracks active RTMP publishers by `user_id` for kick-on-ban
    pub(crate) user_stream_tracker: Arc<StreamTracker>,
    /// Local node ID for comparing with publisher node
    pub(crate) local_node_id: String,
    /// HLS segment storage backend.
    pub(crate) hls_storage_backend: HlsStorageBackend,
    /// HLS proxy client for fetching playlists/segments from remote publisher nodes
    pub(crate) hls_proxy: Option<HlsProxyClient>,
}

impl LiveStreamingInfrastructure {
    pub fn new(
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        user_stream_tracker: Arc<StreamTracker>,
        local_node_id: String,
        ssrf_guard: SsrfGuard,
    ) -> crate::error::StreamResult<Self> {
        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            stream_hub_event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(ExternalPublishManager::new(
            registry.clone(),
            local_node_id.clone(),
            stream_hub_event_sender.clone(),
            ssrf_guard,
        )?);

        Ok(Self::from_parts(
            registry,
            stream_hub_event_sender,
            pull_manager,
            external_publish_manager,
            user_stream_tracker,
            local_node_id,
        ))
    }

    /// Create infrastructure from preconfigured internal managers.
    pub(crate) fn from_parts(
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        pull_manager: Arc<PullStreamManager>,
        external_publish_manager: Arc<ExternalPublishManager>,
        user_stream_tracker: Arc<StreamTracker>,
        local_node_id: String,
    ) -> Self {
        Self {
            registry,
            stream_hub_event_sender,
            pull_manager,
            external_publish_manager,
            segment_manager: None,
            hls_stream_registry: None,
            user_stream_tracker,
            local_node_id,
            hls_storage_backend: HlsStorageBackend::Memory,
            hls_proxy: None,
        }
    }

    #[must_use]
    pub(crate) fn with_segment_manager(mut self, segment_manager: Arc<SegmentManager>) -> Self {
        self.segment_manager = Some(segment_manager);
        self
    }

    #[must_use]
    pub(crate) fn with_hls_stream_registry(
        mut self,
        hls_stream_registry: HlsStreamRegistry,
    ) -> Self {
        self.hls_stream_registry = Some(hls_stream_registry);
        self
    }

    #[must_use]
    pub(crate) fn with_hls_storage_backend(mut self, backend: HlsStorageBackend) -> Self {
        self.hls_storage_backend = backend;
        self
    }

    #[must_use]
    pub(crate) fn with_hls_proxy(mut self, hls_proxy: HlsProxyClient) -> Self {
        self.hls_proxy = Some(hls_proxy);
        self
    }

    /// Enqueue a local `UnPublish` event for an RTMP publisher.
    pub async fn kick_publisher(&self, room_id: &str, media_id: &str) -> Result<()> {
        use synctv_xiu::streamhub::stream::StreamIdentifier;

        // StreamHub uses canonical (room_id, media_id) identifiers after auth rewrite
        let identifier = StreamIdentifier::Rtmp {
            app_name: room_id.to_string(),
            stream_name: media_id.to_string(),
        };

        send_event_with_backpressure_timeout_for(
            &self.stream_hub_event_sender,
            StreamHubEvent::ForceUnPublish { identifier },
            KICK_PUBLISHER_EVENT_SEND_TIMEOUT,
        )
        .await
        .map_err(|_| anyhow::anyhow!("Failed to send unpublish event (StreamHub not running)"))?;

        Ok(())
    }

    fn is_local_publisher_node(&self, node_id: &str) -> bool {
        self.local_node_id.is_empty() || node_id == self.local_node_id
    }

    async fn registry_stream_owned_by_local_node(&self, room_id: &str, media_id: &str) -> bool {
        match self.registry.get_active_generation(room_id, media_id).await {
            Ok(Some(publisher)) => self.is_local_publisher_node(&publisher.node_id),
            Ok(None) => false,
            Err(error) => {
                warn!(
                    room_id = %room_id,
                    media_id = %media_id,
                    error = %error,
                    "Failed to load publisher owner from shared registry"
                );
                false
            }
        }
    }

    pub async fn stream_is_remote(&self, room_id: &str, media_id: &str) -> Result<bool> {
        self.registry
            .get_active_generation(room_id, media_id)
            .await
            .map(|publisher| {
                publisher.is_some_and(|publisher| !self.is_local_publisher_node(&publisher.node_id))
            })
            .map_err(|error| anyhow::anyhow!("Failed to load publisher owner: {error}"))
    }

    /// Common error handler for registry queries.
    fn handle_registry_error<T>(error: &anyhow::Error, context: &str) -> Vec<T> {
        warn!(
            error = %error,
            context = context,
            "Failed to load publishers from shared registry"
        );
        Vec::new()
    }

    async fn filter_local_registry_streams(
        &self,
        registry_streams: Vec<(String, String)>,
    ) -> Vec<(String, String)> {
        let mut local_streams = Vec::with_capacity(registry_streams.len());
        for (room_id, media_id) in registry_streams {
            if self
                .registry_stream_owned_by_local_node(&room_id, &media_id)
                .await
            {
                local_streams.push((room_id, media_id));
            }
        }
        local_streams
    }

    async fn local_registry_user_publishers(&self, user_id: &str) -> Vec<(String, String)> {
        match self.registry.get_user_publishers(user_id).await {
            Ok(registry_streams) => self.filter_local_registry_streams(registry_streams).await,
            Err(error) => Self::handle_registry_error(&error, &format!("user_id={user_id}")),
        }
    }

    async fn local_registry_user_room_publishers(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Vec<(String, String)> {
        match self
            .registry
            .get_user_publishers_for_room(room_id, user_id)
            .await
        {
            Ok(registry_streams) => self.filter_local_registry_streams(registry_streams).await,
            Err(error) => Self::handle_registry_error(
                &error,
                &format!("room_id={room_id}, user_id={user_id}"),
            ),
        }
    }

    async fn local_registry_room_publishers(&self, room_id: &str) -> Vec<String> {
        match self.registry.list_streams_for_room(room_id).await {
            Ok(registry_media_ids) => {
                let mut local_media_ids = Vec::new();
                for media_id in registry_media_ids {
                    if self
                        .registry_stream_owned_by_local_node(room_id, &media_id)
                        .await
                    {
                        local_media_ids.push(media_id);
                    }
                }
                local_media_ids
            }
            Err(error) => Self::handle_registry_error(&error, &format!("room_id={room_id}")),
        }
    }

    pub async fn kick_user_publishers(&self, user_id: &str) {
        let mut streams: BTreeSet<_> = self
            .user_stream_tracker
            .get_user_streams(user_id)
            .into_iter()
            .collect();
        streams.extend(self.local_registry_user_publishers(user_id).await);

        for (room_id, media_id) in streams {
            info!(
                user_id = %user_id,
                room_id = %room_id,
                media_id = %media_id,
                "Kicking RTMP publisher for banned user"
            );
            if let Err(e) = self.kick_stream(&room_id, &media_id).await {
                error!("Failed to kick publisher for user {}: {}", user_id, e);
            }
        }
    }

    pub async fn kick_user_room_publishers(&self, room_id: &str, user_id: &str) {
        let mut streams: BTreeSet<_> = self
            .user_stream_tracker
            .get_user_streams(user_id)
            .into_iter()
            .filter(|(stream_room_id, _)| stream_room_id == room_id)
            .collect();
        streams.extend(
            self.local_registry_user_room_publishers(room_id, user_id)
                .await,
        );

        for (_, media_id) in streams {
            info!(
                user_id = %user_id,
                room_id = %room_id,
                media_id = %media_id,
                "Kicking RTMP publisher for user removed from room"
            );
            if let Err(e) = self.kick_stream(room_id, &media_id).await {
                error!(
                    user_id = %user_id,
                    room_id = %room_id,
                    media_id = %media_id,
                    error = %e,
                    "Failed to kick room-scoped publisher"
                );
            }
        }
    }

    pub async fn kick_room_publishers(&self, room_id: &str) {
        let mut media_ids: BTreeSet<_> = self
            .user_stream_tracker
            .get_room_streams(room_id)
            .into_iter()
            .collect();
        media_ids.extend(self.local_registry_room_publishers(room_id).await);

        for media_id in media_ids {
            let user_id = self.user_stream_tracker.get_stream_user(room_id, &media_id);
            if let Some(user_id) = user_id.as_ref() {
                info!(
                    user_id = %user_id,
                    room_id = %room_id,
                    media_id = %media_id,
                    "Kicking RTMP publisher for banned room"
                );
            }
            if let Err(e) = self.kick_stream(room_id, &media_id).await {
                error!("Failed to kick publisher in room {}: {}", room_id, e);
            }
        }
    }

    /// Publisher ownership is removed later by the RTMP auth/PublisherManager
    /// unpublish path, which fences cleanup against the publisher lease_epoch. Do not
    /// unregister here: this method only enqueues the control event, and deleting
    /// ownership before StreamHub processes it can let a replacement publisher
    /// register and then be torn down by the delayed unpublish.
    pub async fn kick_stream(&self, room_id: &str, media_id: &str) -> Result<()> {
        if let Some(publisher) = self
            .registry
            .get_active_generation(room_id, media_id)
            .await?
        {
            if !self.is_local_publisher_node(&publisher.node_id) {
                warn!(
                    room_id = %room_id,
                    media_id = %media_id,
                    publisher_node_id = %publisher.node_id,
                    local_node_id = %self.local_node_id,
                    "Skipping non-local publisher kick on this replica"
                );
                return Ok(());
            }
        }

        // Send UnPublish to StreamHub
        self.kick_publisher(room_id, media_id).await?;

        Ok(())
    }

    /// Returns a [`StreamSubscriberGuard`] that decrements the subscriber count
    /// when dropped. For FLV, hold it in the streaming task; for HLS, let it
    /// drop at the end of the request (the `last_active_time` touch keeps the
    /// stream alive across polling intervals).
    pub async fn ensure_pull_stream(
        &self,
        room_id: &str,
        media_id: &str,
        external_source: Option<&synctv_core::models::ExternalLiveSourceConfig>,
    ) -> Result<StreamSubscriberGuard> {
        self.ensure_pull_stream_internal(room_id, media_id, external_source)
            .await
    }

    pub async fn ensure_external_pull_stream(
        &self,
        room_id: &str,
        media_id: &str,
        source_config: &synctv_core::models::LiveProxyMediaSourceConfig,
    ) -> Result<StreamSubscriberGuard> {
        self.ensure_pull_stream_internal(room_id, media_id, Some(&source_config.source))
            .await
    }

    async fn ensure_pull_stream_internal(
        &self,
        room_id: &str,
        media_id: &str,
        external_source: Option<&synctv_core::models::ExternalLiveSourceConfig>,
    ) -> Result<StreamSubscriberGuard> {
        // Check Redis for an existing publisher
        let publisher = self
            .registry
            .get_active_generation(room_id, media_id)
            .await
            .map_err(|e| StreamError::RegistryError(format!("Failed to check publisher: {e}")))?;

        if let Some(publisher_info) = publisher {
            let is_local = self.is_local_publisher_node(&publisher_info.node_id);

            if is_local {
                match self
                    .registry
                    .validate_lease(
                        room_id,
                        media_id,
                        &publisher_info.generation_id,
                        publisher_info.lease_epoch,
                    )
                    .await
                {
                    Ok(true) => {
                        tracing::debug!(
                            "Epoch {} validated for local publisher {}/{}",
                            publisher_info.lease_epoch,
                            room_id,
                            media_id
                        );
                        if let Some(source) = external_source {
                            if let Some(stream) = self
                                .external_publish_manager
                                .subscribe_existing(room_id, media_id, source)
                                .await
                                .context("Failed to subscribe to local external stream")?
                            {
                                return Ok(StreamSubscriberGuard::new(move || {
                                    stream.decrement_subscriber_count();
                                }));
                            }

                            // Redis can outlive a local process or pool entry.
                            // Re-enter the manager's single-flight creation path
                            // so the source is restored or replaced atomically.
                            let stream = self
                                .external_publish_manager
                                .get_or_create(room_id, media_id, source)
                                .await
                                .context("Failed to restore local external stream")?;
                            return Ok(StreamSubscriberGuard::new(move || {
                                stream.decrement_subscriber_count();
                            }));
                        }
                        return Ok(StreamSubscriberGuard::new(|| {}));
                    }
                    Ok(false) => {
                        warn!(
                            "Epoch {} is stale for local publisher {}/{}, publisher may have changed",
                            publisher_info.lease_epoch, room_id, media_id
                        );
                        return Err(anyhow::anyhow!(
                            "Stale lease_epoch {} for local publisher {}/{}",
                            publisher_info.lease_epoch,
                            room_id,
                            media_id
                        ));
                    }
                    Err(e) => {
                        error!(
                            "Failed to validate lease_epoch for local publisher {}/{}: {}. \
                             Rejecting to prevent potential split-brain.",
                            room_id, media_id, e
                        );
                        return Err(anyhow::anyhow!(
                            "Epoch validation failed for local publisher {room_id}/{media_id}: {e}"
                        ));
                    }
                }
            }

            let stream = self
                .pull_manager
                .get_or_create_pull_stream(room_id, media_id)
                .await
                .context("Failed to create pull stream")?;
            let guard = StreamSubscriberGuard::new(move || {
                stream.lifecycle().decrement_subscriber_count();
            });
            return Ok(guard);
        }

        if let Some(source) = external_source {
            let stream = self
                .external_publish_manager
                .get_or_create(room_id, media_id, source)
                .await
                .context("Failed to create external publish stream")?;
            let guard = StreamSubscriberGuard::new(move || stream.decrement_subscriber_count());
            return Ok(guard);
        }

        Err(anyhow::anyhow!(
            "No publisher found for {room_id}/{media_id}"
        ))
    }

    pub fn local_room_streams(&self, room_id: &str) -> Vec<String> {
        self.user_stream_tracker.get_room_streams(room_id)
    }

    pub fn local_user_streams(&self, user_id: &str) -> Vec<(String, String)> {
        self.user_stream_tracker.get_user_streams(user_id)
    }

    /// Build the authenticated cross-node relay service for the current infrastructure.
    #[must_use]
    pub fn relay_service(
        &self,
        node_id: String,
        cluster_secret: String,
        cancel_token: CancellationToken,
    ) -> StreamRelayServiceImpl {
        let relay_service = StreamRelayServiceImpl::new(
            self.registry.clone(),
            node_id,
            self.stream_hub_event_sender.clone(),
            cancel_token,
        )
        .with_cluster_secret(cluster_secret)
        .with_external_publish_manager(Arc::clone(&self.external_publish_manager));

        let relay_service = if let Some(segment_manager) = &self.segment_manager {
            relay_service.with_segment_manager(segment_manager.clone())
        } else {
            relay_service
        };

        if let Some(hls_stream_registry) = &self.hls_stream_registry {
            relay_service.with_hls_stream_registry(hls_stream_registry.clone())
        } else {
            relay_service
        }
    }

    pub async fn find_publisher(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<Option<StreamGeneration>> {
        self.registry
            .get_active_generation(room_id, media_id)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get publisher: {e}"))
    }

    pub async fn is_stream_active(&self, room_id: &str, media_id: &str) -> Result<bool> {
        self.registry
            .is_stream_active(room_id, media_id)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to check active stream: {e}"))
    }

    pub async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        self.registry
            .list_streams_for_room(room_id)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list room streams: {e}"))
    }

    pub async fn list_active_generations(&self) -> Result<Vec<ActiveStreamGeneration>> {
        self.registry
            .list_active_generations()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list active streams: {e}"))
    }

    pub async fn cleanup_local_publishers(&self, timeout: std::time::Duration) -> bool {
        if self.local_node_id.is_empty() {
            self.user_stream_tracker.clear();
            return true;
        }

        if timeout.is_zero() {
            warn!(
                node_id = %self.local_node_id,
                "Skipping local publisher cleanup before livestream shutdown because no shutdown budget remains"
            );
            self.user_stream_tracker.clear();
            return false;
        }

        let cleanup_timeout = timeout.min(std::time::Duration::from_secs(2));
        let cleanup_result = tokio::time::timeout(
            cleanup_timeout,
            self.registry
                .deactivate_all_generations_for_node_preserving_hls(&self.local_node_id),
        )
        .await;

        match cleanup_result {
            Ok(Ok(())) => {
                info!(
                    node_id = %self.local_node_id,
                    "Cleaned up local publisher registrations before livestream shutdown"
                );
                self.user_stream_tracker.clear();
                true
            }
            Ok(Err(error)) => {
                warn!(
                    node_id = %self.local_node_id,
                    error = %error,
                    "Failed to cleanup local publisher registrations before livestream shutdown"
                );
                self.user_stream_tracker.clear();
                false
            }
            Err(_) => {
                warn!(
                    node_id = %self.local_node_id,
                    "Timed out cleaning local publisher registrations before livestream shutdown"
                );
                self.user_stream_tracker.clear();
                false
            }
        }
    }
}

pub struct FlvStreamingApi;

impl FlvStreamingApi {
    async fn create_session(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
    ) -> Result<mpsc::Receiver<Result<Bytes, std::io::Error>>> {
        infrastructure
            .find_publisher(room_id, media_id)
            .await?
            .is_some()
            .then_some(())
            .ok_or_else(|| StreamError::NoPublisher(format!("{room_id}/{media_id}")))?;

        let (tx, rx) = mpsc::channel(synctv_xiu::httpflv::FLV_RESPONSE_CHANNEL_CAPACITY);

        let mut flv_session = HttpFlvSession::new(
            room_id.to_string(),
            media_id.to_string(),
            infrastructure.stream_hub_event_sender.clone(),
            tx,
        );

        tokio::spawn(async move {
            if let Err(e) = flv_session.run().await {
                error!("FLV session error: {}", e);
            }
        });

        Ok(rx)
    }

    /// Returns `(receiver, guard)`. The caller **must** hold the
    /// [`StreamSubscriberGuard`] for the lifetime of the FLV streaming task
    /// so the subscriber count is decremented when the viewer disconnects.
    ///
    /// # Arguments
    /// * `external_source` - If provided and no Redis publisher exists, starts an
    ///   external RTMP, RTSP, or HTTP-FLV pull.
    pub async fn create_session_with_pull(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        external_source: Option<&synctv_core::models::LiveProxyMediaSourceConfig>,
    ) -> Result<(
        mpsc::Receiver<Result<Bytes, std::io::Error>>,
        StreamSubscriberGuard,
    )> {
        let guard = match external_source {
            Some(config) => {
                infrastructure
                    .ensure_external_pull_stream(room_id, media_id, config)
                    .await?
            }
            None => {
                infrastructure
                    .ensure_pull_stream(room_id, media_id, None)
                    .await?
            }
        };

        let rx = Self::create_session(infrastructure, room_id, media_id).await?;
        Ok((rx, guard))
    }
}

/// In cluster mode, local-only backends proxy remote publisher reads through
/// the publisher node. The `shared_file` backend reads segment files from the
/// current node's shared mount.
pub struct HlsStreamingApi;

impl HlsStreamingApi {
    async fn wait_for_active_generation(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
    ) -> Result<Option<StreamGeneration>> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let generation = infrastructure
                .registry
                .get_active_generation(room_id, media_id)
                .await
                .map_err(|error| {
                    StreamError::RegistryError(format!(
                        "Failed to resolve active HLS generation: {error}"
                    ))
                })?;
            if generation.is_some() || tokio::time::Instant::now() >= deadline {
                return Ok(generation);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub fn validate_segment_name(segment_name: &str) -> Result<()> {
        crate::util::validate_hls_segment_name(segment_name)
    }

    /// Resolve the active generation used by an HLS master playlist.
    ///
    /// For live_proxy media, the external source is lazily pulled even when the
    /// first viewer is an HLS client. The returned guard is kept only for this
    /// request; idle cleanup still owns long-term shutdown.
    pub async fn resolve_active_generation_with_pull(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        external_source: Option<&synctv_core::models::LiveProxyMediaSourceConfig>,
    ) -> Result<Option<StreamGeneration>> {
        let _guard = if let Some(source) = external_source {
            let active = infrastructure
                .registry
                .get_active_generation(room_id, media_id)
                .await
                .map_err(|error| {
                    StreamError::RegistryError(format!(
                        "Failed to resolve active HLS publisher: {error}"
                    ))
                })?;
            match active {
                Some(publisher) if !infrastructure.is_local_publisher_node(&publisher.node_id) => {
                    None
                }
                _ => Some(
                    infrastructure
                        .ensure_external_pull_stream(room_id, media_id, source)
                        .await?,
                ),
            }
        } else {
            None
        };

        let generation = infrastructure
            .registry
            .get_active_generation(room_id, media_id)
            .await
            .map_err(|error| {
                StreamError::RegistryError(format!(
                    "Failed to resolve active HLS generation: {error}"
                ))
            })?;
        if generation.is_some() || external_source.is_none() {
            return Ok(generation);
        }

        Self::wait_for_active_generation(infrastructure, room_id, media_id).await
    }

    /// Returns `Ok(Some(playlist))` when a stream is found, `Ok(None)` when
    /// the stream is not yet available (caller should return HTTP 404 or retry),
    /// and `Err` on infrastructure failures.
    ///
    /// HLS requests do NOT trigger gRPC RTMP pull streams. Only FLV needs that.
    pub async fn generate_playlist<F>(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        url_generator: F,
    ) -> Result<Option<String>>
    where
        F: Fn(&str) -> String,
    {
        let _activity_guard = infrastructure
            .external_publish_manager
            .subscribe_active_generation(room_id, media_id, generation_id)
            .await;
        let generation = infrastructure
            .registry
            .get_generation(room_id, media_id, generation_id)
            .await
            .map_err(|e| StreamError::RegistryError(format!("Failed to resolve HLS route: {e}")))?;

        let Some(generation) = generation else {
            return Ok(None);
        };

        let is_local = infrastructure.is_local_publisher_node(&generation.node_id);
        let wait_for_ready = generation.ended_at.is_none();

        if !is_local {
            if let Some(hls_proxy) = &infrastructure.hls_proxy {
                let cluster_address = generation.validate_cluster_address().map_err(|e| {
                    anyhow::anyhow!("Cannot proxy HLS for {room_id}/{media_id}: {e}")
                })?;

                let sample_url = url_generator("__PLACEHOLDER__");
                let (segment_url_base, segment_url_suffix) =
                    sample_url.rsplit_once("__PLACEHOLDER__").map_or_else(
                        || (String::new(), String::new()),
                        |(base, suffix)| (base.to_string(), suffix.to_string()),
                    );

                let playlist = hls_proxy
                    .get_playlist(
                        crate::grpc::HlsRelayRoute::new(
                            cluster_address,
                            room_id,
                            media_id,
                            generation_id,
                            generation.lease_epoch,
                        ),
                        &segment_url_base,
                        &segment_url_suffix,
                    )
                    .await?;

                return Ok(playlist);
            }
        }

        Ok(Self::generate_playlist_local(
            infrastructure,
            room_id,
            media_id,
            generation_id,
            wait_for_ready,
            url_generator,
        )
        .await)
    }

    async fn generate_playlist_local<F>(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        wait_for_ready: bool,
        url_generator: F,
    ) -> Option<String>
    where
        F: Fn(&str) -> String,
    {
        if let Some(hls_registry) = &infrastructure.hls_stream_registry {
            let stream_state = find_hls_generation_state(
                hls_registry,
                room_id,
                media_id,
                generation_id,
                wait_for_ready,
            )
            .await?;
            let state = stream_state.read();
            Some(state.generate_m3u8(url_generator))
        } else {
            None
        }
    }

    pub async fn generate_playlist_simple(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        segment_url_base: &str,
    ) -> Result<Option<String>> {
        Self::generate_playlist(
            infrastructure,
            room_id,
            media_id,
            generation_id,
            |ts_name| format!("{segment_url_base}{ts_name}.ts"),
        )
        .await
    }

    /// HLS segment requests do NOT trigger gRPC RTMP pull streams.
    pub async fn get_segment(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        segment_name: &str,
    ) -> Result<Bytes> {
        let _activity_guard = infrastructure
            .external_publish_manager
            .subscribe_active_generation(room_id, media_id, generation_id)
            .await;
        let publisher_info = infrastructure
            .registry
            .get_generation(room_id, media_id, generation_id)
            .await
            .map_err(|e| StreamError::RegistryError(format!("Failed to resolve HLS route: {e}")))?;

        let Some(publisher_info) = publisher_info else {
            return Err(StreamError::StreamNotFound(format!(
                "HLS generation {generation_id} for {room_id}/{media_id}"
            ))
            .into());
        };

        let is_local = infrastructure.is_local_publisher_node(&publisher_info.node_id);

        if !is_local
            && !infrastructure
                .hls_storage_backend
                .supports_cross_node_read()
        {
            if let Some(hls_proxy) = &infrastructure.hls_proxy {
                let cluster_address = publisher_info.validate_cluster_address().map_err(|e| {
                    anyhow::anyhow!("Cannot proxy HLS segment for {room_id}/{media_id}: {e}")
                })?;

                let segment = hls_proxy
                    .get_segment(
                        crate::grpc::HlsRelayRoute::new(
                            cluster_address,
                            room_id,
                            media_id,
                            generation_id,
                            publisher_info.lease_epoch,
                        ),
                        segment_name,
                    )
                    .await?;

                return segment.ok_or_else(|| {
                    StreamError::StreamNotFound(format!(
                        "segment {segment_name} for {room_id}/{media_id} on publisher node"
                    ))
                    .into()
                });
            }
        }

        Self::get_segment_local(infrastructure, room_id, media_id, segment_name).await
    }

    async fn get_segment_local(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        segment_name: &str,
    ) -> Result<Bytes> {
        if let Some(segment_manager) = &infrastructure.segment_manager {
            segment_manager
                .storage()
                .read(room_id, media_id, segment_name)
                .await
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        StreamError::StreamNotFound(format!(
                            "segment {segment_name} for {room_id}/{media_id}"
                        ))
                        .into()
                    } else {
                        anyhow::anyhow!("Failed to read segment: {e}")
                    }
                })
        } else {
            Err(StreamError::InvalidState("Segment manager not configured".to_string()).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::{test_registry::TestStreamRegistry, StreamGeneration};
    use crate::util::TEST_GENERATION_ID;
    use synctv_xiu::storage::HlsStorage as _;
    use synctv_xiu::streamhub::define::StreamHubEvent;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    fn make_infrastructure_with_publisher(
        local_node_id: &str,
        publisher_node_id: &str,
        cluster_address: &str,
    ) -> std::result::Result<LiveStreamingInfrastructure, crate::error::StreamError> {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                StreamGeneration {
                    node_id: publisher_node_id.to_string(),
                    cluster_address: cluster_address.to_string(),
                    app_name: "live".to_string(),
                    user_id: String::new(),
                    started_at: synctv_core::SystemClock.now(),
                    ended_at: None,
                    lease_epoch: 1,
                    generation_id: TEST_GENERATION_ID.to_string(),
                },
            )]),
        ));
        let (event_sender, _event_receiver) = mpsc::channel(64);
        LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            Arc::new(StreamTracker::new()),
            local_node_id.to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )
    }

    async fn recv_force_unpublish_event(
        event_receiver: &mut mpsc::Receiver<StreamHubEvent>,
    ) -> TestResult {
        let event = event_receiver
            .recv()
            .await
            .ok_or_else(|| test_error("expected ForceUnPublish event"))?;
        match event {
            StreamHubEvent::ForceUnPublish { .. } => Ok(()),
            other => Err(test_error(format!(
                "expected ForceUnPublish event, got {other:?}"
            ))),
        }
    }

    #[tokio::test]
    async fn test_ensure_pull_stream_skips_grpc_relay_for_local_publisher() -> TestResult {
        let infrastructure = make_infrastructure_with_publisher("node-local", "node-local", "")?;

        let guard = infrastructure
            .ensure_pull_stream("room1", "media1", None)
            .await?;

        drop(guard);
        Ok(())
    }

    #[tokio::test]
    async fn active_local_playlist_waits_for_remuxer_generation_state() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        let generation_id = synctv_xiu::streamhub::utils::Uuid::new();
        let generation_id_string = generation_id.to_string();
        assert!(
            registry
                .try_activate_generation(
                    "room-ready",
                    "media-ready",
                    "node-local",
                    "",
                    "127.0.0.1:50051",
                    &generation_id_string,
                )
                .await?
        );
        let (event_sender, _event_receiver) = mpsc::channel(8);
        let hls_registry = Arc::new(dashmap::DashMap::new());
        let infrastructure = LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            Arc::new(StreamTracker::new()),
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?
        .with_hls_stream_registry(hls_registry.clone());

        let delayed_registry = hls_registry;
        let delayed_generation_id = generation_id_string.clone();
        let insert_task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let mut playlist = synctv_xiu::hls::HlsPlaylist::new();
            playlist.push_segment(synctv_xiu::hls::SegmentInfo {
                sequence: 0,
                duration_ms: 1_000,
                started_at_ms: 0,
                ts_name: "ready-segment".to_string(),
                discontinuity: false,
            });
            delayed_registry.insert(
                synctv_xiu::hls::generation_registry_key(
                    "room-ready",
                    "media-ready",
                    &delayed_generation_id,
                ),
                Arc::new(parking_lot::RwLock::new(
                    synctv_xiu::hls::StreamProcessorState {
                        app_name: "room-ready".to_string(),
                        stream_name: "media-ready".to_string(),
                        playlist,
                        generation_id,
                        marked_for_cleanup: false,
                        cleanup_segment_names: Vec::new(),
                    },
                )),
            );
        });

        let playlist = HlsStreamingApi::generate_playlist_simple(
            &infrastructure,
            "room-ready",
            "media-ready",
            &generation_id_string,
            "/segments/",
        )
        .await?
        .ok_or_else(|| test_error("active generation should wait for remuxer state"))?;
        insert_task.await?;

        assert!(playlist.contains("/segments/ready-segment.ts"));
        Ok(())
    }

    #[tokio::test]
    async fn ended_local_playlist_remains_available_after_publisher_unregister() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        let generation_id = synctv_xiu::streamhub::utils::Uuid::new();
        let generation_id_string = generation_id.to_string();
        assert!(
            registry
                .try_activate_generation(
                    "room1",
                    "media1",
                    "node-local",
                    "",
                    "127.0.0.1:50051",
                    &generation_id_string,
                )
                .await?
        );
        let publisher = registry
            .get_active_generation("room1", "media1")
            .await?
            .ok_or_else(|| test_error("registered publisher should exist"))?;
        let (event_sender, _event_receiver) = mpsc::channel(64);
        let mut infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            Arc::new(StreamTracker::new()),
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;
        let hls_registry = Arc::new(dashmap::DashMap::new());
        let mut playlist = synctv_xiu::hls::HlsPlaylist::new();
        playlist.mark_ended();
        hls_registry.insert(
            synctv_xiu::hls::generation_registry_key("room1", "media1", &generation_id_string),
            Arc::new(parking_lot::RwLock::new(
                synctv_xiu::hls::StreamProcessorState {
                    app_name: "room1".to_string(),
                    stream_name: "media1".to_string(),
                    playlist,
                    generation_id,
                    marked_for_cleanup: true,
                    cleanup_segment_names: Vec::new(),
                },
            )),
        );
        infrastructure = infrastructure.with_hls_stream_registry(hls_registry);
        infrastructure = infrastructure.with_segment_manager(Arc::new(SegmentManager::new(
            Arc::new(synctv_xiu::storage::MemoryStorage::new()),
            crate::livestream::CleanupConfig::default(),
        )));
        registry
            .deactivate_generation_preserving_hls_if_lease_matches(
                "room1",
                "media1",
                &generation_id.to_string(),
                publisher.lease_epoch,
            )
            .await?;

        let playlist = HlsStreamingApi::generate_playlist_simple(
            &infrastructure,
            "room1",
            "media1",
            &generation_id_string,
            "/segments/",
        )
        .await?
        .ok_or_else(|| test_error("ended local playlist should remain available"))?;

        assert!(playlist.contains("#EXT-X-ENDLIST"));
        Ok(())
    }

    #[tokio::test]
    async fn final_playlist_expires_before_ended_generation_segments() -> TestResult {
        let generation_id = synctv_xiu::streamhub::utils::Uuid::new();
        let generation_id_string = generation_id.to_string();
        let registry = Arc::new(TestStreamRegistry::new());
        assert!(
            registry
                .try_activate_generation(
                    "room-grace",
                    "media-grace",
                    "node-local",
                    "",
                    "127.0.0.1:50051",
                    &generation_id_string,
                )
                .await?
        );
        let lease_epoch = registry
            .get_active_generation("room-grace", "media-grace")
            .await?
            .ok_or_else(|| test_error("publisher should exist"))?
            .lease_epoch;

        let storage = Arc::new(synctv_xiu::storage::MemoryStorage::unlimited());
        storage
            .write(
                "room-grace",
                "media-grace",
                "segment001",
                Bytes::from_static(b"retained-segment"),
            )
            .await?;
        let cleanup_config = crate::livestream::CleanupConfig {
            interval: std::time::Duration::from_millis(10),
            retention: std::time::Duration::from_hours(1),
            final_playlist_grace: std::time::Duration::from_millis(40),
            ended_segment_grace: std::time::Duration::from_millis(160),
            max_segments_per_stream: 0,
        };
        let segment_manager = Arc::new(SegmentManager::new(storage.clone(), cleanup_config));
        segment_manager.schedule_generation_cleanup(
            "room-grace".to_string(),
            "media-grace".to_string(),
            vec!["segment001".to_string()],
        );
        let cleanup_shutdown = CancellationToken::new();
        let cleanup_task =
            Arc::clone(&segment_manager).start_cleanup_task(cleanup_shutdown.clone());

        let mut playlist = synctv_xiu::hls::HlsPlaylist::new();
        playlist.push_segment(synctv_xiu::hls::SegmentInfo {
            sequence: 0,
            duration_ms: 1_000,
            started_at_ms: 0,
            ts_name: "segment001".to_string(),
            discontinuity: false,
        });
        playlist.mark_ended();
        let hls_registry = Arc::new(dashmap::DashMap::new());
        hls_registry.insert(
            synctv_xiu::hls::generation_registry_key(
                "room-grace",
                "media-grace",
                &generation_id_string,
            ),
            Arc::new(parking_lot::RwLock::new(
                synctv_xiu::hls::StreamProcessorState {
                    app_name: "room-grace".to_string(),
                    stream_name: "media-grace".to_string(),
                    playlist,
                    generation_id,
                    marked_for_cleanup: true,
                    cleanup_segment_names: vec!["segment001".to_string()],
                },
            )),
        );
        let (event_sender, _event_receiver) = mpsc::channel(8);
        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            Arc::new(StreamTracker::new()),
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?
        .with_hls_stream_registry(hls_registry.clone())
        .with_segment_manager(segment_manager);
        registry
            .deactivate_generation_preserving_hls_if_lease_matches(
                "room-grace",
                "media-grace",
                &generation_id.to_string(),
                lease_epoch,
            )
            .await?;

        let final_playlist = HlsStreamingApi::generate_playlist_simple(
            &infrastructure,
            "room-grace",
            "media-grace",
            &generation_id_string,
            "/segments/",
        )
        .await?
        .ok_or_else(|| test_error("final playlist should be visible during its grace period"))?;
        assert!(final_playlist.contains("#EXT-X-ENDLIST"));

        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        hls_registry.remove(&synctv_xiu::hls::generation_registry_key(
            "room-grace",
            "media-grace",
            &generation_id_string,
        ));
        assert!(HlsStreamingApi::generate_playlist_simple(
            &infrastructure,
            "room-grace",
            "media-grace",
            &generation_id_string,
            "/segments/",
        )
        .await?
        .is_none());
        assert_eq!(
            HlsStreamingApi::get_segment(
                &infrastructure,
                "room-grace",
                "media-grace",
                &generation_id_string,
                "segment001",
            )
            .await?,
            Bytes::from_static(b"retained-segment")
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if HlsStreamingApi::get_segment(
                    &infrastructure,
                    "room-grace",
                    "media-grace",
                    &generation_id_string,
                    "segment001",
                )
                .await
                .is_err()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;

        cleanup_shutdown.cancel();
        cleanup_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn hls_only_live_proxy_playlist_poll_refreshes_local_activity() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::new());
        let (event_sender, _event_receiver) = mpsc::channel(8);
        let mut infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            Arc::new(StreamTracker::new()),
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;
        let source = synctv_core::models::ExternalLiveSourceConfig::HttpFlv {
            url: "http://127.0.0.1/live.flv".to_string(),
        };
        let (stream, generation_id) = infrastructure
            .external_publish_manager
            .install_running_test_stream("room-activity", "media-activity", source.clone());
        let generation_id_string = generation_id.to_string();
        assert!(
            registry
                .try_activate_generation(
                    "room-activity",
                    "media-activity",
                    "node-local",
                    "",
                    "127.0.0.1:50051",
                    &generation_id_string,
                )
                .await?
        );

        let mut playlist = synctv_xiu::hls::HlsPlaylist::new();
        playlist.push_segment(synctv_xiu::hls::SegmentInfo {
            sequence: 0,
            duration_ms: 1_000,
            started_at_ms: 0,
            ts_name: "segment001".to_string(),
            discontinuity: false,
        });
        let hls_registry = Arc::new(dashmap::DashMap::new());
        hls_registry.insert(
            synctv_xiu::hls::generation_registry_key(
                "room-activity",
                "media-activity",
                &generation_id_string,
            ),
            Arc::new(parking_lot::RwLock::new(
                synctv_xiu::hls::StreamProcessorState {
                    app_name: "room-activity".to_string(),
                    stream_name: "media-activity".to_string(),
                    playlist,
                    generation_id,
                    marked_for_cleanup: false,
                    cleanup_segment_names: Vec::new(),
                },
            )),
        );
        infrastructure = infrastructure.with_hls_stream_registry(hls_registry);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while stream.lifecycle().last_active_elapsed_secs() == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await?;
        let config = synctv_core::models::LiveProxyMediaSourceConfig { source };
        let active_generation = HlsStreamingApi::resolve_active_generation_with_pull(
            &infrastructure,
            "room-activity",
            "media-activity",
            Some(&config),
        )
        .await?
        .ok_or_else(|| test_error("active external stream should expose a generation"))?;
        let playlist = HlsStreamingApi::generate_playlist(
            &infrastructure,
            "room-activity",
            "media-activity",
            &active_generation.generation_id,
            |segment| format!("/segments/{}/{segment}.ts", active_generation.generation_id),
        )
        .await?
        .ok_or_else(|| test_error("active external stream should expose a playlist"))?;

        assert!(playlist.contains(&generation_id_string));
        assert_eq!(stream.lifecycle().last_active_elapsed_secs(), 0);
        assert_eq!(stream.lifecycle().subscriber_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_live_proxy_prefers_registered_remote_publisher_before_external_pull() -> TestResult
    {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                StreamGeneration {
                    node_id: "node-remote".to_string(),
                    cluster_address: String::new(),
                    app_name: "live".to_string(),
                    user_id: String::new(),
                    started_at: synctv_core::SystemClock.now(),
                    ended_at: None,
                    lease_epoch: 1,
                    generation_id: TEST_GENERATION_ID.to_string(),
                },
            )]),
        ));
        let (event_sender, _event_receiver) = mpsc::channel(64);
        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            Arc::new(StreamTracker::new()),
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;

        let source = synctv_core::models::ExternalLiveSourceConfig::HttpFlv {
            url: "http://127.0.0.1:8080/live.flv".to_string(),
        };
        let error = infrastructure
            .ensure_pull_stream("room1", "media1", Some(&source))
            .await
            .expect_err("remote publisher relay should fail fast because cluster_address is empty");

        assert!(
            error.to_string().contains("Failed to create pull stream")
                || error.to_string().contains("cluster_address"),
            "unexpected error: {error}"
        );
        assert_eq!(
            registry.register_call_count(),
            0,
            "live_proxy must relay an existing remote publisher before creating a local external puller"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_empty_local_node_id_treats_registry_publisher_as_local() -> TestResult {
        let infrastructure = make_infrastructure_with_publisher("", "node-local", "")?;

        let guard = infrastructure
            .ensure_pull_stream("room1", "media1", None)
            .await?;

        drop(guard);
        Ok(())
    }

    #[tokio::test]
    async fn test_ensure_pull_stream_validates_epoch_for_local_publisher() -> TestResult {
        use std::collections::HashMap;

        let registry = Arc::new(TestStreamRegistry::with_publishers(HashMap::from([(
            ("room1".to_string(), "media1".to_string()),
            StreamGeneration {
                node_id: "node-local".to_string(),
                cluster_address: String::new(),
                app_name: "live".to_string(),
                user_id: String::new(),
                started_at: synctv_core::SystemClock.now(),
                ended_at: None,
                lease_epoch: 1,
                generation_id: TEST_GENERATION_ID.to_string(),
            },
        )])));

        let (event_sender, _event_receiver) = mpsc::channel(1);
        let infrastructure = LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            Arc::new(StreamTracker::new()),
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;

        let result = infrastructure
            .ensure_pull_stream("room1", "media1", None)
            .await;
        assert!(
            result.is_ok(),
            "Local publisher with valid lease_epoch should succeed, got error: {:?}",
            result.err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_create_session_with_pull_keeps_local_publishers_local() -> TestResult {
        let infrastructure = make_infrastructure_with_publisher("node-local", "node-local", "")?;

        let result =
            FlvStreamingApi::create_session_with_pull(&infrastructure, "room1", "media1", None)
                .await;

        assert!(
            result.is_ok(),
            "local publisher should create FLV session without requiring gRPC pull"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shared_backends_are_read_directly_on_non_publisher_nodes() -> TestResult {
        for backend in [HlsStorageBackend::SharedFile, HlsStorageBackend::S3] {
            let storage: Arc<dyn synctv_xiu::storage::HlsStorage> =
                Arc::new(synctv_xiu::storage::MemoryStorage::new());
            storage
                .write("room1", "media1", "seg1", Bytes::from_static(b"segment"))
                .await?;

            let segment_manager = Arc::new(SegmentManager::new(
                storage,
                crate::livestream::CleanupConfig::default(),
            ));
            let infrastructure =
                make_infrastructure_with_publisher("node-local", "node-remote", "")?
                    .with_segment_manager(segment_manager)
                    .with_hls_storage_backend(backend)
                    .with_hls_proxy(HlsProxyClient::with_defaults(Some(
                        "cluster-secret".to_string(),
                    )));

            let segment = HlsStreamingApi::get_segment(
                &infrastructure,
                "room1",
                "media1",
                TEST_GENERATION_ID,
                "seg1",
            )
            .await?;
            assert_eq!(
                segment,
                Bytes::from_static(b"segment"),
                "backend={backend:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_kick_stream_does_not_delete_tracking_when_unpublish_signal_fails() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                StreamGeneration {
                    node_id: "node-local".to_string(),
                    cluster_address: "127.0.0.1:50051".to_string(),
                    app_name: "live".to_string(),
                    user_id: "user1".to_string(),
                    started_at: synctv_core::SystemClock.now(),
                    ended_at: None,
                    lease_epoch: 1,
                    generation_id: TEST_GENERATION_ID.to_string(),
                },
            )]),
        ));
        let (event_sender, event_receiver) = mpsc::channel(1);
        drop(event_receiver);

        let tracker = Arc::new(StreamTracker::new());
        tracker.insert(
            "user1".to_string(),
            "room1".to_string(),
            "media1".to_string(),
        );

        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender.clone(),
            tracker.clone(),
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;

        let err = match infrastructure.kick_stream("room1", "media1").await {
            Ok(()) => return Err(test_error("closed StreamHub channel should fail the kick")),
            Err(err) => err,
        };

        assert!(
            err.to_string().contains("StreamHub not running"),
            "unexpected error: {err}"
        );
        assert_eq!(
            tracker.get_stream_user("room1", "media1").as_deref(),
            Some("user1"),
            "tracker entry must remain until UnPublish is accepted"
        );
        assert!(
            registry
                .get_active_generation("room1", "media1")
                .await?
                .is_some(),
            "registry entry must remain until UnPublish is accepted"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_kick_stream_keeps_registry_and_tracker_until_unpublish_processing() -> TestResult
    {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                StreamGeneration {
                    node_id: "node-local".to_string(),
                    cluster_address: "127.0.0.1:50051".to_string(),
                    app_name: "live".to_string(),
                    user_id: "user1".to_string(),
                    started_at: synctv_core::SystemClock.now(),
                    ended_at: None,
                    lease_epoch: 1,
                    generation_id: TEST_GENERATION_ID.to_string(),
                },
            )]),
        ));
        let (event_sender, mut event_receiver) = mpsc::channel(1);

        let tracker = Arc::new(StreamTracker::new());
        tracker.insert(
            "user1".to_string(),
            "room1".to_string(),
            "media1".to_string(),
        );

        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            tracker.clone(),
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;

        infrastructure.kick_stream("room1", "media1").await?;

        recv_force_unpublish_event(&mut event_receiver).await?;

        assert_eq!(
            tracker.get_stream_user("room1", "media1").as_deref(),
            Some("user1"),
            "kick_stream must keep tracker entry until StreamHub processes UnPublish"
        );
        assert!(
            registry
                .get_active_generation("room1", "media1")
                .await?
                .is_some(),
            "kick_stream must keep registry entry until lease_epoch-fenced unpublish cleanup"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_kick_stream_waits_for_streamhub_backpressure_to_clear() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                StreamGeneration {
                    node_id: "node-local".to_string(),
                    cluster_address: "127.0.0.1:50051".to_string(),
                    app_name: "live".to_string(),
                    user_id: "user1".to_string(),
                    started_at: synctv_core::SystemClock.now(),
                    ended_at: None,
                    lease_epoch: 1,
                    generation_id: TEST_GENERATION_ID.to_string(),
                },
            )]),
        ));
        let (event_sender, mut event_receiver) = mpsc::channel(1);
        event_sender
            .try_send(StreamHubEvent::ForceUnPublish {
                identifier: synctv_xiu::streamhub::stream::StreamIdentifier::Rtmp {
                    app_name: "blocked".to_string(),
                    stream_name: "blocked".to_string(),
                },
            })
            .map_err(|error| test_error(format!("failed to prefill channel: {error}")))?;

        let tracker = Arc::new(StreamTracker::new());
        let infrastructure = LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            tracker,
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;

        let kick = tokio::spawn(async move { infrastructure.kick_stream("room1", "media1").await });
        let blocked = event_receiver
            .recv()
            .await
            .ok_or_else(|| test_error("expected prefilled event"))?;
        assert!(
            matches!(blocked, StreamHubEvent::ForceUnPublish { .. }),
            "expected prefilled ForceUnPublish event, got {blocked:?}"
        );

        kick.await
            .map_err(|error| test_error(format!("kick task panicked: {error}")))??;
        recv_force_unpublish_event(&mut event_receiver).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_kick_user_publishers_keep_registry_and_tracker_until_unpublish_processing(
    ) -> TestResult {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([
                (
                    ("room1".to_string(), "media1".to_string()),
                    StreamGeneration {
                        node_id: "node-local".to_string(),
                        cluster_address: "127.0.0.1:50051".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: synctv_core::SystemClock.now(),
                        ended_at: None,
                        lease_epoch: 1,
                        generation_id: TEST_GENERATION_ID.to_string(),
                    },
                ),
                (
                    ("room2".to_string(), "media2".to_string()),
                    StreamGeneration {
                        node_id: "node-local".to_string(),
                        cluster_address: "127.0.0.1:50051".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: synctv_core::SystemClock.now(),
                        ended_at: None,
                        lease_epoch: 1,
                        generation_id: TEST_GENERATION_ID.to_string(),
                    },
                ),
            ]),
        ));
        let (event_sender, mut event_receiver) = mpsc::channel(2);

        let tracker = Arc::new(StreamTracker::new());
        tracker.insert(
            "user1".to_string(),
            "room1".to_string(),
            "media1".to_string(),
        );
        tracker.insert(
            "user1".to_string(),
            "room2".to_string(),
            "media2".to_string(),
        );

        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            tracker.clone(),
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;

        infrastructure.kick_user_publishers("user1").await;

        for _ in 0..2 {
            recv_force_unpublish_event(&mut event_receiver).await?;
        }

        let remaining_streams = tracker.get_user_streams("user1");
        assert_eq!(remaining_streams.len(), 2);
        assert!(
            registry
                .get_active_generation("room1", "media1")
                .await?
                .is_some(),
            "kick_user_publishers must keep the first registry entry until unpublish cleanup"
        );
        assert!(
            registry
                .get_active_generation("room2", "media2")
                .await?
                .is_some(),
            "kick_user_publishers must keep the second registry entry until unpublish cleanup"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_kick_user_room_publishers_only_removes_streams_in_target_room() -> TestResult {
        let registry = Arc::new(crate::relay::TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([
                (
                    ("room1".to_string(), "media1".to_string()),
                    StreamGeneration {
                        node_id: "node-local".to_string(),
                        cluster_address: "127.0.0.1:50051".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: synctv_core::SystemClock.now(),
                        ended_at: None,
                        lease_epoch: 1,
                        generation_id: TEST_GENERATION_ID.to_string(),
                    },
                ),
                (
                    ("room2".to_string(), "media2".to_string()),
                    StreamGeneration {
                        node_id: "node-local".to_string(),
                        cluster_address: "127.0.0.1:50051".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: synctv_core::SystemClock.now(),
                        ended_at: None,
                        lease_epoch: 1,
                        generation_id: TEST_GENERATION_ID.to_string(),
                    },
                ),
            ]),
        ));
        let (event_sender, mut event_receiver) = mpsc::channel(2);

        let tracker = Arc::new(StreamTracker::new());
        tracker.insert(
            "user1".to_string(),
            "room1".to_string(),
            "media1".to_string(),
        );
        tracker.insert(
            "user1".to_string(),
            "room2".to_string(),
            "media2".to_string(),
        );

        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            tracker.clone(),
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;

        infrastructure
            .kick_user_room_publishers("room1", "user1")
            .await;

        recv_force_unpublish_event(&mut event_receiver).await?;
        assert!(
            event_receiver.try_recv().is_err(),
            "room-scoped kick should only enqueue the room-local publisher"
        );

        assert!(
            tracker.get_stream_user("room1", "media1").is_some(),
            "target room publisher must remain tracked until unpublish cleanup"
        );
        assert_eq!(
            tracker.get_stream_user("room2", "media2").as_deref(),
            Some("user1"),
            "publishers in other rooms must remain tracked"
        );
        assert!(
            registry
                .get_active_generation("room1", "media1")
                .await?
                .is_some(),
            "target room publisher must remain registered until unpublish cleanup"
        );
        assert!(
            registry
                .get_active_generation("room2", "media2")
                .await?
                .is_some(),
            "publishers in other rooms must remain registered"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_kick_user_publishers_skips_remote_registry_publishers() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([
                (
                    ("room1".to_string(), "media1".to_string()),
                    StreamGeneration {
                        node_id: "node-local".to_string(),
                        cluster_address: "127.0.0.1:50051".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: synctv_core::SystemClock.now(),
                        ended_at: None,
                        lease_epoch: 1,
                        generation_id: TEST_GENERATION_ID.to_string(),
                    },
                ),
                (
                    ("room2".to_string(), "media2".to_string()),
                    StreamGeneration {
                        node_id: "node-remote".to_string(),
                        cluster_address: "127.0.0.1:50052".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: synctv_core::SystemClock.now(),
                        ended_at: None,
                        lease_epoch: 1,
                        generation_id: TEST_GENERATION_ID.to_string(),
                    },
                ),
            ]),
        ));
        let (event_sender, mut event_receiver) = mpsc::channel(2);
        let tracker = Arc::new(StreamTracker::new());
        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            tracker,
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;

        infrastructure.kick_user_publishers("user1").await;

        recv_force_unpublish_event(&mut event_receiver).await?;
        assert!(
            event_receiver.try_recv().is_err(),
            "remote publisher must not be kicked by a non-owner replica"
        );
        assert!(
            registry
                .get_active_generation("room1", "media1")
                .await?
                .is_some(),
            "local publisher must remain registered until its owner processes UnPublish"
        );
        assert!(
            registry
                .get_active_generation("room2", "media2")
                .await?
                .is_some(),
            "remote publisher must remain registered for its owner node to terminate"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_kick_stream_skips_remote_publisher_registry_entry() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                StreamGeneration {
                    node_id: "node-remote".to_string(),
                    cluster_address: "127.0.0.1:50052".to_string(),
                    app_name: "live".to_string(),
                    user_id: "user1".to_string(),
                    started_at: synctv_core::SystemClock.now(),
                    ended_at: None,
                    lease_epoch: 1,
                    generation_id: TEST_GENERATION_ID.to_string(),
                },
            )]),
        ));
        let (event_sender, mut event_receiver) = mpsc::channel(1);
        let tracker = Arc::new(StreamTracker::new());
        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            tracker,
            "node-local".to_string(),
            synctv_common::ssrf::SsrfGuard::disabled(),
        )?;

        infrastructure.kick_stream("room1", "media1").await?;

        assert!(
            event_receiver.try_recv().is_err(),
            "non-owner replica must not send local UnPublish for remote publisher"
        );
        assert!(
            registry
                .get_active_generation("room1", "media1")
                .await?
                .is_some(),
            "non-owner replica must not remove remote publisher registry entry"
        );
        Ok(())
    }
}
