use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::models::{PermissionBits, RoomId, UserId};
use synctv_core::service::{RoomService, UserService};

use crate::cluster_fanout::{publish_best_effort, ClusterFanoutService};
use crate::impls::admin::LOCAL_MANAGEMENT_ACTOR_USER_ID;
use crate::impls::ApiError;
use crate::runtime::RealtimeEventService;

#[async_trait]
pub trait MembershipEventFanoutService: Send + Sync {
    async fn publish_permission_changed(
        &self,
        room_id: &RoomId,
        target_user_id: &UserId,
        changed_by: &UserId,
    ) -> Result<(), ApiError>;

    async fn publish_user_left(&self, room_id: &RoomId, user_id: &UserId) -> Result<(), ApiError>;
}

pub struct DefaultMembershipEventFanoutService {
    cluster_fanout: Arc<dyn ClusterFanoutService>,
    room_service: Arc<RoomService>,
    user_service: Arc<UserService>,
    event_service: Option<Arc<dyn RealtimeEventService>>,
}

impl DefaultMembershipEventFanoutService {
    #[must_use]
    pub fn new(
        cluster_fanout: Arc<dyn ClusterFanoutService>,
        room_service: Arc<RoomService>,
        user_service: Arc<UserService>,
        event_service: Option<Arc<dyn RealtimeEventService>>,
    ) -> Self {
        Self {
            cluster_fanout,
            room_service,
            user_service,
            event_service,
        }
    }

    fn should_broadcast_permission_changed_locally(
        &self,
        target_user_id: &UserId,
        changed_by: &UserId,
    ) -> bool {
        !self.cluster_fanout.is_distributed_enabled() && target_user_id == changed_by
    }

    async fn username_for_actor(&self, user_id: &UserId) -> Result<String, ApiError> {
        if *user_id == LOCAL_MANAGEMENT_ACTOR_USER_ID {
            return Ok("local-management".to_string());
        }

        self.user_service
            .get_user(user_id)
            .await
            .map(|u| u.username)
            .map_err(ApiError::from)
    }
}

impl std::fmt::Debug for DefaultMembershipEventFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultMembershipEventFanoutService")
            .field(
                "cluster_fanout_distributed",
                &self.cluster_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl MembershipEventFanoutService for DefaultMembershipEventFanoutService {
    async fn publish_permission_changed(
        &self,
        room_id: &RoomId,
        target_user_id: &UserId,
        changed_by: &UserId,
    ) -> Result<(), ApiError> {
        let (
            target_username,
            new_permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        ) = if let Some(member) = self
            .room_service
            .member_service()
            .get_member(room_id, target_user_id)
            .await
            .map_err(ApiError::from)?
        {
            let room_settings = self
                .room_service
                .get_room_settings(room_id)
                .await
                .map_err(ApiError::from)?;
            let username = self.username_for_actor(target_user_id).await?;
            let role_default = self
                .room_service
                .permission_service()
                .calculate_role_default_permissions(&member.role, &room_settings);
            (
                username,
                member.effective_permissions(role_default),
                i32::from(member.role),
                member.added_permissions,
                member.removed_permissions,
                member.admin_added_permissions,
                member.admin_removed_permissions,
            )
        } else {
            let username = self.username_for_actor(target_user_id).await?;
            (
                username,
                PermissionBits::empty(),
                synctv_proto::common::RoomMemberRole::Member as i32,
                0,
                0,
                0,
                0,
            )
        };

        let changed_by_username = self.username_for_actor(changed_by).await?;

        let request = PublishRequest {
            event: ClusterEvent::PermissionChanged {
                event_id: synctv_common::snanoid!(16),
                room_id: *room_id,
                target_user_id: *target_user_id,
                target_username,
                changed_by: *changed_by,
                changed_by_username,
                new_permissions,
                role,
                added_permissions: PermissionBits(added_permissions),
                removed_permissions: PermissionBits(removed_permissions),
                admin_added_permissions: PermissionBits(admin_added_permissions),
                admin_removed_permissions: PermissionBits(admin_removed_permissions),
                timestamp: chrono::Utc::now(),
            },
        };
        if self.should_broadcast_permission_changed_locally(target_user_id, changed_by) {
            if let Some(event_service) = &self.event_service {
                event_service.broadcast_local(room_id, &request.event);
            }
        }
        publish_best_effort(self.cluster_fanout.clone(), request);
        Ok(())
    }

    async fn publish_user_left(&self, room_id: &RoomId, user_id: &UserId) -> Result<(), ApiError> {
        let username = self
            .user_service
            .get_user(user_id)
            .await
            .map(|u| u.username)
            .map_err(ApiError::from)?;

        let request = PublishRequest {
            event: ClusterEvent::UserLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: *room_id,
                user_id: *user_id,
                username,
                timestamp: chrono::Utc::now(),
            },
        };
        publish_best_effort(self.cluster_fanout.clone(), request);

        Ok(())
    }
}

