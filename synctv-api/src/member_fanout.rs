use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::models::{RoomId, UserId};

use crate::cluster_fanout::ClusterFanoutService;
use crate::impls::{ApiError, ClusterEventPublishReservation};

#[async_trait]
pub trait MemberFanoutService: Send + Sync {
    async fn reserve_kick_user_from_room(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    fn publish_kick_user_from_room(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        user_id: &UserId,
        reason: &str,
    );
}

pub struct DefaultMemberFanoutService {
    cluster_fanout: Arc<dyn ClusterFanoutService>,
}

impl DefaultMemberFanoutService {
    #[must_use]
    pub fn new(cluster_fanout: Arc<dyn ClusterFanoutService>) -> Self {
        Self { cluster_fanout }
    }
}

impl std::fmt::Debug for DefaultMemberFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultMemberFanoutService")
            .field(
                "cluster_fanout_distributed",
                &self.cluster_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl MemberFanoutService for DefaultMemberFanoutService {
    async fn reserve_kick_user_from_room(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out KickUserFromRoom to cluster replicas")
            .await
    }

    fn publish_kick_user_from_room(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        user_id: &UserId,
        reason: &str,
    ) {
        self.cluster_fanout.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::KickUserFromRoom {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room_id.clone(),
                    user_id: user_id.clone(),
                    reason: reason.to_string(),
                    timestamp: chrono::Utc::now(),
                },
            },
        );
    }
}

#[must_use]
pub fn default_member_fanout_service(
    cluster_fanout: Arc<dyn ClusterFanoutService>,
) -> Arc<dyn MemberFanoutService> {
    Arc::new(DefaultMemberFanoutService::new(cluster_fanout))
}

#[cfg(test)]
mod tests {
    use super::default_member_fanout_service;
    use crate::cluster_fanout::default_cluster_fanout_service;
    use synctv_cluster::sync::ClusterEvent;
    use synctv_core::models::{RoomId, UserId};

    fn room_id() -> RoomId {
        RoomId::from_string("room-member-fanout".to_string())
    }

    fn user_id() -> UserId {
        UserId::from_string("user-member-fanout".to_string())
    }

    #[tokio::test]
    async fn test_member_fanout_is_noop_when_cluster_fanout_is_local() {
        let service = default_member_fanout_service(default_cluster_fanout_service(None, false));
        let reservation = service
            .reserve_kick_user_from_room()
            .await
            .expect("local member fanout should not fail");

        service.publish_kick_user_from_room(reservation, &room_id(), &user_id(), "kicked");
    }

    #[tokio::test]
    async fn test_member_fanout_publishes_kick_user_from_room_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_member_fanout_service(default_cluster_fanout_service(Some(tx), true));

        let reservation = service
            .reserve_kick_user_from_room()
            .await
            .expect("cluster member fanout should reserve");

        service.publish_kick_user_from_room(reservation, &room_id(), &user_id(), "banned");

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            ClusterEvent::KickUserFromRoom {
                room_id,
                user_id,
                reason,
                ..
            } => {
                assert_eq!(room_id.as_str(), "room-member-fanout");
                assert_eq!(user_id.as_str(), "user-member-fanout");
                assert_eq!(reason, "banned");
            }
            other => panic!("expected KickUserFromRoom, got {other:?}"),
        }
    }
}
