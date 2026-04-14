use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::models::{Playlist, PlaylistId, RoomId, UserId};

use crate::cluster_fanout::ClusterFanoutService;
use crate::impls::{ApiError, ClusterEventPublishReservation};

#[async_trait]
pub trait PlaylistFanoutService: Send + Sync {
    async fn reserve_created(&self) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    async fn reserve_updated(&self) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    async fn reserve_deleted(&self) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    fn publish_created(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        playlist: &Playlist,
    );

    fn publish_updated(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        playlist: &Playlist,
    );

    fn publish_deleted(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        playlist_id: &PlaylistId,
    );
}

pub struct DefaultPlaylistFanoutService {
    cluster_fanout: Arc<dyn ClusterFanoutService>,
}

impl DefaultPlaylistFanoutService {
    #[must_use]
    pub fn new(cluster_fanout: Arc<dyn ClusterFanoutService>) -> Self {
        Self { cluster_fanout }
    }
}

impl std::fmt::Debug for DefaultPlaylistFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultPlaylistFanoutService")
            .field(
                "cluster_fanout_distributed",
                &self.cluster_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl PlaylistFanoutService for DefaultPlaylistFanoutService {
    async fn reserve_created(&self) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out PlaylistCreated to cluster replicas")
            .await
    }

    async fn reserve_updated(&self) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out PlaylistUpdated to cluster replicas")
            .await
    }

    async fn reserve_deleted(&self) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out PlaylistDeleted to cluster replicas")
            .await
    }

    fn publish_created(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        playlist: &Playlist,
    ) {
        self.cluster_fanout.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::PlaylistCreated {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room_id.clone(),
                    user_id: user_id.clone(),
                    username: username.to_string(),
                    playlist: playlist.clone(),
                    timestamp: chrono::Utc::now(),
                },
            },
        );
    }

    fn publish_updated(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        playlist: &Playlist,
    ) {
        self.cluster_fanout.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::PlaylistUpdated {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room_id.clone(),
                    user_id: user_id.clone(),
                    username: username.to_string(),
                    playlist: playlist.clone(),
                    timestamp: chrono::Utc::now(),
                },
            },
        );
    }

    fn publish_deleted(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        playlist_id: &PlaylistId,
    ) {
        self.cluster_fanout.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::PlaylistDeleted {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room_id.clone(),
                    user_id: user_id.clone(),
                    username: username.to_string(),
                    playlist_id: playlist_id.clone(),
                    timestamp: chrono::Utc::now(),
                },
            },
        );
    }
}

#[must_use]
pub fn default_playlist_fanout_service(
    cluster_fanout: Arc<dyn ClusterFanoutService>,
) -> Arc<dyn PlaylistFanoutService> {
    Arc::new(DefaultPlaylistFanoutService::new(cluster_fanout))
}

#[cfg(test)]
mod tests {
    use super::default_playlist_fanout_service;
    use crate::cluster_fanout::default_cluster_fanout_service;
    use synctv_cluster::sync::ClusterEvent;
    use synctv_core::models::{Playlist, PlaylistId, RoomId, UserId};

    fn room_id() -> RoomId {
        RoomId::from_string("room-fanout".to_string())
    }

    fn user_id() -> UserId {
        UserId::from_string("user-fanout".to_string())
    }

    fn playlist() -> Playlist {
        Playlist {
            id: PlaylistId::from_string("playlist-fanout".to_string()),
            room_id: room_id(),
            creator_id: Some(user_id()),
            name: "fanout playlist".to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        }
    }

    #[tokio::test]
    async fn test_playlist_fanout_is_noop_when_cluster_fanout_is_local() {
        let service = default_playlist_fanout_service(default_cluster_fanout_service(None, false));
        let reservation = service
            .reserve_created()
            .await
            .expect("local playlist fanout should not fail");

        service.publish_created(reservation, &room_id(), &user_id(), "tester", &playlist());
    }

    #[tokio::test]
    async fn test_playlist_fanout_publishes_created_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service =
            default_playlist_fanout_service(default_cluster_fanout_service(Some(tx), true));

        let reservation = service
            .reserve_created()
            .await
            .expect("cluster playlist fanout should reserve");
        let playlist = playlist();
        service.publish_created(reservation, &room_id(), &user_id(), "tester", &playlist);

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            ClusterEvent::PlaylistCreated {
                room_id,
                user_id,
                username,
                playlist,
                ..
            } => {
                assert_eq!(room_id.as_str(), "room-fanout");
                assert_eq!(user_id.as_str(), "user-fanout");
                assert_eq!(username, "tester");
                assert_eq!(playlist.id.as_str(), "playlist-fanout");
            }
            other => panic!("expected PlaylistCreated, got {other:?}"),
        }
    }
}
