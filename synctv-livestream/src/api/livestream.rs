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
    relay::StreamRegistryTrait,
    livestream::{
        pull_manager::PullStreamManager,
        external_publish_manager::ExternalPublishManager,
        segment_manager::SegmentManager,
    },
    protocols::hls::remuxer::StreamRegistry as HlsStreamRegistry,
    protocols::httpflv::HttpFlvSession,
    grpc::HlsProxyClient,
};
use anyhow::Result;
use bytes::Bytes;
use std::sync::Arc;
use synctv_xiu::streamhub::define::StreamHubEventSender;
use tokio::sync::mpsc;
use tracing::{error, info};

pub use super::tracker::{StreamSubscriberGuard, StreamTracker, UserStreamTracker};

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
    pub user_stream_tracker: UserStreamTracker,
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
        user_stream_tracker: UserStreamTracker,
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
            .map_err(|_| anyhow::anyhow!("Failed to send unpublish event (StreamHub not running)"))?;

        Ok(())
    }

    /// Kick all active RTMP publishers for a given user.
    ///
    /// Looks up all of the user's active streams from the tracker and sends `UnPublish` events.
    /// Used when banning or deleting a user to terminate all their RTMP publish sessions.
    pub fn kick_user_publishers(&self, user_id: &str) {
        let streams = self.user_stream_tracker.remove_user(user_id);
        for (room_id, media_id) in streams {
            info!(
                user_id = %user_id,
                room_id = %room_id,
                media_id = %media_id,
                "Kicking RTMP publisher for banned user"
            );
            if let Err(e) = self.kick_publisher(&room_id, &media_id) {
                error!("Failed to kick publisher for user {}: {}", user_id, e);
            }
        }
    }

    /// Kick all active RTMP publishers in a given room.
    ///
    /// Uses the room->media index for O(1) lookup instead of scanning all entries.
    /// Used when banning or deleting a room.
    pub fn kick_room_publishers(&self, room_id: &str) {
        let media_ids = self.user_stream_tracker.get_room_streams(room_id);

        for media_id in media_ids {
            if let Some(user_id) = self.user_stream_tracker.remove_stream(room_id, &media_id) {
                info!(
                    user_id = %user_id,
                    room_id = %room_id,
                    media_id = %media_id,
                    "Kicking RTMP publisher for banned room"
                );
            }
            if let Err(e) = self.kick_publisher(room_id, &media_id) {
                error!("Failed to kick publisher in room {}: {}", room_id, e);
            }
        }
    }

    /// Kick a specific stream by `room_id` and `media_id`.
    ///
    /// Removes the publisher from Redis and sends an `UnPublish` event.
    pub async fn kick_stream(&self, room_id: &str, media_id: &str) -> Result<()> {
        // Remove from Redis registry
        self.registry
            .unregister_publisher(room_id, media_id)
            .await?;

        // Remove from local tracker
        let _ = self.user_stream_tracker.remove_stream(room_id, media_id);

        // Send UnPublish to StreamHub
        self.kick_publisher(room_id, media_id)?;

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
        let publisher = self.registry.get_publisher(room_id, media_id).await
            .map_err(|e| anyhow::anyhow!("Failed to check publisher: {e}"))?;

        if publisher.is_some() {
            // Publisher found in Redis -- create gRPC relay pull stream
            let stream = self.pull_manager
                .get_or_create_pull_stream(room_id, media_id)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create pull stream: {e}"))?;
            let guard = StreamSubscriberGuard::new(move || stream.decrement_subscriber_count());
            return Ok(guard);
        }

        // No publisher in Redis -- try external publish if URL provided
        if let Some(source_url) = external_source_url {
            let stream = self.external_publish_manager
                .get_or_create(room_id, media_id, source_url)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create external publish stream: {e}"))?;
            let guard = StreamSubscriberGuard::new(move || stream.decrement_subscriber_count());
            return Ok(guard);
        }

        Err(anyhow::anyhow!("No publisher found for {room_id}/{media_id}"))
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
    pub async fn get_publisher(&self, room_id: &str, media_id: &str) -> Result<crate::relay::PublisherInfo> {
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
        infrastructure.has_publisher(room_id, media_id).await?
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
    ) -> Result<(mpsc::Receiver<Result<Bytes, std::io::Error>>, StreamSubscriberGuard)> {
        // Ensure pull stream exists (gRPC relay or external)
        let guard = infrastructure.ensure_pull_stream(room_id, media_id, external_source_url).await?;

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
        let publisher_info = infrastructure.registry
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
            // Validate gRPC address before attempting remote proxy
            let grpc_addr = publisher_info.validate_grpc_address()
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
                    grpc_addr,
                    room_id,
                    media_id,
                    &segment_url_base,
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
        }).await
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
        let publisher_info = infrastructure.registry
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
            // Validate gRPC address before attempting remote proxy
            let grpc_addr = publisher_info.validate_grpc_address()
                .map_err(|e| anyhow::anyhow!("Cannot proxy HLS segment for {room_id}/{media_id}: {e}"))?;

            // Remote publisher: proxy via gRPC (with local cache)
            let segment = hls_proxy
                .get_segment(
                    grpc_addr,
                    room_id,
                    media_id,
                    segment_name,
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
            // Build storage key using the shared canonical format (matches remuxer write side)
            let storage_key = synctv_xiu::hls::hls_segment_storage_key(room_id, media_id, segment_name);

            segment_manager
                .storage()
                .read(&storage_key)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read segment: {e}"))
        } else {
            Err(anyhow::anyhow!("Segment manager not configured"))
        }
    }

    /// Generate HLS M3U8 playlist with lazy-load pull and custom URL generator.
    ///
    /// **DEPRECATED for HLS**: Use `generate_playlist` instead. HLS should NOT
    /// trigger RTMP pull streams. This method is kept for backwards compatibility
    /// but now behaves identically to `generate_playlist` (no pull stream).
    #[deprecated(since = "0.1.0", note = "Use generate_playlist instead; HLS should not trigger RTMP pull streams")]
    pub async fn generate_playlist_with_pull<F>(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        _external_source_url: Option<&str>,
        url_generator: F,
    ) -> Result<Option<String>>
    where
        F: Fn(&str) -> String,
    {
        // HLS does NOT trigger pull streams — only FLV should.
        // Just generate the playlist (local or proxied).
        Self::generate_playlist(infrastructure, room_id, media_id, url_generator).await
    }

    /// Generate HLS M3U8 playlist with lazy-load pull and simple base URL (convenience method).
    ///
    /// **DEPRECATED for HLS**: Use `generate_playlist_simple` instead.
    #[deprecated(since = "0.1.0", note = "Use generate_playlist_simple instead")]
    pub async fn generate_playlist_with_pull_simple(
        infrastructure: &LiveStreamingInfrastructure,
        room_id: &str,
        media_id: &str,
        _external_source_url: Option<&str>,
        segment_url_base: &str,
    ) -> Result<Option<String>> {
        Self::generate_playlist_simple(infrastructure, room_id, media_id, segment_url_base).await
    }
}
