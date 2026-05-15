use std::sync::{Arc, Mutex};
use synctv_core::models::{Room, RoomId, UserId};
use synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent;
use synctv_core::service::RealtimeOutboxRoomEventFactory;
use synctv_realtime::sync::RealtimeEvent;

use crate::realtime_fanout::{
    broadcast_event_locally, PreparedRealtimeFanoutPlan, RealtimeFanoutService,
};
use crate::runtime::{RealtimeDeliveryRequirement, RealtimeEventService};

#[derive(Clone)]
pub struct PreparedRoomCreatedOutboxFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    local_event_service: Option<Arc<dyn RealtimeEventService>>,
    events: Arc<Mutex<Vec<RealtimeEvent>>>,
    creator_id: UserId,
}

impl PreparedRoomCreatedOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> Option<RealtimeOutboxRoomEventFactory> {
        if !self.realtime_fanout.is_distributed_enabled() && self.local_event_service.is_none() {
            return None;
        }

        let prepared = self.clone();
        Some(Arc::new(move |room: &Room| {
            let event = RealtimeEvent::RoomCreated {
                event_id: synctv_common::snanoid!(16),
                room_id: room.id,
                room_name: room.name.clone(),
                creator_id: prepared.creator_id,
                timestamp: chrono::Utc::now(),
            };
            prepared
                .events
                .lock()
                .expect("room created outbox fanout events mutex should not be poisoned")
                .push(event.clone());
            prepared.realtime_fanout.outbox_event(&event)
        }))
    }

    pub fn publish_after_outbox_commit(&self) {
        let events = std::mem::take(
            &mut *self
                .events
                .lock()
                .expect("room created outbox fanout events mutex should not be poisoned"),
        );
        for event in events {
            if self.realtime_fanout.is_distributed_enabled() {
                self.realtime_fanout.publish_after_outbox_commit(event);
            } else if let Some(event_service) = &self.local_event_service {
                broadcast_event_locally(event_service.as_ref(), &event);
            }
        }
    }
}

#[derive(Clone)]
pub struct PreparedRoomLifecycleOutboxFanout {
    plan: PreparedRealtimeFanoutPlan,
    local_event_service: Option<Arc<dyn RealtimeEventService>>,
    distributed: bool,
    local_standalone_delivery: bool,
}

impl PreparedRoomLifecycleOutboxFanout {
    #[must_use]
    pub fn event(&self) -> &RealtimeEvent {
        self.plan.event()
    }

    #[must_use]
    pub fn cloned_outbox_event(&self) -> Option<NewRealtimeOutboxEvent> {
        self.plan.cloned_outbox_event()
    }

    #[must_use]
    pub fn into_event(self) -> RealtimeEvent {
        self.plan.into_event()
    }

    pub fn publish_after_outbox_commit(self) {
        if self.distributed {
            self.plan.publish_after_outbox_commit();
            return;
        }

        if self.local_standalone_delivery {
            if let Some(event_service) = &self.local_event_service {
                broadcast_event_locally(event_service.as_ref(), self.plan.event());
            }
        }
    }
}

pub trait RoomLifecycleFanoutService: Send + Sync {
    fn prepare_room_created_outbox_fanout(
        &self,
        creator_id: UserId,
    ) -> PreparedRoomCreatedOutboxFanout;

    fn prepare_room_deleted_outbox_fanout(
        &self,
        room_id: &RoomId,
        deleted_by: &UserId,
    ) -> PreparedRoomLifecycleOutboxFanout;

    fn prepare_room_banned_outbox_fanout(
        &self,
        room_id: &RoomId,
        banned_by: &UserId,
    ) -> PreparedRoomLifecycleOutboxFanout;

    fn prepare_room_owner_inactive_outbox_fanout(
        &self,
        room_id: &RoomId,
        owner_id: &UserId,
        triggered_by: &UserId,
    ) -> PreparedRoomLifecycleOutboxFanout;
}

pub struct DefaultRoomLifecycleFanoutService {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    local_event_service: Option<Arc<dyn RealtimeEventService>>,
}

