use async_trait::async_trait;
use synctv_core::models::{RoomId, UserId};
use synctv_realtime::sync::{
    BroadcastResult, ConnectionId, RealtimeEvent, RealtimeManager, SharedRealtimeEvent,
};
use tokio::sync::{broadcast, mpsc};

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
        connection_id: ConnectionId,
    ) -> synctv_realtime::Result<(mpsc::Receiver<SharedRealtimeEvent>, ConnectionId)>;

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

    fn node_id(&self) -> &str;

    async fn shutdown(&self);
}

pub struct LocalNoopRealtimeEventService {
    admin_tx: broadcast::Sender<RealtimeEvent>,
}

impl LocalNoopRealtimeEventService {
    #[must_use]
    pub fn new() -> Self {
        let (admin_tx, _) = broadcast::channel(16);
        Self { admin_tx }
    }
}

impl Default for LocalNoopRealtimeEventService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RealtimeEventService for LocalNoopRealtimeEventService {
    async fn subscribe_with_id(
        &self,
        _room_id: RoomId,
        _user_id: UserId,
        connection_id: ConnectionId,
    ) -> synctv_realtime::Result<(mpsc::Receiver<SharedRealtimeEvent>, ConnectionId)> {
        let (_tx, rx) = mpsc::channel(16);
        Ok((rx, connection_id))
    }

    fn unsubscribe(&self, _connection_id: &str) {}

    fn broadcast(&self, _event: RealtimeEvent) -> BroadcastResult {
        BroadcastResult {
            local_sent: 0,
            redis_sent: false,
        }
    }

    fn publish_only(&self, _event: RealtimeEvent) -> bool {
        false
    }

    fn broadcast_local(&self, _room_id: &RoomId, _event: &RealtimeEvent) -> usize {
        0
    }

    fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        self.admin_tx.subscribe()
    }

    fn metrics(&self) -> RealtimeMetrics {
        RealtimeMetrics {
            distributed_enabled: false,
        }
    }

    fn node_id(&self) -> &'static str {
        "local-noop"
    }

    async fn shutdown(&self) {}
}

#[async_trait]
impl RealtimeEventService for RealtimeManager {
    async fn subscribe_with_id(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: ConnectionId,
    ) -> synctv_realtime::Result<(mpsc::Receiver<SharedRealtimeEvent>, ConnectionId)> {
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
        match RealtimeManager::admin_event_tx(self).send(event.clone()) {
            Ok(subscriber_count) => subscriber_count,
            Err(error) => {
                tracing::warn!(%error, "failed to broadcast admin realtime event");
                0
            }
        }
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
    use synctv_realtime::sync::ConnectionId;
    use synctv_realtime::sync::ConnectionRuntime;

    use super::{RealtimeDeliveryOutcome, RealtimeDeliveryRequirement, RealtimeEventService};
    use std::sync::Arc;
    use std::time::Duration;
    use synctv_core::models::{RoomId, UserId};
    use synctv_realtime::sync::{
        BroadcastResult, ConnectionLimits, ConnectionManager, RealtimeConfig, RealtimeEvent,
        RealtimeManager,
    };

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn realtime_ok<T>(result: synctv_realtime::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    fn string_ok<T>(result: Result<T, String>) -> TestResult<T> {
        result.map_err(test_error)
    }

    fn room_id() -> RoomId {
        RoomId::expect_positive(108_001)
    }

    fn user_id() -> UserId {
        UserId::expect_positive(108_002)
    }

    async fn local_realtime_manager(node_id: &str) -> TestResult<Arc<RealtimeManager>> {
        Ok(Arc::new(
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
            .map_err(|error| test_error(error.to_string()))?,
        ))
    }

    #[tokio::test]
    async fn test_realtime_manager_event_service_broadcasts_locally() -> TestResult {
        let realtime_manager = local_realtime_manager("runtime-node").await?;
        let event_service: Arc<dyn RealtimeEventService> = realtime_manager.clone();

        let mut room_rx = realtime_ok(
            event_service
                .subscribe_with_id(room_id(), user_id(), ConnectionId::new("conn-runtime"))
                .await,
        )?
        .0;

        let event = RealtimeEvent::ChatMessage {
            event_id: "evt-runtime".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "runtime-user".to_string(),
            message: "hello".to_string(),
            timestamp: synctv_core::SystemClock.now(),
            display_position: None,
            display_color: None,
        };

        assert_eq!(event_service.broadcast_local(&room_id(), &event), 1);
        assert!(matches!(
            room_rx.recv().await,
            Some(event) if matches!(event.as_ref(), RealtimeEvent::ChatMessage { .. })
        ));
        assert!(!event_service.metrics().distributed_enabled);

        realtime_manager.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_cluster_connection_runtime_exposes_connection_queries() -> TestResult {
        let presence_service = Arc::new(synctv_core::service::OnlinePresenceService::local());
        let connection_service: Arc<dyn ConnectionRuntime> = Arc::new(
            ConnectionManager::new(ConnectionLimits::default())
                .with_presence_service(presence_service.clone())
                .with_node_id("runtime-node"),
        );
        let room_id = room_id();
        let user_id = user_id();

        string_ok(
            connection_service
                .register("conn-runtime".to_string(), user_id)
                .await,
        )?;
        string_ok(connection_service.join_room("conn-runtime", room_id).await)?;

        assert_eq!(
            connection_service.get_connection_id(&room_id, &user_id),
            Some("conn-runtime".to_string())
        );
        assert_eq!(connection_service.connection_count(), 1);
        assert_eq!(connection_service.room_connection_count(&room_id), 1);
        assert_eq!(connection_service.user_connection_count(&user_id), 1);
        assert_eq!(
            presence_service
                .room_stats(room_id)
                .await
                .map_err(|error| test_error(error.to_string()))?
                .online_user_count,
            1
        );

        connection_service.shutdown().await;
        Ok(())
    }

    #[test]
    fn test_publish_only_delivery_requirement_allows_standalone_without_distributed_backend() {
        let metrics = super::RealtimeMetrics {
            distributed_enabled: false,
        };

        let outcome = RealtimeDeliveryOutcome::from_publish_only(false, metrics);

        assert!(outcome.satisfies(RealtimeDeliveryRequirement::DistributedIfAvailable));
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
        assert!(outcome.distributed_delivery_missed());
    }
}
