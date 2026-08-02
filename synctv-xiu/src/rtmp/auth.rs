use async_trait::async_trait;

use crate::streamhub::utils::Uuid;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RtmpStreamMode {
    #[default]
    Default,
    VideoOnly,
    AudioOnly,
}

/// Optional rewrite of RTMP identifiers returned by [`AuthCallback::on_publish`].
///
/// When the auth callback resolves a JWT token in `stream_name` to a logical
/// `media_id`, it returns `Some(AuthPublishRewrite { app_name, stream_name, media_mode })`
/// so the RTMP session uses canonical identifiers and the configured media mode.
///
/// This ensures that publishers, subscribers (HLS, FLV), and gRPC relay all
/// use the same `StreamIdentifier` format: `(room_id, media_id)`.
#[derive(Debug, Clone)]
pub struct AuthPublishRewrite {
    pub app_name: String,
    pub stream_name: String,
    pub media_mode: RtmpStreamMode,
}

/// Trait for RTMP authentication callbacks.
///
/// Implementations can inject custom authentication logic (e.g. JWT validation)
/// into the RTMP server session flow.
#[async_trait]
pub trait AuthCallback: Send + Sync {
    /// Called when a client publishes (pushes) a stream.
    ///
    /// # Arguments
    /// * `generation_id` - Stable StreamHub generation for this publication session
    /// * `app_name` - RTMP application name (e.g. `room_id`)
    /// * `stream_name` - Stream name (e.g. JWT token or `media_id`)
    /// * `query` - Optional query string from the RTMP URL
    ///
    /// # Returns
    /// * `Ok(None)` - Auth succeeded, use original `app_name`/`stream_name`
    /// * `Ok(Some(rewrite))` - Auth succeeded, use rewritten identifiers for `StreamHub`
    /// * `Err(...)` - Auth failed, reject the publish
    async fn on_publish(
        &self,
        generation_id: Uuid,
        app_name: &str,
        stream_name: &str,
        query: Option<&str>,
    ) -> Result<Option<AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>>;

    /// Called when a client plays (pulls) a stream.
    ///
    /// # Arguments
    /// * `app_name` - RTMP application name (e.g. `room_id`)
    /// * `stream_name` - Stream name (e.g. `media_id`)
    /// * `query` - Optional query string from the RTMP URL
    async fn on_play(
        &self,
        app_name: &str,
        stream_name: &str,
        query: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Called when a publisher stops streaming (disconnect, error, or deleteStream).
    ///
    /// This is a fire-and-forget callback — errors are logged, not propagated.
    /// Used for cleanup of tracking state (e.g. removing user→stream mappings).
    ///
    /// # Arguments
    /// * `generation_id` - Stable StreamHub generation passed to `on_publish`
    /// * `app_name` - RTMP application name (e.g. `room_id`)
    /// * `stream_name` - Stream name (e.g. `media_id`)
    /// * `query` - Optional query string from the RTMP URL
    async fn on_unpublish(
        &self,
        _generation_id: Uuid,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) {
        // Default: no-op
    }

    /// Called when a player (subscriber) stops watching (disconnect, error, or deleteStream).
    ///
    /// This is a fire-and-forget callback — errors are logged, not propagated.
    /// Used for cleanup of viewer tracking state.
    ///
    /// # Arguments
    /// * `app_name` - RTMP application name (e.g. `room_id`)
    /// * `stream_name` - Stream name (e.g. `media_id`)
    /// * `query` - Optional query string from the RTMP URL
    async fn on_unplay(&self, _app_name: &str, _stream_name: &str, _query: Option<&str>) {
        // Default: no-op
    }

    /// Called when `on_publish` succeeded but a later step (e.g. `StreamHub` publish) failed.
    ///
    /// This is a fire-and-forget callback — errors are logged, not propagated.
    /// Used to rollback any state changes made during `on_publish`, such as
    /// unregistering a publisher from a cluster registry.
    ///
    /// # Arguments
    /// * `generation_id` - Stable StreamHub generation passed to `on_publish`
    /// * `app_name` - RTMP application name (e.g. `room_id`)
    /// * `stream_name` - Stream name (e.g. `media_id`), after any rewrite
    /// * `query` - Optional query string from the RTMP URL
    ///
    /// # When Called
    /// This method is called when:
    /// 1. `on_publish` returned `Ok(...)` (authentication succeeded)
    /// 2. A subsequent step in the publish flow failed (e.g. `publish_to_stream_hub`)
    ///
    /// This ensures that any registration done in `on_publish` is cleaned up
    /// immediately rather than waiting for TTL expiry or `on_unpublish`.
    async fn on_publish_rollback(
        &self,
        _generation_id: Uuid,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) {
        // Default: no-op
    }
}
