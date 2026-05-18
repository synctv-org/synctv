use async_trait::async_trait;
use synctv_core::models::{RoomId, UserId};
use synctv_realtime::sync::{BroadcastResult, ConnectionId, RealtimeEvent, RealtimeManager};
use tokio::sync::{broadcast, mpsc};

pub use synctv_realtime::sync::ConnectionRuntime as RealtimeConnectionService;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeAdmissionError {
    Capacity(String),
    ClusterUnavailable(String),
    Internal(String),
}

impl RealtimeAdmissionError {
    #[must_use]
    pub fn from_runtime_message(message: String) -> Self {
        let lower = message.to_ascii_lowercase();
        if lower.contains("distributed room capacity check unavailable")
            || lower.contains("distributed user connection check unavailable")
            || lower.contains("distributed total connection check unavailable")
        {
            return Self::ClusterUnavailable(message);
        }

        if lower.contains("room at capacity")
            || lower.contains("user at capacity")
            || lower.contains("server at capacity")
            || lower.contains("too many connections for this user")
        {
            return Self::Capacity(message);
        }

        Self::Internal(message)
    }
}

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
    ) -> synctv_realtime::Result<(mpsc::Receiver<RealtimeEvent>, ConnectionId)>;

    fn unsubscribe(&self, connection_id: &str);

    fn broadcast(&self, event: RealtimeEvent) -> BroadcastResult;

    fn publish_only(&self, event: RealtimeEvent) -> bool;

    fn broadcast_local(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize;

    fn broadcast_admin_local(&self, _event: &RealtimeEvent) -> usize {
        0
    }

    fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent>;

    fn subscribe_lifecycle_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        self.subscribe_admin_events()
    }

    fn metrics(&self) -> RealtimeMetrics;

    fn broadcast_outcome(&self, event: RealtimeEvent) -> RealtimeDeliveryOutcome {
        let result = self.broadcast(event);
        RealtimeDeliveryOutcome::from_broadcast(&result, self.metrics())
    }

    fn publish_only_outcome(&self, event: RealtimeEvent) -> RealtimeDeliveryOutcome {
        RealtimeDeliveryOutcome::from_publish_only(self.publish_only(event), self.metrics())
    }

    fn retry_broadcast_outcome(&self, event: RealtimeEvent) -> RealtimeDeliveryOutcome {
        if self.metrics().distributed_enabled {
            self.publish_only_outcome(event)
        } else {
            self.broadcast_outcome(event)
        }
    }

    fn node_id(&self) -> &str;

    async fn shutdown(&self);
}

#[async_trait]
impl RealtimeEventService for RealtimeManager {
    async fn subscribe_with_id(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: String,
    ) -> synctv_realtime::Result<(mpsc::Receiver<RealtimeEvent>, ConnectionId)> {
        RealtimeManager::subscribe_with_id(self, room_id, user_id, connection_id).await
    }

    fn unsubscribe(&self, connection_id: &str) {
        RealtimeManager::unsubscribe(self, connection_id);
    }

    fn broadcast(&self, event: RealtimeEvent) -> BroadcastResult {
        RealtimeManager::broadcast(self, event)
    }

    fn publish_only(&self, event: RealtimeEvent) -> bool {
        RealtimeManager::publish_only(self, event)
    }

    fn broadcast_local(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize {
        RealtimeManager::message_hub(self).broadcast(room_id, event)
    }

    fn broadcast_admin_local(&self, event: &RealtimeEvent) -> usize {
        RealtimeManager::admin_event_tx(self)
            .send(event.clone())
            .unwrap_or_default()
    }

    fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        RealtimeManager::subscribe_admin_events(self)
    }

    fn subscribe_lifecycle_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        RealtimeManager::subscribe_lifecycle_events(self)
    }

    fn metrics(&self) -> RealtimeMetrics {
        let metrics = RealtimeManager::metrics(self);
        RealtimeMetrics {
            distributed_enabled: metrics.distributed_enabled,
        }
    }

    fn node_id(&self) -> &str {
        RealtimeManager::node_id(self)
    }

    async fn shutdown(&self) {
        RealtimeManager::shutdown(self).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RealtimeConnectionService, RealtimeDeliveryOutcome, RealtimeDeliveryRequirement,
        RealtimeEventService,
    };
    use std::sync::Arc;
    use std::time::Duration;
    use synctv_core::models::{RoomId, UserId};
    use synctv_realtime::sync::{
        BroadcastResult, ConnectionLimits, ConnectionManager, RealtimeConfig, RealtimeEvent,
        RealtimeManager,
    };

    fn room_id() -> RoomId {
        RoomId::expect_positive(108_001)
    }

    fn user_id() -> UserId {
        UserId::expect_positive(108_002)
    }

    async fn local_realtime_manager(node_id: &str) -> Arc<RealtimeManager> {
        Arc::new(
            RealtimeManager::new(RealtimeConfig {
                distributed_transport_factory: None,
                message_runtime: Arc::new(synctv_realtime::sync::RoomMessageHub::new()),
                distributed_enabled: false,
                node_id: node_id.to_string(),
                dedup_window: Duration::from_secs(30),
                critical_channel_capacity: 16,
                publish_channel_capacity: 16,
                key_prefix: "runtime-test:".to_string(),
                catchup_window_secs: 30,
                stream_max_length: 128,
                event_handler: None,
                parent_cancel_token: None,
            })
            .await
            .expect("local realtime manager should initialize"),
        )
    }

    #[tokio::test]
    async fn test_realtime_manager_event_service_broadcasts_locally() {
        let realtime_manager = local_realtime_manager("runtime-node").await;
        let event_service: Arc<dyn RealtimeEventService> = realtime_manager.clone();

        let mut room_rx = event_service
            .subscribe_with_id(room_id(), user_id(), "conn-runtime".to_string())
            .await
            .expect("room subscription should succeed")
            .0;

        let event = RealtimeEvent::ChatMessage {
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
            Some(RealtimeEvent::ChatMessage { .. })
        ));
        assert!(!event_service.metrics().distributed_enabled);

        realtime_manager.shutdown().await;
    }

    #[tokio::test]
    async fn test_cluster_connection_runtime_exposes_connection_queries() {
        let connection_service: Arc<dyn RealtimeConnectionService> =
            Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        let room_id = room_id();
        let user_id = user_id();

        connection_service.start();
        connection_service
            .register("conn-runtime".to_string(), user_id)
            .await
            .expect("connection registration should succeed");
        connection_service
            .join_room("conn-runtime", room_id)
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
