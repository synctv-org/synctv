// Live streaming API abstractions for synctv-api integration
//
// This module provides flexible APIs and abstractions for implementing
// live streaming HTTP endpoints in synctv-api.
//
// Architecture:
// - synctv-stream provides infrastructure + abstractions (this module)
// - synctv-api implements HTTP endpoints using these abstractions
//
// Features:
// - Lazy-load FLV streaming (create pull streams on demand)
// - HLS streaming with M3U8 playlist generation
// - HLS proxy for cluster mode (fetch from publisher node via gRPC)
// - GOP cache for instant playback
// - Publisher/Puller architecture
// - Cross-node gRPC relay

use crate::{
    grpc::HlsProxyClient,
    livestream::{
        external_publish_manager::ExternalPublishManager, pull_manager::PullStreamManager,
        segment_manager::SegmentManager,
    },
    protocols::hls::remuxer::StreamRegistry as HlsStreamRegistry,
    protocols::httpflv::HttpFlvSession,
    relay::StreamRegistryTrait,
};
use anyhow::Result;
use bytes::Bytes;
use std::sync::Arc;
use synctv_xiu::streamhub::define::StreamHubEventSender;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

pub use super::tracker::{StreamSubscriberGuard, StreamTracker};

/// Live streaming infrastructure bundle
///
/// Provides all necessary components for implementing live streaming endpoints:
/// - FLV streaming sessions
/// - HLS playlist generation (local or proxied from publisher node)
/// - HLS segment serving (local or proxied from publisher node)
/// - Publisher discovery
/// - GOP cache access
#[derive(Clone)]
pub struct LiveStreamingInfrastructure {
    /// Registry for finding publishers (Redis)
    pub registry: Arc<dyn StreamRegistryTrait>,
    /// `StreamHub` event sender for subscribing to streams
    pub stream_hub_event_sender: StreamHubEventSender,
    /// Pull stream manager for gRPC relay (cross-node pull)
    pub pull_manager: Arc<PullStreamManager>,
    /// External publish manager for pull-to-publish streams (RTMP/HTTP-FLV sources)
    pub external_publish_manager: Arc<ExternalPublishManager>,
    /// Segment manager for HLS storage
    pub segment_manager: Option<Arc<SegmentManager>>,
    /// HLS stream registry for M3U8 generation
    pub hls_stream_registry: Option<HlsStreamRegistry>,
    /// Tracks active RTMP publishers by `user_id` for kick-on-ban
    pub user_stream_tracker: Arc<StreamTracker>,
    /// Local node ID for comparing with publisher node
    pub local_node_id: String,
    /// HLS proxy client for fetching playlists/segments from remote publisher nodes
    pub hls_proxy: Option<HlsProxyClient>,
}

impl LiveStreamingInfrastructure {
    /// Create new live streaming infrastructure
    pub fn new(
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        pull_manager: Arc<PullStreamManager>,
        external_publish_manager: Arc<ExternalPublishManager>,
        user_stream_tracker: Arc<StreamTracker>,
    ) -> Self {
        Self {
            registry,
            stream_hub_event_sender,
            pull_manager,
            external_publish_manager,
            segment_manager: None,
            hls_stream_registry: None,
            user_stream_tracker,
            local_node_id: String::new(),
            hls_proxy: None,
        }
    }

    /// Add HLS segment manager
    #[must_use]
    pub fn with_segment_manager(mut self, segment_manager: Arc<SegmentManager>) -> Self {
        self.segment_manager = Some(segment_manager);
        self
    }

    /// Add HLS stream registry
    #[must_use]
    pub fn with_hls_stream_registry(mut self, hls_stream_registry: HlsStreamRegistry) -> Self {
        self.hls_stream_registry = Some(hls_stream_registry);
        self
    }

    /// Set the local node ID (used to determine if publisher is local)
    #[must_use]
    pub fn with_local_node_id(mut self, node_id: String) -> Self {
        self.local_node_id = node_id;
        self
    }

    /// Set the HLS proxy client for cross-node HLS streaming
    #[must_use]
    pub fn with_hls_proxy(mut self, hls_proxy: HlsProxyClient) -> Self {
        self.hls_proxy = Some(hls_proxy);
        self
    }

