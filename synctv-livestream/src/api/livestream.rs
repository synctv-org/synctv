// Live streaming API helpers used by synctv-api HTTP endpoints.

use crate::{
    error::StreamError,
    grpc::{HlsProxyClient, StreamRelayServiceImpl},
    livestream::{
        external_publish_manager::ExternalPublishManager, managed_stream::ManagedStream,
        pull_manager::PullStreamManager, SegmentManager,
    },
    relay::{ActivePublisherEntry, PublisherInfo, StreamRegistryTrait},
};
use anyhow::{Context, Result};
use bytes::Bytes;
use std::collections::BTreeSet;
use std::sync::Arc;
use synctv_common::ssrf::SsrfGuard;
use synctv_core::config::HlsStorageBackend;
use synctv_xiu::hls::remuxer::StreamRegistry as HlsStreamRegistry;
use synctv_xiu::httpflv::HttpFlvSession;
use synctv_xiu::streamhub::define::StreamHubEventSender;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

pub use super::tracker::{StreamSubscriberGuard, StreamTracker};

#[derive(Clone)]
pub struct LiveStreamingInfrastructure {
    /// Registry for finding publishers (Redis)
    pub(crate) registry: Arc<dyn StreamRegistryTrait>,
    /// `StreamHub` event sender for subscribing to streams
    pub(crate) stream_hub_event_sender: StreamHubEventSender,
    /// Pull stream manager for gRPC relay (cross-node pull)
    pub(crate) pull_manager: Arc<PullStreamManager>,
    /// External publish manager for pull-to-publish streams (RTMP/HTTP-FLV sources)
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
    pub fn kick_publisher(&self, room_id: &str, media_id: &str) -> Result<()> {
        use synctv_xiu::streamhub::stream::StreamIdentifier;

        // StreamHub uses canonical (room_id, media_id) identifiers after auth rewrite
        let identifier = StreamIdentifier::Rtmp {
            app_name: room_id.to_string(),
            stream_name: media_id.to_string(),
        };

        self.stream_hub_event_sender
            .try_send(synctv_xiu::streamhub::define::StreamHubEvent::UnPublish { identifier })
            .map_err(|_| {
                anyhow::anyhow!("Failed to send unpublish event (StreamHub not running)")
            })?;

        Ok(())
    }

    fn is_local_publisher_node(&self, node_id: &str) -> bool {
        self.local_node_id.is_empty() || node_id == self.local_node_id
    }

