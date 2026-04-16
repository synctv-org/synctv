use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::models::{PermissionBits, RoomId, RoomRole, UserId};
use synctv_core::service::{RoomService, UserService};

use crate::cluster_fanout::ClusterFanoutService;
use crate::impls::{ApiError, ClusterEventPublishReservation};
use crate::runtime::RealtimeEventService;

#[async_trait]
pub trait MembershipEventFanoutService: Send + Sync {
    async fn reserve_permission_changed(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    async fn publish_permission_changed(
        &self,
        room_id: &RoomId,
        target_user_id: &UserId,
        changed_by: &UserId,
        reservation: Option<ClusterEventPublishReservation>,
    ) -> Result<(), ApiError>;

    async fn reserve_user_left(&self) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    async fn publish_user_left(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        reservation: Option<ClusterEventPublishReservation>,
    ) -> Result<(), ApiError>;
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

    fn role_to_proto(role: RoomRole) -> i32 {
        match role {
            RoomRole::Creator => synctv_proto::common::RoomMemberRole::Creator as i32,
            RoomRole::Admin => synctv_proto::common::RoomMemberRole::Admin as i32,
            RoomRole::Member => synctv_proto::common::RoomMemberRole::Member as i32,
            RoomRole::Guest => synctv_proto::common::RoomMemberRole::Guest as i32,
        }
    }

    fn should_broadcast_permission_changed_locally(
        &self,
        target_user_id: &UserId,
        changed_by: &UserId,
    ) -> bool {
        !self.cluster_fanout.is_distributed_enabled() && target_user_id == changed_by
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
    async fn reserve_permission_changed(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out permission changes to cluster replicas")
            .await
    }

    async fn publish_permission_changed(
        &self,
        room_id: &RoomId,
        target_user_id: &UserId,
        changed_by: &UserId,
        reservation: Option<ClusterEventPublishReservation>,
    ) -> Result<(), ApiError> {
        let room_settings = self
            .room_service
            .get_room_settings(room_id)
            .await
            .unwrap_or_default();

        let (target_username, new_permissions, role, added_permissions, removed_permissions) =
            match self
                .room_service
                .member_service()
                .get_member(room_id, target_user_id)
                .await
            {
                Ok(Some(member)) => {
                    let username = self
                        .user_service
                        .get_user(target_user_id)
                        .await
                        .map(|u| u.username)
                        .unwrap_or_default();
                    let role_default = self
                        .room_service
                        .permission_service()
                        .calculate_role_default_permissions(&member.role, &room_settings);
                    (
                        username,
                        member.effective_permissions(role_default),
                        Self::role_to_proto(member.role),
                        member.added_permissions,
                        member.removed_permissions,
                    )
                }
                _ => (
                    String::new(),
                    PermissionBits::empty(),
                    synctv_proto::common::RoomMemberRole::Member as i32,
                    0,
                    0,
                ),
            };

        let changed_by_username = self
            .user_service
            .get_user(changed_by)
            .await
            .map(|u| u.username)
            .unwrap_or_default();

        let request = PublishRequest {
            event: ClusterEvent::PermissionChanged {
                event_id: synctv_common::snanoid!(16),
                room_id: room_id.clone(),
                target_user_id: target_user_id.clone(),
                target_username,
                changed_by: changed_by.clone(),
                changed_by_username,
                new_permissions,
                role,
                added_permissions: PermissionBits(added_permissions),
                removed_permissions: PermissionBits(removed_permissions),
                admin_added_permissions: PermissionBits(0),
                admin_removed_permissions: PermissionBits(0),
                timestamp: chrono::Utc::now(),
            },
        };
        if self.should_broadcast_permission_changed_locally(target_user_id, changed_by) {
            if let Some(event_service) = &self.event_service {
                event_service.broadcast_local(room_id, &request.event);
            }
        }
        if let Some(reservation) = reservation {
            reservation.publish(request);
            return Ok(());
        }

        self.cluster_fanout
            .reserve("failed to fan out permission changes to cluster replicas")
            .await
            .map(|reservation| {
                if let Some(reservation) = reservation {
                    reservation.publish(request);
                }
            })
            .inspect_err(|error| {
                tracing::warn!(
                    room_id = %room_id.as_str(),
                    target_user_id = %target_user_id.as_str(),
                    error = %error.message(),
                    "Permission change fanout failed"
                );
            })
    }

    async fn reserve_user_left(&self) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out UserLeft to cluster replicas")
            .await
    }

    async fn publish_user_left(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        reservation: Option<ClusterEventPublishReservation>,
    ) -> Result<(), ApiError> {
        let username = self
            .user_service
            .get_user(user_id)
            .await
            .map(|u| u.username)
            .unwrap_or_default();

        let request = PublishRequest {
            event: ClusterEvent::UserLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: room_id.clone(),
                user_id: user_id.clone(),
                username,
                timestamp: chrono::Utc::now(),
            },
        };
        self.cluster_fanout.publish(reservation, request);

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
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
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
                .push((room_id.as_str().to_string(), event.clone()));
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
        RoomId::from_string("room-membership-fanout".to_string())
    }

