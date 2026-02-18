// RTMP Authentication implementation using xiu's AuthCallback trait
//
// JWT Claims structure (from synctv-go internal/rtmp/rtmp.go):
// type Claims struct {
//     MovieID string `json:"m"`
//     jwt.RegisteredClaims
// }
//
// RTMP URL scheme:
// - Publisher: rtmp://host/room_id/JWT_TOKEN  or  rtmp://host/room_id/media_id?token=JWT
// - Player:   rtmp://host/room_id/media_id

use synctv_xiu::rtmp::auth::{AuthCallback, AuthPublishRewrite};
use async_trait::async_trait;
use std::sync::Arc;
use synctv_core::service::PublishKeyService;
use tracing::{debug, error, info, warn};
use crate::relay::registry_trait::StreamRegistryTrait;

pub struct RtmpAuthCallbackImpl {
    publish_key_service: Arc<PublishKeyService>,
    /// Optional stream tracker for cleanup on unpublish.
    /// When set, `on_unpublish` removes the publisher from the tracker.
    stream_tracker: Option<Arc<crate::api::StreamTracker>>,
    /// Optional publisher registry for atomic registration in `on_publish`.
    /// When set, publishers are atomically registered in Redis BEFORE the
    /// `StreamHub` Publish event, ensuring no window where a publisher is
    /// active in `StreamHub` but not registered in the cluster registry.
    registry: Option<Arc<dyn StreamRegistryTrait>>,
    /// Node ID for publisher registration (required if registry is set).
    node_id: String,
    /// Advertised gRPC address for cross-node proxying.
    grpc_address: String,
}

impl RtmpAuthCallbackImpl {
    #[must_use]
    pub fn new(publish_key_service: Arc<PublishKeyService>) -> Self {
        Self {
            publish_key_service,
            stream_tracker: None,
            registry: None,
            node_id: String::new(),
            grpc_address: String::new(),
        }
    }

    /// Create with a stream tracker for automatic cleanup on unpublish.
    #[must_use]
    pub fn with_stream_tracker(
        publish_key_service: Arc<PublishKeyService>,
        stream_tracker: Arc<crate::api::StreamTracker>,
    ) -> Self {
        Self {
            publish_key_service,
            stream_tracker: Some(stream_tracker),
            registry: None,
            node_id: String::new(),
            grpc_address: String::new(),
        }
    }

    /// Enable atomic publisher registration in Redis during `on_publish`.
    /// This ensures the publisher is registered BEFORE the `StreamHub` Publish event,
    /// preventing a window where a publisher is active but not discoverable by other nodes.
    #[must_use]
    pub fn with_registry(
        mut self,
        registry: Arc<dyn StreamRegistryTrait>,
        node_id: String,
        grpc_address: String,
    ) -> Self {
        self.registry = Some(registry);
        self.node_id = node_id;
        self.grpc_address = grpc_address;
        self
    }
}

#[async_trait]
impl AuthCallback for RtmpAuthCallbackImpl {
    async fn on_publish(
        &self,
        app_name: &str,
        stream_name: &str,
        query: Option<&str>,
    ) -> Result<Option<AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>> {
        debug!(
            "RTMP publish auth: app={}, stream={}, query={:?}",
            app_name,
            stream_name,
            query
        );

        // Extract token: prefer query string parameter, fall back to stream_name as token
        let token = if let Some(q) = query {
            extract_token_from_query(q).unwrap_or(stream_name)
        } else {
            stream_name
        };

        let claims = self
            .publish_key_service
            .validate_publish_key(token)
            .await
            .map_err(|e| format!("Authentication failed: {e}"))?;

        // Verify room_id matches app_name
        if claims.room_id != app_name {
            return Err(format!(
                "Room ID mismatch: token has {}, request is for {}",
                claims.room_id, app_name
            )
            .into());
        }

        info!(
            "RTMP publisher authenticated: room_id={}, media_id={}, user_id={}",
            claims.room_id,
            claims.media_id,
            claims.user_id
        );

        // Atomic registration in Redis BEFORE StreamHub Publish event.
        // This ensures the publisher is discoverable by other nodes as soon as
        // the stream starts, with no window where it's active but unregistered.
        if let Some(ref registry) = self.registry {
            let registered = registry
                .try_register_publisher(
                    &claims.room_id,
                    &claims.media_id,
                    &self.node_id,
                    &claims.user_id,
                    &self.grpc_address,
                )
                .await
                .map_err(|e| format!("Failed to register publisher in Redis: {e}"))?;

            if !registered {
                return Err(format!(
                    "Another publisher is already active for media {} in room {}",
                    claims.media_id, claims.room_id
                )
                .into());
            }

            info!(
                "Publisher registered atomically in auth phase: room={}, media={}, node={}",
                claims.room_id, claims.media_id, self.node_id
            );
        }

        // Register in stream tracker so kick_user/room_publishers can find this publisher
        if let Some(tracker) = &self.stream_tracker {
            tracker.insert(
                claims.user_id.clone(),
                claims.room_id.clone(),
                claims.media_id.clone(),
                app_name,
                stream_name,
            );
        }

        // Return rewrite so StreamHub uses canonical (room_id, media_id)
        Ok(Some(AuthPublishRewrite {
            app_name: claims.room_id,
            stream_name: claims.media_id,
        }))
    }

    async fn on_play(
        &self,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        warn!(
            "RTMP play rejected: room_id={}, media_id={} — use HTTP-FLV or HLS",
            app_name,
            stream_name
        );
        Err("RTMP pull is disabled. Use HTTP-FLV or HLS endpoints for playback.".into())
    }

    async fn on_unpublish(
        &self,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) {
        let tracked = if let Some(tracker) = &self.stream_tracker {
            tracker.remove_by_app_stream(app_name, stream_name)
        } else {
            None
        };

        if let Some((ref user_id, ref room_id, ref media_id)) = tracked {
            info!(
                user_id = %user_id,
                room_id = %room_id,
                media_id = %media_id,
                "RTMP publisher unpublished, removed from tracker"
            );

            // Unregister from Redis if registry is configured
            if let Some(ref registry) = self.registry {
                if let Err(e) = registry.unregister_publisher(room_id, media_id).await {
                    error!(
                        room_id = %room_id,
                        media_id = %media_id,
                        "Failed to unregister publisher from Redis: {}", e
                    );
                }
            }
        } else {
            warn!(
                app_name = %app_name,
                "on_unpublish: no matching stream found in tracker"
            );
            // Best-effort: try to unregister using app_name/stream_name directly
            // (these are the canonical room_id/media_id after auth rewrite)
            if let Some(ref registry) = self.registry {
                if let Err(e) = registry.unregister_publisher(app_name, stream_name).await {
                    error!(
                        app_name = %app_name,
                        stream_name = %stream_name,
                        "Failed to unregister publisher from Redis (fallback): {}", e
                    );
                }
            }
        }
    }

    async fn on_unplay(
        &self,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) {
        info!(
            "RTMP player disconnected: room_id={}, media_id={}",
            app_name,
            stream_name
        );
    }
}

/// Extract token from query string (e.g. "token=xxx&foo=bar" -> "xxx")
fn extract_token_from_query(query: &str) -> Option<&str> {
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("token=") {
            return Some(value);
        }
    }
    None
}
