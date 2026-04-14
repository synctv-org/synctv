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
use crate::relay::StreamRegistryTrait;

// Re-export HttpFlvSession from xiu-httpflv
pub use synctv_xiu::httpflv::HttpFlvSession;

#[derive(Clone)]
pub struct HttpFlvState {
    registry: Arc<dyn StreamRegistryTrait>,
    stream_hub_event_sender: StreamHubEventSender,
    /// Optional infrastructure for subscriber tracking via `StreamSubscriberGuard`.
    /// When set, each FLV session holds a guard that decrements the subscriber
    /// count on drop, ensuring correct idle-cleanup lifecycle.
    infrastructure: Option<LiveStreamingInfrastructure>,
}

impl HttpFlvState {
    #[must_use]
    pub const fn new(
        registry: Arc<dyn StreamRegistryTrait>,
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
    let publisher_info = match state.registry.get_publisher(&room_id, media_id).await {
        Ok(Some(info)) => info,
        Ok(None) => {
            warn!("No publisher for room {} / media {}", room_id, media_id);
            return Err(StatusCode::NOT_FOUND);
        }
        Err(e) => {
            error!("Failed to query publisher: {}", e);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

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
            let is_local =
                !infra.local_node_id.is_empty() && publisher_info.node_id == infra.local_node_id;
            if is_local {
                None
            } else {
                match infra.ensure_pull_stream(&room_id, media_id, None).await {
                    Ok(guard) => Some(guard),
                    Err(e) => {
                        warn!("Failed to create subscriber guard for FLV session: {}", e);
                        return Err(StatusCode::SERVICE_UNAVAILABLE);
                    }
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

    if let Err(e) = flv_session.start().await {
        warn!(
            room_id = %room_id,
            media_id = %media_id,
            error = %e,
            "Failed to subscribe HTTP-FLV session to StreamHub before responding"
        );
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    tokio::spawn(async move {
        let _guard = subscriber_guard; // held for the lifetime of this task
        if let Err(e) = flv_session.run_after_start().await {
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
    use crate::api::{LiveStreamingInfrastructure, StreamTracker};
    use crate::livestream::{
        external_publish_manager::ExternalPublishManager, pull_manager::PullStreamManager,
    };
    use crate::relay::{mock_registry::MockStreamRegistry, PublisherInfo};
    use chrono::Utc;

    #[tokio::test]
    async fn test_http_flv_state_creation() {
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
        let registry = crate::relay::local_stream_registry();

        let state = HttpFlvState::new(registry, event_sender);
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

    #[tokio::test]
    async fn test_handle_flv_stream_local_publisher_does_not_require_pull_stream() {
        let registry = Arc::new(MockStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                PublisherInfo {
                    node_id: "node-local".to_string(),
                    api_address: "127.0.0.1:50051".to_string(),
                    app_name: "live".to_string(),
                    user_id: String::new(),
                    started_at: Utc::now(),
                    epoch: 1,
                },
            )]),
        ));
        let (event_sender, mut event_rx) = tokio::sync::mpsc::channel(64);
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
            Arc::new(StreamTracker::new()),
        )
        .with_local_node_id("node-local".to_string());
        let state = HttpFlvState::new(registry, event_sender).with_infrastructure(infrastructure);

        let streamhub_task = tokio::spawn(async move {
            let subscribe =
                tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
                    .await
                    .expect("subscribe event should be emitted")
                    .expect("event channel should stay open");

            let result_sender = match subscribe {
                synctv_xiu::streamhub::define::StreamHubEvent::Subscribe {
                    result_sender, ..
                } => result_sender,
                other => panic!("expected subscribe event, got {other:?}"),
            };

            let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(8);
            result_sender
                .send(Ok((
                    synctv_xiu::streamhub::define::DataReceiver {
                        frame_receiver: Some(frame_rx),
                        packet_receiver: None,
                    },
                    None,
                )))
                .expect("subscribe result should be delivered");
            drop(frame_tx);

            let unsubscribe =
                tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
                    .await
                    .expect("unsubscribe event should be emitted")
                    .expect("event channel should stay open");
            assert!(
                matches!(
                    unsubscribe,
                    synctv_xiu::streamhub::define::StreamHubEvent::UnSubscribe { .. }
                ),
                "expected unsubscribe event after spawned FLV session exits"
            );
        });

        let response = handle_flv_stream(
            Path("media1.flv".to_string()),
            Extension("room1".to_string()),
            State(state),
        )
        .await
        .expect("local publisher should not require relay setup");

        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::timeout(std::time::Duration::from_secs(1), streamhub_task)
            .await
            .expect("streamhub task should finish")
            .expect("streamhub task should join");
    }

    #[tokio::test]
    async fn test_handle_flv_stream_remote_publisher_requires_pull_stream() {
        let registry = Arc::new(MockStreamRegistry::with_publishers(
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
        let (event_sender, _) = tokio::sync::mpsc::channel(64);
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
            Arc::new(StreamTracker::new()),
        )
        .with_local_node_id("node-local".to_string());
        let state = HttpFlvState::new(registry, event_sender).with_infrastructure(infrastructure);

        let response = handle_flv_stream(
            Path("media1.flv".to_string()),
            Extension("room1".to_string()),
            State(state),
        )
        .await;

        assert!(
            matches!(response, Err(StatusCode::SERVICE_UNAVAILABLE)),
            "remote publishers must still go through relay setup, and invalid relay config should fail closed"
        );
    }

    #[tokio::test]
    async fn test_handle_flv_stream_fails_closed_when_streamhub_subscribe_cannot_start() {
        let registry = Arc::new(MockStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                PublisherInfo {
                    node_id: "node-local".to_string(),
                    api_address: "127.0.0.1:50051".to_string(),
                    app_name: "live".to_string(),
                    user_id: String::new(),
                    started_at: Utc::now(),
                    epoch: 1,
                },
            )]),
        ));
        let (event_sender, event_rx) = tokio::sync::mpsc::channel(1);
        drop(event_rx);
        let state = HttpFlvState::new(registry, event_sender);

        let response = handle_flv_stream(
            Path("media1.flv".to_string()),
            Extension("room1".to_string()),
            State(state),
        )
        .await;

        assert!(
            matches!(response, Err(StatusCode::SERVICE_UNAVAILABLE)),
            "HTTP-FLV must not return 200 OK before StreamHub subscription succeeds"
        );
    }
}
