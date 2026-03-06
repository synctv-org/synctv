// HTTP-FLV streaming implementation
// Provides router for synctv-api integration

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Extension, Router,
};
use std::sync::Arc;
use synctv_xiu::streamhub::define::StreamHubEventSender;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::api::tracker::StreamSubscriberGuard;
use crate::api::LiveStreamingInfrastructure;
use crate::relay::StreamRegistry;

// Re-export HttpFlvSession from xiu-httpflv
pub use synctv_xiu::httpflv::HttpFlvSession;

#[derive(Clone)]
pub struct HttpFlvState {
    registry: Arc<StreamRegistry>,
    stream_hub_event_sender: StreamHubEventSender,
    /// Optional infrastructure for subscriber tracking via `StreamSubscriberGuard`.
    /// When set, each FLV session holds a guard that decrements the subscriber
    /// count on drop, ensuring correct idle-cleanup lifecycle.
    infrastructure: Option<LiveStreamingInfrastructure>,
}

impl HttpFlvState {
    #[must_use]
    pub const fn new(
        registry: Arc<StreamRegistry>,
        stream_hub_event_sender: StreamHubEventSender,
    ) -> Self {
        Self {
            registry,
            stream_hub_event_sender,
            infrastructure: None,
        }
    }

    /// Set the live streaming infrastructure for subscriber tracking.
    #[must_use]
    pub fn with_infrastructure(mut self, infra: LiveStreamingInfrastructure) -> Self {
        self.infrastructure = Some(infra);
        self
    }
}

/// Create HTTP-FLV router
/// Routes:
/// - GET /`live/flv/:media_id` - FLV streaming (requires auth with `room_id` in Extension)
pub fn create_flv_router(state: HttpFlvState) -> Router {
    Router::new()
        .route("/live/flv/:media_id", get(handle_flv_stream))
        .with_state(state)
}

/// Handle FLV streaming request
/// Path: GET /`live/flv/:media_id`
/// Requires: Extension<RoomId> from auth middleware
async fn handle_flv_stream(
    Path(media_id): Path<String>,
    Extension(room_id): Extension<String>,
    State(state): State<HttpFlvState>,
) -> Result<Response, StatusCode> {
    // Remove .flv suffix if present
    let media_id = media_id.trim_end_matches(".flv");

    info!(
        room_id = %room_id,
        media_id = %media_id,
        "FLV streaming request"
    );

    // Check if stream exists (publisher registered)
    match state.registry.get_publisher_immut(&room_id, media_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            warn!("No publisher for room {} / media {}", room_id, media_id);
            return Err(StatusCode::NOT_FOUND);
        }
        Err(e) => {
            error!("Failed to query publisher: {}", e);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    // Create bounded channel for HTTP response data (backpressure for slow clients)
    let (tx, rx) = mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(
        synctv_xiu::httpflv::FLV_RESPONSE_CHANNEL_CAPACITY,
    );

    // Create subscriber guard if infrastructure is available.
    // The guard decrements the subscriber count when dropped, ensuring
    // the idle-cleanup task correctly tracks active viewers.
    // If ensure_pull_stream fails, do NOT proceed — the FLV session must
    // not be spawned without lifecycle tracking.
    let subscriber_guard: Option<StreamSubscriberGuard> =
        if let Some(ref infra) = state.infrastructure {
            match infra.ensure_pull_stream(&room_id, media_id, None).await {
                Ok(guard) => Some(guard),
                Err(e) => {
                    warn!("Failed to create subscriber guard for FLV session: {}", e);
                    return Err(StatusCode::SERVICE_UNAVAILABLE);
                }
            }
        } else {
            None
        };

    // Spawn FLV session using canonical (room_id, media_id) StreamIdentifier
    let mut flv_session = HttpFlvSession::new(
        room_id.clone(),
        media_id.to_string(),
        state.stream_hub_event_sender,
        tx,
    );

    tokio::spawn(async move {
        let _guard = subscriber_guard; // held for the lifetime of this task
        if let Err(e) = flv_session.run().await {
            error!("FLV session error: {}", e);
        }
    });

    // Return streaming response
    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/x-flv")
        .header(header::CACHE_CONTROL, "no-cache, no-store")
        .header(header::CONNECTION, "close")
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_http_flv_state_creation() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);

        let Some(redis_conn) = try_redis_connection().await else {
            eprintln!("Redis not available, skipping test");
            return;
        };

        let registry = StreamRegistry::new(redis_conn);

        let state = HttpFlvState::new(std::sync::Arc::new(registry), event_sender);
        assert!(Arc::strong_count(&state.registry) >= 1);
    }

    #[test]
    fn test_http_flv_session_creation() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        let (response_tx, _response_rx) =
            mpsc::channel(synctv_xiu::httpflv::FLV_RESPONSE_CHANNEL_CAPACITY);

        let session = HttpFlvSession::new(
            "live".to_string(),
            "room123/media456".to_string(),
            event_sender,
            response_tx,
        );

        assert_eq!(session.app_name, "live");
        assert_eq!(session.stream_name, "room123/media456");
        assert!(!session.has_send_header);
        assert!(!session.has_audio);
        assert!(!session.has_video);
    }

    #[test]
    fn test_flv_session_defaults() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        let (response_tx, _response_rx) =
            mpsc::channel(synctv_xiu::httpflv::FLV_RESPONSE_CHANNEL_CAPACITY);

        let session = HttpFlvSession::new(
            "live".to_string(),
            "test/stream".to_string(),
            event_sender,
            response_tx,
        );

        // Verify default states
        assert!(!session.has_send_header);
        assert!(!session.has_audio);
        assert!(!session.has_video);
    }

    // Helper function for tests that need Redis
    // Returns None if Redis is not available
    async fn try_redis_connection() -> Option<redis::aio::ConnectionManager> {
        let redis_client = redis::Client::open("redis://127.0.0.1:6379").unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            redis::aio::ConnectionManager::new(redis_client),
        )
        .await
        .ok()?
        .ok()
    }
}