impl DefaultRoomLifecycleFanoutService {
    #[must_use]
    pub fn new(
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
        local_event_service: Option<Arc<dyn RealtimeEventService>>,
    ) -> Self {
        Self {
            realtime_fanout,
            local_event_service,
        }
    }
}

impl std::fmt::Debug for DefaultRoomLifecycleFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultRoomLifecycleFanoutService")
            .field(
                "realtime_fanout_distributed",
                &self.realtime_fanout.is_distributed_enabled(),
            )
            .field(
                "has_local_event_service",
                &self.local_event_service.is_some(),
            )
            .finish()
    }
}

impl RoomLifecycleFanoutService for DefaultRoomLifecycleFanoutService {
    fn prepare_room_created_outbox_fanout(
        &self,
        creator_id: UserId,
    ) -> PreparedRoomCreatedOutboxFanout {
        PreparedRoomCreatedOutboxFanout {
            realtime_fanout: self.realtime_fanout.clone(),
            local_event_service: self.local_event_service.clone(),
            events: Arc::new(Mutex::new(Vec::new())),
            creator_id,
        }
    }

    fn prepare_room_deleted_outbox_fanout(
        &self,
        room_id: &RoomId,
        deleted_by: &UserId,
    ) -> PreparedRoomLifecycleOutboxFanout {
        let event = room_deleted_event(room_id, deleted_by);
        PreparedRoomLifecycleOutboxFanout {
            distributed: self.realtime_fanout.is_distributed_enabled(),
            plan: PreparedRealtimeFanoutPlan::new(
                self.realtime_fanout.clone(),
                event,
                RealtimeDeliveryRequirement::DistributedIfAvailable,
            ),
            local_event_service: self.local_event_service.clone(),
            local_standalone_delivery: false,
        }
    }

    fn prepare_room_banned_outbox_fanout(
        &self,
        room_id: &RoomId,
        banned_by: &UserId,
    ) -> PreparedRoomLifecycleOutboxFanout {
        let event = room_banned_event(room_id, banned_by);
        PreparedRoomLifecycleOutboxFanout {
            distributed: self.realtime_fanout.is_distributed_enabled(),
            plan: PreparedRealtimeFanoutPlan::new(
                self.realtime_fanout.clone(),
                event,
                RealtimeDeliveryRequirement::DistributedIfAvailable,
            ),
            local_event_service: self.local_event_service.clone(),
            local_standalone_delivery: true,
        }
    }

    fn prepare_room_owner_inactive_outbox_fanout(
        &self,
        room_id: &RoomId,
        owner_id: &UserId,
        triggered_by: &UserId,
    ) -> PreparedRoomLifecycleOutboxFanout {
        let event = room_owner_inactive_event(room_id, owner_id, triggered_by);
        PreparedRoomLifecycleOutboxFanout {
            distributed: self.realtime_fanout.is_distributed_enabled(),
            plan: PreparedRealtimeFanoutPlan::new(
                self.realtime_fanout.clone(),
                event,
                RealtimeDeliveryRequirement::DistributedIfAvailable,
            ),
            local_event_service: self.local_event_service.clone(),
            local_standalone_delivery: true,
        }
    }
}

fn room_deleted_event(room_id: &RoomId, deleted_by: &UserId) -> RealtimeEvent {
    RealtimeEvent::RoomDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        deleted_by: *deleted_by,
        timestamp: chrono::Utc::now(),
    }
}

fn room_banned_event(room_id: &RoomId, banned_by: &UserId) -> RealtimeEvent {
    RealtimeEvent::RoomBanned {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        banned_by: *banned_by,
        timestamp: chrono::Utc::now(),
    }
}

fn room_owner_inactive_event(
    room_id: &RoomId,
    owner_id: &UserId,
    triggered_by: &UserId,
) -> RealtimeEvent {
    RealtimeEvent::RoomOwnerInactive {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        owner_id: *owner_id,
        triggered_by: *triggered_by,
        timestamp: chrono::Utc::now(),
    }
}

#[must_use]
pub fn default_room_lifecycle_fanout_service(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
) -> Arc<dyn RoomLifecycleFanoutService> {
    default_room_lifecycle_fanout_service_with_realtime(realtime_fanout, None)
}

