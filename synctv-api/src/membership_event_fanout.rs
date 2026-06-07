use std::sync::Arc;
use synctv_core::models::UserId;
use synctv_core::service::{
    PermissionChangedOutboxSnapshot, RealtimeOutboxPermissionChangedEventFactory,
    RealtimeOutboxUserLeftEventFactory, UserLeftOutboxSnapshot,
};
use synctv_realtime::sync::RealtimeEvent;

use crate::realtime_fanout::RealtimeFanoutService;
use crate::runtime::RealtimeEventService;

pub trait MembershipEventFanoutService: Send + Sync {
    fn prepare_permission_changed_outbox_fanout(
        &self,
        target_user_id: UserId,
        changed_by: UserId,
    ) -> PreparedPermissionChangedFanout;

    fn prepare_user_left_outbox_fanout(&self) -> PreparedUserLeftFanout;
}

#[derive(Clone)]
pub struct PreparedPermissionChangedFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    event_service: Arc<dyn RealtimeEventService>,
    events: Arc<parking_lot::Mutex<Vec<RealtimeEvent>>>,
}

impl PreparedPermissionChangedFanout {
    #[doc(hidden)]
    #[must_use]
    pub fn new(
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
        event_service: Arc<dyn RealtimeEventService>,
        _target_user_id: UserId,
        _changed_by: UserId,
    ) -> Self {
        Self {
            realtime_fanout,
            event_service,
            events: Arc::new(parking_lot::Mutex::new(Vec::new())),
        }
    }

    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxPermissionChangedEventFactory {
        let prepared = self.clone();
        Arc::new(move |snapshot: &PermissionChangedOutboxSnapshot| {
            let event = RealtimeEvent::PermissionChanged {
                event_id: synctv_common::snanoid!(16),
                room_id: snapshot.room_id,
                target_user_id: snapshot.target_user_id,
                target_username: snapshot.target_username.clone(),
                changed_by: snapshot.changed_by,
                changed_by_username: snapshot.changed_by_username.clone(),
                new_permissions: snapshot.new_permissions,
                role: snapshot.role,
                added_permissions: snapshot.added_permissions,
                removed_permissions: snapshot.removed_permissions,
                admin_added_permissions: snapshot.admin_added_permissions,
                admin_removed_permissions: snapshot.admin_removed_permissions,
                timestamp: chrono::Utc::now(),
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
            } else if let Some(room_id) = event.room_id() {
                self.event_service.broadcast_local(room_id, &event);
            }
        }
    }
}

#[derive(Clone)]
pub struct PreparedUserLeftFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    event: Arc<parking_lot::Mutex<Option<RealtimeEvent>>>,
}

