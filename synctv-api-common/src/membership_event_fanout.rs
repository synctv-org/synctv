use std::sync::Arc;
use synctv_core::models::RoomId;
use synctv_realtime::fanout::{
    LocalRealtimeEventPublisher, RealtimeEventService, RealtimeFanoutService,
};
use synctv_realtime::sync::RealtimeEvent;

pub use synctv_realtime::fanout::{
    MembershipEventFanoutService, PreparedPermissionChangedFanout, PreparedUserLeftFanout,
};

struct RealtimeEventServiceLocalPublisher {
    event_service: Arc<dyn RealtimeEventService>,
}

impl RealtimeEventServiceLocalPublisher {
    fn new(event_service: Arc<dyn RealtimeEventService>) -> Self {
        Self { event_service }
    }
}

impl LocalRealtimeEventPublisher for RealtimeEventServiceLocalPublisher {
    fn broadcast_room_local(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize {
        self.event_service.broadcast_local(room_id, event)
    }
}

pub fn local_realtime_event_publisher(
    event_service: Arc<dyn RealtimeEventService>,
) -> Arc<dyn LocalRealtimeEventPublisher> {
    Arc::new(RealtimeEventServiceLocalPublisher::new(event_service))
}

pub struct DefaultMembershipEventFanoutService {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    local_event_publisher: Arc<dyn LocalRealtimeEventPublisher>,
}

impl DefaultMembershipEventFanoutService {
    #[must_use]
    pub fn new(
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
        event_service: Arc<dyn RealtimeEventService>,
    ) -> Self {
        Self {
            realtime_fanout,
            local_event_publisher: local_realtime_event_publisher(event_service),
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
        target_is_online: bool,
        target_connection_count: usize,
    ) -> PreparedPermissionChangedFanout {
        PreparedPermissionChangedFanout::new(
            self.realtime_fanout.clone(),
            self.local_event_publisher.clone(),
            target_is_online,
            target_connection_count,
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
    use synctv_core::service::{PermissionChangedOutboxSnapshot, UserLeftOutboxSnapshot};
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
            target_remark_name: String::new(),
            target_display_tag: String::new(),
            changed_by,
            changed_by_username: "actor-user".to_string(),
            role_changed: false,
            new_permissions: synctv_core::models::RoomPermissionSet(7),
            role: synctv_core::models::RoomRole::Member,
            added_permissions: synctv_core::models::RoomPermissionSet(1),
            removed_permissions: synctv_core::models::RoomPermissionSet(2),
            admin_added_permissions: synctv_core::models::RoomPermissionSet(0),
            admin_removed_permissions: synctv_core::models::RoomPermissionSet(0),
        }
    }

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    #[tokio::test]
    async fn test_permission_changed_self_event_broadcasts_locally_after_commit() -> TestResult {
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = super::DefaultMembershipEventFanoutService::new(
            disabled_realtime_fanout_service(),
            event_service.clone(),
        );
        let user = user_id("self-joiner");
        let prepared = service.prepare_permission_changed_outbox_fanout(true, 2);
        let factory = prepared.outbox_factory();

        let event = factory(&permission_snapshot(user, user))?;
        assert!(!event.enqueue_outbox);
        prepared.publish_after_outbox_commit();

        assert_eq!(event_service.room_calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_permission_changed_publishes_same_prepared_event_after_commit() -> TestResult {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let prepared = PreparedPermissionChangedFanout::new(
            channel_realtime_fanout_service(tx),
            local_realtime_event_publisher(Arc::new(RecordingRealtimeEventService::default())),
            false,
            0,
        );
        let factory = prepared.outbox_factory();
        let event = factory(&permission_snapshot(user_id("target"), user_id("actor")))?;
        assert!(!event.enqueue_outbox);
        prepared.publish_after_outbox_commit();
        let event = rx
            .recv()
            .await
            .ok_or_else(|| test_error("prepared event should publish"))?;
        assert!(matches!(
            event.event,
            RealtimeEvent::PermissionChanged {
                target_is_online: false,
                target_connection_count: 0,
                ..
            }
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_user_left_publishes_same_prepared_event_after_commit() -> TestResult {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let prepared = PreparedUserLeftFanout::new(channel_realtime_fanout_service(tx));
        let factory = prepared.outbox_factory();
        let event = factory(&UserLeftOutboxSnapshot {
            room_id: room_id(),
            user_id: user_id("target"),
            username: "target-user".to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
            role: synctv_core::models::RoomRole::Admin,
        })?;
        assert!(!event.enqueue_outbox);
        prepared.publish_after_outbox_commit();
        let event = rx
            .recv()
            .await
            .ok_or_else(|| test_error("prepared event should publish"))?;
        assert!(matches!(event.event, RealtimeEvent::UserLeft { .. }));
        Ok(())
    }
}
