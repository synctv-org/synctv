use std::sync::Arc;
use synctv_core::models::RoomId;
use synctv_realtime::fanout::{publish_best_effort, RealtimeFanoutService};
use synctv_realtime::sync::{CacheTarget, PublishRequest, RealtimeEvent};

pub use synctv_realtime::fanout::RoomCacheFanoutService;

pub struct DefaultRoomCacheFanoutService {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl DefaultRoomCacheFanoutService {
    #[must_use]
    pub fn new(realtime_fanout: Arc<dyn RealtimeFanoutService>) -> Self {
        Self { realtime_fanout }
    }
}

impl std::fmt::Debug for DefaultRoomCacheFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultRoomCacheFanoutService")
            .field(
                "realtime_fanout_distributed",
                &self.realtime_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait::async_trait]
impl RoomCacheFanoutService for DefaultRoomCacheFanoutService {
    fn publish_invalidation(&self, room_id: &RoomId) {
        publish_best_effort(
            self.realtime_fanout.clone(),
            PublishRequest::new(RealtimeEvent::CacheInvalidate {
                event_id: synctv_common::snanoid!(16),
                targets: vec![CacheTarget::Room { room_id: *room_id }],
                timestamp: synctv_core::SystemClock.now(),
            }),
        );
    }

    async fn try_publish_all_invalidation(&self) -> bool {
        self.realtime_fanout
            .try_publish(PublishRequest::new(RealtimeEvent::CacheInvalidate {
                event_id: synctv_common::snanoid!(16),
                targets: vec![CacheTarget::All],
                timestamp: synctv_core::SystemClock.now(),
            }))
            .await
    }
}

#[must_use]
pub fn default_room_cache_fanout_service(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
) -> Arc<dyn RoomCacheFanoutService> {
    Arc::new(DefaultRoomCacheFanoutService::new(realtime_fanout))
}

#[cfg(test)]
mod tests {
    use super::default_room_cache_fanout_service;
    use crate::realtime_fanout::disabled_realtime_fanout_service;
    use crate::test_support::channel_realtime_fanout_service;
    use synctv_core::models::RoomId;
    use synctv_realtime::sync::{CacheTarget, RealtimeEvent};

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    #[tokio::test]
    async fn test_room_cache_fanout_is_noop_when_realtime_fanout_is_local() {
        let service = default_room_cache_fanout_service(disabled_realtime_fanout_service());

        service.publish_invalidation(&RoomId::expect_positive(109_001));
    }

    #[tokio::test]
    async fn test_room_cache_fanout_publishes_room_target_invalidation() -> TestResult {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_cache_fanout_service(channel_realtime_fanout_service(tx));
        let room_id = RoomId::expect_positive(109_002);

        service.publish_invalidation(&room_id);

        let request = rx
            .recv()
            .await
            .ok_or_else(|| test_error("publish request should be queued"))?;
        match request.event {
            RealtimeEvent::CacheInvalidate {
                targets, event_id, ..
            } => {
                assert_eq!(targets.len(), 1);
                match &targets[0] {
                    CacheTarget::Room { room_id } => {
                        assert_eq!(room_id, &RoomId::expect_positive(109_002));
                    }
                    other => {
                        return Err(test_error(format!(
                            "expected CacheTarget::Room, got {other:?}"
                        )))
                    }
                }
                assert!(!event_id.is_empty());
            }
            other => {
                return Err(test_error(format!(
                    "expected CacheInvalidate, got {other:?}"
                )))
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_room_cache_fanout_publishes_all_target_invalidation() -> TestResult {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_cache_fanout_service(channel_realtime_fanout_service(tx));

        assert!(
            service.try_publish_all_invalidation().await,
            "all-target cache invalidation should publish"
        );

        let request = rx
            .recv()
            .await
            .ok_or_else(|| test_error("publish request should be queued"))?;
        match request.event {
            RealtimeEvent::CacheInvalidate { targets, .. } => {
                assert_eq!(targets.len(), 1);
                assert!(matches!(targets[0], CacheTarget::All));
            }
            other => {
                return Err(test_error(format!(
                    "expected CacheInvalidate, got {other:?}"
                )))
            }
        }
        Ok(())
    }
}
