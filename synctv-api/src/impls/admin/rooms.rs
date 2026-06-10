use synctv_core::{
    models::{ReviewRequestId, RoomId, SortDirection as CoreSortDirection, UserId},
    service::{AdminAddMemberWithOutboxRequest, AdminRejectJoinRequestWithOutbox},
};

use super::{
    i64_to_i32_api, load_creator_status_map, normalize_non_empty_filter,
    prepare_delete_entries_outbox_fanout, proto_admin_room_list_sort_by,
    proto_admin_room_member_list_sort_by, proto_admin_sort_direction, proto_room_status_filter,
    required_room_settings, room_creator_status_from_map, try_admin_room_to_proto,
    try_members_to_proto, AdminApiImpl, ApiError, RequestContext, LOCAL_MANAGEMENT_ACTOR_USER_ID,
};
use crate::impls::client::{
    proto_role_filter_to_room_role, proto_role_to_assignable_room_role, proto_role_to_room_role,
};

pub(in crate::impls::admin) fn username_from_loaded_user(
    user: synctv_core::models::User,
) -> Result<String, ApiError> {
    if user.username.trim().is_empty() {
        return Err(ApiError::Internal(format!(
            "Loaded user {} has empty username",
            user.id
        )));
    }

    Ok(user.username)
}

impl AdminApiImpl {
    async fn load_member_response_username(&self, target_uid: &UserId) -> Result<String, ApiError> {
        self.user_service
            .get_user(target_uid)
            .await
            .map_err(ApiError::from)
            .and_then(username_from_loaded_user)
    }

    pub async fn list_rooms(
        &self,
        req: synctv_proto::admin::ListRoomsRequest,
    ) -> Result<synctv_proto::admin::ListRoomsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let status = proto_room_status_filter(req.status)?;

        let query = synctv_core::models::RoomListQuery {
            pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
            status,
            search: if req.search.is_empty() {
                None
            } else {
                Some(req.search)
            },
            is_banned: req.is_banned,
            creator_id: if req.creator_id.is_empty() {
                None
            } else {
                Some(crate::impls::proto_validated_user_id(
                    req.creator_id,
                    &self.public_id_codec,
                )?)
            },
            sort_by: proto_admin_room_list_sort_by(req.sort_by)?,
            sort_direction: proto_admin_sort_direction(
                req.sort_direction,
                CoreSortDirection::Desc,
            )?,
        };

        let (rooms, total) = self
            .room_service
            .list_rooms(&query)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch creator usernames for all rooms
        let creator_ids: Vec<synctv_core::models::UserId> = rooms
            .iter()
            .map(|r| r.created_by)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let username_map = self
            .user_service
            .get_usernames(&creator_ids)
            .await
            .map_err(ApiError::from)?;
        let creator_status_map = load_creator_status_map(&self.user_service, &creator_ids).await?;

        let room_id_refs: Vec<&synctv_core::models::RoomId> = rooms.iter().map(|r| &r.id).collect();
        let room_ids: Vec<synctv_core::models::RoomId> = rooms.iter().map(|room| room.id).collect();
        let member_counts = self
            .room_service
            .get_member_count_batch(&room_id_refs)
            .await
            .map_err(ApiError::from)?;
        let room_settings_map = self
            .room_service
            .get_room_settings_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;

        let room_list: Vec<_> = rooms
            .into_iter()
            .map(|r| {
                let member_count = crate::impls::room_member_count_or_zero(&member_counts, &r.id);
                let creator_username = username_map.get(&r.created_by).map(String::as_str);
                let creator_status = room_creator_status_from_map(&creator_status_map, &r)?;
                let settings = required_room_settings(&room_settings_map, &r.id)?;
                try_admin_room_to_proto(
                    &r,
                    Some(settings),
                    Some(member_count),
                    creator_username,
                    creator_status,
                    &self.public_id_codec,
                )
            })
            .collect::<Result<Vec<_>, ApiError>>()?;

