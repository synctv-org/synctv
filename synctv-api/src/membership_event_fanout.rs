use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::models::{PermissionBits, RoomId, RoomRole, UserId};
use synctv_core::service::{RoomService, UserService};

use crate::cluster_fanout::ClusterFanoutService;
use crate::impls::{ApiError, ClusterEventPublishReservation};

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

    async fn reserve_user_left(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

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
}

impl DefaultMembershipEventFanoutService {
    #[must_use]
    pub fn new(
        cluster_fanout: Arc<dyn ClusterFanoutService>,
        room_service: Arc<RoomService>,
        user_service: Arc<UserService>,
    ) -> Self {
        Self {
            cluster_fanout,
            room_service,
            user_service,
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

    async fn reserve_user_left(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
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

        self.cluster_fanout.publish(
            reservation,
            PublishRequest {
                event: ClusterEvent::UserLeft {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room_id.clone(),
                    user_id: user_id.clone(),
                    username,
                    timestamp: chrono::Utc::now(),
                },
            },
        );

        Ok(())
    }
}

#[must_use]
pub fn default_membership_event_fanout_service(
    cluster_fanout: Arc<dyn ClusterFanoutService>,
    room_service: Arc<RoomService>,
    user_service: Arc<UserService>,
) -> Arc<dyn MembershipEventFanoutService> {
    Arc::new(DefaultMembershipEventFanoutService::new(
        cluster_fanout,
        room_service,
        user_service,
    ))
}
