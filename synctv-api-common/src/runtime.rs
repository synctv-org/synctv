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

#[cfg(test)]
mod tests {
    use synctv_realtime::fanout::{
        RealtimeDeliveryOutcome, RealtimeDeliveryRequirement, RealtimeEventService, RealtimeMetrics,
    };
    use synctv_realtime::sync::ConnectionId;
    use synctv_realtime::sync::ConnectionRuntime;

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
                .subscribe_with_id(
                    room_id(),
                    synctv_core::models::RealtimeActor::user(user_id(), "user-runtime"),
                    ConnectionId::new("conn-runtime"),
                )
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
        let presence = presence_service
            .room_stats(room_id)
            .await
            .map_err(|error| test_error(error.to_string()))?;
        assert_eq!(presence.online_member_count, 1);
        assert_eq!(presence.online_guest_count, 0);

        connection_service.shutdown().await;
        Ok(())
    }

    #[test]
    fn test_publish_only_delivery_requirement_allows_standalone_without_distributed_backend() {
        let metrics = RealtimeMetrics {
            distributed_enabled: false,
        };

        let outcome = RealtimeDeliveryOutcome::from_publish_only(false, metrics);

        assert!(outcome.satisfies(RealtimeDeliveryRequirement::DistributedIfAvailable));
    }

    #[test]
    fn test_broadcast_delivery_requirement_prefers_distributed_when_available() {
        let metrics = RealtimeMetrics {
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
