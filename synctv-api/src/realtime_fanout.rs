use async_trait::async_trait;
use std::sync::Arc;
use synctv_core::repository::realtime_outbox::{NewRealtimeOutboxEvent, RealtimeOutboxRepository};
use synctv_realtime::sync::{PublishRequest, RealtimeEvent};

use crate::runtime::RealtimeEventService;

#[async_trait]
pub trait RealtimeFanoutService: Send + Sync {
    async fn try_publish(&self, request: PublishRequest) -> bool;

    fn outbox_event(&self, event: &RealtimeEvent) -> Option<NewRealtimeOutboxEvent>;

    fn publish_after_outbox_commit(&self, event: RealtimeEvent);

    fn is_distributed_enabled(&self) -> bool;
}

pub fn publish_best_effort(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    request: PublishRequest,
) {
    if !realtime_fanout.is_distributed_enabled() {
        return;
    }

    synctv_core::spawn::spawn_monitored("realtime_fanout_best_effort_publish", async move {
        if !realtime_fanout.try_publish(request).await {
            tracing::warn!("Best-effort realtime fanout publish was not accepted");
        }
    });
}

#[derive(Debug, Clone, Default)]
pub struct NoopRealtimeFanoutService;

#[async_trait]
impl RealtimeFanoutService for NoopRealtimeFanoutService {
    async fn try_publish(&self, _request: PublishRequest) -> bool {
        false
    }

    fn outbox_event(&self, _event: &RealtimeEvent) -> Option<NewRealtimeOutboxEvent> {
        None
    }

    fn publish_after_outbox_commit(&self, _event: RealtimeEvent) {}

    fn is_distributed_enabled(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct OutboxRealtimeFanoutService {
    outbox: Arc<RealtimeOutboxRepository>,
    event_service: Option<Arc<dyn RealtimeEventService>>,
}

impl OutboxRealtimeFanoutService {
    #[must_use]
    pub const fn new(
        outbox: Arc<RealtimeOutboxRepository>,
        event_service: Option<Arc<dyn RealtimeEventService>>,
    ) -> Self {
        Self {
            outbox,
            event_service,
        }
    }
}

#[async_trait]
impl RealtimeFanoutService for OutboxRealtimeFanoutService {
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
                synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                    .with_label_values(&["outbox_insert_failed"])
                    .inc();
                tracing::error!(
                    error = %error,
                    event_type = %event.event_type(),
                    event_id = %event.event_id(),
                    "Failed to persist realtime event to outbox"
                );
                false
            }
        }
    }

    fn outbox_event(&self, event: &RealtimeEvent) -> Option<NewRealtimeOutboxEvent> {
        Some(new_outbox_event(event))
    }

    fn publish_after_outbox_commit(&self, event: RealtimeEvent) {
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

fn new_outbox_event(event: &RealtimeEvent) -> NewRealtimeOutboxEvent {
    NewRealtimeOutboxEvent {
        id: event.event_id().to_string(),
        aggregate_type: aggregate_type(event).to_string(),
        aggregate_id: aggregate_id(event),
        event_type: event.event_type().to_string(),
        event_version: 1,
        aggregate_version: aggregate_version(event),
        payload: serde_json::to_value(event).expect("RealtimeEvent serialization should not fail"),
    }
}

fn is_admin_channel_event(event: &RealtimeEvent) -> bool {
    matches!(
        event,
        RealtimeEvent::KickUser { .. }
            | RealtimeEvent::UserNotification { .. }
            | RealtimeEvent::ProviderCredentialChanged { .. }
            | RealtimeEvent::CacheInvalidate { .. }
    )
}

fn aggregate_type(event: &RealtimeEvent) -> &'static str {
    match event {
        RealtimeEvent::CacheInvalidate { .. } => "cache",
        RealtimeEvent::PlaylistCreated { .. }
        | RealtimeEvent::PlaylistUpdated { .. }
        | RealtimeEvent::PlaylistDeleted { .. }
        | RealtimeEvent::PlaylistReordered { .. } => "playlist",
        RealtimeEvent::MediaAdded { .. }
        | RealtimeEvent::MediaRemoved { .. }
        | RealtimeEvent::MediaUpdated { .. }
        | RealtimeEvent::MediaRemovedBatch { .. } => "media",
        RealtimeEvent::PermissionChanged { .. }
        | RealtimeEvent::UserJoined { .. }
        | RealtimeEvent::UserLeft { .. }
        | RealtimeEvent::KickUserFromRoom { .. } => "membership",
        RealtimeEvent::KickUser { .. }
        | RealtimeEvent::UserNotification { .. }
        | RealtimeEvent::ProviderCredentialChanged { .. } => "user",
        _ => "room",
    }
}

