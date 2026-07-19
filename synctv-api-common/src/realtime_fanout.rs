use async_trait::async_trait;
use std::sync::Arc;
use synctv_core::service::{NewRealtimeOutboxEvent, RealtimeOutboxService};
use synctv_realtime::fanout::RealtimeEventService;
use synctv_realtime::sync::{PublishRequest, RealtimeEvent};

pub use synctv_realtime::fanout::{
    publish_best_effort, PreparedOutboxFanout, PreparedRealtimeFanoutPlan, RealtimeFanoutService,
};

#[derive(Debug, Clone, Default)]
pub struct NoopRealtimeFanoutService;

#[async_trait]
impl RealtimeFanoutService for NoopRealtimeFanoutService {
    async fn try_publish(&self, _request: PublishRequest) -> bool {
        false
    }

    fn outbox_event(&self, event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String> {
        new_durable_event(event)
    }

    fn publish_after_outbox_commit(&self, _event: RealtimeEvent) {}

    fn is_distributed_enabled(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct LocalRealtimeFanoutService {
    event_service: Arc<dyn RealtimeEventService>,
}

impl LocalRealtimeFanoutService {
    #[must_use]
    pub fn new(event_service: Arc<dyn RealtimeEventService>) -> Self {
        Self { event_service }
    }
}

impl std::fmt::Debug for LocalRealtimeFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalRealtimeFanoutService").finish()
    }
}

#[async_trait]
impl RealtimeFanoutService for LocalRealtimeFanoutService {
    async fn try_publish(&self, request: PublishRequest) -> bool {
        broadcast_event_locally(self.event_service.as_ref(), &request.event);
        true
    }

