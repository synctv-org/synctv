use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use synctv_cluster::sync::ClusterEvent;
use synctv_core::models::{Room, RoomId, UserId};
use synctv_core::repository::cluster_outbox::NewClusterOutboxEvent;
use synctv_core::service::ClusterOutboxRoomEventFactory;

use crate::cluster_fanout::ClusterFanoutService;

#[derive(Clone)]
pub struct PreparedRoomCreatedOutboxFanout {
    cluster_fanout: Arc<dyn ClusterFanoutService>,
    events: Arc<Mutex<Vec<ClusterEvent>>>,
    creator_id: UserId,
}

impl PreparedRoomCreatedOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> Option<ClusterOutboxRoomEventFactory> {
        if !self.cluster_fanout.is_distributed_enabled() {
            return None;
        }

        let prepared = self.clone();
        Some(Arc::new(move |room: &Room| {
            let event = ClusterEvent::RoomCreated {
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
            prepared.cluster_fanout.outbox_event(&event)
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
            self.cluster_fanout.publish_after_outbox_commit(event);
        }
    }
}

#[derive(Clone)]
pub struct PreparedRoomLifecycleOutboxFanout {
    pub event: ClusterEvent,
    pub outbox_event: Option<NewClusterOutboxEvent>,
    cluster_fanout: Arc<dyn ClusterFanoutService>,
}

impl PreparedRoomLifecycleOutboxFanout {
    pub fn publish_after_outbox_commit(self) {
        self.cluster_fanout.publish_after_outbox_commit(self.event);
    }
}

#[async_trait]
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
    cluster_fanout: Arc<dyn ClusterFanoutService>,
}

impl DefaultRoomLifecycleFanoutService {
    #[must_use]
    pub fn new(cluster_fanout: Arc<dyn ClusterFanoutService>) -> Self {
        Self { cluster_fanout }
    }
}

impl std::fmt::Debug for DefaultRoomLifecycleFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultRoomLifecycleFanoutService")
            .field(
                "cluster_fanout_distributed",
                &self.cluster_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl RoomLifecycleFanoutService for DefaultRoomLifecycleFanoutService {
    fn prepare_room_created_outbox_fanout(
        &self,
        creator_id: UserId,
    ) -> PreparedRoomCreatedOutboxFanout {
        PreparedRoomCreatedOutboxFanout {
            cluster_fanout: self.cluster_fanout.clone(),
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
        let outbox_event = self.cluster_fanout.outbox_event(&event);
        PreparedRoomLifecycleOutboxFanout {
            event,
            outbox_event,
            cluster_fanout: self.cluster_fanout.clone(),
        }
    }

    fn prepare_room_banned_outbox_fanout(
        &self,
        room_id: &RoomId,
        banned_by: &UserId,
    ) -> PreparedRoomLifecycleOutboxFanout {
        let event = room_banned_event(room_id, banned_by);
        let outbox_event = self.cluster_fanout.outbox_event(&event);
        PreparedRoomLifecycleOutboxFanout {
            event,
            outbox_event,
            cluster_fanout: self.cluster_fanout.clone(),
        }
    }

    fn prepare_room_owner_inactive_outbox_fanout(
        &self,
        room_id: &RoomId,
        owner_id: &UserId,
        triggered_by: &UserId,
    ) -> PreparedRoomLifecycleOutboxFanout {
        let event = room_owner_inactive_event(room_id, owner_id, triggered_by);
        let outbox_event = self.cluster_fanout.outbox_event(&event);
        PreparedRoomLifecycleOutboxFanout {
            event,
            outbox_event,
            cluster_fanout: self.cluster_fanout.clone(),
        }
    }
}

fn room_deleted_event(room_id: &RoomId, deleted_by: &UserId) -> ClusterEvent {
    ClusterEvent::RoomDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        deleted_by: *deleted_by,
        timestamp: chrono::Utc::now(),
    }
}

fn room_banned_event(room_id: &RoomId, banned_by: &UserId) -> ClusterEvent {
    ClusterEvent::RoomBanned {
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
) -> ClusterEvent {
    ClusterEvent::RoomOwnerInactive {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        owner_id: *owner_id,
        triggered_by: *triggered_by,
        timestamp: chrono::Utc::now(),
    }
}

#[must_use]
pub fn default_room_lifecycle_fanout_service(
    cluster_fanout: Arc<dyn ClusterFanoutService>,
) -> Arc<dyn RoomLifecycleFanoutService> {
    Arc::new(DefaultRoomLifecycleFanoutService::new(cluster_fanout))
}

#[cfg(test)]
mod tests {
    use super::default_room_lifecycle_fanout_service;
    use crate::test_support::channel_cluster_fanout_service;
    use synctv_cluster::sync::ClusterEvent;
    use synctv_core::models::{RoomId, UserId};

    fn room_id() -> RoomId {
        RoomId::from(104_001)
    }

    fn user_id() -> UserId {
        UserId::from(104_002)
    }

    #[tokio::test]
    async fn test_room_lifecycle_fanout_publishes_prepared_room_deleted_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_lifecycle_fanout_service(channel_cluster_fanout_service(tx));

        service
            .prepare_room_deleted_outbox_fanout(&room_id(), &user_id())
            .publish_after_outbox_commit();

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            ClusterEvent::RoomDeleted {
                room_id,
                deleted_by,
                ..
            } => {
                assert_eq!(room_id, RoomId::from(104_001));
                assert_eq!(deleted_by, UserId::from(104_002));
            }
            other => panic!("expected RoomDeleted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_room_lifecycle_fanout_publishes_prepared_room_banned_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_lifecycle_fanout_service(channel_cluster_fanout_service(tx));

        service
            .prepare_room_banned_outbox_fanout(&room_id(), &user_id())
            .publish_after_outbox_commit();

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            ClusterEvent::RoomBanned {
                room_id, banned_by, ..
            } => {
                assert_eq!(room_id, RoomId::from(104_001));
                assert_eq!(banned_by, UserId::from(104_002));
            }
            other => panic!("expected RoomBanned, got {other:?}"),
        }
    }
}
