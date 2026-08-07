use std::collections::HashMap;

use synctv_core::models::{RoomId, UserId, UserRole};
use synctv_core::service::NewRealtimeOutboxEvent;

use crate::impls::pagination::paginate_all;
use crate::realtime_lifecycle::DeletedRoomAfterCommitFanout;

use super::{check_role_hierarchy, AdminApiImpl, ApiError, LOCAL_MANAGEMENT_ACTOR_USER_ID};

const USER_ROOM_CLEANUP_PAGE_SIZE: u32 = 100;

impl AdminApiImpl {
    pub(in crate::impls::admin) fn publish_room_cache_invalidation(&self, room_id: &RoomId) {
        self.room_cache_fanout.publish_invalidation(room_id);
    }

    pub(in crate::impls::admin) fn prepare_deleted_room_outbox_fanout(
        &self,
        room_ids: &[RoomId],
        deleted_by: &UserId,
    ) -> Result<
        (
            HashMap<RoomId, NewRealtimeOutboxEvent>,
            Vec<DeletedRoomAfterCommitFanout>,
        ),
        ApiError,
    > {
        let mut outbox_events = HashMap::with_capacity(room_ids.len());
        let mut fanout = Vec::with_capacity(room_ids.len());
        for room_id in room_ids {
            let prepared = self
                .room_lifecycle_fanout
                .prepare_room_deleted_outbox_fanout(room_id, deleted_by)?;
            outbox_events.insert(*room_id, prepared.cloned_outbox_event());
            fanout.push(DeletedRoomAfterCommitFanout {
                room_id: *room_id,
                event: prepared.into_event(),
            });
        }
        Ok((outbox_events, fanout))
    }

    pub(in crate::impls::admin) async fn ban_user_with_cleanup(
        &self,
        target_user_id: &UserId,
        admin_user_id: &UserId,
        caller_role: UserRole,
        reason: Option<String>,
    ) -> Result<synctv_core::models::User, ApiError> {
        let user = self
            .user_service
            .get_user(target_user_id)
            .await
            .map_err(ApiError::from)?;

        check_role_hierarchy(caller_role, user.role, "ban")?;

        if user.is_banned {
            return Err(ApiError::InvalidInput("User is already banned".to_string()));
        }

        let (affected_room_ids, owned_room_ids) = tokio::try_join!(
            async {
                self.list_active_user_room_ids(target_user_id)
                    .await
                    .map_err(ApiError::from)
            },
            async {
                self.list_owned_room_ids(target_user_id)
                    .await
                    .map_err(ApiError::from)
            },
        )?;
        let mut owner_inactive_fanout = Vec::with_capacity(owned_room_ids.len());
        for room_id in owned_room_ids {
            owner_inactive_fanout.push(
                self.room_lifecycle_fanout
                    .prepare_room_owner_inactive_outbox_fanout(
                        &room_id,
                        target_user_id,
                        admin_user_id,
                    )?,
            );
        }

        let updated = self
            .user_service
            .ban_user_and_cleanup_memberships(
                target_user_id,
                (*admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID).then_some(admin_user_id),
                reason,
            )
            .await
            .map_err(ApiError::from)?;

        let prepared_playback_reset = self
            .playback_fanout
            .prepare_system_state_changed_batch_outbox_fanout();
        self.room_service
            .playback_service()
            .reset_playback_for_creator_with_outbox(
                target_user_id,
                Some(prepared_playback_reset.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_playback_reset.publish_after_outbox_commit();

        for prepared_fanout in owner_inactive_fanout {
            let room_id = *prepared_fanout.event().room_id().ok_or_else(|| {
                ApiError::Internal("RoomOwnerInactive missing room id".to_string())
            })?;
            self.room_service
                .finalize_room_owner_inactive_after_commit(&room_id)
                .await;

            prepared_fanout.publish_after_outbox_commit();

            self.realtime_lifecycle
                .disconnect_room(&room_id, "room_owner_inactive")
                .await;
        }

        self.invalidate_user_room_permission_caches(target_user_id, &affected_room_ids)
            .await;

        self.realtime_lifecycle
            .disconnect_user(target_user_id, "user_banned")
            .await;

        Ok(updated)
    }
    pub(in crate::impls::admin) async fn list_owned_room_ids(
        &self,
        user_id: &UserId,
    ) -> synctv_core::Result<Vec<RoomId>> {
        paginate_all(|page| async move {
            let (rooms, total) = self
                .room_service
                .list_rooms_by_creator(
                    user_id,
                    synctv_core::models::PageParams::new(
                        Some(page),
                        Some(USER_ROOM_CLEANUP_PAGE_SIZE),
                    ),
                )
                .await?;
            let room_ids = rooms.into_iter().map(|room| room.id).collect();
            Ok((room_ids, total))
        })
        .await
    }

    async fn list_active_user_room_ids(
        &self,
        user_id: &UserId,
    ) -> synctv_core::Result<Vec<RoomId>> {
        paginate_all(|page| async move {
            let (page_room_ids, total) = self
                .room_service
                .member_service()
                .list_user_rooms(
                    user_id,
                    synctv_core::models::PageParams::new(
                        Some(page),
                        Some(USER_ROOM_CLEANUP_PAGE_SIZE),
                    ),
                )
                .await?;
            Ok((page_room_ids, total))
        })
        .await
    }

    async fn invalidate_user_room_permission_caches(&self, user_id: &UserId, room_ids: &[RoomId]) {
        for room_id in room_ids {
            self.room_service
                .permission_service()
                .invalidate_cache(room_id, user_id)
                .await;
        }
    }
}
