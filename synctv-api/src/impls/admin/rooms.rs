use synctv_core::{
    models::{AuditDetails, ReviewRequestId, RoomId, SortDirection as CoreSortDirection, UserId},
    service::{
        room::RoomCategoryUpdate, AdminAddMemberWithOutboxRequest, AdminRejectJoinRequestWithOutbox,
    },
};

use super::{
    i64_to_i32_api, load_creator_user_map, normalize_non_empty_filter,
    prepare_delete_entries_outbox_fanout, proto_admin_room_list_sort_by,
    proto_admin_room_member_list_sort_by, proto_admin_sort_direction, proto_room_status_filter,
    required_room_settings, try_admin_room_to_proto, try_members_to_proto, AdminApiImpl, ApiError,
    RequestContext, LOCAL_MANAGEMENT_ACTOR_USER_ID,
};
use crate::impls::client::{
    convert::{room_category_to_proto, room_label_to_proto, room_presence_stats_to_proto},
    parse_optional_room_category_id, parse_required_room_category_id, parse_room_label_ids,
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

fn room_member_with_user(
    member: &synctv_core::models::RoomMember,
    username: String,
    presence: &synctv_core::service::OnlineUserRoomStats,
) -> synctv_core::models::RoomMemberWithUser {
    synctv_core::models::RoomMemberWithUser {
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
        is_online: presence.is_online,
        is_active: member.status.is_active(),
    }
}

fn creator_user_from_map<'a>(
    users: &'a std::collections::HashMap<UserId, synctv_core::models::User>,
    room: &synctv_core::models::Room,
) -> Result<&'a synctv_core::models::User, ApiError> {
    users.get(&room.created_by).ok_or_else(|| {
        ApiError::Internal(format!(
            "Missing creator user for admin room {} creator {}",
            room.id, room.created_by
        ))
    })
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
            category_id: parse_optional_room_category_id(&req.category_id, &self.public_id_codec)?,
            label_ids: parse_room_label_ids(&req.label_ids, &self.public_id_codec)?,
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
        let creator_user_map = load_creator_user_map(&self.user_service, &creator_ids).await?;

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
        let presence_stats = self
            .presence_service
            .room_stats_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;
        let presence_by_room: std::collections::HashMap<synctv_core::models::RoomId, _> =
            presence_stats
                .iter()
                .map(|stats| (stats.room_id, stats))
                .collect();

        let mut room_list = Vec::with_capacity(rooms.len());
        for r in rooms {
            let member_count = crate::impls::room_member_count_or_zero(&member_counts, &r.id);
            let creator = creator_user_from_map(&creator_user_map, &r)?;
            let creator_avatar_url = self.creator_avatar_url(creator).await?;
            let cover = self.room_cover_for_admin(&r).await?;
            let settings = required_room_settings(&room_settings_map, &r.id)?;
            room_list.push(try_admin_room_to_proto(
                &r,
                Some(settings),
                Some(member_count),
                Some(creator.username.as_str()),
                creator.status,
                creator_avatar_url.as_deref(),
                cover.as_ref().map(|(reference, _)| reference),
                cover.as_ref().map(|(_, url)| url.as_str()),
                presence_by_room.get(&r.id).copied(),
                &self.public_id_codec,
            )?);
        }

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

    pub async fn list_room_categories(
        &self,
        req: synctv_proto::admin::ListRoomCategoriesRequest,
    ) -> Result<synctv_proto::admin::ListRoomCategoriesResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let categories = self
            .room_service
            .list_room_categories(!req.include_disabled)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::ListRoomCategoriesResponse {
            categories: categories
                .iter()
                .map(|category| room_category_to_proto(category, &self.public_id_codec))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    pub async fn upsert_room_category(
        &self,
        req: synctv_proto::admin::UpsertRoomCategoryRequest,
    ) -> Result<synctv_proto::admin::UpsertRoomCategoryResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let category = self
            .room_service
            .upsert_room_category(synctv_core::models::UpsertRoomCategory {
                key: req.key,
                name: req.name,
                description: req.description,
                sort_order: req.sort_order,
                is_enabled: req.is_enabled.unwrap_or(true),
            })
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::UpsertRoomCategoryResponse {
            category: Some(room_category_to_proto(&category, &self.public_id_codec)?),
        })
    }

    pub async fn delete_room_category(
        &self,
        req: synctv_proto::admin::DeleteRoomCategoryRequest,
    ) -> Result<synctv_proto::admin::DeleteRoomCategoryResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let category_id = parse_required_room_category_id(&req.category_id, &self.public_id_codec)?;
        let success = self
            .room_service
            .delete_room_category(category_id)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::DeleteRoomCategoryResponse { success })
    }

    pub async fn list_room_labels(
        &self,
        req: synctv_proto::admin::ListRoomLabelsRequest,
    ) -> Result<synctv_proto::admin::ListRoomLabelsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let category_id = parse_optional_room_category_id(&req.category_id, &self.public_id_codec)?;
        let labels = self
            .room_service
            .list_room_labels(!req.include_disabled, category_id)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::ListRoomLabelsResponse {
            labels: labels
                .iter()
                .map(|label| room_label_to_proto(label, &self.public_id_codec))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    pub async fn upsert_room_label(
        &self,
        req: synctv_proto::admin::UpsertRoomLabelRequest,
    ) -> Result<synctv_proto::admin::UpsertRoomLabelResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let category_id = parse_optional_room_category_id(&req.category_id, &self.public_id_codec)?;
        let label = self
            .room_service
            .upsert_room_label(synctv_core::models::UpsertRoomLabel {
                key: req.key,
                name: req.name,
                description: req.description,
                color: req.color,
                category_id,
                sort_order: req.sort_order,
                is_enabled: req.is_enabled.unwrap_or(true),
            })
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::UpsertRoomLabelResponse {
            label: Some(room_label_to_proto(&label, &self.public_id_codec)?),
        })
    }

    pub async fn delete_room_label(
        &self,
        req: synctv_proto::admin::DeleteRoomLabelRequest,
    ) -> Result<synctv_proto::admin::DeleteRoomLabelResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let label_id = self
            .public_id_codec
            .decode_room_label_id(&req.label_id)
            .map_err(ApiError::InvalidInput)?;
        let success = self
            .room_service
            .delete_room_label(label_id)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::DeleteRoomLabelResponse { success })
    }

    pub async fn update_room_taxonomy(
        &self,
        req: synctv_proto::admin::UpdateRoomTaxonomyRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::admin::UpdateRoomTaxonomyResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let room_id = crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        if req.clear_category && req.category_id.is_some() {
            return Err(ApiError::InvalidInput(
                "category_id conflicts with clear_category".to_string(),
            ));
        }
        let category_update = if req.clear_category {
            RoomCategoryUpdate::Set(None)
        } else if let Some(category_id) = req.category_id.as_deref() {
            RoomCategoryUpdate::Set(Some(parse_required_room_category_id(
                category_id,
                &self.public_id_codec,
            )?))
        } else {
            RoomCategoryUpdate::Preserve
        };
        let requested_label_ids = parse_room_label_ids(&req.label_ids, &self.public_id_codec)?;
        let assigned_by =
            (*admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID).then_some(*admin_user_id);
        self.room_service
            .update_room_taxonomy(room_id, category_update, &requested_label_ids, assigned_by)
            .await
            .map_err(ApiError::from)?;
        let response = self
            .get_room(synctv_proto::admin::GetRoomRequest {
                room_id: self
                    .public_id_codec
                    .encode_room_id(room_id)
                    .map_err(ApiError::Internal)?,
            })
            .await?;
        Ok(synctv_proto::admin::UpdateRoomTaxonomyResponse {
            room: response.room,
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
            AuditDetails {
                room_id: Some(rid.to_string()),
                ..Default::default()
            },
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
            AuditDetails {
                room_id: Some(room_id.to_string()),
                password_set: Some(new_password.is_some()),
                ..Default::default()
            },
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
        let (mut members, total) = self
            .room_service
            .get_room_members_query(&rid, query)
            .await
            .map_err(ApiError::from)?;
        let member_user_ids = members
            .iter()
            .map(|member| member.user_id)
            .collect::<Vec<_>>();
        let mut member_connection_counts = std::collections::HashMap::new();
        for user_id in &member_user_ids {
            let stats = self
                .presence_service
                .user_room_stats(*user_id, rid)
                .await
                .map_err(ApiError::from)?;
            member_connection_counts.insert(*user_id, stats.connection_count);
        }
        for member in &mut members {
            member.is_online = member_connection_counts
                .get(&member.user_id)
                .is_some_and(|count| *count > 0);
        }
        let room_presence = self
            .presence_service
            .room_stats(rid)
            .await
            .map_err(ApiError::from)?;
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;

        let mut proto_members = try_members_to_proto(
            &members,
            &room_settings,
            self.room_service.permission_service(),
            &self.public_id_codec,
        )?;
        for (member, proto_member) in members.iter().zip(proto_members.iter_mut()) {
            proto_member.connection_count = member_connection_counts
                .get(&member.user_id)
                .copied()
                .map(|count| {
                    i32::try_from(count).map_err(|_| {
                        ApiError::Internal("member connection count exceeds i32 range".to_string())
                    })
                })
                .transpose()?
                .unwrap_or_default();
        }

        Ok(synctv_proto::admin::GetRoomMembersResponse {
            members: proto_members,
            total: i64_to_i32_api(total, "room member count")?,
            presence: Some(room_presence_stats_to_proto(&room_presence)?),
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
        let target_presence = self
            .presence_service
            .user_room_stats_fresh(target_uid, rid)
            .await
            .map_err(ApiError::from)?;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(
                target_presence.is_online,
                target_presence.connection_count,
            );
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
        let member_with_user = room_member_with_user(&member, username, &target_presence);

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::MemberStatusUpdated,
            synctv_core::models::AuditTargetType::Member,
            Some(target_uid.to_string()),
            AuditDetails {
                room_id: Some(rid.to_string()),
                new_status: Some("active".to_string()),
                role: Some(role.to_string()),
                notify: Some(notify),
                ..Default::default()
            },
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
        let target_uid = self
            .review_service
            .load_room_join_in_room(request_id, rid)
            .await?
            .ok_or_else(|| ApiError::NotFound("Room join review not found".to_string()))?
            .user_id;
        let target_presence = self
            .presence_service
            .user_room_stats_fresh(target_uid, rid)
            .await
            .map_err(ApiError::from)?;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(
                target_presence.is_online,
                target_presence.connection_count,
            );

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
        prepared_membership_fanout.publish_after_outbox_commit();

        let username = self.load_member_response_username(&target_uid).await?;
        let member_with_user = room_member_with_user(&member, username, &target_presence);

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::MemberStatusUpdated,
            synctv_core::models::AuditTargetType::Member,
            Some(target_uid.to_string()),
            AuditDetails {
                room_id: Some(rid.to_string()),
                request_id: Some(request_id.to_string()),
                previous_review_status: Some("pending".to_string()),
                new_review_status: Some("approved".to_string()),
                ..Default::default()
            },
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
        let target_uid = self
            .review_service
            .load_room_join_in_room(request_id, rid)
            .await?
            .ok_or_else(|| ApiError::NotFound("Room join review not found".to_string()))?
            .user_id;
        let target_presence = self
            .presence_service
            .user_room_stats_fresh(target_uid, rid)
            .await
            .map_err(ApiError::from)?;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(
                target_presence.is_online,
                target_presence.connection_count,
            );

        self.room_service
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
            AuditDetails {
                room_id: Some(rid.to_string()),
                request_id: Some(request_id.to_string()),
                previous_review_status: Some("pending".to_string()),
                new_review_status: Some("rejected".to_string()),
                reason: (!reason.trim().is_empty()).then_some(reason.to_string()),
                ..Default::default()
            },
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
        let target_presence = self
            .presence_service
            .user_room_stats_fresh(target_uid, rid)
            .await
            .map_err(ApiError::from)?;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(
                target_presence.is_online,
                target_presence.connection_count,
            );
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

        let member_with_user = room_member_with_user(&updated_member, username, &target_presence);

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::MemberPermissionUpdated,
            synctv_core::models::AuditTargetType::Member,
            Some(target_uid.to_string()),
            AuditDetails {
                room_id: Some(rid.to_string()),
                role: Some(
                    role.map(crate::impls::client::room_role_to_proto)
                        .unwrap_or_default()
                        .to_string(),
                ),
                added_permissions: Some(added_permissions),
                removed_permissions: Some(removed_permissions),
                admin_added_permissions: Some(admin_added_permissions),
                admin_removed_permissions: Some(admin_removed_permissions),
                ..Default::default()
            },
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

        let target_presence = self
            .presence_service
            .user_room_stats_fresh(target_uid, rid)
            .await
            .map_err(ApiError::from)?;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(
                target_presence.is_online,
                target_presence.connection_count,
            );
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
            AuditDetails {
                room_id: Some(rid.to_string()),
                mode: Some("admin_override".to_string()),
                ..Default::default()
            },
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

        let creator_ids: Vec<synctv_core::models::UserId> = rooms
            .iter()
            .map(|r| r.created_by)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let creator_user_map = load_creator_user_map(&self.user_service, &creator_ids).await?;

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
        let presence_stats = self
            .presence_service
            .room_stats_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;
        let presence_by_room: std::collections::HashMap<synctv_core::models::RoomId, _> =
            presence_stats
                .iter()
                .map(|stats| (stats.room_id, stats))
                .collect();
        let mut admin_rooms = Vec::with_capacity(rooms.len());
        for room in &rooms {
            let creator = creator_user_from_map(&creator_user_map, room)?;
            let creator_avatar_url = self.creator_avatar_url(creator).await?;
            let cover = self.room_cover_for_admin(room).await?;
            let settings = required_room_settings(&room_settings_map, &room.id)?;
            let member_count = crate::impls::room_member_count_or_zero(&member_counts, &room.id);
            admin_rooms.push(try_admin_room_to_proto(
                room,
                Some(settings),
                Some(member_count),
                Some(creator.username.as_str()),
                creator.status,
                creator_avatar_url.as_deref(),
                cover.as_ref().map(|(reference, _)| reference),
                cover.as_ref().map(|(_, url)| url.as_str()),
                presence_by_room.get(&room.id).copied(),
                &self.public_id_codec,
            )?);
        }

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
            AuditDetails {
                room_id: Some(rid.to_string()),
                room_name: Some(room.name.clone()),
                reason: (!req.reason.trim().is_empty()).then_some(req.reason),
                ..Default::default()
            },
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
            AuditDetails {
                room_id: Some(rid.to_string()),
                room_name: Some(room.name.clone()),
                ..Default::default()
            },
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
            AuditDetails {
                request_id: Some(request_id.to_string()),
                room_id: Some(room.id.to_string()),
                room_name: Some(room.name.clone()),
                ..Default::default()
            },
            ctx,
        )
        .await;

        self.load_admin_room_proto(&room, None).await
    }
}
