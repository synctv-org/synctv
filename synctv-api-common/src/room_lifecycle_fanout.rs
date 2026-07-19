use parking_lot::Mutex;
use std::sync::Arc;
use synctv_core::models::{Room, RoomId, UserId};
use synctv_core::service::{NewRealtimeOutboxEvent, RealtimeOutboxRoomEventFactory};
use synctv_realtime::fanout::RealtimeEventService;
use synctv_realtime::sync::RealtimeEvent;

use crate::realtime_fanout::{
    broadcast_event_locally, PreparedRealtimeFanoutPlan, RealtimeFanoutService,
};
#[derive(Clone)]
pub struct PreparedRoomCreatedOutboxFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    local_event_service: Arc<dyn RealtimeEventService>,
    events: Arc<Mutex<Vec<RealtimeEvent>>>,
    creator_id: UserId,
}

impl PreparedRoomCreatedOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxRoomEventFactory {
        let prepared = self.clone();
        Arc::new(move |room: &Room| {
            let event = RealtimeEvent::RoomCreated {
                event_id: synctv_common::snanoid!(16),
                room_id: room.id,
                room_name: room.name.clone(),
                creator_id: prepared.creator_id,
                timestamp: synctv_core::SystemClock.now(),
            };
            prepared.events.lock().push(event.clone());
            prepared
                .realtime_fanout
                .outbox_event(&event)
                .map_err(synctv_core::Error::Internal)
        })
    }

    pub fn publish_after_outbox_commit(&self) {
        let events = std::mem::take(&mut *self.events.lock());
        for event in events {
            if self.realtime_fanout.is_distributed_enabled() {
                self.realtime_fanout.publish_after_outbox_commit(event);
            } else {
                broadcast_event_locally(self.local_event_service.as_ref(), &event);
            }
        }
    }
}

#[derive(Clone)]
pub struct PreparedRoomLifecycleOutboxFanout {
    plan: PreparedRealtimeFanoutPlan,
    local_event_service: Arc<dyn RealtimeEventService>,
    distributed: bool,
    local_standalone_delivery: bool,
}

impl PreparedRoomLifecycleOutboxFanout {
    #[must_use]
    pub fn event(&self) -> &RealtimeEvent {
        self.plan.event()
    }

    #[must_use]
    pub fn cloned_outbox_event(&self) -> NewRealtimeOutboxEvent {
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
            broadcast_event_locally(self.local_event_service.as_ref(), self.plan.event());
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
    ) -> synctv_core::Result<PreparedRoomLifecycleOutboxFanout>;

    fn prepare_room_banned_outbox_fanout(
        &self,
        room_id: &RoomId,
        banned_by: &UserId,
    ) -> synctv_core::Result<PreparedRoomLifecycleOutboxFanout>;

    fn prepare_room_owner_inactive_outbox_fanout(
        &self,
        room_id: &RoomId,
        owner_id: &UserId,
        triggered_by: &UserId,
    ) -> synctv_core::Result<PreparedRoomLifecycleOutboxFanout>;
}

pub struct DefaultRoomLifecycleFanoutService {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    local_event_service: Arc<dyn RealtimeEventService>,
}

impl DefaultRoomLifecycleFanoutService {
    #[must_use]
    pub fn new(
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
        local_event_service: Arc<dyn RealtimeEventService>,
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
    ) -> synctv_core::Result<PreparedRoomLifecycleOutboxFanout> {
        let event = room_deleted_event(room_id, deleted_by);
        Ok(PreparedRoomLifecycleOutboxFanout {
            distributed: self.realtime_fanout.is_distributed_enabled(),
            plan: PreparedRealtimeFanoutPlan::new(self.realtime_fanout.clone(), event)
                .map_err(synctv_core::Error::Internal)?,
            local_event_service: self.local_event_service.clone(),
            local_standalone_delivery: false,
        })
    }

    fn prepare_room_banned_outbox_fanout(
        &self,
        room_id: &RoomId,
        banned_by: &UserId,
    ) -> synctv_core::Result<PreparedRoomLifecycleOutboxFanout> {
        let event = room_banned_event(room_id, banned_by);
        Ok(PreparedRoomLifecycleOutboxFanout {
            distributed: self.realtime_fanout.is_distributed_enabled(),
            plan: PreparedRealtimeFanoutPlan::new(self.realtime_fanout.clone(), event)
                .map_err(synctv_core::Error::Internal)?,
            local_event_service: self.local_event_service.clone(),
            local_standalone_delivery: true,
        })
    }

    fn prepare_room_owner_inactive_outbox_fanout(
        &self,
        room_id: &RoomId,
        owner_id: &UserId,
        triggered_by: &UserId,
    ) -> synctv_core::Result<PreparedRoomLifecycleOutboxFanout> {
        let event = room_owner_inactive_event(room_id, owner_id, triggered_by);
        Ok(PreparedRoomLifecycleOutboxFanout {
            distributed: self.realtime_fanout.is_distributed_enabled(),
            plan: PreparedRealtimeFanoutPlan::new(self.realtime_fanout.clone(), event)
                .map_err(synctv_core::Error::Internal)?,
            local_event_service: self.local_event_service.clone(),
            local_standalone_delivery: true,
        })
    }
}

