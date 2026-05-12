use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::connection_manager::{
    ConnectionInfo, ConnectionLimits, ConnectionManager, ConnectionMetrics, DisconnectSignal,
    ShutdownReport,
};
use super::events::RealtimeEvent;
use super::room_hub::{ConnectionId, RoomLifecycleEvent, RoomMessageHub};
use crate::error::{Error, Result};
use synctv_core::models::id::{RoomId, UserId};
use synctv_core::{SharedStateMode, SharedStateProfile};

pub fn build_connection_manager(
    limits: ConnectionLimits,
    profile: &SharedStateProfile,
) -> Result<ConnectionManager> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let shared_runtime = profile.shared_runtime().ok_or_else(|| {
                Error::Configuration(
                    "distributed runtime requires shared realtime connection state".to_string(),
                )
            })?;
            Ok(ConnectionManager::from_redis_runtime(
                limits,
                Some(shared_runtime),
                profile.key_prefix(),
            ))
        }
        SharedStateMode::SharedBestEffort | SharedStateMode::LocalOnly => Ok(
            ConnectionManager::from_redis_runtime(limits, None, profile.key_prefix()),
        ),
    }
}

pub fn build_connection_runtime(
    limits: ConnectionLimits,
    profile: &SharedStateProfile,
) -> Result<Arc<dyn ConnectionRuntime>> {
    Ok(Arc::new(build_connection_manager(limits, profile)?))
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
        SharedStateMode::SharedBestEffort | SharedStateMode::LocalOnly => {
            Ok(Arc::new(RoomMessageHub::new()))
        }
    }
}

#[async_trait]
pub trait RoomMessageRuntime: Send + Sync {
    fn subscribe_lifecycle(&self) -> broadcast::Receiver<RoomLifecycleEvent>;

    async fn subscribe(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: ConnectionId,
    ) -> Result<mpsc::Receiver<RealtimeEvent>>;

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

    fn active_room_ids(&self) -> Vec<RoomId>;

    fn connection_count(&self) -> usize;

    fn remove_room(&self, room_id: &RoomId);

    fn get_room_subscribers(&self, room_id: &RoomId) -> Vec<(UserId, ConnectionId)>;

    async fn get_room_subscribers_replicas_wide(
        &self,
        room_id: &RoomId,
    ) -> Vec<(UserId, ConnectionId)>;

    async fn audit_shared_subscriptions(&self) -> std::result::Result<usize, String>;

