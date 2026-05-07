use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{CacheTarget, ClusterEvent, PublishRequest};
use synctv_core::models::RoomId;

use crate::cluster_fanout::{publish_best_effort, ClusterFanoutService};

#[async_trait]
pub trait RoomCacheFanoutService: Send + Sync {
    fn publish_invalidation(&self, room_id: &RoomId);

    async fn try_publish_all_invalidation(&self) -> bool;
}

pub struct DefaultRoomCacheFanoutService {
    cluster_fanout: Arc<dyn ClusterFanoutService>,
}

impl DefaultRoomCacheFanoutService {
    #[must_use]
    pub fn new(cluster_fanout: Arc<dyn ClusterFanoutService>) -> Self {
        Self { cluster_fanout }
    }
}

impl std::fmt::Debug for DefaultRoomCacheFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultRoomCacheFanoutService")
            .field(
                "cluster_fanout_distributed",
                &self.cluster_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl RoomCacheFanoutService for DefaultRoomCacheFanoutService {
    fn publish_invalidation(&self, room_id: &RoomId) {
        publish_best_effort(
            self.cluster_fanout.clone(),
            PublishRequest {
                event: ClusterEvent::CacheInvalidate {
                    event_id: synctv_common::snanoid!(16),
                    targets: vec![CacheTarget::Room { room_id: *room_id }],
                    timestamp: chrono::Utc::now(),
                },
            },
        );
    }

    async fn try_publish_all_invalidation(&self) -> bool {
        self.cluster_fanout
            .try_publish(PublishRequest {
                event: ClusterEvent::CacheInvalidate {
                    event_id: synctv_common::snanoid!(16),
                    targets: vec![CacheTarget::All],
                    timestamp: chrono::Utc::now(),
                },
            })
            .await
    }
}

#[must_use]
pub fn default_room_cache_fanout_service(
    cluster_fanout: Arc<dyn ClusterFanoutService>,
) -> Arc<dyn RoomCacheFanoutService> {
    Arc::new(DefaultRoomCacheFanoutService::new(cluster_fanout))
}

#[cfg(test)]
mod tests {
    use super::default_room_cache_fanout_service;
    use crate::cluster_fanout::default_cluster_fanout_service;
    use crate::test_support::channel_cluster_fanout_service;
    use synctv_cluster::sync::{CacheTarget, ClusterEvent};
    use synctv_core::models::RoomId;

    #[tokio::test]
    async fn test_room_cache_fanout_is_noop_when_cluster_fanout_is_local() {
        let service =
            default_room_cache_fanout_service(default_cluster_fanout_service(None, false));

        service.publish_invalidation(&RoomId::from(109_001));
    }

    #[tokio::test]
    async fn test_room_cache_fanout_publishes_room_target_invalidation() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_cache_fanout_service(channel_cluster_fanout_service(tx));
        let room_id = RoomId::from(109_002);

        service.publish_invalidation(&room_id);

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            ClusterEvent::CacheInvalidate {
                targets, event_id, ..
            } => {
                assert_eq!(targets.len(), 1);
                match &targets[0] {
                    CacheTarget::Room { room_id } => {
                        assert_eq!(room_id, &RoomId::from(109_002));
                    }
                    other => panic!("expected CacheTarget::Room, got {other:?}"),
                }
                assert!(!event_id.is_empty());
            }
            other => panic!("expected CacheInvalidate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_room_cache_fanout_publishes_all_target_invalidation() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_cache_fanout_service(channel_cluster_fanout_service(tx));

        assert!(
            service.try_publish_all_invalidation().await,
            "all-target cache invalidation should publish"
        );

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            ClusterEvent::CacheInvalidate { targets, .. } => {
                assert_eq!(targets.len(), 1);
                assert!(matches!(targets[0], CacheTarget::All));
            }
            other => panic!("expected CacheInvalidate, got {other:?}"),
        }
    }
}
