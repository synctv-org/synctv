use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::repository::cluster_outbox::{ClusterOutboxRepository, NewClusterOutboxEvent};

use crate::runtime::RealtimeEventService;

#[async_trait]
pub trait ClusterFanoutService: Send + Sync {
    async fn try_publish(&self, request: PublishRequest) -> bool;

    fn outbox_event(&self, event: &ClusterEvent) -> Option<NewClusterOutboxEvent>;

    fn publish_after_outbox_commit(&self, event: ClusterEvent);

    fn is_distributed_enabled(&self) -> bool;
}

pub fn publish_best_effort(cluster_fanout: Arc<dyn ClusterFanoutService>, request: PublishRequest) {
    synctv_core::spawn::spawn_monitored("cluster_fanout_best_effort_publish", async move {
        if !cluster_fanout.try_publish(request).await {
            tracing::warn!("Best-effort cluster fanout publish was not accepted");
        }
    });
}

#[derive(Debug, Clone, Default)]
pub struct NoopClusterFanoutService;

#[async_trait]
impl ClusterFanoutService for NoopClusterFanoutService {
    async fn try_publish(&self, _request: PublishRequest) -> bool {
        false
    }

    fn outbox_event(&self, _event: &ClusterEvent) -> Option<NewClusterOutboxEvent> {
        None
    }

    fn publish_after_outbox_commit(&self, _event: ClusterEvent) {}

    fn is_distributed_enabled(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct OutboxClusterFanoutService {
    outbox: Arc<ClusterOutboxRepository>,
    event_service: Option<Arc<dyn RealtimeEventService>>,
}

impl OutboxClusterFanoutService {
    #[must_use]
    pub const fn new(
        outbox: Arc<ClusterOutboxRepository>,
        event_service: Option<Arc<dyn RealtimeEventService>>,
    ) -> Self {
        Self {
            outbox,
            event_service,
        }
    }
}

#[async_trait]
impl ClusterFanoutService for OutboxClusterFanoutService {
    async fn try_publish(&self, request: PublishRequest) -> bool {
        let event = request.event;
        if let Some(event_service) = &self.event_service {
            if let Some(room_id) = event.room_id() {
                event_service.broadcast_local(room_id, &event);
            } else if is_admin_channel_event(&event) {
                event_service.broadcast_admin_local(&event);
            }
        }
        match self.outbox.insert(&new_outbox_event(&event)).await {
            Ok(()) => true,
            Err(error) => {
                synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                    .with_label_values(&["outbox_insert_failed"])
                    .inc();
                tracing::error!(
                    error = %error,
                    event_type = %event.event_type(),
                    event_id = %event.event_id(),
                    "Failed to persist cluster event to outbox"
                );
                false
            }
        }
    }

    fn outbox_event(&self, event: &ClusterEvent) -> Option<NewClusterOutboxEvent> {
        Some(new_outbox_event(event))
    }

    fn publish_after_outbox_commit(&self, event: ClusterEvent) {
        if let Some(event_service) = &self.event_service {
            if let Some(room_id) = event.room_id() {
                event_service.broadcast_local(room_id, &event);
            } else if is_admin_channel_event(&event) {
                event_service.broadcast_admin_local(&event);
            }
        }
    }

    fn is_distributed_enabled(&self) -> bool {
        true
    }
}

fn new_outbox_event(event: &ClusterEvent) -> NewClusterOutboxEvent {
    NewClusterOutboxEvent {
        id: event.event_id().to_string(),
        aggregate_type: aggregate_type(event).to_string(),
        aggregate_id: aggregate_id(event),
        event_type: event.event_type().to_string(),
        event_version: 1,
        aggregate_version: aggregate_version(event),
        payload: serde_json::to_value(event).expect("ClusterEvent serialization should not fail"),
    }
}

fn is_admin_channel_event(event: &ClusterEvent) -> bool {
    matches!(
        event,
        ClusterEvent::KickUser { .. }
            | ClusterEvent::UserNotification { .. }
            | ClusterEvent::ProviderCredentialChanged { .. }
            | ClusterEvent::CacheInvalidate { .. }
    )
}

fn aggregate_type(event: &ClusterEvent) -> &'static str {
    match event {
        ClusterEvent::CacheInvalidate { .. } => "cache",
        ClusterEvent::PlaylistCreated { .. }
        | ClusterEvent::PlaylistUpdated { .. }
        | ClusterEvent::PlaylistDeleted { .. }
        | ClusterEvent::PlaylistReordered { .. } => "playlist",
        ClusterEvent::MediaAdded { .. }
        | ClusterEvent::MediaRemoved { .. }
        | ClusterEvent::MediaUpdated { .. }
        | ClusterEvent::MediaRemovedBatch { .. } => "media",
        ClusterEvent::PermissionChanged { .. }
        | ClusterEvent::UserJoined { .. }
        | ClusterEvent::UserLeft { .. }
        | ClusterEvent::KickUserFromRoom { .. } => "membership",
        ClusterEvent::KickUser { .. }
        | ClusterEvent::UserNotification { .. }
        | ClusterEvent::ProviderCredentialChanged { .. } => "user",
        _ => "room",
    }
}

fn aggregate_id(event: &ClusterEvent) -> String {
    if let Some(room_id) = event.room_id() {
        return room_id.to_string();
    }
    event
        .user_id()
        .map_or_else(|| "global".to_string(), ToString::to_string)
}

fn aggregate_version(event: &ClusterEvent) -> Option<i64> {
    match event {
        ClusterEvent::RoomSettingsChanged { version, .. } => Some(*version),
        _ => None,
    }
}

#[must_use]
pub fn default_cluster_fanout_service(
    outbox: Option<Arc<ClusterOutboxRepository>>,
    cluster_mode: bool,
) -> Arc<dyn ClusterFanoutService> {
    default_cluster_fanout_service_with_realtime(outbox, cluster_mode, None)
}

#[must_use]
pub fn default_cluster_fanout_service_with_realtime(
    outbox: Option<Arc<ClusterOutboxRepository>>,
    cluster_mode: bool,
    event_service: Option<Arc<dyn RealtimeEventService>>,
) -> Arc<dyn ClusterFanoutService> {
    if cluster_mode {
        if let Some(outbox) = outbox {
            return Arc::new(OutboxClusterFanoutService::new(outbox, event_service));
        }
    }

    Arc::new(NoopClusterFanoutService)
}

#[derive(Debug, Clone)]
#[doc(hidden)]
struct ChannelClusterFanoutService {
    sender: tokio::sync::mpsc::Sender<PublishRequest>,
}

#[async_trait]
impl ClusterFanoutService for ChannelClusterFanoutService {
    async fn try_publish(&self, request: PublishRequest) -> bool {
        self.sender.send(request).await.is_ok()
    }