fn room_deleted_event(room_id: &RoomId, deleted_by: &UserId) -> RealtimeEvent {
    RealtimeEvent::RoomDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        deleted_by: *deleted_by,
        timestamp: synctv_core::SystemClock.now(),
    }
}

fn room_banned_event(room_id: &RoomId, banned_by: &UserId) -> RealtimeEvent {
    RealtimeEvent::RoomBanned {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        banned_by: *banned_by,
        timestamp: synctv_core::SystemClock.now(),
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
        timestamp: synctv_core::SystemClock.now(),
    }
}

#[must_use]
pub fn default_room_lifecycle_fanout_service_with_realtime(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    local_event_service: Arc<dyn RealtimeEventService>,
) -> Arc<dyn RoomLifecycleFanoutService> {
    Arc::new(DefaultRoomLifecycleFanoutService::new(
        realtime_fanout,
        local_event_service,
    ))
}

#[cfg(test)]
mod tests {
    use super::default_room_lifecycle_fanout_service_with_realtime;
    use crate::realtime_fanout::disabled_realtime_fanout_service;
    use crate::test_support::{channel_realtime_fanout_service, RecordingRealtimeEventService};
    use std::sync::Arc;
    use synctv_core::models::{RoomId, UserId};
    use synctv_realtime::sync::RealtimeEvent;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
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
            disabled_realtime_fanout_service(),
            recorder,
        )
    }

    #[tokio::test]
    async fn test_room_lifecycle_fanout_publishes_prepared_room_deleted_event() -> TestResult {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_lifecycle_fanout_service_with_realtime(
            channel_realtime_fanout_service(tx),
            Arc::new(RecordingRealtimeEventService::default()),
        );

        service
            .prepare_room_deleted_outbox_fanout(&room_id(), &user_id())
            .map_err(|error| test_error(format!("{error:?}")))?
            .publish_after_outbox_commit();

        let request = rx
            .recv()
            .await
            .ok_or_else(|| test_error("publish request should be queued"))?;
        match request.event {
            RealtimeEvent::RoomDeleted {
                room_id,
                deleted_by,
                ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(104_001));
                assert_eq!(deleted_by, UserId::expect_positive(104_002));
            }
            other => return Err(test_error(format!("expected RoomDeleted, got {other:?}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_room_lifecycle_fanout_publishes_prepared_room_banned_event() -> TestResult {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_lifecycle_fanout_service_with_realtime(
            channel_realtime_fanout_service(tx),
            Arc::new(RecordingRealtimeEventService::default()),
        );

        service
            .prepare_room_banned_outbox_fanout(&room_id(), &user_id())
            .map_err(|error| test_error(format!("{error:?}")))?
            .publish_after_outbox_commit();

        let request = rx
            .recv()
            .await
            .ok_or_else(|| test_error("publish request should be queued"))?;
        match request.event {
            RealtimeEvent::RoomBanned {
                room_id, banned_by, ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(104_001));
                assert_eq!(banned_by, UserId::expect_positive(104_002));
            }
            other => return Err(test_error(format!("expected RoomBanned, got {other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_standalone_room_created_fanout_broadcasts_after_commit() -> TestResult {
        let recorder = Arc::new(RecordingRealtimeEventService::default());
        let service = standalone_recording_service(recorder.clone());
        let prepared = service.prepare_room_created_outbox_fanout(user_id());
        let factory = prepared.outbox_factory();
        let room = synctv_core::models::Room::new("created room".to_string(), user_id());

        let event = core_ok(factory(&room))?;
        assert!(!event.enqueue_outbox);
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
            other => return Err(test_error(format!("expected RoomCreated, got {other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_standalone_room_banned_fanout_broadcasts_locally() -> TestResult {
        let recorder = Arc::new(RecordingRealtimeEventService::default());
        let service = standalone_recording_service(recorder.clone());

        service
            .prepare_room_banned_outbox_fanout(&room_id(), &user_id())
            .map_err(|error| test_error(format!("{error:?}")))?
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
            other => return Err(test_error(format!("expected RoomBanned, got {other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_standalone_room_owner_inactive_fanout_broadcasts_locally() -> TestResult {
        let recorder = Arc::new(RecordingRealtimeEventService::default());
        let service = standalone_recording_service(recorder.clone());
        let owner_id = UserId::expect_positive(104_003);

        service
            .prepare_room_owner_inactive_outbox_fanout(&room_id(), &owner_id, &user_id())
            .map_err(|error| test_error(format!("{error:?}")))?
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
            other => {
                return Err(test_error(format!(
                    "expected RoomOwnerInactive, got {other:?}"
                )))
            }
        }
        Ok(())
    }

    #[test]
    fn test_standalone_room_deleted_fanout_does_not_duplicate_notification_bridge() -> TestResult {
        let recorder = Arc::new(RecordingRealtimeEventService::default());
        let service = standalone_recording_service(recorder.clone());

        service
            .prepare_room_deleted_outbox_fanout(&room_id(), &user_id())
            .map_err(|error| test_error(format!("{error:?}")))?
            .publish_after_outbox_commit();

        assert!(
            recorder.room_events().is_empty(),
            "RoomDeleted is already delivered through the room notification bridge"
        );
        assert!(
            recorder.admin_events().is_empty(),
            "RoomDeleted admin delivery should not be duplicated in standalone lifecycle fanout"
        );
        Ok(())
    }
}
