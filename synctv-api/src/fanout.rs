use std::sync::Arc;
use synctv_core::models::{RoomId, RoomSettings, UserId};
use synctv_core::service::RealtimeOutboxSettingsEventFactory;
use synctv_realtime::sync::RealtimeEvent;

use crate::realtime_fanout::{PreparedRealtimeFanoutPlan, RealtimeFanoutService};
use crate::runtime::RealtimeDeliveryRequirement;

#[derive(Clone)]
pub struct PreparedRoomSettingsFanout {
    plan: PreparedRealtimeFanoutPlan,
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl PreparedRoomSettingsFanout {
    #[must_use]
    pub fn event(&self) -> &RealtimeEvent {
        self.plan.event()
    }

    #[must_use]
    fn from_event(
        event: RealtimeEvent,
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
    ) -> synctv_core::Result<Self> {
        let plan = PreparedRealtimeFanoutPlan::new(
            realtime_fanout.clone(),
            event,
            RealtimeDeliveryRequirement::DistributedIfAvailable,
        )
        .map_err(synctv_core::Error::Internal)?;
        Ok(Self {
            plan,
            realtime_fanout,
        })
    }

    #[must_use]
    pub fn settings_outbox_factory(&self) -> RealtimeOutboxSettingsEventFactory {
        let prepared = self.clone();
        Arc::new(move |settings: &RoomSettings, version| {
            let settings_json = serde_json::to_vec(settings)?;
            let event = room_settings_event_with_settings_and_version(
                prepared.event(),
                settings_json,
                version,
            );
            prepared
                .realtime_fanout
                .outbox_event(&event)
                .map_err(synctv_core::Error::Internal)
        })
    }

    #[must_use]
    pub fn with_version(&self, version: i64) -> synctv_core::Result<Self> {
        Self::from_event(
            room_settings_event_with_version(self.event(), version),
            self.realtime_fanout.clone(),
        )
    }

    pub fn with_settings_and_version(
        &self,
        settings: &RoomSettings,
        version: i64,
    ) -> synctv_core::Result<Self> {
        let settings_json = serde_json::to_vec(settings)?;
        Self::from_event(
            room_settings_event_with_settings_and_version(self.event(), settings_json, version),
            self.realtime_fanout.clone(),
        )
    }
}

impl std::fmt::Debug for PreparedRoomSettingsFanout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedRoomSettingsFanout")
            .field("event", self.event())
            .field("outbox_event", &self.plan.outbox_event())
            .finish()
    }
}

pub trait RoomSettingsFanoutService: Send + Sync {
    fn prepare_settings_changed(
        &self,
        room_id: &RoomId,
        actor_user_id: &UserId,
        actor_username: &str,
        settings_json: Vec<u8>,
        version: i64,
    ) -> synctv_core::Result<PreparedRoomSettingsFanout>;

    fn publish_prepared_after_outbox_commit(&self, prepared: PreparedRoomSettingsFanout);
}