    async fn registry_stream_owned_by_local_node(&self, room_id: &str, media_id: &str) -> bool {
        match self.registry.get_publisher(room_id, media_id).await {
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
            .get_publisher(room_id, media_id)
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

    async fn local_registry_user_publishers(&self, user_id: &str) -> Vec<(String, String)> {
        match self.registry.get_user_publishers(user_id).await {
            Ok(registry_streams) => {
                let mut local_streams = Vec::new();
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
            Ok(registry_streams) => {
                let mut local_streams = Vec::new();
                for (stream_room_id, media_id) in registry_streams {
                    if self
                        .registry_stream_owned_by_local_node(&stream_room_id, &media_id)
                        .await
                    {
                        local_streams.push((stream_room_id, media_id));
                    }
                }
                local_streams
            }
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
    /// unpublish path, which fences cleanup against the publisher epoch. Do not
    /// unregister here: this method only enqueues the control event, and deleting
    /// ownership before StreamHub processes it can let a replacement publisher
    /// register and then be torn down by the delayed unpublish.
    pub async fn kick_stream(&self, room_id: &str, media_id: &str) -> Result<()> {
        if let Some(publisher) = self.registry.get_publisher(room_id, media_id).await? {
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
        self.kick_publisher(room_id, media_id)?;

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
        external_source_url: Option<&str>,
    ) -> Result<StreamSubscriberGuard> {
        // Check Redis for an existing publisher
        let publisher = self
            .registry
            .get_publisher(room_id, media_id)
            .await
            .map_err(|e| StreamError::RegistryError(format!("Failed to check publisher: {e}")))?;

        if let Some(publisher_info) = publisher {
            let is_local = self.is_local_publisher_node(&publisher_info.node_id);

            if is_local {
                match self
                    .registry
                    .validate_epoch(room_id, media_id, publisher_info.epoch)
                    .await
                {
                    Ok(true) => {
                        tracing::debug!(
                            "Epoch {} validated for local publisher {}/{}",
                            publisher_info.epoch,
                            room_id,
                            media_id
                        );
                        return Ok(StreamSubscriberGuard::new(|| {}));
                    }
                    Ok(false) => {
                        warn!(
                            "Epoch {} is stale for local publisher {}/{}, publisher may have changed",
                            publisher_info.epoch, room_id, media_id
                        );
                        return Err(anyhow::anyhow!(
                            "Stale epoch {} for local publisher {}/{}",
                            publisher_info.epoch,
                            room_id,
                            media_id
                        ));
                    }
                    Err(e) => {
                        error!(
                            "Failed to validate epoch for local publisher {}/{}: {}. \
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

        if let Some(source_url) = external_source_url {
            let stream = self
                .external_publish_manager
                .get_or_create(room_id, media_id, source_url)
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
        .with_cluster_secret(cluster_secret);

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
    ) -> Result<Option<PublisherInfo>> {
        self.registry
            .get_publisher(room_id, media_id)
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

    pub async fn list_active_publishers(&self) -> Result<Vec<ActivePublisherEntry>> {
        self.registry
            .list_active_publishers()
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
                .cleanup_all_publishers_for_node(&self.local_node_id),
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
    /// * `external_source_url` - If provided and no Redis publisher exists, starts an
    ///   external pull from this URL (RTMP or HTTP-FLV).
    pub async fn create_session_with_pull(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        external_source_url: Option<&str>,
    ) -> Result<(
        mpsc::Receiver<Result<Bytes, std::io::Error>>,
        StreamSubscriberGuard,
    )> {
        let guard = infrastructure
            .ensure_pull_stream(room_id, media_id, external_source_url)
            .await?;

        let rx = Self::create_session(infrastructure, room_id, media_id).await?;
        Ok((rx, guard))
    }
}

/// In cluster mode, local-only backends proxy remote publisher reads through
/// the publisher node. The `shared_file` backend reads segment files from the
/// current node's shared mount.
pub struct HlsStreamingApi;

impl HlsStreamingApi {
    async fn wait_for_playlist<F>(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        url_generator: &F,
    ) -> Result<Option<String>>
    where
        F: Fn(&str) -> String,
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let playlist =
                Self::generate_playlist(infrastructure, room_id, media_id, url_generator).await?;
            if playlist.is_some() || tokio::time::Instant::now() >= deadline {
                return Ok(playlist);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub fn validate_segment_name(segment_name: &str) -> Result<()> {
        crate::util::validate_hls_segment_name(segment_name)
    }

    /// Start or touch the live stream needed by an HLS request, then generate the playlist.
    ///
    /// For live_proxy media, the external source is lazily pulled even when the
    /// first viewer is an HLS client. The returned guard is kept only for this
    /// request; idle cleanup still owns long-term shutdown.
    pub async fn generate_playlist_with_pull<F>(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        external_source_url: Option<&str>,
        url_generator: F,
    ) -> Result<Option<String>>
    where
        F: Fn(&str) -> String,
    {
        if let Some(source_url) = external_source_url {
            let _guard = infrastructure
                .ensure_pull_stream(room_id, media_id, Some(source_url))
                .await?;
            return Self::wait_for_playlist(infrastructure, room_id, media_id, &url_generator)
                .await;
        }

        Self::generate_playlist(infrastructure, room_id, media_id, url_generator).await
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
        url_generator: F,
    ) -> Result<Option<String>>
    where
        F: Fn(&str) -> String,
    {
        let publisher_info = infrastructure
            .registry
            .get_publisher(room_id, media_id)
            .await
            .map_err(|e| StreamError::RegistryError(format!("Failed to check publisher: {e}")))?;

        let Some(publisher_info) = publisher_info else {
            return Ok(None);
        };

        let is_local = infrastructure.is_local_publisher_node(&publisher_info.node_id);

        if is_local {
            Ok(Self::generate_playlist_local(
                infrastructure,
                room_id,
                media_id,
                url_generator,
            ))
        } else if let Some(hls_proxy) = &infrastructure.hls_proxy {
            let api_addr = publisher_info
                .validate_api_address()
                .map_err(|e| anyhow::anyhow!("Cannot proxy HLS for {room_id}/{media_id}: {e}"))?;

            let sample_url = url_generator("__PLACEHOLDER__");
            let (segment_url_base, segment_url_suffix) =
                sample_url.rsplit_once("__PLACEHOLDER__").map_or_else(
                    || (String::new(), String::new()),
                    |(base, suffix)| (base.to_string(), suffix.to_string()),
                );

            let playlist = hls_proxy
                .get_playlist(
                    api_addr,
                    room_id,
                    media_id,
                    &segment_url_base,
                    &segment_url_suffix,
                    publisher_info.epoch,
                )
                .await?;

            Ok(playlist)
        } else {
            Ok(Self::generate_playlist_local(
                infrastructure,
                room_id,
                media_id,
                url_generator,
            ))
        }
    }

    fn generate_playlist_local<F>(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        url_generator: F,
    ) -> Option<String>
    where
        F: Fn(&str) -> String,
    {
        if let Some(hls_registry) = &infrastructure.hls_stream_registry {
            let stream_key = format!("{room_id}/{media_id}");

            match hls_registry.get(&stream_key) {
                Some(stream_state) => {
                    let state = stream_state.read();
                    Some(state.generate_m3u8(url_generator))
                }
                None => None,
            }
        } else {
            None
        }
    }

    pub async fn generate_playlist_simple(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        segment_url_base: &str,
    ) -> Result<Option<String>> {
        Self::generate_playlist(infrastructure, room_id, media_id, |ts_name| {
            format!("{segment_url_base}{ts_name}.ts")
        })
        .await
    }

    /// HLS segment requests do NOT trigger gRPC RTMP pull streams.
    pub async fn get_segment(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        segment_name: &str,
    ) -> Result<Bytes> {
        let publisher_info = infrastructure
            .registry
            .get_publisher(room_id, media_id)
            .await
            .map_err(|e| StreamError::RegistryError(format!("Failed to check publisher: {e}")))?;

        let Some(publisher_info) = publisher_info else {
            return Err(StreamError::NoPublisher(format!("{room_id}/{media_id}")).into());
        };

        let is_local = infrastructure.is_local_publisher_node(&publisher_info.node_id);

        if is_local || infrastructure.hls_storage_backend == HlsStorageBackend::SharedFile {
            Self::get_segment_local(infrastructure, room_id, media_id, segment_name).await
        } else if let Some(hls_proxy) = &infrastructure.hls_proxy {
            let api_addr = publisher_info.validate_api_address().map_err(|e| {
                anyhow::anyhow!("Cannot proxy HLS segment for {room_id}/{media_id}: {e}")
            })?;

            let segment = hls_proxy
                .get_segment(
                    api_addr,
                    room_id,
                    media_id,
                    segment_name,
                    publisher_info.epoch,
                )
                .await?;

            segment.ok_or_else(|| {
                StreamError::StreamNotFound(format!(
                    "segment {segment_name} for {room_id}/{media_id} on publisher node"
                ))
                .into()
            })
        } else {
            Self::get_segment_local(infrastructure, room_id, media_id, segment_name).await
        }
    }

    /// Start or touch the live stream needed by an HLS segment request, then read the segment.
    pub async fn get_segment_with_pull(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        segment_name: &str,
        external_source_url: Option<&str>,
    ) -> Result<Bytes> {
        let _guard = if let Some(source_url) = external_source_url {
            Some(
                infrastructure
                    .ensure_pull_stream(room_id, media_id, Some(source_url))
                    .await?,
            )
        } else {
            None
        };
        Self::get_segment(infrastructure, room_id, media_id, segment_name).await
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
    use crate::relay::{test_registry::TestStreamRegistry, PublisherInfo};
    use chrono::Utc;
    use synctv_xiu::streamhub::define::StreamHubEvent;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    fn make_infrastructure_with_publisher(
        local_node_id: &str,
        publisher_node_id: &str,
        api_address: &str,
    ) -> std::result::Result<LiveStreamingInfrastructure, crate::error::StreamError> {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                PublisherInfo {
                    node_id: publisher_node_id.to_string(),
                    api_address: api_address.to_string(),
                    app_name: "live".to_string(),
                    user_id: String::new(),
                    started_at: Utc::now(),
                    epoch: 1,
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

    async fn recv_unpublish_event(
        event_receiver: &mut mpsc::Receiver<StreamHubEvent>,
    ) -> TestResult {
        let event = event_receiver
            .recv()
            .await
            .ok_or_else(|| test_error("expected UnPublish event"))?;
        match event {
            StreamHubEvent::UnPublish { .. } => Ok(()),
            other => Err(test_error(format!(
                "expected UnPublish event, got {other:?}"
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
    async fn test_live_proxy_prefers_registered_remote_publisher_before_external_pull() -> TestResult
    {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                PublisherInfo {
                    node_id: "node-remote".to_string(),
                    api_address: String::new(),
                    app_name: "live".to_string(),
                    user_id: String::new(),
                    started_at: Utc::now(),
                    epoch: 1,
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

        let error = infrastructure
            .ensure_pull_stream("room1", "media1", Some("http://127.0.0.1:8080/live.flv"))
            .await
            .expect_err("remote publisher relay should fail fast because api_address is empty");

        assert!(
            error.to_string().contains("Failed to create pull stream")
                || error.to_string().contains("api_address"),
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
            PublisherInfo {
                node_id: "node-local".to_string(),
                api_address: String::new(),
                app_name: "live".to_string(),
                user_id: String::new(),
                started_at: Utc::now(),
                epoch: 1,
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
            "Local publisher with valid epoch should succeed, got error: {:?}",
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
    async fn test_shared_file_segments_are_read_from_current_node_storage() -> TestResult {
        let storage: Arc<dyn synctv_xiu::storage::HlsStorage> =
            Arc::new(synctv_xiu::storage::MemoryStorage::new());
        storage
            .write("room1", "media1", "seg1", Bytes::from_static(b"segment"))
            .await?;

        let segment_manager = Arc::new(SegmentManager::new(
            storage,
            crate::livestream::CleanupConfig::default(),
        ));
        let infrastructure = make_infrastructure_with_publisher("node-local", "node-remote", "")?
            .with_segment_manager(segment_manager)
            .with_hls_storage_backend(HlsStorageBackend::SharedFile)
            .with_hls_proxy(HlsProxyClient::with_defaults(Some(
                "cluster-secret".to_string(),
            )));

        let segment =
            HlsStreamingApi::get_segment(&infrastructure, "room1", "media1", "seg1").await?;

        assert_eq!(segment, Bytes::from_static(b"segment"));
        Ok(())
    }

    #[tokio::test]
    async fn test_kick_stream_does_not_delete_tracking_when_unpublish_signal_fails() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                PublisherInfo {
                    node_id: "node-local".to_string(),
                    api_address: "127.0.0.1:50051".to_string(),
                    app_name: "live".to_string(),
                    user_id: "user1".to_string(),
                    started_at: Utc::now(),
                    epoch: 1,
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
            registry.get_publisher("room1", "media1").await?.is_some(),
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
                PublisherInfo {
                    node_id: "node-local".to_string(),
                    api_address: "127.0.0.1:50051".to_string(),
                    app_name: "live".to_string(),
                    user_id: "user1".to_string(),
                    started_at: Utc::now(),
                    epoch: 1,
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

        recv_unpublish_event(&mut event_receiver).await?;

        assert_eq!(
            tracker.get_stream_user("room1", "media1").as_deref(),
            Some("user1"),
            "kick_stream must keep tracker entry until StreamHub processes UnPublish"
        );
        assert!(
            registry.get_publisher("room1", "media1").await?.is_some(),
            "kick_stream must keep registry entry until epoch-fenced unpublish cleanup"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_kick_user_publishers_keep_registry_and_tracker_until_unpublish_processing(
    ) -> TestResult {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([
                (
                    ("room1".to_string(), "media1".to_string()),
                    PublisherInfo {
                        node_id: "node-local".to_string(),
                        api_address: "127.0.0.1:50051".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: Utc::now(),
                        epoch: 1,
                    },
                ),
                (
                    ("room2".to_string(), "media2".to_string()),
                    PublisherInfo {
                        node_id: "node-local".to_string(),
                        api_address: "127.0.0.1:50051".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: Utc::now(),
                        epoch: 1,
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
            recv_unpublish_event(&mut event_receiver).await?;
        }

        let remaining_streams = tracker.get_user_streams("user1");
        assert_eq!(remaining_streams.len(), 2);
        assert!(
            registry.get_publisher("room1", "media1").await?.is_some(),
            "kick_user_publishers must keep the first registry entry until unpublish cleanup"
        );
        assert!(
            registry.get_publisher("room2", "media2").await?.is_some(),
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
                    PublisherInfo {
                        node_id: "node-local".to_string(),
                        api_address: "127.0.0.1:50051".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: Utc::now(),
                        epoch: 1,
                    },
                ),
                (
                    ("room2".to_string(), "media2".to_string()),
                    PublisherInfo {
                        node_id: "node-local".to_string(),
                        api_address: "127.0.0.1:50051".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: Utc::now(),
                        epoch: 1,
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

        recv_unpublish_event(&mut event_receiver).await?;
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
            registry.get_publisher("room1", "media1").await?.is_some(),
            "target room publisher must remain registered until unpublish cleanup"
        );
        assert!(
            registry.get_publisher("room2", "media2").await?.is_some(),
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
                    PublisherInfo {
                        node_id: "node-local".to_string(),
                        api_address: "127.0.0.1:50051".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: Utc::now(),
                        epoch: 1,
                    },
                ),
                (
                    ("room2".to_string(), "media2".to_string()),
                    PublisherInfo {
                        node_id: "node-remote".to_string(),
                        api_address: "127.0.0.1:50052".to_string(),
                        app_name: "live".to_string(),
                        user_id: "user1".to_string(),
                        started_at: Utc::now(),
                        epoch: 1,
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

        recv_unpublish_event(&mut event_receiver).await?;
        assert!(
            event_receiver.try_recv().is_err(),
            "remote publisher must not be kicked by a non-owner replica"
        );
        assert!(
            registry.get_publisher("room1", "media1").await?.is_some(),
            "local publisher must remain registered until its owner processes UnPublish"
        );
        assert!(
            registry.get_publisher("room2", "media2").await?.is_some(),
            "remote publisher must remain registered for its owner node to terminate"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_kick_stream_skips_remote_publisher_registry_entry() -> TestResult {
        let registry = Arc::new(TestStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                PublisherInfo {
                    node_id: "node-remote".to_string(),
                    api_address: "127.0.0.1:50052".to_string(),
                    app_name: "live".to_string(),
                    user_id: "user1".to_string(),
                    started_at: Utc::now(),
                    epoch: 1,
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
            registry.get_publisher("room1", "media1").await?.is_some(),
            "non-owner replica must not remove remote publisher registry entry"
        );
        Ok(())
    }
}