    fn outbox_event(&self, _event: &ClusterEvent) -> Option<NewClusterOutboxEvent> {
        None
    }

    fn publish_after_outbox_commit(&self, event: ClusterEvent) {
        if let Err(error) = self.sender.try_send(PublishRequest { event }) {
            tracing::error!(error = %error, "Test cluster fanout channel rejected committed outbox event");
        }
    }

    fn is_distributed_enabled(&self) -> bool {
        true
    }
}

#[must_use]
#[doc(hidden)]
pub fn channel_cluster_fanout_service(
    sender: tokio::sync::mpsc::Sender<PublishRequest>,
) -> Arc<dyn ClusterFanoutService> {
    Arc::new(ChannelClusterFanoutService { sender })
}

#[cfg(test)]
mod tests {
    use super::{default_cluster_fanout_service, is_admin_channel_event};
    use chrono::Utc;
    use synctv_cluster::sync::{CacheTarget, ClusterEvent};
    use synctv_core::models::RoomId;

    #[tokio::test]
    async fn test_cluster_fanout_without_outbox_degrades_to_noop() {
        let service = default_cluster_fanout_service(None, true);

        assert!(
            !service.is_distributed_enabled(),
            "fanout without an outbox must report distributed delivery as disabled"
        );
    }

    #[test]
    fn test_cache_invalidate_is_admin_channel_event() {
        let event = ClusterEvent::CacheInvalidate {
            event_id: "cache-invalidate-admin-route".to_string(),
            targets: vec![CacheTarget::Room {
                room_id: RoomId::expect_positive(10_000_151),
            }],
            timestamp: Utc::now(),
        };

        assert!(is_admin_channel_event(&event));
    }
}
