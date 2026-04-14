use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::models::{MediaId, RoomId, UserId};

use crate::cluster_fanout::ClusterFanoutService;
use crate::impls::{ApiError, ClusterEventPublishReservation};

#[async_trait]
pub trait MediaFanoutService: Send + Sync {
    async fn reserve_added(
        &self,
        count: usize,
    ) -> Result<Vec<ClusterEventPublishReservation>, ApiError>;

    async fn reserve_removed(
        &self,
        count: usize,
    ) -> Result<Vec<ClusterEventPublishReservation>, ApiError>;

    async fn reserve_updated(
        &self,
        count: usize,
    ) -> Result<Vec<ClusterEventPublishReservation>, ApiError>;

    async fn reserve_removed_batch(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    fn publish_added(
        &self,
        reservation: ClusterEventPublishReservation,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    );

    fn publish_removed(
        &self,
        reservation: ClusterEventPublishReservation,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
    );

    fn publish_updated(
        &self,
        reservation: ClusterEventPublishReservation,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    );

    fn publish_removed_batch(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_ids: Vec<MediaId>,
    );
}

pub struct DefaultMediaFanoutService {
    cluster_fanout: Arc<dyn ClusterFanoutService>,
}

impl DefaultMediaFanoutService {
    #[must_use]
    pub fn new(cluster_fanout: Arc<dyn ClusterFanoutService>) -> Self {
        Self { cluster_fanout }
    }

    async fn reserve_many(
        &self,
        count: usize,
        failure_message: &'static str,
    ) -> Result<Vec<ClusterEventPublishReservation>, ApiError> {
        let mut reservations = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(reservation) = self.cluster_fanout.reserve(failure_message).await? {
                reservations.push(reservation);
            }
        }
        Ok(reservations)
    }
}

impl std::fmt::Debug for DefaultMediaFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultMediaFanoutService")
            .field(
                "cluster_fanout_distributed",
                &self.cluster_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl MediaFanoutService for DefaultMediaFanoutService {
    async fn reserve_added(
        &self,
        count: usize,
    ) -> Result<Vec<ClusterEventPublishReservation>, ApiError> {
        self.reserve_many(count, "failed to fan out MediaAdded to cluster replicas")
            .await
    }

    async fn reserve_removed(
        &self,
        count: usize,
    ) -> Result<Vec<ClusterEventPublishReservation>, ApiError> {
        self.reserve_many(count, "failed to fan out MediaRemoved to cluster replicas")
            .await
    }

    async fn reserve_updated(
        &self,
        count: usize,
    ) -> Result<Vec<ClusterEventPublishReservation>, ApiError> {
        self.reserve_many(count, "failed to fan out MediaUpdated to cluster replicas")
            .await
    }

    async fn reserve_removed_batch(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out MediaRemovedBatch to cluster replicas")
            .await
    }

    fn publish_added(
        &self,
        reservation: ClusterEventPublishReservation,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    ) {
        reservation.publish(PublishRequest {
            event: ClusterEvent::MediaAdded {
                event_id: synctv_common::snanoid!(16),
                room_id: room_id.clone(),
                user_id: user_id.clone(),
                username: username.to_string(),
                media_id: media_id.clone(),
                media_title: media_title.to_string(),
                timestamp: chrono::Utc::now(),
            },
        });
    }

    fn publish_removed(
        &self,
        reservation: ClusterEventPublishReservation,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
    ) {
        reservation.publish(PublishRequest {
            event: ClusterEvent::MediaRemoved {
                event_id: synctv_common::snanoid!(16),
                room_id: room_id.clone(),
                user_id: user_id.clone(),
                username: username.to_string(),
                media_id: media_id.clone(),
                timestamp: chrono::Utc::now(),
            },
        });
    }

    fn publish_updated(
        &self,
        reservation: ClusterEventPublishReservation,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    ) {
        reservation.publish(PublishRequest {
            event: ClusterEvent::MediaUpdated {
                event_id: synctv_common::snanoid!(16),
                room_id: room_id.clone(),
                user_id: user_id.clone(),
                username: username.to_string(),
                media_id: media_id.clone(),
                media_title: media_title.to_string(),
                timestamp: chrono::Utc::now(),
            },
        });
    }

    fn publish_removed_batch(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_ids: Vec<MediaId>,
    ) {
        self.cluster_fanout.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::MediaRemovedBatch {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room_id.clone(),
                    user_id: user_id.clone(),
                    username: username.to_string(),
                    media_ids,
                    timestamp: chrono::Utc::now(),
                },
            },
        );
    }
}

#[must_use]
pub fn default_media_fanout_service(
    cluster_fanout: Arc<dyn ClusterFanoutService>,
) -> Arc<dyn MediaFanoutService> {
    Arc::new(DefaultMediaFanoutService::new(cluster_fanout))
}

#[cfg(test)]
mod tests {
    use super::default_media_fanout_service;
    use crate::cluster_fanout::default_cluster_fanout_service;
    use synctv_cluster::sync::ClusterEvent;
    use synctv_core::models::{MediaId, RoomId, UserId};

    fn room_id() -> RoomId {
        RoomId::from_string("room-media-fanout".to_string())
    }

    fn user_id() -> UserId {
        UserId::from_string("user-media-fanout".to_string())
    }

    fn media_id() -> MediaId {
        MediaId::from_string("media-fanout".to_string())
    }

    #[tokio::test]
    async fn test_media_fanout_is_noop_when_cluster_fanout_is_local() {
        let service = default_media_fanout_service(default_cluster_fanout_service(None, false));
        let reservations = service
            .reserve_added(1)
            .await
            .expect("local media fanout should not fail");
        assert!(reservations.is_empty());
    }

    #[tokio::test]
    async fn test_media_fanout_publishes_media_added_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_media_fanout_service(default_cluster_fanout_service(Some(tx), true));
        let mut reservations = service
            .reserve_added(1)
            .await
            .expect("cluster media fanout should reserve");
        let reservation = reservations.pop().expect("missing reservation");

        service.publish_added(
            reservation,
            &room_id(),
            &user_id(),
            "tester",
            &media_id(),
            "demo",
        );

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            ClusterEvent::MediaAdded {
                room_id,
                user_id,
                username,
                media_id,
                media_title,
                ..
            } => {
                assert_eq!(room_id.as_str(), "room-media-fanout");
                assert_eq!(user_id.as_str(), "user-media-fanout");
                assert_eq!(username, "tester");
                assert_eq!(media_id.as_str(), "media-fanout");
                assert_eq!(media_title, "demo");
            }
            other => panic!("expected MediaAdded, got {other:?}"),
        }
    }
}