impl PreparedUserLeftFanout {
    #[doc(hidden)]
    #[must_use]
    pub fn new(realtime_fanout: Arc<dyn RealtimeFanoutService>) -> Self {
        Self {
            realtime_fanout,
            event: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxUserLeftEventFactory {
        let prepared = self.clone();
        Arc::new(move |snapshot: &UserLeftOutboxSnapshot| {
            let event = RealtimeEvent::UserLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: snapshot.room_id,
                user_id: snapshot.user_id,
                username: snapshot.username.clone(),
                timestamp: chrono::Utc::now(),
            };
            *prepared.event.lock() = Some(event.clone());
            prepared
                .realtime_fanout
                .outbox_event(&event)
                .map_err(synctv_core::Error::Internal)
        })
    }

    pub fn publish_after_outbox_commit(&self) {
        if let Some(event) = self.event.lock().take() {
            self.realtime_fanout.publish_after_outbox_commit(event);
        }
    }
}

pub struct DefaultMembershipEventFanoutService {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl DefaultMembershipEventFanoutService {
    #[must_use]
    pub fn new(
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
        event_service: Arc<dyn RealtimeEventService>,
    ) -> Self {
        Self {
            realtime_fanout,
            event_service,
        }
    }
}

impl std::fmt::Debug for DefaultMembershipEventFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultMembershipEventFanoutService")
            .field(
                "realtime_fanout_distributed",
                &self.realtime_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

impl MembershipEventFanoutService for DefaultMembershipEventFanoutService {
    fn prepare_permission_changed_outbox_fanout(
        &self,
        target_user_id: UserId,
        changed_by: UserId,
    ) -> PreparedPermissionChangedFanout {
        PreparedPermissionChangedFanout::new(
            self.realtime_fanout.clone(),
            self.event_service.clone(),
            target_user_id,
            changed_by,
        )
    }

    fn prepare_user_left_outbox_fanout(&self) -> PreparedUserLeftFanout {
        PreparedUserLeftFanout::new(self.realtime_fanout.clone())
    }
}

#[must_use]
pub fn default_membership_event_fanout_service(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    event_service: Arc<dyn RealtimeEventService>,
) -> Arc<dyn MembershipEventFanoutService> {
    Arc::new(DefaultMembershipEventFanoutService::new(
        realtime_fanout,
        event_service,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::realtime_fanout::disabled_realtime_fanout_service;
    use crate::test_support::{channel_realtime_fanout_service, RecordingRealtimeEventService};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use synctv_core::models::{RoomId, UserId};
    use synctv_realtime::sync::RealtimeEvent;

    fn room_id() -> RoomId {
        RoomId::expect_positive(102_001)
    }

    fn user_id(value: &str) -> UserId {
        let id = match value {
            "target" => 102_002,
            "actor" => 102_003,
            "self-joiner" => 102_004,
            _ => 102_099,
        };
        UserId::expect_positive(id)
    }

    fn permission_snapshot(
        target_user_id: UserId,
        changed_by: UserId,
    ) -> PermissionChangedOutboxSnapshot {
        PermissionChangedOutboxSnapshot {
            room_id: room_id(),
            target_user_id,
            target_username: "target-user".to_string(),
            changed_by,
            changed_by_username: "actor-user".to_string(),
            new_permissions: synctv_core::models::RoomPermissionSet(7),
            role: i32::from(synctv_core::models::RoomRole::Member),
            added_permissions: synctv_core::models::RoomPermissionSet(1),
            removed_permissions: synctv_core::models::RoomPermissionSet(2),
            admin_added_permissions: synctv_core::models::RoomPermissionSet(0),
            admin_removed_permissions: synctv_core::models::RoomPermissionSet(0),
        }
    }

    #[tokio::test]
    async fn test_permission_changed_self_event_broadcasts_locally_after_commit() {
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = super::DefaultMembershipEventFanoutService::new(
            disabled_realtime_fanout_service(),
            event_service.clone(),
        );
        let user = user_id("self-joiner");
        let prepared = service.prepare_permission_changed_outbox_fanout(user, user);
        let factory = prepared.outbox_factory();

        let event = factory(&permission_snapshot(user, user))
            .expect("permission change should prepare a durable resource event");
        assert!(!event.enqueue_outbox);
        prepared.publish_after_outbox_commit();

        assert_eq!(event_service.room_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_permission_changed_publishes_same_prepared_event_after_commit() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let prepared = PreparedPermissionChangedFanout::new(
            channel_realtime_fanout_service(tx),
            Arc::new(RecordingRealtimeEventService::default()),
            user_id("target"),
            user_id("actor"),
        );
        let factory = prepared.outbox_factory();
        let event = factory(&permission_snapshot(user_id("target"), user_id("actor")))
            .expect("permission change should prepare a durable resource event");
        assert!(!event.enqueue_outbox);
        prepared.publish_after_outbox_commit();
        let event = rx.recv().await.expect("prepared event should publish");
        assert!(matches!(
            event.event,
            RealtimeEvent::PermissionChanged { .. }
        ));
    }

    #[tokio::test]
    async fn test_user_left_publishes_same_prepared_event_after_commit() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let prepared = PreparedUserLeftFanout::new(channel_realtime_fanout_service(tx));
        let factory = prepared.outbox_factory();
        let event = factory(&UserLeftOutboxSnapshot {
            room_id: room_id(),
            user_id: user_id("target"),
            username: "target-user".to_string(),
        })
        .expect("user left should prepare a durable resource event");
        assert!(!event.enqueue_outbox);
        prepared.publish_after_outbox_commit();
        let event = rx.recv().await.expect("prepared event should publish");
        assert!(matches!(event.event, RealtimeEvent::UserLeft { .. }));
    }
}
