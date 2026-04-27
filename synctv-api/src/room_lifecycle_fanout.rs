use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::models::{RoomId, UserId};

use crate::cluster_fanout::ClusterFanoutService;
use crate::impls::{ApiError, ClusterEventPublishReservation};

#[async_trait]
pub trait RoomLifecycleFanoutService: Send + Sync {
    async fn reserve_room_created(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    async fn reserve_room_deleted(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    async fn reserve_room_banned(&self)
        -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    async fn reserve_room_owner_inactive(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    fn publish_room_created(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        room_name: &str,
        creator_id: &UserId,
    );

    fn publish_room_deleted(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        deleted_by: &UserId,
    );

    fn publish_room_banned(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        banned_by: &UserId,
    );

    fn publish_room_owner_inactive(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        owner_id: &UserId,
        triggered_by: &UserId,
    );
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
    async fn reserve_room_created(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out RoomCreated to cluster replicas")
            .await
    }

    async fn reserve_room_deleted(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out RoomDeleted to cluster replicas")
            .await
    }

    async fn reserve_room_banned(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out RoomBanned to cluster replicas")
            .await
    }

    async fn reserve_room_owner_inactive(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out RoomOwnerInactive to cluster replicas")
            .await
    }

    fn publish_room_created(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        room_name: &str,
        creator_id: &UserId,
    ) {
        self.cluster_fanout.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::RoomCreated {
                    event_id: synctv_common::snanoid!(16),
                    room_id: *room_id,
                    room_name: room_name.to_string(),
                    creator_id: *creator_id,
                    timestamp: chrono::Utc::now(),
                },
            },
        );
    }

    fn publish_room_deleted(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        deleted_by: &UserId,
    ) {
        self.cluster_fanout.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::RoomDeleted {
                    event_id: synctv_common::snanoid!(16),
                    room_id: *room_id,
                    deleted_by: *deleted_by,
                    timestamp: chrono::Utc::now(),
                },
            },
        );
    }

    fn publish_room_banned(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        banned_by: &UserId,
    ) {
        self.cluster_fanout.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::RoomBanned {
                    event_id: synctv_common::snanoid!(16),
                    room_id: *room_id,
                    banned_by: *banned_by,
                    timestamp: chrono::Utc::now(),
                },
            },
        );
    }

    fn publish_room_owner_inactive(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        owner_id: &UserId,
        triggered_by: &UserId,
    ) {
        self.cluster_fanout.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::RoomOwnerInactive {
                    event_id: synctv_common::snanoid!(16),
                    room_id: *room_id,
                    owner_id: *owner_id,
                    triggered_by: *triggered_by,
                    timestamp: chrono::Utc::now(),
                },
            },
        );
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
    use crate::cluster_fanout::default_cluster_fanout_service;
    use synctv_cluster::sync::ClusterEvent;
    use synctv_core::models::{RoomId, UserId};

    fn room_id() -> RoomId {
        RoomId::from(104_001)
    }

    fn user_id() -> UserId {
        UserId::from(104_002)
    }

    #[tokio::test]
    async fn test_room_lifecycle_fanout_is_noop_when_cluster_fanout_is_local() {
        let service =
            default_room_lifecycle_fanout_service(default_cluster_fanout_service(None, false));
        let reservation = service
            .reserve_room_created()
            .await
            .expect("local room lifecycle fanout should not fail");

        service.publish_room_created(reservation, &room_id(), "room lifecycle", &user_id());
    }

    #[tokio::test]
    async fn test_room_lifecycle_fanout_publishes_room_deleted_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service =
            default_room_lifecycle_fanout_service(default_cluster_fanout_service(Some(tx), true));

        let reservation = service
            .reserve_room_deleted()
            .await
            .expect("cluster room deleted fanout should reserve");
        service.publish_room_deleted(reservation, &room_id(), &user_id());

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
    async fn test_room_lifecycle_fanout_publishes_room_banned_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service =
            default_room_lifecycle_fanout_service(default_cluster_fanout_service(Some(tx), true));

        let reservation = service
            .reserve_room_banned()
            .await
            .expect("cluster room banned fanout should reserve");
        service.publish_room_banned(reservation, &room_id(), &user_id());

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
