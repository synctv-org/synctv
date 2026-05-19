use async_trait::async_trait;
use std::collections::BTreeSet;
use std::sync::Arc;
use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::service::user::UserDeletionSummary;
use synctv_core::service::RoomService;
use synctv_livestream::api::LiveStreamingInfrastructure;
use synctv_livestream::error::StreamError;
use synctv_realtime::sync::{PublishRequest, RealtimeEvent};

use crate::realtime_fanout::RealtimeFanoutService;
use crate::runtime::RealtimeConnectionService;

pub struct DeletedRoomAfterCommitFanout {
    pub room_id: RoomId,
    pub event: RealtimeEvent,
}

#[async_trait]
pub trait RealtimeLifecycleService: Send + Sync {
    async fn kick_stream(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        reason: &str,
    ) -> Result<(), StreamError>;

    async fn kick_local_stream(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<(), StreamError>;

    async fn active_room_stream_media_ids(&self, room_id: &RoomId) -> Vec<MediaId>;

    async fn disconnect_room(&self, room_id: &RoomId, publisher_reason: &str);

    async fn disconnect_user_from_room(&self, room_id: &RoomId, user_id: &UserId);

    async fn disconnect_user(&self, user_id: &UserId, reason: &str);

    async fn finalize_user_deletion(
        &self,
        room_service: &RoomService,
        summary: &UserDeletionSummary,
        _deleted_by: &UserId,
        disconnect_reason: &str,
        deleted_room_fanout: Vec<DeletedRoomAfterCommitFanout>,
    );
}

pub struct DefaultRealtimeLifecycleService {
    connection_service: Arc<dyn RealtimeConnectionService>,
    live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl DefaultRealtimeLifecycleService {
    #[must_use]
    pub fn new(
        connection_service: Arc<dyn RealtimeConnectionService>,
        live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
    ) -> Self {
        Self {
            connection_service,
            live_streaming_infrastructure,
            realtime_fanout,
        }
    }
}

impl std::fmt::Debug for DefaultRealtimeLifecycleService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultRealtimeLifecycleService")
            .field(
                "live_streaming_enabled",
                &self.live_streaming_infrastructure.is_some(),
            )
            .field(
                "realtime_fanout_distributed",
                &self.realtime_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl RealtimeLifecycleService for DefaultRealtimeLifecycleService {
    async fn kick_stream(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        reason: &str,
    ) -> Result<(), StreamError> {
        let mut remote_owner = false;
        if let Some(infra) = &self.live_streaming_infrastructure {
            let room_id_key = room_id.to_string();
            let media_id_key = media_id.to_string();
            match infra
                .stream_owned_by_local_node(&room_id_key, &media_id_key)
                .await
            {
                Ok(true) => self.kick_local_stream(room_id, media_id).await?,
                Ok(false) => {
                    remote_owner = true;
                    tracing::debug!(
                        room_id = %room_id,
                        media_id = %media_id,
                        "Stream is not owned by this node; publishing replica-wide kick without local unpublish"
                    );
                }
                Err(error) => {
                    let stream_error = StreamError::StreamHubError(error.to_string());
                    tracing::warn!(
                        room_id = %room_id,
                        media_id = %media_id,
                        error = %stream_error,
                        "Failed to determine stream owner before publishing kick"
                    );
                    return Err(stream_error);
                }
            }
        }

        if !self
            .realtime_fanout
            .try_publish(PublishRequest::new(RealtimeEvent::KickPublisher {
                event_id: synctv_common::snanoid!(16),
                room_id: *room_id,
                media_id: *media_id,
                reason: reason.to_string(),
                timestamp: chrono::Utc::now(),
            }))
            .await
            && self.realtime_fanout.is_distributed_enabled()
        {
            tracing::warn!(
                room_id = %room_id,
                media_id = %media_id,
                "Failed to send replica-wide kick event after bounded retry"
            );
            if remote_owner {
                return Err(StreamError::StreamHubError(
                    "Failed to publish remote stream kick event".to_string(),
                ));
            }
        }

        Ok(())
    }

    async fn kick_local_stream(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<(), StreamError> {
        let room_id_key = room_id.to_string();
        let media_id_key = media_id.to_string();
        if let Some(infra) = &self.live_streaming_infrastructure {
            if let Err(error) = infra.kick_publisher(&room_id_key, &media_id_key) {
                let stream_error = StreamError::StreamHubError(error.to_string());
                tracing::warn!(
                    room_id = %room_id,
                    media_id = %media_id,
                    error = %stream_error,
                    "Failed to kick local publisher"
                );
                return Err(stream_error);
            }
        }
        Ok(())
    }

    async fn active_room_stream_media_ids(&self, room_id: &RoomId) -> Vec<MediaId> {
        let mut media_ids = BTreeSet::new();
        let room_id_key = room_id.to_string();

        if let Some(infra) = &self.live_streaming_infrastructure {
            for media_id in infra.user_stream_tracker.get_room_streams(&room_id_key) {
                match media_id.parse::<MediaId>() {
                    Ok(media_id) => {
                        media_ids.insert(media_id);
                    }
                    Err(error) => tracing::warn!(
                        room_id = %room_id,
                        media_id = %media_id,
                        error = %error,
                        "Ignoring invalid media id from local live stream tracker"
                    ),
                }
            }

            match infra.registry.list_streams_for_room(&room_id_key).await {
                Ok(remote_media_ids) => {
                    for media_id in remote_media_ids {
                        match media_id.parse::<MediaId>() {
                            Ok(media_id) => {
                                media_ids.insert(media_id);
                            }
                            Err(error) => tracing::warn!(
                                room_id = %room_id,
                                media_id = %media_id,
                                error = %error,
                                "Ignoring invalid media id from live stream registry"
                            ),
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        room_id = %room_id,
                        error = %error,
                        "Failed to list room streams from registry; falling back to local tracker view"
                    );
                }
            }
        }

        media_ids.into_iter().collect()
    }

    async fn disconnect_room(&self, room_id: &RoomId, _publisher_reason: &str) {
        self.connection_service.disconnect_room(room_id);
        let room_id_key = room_id.to_string();

        if let Some(infra) = &self.live_streaming_infrastructure {
            infra.kick_room_publishers(&room_id_key).await;
        }
    }

    async fn disconnect_user_from_room(&self, room_id: &RoomId, user_id: &UserId) {
        self.connection_service
            .disconnect_user_from_room(user_id, room_id);

        if let Some(infra) = &self.live_streaming_infrastructure {
            let room_id_key = room_id.to_string();
            let user_id_key = user_id.to_string();
            infra
                .kick_user_room_publishers(&room_id_key, &user_id_key)
                .await;
        }
    }

    async fn disconnect_user(&self, user_id: &UserId, reason: &str) {
        self.connection_service.disconnect_user(user_id);

        if let Some(infra) = &self.live_streaming_infrastructure {
            let user_id_key = user_id.to_string();
            let streams = infra.user_stream_tracker.get_user_streams(&user_id_key);

            for (room_id, media_id) in &streams {
                let (Ok(room_id), Ok(media_id)) =
                    (room_id.parse::<RoomId>(), media_id.parse::<MediaId>())
                else {
                    tracing::warn!(
                        room_id = %room_id,
                        media_id = %media_id,
                        "Ignoring invalid live stream tracker entry while disconnecting user"
                    );
                    continue;
                };
                if let Err(error) = self.kick_stream(&room_id, &media_id, reason).await {
                    tracing::warn!(
                        room_id = %room_id,
                        media_id = %media_id,
                        error = %error,
                        "Failed to kick publisher while disconnecting user"
                    );
                }
            }

            infra.kick_user_publishers(&user_id_key).await;
        }

        let _ = self
            .realtime_fanout
            .try_publish(PublishRequest::new(RealtimeEvent::KickUser {
                event_id: synctv_common::snanoid!(16),
                user_id: *user_id,
                reason: reason.to_string(),
                timestamp: chrono::Utc::now(),
            }))
            .await;
    }

    async fn finalize_user_deletion(
        &self,
        room_service: &RoomService,
        summary: &UserDeletionSummary,
        _deleted_by: &UserId,
        disconnect_reason: &str,
        deleted_room_fanout: Vec<DeletedRoomAfterCommitFanout>,
    ) {
        for room_id in &summary.membership_room_ids {
            room_service
                .permission_service()
                .invalidate_cache(room_id, &summary.user_id)
                .await;
        }

        for impact in &summary.modified_rooms {
            room_service
                .finalize_entry_deletions_after_commit(
                    &impact.room_id,
                    &impact.deleted_media_ids,
                    impact.playback_state.as_ref(),
                )
                .await;

            for media_id in &impact.deleted_media_ids {
                if let Err(error) = self
                    .kick_stream(&impact.room_id, media_id, "user_resource_deleted")
                    .await
                {
                    tracing::warn!(
                        room_id = %impact.room_id,
                        media_id = %media_id,
                        error = %error,
                        "Failed to kick publisher while finalizing user deletion"
                    );
                }
            }
        }

        for deleted_room in deleted_room_fanout {
            let room_id = deleted_room.room_id;
            room_service
                .finalize_deleted_room_after_commit(&room_id)
                .await;

            self.realtime_fanout
                .publish_after_outbox_commit(deleted_room.event);

            self.disconnect_room(&room_id, "room_deleted").await;
        }

        self.disconnect_user(&summary.user_id, disconnect_reason)
            .await;
    }
}

#[must_use]
pub fn default_realtime_lifecycle_service(
    connection_service: Arc<dyn RealtimeConnectionService>,
    live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
) -> Arc<dyn RealtimeLifecycleService> {
    Arc::new(DefaultRealtimeLifecycleService::new(
        connection_service,
        live_streaming_infrastructure,
        realtime_fanout,
    ))
}

#[cfg(test)]
mod tests {
    use super::default_realtime_lifecycle_service;
    use crate::realtime_fanout::default_realtime_fanout_service;
    use crate::runtime::RealtimeConnectionService;
    use crate::test_support::channel_realtime_fanout_service;
    use std::sync::Arc;
    use synctv_core::models::{MediaId, RoomId, UserId};
    use synctv_livestream::api::{LiveStreamingInfrastructure, StreamTracker};
    use synctv_livestream::livestream::{ExternalPublishManager, PullStreamManager};
    use synctv_realtime::sync::{ConnectionLimits, ConnectionManager, RealtimeEvent};
    #[tokio::test]
    async fn test_realtime_lifecycle_kick_stream_uses_realtime_fanout_service() {
        let connection_service: Arc<dyn RealtimeConnectionService> =
            Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        let (publish_tx, mut publish_rx) = tokio::sync::mpsc::channel(4);
        let service = default_realtime_lifecycle_service(
            connection_service,
            None,
            channel_realtime_fanout_service(publish_tx),
        );

        service
            .kick_stream(
                &RoomId::expect_positive(1001),
                &MediaId::expect_positive(2001),
                "test-reason",
            )
            .await
            .expect("kick stream should succeed without livestream infrastructure");

        let published = publish_rx
            .recv()
            .await
            .expect("kick helper should publish exactly one realtime event");
        assert!(matches!(
            published.event,
            RealtimeEvent::KickPublisher { ref room_id, ref media_id, ref reason, .. }
                if room_id.as_i64() == 1001
                    && media_id.as_i64() == 2001
                    && reason == "test-reason"
        ));
    }

    #[tokio::test]
    async fn test_realtime_lifecycle_disconnect_user_publishes_cluster_kick_event() {
        let connection_service: Arc<dyn RealtimeConnectionService> =
            Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        connection_service.start();

        let (publish_tx, mut publish_rx) = tokio::sync::mpsc::channel(4);
        let service = default_realtime_lifecycle_service(
            Arc::clone(&connection_service),
            None,
            channel_realtime_fanout_service(publish_tx),
        );
        let user_id = UserId::expect_positive(101_001);

        service.disconnect_user(&user_id, "user_deleted").await;

        let published = publish_rx
            .recv()
            .await
            .expect("disconnect_user should publish a kick event");
        match published.event {
            RealtimeEvent::KickUser {
                user_id: published_user_id,
                reason,
                ..
            } => {
                assert_eq!(published_user_id, user_id);
                assert_eq!(reason, "user_deleted");
            }
            other => panic!("expected KickUser event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_realtime_lifecycle_disconnect_user_from_room_only_cleans_local_room_publishers() {
        let connection_service: Arc<dyn RealtimeConnectionService> =
            Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        connection_service.start();
        let mut disconnect_rx = connection_service.subscribe_disconnect();

        let user_id = UserId::expect_positive(101);
        let room_one = RoomId::expect_positive(201);
        let room_two = RoomId::expect_positive(202);
        let user_id_key = user_id.to_string();
        let room_one_key = room_one.to_string();
        let room_two_key = room_two.to_string();

        connection_service
            .register("conn-room-1".to_string(), user_id)
            .await
            .expect("room-1 connection should register");
        connection_service
            .join_room("conn-room-1", room_one)
            .await
            .expect("room-1 connection should join");
        connection_service
            .register("conn-room-2".to_string(), user_id)
            .await
            .expect("room-2 connection should register");
        connection_service
            .join_room("conn-room-2", room_two)
            .await
            .expect("room-2 connection should join");

        let registry = synctv_livestream::relay::local_stream_registry();
        registry
            .try_register_publisher(
                &room_one_key,
                "media-1",
                "test-node",
                &user_id_key,
                "127.0.0.1:50051",
            )
            .await
            .expect("room-1 publisher should register");
        registry
            .try_register_publisher(
                &room_two_key,
                "media-2",
                "test-node",
                &user_id_key,
                "127.0.0.1:50051",
            )
            .await
            .expect("room-2 publisher should register");

        let tracker = Arc::new(StreamTracker::new());
        tracker.insert(
            user_id_key.clone(),
            room_one_key.clone(),
            "media-1".to_string(),
            &room_one_key,
            "media-1",
        );
        tracker.insert(
            user_id_key,
            room_two_key.clone(),
            "media-2".to_string(),
            &room_two_key,
            "media-2",
        );

        let (event_sender, mut event_receiver) = tokio::sync::mpsc::channel(8);
        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "test-node".to_string(),
                event_sender.clone(),
                synctv_common::ssrf::SsrfGuard::disabled(),
            )
            .expect("external publish manager should build"),
        );
        let infra = Arc::new(LiveStreamingInfrastructure::new(
            registry.clone(),
            event_sender,
            pull_manager,
            external_publish_manager,
            tracker.clone(),
        ));

        let service = default_realtime_lifecycle_service(
            connection_service.clone(),
            Some(infra),
            default_realtime_fanout_service(None, false),
        );

        service.disconnect_user_from_room(&room_one, &user_id).await;

        let event = event_receiver
            .recv()
            .await
            .expect("room-scoped disconnect should enqueue one unpublish");
        let event_debug = format!("{event:?}");
        assert!(
            event_debug.contains("UnPublish"),
            "expected UnPublish event, got {event_debug}"
        );
        assert!(
            event_debug.contains(&room_one_key) && event_debug.contains("media-1"),
            "room-scoped disconnect must target the room-1 publisher, got {event_debug}"
        );

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), event_receiver.recv())
                .await
                .is_err(),
            "room-scoped disconnect must not kick publishers from other rooms"
        );

        let disconnect_signal = disconnect_rx
            .recv()
            .await
            .expect("room-scoped disconnect must emit a disconnect signal");
        assert!(matches!(
            disconnect_signal,
            synctv_realtime::sync::DisconnectSignal::UserFromRoom {
                user_id: ref signal_user_id,
                room_id: ref signal_room_id,
            } if signal_user_id == &user_id && signal_room_id == &room_one
        ));
        assert!(
            registry
                .get_publisher(&room_one_key, "media-1")
                .await
                .expect("room-1 publisher lookup should succeed")
                .is_some(),
            "room-scoped disconnect must not remove publisher ownership before StreamHub processes unpublish"
        );
        assert!(
            registry
                .get_publisher(&room_two_key, "media-2")
                .await
                .expect("room-2 publisher lookup should succeed")
                .is_some(),
            "room-scoped disconnect must preserve publishers from other rooms"
        );

        registry
            .unregister_publisher(&room_one_key, "media-1")
            .await
            .expect("test unpublish completion should unregister room-1 publisher");
        let _ = tracker.remove_stream(&room_one_key, "media-1");

        assert!(
            registry
                .get_publisher(&room_one_key, "media-1")
                .await
                .expect("room-1 publisher lookup should succeed")
                .is_none(),
            "actual unpublish completion must remove the matching room publisher"
        );
        assert!(
            registry
                .get_publisher(&room_two_key, "media-2")
                .await
                .expect("room-2 publisher lookup should succeed")
                .is_some(),
            "actual unpublish completion must preserve publishers from other rooms"
        );
    }
}
