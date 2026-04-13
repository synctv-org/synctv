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

#[derive(Debug, Clone)]
pub struct DefaultClusterFanoutService {
    publish_tx: Option<tokio::sync::mpsc::Sender<PublishRequest>>,
    cluster_mode: bool,
}

impl DefaultClusterFanoutService {
    #[must_use]
    pub const fn new(
        publish_tx: Option<tokio::sync::mpsc::Sender<PublishRequest>>,
        cluster_mode: bool,
    ) -> Self {
        Self {
            publish_tx,
            cluster_mode,
        }
    }
}

#[async_trait]
impl ClusterFanoutService for DefaultClusterFanoutService {
    async fn reserve(
        &self,
        failure_message: &'static str,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        reserve_cluster_event_publish(self.publish_tx.as_ref(), self.cluster_mode, failure_message)
            .await
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
        match self.publish_tx.as_ref() {
            Some(tx) => try_publish_cluster_event(tx, request).await,
            None => false,
        }
    }

    fn is_distributed_enabled(&self) -> bool {
        self.cluster_mode && self.publish_tx.is_some()
    }
}

#[must_use]
pub fn default_cluster_fanout_service(
    publish_tx: Option<tokio::sync::mpsc::Sender<PublishRequest>>,
    cluster_mode: bool,
) -> Arc<dyn ClusterFanoutService> {
    Arc::new(DefaultClusterFanoutService::new(publish_tx, cluster_mode))
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
}