fn aggregate_id(event: &RealtimeEvent) -> String {
    if let Some(room_id) = event.room_id() {
        return room_id.to_string();
    }
    event
        .user_id()
        .map_or_else(|| "global".to_string(), ToString::to_string)
}

fn aggregate_version(event: &RealtimeEvent) -> Option<i64> {
    match event {
        RealtimeEvent::RoomSettingsChanged { version, .. } => Some(*version),
        _ => None,
    }
}

#[must_use]
pub fn default_realtime_fanout_service(
    outbox: Option<Arc<RealtimeOutboxRepository>>,
    distributed_enabled: bool,
) -> Arc<dyn RealtimeFanoutService> {
    default_realtime_fanout_service_with_realtime(outbox, distributed_enabled, None)
}

#[must_use]
pub fn default_realtime_fanout_service_with_realtime(
    outbox: Option<Arc<RealtimeOutboxRepository>>,
    distributed_enabled: bool,
    event_service: Option<Arc<dyn RealtimeEventService>>,
) -> Arc<dyn RealtimeFanoutService> {
    if distributed_enabled {
        if let Some(outbox) = outbox {
            return Arc::new(OutboxRealtimeFanoutService::new(outbox, event_service));
        }
    }

    Arc::new(NoopRealtimeFanoutService)
}

#[derive(Debug, Clone)]
#[doc(hidden)]
struct ChannelRealtimeFanoutService {
    sender: tokio::sync::mpsc::Sender<PublishRequest>,
}

#[async_trait]
impl RealtimeFanoutService for ChannelRealtimeFanoutService {
    async fn try_publish(&self, request: PublishRequest) -> bool {
        self.sender.send(request).await.is_ok()
    }

    fn outbox_event(&self, _event: &RealtimeEvent) -> Option<NewRealtimeOutboxEvent> {
        None
    }

    fn publish_after_outbox_commit(&self, event: RealtimeEvent) {
        if let Err(error) = self.sender.try_send(PublishRequest { event }) {
            tracing::error!(error = %error, "Test realtime fanout channel rejected committed outbox event");
        }
    }

    fn is_distributed_enabled(&self) -> bool {
        true
    }
}

#[must_use]
#[doc(hidden)]
pub fn channel_realtime_fanout_service(
    sender: tokio::sync::mpsc::Sender<PublishRequest>,
) -> Arc<dyn RealtimeFanoutService> {
    Arc::new(ChannelRealtimeFanoutService { sender })
}

#[cfg(test)]
mod tests {
    use super::{default_realtime_fanout_service, is_admin_channel_event, publish_best_effort};
    use chrono::Utc;
    use synctv_core::models::RoomId;
    use synctv_realtime::sync::{CacheTarget, PublishRequest, RealtimeEvent};

    #[tokio::test]
    async fn test_realtime_fanout_without_outbox_degrades_to_noop() {
        let service = default_realtime_fanout_service(None, true);

        assert!(
            !service.is_distributed_enabled(),
            "fanout without an outbox must report distributed delivery as disabled"
        );
    }

    #[tokio::test]
    async fn test_best_effort_publish_skips_disabled_fanout() {
        let service = default_realtime_fanout_service(None, false);

        publish_best_effort(
            service,
            PublishRequest {
                event: RealtimeEvent::CacheInvalidate {
                    event_id: "disabled-best-effort".to_string(),
                    targets: vec![CacheTarget::All],
                    timestamp: Utc::now(),
                },
            },
        );
    }

    #[test]
    fn test_cache_invalidate_is_admin_channel_event() {
        let event = RealtimeEvent::CacheInvalidate {
            event_id: "cache-invalidate-admin-route".to_string(),
            targets: vec![CacheTarget::Room {
                room_id: RoomId::expect_positive(10_000_151),
            }],
            timestamp: Utc::now(),
        };

        assert!(is_admin_channel_event(&event));
    }
}