    /// Kick an active RTMP publisher, forcing their session to disconnect.
    ///
    /// Sends an `UnPublish` event through `StreamHub` which terminates the transceiver's data pipeline.
    /// The RTMP session naturally terminates when its `data_sender` channel closes.
    ///
    /// Returns Ok(()) if the event was sent. The actual disconnection is asynchronous.
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

    /// Kick all active RTMP publishers for a given user.
    ///
    /// Looks up all of the user's active streams from the tracker and sends `UnPublish` events.
    /// Used when banning or deleting a user to terminate all their RTMP publish sessions.
    pub async fn kick_user_publishers(&self, user_id: &str) {
        let streams = self.user_stream_tracker.get_user_streams(user_id);
        for (room_id, media_id) in streams {
            info!(
                user_id = %user_id,
                room_id = %room_id,
                media_id = %media_id,
                "Kicking RTMP publisher for banned user"
            );
            if let Err(e) = self.kick_stream(&room_id, &media_id).await {
                error!("Failed to kick publisher for user {}: {}", user_id, e);
                continue;
            }
        }
    }

    /// Kick all active RTMP publishers for a given user within a specific room.
    ///
    /// This preserves room-scoped moderation semantics: a room ban/kick must not
    /// terminate the same user's publishers in other rooms.
    pub async fn kick_user_room_publishers(&self, room_id: &str, user_id: &str) {
        let streams = self.user_stream_tracker.get_user_streams(user_id);
        for (stream_room_id, media_id) in streams {
            if stream_room_id != room_id {
                continue;
            }

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

    /// Kick all active RTMP publishers in a given room.
    ///
    /// Uses the room->media index for O(1) lookup instead of scanning all entries.
    /// Used when banning or deleting a room.
    pub async fn kick_room_publishers(&self, room_id: &str) {
        let media_ids = self.user_stream_tracker.get_room_streams(room_id);

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
                continue;
            }
        }
    }

    /// Kick a specific stream by `room_id` and `media_id`.
    ///
    /// Removes the publisher from Redis and sends an `UnPublish` event.
    pub async fn kick_stream(&self, room_id: &str, media_id: &str) -> Result<()> {
        // Send UnPublish to StreamHub
        self.kick_publisher(room_id, media_id)?;

        let _ = self.user_stream_tracker.remove_stream(room_id, media_id);
        self.registry.unregister_publisher(room_id, media_id).await?;

        Ok(())
    }

    /// Ensure a pull stream exists for the given room/media.
    ///
    /// Unified entry point that handles both gRPC relay and external pull:
    /// 1. If a publisher exists in Redis -> gRPC relay (cross-node)
    /// 2. If no publisher + `external_source_url` provided -> external pull (lazy start)
    /// 3. If no publisher + no URL -> error
    ///
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
            .map_err(|e| anyhow::anyhow!("Failed to check publisher: {e}"))?;

        if let Some(publisher_info) = publisher {
            let is_local =
                !self.local_node_id.is_empty() && publisher_info.node_id == self.local_node_id;

            if is_local {
                // LIVE-003 fix: Even for local publishers, validate epoch to detect
                // stale streams from crashed and restarted publishers
                match self.registry.validate_epoch(room_id, media_id, publisher_info.epoch).await {
                    Ok(true) => {
                        tracing::debug!(
                            "Epoch {} validated for local publisher {}/{}",
                            publisher_info.epoch, room_id, media_id
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
                            publisher_info.epoch, room_id, media_id
                        ));
                    }
                    Err(e) => {
                        error!(
                            "Failed to validate epoch for local publisher {}/{}: {}. \
                             Rejecting to prevent potential split-brain.",
                            room_id, media_id, e
                        );
                        return Err(anyhow::anyhow!(
                            "Epoch validation failed for local publisher {}/{}: {e}",
                            room_id, media_id
                        ));
                    }
                }
            }