    fn outbox_event(&self, event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String> {
        new_durable_event(event)
    }

    fn publish_after_outbox_commit(&self, event: RealtimeEvent) {
        broadcast_event_locally(self.event_service.as_ref(), &event);
    }

    fn is_distributed_enabled(&self) -> bool {
        false
    }

    fn accepts_immediate_publish(&self) -> bool {
        true
    }
}

#[derive(Clone)]
pub struct OutboxRealtimeFanoutService {
    outbox: Arc<RealtimeOutboxService>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl OutboxRealtimeFanoutService {
    #[must_use]
    pub fn new(
        outbox: Arc<RealtimeOutboxService>,
        event_service: Arc<dyn RealtimeEventService>,
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
        broadcast_event_locally(self.event_service.as_ref(), &event);
        let outbox_event = match new_outbox_event(&event) {
            Ok(outbox_event) => outbox_event,
            Err(error) => {
                synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                    .with_label_values(&["outbox_serialize_failed"])
                    .inc();
                tracing::error!(
                    error = %error,
                    event_type = %event.event_type(),
                    event_id = %event.event_id(),
                    "Failed to serialize realtime event for outbox"
                );
                return false;
            }
        };
        match self.outbox.insert(&outbox_event).await {
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

    fn outbox_event(&self, event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String> {
        new_outbox_event(event)
    }

    fn publish_after_outbox_commit(&self, event: RealtimeEvent) {
        broadcast_event_locally(self.event_service.as_ref(), &event);
    }

    fn is_distributed_enabled(&self) -> bool {
        true
    }
}

pub fn broadcast_event_locally(event_service: &dyn RealtimeEventService, event: &RealtimeEvent) {
    if event.delivers_to_room_channel() {
        if let Some(room_id) = event.room_id() {
            event_service.broadcast_local(room_id, event);
        }
    }
    if event.delivers_to_admin_channel() {
        event_service.broadcast_admin_local(event);
    }
}

fn new_outbox_event(event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String> {
    Ok(NewRealtimeOutboxEvent {
        id: event.event_id().to_string(),
        enqueue_outbox: true,
        aggregate_type: aggregate_type(event).to_string(),
        aggregate_id: aggregate_id(event),
        event_type: event.event_type().to_string(),
        event_version: 1,
        aggregate_version: aggregate_version(event),
        payload: event.clone(),
    })
}

fn new_durable_event(event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String> {
    Ok(NewRealtimeOutboxEvent {
        enqueue_outbox: false,
        ..new_outbox_event(event)?
    })
}

#[cfg(test)]
fn is_admin_channel_event(event: &RealtimeEvent) -> bool {
    event.delivers_to_admin_channel()
}

fn aggregate_type(event: &RealtimeEvent) -> &'static str {
    match event {
        RealtimeEvent::CacheInvalidate { .. } => "cache",
        RealtimeEvent::PlaybackStateChanged { .. } => "room_playback_state",
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
        RealtimeEvent::PlaybackStateChanged { state, .. } => Some(state.version),
        _ => None,
    }
}

#[must_use]
pub fn disabled_realtime_fanout_service() -> Arc<dyn RealtimeFanoutService> {
    Arc::new(NoopRealtimeFanoutService)
}

#[must_use]
pub fn local_realtime_fanout_service(
    event_service: Arc<dyn RealtimeEventService>,
) -> Arc<dyn RealtimeFanoutService> {
    Arc::new(LocalRealtimeFanoutService::new(event_service))
}

#[must_use]
pub fn distributed_realtime_fanout_service(
    outbox: Arc<RealtimeOutboxService>,
    event_service: Arc<dyn RealtimeEventService>,
) -> Arc<dyn RealtimeFanoutService> {
    Arc::new(OutboxRealtimeFanoutService::new(outbox, event_service))
}

#[derive(Debug, Clone)]
#[cfg(any(test, feature = "test-support"))]
pub struct ChannelRealtimeFanoutService {
    pub sender: tokio::sync::mpsc::Sender<PublishRequest>,
}

#[async_trait]
#[cfg(any(test, feature = "test-support"))]
impl RealtimeFanoutService for ChannelRealtimeFanoutService {
    async fn try_publish(&self, request: PublishRequest) -> bool {
        self.sender.send(request).await.is_ok()
    }

    fn outbox_event(&self, event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String> {
        new_durable_event(event)
    }

    fn publish_after_outbox_commit(&self, event: RealtimeEvent) {
        if let Err(error) = self.sender.try_send(PublishRequest::new(event)) {
            tracing::error!(error = %error, "Test realtime fanout channel rejected committed outbox event");
        }
    }

    fn is_distributed_enabled(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        broadcast_event_locally, disabled_realtime_fanout_service, is_admin_channel_event,
        local_realtime_fanout_service, publish_best_effort, PreparedOutboxFanout,
        PreparedRealtimeFanoutPlan,
    };
    use crate::test_support::RecordingRealtimeEventService;
    use std::sync::atomic::Ordering;
    use synctv_core::models::{MediaId, RoomId, RoomPlaybackState, UserId};
    use synctv_realtime::sync::{CacheTarget, PublishRequest, RealtimeEvent};

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    #[tokio::test]
    async fn test_realtime_fanout_without_outbox_degrades_to_noop() {
        let service = disabled_realtime_fanout_service();

        assert!(
            !service.is_distributed_enabled(),
            "fanout without an outbox must report distributed delivery as disabled"
        );
    }

    #[tokio::test]
    async fn test_best_effort_publish_skips_disabled_fanout() {
        let service = disabled_realtime_fanout_service();

        publish_best_effort(
            service,
            PublishRequest::new(RealtimeEvent::CacheInvalidate {
                event_id: "disabled-best-effort".to_string(),
                targets: vec![CacheTarget::All],
                timestamp: synctv_core::SystemClock.now(),
            }),
        );
    }

    #[tokio::test]
    async fn test_local_realtime_fanout_broadcasts_committed_room_event() {
        let event_service = std::sync::Arc::new(RecordingRealtimeEventService::default());
        let service = local_realtime_fanout_service(event_service.clone());

        service.publish_after_outbox_commit(RealtimeEvent::RoomSettingsChanged {
            event_id: "local-room-settings".to_string(),
            room_id: RoomId::expect_positive(10_000_158),
            user_id: UserId::expect_positive(10_000_159),
            username: "tester".to_string(),
            settings: synctv_core::models::RoomSettings::default(),
            version: 1,
            timestamp: synctv_core::SystemClock.now(),
        });

        assert_eq!(event_service.room_calls.load(Ordering::SeqCst), 1);
        assert_eq!(event_service.admin_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_prepared_outbox_fanout_builds_local_event_without_outbox_row() -> TestResult {
        let event_service = std::sync::Arc::new(RecordingRealtimeEventService::default());
        let service = local_realtime_fanout_service(event_service.clone());
        let prepared = PreparedOutboxFanout::new(service, |room_id: &RoomId| {
            RealtimeEvent::RoomSettingsChanged {
                event_id: "local-prepared-room-settings".to_string(),
                room_id: *room_id,
                user_id: UserId::expect_positive(10_000_160),
                username: "tester".to_string(),
                settings: synctv_core::models::RoomSettings::default(),
                version: 1,
                timestamp: synctv_core::SystemClock.now(),
            }
        });
        let factory = prepared.outbox_factory();

        let event = core_ok(factory(&RoomId::expect_positive(10_000_161)))?;
        assert!(!event.enqueue_outbox);
        prepared.publish_after_outbox_commit();

        assert_eq!(event_service.room_calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn test_cache_invalidate_is_admin_channel_event() {
        let event = RealtimeEvent::CacheInvalidate {
            event_id: "cache-invalidate-admin-route".to_string(),
            targets: vec![CacheTarget::Room {
                room_id: RoomId::expect_positive(10_000_151),
            }],
            timestamp: synctv_core::SystemClock.now(),
        };

        assert!(is_admin_channel_event(&event));
    }

    #[test]
    fn test_room_created_is_admin_channel_event() {
        let event = RealtimeEvent::RoomCreated {
            event_id: "room-created-admin-route".to_string(),
            room_id: RoomId::expect_positive(10_000_152),
            room_name: "created room".to_string(),
            creator_id: synctv_core::models::UserId::expect_positive(10_000_153),
            timestamp: synctv_core::SystemClock.now(),
        };

        assert!(is_admin_channel_event(&event));
    }

    #[test]
    fn test_broadcast_event_locally_uses_room_and_admin_channels() {
        let event_service = RecordingRealtimeEventService::default();
        let event = RealtimeEvent::RoomCreated {
            event_id: "room-created-local-route".to_string(),
            room_id: RoomId::expect_positive(10_000_156),
            room_name: "created room".to_string(),
            creator_id: UserId::expect_positive(10_000_157),
            timestamp: synctv_core::SystemClock.now(),
        };

        broadcast_event_locally(&event_service, &event);

        assert_eq!(event_service.room_calls.load(Ordering::SeqCst), 1);
        assert_eq!(event_service.admin_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_prepared_realtime_fanout_plan_captures_delivery_contract() -> TestResult {
        let event = RealtimeEvent::RoomDeleted {
            event_id: "prepared-plan-room-deleted".to_string(),
            room_id: RoomId::expect_positive(10_000_154),
            deleted_by: synctv_core::models::UserId::expect_positive(10_000_155),
            timestamp: synctv_core::SystemClock.now(),
        };
        let plan = PreparedRealtimeFanoutPlan::new(disabled_realtime_fanout_service(), event)
            .map_err(test_error)?;

        let event = plan.outbox_event();
        assert!(!event.enqueue_outbox);
        assert_eq!(plan.event().event_id(), "prepared-plan-room-deleted");
        Ok(())
    }

    #[test]
    fn test_playback_state_outbox_uses_playback_aggregate_metadata() -> TestResult {
        let room_id = RoomId::expect_positive(10_000_162);
        let state = RoomPlaybackState {
            room_id,
            playing_media_id: Some(MediaId::expect_positive(10_000_163)),
            playing_playlist_id: None,
            target: None,
            current_progress_id: None,
            history_cursor_id: None,
            is_playing: true,
            position: 12.5,
            speed: 1.0,
            playback_generation: 0,
            version: 7,
            updated_at: synctv_core::SystemClock.now(),
        };
        let plan = PreparedRealtimeFanoutPlan::new(
            disabled_realtime_fanout_service(),
            RealtimeEvent::PlaybackStateChanged {
                event_id: "playback-state-outbox-metadata".to_string(),
                room_id,
                user_id: UserId::expect_positive(10_000_164),
                username: "tester".to_string(),
                state,
                source_changed: false,
                client_operation_id: None,
                timestamp: synctv_core::SystemClock.now(),
            },
        )
        .map_err(test_error)?;

        let event = plan.outbox_event();
        assert_eq!(event.aggregate_type, "room_playback_state");
        assert_eq!(event.aggregate_id, room_id.to_string());
        assert_eq!(event.event_type, "playback_state_changed");
        assert_eq!(event.aggregate_version, Some(7));
        Ok(())
    }
}
