use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::models::{RoomId, UserId};

use crate::cluster_fanout::ClusterFanoutService;
use crate::impls::{ApiError, ClusterEventPublishReservation};

#[async_trait]
pub trait RoomSettingsFanoutService: Send + Sync {
    async fn reserve_settings_changed(&self)
        -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    fn publish_settings_changed(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        actor_user_id: &UserId,
        actor_username: &str,
        settings_json: Vec<u8>,
    );
}

pub struct DefaultRoomSettingsFanoutService {
    cluster_fanout: Arc<dyn ClusterFanoutService>,
}

impl DefaultRoomSettingsFanoutService {
    #[must_use]
    pub fn new(cluster_fanout: Arc<dyn ClusterFanoutService>) -> Self {
        Self { cluster_fanout }
    }
}

impl std::fmt::Debug for DefaultRoomSettingsFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultRoomSettingsFanoutService")
            .field(
                "cluster_fanout_distributed",
                &self.cluster_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl RoomSettingsFanoutService for DefaultRoomSettingsFanoutService {
    async fn reserve_settings_changed(&self) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out RoomSettingsChanged to cluster replicas")
            .await
    }

    fn publish_settings_changed(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        actor_user_id: &UserId,
        actor_username: &str,
        settings_json: Vec<u8>,
    ) {
        if let Some(reservation) = reservation {
            reservation.publish(PublishRequest {
                event: ClusterEvent::RoomSettingsChanged {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room_id.clone(),
                    user_id: actor_user_id.clone(),
                    username: actor_username.to_string(),
                    settings_json,
                    timestamp: chrono::Utc::now(),
                },
            });
        }
    }
}

#[must_use]
pub fn default_room_settings_fanout_service(
    cluster_fanout: Arc<dyn ClusterFanoutService>,
) -> Arc<dyn RoomSettingsFanoutService> {
    Arc::new(DefaultRoomSettingsFanoutService::new(cluster_fanout))
}

#[cfg(test)]
mod tests {
    use super::default_room_settings_fanout_service;
    use crate::cluster_fanout::default_cluster_fanout_service;
    use synctv_cluster::sync::ClusterEvent;
    use synctv_core::models::{RoomId, UserId};

    fn room_id() -> RoomId {
        RoomId::from_string("room-fanout".to_string())
    }

    fn user_id() -> UserId {
        UserId::from_string("user-fanout".to_string())
    }

    #[tokio::test]
    async fn test_room_settings_fanout_is_noop_when_cluster_fanout_is_local() {
        let service = default_room_settings_fanout_service(default_cluster_fanout_service(None, false));
        let reservation = service
            .reserve_settings_changed()
            .await
            .expect("local room settings fanout should not fail");

        service.publish_settings_changed(
            reservation,
            &room_id(),
            &user_id(),
            "tester",
            br#"{"allow_guest_join":true}"#.to_vec(),
        );
    }

    #[tokio::test]
    async fn test_cluster_room_settings_fanout_publishes_when_channel_available() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service =
            default_room_settings_fanout_service(default_cluster_fanout_service(Some(tx), true));

        let reservation = service
            .reserve_settings_changed()
            .await
            .expect("cluster room settings fanout should reserve");

        service.publish_settings_changed(
            reservation,
            &room_id(),
            &user_id(),
            "tester",
            br#"{"require_password":false}"#.to_vec(),
        );

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            ClusterEvent::RoomSettingsChanged {
                room_id,
                user_id,
                username,
                settings_json,
                ..
            } => {
                assert_eq!(room_id.as_str(), "room-fanout");
                assert_eq!(user_id.as_str(), "user-fanout");
                assert_eq!(username, "tester");
                assert_eq!(settings_json, br#"{"require_password":false}"#.to_vec());
            }
            other => panic!("expected RoomSettingsChanged, got {other:?}"),
        }
    }
}