            // Publisher found in Redis -- create gRPC relay pull stream
            let stream = self
                .pull_manager
                .get_or_create_pull_stream(room_id, media_id)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create pull stream: {e}"))?;
            let guard = StreamSubscriberGuard::new(move || stream.decrement_subscriber_count());
            return Ok(guard);
        }

        // No publisher in Redis -- try external publish if URL provided
        if let Some(source_url) = external_source_url {
            let stream = self
                .external_publish_manager
                .get_or_create(room_id, media_id, source_url)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create external publish stream: {e}"))?;
            let guard = StreamSubscriberGuard::new(move || stream.decrement_subscriber_count());
            return Ok(guard);
        }

        Err(anyhow::anyhow!(
            "No publisher found for {room_id}/{media_id}"
        ))
    }

    /// Get the registry (for admin queries)
    #[must_use]
    pub fn registry(&self) -> &Arc<dyn StreamRegistryTrait> {
        &self.registry
    }

    /// Check if publisher exists for a room/media
    pub async fn has_publisher(&self, room_id: &str, media_id: &str) -> Result<bool> {
        self.registry
            .get_publisher(room_id, media_id)
            .await
            .map(|opt| opt.is_some())
            .map_err(|e| anyhow::anyhow!("Failed to check publisher: {e}"))
    }

    /// Get publisher info
    pub async fn get_publisher(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<crate::relay::PublisherInfo> {
        self.registry
            .get_publisher(room_id, media_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("No publisher found for {room_id}/{media_id}"))
    }
}

/// FLV streaming API
///
/// Provides methods for creating FLV streaming sessions
pub struct FlvStreamingApi;

impl FlvStreamingApi {
    /// Create a new FLV streaming session
    ///
    /// Returns a channel receiver that streams FLV data.
    /// The caller is responsible for converting this to an HTTP response.
    ///
    /// # Arguments
    /// * `infrastructure` - Live streaming infrastructure
    /// * `room_id` - Room identifier
    /// * `media_id` - Media/stream identifier
    ///
    /// # Returns
    /// A bounded channel receiver that yields FLV data chunks
    async fn create_session(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
    ) -> Result<mpsc::Receiver<Result<Bytes, std::io::Error>>> {
        // Ensure publisher exists
        infrastructure
            .has_publisher(room_id, media_id)
            .await?
            .then_some(())
            .ok_or_else(|| anyhow::anyhow!("No publisher for {room_id}/{media_id}"))?;

        // Create bounded channel for FLV data (backpressure for slow clients)
        let (tx, rx) = mpsc::channel(synctv_xiu::httpflv::FLV_RESPONSE_CHANNEL_CAPACITY);

        // Create FLV session using canonical (room_id, media_id) StreamIdentifier
        let mut flv_session = HttpFlvSession::new(
            room_id.to_string(),
            media_id.to_string(),
            infrastructure.stream_hub_event_sender.clone(),
            tx,
        );

        // Spawn FLV session task
        tokio::spawn(async move {
            if let Err(e) = flv_session.run().await {
                error!("FLV session error: {}", e);
            }
        });

        Ok(rx)
    }

    /// Create FLV streaming session with lazy-load pull
    ///
    /// This ensures a pull stream is created if one doesn't exist.
    /// Supports both cross-node gRPC relay and external source pulling.
    ///
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
        // Ensure pull stream exists (gRPC relay or external)
        let guard = infrastructure
            .ensure_pull_stream(room_id, media_id, external_source_url)
            .await?;

        // Create FLV session (subscribes to local StreamHub)
        let rx = Self::create_session(infrastructure, room_id, media_id).await?;
        Ok((rx, guard))
    }
}

/// HLS streaming API
///
/// Provides methods for HLS playlist generation and segment serving.
/// In cluster mode, automatically proxies requests to the publisher node
/// if the publisher is on a different node.
pub struct HlsStreamingApi;

impl HlsStreamingApi {
    /// Generate HLS M3U8 playlist for a stream.
    ///
    /// In cluster mode:
    /// - If publisher is local: generates from local HLS stream registry
    /// - If publisher is remote: proxies to publisher node via gRPC
    ///
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
        // Get publisher info to determine if local or remote
        let publisher_info = infrastructure
            .registry
            .get_publisher(room_id, media_id)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to check publisher: {e}"))?;

