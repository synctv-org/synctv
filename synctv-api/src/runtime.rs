use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{BroadcastResult, ClusterEvent, ClusterManager, ConnectionId};
use synctv_core::models::{RoomId, UserId};
use tokio::sync::{broadcast, mpsc};

pub use synctv_cluster::sync::ConnectionRuntime as RealtimeConnectionService;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeMetrics {
    pub distributed_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeDeliveryRequirement {
    BestEffort,
    AnyAvailablePath,
    DistributedWhenAvailable,
    DistributedIfAvailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeDeliveryOutcome {
    local_delivered: bool,
    distributed_delivered: bool,
    distributed_available: bool,
}

impl RealtimeDeliveryOutcome {
    #[must_use]
    pub const fn from_broadcast(result: &BroadcastResult, metrics: RealtimeMetrics) -> Self {
        Self {
            local_delivered: result.local_sent > 0,
            distributed_delivered: result.redis_sent,
            distributed_available: metrics.distributed_enabled,
        }
    }

    #[must_use]
    pub const fn from_publish_only(distributed_delivered: bool, metrics: RealtimeMetrics) -> Self {
        Self {
            local_delivered: false,
            distributed_delivered,
            distributed_available: metrics.distributed_enabled,
        }
    }

    #[must_use]
    pub const fn local_delivered(self) -> bool {
        self.local_delivered
    }

    #[must_use]
    pub const fn distributed_available(self) -> bool {
        self.distributed_available
    }

    #[must_use]
    pub const fn distributed_delivered(self) -> bool {
        self.distributed_delivered
    }

    #[must_use]
    pub const fn delivered_to_any(self) -> bool {
        self.local_delivered || self.distributed_delivered
    }

    #[must_use]
    pub const fn distributed_delivery_missed(self) -> bool {
        self.distributed_available && !self.distributed_delivered
    }

    #[must_use]
    pub const fn satisfies(self, requirement: RealtimeDeliveryRequirement) -> bool {
        match requirement {
            RealtimeDeliveryRequirement::BestEffort => true,
            RealtimeDeliveryRequirement::AnyAvailablePath => self.delivered_to_any(),
            RealtimeDeliveryRequirement::DistributedWhenAvailable => {
                if self.distributed_available {
                    self.distributed_delivered
                } else {
                    self.delivered_to_any()
                }
            }
            RealtimeDeliveryRequirement::DistributedIfAvailable => {
                !self.distributed_available || self.distributed_delivered
            }
        }
    }
}

#[async_trait]
pub trait RealtimeEventService: Send + Sync {
    async fn subscribe_with_id(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: String,
    ) -> synctv_cluster::Result<(mpsc::Receiver<ClusterEvent>, ConnectionId)>;

    fn unsubscribe(&self, connection_id: &str);

    fn broadcast(&self, event: ClusterEvent) -> BroadcastResult;

    fn publish_only(&self, event: ClusterEvent) -> bool;

    fn broadcast_local(&self, room_id: &RoomId, event: &ClusterEvent) -> usize;

    fn subscribe_admin_events(&self) -> broadcast::Receiver<ClusterEvent>;

    fn metrics(&self) -> RealtimeMetrics;

    fn broadcast_outcome(&self, event: ClusterEvent) -> RealtimeDeliveryOutcome {
        let result = self.broadcast(event);
        RealtimeDeliveryOutcome::from_broadcast(&result, self.metrics())
    }

    fn publish_only_outcome(&self, event: ClusterEvent) -> RealtimeDeliveryOutcome {
        RealtimeDeliveryOutcome::from_publish_only(self.publish_only(event), self.metrics())
    }

    fn retry_broadcast_outcome(&self, event: ClusterEvent) -> RealtimeDeliveryOutcome {
        if self.metrics().distributed_enabled {
            self.publish_only_outcome(event)
        } else {
            self.broadcast_outcome(event)
        }
    }

    fn node_id(&self) -> &str;

    async fn shutdown(&self);
}

#[derive(Clone)]
pub struct ClusterRealtimeEventService {
    cluster_manager: Arc<ClusterManager>,
}

impl ClusterRealtimeEventService {
    #[must_use]
    pub const fn new(cluster_manager: Arc<ClusterManager>) -> Self {
        Self { cluster_manager }
    }
}

#[async_trait]
impl RealtimeEventService for ClusterManager {
    async fn subscribe_with_id(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: String,
    ) -> synctv_cluster::Result<(mpsc::Receiver<ClusterEvent>, ConnectionId)> {
        ClusterManager::subscribe_with_id(self, room_id, user_id, connection_id).await
    }

    fn unsubscribe(&self, connection_id: &str) {
        ClusterManager::unsubscribe(self, connection_id);
    }

    fn broadcast(&self, event: ClusterEvent) -> BroadcastResult {
        ClusterManager::broadcast(self, event)
    }

    fn publish_only(&self, event: ClusterEvent) -> bool {
        ClusterManager::publish_only(self, event)
    }

    fn broadcast_local(&self, room_id: &RoomId, event: &ClusterEvent) -> usize {
        ClusterManager::message_hub(self).broadcast(room_id, event)
    }

    fn subscribe_admin_events(&self) -> broadcast::Receiver<ClusterEvent> {
        ClusterManager::subscribe_admin_events(self)
    }

    fn metrics(&self) -> RealtimeMetrics {
        let metrics = ClusterManager::metrics(self);
        RealtimeMetrics {
            distributed_enabled: metrics.distributed_enabled,
        }
    }

    fn node_id(&self) -> &str {
        ClusterManager::node_id(self)
    }

    async fn shutdown(&self) {
        ClusterManager::shutdown(self).await;
    }
}

#[async_trait]
impl RealtimeEventService for ClusterRealtimeEventService {
    async fn subscribe_with_id(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: String,
    ) -> synctv_cluster::Result<(mpsc::Receiver<ClusterEvent>, ConnectionId)> {
        <ClusterManager as RealtimeEventService>::subscribe_with_id(
            self.cluster_manager.as_ref(),
            room_id,
            user_id,
            connection_id,
        )
        .await
    }

    fn unsubscribe(&self, connection_id: &str) {
        <ClusterManager as RealtimeEventService>::unsubscribe(
            self.cluster_manager.as_ref(),
            connection_id,
        );
    }

    fn broadcast(&self, event: ClusterEvent) -> BroadcastResult {
        <ClusterManager as RealtimeEventService>::broadcast(self.cluster_manager.as_ref(), event)
    }

    fn publish_only(&self, event: ClusterEvent) -> bool {
        <ClusterManager as RealtimeEventService>::publish_only(self.cluster_manager.as_ref(), event)
    }

    fn broadcast_local(&self, room_id: &RoomId, event: &ClusterEvent) -> usize {
        <ClusterManager as RealtimeEventService>::broadcast_local(
            self.cluster_manager.as_ref(),
            room_id,
            event,
        )
    }

    fn subscribe_admin_events(&self) -> broadcast::Receiver<ClusterEvent> {
        <ClusterManager as RealtimeEventService>::subscribe_admin_events(
            self.cluster_manager.as_ref(),
        )
    }

    fn metrics(&self) -> RealtimeMetrics {
        <ClusterManager as RealtimeEventService>::metrics(self.cluster_manager.as_ref())
    }

    fn node_id(&self) -> &str {
        <ClusterManager as RealtimeEventService>::node_id(self.cluster_manager.as_ref())
    }

    async fn shutdown(&self) {
        <ClusterManager as RealtimeEventService>::shutdown(self.cluster_manager.as_ref()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClusterRealtimeEventService, RealtimeConnectionService, RealtimeDeliveryOutcome,
        RealtimeDeliveryRequirement, RealtimeEventService,
    };
    use std::sync::Arc;
    use std::time::Duration;
    use synctv_cluster::sync::{
        BroadcastResult, ClusterConfig, ClusterEvent, ClusterManager, ConnectionLimits,
        ConnectionManager,
    };
    use synctv_core::models::{RoomId, UserId};

    fn room_id() -> RoomId {
        RoomId::from_string("room-runtime".to_string())
    }

    fn user_id() -> UserId {
        UserId::from_string("user-runtime".to_string())
    }

    async fn local_cluster_manager(node_id: &str) -> Arc<ClusterManager> {
        Arc::new(
            ClusterManager::new(
                ClusterConfig {
                    distributed_transport_factory: None,
                    message_runtime: Arc::new(synctv_cluster::sync::RoomMessageHub::new()),
                    cluster_enabled: false,
                    node_id: node_id.to_string(),
                    dedup_window: Duration::from_secs(30),
                    critical_channel_capacity: 16,
                    publish_channel_capacity: 16,
                    key_prefix: "runtime-test:".to_string(),
                    catchup_window_secs: 30,
                    stream_max_length: 128,
                    parent_cancel_token: None,
                },
                None,
                None,
            )
            .await
            .expect("local cluster manager should initialize"),
        )
    }

    #[tokio::test]
    async fn test_cluster_realtime_event_service_delegates_local_broadcasts() {
        let cluster_manager = local_cluster_manager("runtime-node").await;
        let event_service = ClusterRealtimeEventService::new(cluster_manager.clone());

        let mut room_rx = event_service
            .subscribe_with_id(room_id(), user_id(), "conn-runtime".to_string())
            .await
            .expect("room subscription should succeed")
            .0;

        let event = ClusterEvent::ChatMessage {
            event_id: "evt-runtime".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "runtime-user".to_string(),
            message: "hello".to_string(),
            timestamp: chrono::Utc::now(),
            position: None,
            color: None,
        };

        assert_eq!(event_service.broadcast_local(&room_id(), &event), 1);
        assert!(matches!(
            room_rx.recv().await,
            Some(ClusterEvent::ChatMessage { .. })
        ));
        assert!(!event_service.metrics().distributed_enabled);

        cluster_manager.shutdown().await;
    }

    #[tokio::test]
    async fn test_cluster_connection_runtime_exposes_connection_queries() {
        let connection_service: Arc<dyn RealtimeConnectionService> =
            Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        let room_id = room_id();
        let user_id = user_id();

        connection_service.start();
        connection_service
            .register("conn-runtime".to_string(), user_id.clone())
            .await
            .expect("connection registration should succeed");
        connection_service
            .join_room("conn-runtime", room_id.clone())
            .await
            .expect("room join should succeed");

        assert_eq!(
            connection_service.get_connection_id(&room_id, &user_id),
            Some("conn-runtime".to_string())
        );
        assert_eq!(connection_service.connection_count(), 1);
        assert_eq!(connection_service.room_connection_count(&room_id), 1);
        assert_eq!(connection_service.user_connection_count(&user_id), 1);
        assert_eq!(
            connection_service
                .room_online_user_count_distributed_batch(&[&room_id])
                .await
                .expect("online counts should succeed"),
            vec![1]
        );

        connection_service.shutdown().await;
    }

    #[test]
    fn test_publish_only_delivery_requirement_allows_standalone_without_distributed_backend() {
        let metrics = super::RealtimeMetrics {
            distributed_enabled: false,
        };

        let outcome = RealtimeDeliveryOutcome::from_publish_only(false, metrics);

        assert!(outcome.satisfies(RealtimeDeliveryRequirement::DistributedIfAvailable));
        assert!(!outcome.satisfies(RealtimeDeliveryRequirement::AnyAvailablePath));
    }

    #[test]
    fn test_broadcast_delivery_requirement_prefers_distributed_when_available() {
        let metrics = super::RealtimeMetrics {
            distributed_enabled: true,
        };
        let outcome = RealtimeDeliveryOutcome::from_broadcast(
            &BroadcastResult {
                local_sent: 3,
                redis_sent: false,
            },
            metrics,
        );

        assert!(!outcome.satisfies(RealtimeDeliveryRequirement::DistributedWhenAvailable));
        assert!(outcome.satisfies(RealtimeDeliveryRequirement::AnyAvailablePath));
        assert!(outcome.distributed_delivery_missed());
    }
}
