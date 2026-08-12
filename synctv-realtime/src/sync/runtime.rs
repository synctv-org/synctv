use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::connection_manager::{
    ConnectionInfo, ConnectionLimits, ConnectionManager, ConnectionMetrics,
    ConnectionReservationError, DisconnectSignal, RoomDisconnectReason, ShutdownReport,
    VoiceRtcJoinOutcome,
};
use super::room_hub::{ConnectionId, RoomLifecycleEvent, RoomMessageHub};
use super::{RealtimeEvent, SharedRealtimeEvent};
use crate::error::{Error, Result};
use synctv_core::models::{RealtimeActor, RoomId, UserId};
use synctv_core::{service::OnlinePresenceService, SharedStateMode, SharedStateProfile};

pub fn build_connection_manager(
    limits: ConnectionLimits,
    profile: &SharedStateProfile,
    presence_service: Arc<OnlinePresenceService>,
    node_id: impl Into<Arc<str>>,
) -> Result<ConnectionManager> {
    let manager = match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let shared_runtime = profile.shared_runtime().ok_or_else(|| {
                Error::Configuration(
                    "distributed runtime requires shared realtime connection state".to_string(),
                )
            })?;
            ConnectionManager::from_redis_runtime(
                limits,
                Some(shared_runtime),
                profile.key_prefix(),
            )
        }
        SharedStateMode::SharedBestEffort => ConnectionManager::from_redis_runtime(
            limits,
            profile.shared_runtime(),
            profile.key_prefix(),
        ),
        SharedStateMode::LocalOnly => {
            ConnectionManager::from_redis_runtime(limits, None, profile.key_prefix())
        }
    };
    Ok(manager
        .with_presence_service(presence_service)
        .with_node_id(node_id))
}

pub fn build_connection_runtime(
    limits: ConnectionLimits,
    profile: &SharedStateProfile,
    presence_service: Arc<OnlinePresenceService>,
    node_id: impl Into<Arc<str>>,
) -> Result<Arc<dyn ConnectionRuntime>> {
    Ok(Arc::new(build_connection_manager(
        limits,
        profile,
        presence_service,
        node_id,
    )?))
}

pub fn build_room_message_runtime(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn RoomMessageRuntime>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let shared_runtime = profile.shared_runtime().ok_or_else(|| {
                Error::Configuration(
                    "distributed runtime requires shared realtime message state".to_string(),
                )
            })?;
            Ok(Arc::new(RoomMessageHub::from_redis_runtime(
                Some(shared_runtime),
                profile.key_prefix(),
            )))
        }
        SharedStateMode::SharedBestEffort => Ok(Arc::new(RoomMessageHub::from_redis_runtime(
            profile.shared_runtime(),
            profile.key_prefix(),
        ))),
        SharedStateMode::LocalOnly => Ok(Arc::new(RoomMessageHub::new())),
    }
}

#[async_trait]
pub trait RoomMessageRuntime: Send + Sync {
    fn subscribe_lifecycle(&self) -> broadcast::Receiver<RoomLifecycleEvent>;

    async fn subscribe(
        &self,
        room_id: RoomId,
        actor: RealtimeActor,
        connection_id: ConnectionId,
    ) -> Result<mpsc::Receiver<SharedRealtimeEvent>>;

    fn unsubscribe(&self, connection_id: &str);