#[must_use]
pub fn default_room_lifecycle_fanout_service_with_realtime(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    local_event_service: Option<Arc<dyn RealtimeEventService>>,
) -> Arc<dyn RoomLifecycleFanoutService> {
    Arc::new(DefaultRoomLifecycleFanoutService::new(
        realtime_fanout,
        local_event_service,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        default_room_lifecycle_fanout_service, default_room_lifecycle_fanout_service_with_realtime,
    };
    use crate::realtime_fanout::default_realtime_fanout_service;
    use crate::runtime::{RealtimeEventService, RealtimeMetrics};
    use crate::test_support::channel_realtime_fanout_service;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use synctv_core::models::{RoomId, UserId};
    use synctv_realtime::sync::RealtimeEvent;
    use synctv_realtime::sync::{BroadcastResult, ConnectionId};
    use tokio::sync::{broadcast, mpsc};

    #[derive(Default)]
    struct RecordingRealtimeEventService {
        room_events: Mutex<Vec<RealtimeEvent>>,
        admin_events: Mutex<Vec<RealtimeEvent>>,
    }

    impl RecordingRealtimeEventService {
        fn room_events(&self) -> Vec<RealtimeEvent> {
            self.room_events
                .lock()
                .expect("room events mutex should not be poisoned")
                .clone()
        }

        fn admin_events(&self) -> Vec<RealtimeEvent> {
            self.admin_events
                .lock()
                .expect("admin events mutex should not be poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl RealtimeEventService for RecordingRealtimeEventService {
        async fn subscribe_with_id(
            &self,
            _room_id: RoomId,
            _user_id: UserId,
            _connection_id: String,
        ) -> synctv_realtime::Result<(mpsc::Receiver<RealtimeEvent>, ConnectionId)> {
            panic!("subscribe_with_id should not be called in room lifecycle fanout tests");
        }

        fn unsubscribe(&self, _connection_id: &str) {
            panic!("unsubscribe should not be called in room lifecycle fanout tests");
        }

        fn broadcast(&self, _event: RealtimeEvent) -> BroadcastResult {
            panic!("broadcast should not be called in room lifecycle fanout tests");
        }

        fn publish_only(&self, _event: RealtimeEvent) -> bool {
            panic!("publish_only should not be called in room lifecycle fanout tests");
        }

        fn broadcast_local(&self, _room_id: &RoomId, event: &RealtimeEvent) -> usize {
            self.room_events
                .lock()
                .expect("room events mutex should not be poisoned")
                .push(event.clone());
            1
        }

        fn broadcast_admin_local(&self, event: &RealtimeEvent) -> usize {
            self.admin_events
                .lock()
                .expect("admin events mutex should not be poisoned")
                .push(event.clone());
            1
        }

        fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
            panic!("subscribe_admin_events should not be called in room lifecycle fanout tests");
        }

        fn metrics(&self) -> RealtimeMetrics {
            RealtimeMetrics {
                distributed_enabled: false,
            }
        }

        fn node_id(&self) -> &'static str {
            "room-lifecycle-fanout-test-node"
        }

        async fn shutdown(&self) {}
    }

    fn room_id() -> RoomId {
        RoomId::expect_positive(104_001)
    }

    fn user_id() -> UserId {
        UserId::expect_positive(104_002)
    }

    fn standalone_recording_service(
        recorder: Arc<RecordingRealtimeEventService>,
    ) -> Arc<dyn super::RoomLifecycleFanoutService> {
        default_room_lifecycle_fanout_service_with_realtime(
            default_realtime_fanout_service(None, false),
            Some(recorder),
        )
    }

    #[tokio::test]
    async fn test_room_lifecycle_fanout_publishes_prepared_room_deleted_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_lifecycle_fanout_service(channel_realtime_fanout_service(tx));

        service
            .prepare_room_deleted_outbox_fanout(&room_id(), &user_id())
            .publish_after_outbox_commit();

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            RealtimeEvent::RoomDeleted {
                room_id,
                deleted_by,
                ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(104_001));
                assert_eq!(deleted_by, UserId::expect_positive(104_002));
            }
            other => panic!("expected RoomDeleted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_room_lifecycle_fanout_publishes_prepared_room_banned_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_lifecycle_fanout_service(channel_realtime_fanout_service(tx));

        service
            .prepare_room_banned_outbox_fanout(&room_id(), &user_id())
            .publish_after_outbox_commit();

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            RealtimeEvent::RoomBanned {
                room_id, banned_by, ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(104_001));
                assert_eq!(banned_by, UserId::expect_positive(104_002));
            }
            other => panic!("expected RoomBanned, got {other:?}"),
        }
    }

    #[test]
    fn test_standalone_room_created_fanout_broadcasts_after_commit() {
        let recorder = Arc::new(RecordingRealtimeEventService::default());
        let service = standalone_recording_service(recorder.clone());
        let prepared = service.prepare_room_created_outbox_fanout(user_id());
        let factory = prepared
            .outbox_factory()
            .expect("standalone local fanout should still capture the created room event");
        let room = synctv_core::models::Room::new("created room".to_string(), user_id());

        assert!(
            factory(&room).is_none(),
            "standalone local fanout must not create an outbox row"
        );
        prepared.publish_after_outbox_commit();

        let room_events = recorder.room_events();
        let admin_events = recorder.admin_events();
        assert_eq!(room_events.len(), 1);
        assert_eq!(admin_events.len(), 1);
        match &room_events[0] {
            RealtimeEvent::RoomCreated {
                room_id,
                room_name,
                creator_id,
                ..
            } => {
                assert_eq!(*room_id, room.id);
                assert_eq!(room_name, "created room");
                assert_eq!(*creator_id, user_id());
            }
            other => panic!("expected RoomCreated, got {other:?}"),
        }
    }

    #[test]
    fn test_standalone_room_banned_fanout_broadcasts_locally() {
        let recorder = Arc::new(RecordingRealtimeEventService::default());
        let service = standalone_recording_service(recorder.clone());

        service
            .prepare_room_banned_outbox_fanout(&room_id(), &user_id())
            .publish_after_outbox_commit();

        assert_eq!(recorder.room_events().len(), 1);
        assert_eq!(recorder.admin_events().len(), 1);
        match &recorder.room_events()[0] {
            RealtimeEvent::RoomBanned {
                room_id, banned_by, ..
            } => {
                assert_eq!(*room_id, RoomId::expect_positive(104_001));
                assert_eq!(*banned_by, UserId::expect_positive(104_002));
            }
            other => panic!("expected RoomBanned, got {other:?}"),
        }
    }

    #[test]
    fn test_standalone_room_owner_inactive_fanout_broadcasts_locally() {
        let recorder = Arc::new(RecordingRealtimeEventService::default());
        let service = standalone_recording_service(recorder.clone());
        let owner_id = UserId::expect_positive(104_003);

        service
            .prepare_room_owner_inactive_outbox_fanout(&room_id(), &owner_id, &user_id())
            .publish_after_outbox_commit();

        assert_eq!(recorder.room_events().len(), 1);
        assert_eq!(recorder.admin_events().len(), 1);
        match &recorder.room_events()[0] {
            RealtimeEvent::RoomOwnerInactive {
                room_id,
                owner_id: actual_owner_id,
                triggered_by,
                ..
            } => {
                assert_eq!(*room_id, RoomId::expect_positive(104_001));
                assert_eq!(*actual_owner_id, owner_id);
                assert_eq!(*triggered_by, UserId::expect_positive(104_002));
            }
            other => panic!("expected RoomOwnerInactive, got {other:?}"),
        }
    }

    #[test]
    fn test_standalone_room_deleted_fanout_does_not_duplicate_notification_bridge() {
        let recorder = Arc::new(RecordingRealtimeEventService::default());
        let service = standalone_recording_service(recorder.clone());

        service
            .prepare_room_deleted_outbox_fanout(&room_id(), &user_id())
            .publish_after_outbox_commit();

        assert!(
            recorder.room_events().is_empty(),
            "RoomDeleted is already delivered through the room notification bridge"
        );
        assert!(
            recorder.admin_events().is_empty(),
            "RoomDeleted admin delivery should not be duplicated in standalone lifecycle fanout"
        );
    }
}