    fn spawn_shared_subscription_cleanup_task(
        &self,
        cleanup_interval: Duration,
        cancel_token: CancellationToken,
    ) -> JoinHandle<()>;

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
        user_id: UserId,
        actor_id: String,
    ) -> std::result::Result<(), String>;

    async fn join_room(
        &self,
        connection_id: &str,
        room_id: RoomId,
    ) -> std::result::Result<(), String>;

    async fn unregister(&self, connection_id: &str);

    fn reserve_room_slot(&self, room_id: &RoomId) -> std::result::Result<(), String>;

    fn reserve_user_slot(&self, user_id: &UserId) -> std::result::Result<(), String>;

    fn release_room_reservation(&self, room_id: &RoomId);

    fn release_user_reservation(&self, user_id: &UserId);

    fn subscribe_disconnect(&self) -> broadcast::Receiver<DisconnectSignal>;

    fn disconnect_connection(&self, connection_id: &str);

    fn disconnect_user(&self, user_id: &UserId);

    fn disconnect_room(&self, room_id: &RoomId);

    fn disconnect_user_from_room(&self, user_id: &UserId, room_id: &RoomId);

    fn get_connection(&self, connection_id: &str) -> Option<ConnectionInfo>;

    fn get_user_connections(&self, user_id: &UserId) -> Vec<ConnectionInfo>;

    fn get_room_connections(&self, room_id: &RoomId) -> Vec<ConnectionInfo>;

    fn get_connection_id(&self, room_id: &RoomId, user_id: &UserId) -> Option<String>;

    fn mark_rtc_joined(&self, room_id: &RoomId, user_id: &UserId, conn_id: &str, joined: bool);

    async fn has_other_connection_for_user_in_room_distributed(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        exclude_connection_id: &str,
    ) -> std::result::Result<bool, String>;

    async fn has_existing_presence_for_user_in_room_distributed(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        exclude_connection_id: &str,
    ) -> std::result::Result<bool, String>;

    async fn room_online_user_count_distributed(
        &self,
        room_id: &RoomId,
    ) -> std::result::Result<usize, String>;

    async fn room_online_user_count_distributed_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> std::result::Result<Vec<usize>, String>;

    fn connection_count(&self) -> usize;

    fn room_connection_count(&self, room_id: &RoomId) -> usize;

    fn user_connection_count(&self, user_id: &UserId) -> usize;

    fn start(&self);

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
        user_id: UserId,
        actor_id: String,
    ) -> std::result::Result<(), String> {
        ConnectionManager::register_actor(self, connection_id, user_id, actor_id).await
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

    fn reserve_room_slot(&self, room_id: &RoomId) -> std::result::Result<(), String> {
        ConnectionManager::reserve_room_slot(self, room_id)
    }

    fn reserve_user_slot(&self, user_id: &UserId) -> std::result::Result<(), String> {
        ConnectionManager::reserve_user_slot(self, user_id)
    }

    fn release_room_reservation(&self, room_id: &RoomId) {
        ConnectionManager::release_room_reservation(self, room_id);
    }

    fn release_user_reservation(&self, user_id: &UserId) {
        ConnectionManager::release_user_reservation(self, user_id);
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

    fn disconnect_room(&self, room_id: &RoomId) {
        ConnectionManager::disconnect_room(self, room_id);
    }

    fn disconnect_user_from_room(&self, user_id: &UserId, room_id: &RoomId) {
        ConnectionManager::disconnect_user_from_room(self, user_id, room_id);
    }

    fn get_connection(&self, connection_id: &str) -> Option<ConnectionInfo> {
        ConnectionManager::get_connection(self, connection_id)
    }

    fn get_user_connections(&self, user_id: &UserId) -> Vec<ConnectionInfo> {
        ConnectionManager::get_user_connections(self, user_id)
    }

    fn get_room_connections(&self, room_id: &RoomId) -> Vec<ConnectionInfo> {
        ConnectionManager::get_room_connections(self, room_id)
    }

    fn get_connection_id(&self, room_id: &RoomId, user_id: &UserId) -> Option<String> {
        ConnectionManager::get_connection_id(self, room_id, user_id)
    }

    fn mark_rtc_joined(&self, room_id: &RoomId, user_id: &UserId, conn_id: &str, joined: bool) {
        ConnectionManager::mark_rtc_joined(self, room_id, user_id, conn_id, joined);
    }

    async fn has_other_connection_for_user_in_room_distributed(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        exclude_connection_id: &str,
    ) -> std::result::Result<bool, String> {
        ConnectionManager::has_other_connection_for_user_in_room_distributed(
            self,
            user_id,
            room_id,
            exclude_connection_id,
        )
        .await
    }

    async fn has_existing_presence_for_user_in_room_distributed(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        exclude_connection_id: &str,
    ) -> std::result::Result<bool, String> {
        ConnectionManager::has_existing_presence_for_user_in_room_distributed(
            self,
            user_id,
            room_id,
            exclude_connection_id,
        )
        .await
    }

    async fn room_online_user_count_distributed(
        &self,
        room_id: &RoomId,
    ) -> std::result::Result<usize, String> {
        ConnectionManager::room_online_user_count_distributed(self, room_id).await
    }

    async fn room_online_user_count_distributed_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> std::result::Result<Vec<usize>, String> {
        ConnectionManager::room_online_user_count_distributed_batch(self, room_ids).await
    }

    fn connection_count(&self) -> usize {
        ConnectionManager::connection_count(self)
    }

    fn room_connection_count(&self, room_id: &RoomId) -> usize {
        ConnectionManager::room_connection_count(self, room_id)
    }

    fn user_connection_count(&self, user_id: &UserId) -> usize {
        ConnectionManager::user_connection_count(self, user_id)
    }

    fn start(&self) {
        ConnectionManager::start(self);
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

    use crate::sync::ConnectionLimits;
    use synctv_core::{SharedStateMode, SharedStateProfile};

    #[test]
    fn test_realtime_state_profile_keeps_local_mode_when_shared_state_not_required() {
        let profile = SharedStateProfile::from_runtime(None, "test:", false);

        assert_eq!(profile.state_mode(), SharedStateMode::LocalOnly);
    }

    #[test]
    fn test_build_connection_manager_requires_shared_runtime_when_cluster_state_is_required() {
        let profile = SharedStateProfile::from_runtime(None, "test:", true);
        let Err(error) = build_connection_manager(ConnectionLimits::default(), &profile) else {
            panic!("cluster realtime connection state must require a shared runtime");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared realtime connection state"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_build_connection_manager_stays_local_when_shared_state_is_not_required() {
        let profile = SharedStateProfile::from_runtime(None, "test:", false);

        let manager = build_connection_manager(ConnectionLimits::default(), &profile)
            .expect("standalone realtime runtime should stay local-only");

        manager.start();
        assert_eq!(manager.connection_count(), 0);
        manager.shutdown().await;
    }

    #[test]
    fn test_build_room_message_runtime_requires_shared_runtime_when_cluster_state_is_required() {
        let profile = SharedStateProfile::from_runtime(None, "test:", true);
        let Err(error) = build_room_message_runtime(&profile) else {
            panic!("cluster realtime message state must require a shared runtime");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared realtime message state"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_build_room_message_runtime_stays_local_when_shared_state_is_not_required() {
        let profile = SharedStateProfile::from_runtime(None, "test:", false);

        let runtime = build_room_message_runtime(&profile)
            .expect("standalone realtime message runtime should stay local");

        assert_eq!(runtime.room_count(), 0);
        runtime.shutdown().await;
    }
}