    fn broadcast(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize;

    async fn broadcast_reliably(&self, room_id: &RoomId, event: RealtimeEvent) -> usize;

    async fn broadcast_to_connection(
        &self,
        room_id: &RoomId,
        connection_id: &str,
        event: RealtimeEvent,
    ) -> usize;

    fn room_count(&self) -> usize;

    /// Rooms with at least one realtime connection on this process.
    ///
    /// This is local-node state for lifecycle work such as playback background
    /// workers. Distributed room statistics live in presence/Redis APIs.
    fn active_room_ids(&self) -> Vec<RoomId>;

    fn connection_count(&self) -> usize;

    fn remove_room(&self, room_id: &RoomId);

    fn get_room_subscribers(&self, room_id: &RoomId) -> Vec<(RealtimeActor, ConnectionId)>;

    async fn get_room_subscribers_replicas_wide(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<(RealtimeActor, ConnectionId)>>;

    async fn audit_shared_subscriptions(&self) -> std::result::Result<usize, String>;

    async fn shutdown(&self);

    #[cfg(test)]
    fn background_shutdown_requested(&self) -> bool;
}

#[async_trait]
pub trait ConnectionRuntime: Send + Sync {
    async fn register(
        &self,
        connection_id: String,
        user_id: UserId,
    ) -> std::result::Result<(), String>;

    async fn register_actor(
        &self,
        connection_id: String,
        actor: RealtimeActor,
    ) -> std::result::Result<(), String>;

    async fn join_room(
        &self,
        connection_id: &str,
        room_id: RoomId,
    ) -> std::result::Result<(), String>;

    async fn unregister(&self, connection_id: &str);

    fn record_message(&self, connection_id: &str);

    fn reserve_room_slot(
        &self,
        room_id: &RoomId,
    ) -> std::result::Result<(), ConnectionReservationError>;

    fn reserve_user_slot(
        &self,
        user_id: &UserId,
    ) -> std::result::Result<(), ConnectionReservationError>;

    fn reserve_actor_slot(
        &self,
        actor: &RealtimeActor,
    ) -> std::result::Result<(), ConnectionReservationError>;

    fn release_room_reservation(&self, room_id: &RoomId);

    fn release_user_reservation(&self, user_id: &UserId);

    fn release_actor_reservation(&self, actor: &RealtimeActor);

    fn subscribe_disconnect(&self) -> broadcast::Receiver<DisconnectSignal>;

    fn disconnect_connection(&self, connection_id: &str);

    fn disconnect_user(&self, user_id: &UserId);

    fn disconnect_room(&self, room_id: &RoomId, reason: RoomDisconnectReason);

    fn disconnect_user_from_room(&self, user_id: &UserId, room_id: &RoomId);

    fn get_connection(&self, connection_id: &str) -> Option<ConnectionInfo>;

    async fn get_connection_distributed(
        &self,
        connection_id: &str,
    ) -> std::result::Result<Option<ConnectionInfo>, String>;

    fn get_user_connections(&self, user_id: &UserId) -> Vec<ConnectionInfo>;

    fn get_actor_connections(&self, actor: &RealtimeActor) -> Vec<ConnectionInfo>;

    fn get_room_connections(&self, room_id: &RoomId) -> Vec<ConnectionInfo>;

    fn get_connection_id(&self, room_id: &RoomId, user_id: &UserId) -> Option<String>;

    async fn try_join_voice_rtc(
        &self,
        room_id: &RoomId,
        actor: &RealtimeActor,
        conn_id: &str,
        max_participants: usize,
    ) -> std::result::Result<VoiceRtcJoinOutcome, String>;

    async fn leave_voice_rtc(
        &self,
        room_id: &RoomId,
        actor: &RealtimeActor,
        conn_id: &str,
    ) -> std::result::Result<bool, String>;

    fn mark_voice_rtc_joined(
        &self,
        room_id: &RoomId,
        actor: &RealtimeActor,
        conn_id: &str,
        joined: bool,
    );

    async fn sync_connection_metadata_distributed(
        &self,
        connection_id: &str,
    ) -> std::result::Result<(), String>;

    fn connection_count(&self) -> usize;

    fn room_connection_count(&self, room_id: &RoomId) -> usize;

    /// Rooms with at least one connection owned by this process.
    ///
    /// This is the scheduling boundary for per-node playback lifecycle
    /// workers: duration probing, auto-advance, RTMP, and live-proxy resource
    /// maintenance. Presence, hot-room indexes, and distributed room counters
    /// are read models for lists, admin views, analytics, and metrics. Workers
    /// run on every node and converge duplicate room attempts through database
    /// locks, `SKIP LOCKED`, and playback-state optimistic versions.
    fn active_room_ids(&self) -> Vec<RoomId>;

    fn user_connection_count(&self, user_id: &UserId) -> usize;

    fn spawn_cleanup_task(
        &self,
        interval: Duration,
        cancel_token: CancellationToken,
    ) -> JoinHandle<()>;

    async fn shutdown(&self) -> ShutdownReport;

    fn metrics(&self) -> ConnectionMetrics;

    fn abort_background_tasks(&self);
}

#[async_trait]
impl ConnectionRuntime for ConnectionManager {
    async fn register(
        &self,
        connection_id: String,
        user_id: UserId,
    ) -> std::result::Result<(), String> {
        ConnectionManager::register(self, connection_id, user_id).await
    }

    async fn register_actor(
        &self,
        connection_id: String,
        actor: RealtimeActor,
    ) -> std::result::Result<(), String> {
        ConnectionManager::register_actor(self, connection_id, actor).await
    }

    async fn join_room(
        &self,
        connection_id: &str,
        room_id: RoomId,
    ) -> std::result::Result<(), String> {
        ConnectionManager::join_room(self, connection_id, room_id).await
    }

    async fn unregister(&self, connection_id: &str) {
        ConnectionManager::unregister(self, connection_id).await;
    }

    fn record_message(&self, connection_id: &str) {
        ConnectionManager::record_message(self, connection_id);
    }

    fn reserve_room_slot(
        &self,
        room_id: &RoomId,
    ) -> std::result::Result<(), ConnectionReservationError> {
        ConnectionManager::reserve_room_slot(self, room_id)
    }

    fn reserve_user_slot(
        &self,
        user_id: &UserId,
    ) -> std::result::Result<(), ConnectionReservationError> {
        ConnectionManager::reserve_user_slot(self, user_id)
    }

    fn reserve_actor_slot(
        &self,
        actor: &RealtimeActor,
    ) -> std::result::Result<(), ConnectionReservationError> {
        ConnectionManager::reserve_actor_slot(self, actor)
    }

    fn release_room_reservation(&self, room_id: &RoomId) {
        ConnectionManager::release_room_reservation(self, room_id);
    }

    fn release_user_reservation(&self, user_id: &UserId) {
        ConnectionManager::release_user_reservation(self, user_id);
    }

    fn release_actor_reservation(&self, actor: &RealtimeActor) {
        ConnectionManager::release_actor_reservation(self, actor);
    }

    fn subscribe_disconnect(&self) -> broadcast::Receiver<DisconnectSignal> {
        ConnectionManager::subscribe_disconnect(self)
    }

    fn disconnect_connection(&self, connection_id: &str) {
        ConnectionManager::disconnect_connection(self, connection_id);
    }

    fn disconnect_user(&self, user_id: &UserId) {
        ConnectionManager::disconnect_user(self, user_id);
    }

    fn disconnect_room(&self, room_id: &RoomId, reason: RoomDisconnectReason) {
        ConnectionManager::disconnect_room(self, room_id, reason);
    }

    fn disconnect_user_from_room(&self, user_id: &UserId, room_id: &RoomId) {
        ConnectionManager::disconnect_user_from_room(self, user_id, room_id);
    }

    fn get_connection(&self, connection_id: &str) -> Option<ConnectionInfo> {
        ConnectionManager::get_connection(self, connection_id)
    }

    async fn get_connection_distributed(
        &self,
        connection_id: &str,
    ) -> std::result::Result<Option<ConnectionInfo>, String> {
        ConnectionManager::get_connection_distributed(self, connection_id).await
    }

    fn get_user_connections(&self, user_id: &UserId) -> Vec<ConnectionInfo> {
        ConnectionManager::get_user_connections(self, user_id)
    }

    fn get_actor_connections(&self, actor: &RealtimeActor) -> Vec<ConnectionInfo> {
        ConnectionManager::get_actor_connections(self, actor)
    }

    fn get_room_connections(&self, room_id: &RoomId) -> Vec<ConnectionInfo> {
        ConnectionManager::get_room_connections(self, room_id)
    }

    fn get_connection_id(&self, room_id: &RoomId, user_id: &UserId) -> Option<String> {
        ConnectionManager::get_connection_id(self, room_id, user_id)
    }

    async fn try_join_voice_rtc(
        &self,
        room_id: &RoomId,
        actor: &RealtimeActor,
        conn_id: &str,
        max_participants: usize,
    ) -> std::result::Result<VoiceRtcJoinOutcome, String> {
        ConnectionManager::try_join_voice_rtc(self, room_id, actor, conn_id, max_participants).await
    }

    async fn leave_voice_rtc(
        &self,
        room_id: &RoomId,
        actor: &RealtimeActor,
        conn_id: &str,
    ) -> std::result::Result<bool, String> {
        ConnectionManager::leave_voice_rtc(self, room_id, actor, conn_id).await
    }

    fn mark_voice_rtc_joined(
        &self,
        room_id: &RoomId,
        actor: &RealtimeActor,
        conn_id: &str,
        joined: bool,
    ) {
        ConnectionManager::mark_voice_rtc_joined(self, room_id, actor, conn_id, joined);
    }

    async fn sync_connection_metadata_distributed(
        &self,
        connection_id: &str,
    ) -> std::result::Result<(), String> {
        ConnectionManager::sync_connection_metadata_distributed(self, connection_id).await
    }

    fn connection_count(&self) -> usize {
        ConnectionManager::connection_count(self)
    }

    fn room_connection_count(&self, room_id: &RoomId) -> usize {
        ConnectionManager::room_connection_count(self, room_id)
    }

    fn active_room_ids(&self) -> Vec<RoomId> {
        ConnectionManager::active_room_ids(self)
    }

    fn user_connection_count(&self, user_id: &UserId) -> usize {
        ConnectionManager::user_connection_count(self, user_id)
    }

    fn spawn_cleanup_task(
        &self,
        interval: Duration,
        cancel_token: CancellationToken,
    ) -> JoinHandle<()> {
        ConnectionManager::spawn_cleanup_task(self, interval, cancel_token)
    }

    async fn shutdown(&self) -> ShutdownReport {
        ConnectionManager::shutdown(self).await
    }

    fn metrics(&self) -> ConnectionMetrics {
        ConnectionManager::metrics(self)
    }

    fn abort_background_tasks(&self) {
        ConnectionManager::abort_background_tasks(self);
    }
}

#[cfg(test)]
mod tests {
    use super::{build_connection_manager, build_room_message_runtime};

    use crate::{sync::ConnectionLimits, Error};
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::time::Duration;
    use synctv_core::models::id::RoomId;
    use synctv_core::service::OnlinePresenceService;
    use synctv_core::{RedisConnectionRuntime, SharedStateMode, SharedStateProfile};

    struct HangingRedisRuntime;

    #[async_trait]
    impl RedisConnectionRuntime for HangingRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            std::future::pending().await
        }

        fn operation_timeout(&self) -> Duration {
            Duration::from_millis(10)
        }
    }