        Ok(synctv_proto::admin::ListRoomsResponse {
            rooms: room_list,
            total: i64_to_i32_api(total, "room count")?,
        })
    }

    pub async fn get_room(
        &self,
        req: synctv_proto::admin::GetRoomRequest,
    ) -> Result<synctv_proto::admin::GetRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let (room, settings) = self
            .room_service
            .get_room_with_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::GetRoomResponse {
            room: Some(self.load_admin_room_proto(&room, Some(&settings)).await?),
        })
    }

    pub async fn delete_room(
        &self,
        req: synctv_proto::admin::DeleteRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::DeleteRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;
        let prepared_outbox_fanout = self
            .room_lifecycle_fanout
            .prepare_room_deleted_outbox_fanout(&rid, admin_user_id)?;

        self.room_service
            .admin_delete_room_as_with_outbox(
                &rid,
                &actor,
                Some(prepared_outbox_fanout.cloned_outbox_event()),
            )
            .await
            .map_err(ApiError::from)?;

        prepared_outbox_fanout.publish_after_outbox_commit();

        // Force disconnect all connections and publishers in the deleted room.
        self.realtime_lifecycle
            .disconnect_room(&rid, "room_deleted")
            .await;

        // Audit log: delete_room is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::RoomDeleted,
            synctv_core::models::AuditTargetType::Room,
            Some(rid.to_string()),
            serde_json::json!({ "room_id": rid.to_string() }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::DeleteRoomResponse { success: true })
    }

    pub async fn update_room_password(
        &self,
        req: synctv_proto::admin::UpdateRoomPasswordRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::UpdateRoomPasswordResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let room_id =
            crate::impls::proto_validated_room_id(req.room_id.clone(), &self.public_id_codec)?;
        let new_password = if req.new_password.is_empty() {
            None
        } else {
            Some(req.new_password.as_str())
        };
        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let admin_username = admin_actor.username;
        let state = self
            .room_service
            .admin_set_room_password_as_internal(&room_id, new_password, Some(admin_user_id))
            .await
            .map_err(ApiError::from)?;
        self.publish_room_cache_invalidation(&room_id);
        tracing::debug!(
            room_id = %room_id,
            admin_user_id = %admin_user_id,
            admin_username = %admin_username,
            password_enabled = state.enabled,
            password_version = state.version,
            "Admin updated room password"
        );

        // Audit log: room password change is a security-relevant operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::RoomPasswordUpdated,
            synctv_core::models::AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({
                "room_id": room_id.to_string(),
                "password_set": new_password.is_some(),
            }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::UpdateRoomPasswordResponse { success: true })
    }

    pub async fn get_room_members(
        &self,
        req: synctv_proto::admin::GetRoomMembersRequest,
    ) -> Result<synctv_proto::admin::GetRoomMembersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let rid =
            crate::impls::proto_validated_room_id(req.room_id.clone(), &self.public_id_codec)?;
        let role = proto_role_filter_to_room_role(req.role)?;
        let query = synctv_core::models::RoomMemberListQuery {
            pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
            search: (!req.search.is_empty()).then_some(req.search),
            role,
            is_online: None,
            sort_by: proto_admin_room_member_list_sort_by(req.sort_by)?,
            sort_direction: proto_admin_sort_direction(req.sort_direction, CoreSortDirection::Asc)?,
        };
        let (members, total) = self
            .room_service
            .get_room_members_query(&rid, query)
            .await
            .map_err(ApiError::from)?;
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;

        let proto_members = try_members_to_proto(
            &members,
            &room_settings,
            self.room_service.permission_service(),
            &self.public_id_codec,
        )?;

        Ok(synctv_proto::admin::GetRoomMembersResponse {
            members: proto_members,
            total: i64_to_i32_api(total, "room member count")?,
        })
    }

    pub async fn add_member(
        &self,
        req: synctv_proto::admin::AddMemberRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::AddMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let synctv_proto::admin::AddMemberRequest {
            room_id,
            user_id,
            role,
            notify,
        } = req;
        let actor = self.require_admin_actor(admin_user_id).await?;
        let rid = crate::impls::proto_validated_room_id(room_id, &self.public_id_codec)?;
        let target_uid = crate::impls::proto_validated_user_id(user_id, &self.public_id_codec)?;
        let role = if role == synctv_proto::common::RoomMemberRole::Unspecified as i32 {
            synctv_core::models::RoomRole::Member
        } else {
            proto_role_to_assignable_room_role(role)?
        };
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(target_uid, *admin_user_id);
        let member = self
            .room_service
            .admin_add_member_with_outbox(AdminAddMemberWithOutboxRequest {
                room_id: rid,
                actor_id: *admin_user_id,
                actor_username: &actor.username,
                target_user_id: target_uid,
                role,
                notify,
                outbox_event_factory: Some(prepared_membership_fanout.outbox_factory()),
            })
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        let username = self.load_member_response_username(&target_uid).await?;
        let is_online = self
            .connection_service
            .get_connection_id(&rid, &target_uid)
            .is_some();
        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: member.room_id,
            user_id: member.user_id,
            username,
            role: member.role,
            status: member.status,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            joined_at: member.joined_at,
            is_online,
            is_active: member.status.is_active(),
        };

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::MemberStatusUpdated,
            synctv_core::models::AuditTargetType::Member,
            Some(target_uid.to_string()),
            serde_json::json!({
                "room_id": rid.to_string(),
                "new_status": "active",
                "role": role.to_string(),
                "notify": notify,
            }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::AddMemberResponse {
            member: Some(self.admin_room_member_to_proto(&member_with_user).await?),
        })
    }

    pub(super) async fn approve_room_join_request(
        &self,
        room_id: &str,
        request_id: ReviewRequestId,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::common::RoomMember, ApiError> {
        let actor = self.require_admin_actor(admin_user_id).await?;
        let rid = crate::impls::proto_validated_room_id(room_id, &self.public_id_codec)?;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(UserId::MAX, *admin_user_id);

        let member = self
            .room_service
            .admin_approve_join_request_with_outbox(
                rid,
                *admin_user_id,
                (*admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID).then_some(admin_user_id),
                &actor.username,
                request_id,
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        let target_uid = member.user_id;
        prepared_membership_fanout.publish_after_outbox_commit();

        let username = self.load_member_response_username(&target_uid).await?;
        let is_online = self
            .connection_service
            .get_connection_id(&rid, &target_uid)
            .is_some();
        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: member.room_id,
            user_id: member.user_id,
            username,
            role: member.role,
            status: member.status,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            joined_at: member.joined_at,
            is_online,
            is_active: member.status.is_active(),
        };

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::MemberStatusUpdated,
            synctv_core::models::AuditTargetType::Member,
            Some(target_uid.to_string()),
            serde_json::json!({
                "room_id": rid.to_string(),
                "request_id": request_id,
                "previous_review_status": "pending",
                "new_review_status": "approved",
            }),
            ctx,
        )
        .await;

        self.admin_room_member_to_proto(&member_with_user).await
    }

    pub(super) async fn reject_room_join_request(
        &self,
        room_id: &str,
        request_id: ReviewRequestId,
        reason: &str,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<bool, ApiError> {
        let actor = self.require_admin_actor(admin_user_id).await?;
        let rid = crate::impls::proto_validated_room_id(room_id, &self.public_id_codec)?;
        let reason_for_service = (!reason.trim().is_empty()).then_some(reason);
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(UserId::MAX, *admin_user_id);

        let target_uid = self
            .room_service
            .admin_reject_join_request_with_outbox(AdminRejectJoinRequestWithOutbox {
                room_id: rid,
                actor_id: *admin_user_id,
                reviewed_by: (*admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID)
                    .then_some(admin_user_id),
                actor_username: &actor.username,
                request_id,
                reason: reason_for_service,
                outbox_event_factory: Some(prepared_membership_fanout.outbox_factory()),
            })
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::MemberStatusUpdated,
            synctv_core::models::AuditTargetType::Member,
            Some(target_uid.to_string()),
            serde_json::json!({
                "room_id": rid.to_string(),
                "request_id": request_id,
                "previous_review_status": "pending",
                "new_review_status": "rejected",
                "reason": reason,
            }),
            ctx,
        )
        .await;

        Ok(true)
    }

    pub async fn update_member_permissions(
        &self,
        req: synctv_proto::admin::UpdateMemberPermissionsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::UpdateMemberPermissionsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let synctv_proto::admin::UpdateMemberPermissionsRequest {
            room_id,
            user_id,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = req;
        let rid = crate::impls::proto_validated_room_id(room_id, &self.public_id_codec)?;
        let target_uid = crate::impls::proto_validated_user_id(user_id, &self.public_id_codec)?;
        let role = if role == synctv_proto::common::RoomMemberRole::Unspecified as i32 {
            None
        } else {
            Some(proto_role_to_room_role(role)?)
        };
        if role.is_none()
            && added_permissions == 0
            && removed_permissions == 0
            && admin_added_permissions == 0
            && admin_removed_permissions == 0
        {
            return Err(ApiError::InvalidInput(
                "member permission update requires at least one changed field".to_string(),
            ));
        }

        let admin_username = self
            .load_admin_actor(admin_user_id)
            .await
            .map_err(|_| ApiError::NotFound("User not found".to_string()))?
            .username;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(target_uid, *admin_user_id);
        let updated_member = self
            .room_service
            .admin_update_member_with_outbox(
                synctv_core::service::member::AdminMemberUpdate {
                    room_id: rid,
                    actor_id: *admin_user_id,
                    actor_username: admin_username,
                    target_user_id: target_uid,
                    role,
                    added_permissions,
                    removed_permissions,
                    admin_added_permissions,
                    admin_removed_permissions,
                },
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        let username = self.load_member_response_username(&target_uid).await?;

        let is_online = self
            .connection_service
            .get_connection_id(&rid, &target_uid)
            .is_some();
        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: updated_member.room_id,
            user_id: updated_member.user_id,
            username,
            role: updated_member.role,
            status: updated_member.status,
            added_permissions: updated_member.added_permissions,
            removed_permissions: updated_member.removed_permissions,
            admin_added_permissions: updated_member.admin_added_permissions,
            admin_removed_permissions: updated_member.admin_removed_permissions,
            joined_at: updated_member.joined_at,
            is_online,
            is_active: true,
        };

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::MemberPermissionUpdated,
            synctv_core::models::AuditTargetType::Member,
            Some(target_uid.to_string()),
            serde_json::json!({
                "room_id": rid.to_string(),
                "role": role
                    .map(crate::impls::client::room_role_to_proto)
                    .unwrap_or_default(),
                "added_permissions": added_permissions,
                "removed_permissions": removed_permissions,
                "admin_added_permissions": admin_added_permissions,
                "admin_removed_permissions": admin_removed_permissions,
            }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::UpdateMemberPermissionsResponse {
            member: Some(self.admin_room_member_to_proto(&member_with_user).await?),
        })
    }

    pub async fn kick_member(
        &self,
        req: synctv_proto::admin::KickMemberRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::KickMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let synctv_proto::admin::KickMemberRequest {
            room_id,
            user_id,
            kick_cooldown_seconds,
        } = req;
        let rid = crate::impls::proto_validated_room_id(room_id, &self.public_id_codec)?;
        let target_uid = crate::impls::proto_validated_user_id(user_id, &self.public_id_codec)?;
        let admin_username = self
            .load_admin_actor(admin_user_id)
            .await
            .map_err(|_| ApiError::NotFound("User not found".to_string()))?
            .username;
        let persisted_kicked_by =
            (*admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID).then_some(*admin_user_id);

        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(target_uid, *admin_user_id);
        let prepared_cleanup_fanout = prepare_delete_entries_outbox_fanout(
            self.media_fanout.clone(),
            self.playlist_fanout.clone(),
            self.playback_fanout.clone(),
            self.realtime_fanout.clone(),
            rid,
            *admin_user_id,
            admin_username.clone(),
        );
        let lifecycle_event = synctv_realtime::sync::RealtimeEvent::KickUserFromRoom {
            event_id: synctv_common::snanoid!(16),
            room_id: rid,
            user_id: target_uid,
            reason: "kicked".to_string(),
            timestamp: chrono::Utc::now(),
        };
        let lifecycle_outbox_event = self
            .realtime_fanout
            .outbox_event(&lifecycle_event)
            .map_err(ApiError::Internal)?;
        self.room_service
            .admin_kick_member_with_outbox(
                rid,
                *admin_user_id,
                target_uid,
                kick_cooldown_seconds,
                persisted_kicked_by,
                synctv_core::service::room::KickMemberOutboxOptions {
                    permission_changed: Some(prepared_membership_fanout.outbox_factory()),
                    cleanup: Some(prepared_cleanup_fanout.member_cleanup_outbox_factory()),
                    lifecycle: Some(lifecycle_outbox_event),
                },
            )
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();
        prepared_cleanup_fanout.publish_after_outbox_commit();
        self.realtime_fanout
            .publish_after_outbox_commit(lifecycle_event);

        self.realtime_lifecycle
            .disconnect_user_from_room(&rid, &target_uid)
            .await;

        self.room_service
            .permission_service()
            .invalidate_cache(&rid, &target_uid)
            .await;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::MemberKicked,
            synctv_core::models::AuditTargetType::Member,
            Some(target_uid.to_string()),
            serde_json::json!({
                "room_id": rid.to_string(),
                "mode": "admin_override",
            }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::KickMemberResponse { success: true })
    }

    pub async fn get_user_rooms(
        &self,
        req: synctv_proto::admin::GetUserRoomsRequest,
    ) -> Result<synctv_proto::admin::GetUserRoomsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let uid =
            crate::impls::proto_validated_user_id(req.user_id.clone(), &self.public_id_codec)?;
        let status = proto_room_status_filter(req.status)?;
        let query = synctv_core::models::RoomListQuery {
            pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
            status,
            search: normalize_non_empty_filter(&req.search),
            is_banned: req.is_banned,
            sort_by: proto_admin_room_list_sort_by(req.sort_by)?,
            sort_direction: proto_admin_sort_direction(
                req.sort_direction,
                CoreSortDirection::Desc,
            )?,
            ..Default::default()
        };

        let (rooms, total) = self
            .room_service
            .list_related_rooms_for_user(&uid, &query)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch creator usernames for all rooms in a single query.
        let creator_ids: Vec<synctv_core::models::UserId> = rooms
            .iter()
            .map(|r| r.created_by)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let username_map = self
            .user_service
            .get_usernames(&creator_ids)
            .await
            .map_err(ApiError::from)?;
        let creator_status_map = load_creator_status_map(&self.user_service, &creator_ids).await?;

        let room_id_refs: Vec<&synctv_core::models::RoomId> =
            rooms.iter().map(|room| &room.id).collect();
        let room_ids: Vec<synctv_core::models::RoomId> = rooms.iter().map(|room| room.id).collect();
        let member_counts = self
            .room_service
            .get_member_count_batch(&room_id_refs)
            .await
            .map_err(ApiError::from)?;
        let room_settings_map = self
            .room_service
            .get_room_settings_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;
        let admin_rooms: Vec<synctv_proto::admin::AdminRoom> = rooms
            .iter()
            .map(|room| {
                let creator_username = username_map.get(&room.created_by).map(String::as_str);
                let creator_status = room_creator_status_from_map(&creator_status_map, room)?;
                let settings = required_room_settings(&room_settings_map, &room.id)?;
                let member_count =
                    crate::impls::room_member_count_or_zero(&member_counts, &room.id);
                try_admin_room_to_proto(
                    room,
                    Some(settings),
                    Some(member_count),
                    creator_username,
                    creator_status,
                    &self.public_id_codec,
                )
            })
            .collect::<Result<Vec<_>, ApiError>>()?;

        Ok(synctv_proto::admin::GetUserRoomsResponse {
            rooms: admin_rooms,
            total: i64_to_i32_api(total, "user room count")?,
        })
    }

    pub async fn ban_room(
        &self,
        req: synctv_proto::admin::BanRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::BanRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid =
            crate::impls::proto_validated_room_id(req.room_id.clone(), &self.public_id_codec)?;
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        if room.is_banned {
            return Err(ApiError::InvalidInput("Room is already banned".to_string()));
        }
        let prepared_outbox_fanout = self
            .room_lifecycle_fanout
            .prepare_room_banned_outbox_fanout(&rid, admin_user_id)?;

        let updated = self
            .room_service
            .ban_room_with_outbox(
                &rid,
                admin_user_id,
                Some(prepared_outbox_fanout.cloned_outbox_event()),
            )
            .await
            .map_err(ApiError::from)?;

        prepared_outbox_fanout.publish_after_outbox_commit();

        self.realtime_lifecycle
            .disconnect_room(&rid, "room_banned")
            .await;

        // Audit log: ban_room is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::RoomBanned,
            synctv_core::models::AuditTargetType::Room,
            Some(rid.to_string()),
            serde_json::json!({
                "room_id": rid.to_string(),
                "room_name": room.name,
                "reason": req.reason,
            }),
            ctx,
        )
        .await;

        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;

        Ok(synctv_proto::admin::BanRoomResponse {
            room: Some(
                self.load_admin_room_proto(&updated, Some(&settings))
                    .await?,
            ),
        })
    }

    pub async fn unban_room(
        &self,
        req: synctv_proto::admin::UnbanRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::UnbanRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        if !room.is_banned {
            return Err(ApiError::InvalidInput("Room is not banned".to_string()));
        }

        let updated = self
            .room_service
            .unban_room(&rid, admin_user_id)
            .await
            .map_err(ApiError::from)?;

        // Audit log: unban_room (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::RoomUnbanned,
            synctv_core::models::AuditTargetType::Room,
            Some(rid.to_string()),
            serde_json::json!({
                "room_id": rid.to_string(),
                "room_name": room.name,
            }),
            ctx,
        )
        .await;

        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;

        Ok(synctv_proto::admin::UnbanRoomResponse {
            room: Some(
                self.load_admin_room_proto(&updated, Some(&settings))
                    .await?,
            ),
        })
    }

    pub(super) async fn approve_room_creation_request(
        &self,
        request_id: RoomId,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::AdminRoom, ApiError> {
        self.require_admin_actor(admin_user_id).await?;
        let persisted_reviewed_by =
            (*admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID).then_some(admin_user_id);
        let room = self
            .room_service
            .approve_pending_room(request_id, persisted_reviewed_by)
            .await
            .map_err(ApiError::from)?;

        // Audit log: approving a room (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::RoomApproved,
            synctv_core::models::AuditTargetType::Room,
            Some(room.id.to_string()),
            serde_json::json!({
                "request_id": request_id.to_string(),
                "room_id": room.id.to_string(),
                "room_name": room.name,
            }),
            ctx,
        )
        .await;

        self.load_admin_room_proto(&room, None).await
    }
}