    fn user_id(value: &str) -> UserId {
        UserId::from_string(value.to_string())
    }

    fn test_services() -> (Arc<RoomService>, Arc<UserService>) {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool should initialize");
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

    #[tokio::test]
    async fn test_permission_changed_does_not_broadcast_locally_in_standalone_mode() {
        let (room_service, user_service) = test_services();
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_membership_event_fanout_service(
            default_cluster_fanout_service(None, false),
            room_service,
            user_service,
            Some(event_service.clone()),
        );

        service
            .publish_permission_changed(&room_id(), &user_id("target"), &user_id("actor"), None)
            .await
            .expect("standalone permission change publish should succeed");

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
    async fn test_permission_changed_broadcasts_locally_for_self_join_in_standalone_mode() {
        let (room_service, user_service) = test_services();
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_membership_event_fanout_service(
            default_cluster_fanout_service(None, false),
            room_service,
            user_service,
            Some(event_service.clone()),
        );
        let user = user_id("self-joiner");

        service
            .publish_permission_changed(&room_id(), &user, &user, None)
            .await
            .expect("standalone self-join publish should succeed");

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            1
        );
        let local_events = event_service
            .local_events
            .lock()
            .expect("recorded local events mutex should not be poisoned");
        assert_eq!(local_events.len(), 1);
        assert!(matches!(
            local_events[0].1,
            ClusterEvent::PermissionChanged { .. }
        ));
    }

    #[tokio::test]
    async fn test_permission_changed_skips_local_broadcast_and_uses_single_cluster_publish() {
        let (room_service, user_service) = test_services();
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_membership_event_fanout_service(
            default_cluster_fanout_service(Some(tx), true),
            room_service,
            user_service,
            Some(event_service.clone()),
        );
        let reservation = service
            .reserve_permission_changed()
            .await
            .expect("cluster reservation should succeed");

        service
            .publish_permission_changed(
                &room_id(),
                &user_id("target"),
                &user_id("actor"),
                reservation,
            )
            .await
            .expect("cluster permission change publish should succeed");

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );
        let request = rx
            .recv()
            .await
            .expect("cluster publish queue should receive a single request");
        assert!(matches!(
            request.event,
            ClusterEvent::PermissionChanged { .. }
        ));
        assert!(
            rx.try_recv().is_err(),
            "permission change should publish exactly one cluster event"
        );
    }

    #[tokio::test]
    async fn test_user_left_skips_local_broadcast_and_uses_single_cluster_publish() {
        let (room_service, user_service) = test_services();
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_membership_event_fanout_service(
            default_cluster_fanout_service(Some(tx), true),
            room_service,
            user_service,
            Some(event_service.clone()),
        );
        let reservation = service
            .reserve_user_left()
            .await
            .expect("cluster reservation should succeed");

        service
            .publish_user_left(&room_id(), &user_id("target"), reservation)
            .await
            .expect("cluster user-left publish should succeed");

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );
        let request = rx
            .recv()
            .await
            .expect("cluster publish queue should receive a single request");
        assert!(matches!(request.event, ClusterEvent::UserLeft { .. }));
        assert!(
            rx.try_recv().is_err(),
            "user-left should publish exactly one cluster event"
        );
    }

    #[tokio::test]
    async fn test_user_left_does_not_broadcast_locally_in_standalone_mode() {
        let (room_service, user_service) = test_services();
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_membership_event_fanout_service(
            default_cluster_fanout_service(None, false),
            room_service,
            user_service,
            Some(event_service.clone()),
        );

        service
            .publish_user_left(&room_id(), &user_id("target"), None)
            .await
            .expect("standalone user-left publish should succeed");

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