    fn hanging_runtime() -> Arc<dyn RedisConnectionRuntime> {
        Arc::new(HangingRedisRuntime)
    }

    fn local_presence() -> Arc<OnlinePresenceService> {
        Arc::new(OnlinePresenceService::local())
    }

    fn require_error<T>(result: crate::Result<T>, message: &'static str) -> crate::Result<Error> {
        match result {
            Ok(_) => Err(Error::Internal(anyhow::anyhow!(message))),
            Err(error) => Ok(error),
        }
    }

    #[test]
    fn test_realtime_state_profile_keeps_local_mode_when_shared_state_not_required() {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", false);

        assert_eq!(profile.state_mode(), SharedStateMode::LocalOnly);
    }

    #[test]
    fn test_build_connection_manager_requires_shared_runtime_when_cluster_state_is_required(
    ) -> crate::Result<()> {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);
        let error = require_error(
            build_connection_manager(
                ConnectionLimits::default(),
                &profile,
                local_presence(),
                "test-node",
            ),
            "cluster realtime connection state must require a shared runtime",
        )?;

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared realtime connection state"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_build_connection_manager_stays_local_when_shared_state_is_not_required(
    ) -> crate::Result<()> {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", false);

        let manager = build_connection_manager(
            ConnectionLimits::default(),
            &profile,
            local_presence(),
            "test-node",
        )?;

        assert_eq!(manager.connection_count(), 0);
        manager.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_build_connection_manager_uses_shared_runtime_when_best_effort_has_one(
    ) -> crate::Result<()> {
        let profile =
            SharedStateProfile::for_cluster_runtime(Some(hanging_runtime()), "test:", false);

        let manager = build_connection_manager(
            ConnectionLimits::default(),
            &profile,
            local_presence(),
            "test-node",
        )?;

        let error = manager
            .connection_count_distributed()
            .await
            .expect_err("best-effort runtime with Redis configured must not behave local-only");
        assert!(
            error.contains("Distributed total connection count unavailable"),
            "unexpected error: {error}"
        );
        manager.shutdown().await;
        Ok(())
    }

    #[test]
    fn test_build_room_message_runtime_requires_shared_runtime_when_cluster_state_is_required(
    ) -> crate::Result<()> {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);
        let error = require_error(
            build_room_message_runtime(&profile),
            "cluster realtime message state must require a shared runtime",
        )?;

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared realtime message state"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_build_room_message_runtime_stays_local_when_shared_state_is_not_required(
    ) -> crate::Result<()> {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", false);

        let runtime = build_room_message_runtime(&profile)?;

        assert_eq!(runtime.room_count(), 0);
        runtime.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_build_room_message_runtime_uses_shared_runtime_when_best_effort_has_one(
    ) -> crate::Result<()> {
        let profile =
            SharedStateProfile::for_cluster_runtime(Some(hanging_runtime()), "test:", false);

        let runtime = build_room_message_runtime(&profile)?;

        let error = runtime
            .get_room_subscribers_replicas_wide(&RoomId::expect_positive(10_000_401))
            .await
            .expect_err("best-effort message runtime with Redis configured must not be local-only");
        assert!(
            error.to_string().contains("Redis"),
            "unexpected error: {error}"
        );
        runtime.shutdown().await;
        Ok(())
    }
}