        let publisher_info = match publisher_info {
            Some(info) => info,
            None => {
                return Ok(None);
            }
        };

        // Check if publisher is local
        let is_local = !infrastructure.local_node_id.is_empty()
            && publisher_info.node_id == infrastructure.local_node_id;

        if is_local {
            // Local publisher: read from local HLS stream registry
            Self::generate_playlist_local(infrastructure, room_id, media_id, url_generator)
        } else if let Some(hls_proxy) = &infrastructure.hls_proxy {
            // Validate API address before attempting remote proxy
            let api_addr = publisher_info
                .validate_api_address()
                .map_err(|e| anyhow::anyhow!("Cannot proxy HLS for {room_id}/{media_id}: {e}"))?;

            // Remote publisher: proxy via gRPC
            // We need a segment_url_base for the remote node to generate URLs.
            // The remote node will use this base to construct segment URLs in the M3U8.
            // We generate a representative URL to extract the base pattern.
            let sample_url = url_generator("__PLACEHOLDER__");
            let segment_url_base = sample_url
                .rsplit_once("__PLACEHOLDER__")
                .map(|(base, _)| base.to_string())
                .unwrap_or_default();

            let playlist = hls_proxy
                .get_playlist(
                    api_addr,
                    room_id,
                    media_id,
                    &segment_url_base,
                    publisher_info.epoch,
                )
                .await?;

            Ok(playlist)
        } else {
            // No proxy configured, try local anyway (single-node mode)
            Self::generate_playlist_local(infrastructure, room_id, media_id, url_generator)
        }
    }

    /// Generate playlist from local HLS stream registry.
    ///
    /// Returns `Ok(None)` if the stream is not yet in the HLS registry
    /// (publisher exists but no segments have been generated yet).
    fn generate_playlist_local<F>(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        url_generator: F,
    ) -> Result<Option<String>>
    where
        F: Fn(&str) -> String,
    {
        if let Some(hls_registry) = &infrastructure.hls_stream_registry {
            // Registry key format: "room_id/media_id" (matches remuxer's app_name/stream_name)
            let stream_key = format!("{room_id}/{media_id}");

            match hls_registry.get(&stream_key) {
                Some(stream_state) => {
                    let state = stream_state.read();
                    // Use caller-provided URL generator for maximum flexibility
                    Ok(Some(state.generate_m3u8(url_generator)))
                }
                None => {
                    // Stream not in registry yet — signal caller to return 404
                    Ok(None)
                }
            }
        } else {
            // No HLS registry configured — signal caller to return 404
            Ok(None)
        }
    }

    /// Generate HLS M3U8 playlist with simple base URL (convenience method).
    ///
    /// Returns `Ok(None)` when the stream does not exist (HTTP handler should return 404).
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

    /// Get HLS segment data.
    ///
    /// In cluster mode:
    /// - If publisher is local: reads from local `SegmentManager`
    /// - If publisher is remote: proxies to publisher node via gRPC (with local cache)
    ///
    /// HLS segment requests do NOT trigger gRPC RTMP pull streams.
    pub async fn get_segment(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        segment_name: &str,
    ) -> Result<Bytes> {
        // Get publisher info to determine if local or remote
        let publisher_info = infrastructure
            .registry
            .get_publisher(room_id, media_id)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to check publisher: {e}"))?;

        let publisher_info = match publisher_info {
            Some(info) => info,
            None => {
                return Err(anyhow::anyhow!("No publisher for {room_id}/{media_id}"));
            }
        };

        // Check if publisher is local
        let is_local = !infrastructure.local_node_id.is_empty()
            && publisher_info.node_id == infrastructure.local_node_id;

        if is_local {
            // Local publisher: read from local storage
            Self::get_segment_local(infrastructure, room_id, media_id, segment_name).await
        } else if let Some(hls_proxy) = &infrastructure.hls_proxy {
            // Validate API address before attempting remote proxy
            let api_addr = publisher_info.validate_api_address().map_err(|e| {
                anyhow::anyhow!("Cannot proxy HLS segment for {room_id}/{media_id}: {e}")
            })?;

            // Remote publisher: proxy via gRPC (with local cache)
            let segment = hls_proxy
                .get_segment(
                    api_addr,
                    room_id,
                    media_id,
                    segment_name,
                    publisher_info.epoch,
                )
                .await?;

            segment.ok_or_else(|| anyhow::anyhow!("Segment not found on publisher node"))
        } else {
            // No proxy configured, try local anyway (single-node mode)
            Self::get_segment_local(infrastructure, room_id, media_id, segment_name).await
        }
    }

    /// Get segment from local storage.
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
                .map_err(|e| anyhow::anyhow!("Failed to read segment: {e}"))
        } else {
            Err(anyhow::anyhow!("Segment manager not configured"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::{mock_registry::MockStreamRegistry, PublisherInfo};
    use chrono::Utc;

    fn make_infrastructure_with_publisher(
        local_node_id: &str,
        publisher_node_id: &str,
        api_address: &str,
    ) -> LiveStreamingInfrastructure {
        let registry = Arc::new(MockStreamRegistry::with_publishers(
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
        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                local_node_id.to_string(),
                event_sender.clone(),
            )
            .expect("external publish manager should build"),
        );

        LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            pull_manager,
            external_publish_manager,
            Arc::new(StreamTracker::new()),
        )
        .with_local_node_id(local_node_id.to_string())
    }

    #[tokio::test]
    async fn test_ensure_pull_stream_skips_grpc_relay_for_local_publisher() {
        let infrastructure = make_infrastructure_with_publisher("node-local", "node-local", "");

        let guard = infrastructure
            .ensure_pull_stream("room1", "media1", None)
            .await
            .expect("local publisher should not require a relay stream");

        drop(guard);
    }

    #[tokio::test]
    async fn test_ensure_pull_stream_validates_epoch_for_local_publisher() {
        use std::collections::HashMap;

        let registry = Arc::new(MockStreamRegistry::with_publishers(HashMap::from([(
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
        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "node-local".to_string(),
                event_sender.clone(),
            )
            .expect("external publish manager should build"),
        );

        let infrastructure = LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            pull_manager,
            external_publish_manager,
            Arc::new(StreamTracker::new()),
        )
        .with_local_node_id("node-local".to_string());

        let result = infrastructure.ensure_pull_stream("room1", "media1", None).await;
        assert!(
            result.is_ok(),
            "Local publisher with valid epoch should succeed, got error: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_create_session_with_pull_keeps_local_publishers_local() {
        let infrastructure = make_infrastructure_with_publisher("node-local", "node-local", "");

        let result =
            FlvStreamingApi::create_session_with_pull(&infrastructure, "room1", "media1", None)
                .await;

        assert!(
            result.is_ok(),
            "local publisher should create FLV session without requiring gRPC pull"
        );
    }

    #[tokio::test]
    async fn test_kick_stream_does_not_delete_tracking_when_unpublish_signal_fails() {
        let registry = Arc::new(MockStreamRegistry::with_publishers(
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
            "rtmp-room",
            "rtmp-stream",
        );

        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "node-local".to_string(),
                event_sender.clone(),
            )
            .expect("external publish manager should build"),
        );

        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender.clone(),
            pull_manager,
            external_publish_manager,
            tracker.clone(),
        );

        let err = infrastructure
            .kick_stream("room1", "media1")
            .await
            .expect_err("closed StreamHub channel should fail the kick");

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
                .get_publisher("room1", "media1")
                .await
                .expect("registry lookup should succeed")
                .is_some(),
            "registry entry must remain until UnPublish is accepted"
        );
    }

    #[tokio::test]
    async fn test_kick_stream_cleans_up_registry_and_tracker_after_accepting_unpublish() {
        let registry = Arc::new(MockStreamRegistry::with_publishers(
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
            "rtmp-room",
            "rtmp-stream",
        );

        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "node-local".to_string(),
                event_sender.clone(),
            )
            .expect("external publish manager should build"),
        );

        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            pull_manager,
            external_publish_manager,
            tracker.clone(),
        );

        infrastructure
            .kick_stream("room1", "media1")
            .await
            .expect("accepted UnPublish should not fail");

        let event = event_receiver
            .recv()
            .await
            .expect("kick_stream should enqueue an UnPublish event");
        match event {
            synctv_xiu::streamhub::define::StreamHubEvent::UnPublish { .. } => {}
            other => panic!("expected UnPublish event, got {other:?}"),
        }

        assert_eq!(
            tracker.get_stream_user("room1", "media1"),
            None,
            "kick_stream must remove tracker entry once UnPublish is accepted"
        );
        assert!(
            registry
                .get_publisher("room1", "media1")
                .await
                .expect("registry lookup should succeed")
                .is_none(),
            "kick_stream must remove registry entry once UnPublish is accepted"
        );
    }

    #[tokio::test]
    async fn test_kick_user_publishers_clean_up_registry_and_tracker_after_accepting_unpublish() {
        let registry = Arc::new(MockStreamRegistry::with_publishers(
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
            "rtmp-room-1",
            "rtmp-stream-1",
        );
        tracker.insert(
            "user1".to_string(),
            "room2".to_string(),
            "media2".to_string(),
            "rtmp-room-2",
            "rtmp-stream-2",
        );

        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "node-local".to_string(),
                event_sender.clone(),
            )
            .expect("external publish manager should build"),
        );

        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            pull_manager,
            external_publish_manager,
            tracker.clone(),
        );

        infrastructure.kick_user_publishers("user1").await;

        for _ in 0..2 {
            let event = event_receiver
                .recv()
                .await
                .expect("kick_user_publishers should enqueue UnPublish events");
            match event {
                synctv_xiu::streamhub::define::StreamHubEvent::UnPublish { .. } => {}
                other => panic!("expected UnPublish event, got {other:?}"),
            }
        }

        let remaining_streams = tracker.get_user_streams("user1");
        assert_eq!(
            remaining_streams.len(),
            0,
            "kick_user_publishers must remove tracker entries once UnPublish is accepted"
        );
        assert!(
            registry
                .get_publisher("room1", "media1")
                .await
                .expect("registry lookup should succeed")
                .is_none(),
            "kick_user_publishers must remove the first registry entry once UnPublish is accepted"
        );
        assert!(
            registry
                .get_publisher("room2", "media2")
                .await
                .expect("registry lookup should succeed")
                .is_none(),
            "kick_user_publishers must remove the second registry entry once UnPublish is accepted"
        );
    }

    #[tokio::test]
    async fn test_kick_user_room_publishers_only_removes_streams_in_target_room() {
        let registry = Arc::new(crate::relay::MockStreamRegistry::with_publishers(
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
            "rtmp-room-1",
            "rtmp-stream-1",
        );
        tracker.insert(
            "user1".to_string(),
            "room2".to_string(),
            "media2".to_string(),
            "rtmp-room-2",
            "rtmp-stream-2",
        );

        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "node-local".to_string(),
                event_sender.clone(),
            )
            .expect("external publish manager should build"),
        );

        let infrastructure = LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            pull_manager,
            external_publish_manager,
            tracker.clone(),
        );

        infrastructure.kick_user_room_publishers("room1", "user1").await;

        let event = event_receiver
            .recv()
            .await
            .expect("kick_user_room_publishers should enqueue one UnPublish event");
        match event {
            synctv_xiu::streamhub::define::StreamHubEvent::UnPublish { .. } => {}
            other => panic!("expected UnPublish event, got {other:?}"),
        }
        assert!(
            event_receiver.try_recv().is_err(),
            "room-scoped kick should only enqueue the room-local publisher"
        );

        assert!(
            tracker.get_stream_user("room1", "media1").is_none(),
            "target room publisher must be removed from tracker"
        );
        assert_eq!(
            tracker.get_stream_user("room2", "media2").as_deref(),
            Some("user1"),
            "publishers in other rooms must remain tracked"
        );
        assert!(
            registry
                .get_publisher("room1", "media1")
                .await
                .expect("registry lookup should succeed")
                .is_none(),
            "target room publisher must be removed from registry"
        );
        assert!(
            registry
                .get_publisher("room2", "media2")
                .await
                .expect("registry lookup should succeed")
                .is_some(),
            "publishers in other rooms must remain registered"
        );
    }
}
