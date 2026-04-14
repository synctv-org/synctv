use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::PublishRequest;

use crate::impls::{
    reserve_cluster_event_publish, try_publish_cluster_event, ApiError,
    ClusterEventPublishReservation,
};

#[async_trait]
pub trait ClusterFanoutService: Send + Sync {
    async fn reserve(
        &self,
        failure_message: &'static str,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    fn publish(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        request: PublishRequest,
    );

    async fn try_publish(&self, request: PublishRequest) -> bool;

    fn is_distributed_enabled(&self) -> bool;
}

#[derive(Debug, Clone, Default)]
pub struct NoopClusterFanoutService;

#[async_trait]
impl ClusterFanoutService for NoopClusterFanoutService {
    async fn reserve(
        &self,
        _failure_message: &'static str,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        Ok(None)
    }

    fn publish(
        &self,
        _reservation: Option<ClusterEventPublishReservation>,
        _request: PublishRequest,
    ) {
    }

    async fn try_publish(&self, _request: PublishRequest) -> bool {
        false
    }

    fn is_distributed_enabled(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
pub struct QueuedClusterFanoutService {
    publish_tx: tokio::sync::mpsc::Sender<PublishRequest>,
}

impl QueuedClusterFanoutService {
    #[must_use]
    pub const fn new(publish_tx: tokio::sync::mpsc::Sender<PublishRequest>) -> Self {
        Self { publish_tx }
    }
}

#[async_trait]
impl ClusterFanoutService for QueuedClusterFanoutService {
    async fn reserve(
        &self,
        failure_message: &'static str,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        reserve_cluster_event_publish(Some(&self.publish_tx), true, failure_message).await
    }

    fn publish(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        request: PublishRequest,
    ) {
        if let Some(reservation) = reservation {
            reservation.publish(request);
        }
    }

    async fn try_publish(&self, request: PublishRequest) -> bool {
        try_publish_cluster_event(&self.publish_tx, request).await
    }

    fn is_distributed_enabled(&self) -> bool {
        true
    }
}

#[must_use]
pub fn default_cluster_fanout_service(
    publish_tx: Option<tokio::sync::mpsc::Sender<PublishRequest>>,
    cluster_mode: bool,
) -> Arc<dyn ClusterFanoutService> {
    if cluster_mode {
        if let Some(publish_tx) = publish_tx {
            return Arc::new(QueuedClusterFanoutService::new(publish_tx));
        }
    }

    Arc::new(NoopClusterFanoutService)
}

#[cfg(test)]
mod tests {
    use super::default_cluster_fanout_service;
    use synctv_cluster::sync::{ClusterEvent, PublishRequest};

    #[tokio::test]
    async fn test_reserve_is_noop_when_cluster_fanout_not_required() {
        let service = default_cluster_fanout_service(None, false);

        let reservation = service
            .reserve("unused failure message")
            .await
            .expect("standalone fanout should not fail");

        assert!(
            reservation.is_none(),
            "standalone fanout should not require a channel reservation"
        );
        assert!(
            !service.is_distributed_enabled(),
            "standalone fanout should report distributed mode as disabled"
        );
    }

    #[tokio::test]
    async fn test_publish_sends_reserved_request() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_cluster_fanout_service(Some(tx), true);
        let reservation = service
            .reserve("reservation should succeed")
            .await
            .expect("cluster reservation should succeed");

        service.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::SystemNotification {
                    event_id: synctv_common::snanoid!(16),
                    message: "test".to_string(),
                    level: synctv_cluster::sync::NotificationLevel::Info,
                    timestamp: chrono::Utc::now(),
                },
            },
        );

        let request = rx.recv().await.expect("reserved request should be published");
        assert!(matches!(request.event, ClusterEvent::SystemNotification { .. }));
        assert!(
            service.is_distributed_enabled(),
            "configured clustered fanout should report distributed mode as enabled"
        );
    }

    #[tokio::test]
    async fn test_cluster_fanout_without_publish_channel_degrades_to_noop() {
        let service = default_cluster_fanout_service(None, true);

        let reservation = service
            .reserve("should not fail without channel")
            .await
            .expect("missing queue should degrade to no-op fanout");

        assert!(reservation.is_none());
        assert!(
            !service.is_distributed_enabled(),
            "fanout without a publish channel must report distributed delivery as disabled"
        );
    }
}