#[must_use]
pub fn default_membership_event_fanout_service(
    cluster_fanout: Arc<dyn ClusterFanoutService>,
    room_service: Arc<RoomService>,
    user_service: Arc<UserService>,
    event_service: Option<Arc<dyn RealtimeEventService>>,
) -> Arc<dyn MembershipEventFanoutService> {
    Arc::new(DefaultMembershipEventFanoutService::new(
        cluster_fanout,
        room_service,
        user_service,
        event_service,
    ))
}

#[cfg(test)]
mod tests {
    use super::default_membership_event_fanout_service;
    use crate::cluster_fanout::default_cluster_fanout_service;
    use crate::runtime::{RealtimeEventService, RealtimeMetrics};
    use crate::test_support::channel_cluster_fanout_service;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use synctv_cluster::sync::{BroadcastResult, ClusterEvent, ConnectionId};
    use synctv_core::cache::UsernameCache;
    use synctv_core::models::{RoomId, UserId};
    use synctv_core::service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, RoomService, UserService,
    };
    use synctv_core::KeyBuilder;
    use tokio::sync::{broadcast, mpsc};

    #[derive(Default)]
    struct RecordingRealtimeEventService {
        broadcast_calls: AtomicUsize,
        broadcast_local_calls: AtomicUsize,
        local_events: Mutex<Vec<(String, ClusterEvent)>>,
    }

    #[async_trait]
    impl RealtimeEventService for RecordingRealtimeEventService {
        async fn subscribe_with_id(
            &self,
            _room_id: RoomId,
            _user_id: UserId,
            _connection_id: String,
        ) -> synctv_cluster::Result<(mpsc::Receiver<ClusterEvent>, ConnectionId)> {
            panic!("subscribe_with_id should not be called in membership fanout tests");
        }

        fn unsubscribe(&self, _connection_id: &str) {
            panic!("unsubscribe should not be called in membership fanout tests");
        }

        fn broadcast(&self, _event: ClusterEvent) -> BroadcastResult {
            self.broadcast_calls.fetch_add(1, Ordering::SeqCst);
            BroadcastResult {
                local_sent: 0,
                redis_sent: false,
            }
        }

        fn publish_only(&self, _event: ClusterEvent) -> bool {
            panic!("publish_only should not be called in membership fanout tests");
        }

        fn broadcast_local(&self, room_id: &RoomId, event: &ClusterEvent) -> usize {
            self.broadcast_local_calls.fetch_add(1, Ordering::SeqCst);
            self.local_events
                .lock()
                .expect("recorded local events mutex should not be poisoned")
                .push((room_id.to_string(), event.clone()));
            1
        }

        fn subscribe_admin_events(&self) -> broadcast::Receiver<ClusterEvent> {
            panic!("subscribe_admin_events should not be called in membership fanout tests");
        }

        fn metrics(&self) -> RealtimeMetrics {
            RealtimeMetrics {
                distributed_enabled: false,
            }
        }

        fn node_id(&self) -> &'static str {
            "membership-fanout-test-node"
        }

        async fn shutdown(&self) {}
    }

    fn room_id() -> RoomId {
        RoomId::expect_positive(102_001)
    }

    fn user_id(value: &str) -> UserId {
        let id = match value {
            "target" => 102_002,
            "actor" => 102_003,
            "self-joiner" => 102_004,
            _ => 102_099,
        };
        UserId::expect_positive(id)
    }

    fn test_services() -> (Arc<RoomService>, Arc<UserService>) {
        let connect_options = <sqlx::postgres::PgConnectOptions as std::str::FromStr>::from_str(
            "postgresql://synctv:synctv@127.0.0.1:1/synctv",
        )
        .expect("test connect options should parse");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(20))
            .connect_lazy_with(connect_options);
        let jwt_service =
            JwtService::new("membership-fanout-test-secret-key-minimum-32-chars").expect("jwt");
        let username_cache = UsernameCache::local_only("membership:username:".to_string(), 128, 60);
        let user_service = Arc::new(UserService::new(
            pool.clone(),
            jwt_service,
            username_cache,
            synctv_core::config::PasswordComplexityConfig::default(),
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("membership-fanout-test"),
            BruteForceProtection::in_memory("membership-fanout-test".to_string()),
        ));
        let room_service = Arc::new(RoomService::new(pool, (*user_service).clone()));
        (room_service, user_service)
    }

    fn assert_service_unavailable(error: &crate::impls::ApiError) {
        assert!(
            matches!(error, crate::impls::ApiError::ServiceUnavailable(_)),
            "repository lookup failure should surface as ServiceUnavailable, got {error:?}"
        );
    }

    #[tokio::test]
    async fn test_permission_changed_lookup_failure_does_not_broadcast_locally_in_standalone_mode()
    {
        let (room_service, user_service) = test_services();
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_membership_event_fanout_service(
            default_cluster_fanout_service(None, false),
            room_service,
            user_service,
            Some(event_service.clone()),
        );

        let error = service
            .publish_permission_changed(&room_id(), &user_id("target"), &user_id("actor"))
            .await
            .expect_err("repository lookup failure must abort permission-change publish");
        assert_service_unavailable(&error);

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );
        let local_events = event_service
            .local_events
            .lock()
            .expect("recorded local events mutex should not be poisoned");
        assert!(
            local_events.is_empty(),
            "standalone permission-change fanout must rely on the room notification bridge instead of rebroadcasting locally"
        );
    }

    #[tokio::test]
    async fn test_permission_changed_lookup_failure_does_not_broadcast_self_join_locally() {
        let (room_service, user_service) = test_services();
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_membership_event_fanout_service(
            default_cluster_fanout_service(None, false),
            room_service,
            user_service,
            Some(event_service.clone()),
        );
        let user = user_id("self-joiner");

        let error = service
            .publish_permission_changed(&room_id(), &user, &user)
            .await
            .expect_err("repository lookup failure must abort self-join publish");
        assert_service_unavailable(&error);

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );
        let local_events = event_service
            .local_events
            .lock()
            .expect("recorded local events mutex should not be poisoned");
        assert!(local_events.is_empty());
    }

    #[tokio::test]
    async fn test_permission_changed_lookup_failure_skips_cluster_publish() {
        let (room_service, user_service) = test_services();
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_membership_event_fanout_service(
            channel_cluster_fanout_service(tx),
            room_service,
            user_service,
            Some(event_service.clone()),
        );
        let error = service
            .publish_permission_changed(&room_id(), &user_id("target"), &user_id("actor"))
            .await
            .expect_err("repository lookup failure must abort cluster permission publish");
        assert_service_unavailable(&error);

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );
        assert!(
            rx.try_recv().is_err(),
            "permission change must not publish a cluster event after lookup failure"
        );
    }

    #[tokio::test]
    async fn test_user_left_lookup_failure_skips_cluster_publish() {
        let (room_service, user_service) = test_services();
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_membership_event_fanout_service(
            channel_cluster_fanout_service(tx),
            room_service,
            user_service,
            Some(event_service.clone()),
        );
        let error = service
            .publish_user_left(&room_id(), &user_id("target"))
            .await
            .expect_err("repository lookup failure must abort cluster user-left publish");
        assert_service_unavailable(&error);

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );
        assert!(
            rx.try_recv().is_err(),
            "user-left must not publish a cluster event after lookup failure"
        );
    }

    #[tokio::test]
    async fn test_user_left_lookup_failure_does_not_broadcast_locally_in_standalone_mode() {
        let (room_service, user_service) = test_services();
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_membership_event_fanout_service(
            default_cluster_fanout_service(None, false),
            room_service,
            user_service,
            Some(event_service.clone()),
        );

        let error = service
            .publish_user_left(&room_id(), &user_id("target"))
            .await
            .expect_err("repository lookup failure must abort user-left publish");
        assert_service_unavailable(&error);

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );
        let local_events = event_service
            .local_events
            .lock()
            .expect("recorded local events mutex should not be poisoned");
        assert!(
            local_events.is_empty(),
            "standalone user-left fanout must rely on the room notification bridge instead of rebroadcasting locally"
        );
    }
}