pub struct DefaultRoomSettingsFanoutService {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl DefaultRoomSettingsFanoutService {
    #[must_use]
    pub fn new(realtime_fanout: Arc<dyn RealtimeFanoutService>) -> Self {
        Self { realtime_fanout }
    }
}

impl std::fmt::Debug for DefaultRoomSettingsFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultRoomSettingsFanoutService")
            .field(
                "realtime_fanout_distributed",
                &self.realtime_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

impl RoomSettingsFanoutService for DefaultRoomSettingsFanoutService {
    fn prepare_settings_changed(
        &self,
        room_id: &RoomId,
        actor_user_id: &UserId,
        actor_username: &str,
        settings_json: Vec<u8>,
        version: i64,
    ) -> synctv_core::Result<PreparedRoomSettingsFanout> {
        let event = RealtimeEvent::RoomSettingsChanged {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *actor_user_id,
            username: actor_username.to_string(),
            settings_json,
            version,
            timestamp: chrono::Utc::now(),
        };
        PreparedRoomSettingsFanout::from_event(event, self.realtime_fanout.clone())
    }

    fn publish_prepared_after_outbox_commit(&self, prepared: PreparedRoomSettingsFanout) {
        prepared.plan.publish_after_outbox_commit();
    }
}

fn room_settings_event_with_version(event: &RealtimeEvent, version: i64) -> RealtimeEvent {
    match event {
        RealtimeEvent::RoomSettingsChanged {
            event_id,
            room_id,
            user_id,
            username,
            settings_json,
            timestamp,
            ..
        } => RealtimeEvent::RoomSettingsChanged {
            event_id: event_id.clone(),
            room_id: *room_id,
            user_id: *user_id,
            username: username.clone(),
            settings_json: settings_json.clone(),
            version,
            timestamp: *timestamp,
        },
        _ => event.clone(),
    }
}

fn room_settings_event_with_settings_and_version(
    event: &RealtimeEvent,
    settings_json: Vec<u8>,
    version: i64,
) -> RealtimeEvent {
    match event {
        RealtimeEvent::RoomSettingsChanged {
            event_id,
            room_id,
            user_id,
            username,
            timestamp,
            ..
        } => RealtimeEvent::RoomSettingsChanged {
            event_id: event_id.clone(),
            room_id: *room_id,
            user_id: *user_id,
            username: username.clone(),
            settings_json,
            version,
            timestamp: *timestamp,
        },
        _ => event.clone(),
    }
}

#[must_use]
pub fn default_room_settings_fanout_service(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
) -> Arc<dyn RoomSettingsFanoutService> {
    Arc::new(DefaultRoomSettingsFanoutService::new(realtime_fanout))
}

#[cfg(test)]
mod tests {
    use super::default_room_settings_fanout_service;
    use crate::realtime_fanout::local_realtime_fanout_service;
    use crate::runtime::{RealtimeEventService, RealtimeMetrics};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use synctv_core::models::{RoomId, UserId};
    use synctv_realtime::sync::{BroadcastResult, ConnectionId, RealtimeEvent};
    use tokio::sync::{broadcast, mpsc};

    #[derive(Default)]
    struct RecordingRealtimeEventService {
        broadcast_calls: AtomicUsize,
        broadcast_local_calls: AtomicUsize,
        local_events: Mutex<Vec<(String, RealtimeEvent)>>,
    }

    #[async_trait]
    impl RealtimeEventService for RecordingRealtimeEventService {
        async fn subscribe_with_id(
            &self,
            _room_id: RoomId,
            _user_id: UserId,
            _connection_id: String,
        ) -> synctv_realtime::Result<(mpsc::Receiver<RealtimeEvent>, ConnectionId)> {
            panic!("subscribe_with_id should not be called in room settings fanout tests");
        }

        fn unsubscribe(&self, _connection_id: &str) {
            panic!("unsubscribe should not be called in room settings fanout tests");
        }

        fn broadcast(&self, _event: RealtimeEvent) -> BroadcastResult {
            self.broadcast_calls.fetch_add(1, Ordering::SeqCst);
            BroadcastResult {
                local_sent: 0,
                redis_sent: false,
            }
        }

        fn publish_only(&self, _event: RealtimeEvent) -> bool {
            panic!("publish_only should not be called in room settings fanout tests");
        }

        fn broadcast_local(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize {
            self.broadcast_local_calls.fetch_add(1, Ordering::SeqCst);
            self.local_events
                .lock()
                .expect("recorded local events mutex should not be poisoned")
                .push((room_id.to_string(), event.clone()));
            1
        }

        fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
            panic!("subscribe_admin_events should not be called in room settings fanout tests");
        }

        fn metrics(&self) -> RealtimeMetrics {
            RealtimeMetrics {
                distributed_enabled: true,
            }
        }

        fn node_id(&self) -> &'static str {
            "room-settings-fanout-test-node"
        }

        async fn shutdown(&self) {}
    }

    fn room_id() -> RoomId {
        RoomId::expect_positive(107_001)
    }

    fn user_id() -> UserId {
        UserId::expect_positive(107_002)
    }

    #[tokio::test]
    async fn test_standalone_room_settings_fanout_broadcasts_locally() {
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_room_settings_fanout_service(local_realtime_fanout_service(
            event_service.clone(),
        ));

        let prepared = service
            .prepare_settings_changed(
                &room_id(),
                &user_id(),
                "tester",
                br#"{"chat_enabled":false}"#.to_vec(),
                11,
            )
            .expect("room settings fanout should prepare");
        service.publish_prepared_after_outbox_commit(prepared);

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            event_service
                .local_events
                .lock()
                .expect("recorded local events mutex should not be poisoned")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn test_prepared_room_settings_fanout_keeps_event_identity_when_snapshot_is_applied() {
        let service = default_room_settings_fanout_service(
            crate::realtime_fanout::disabled_realtime_fanout_service(),
        );
        let prepared = service
            .prepare_settings_changed(&room_id(), &user_id(), "tester", Vec::new(), 0)
            .expect("room settings fanout should prepare");
        let original_event_id = prepared.event().event_id().to_string();

        let prepared = prepared
            .with_settings_and_version(&synctv_core::models::RoomSettings::default(), 42)
            .expect("default room settings should serialize");

        assert_eq!(
            prepared.event().event_id(),
            original_event_id,
            "outbox and local room settings fanout must share one event id"
        );
        match prepared.event() {
            RealtimeEvent::RoomSettingsChanged { version, .. } => {
                assert_eq!(*version, 42);
            }
            other => panic!("expected RoomSettingsChanged, got {other:?}"),
        }
    }
}
