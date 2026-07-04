//! Member operations: `get_room_members`, `update_member_permissions`, and kick.

use crate::impls::ApiError;
use hex::encode as hex_encode;
use sha2::{Digest, Sha256};
use synctv_core::models::{ReviewRequestId, ReviewStatus, UserId};
use synctv_core::service::{
    AddMemberWithOutboxRequest, MemberPermissionPatch, RoomJoinReviewListQuery,
    RoomJoinReviewRecord, UpdateMemberDisplayTagWithOutboxRequest,
    UpdateMemberRemarkNameWithOutboxRequest, UpdateMemberWithOutboxRequest,
};

use super::convert::{
    proto_role_filter_to_room_role, proto_role_to_assignable_room_role, proto_role_to_room_role,
    room_presence_stats_to_proto, try_members_to_proto, try_room_member_to_proto_with_permissions,
};
use super::media::prepare_delete_entries_outbox_fanout;
use super::{ClientApiImpl, GuestRoomAccess, RoomActor};

pub(crate) fn compute_room_members_response_version(
    response: &synctv_proto::client::GetRoomMembersResponse,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"room-members-snapshot-v1");
    hasher.update(response.total.to_le_bytes());
    for member in &response.members {
        hasher.update(member.room_id.as_bytes());
        hasher.update([0]);
        hasher.update(member.user_id.as_bytes());
        hasher.update([0]);
        hasher.update(member.username.as_bytes());
        hasher.update([0]);
        hasher.update(member.remark_name.as_bytes());
        hasher.update([0]);
        hasher.update(member.display_tag.as_bytes());
        hasher.update([0]);
        hasher.update(member.role.to_le_bytes());
        hasher.update(member.permissions.to_le_bytes());
        hasher.update(member.added_permissions.to_le_bytes());
        hasher.update(member.removed_permissions.to_le_bytes());
        hasher.update(member.admin_added_permissions.to_le_bytes());
        hasher.update(member.admin_removed_permissions.to_le_bytes());
        hasher.update(member.joined_at.to_le_bytes());
        hasher.update([u8::from(member.is_online)]);
        hasher.update(member.connection_count.to_le_bytes());
    }
    hex_encode(hasher.finalize())
}

fn review_status_i32_to_core(value: i32) -> Result<ReviewStatus, ApiError> {
    ReviewStatus::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported review status".to_string()))
}

async fn required_member_username(
    api: &ClientApiImpl,
    user_id: &UserId,
) -> Result<String, ApiError> {
    api.user_service
        .get_user(user_id)
        .await
        .map(|user| user.username)
        .map_err(ApiError::from)
}

fn proto_room_member_list_sort_by(
    value: i32,
) -> Result<synctv_core::models::RoomMemberListSortBy, ApiError> {
    match synctv_proto::client::RoomMemberListSortBy::try_from(value).map_err(|_| {
        ApiError::InvalidInput("Unsupported room member list sort field".to_string())
    })? {
        synctv_proto::client::RoomMemberListSortBy::Unspecified
        | synctv_proto::client::RoomMemberListSortBy::JoinedAt => {
            Ok(synctv_core::models::RoomMemberListSortBy::JoinedAt)
        }
        synctv_proto::client::RoomMemberListSortBy::Username => {
            Ok(synctv_core::models::RoomMemberListSortBy::Username)
        }
        synctv_proto::client::RoomMemberListSortBy::Role => {
            Ok(synctv_core::models::RoomMemberListSortBy::Role)
        }
    }
}

fn proto_sort_direction(value: i32) -> Result<synctv_core::models::SortDirection, ApiError> {
    match synctv_proto::client::SortDirection::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported sort direction".to_string()))?
    {
        synctv_proto::client::SortDirection::Unspecified
        | synctv_proto::client::SortDirection::Asc => Ok(synctv_core::models::SortDirection::Asc),
        synctv_proto::client::SortDirection::Desc => Ok(synctv_core::models::SortDirection::Desc),
    }
}

fn page_i32_to_usize(value: i32) -> Result<usize, ApiError> {
    let value = u32::try_from(value.max(1))
        .map_err(|_| ApiError::Internal("page must be positive".to_string()))?;
    usize::try_from(value).map_err(|_| ApiError::Internal("page exceeds usize::MAX".to_string()))
}

fn page_size_i32_to_usize(value: i32) -> Result<usize, ApiError> {
    let normalized = if value <= 0 { 50 } else { value.min(100) };
    let value = u32::try_from(normalized)
        .map_err(|_| ApiError::Internal("page_size must be positive".to_string()))?;
    usize::try_from(value)
        .map_err(|_| ApiError::Internal("page_size exceeds usize::MAX".to_string()))
}

fn usize_to_i64_api(value: usize, field: &'static str) -> Result<i64, ApiError> {
    i64::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i64::MAX")))
}

fn room_join_review_row_to_proto(
    row: &RoomJoinReviewRecord,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<synctv_proto::client::RoomJoinReview, ApiError> {
    Ok(synctv_proto::client::RoomJoinReview {
        id: public_id_codec
            .encode_review_request_id(row.id)
            .map_err(ApiError::InvalidInput)?,
        room_id: public_id_codec
            .encode_room_id(row.room_id)
            .map_err(ApiError::InvalidInput)?,
        user_id: public_id_codec
            .encode_user_id(row.user_id)
            .map_err(ApiError::InvalidInput)?,
        username: row.username.clone(),
        requested_role: row.requested_role,
        status: i32::from(i16::from(row.status)),
        requested_at: row.requested_at.timestamp(),
        reviewed_at: row.reviewed_at.map_or(0, |timestamp| timestamp.timestamp()),
        reviewed_by: row
            .reviewed_by
            .map(|id| {
                public_id_codec
                    .encode_user_id(id)
                    .map_err(ApiError::InvalidInput)
            })
            .transpose()?,
        rejection_reason: row.rejection_reason.clone(),
    })
}

impl ClientApiImpl {
    async fn require_member_review_permission(
        &self,
        room_id: &synctv_core::models::RoomId,
        user_id: &UserId,
    ) -> Result<(), ApiError> {
        self.room_service
            .check_membership(room_id, user_id)
            .await
            .map_err(Self::map_room_access_error)?;

        let permissions = self
            .room_service
            .permission_service()
            .get_user_permissions_no_cache(room_id, user_id)
            .await
            .map_err(ApiError::from)?;

        if permissions.has(synctv_core::models::RoomPermission::APPROVE_MEMBER) {
            Ok(())
        } else {
            Err(ApiError::Authorization(
                "Forbidden: Permission denied".to_string(),
            ))
        }
    }

    async fn load_room_join_review(
        &self,
        room_id: &synctv_core::models::RoomId,
        request_id: ReviewRequestId,
    ) -> Result<synctv_proto::client::RoomJoinReview, ApiError> {
        let row = self
            .review_service
            .load_room_join_in_room(request_id, *room_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Room join review not found".to_string()))?;
        room_join_review_row_to_proto(&row, &self.public_id_codec)
    }

    pub async fn list_room_join_reviews(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::ListRoomJoinReviewsRequest,
    ) -> Result<synctv_proto::client::ListRoomJoinReviewsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        self.require_member_review_permission(&rid, &uid).await?;

        let page = page_i32_to_usize(req.page)?;
        let page_size = page_size_i32_to_usize(req.page_size)?;
        let offset = page.saturating_sub(1).saturating_mul(page_size);
        let limit = usize_to_i64_api(page_size, "join review page size")?;
        let offset = usize_to_i64_api(offset, "join review offset")?;
        let status = review_status_i32_to_core(req.status)?;
        let target_user_id = if req.user_id.trim().is_empty() {
            None
        } else {
            Some(crate::impls::parse_user_id_param(
                &req.user_id,
                "user_id",
                &self.public_id_codec,
            )?)
        };

        let page = self
            .review_service
            .list_room_joins(&RoomJoinReviewListQuery {
                status,
                room_id: Some(rid),
                user_id: target_user_id,
                search: None,
                limit,
                offset,
            })
            .await?;
        let reviews = page
            .rows
            .iter()
            .map(|row| room_join_review_row_to_proto(row, &self.public_id_codec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(synctv_proto::client::ListRoomJoinReviewsResponse {
            reviews,
            total: i32::try_from(page.total).map_err(|_| {
                ApiError::Internal("room join review count exceeds i32 range".to_string())
            })?,
        })
    }

    /// Get room members with pagination.
    ///
    /// Uses the paginated service path so large rooms do not load every member
    /// before building the response.
    pub async fn get_room_members(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::GetRoomMembersRequest,
    ) -> Result<synctv_proto::client::GetRoomMembersResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_room_members_for_actor(&actor, req).await
    }

    pub async fn get_room_members_as_guest(
        &self,
        access: &GuestRoomAccess,
        req: synctv_proto::client::GetRoomMembersRequest,
    ) -> Result<synctv_proto::client::GetRoomMembersResponse, ApiError> {
        self.get_room_members_for_actor(&RoomActor::Guest(access.clone()), req)
            .await
    }

    pub async fn get_room_members_for_actor(
        &self,
        actor: &RoomActor,
        req: synctv_proto::client::GetRoomMembersRequest,
    ) -> Result<synctv_proto::client::GetRoomMembersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_room_permission(actor, synctv_core::models::RoomPermission::VIEW_MEMBER_LIST)
            .await?;
        let rid = actor.room_id();

        let role = req
            .role
            .map(proto_role_filter_to_room_role)
            .transpose()?
            .flatten();
        let query = synctv_core::models::RoomMemberListQuery {
            pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
            search: (!req.search.is_empty()).then_some(req.search),
            role,
            is_online: None,
            sort_by: proto_room_member_list_sort_by(req.sort_by)?,
            sort_direction: proto_sort_direction(req.sort_direction)?,
        };
        let (members, total) = self
            .room_service
            .get_room_members_query(&rid, query)
            .await
            .map_err(ApiError::from)?;
        let mut members = members;
        let member_user_ids: Vec<_> = members.iter().map(|member| member.user_id).collect();
        let online_user_ids: std::collections::HashSet<_> = self
            .presence_service
            .room_online_user_ids(rid, &member_user_ids)
            .await
            .map_err(ApiError::from)?
            .into_iter()
            .collect();
        let mut member_connection_counts =
            std::collections::HashMap::with_capacity(member_user_ids.len());
        for user_id in &member_user_ids {
            let stats = self
                .presence_service
                .user_room_stats(*user_id, rid)
                .await
                .map_err(ApiError::from)?;
            member_connection_counts.insert(*user_id, stats.connection_count);
        }
        for member in &mut members {
            member.is_online = online_user_ids.contains(&member.user_id);
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

        let mut response = synctv_proto::client::GetRoomMembersResponse {
            members: proto_members,
            total: i32::try_from(total)
                .map_err(|_| ApiError::Internal("Member count exceeds i32 range".to_string()))?,
            version: String::new(),
            presence: Some(room_presence_stats_to_proto(&room_presence)?),
        };
        response.version = compute_room_members_response_version(&response);
        Ok(response)
    }

    pub async fn add_member(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::AddMemberRequest,
    ) -> Result<synctv_proto::common::RoomMember, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let synctv_proto::client::AddMemberRequest {
            user_id: target_user_id,
            role,
            notify,
            remark_name,
            display_tag,
        } = req;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target_uid =
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec)?;
        let role = if role == synctv_proto::common::RoomMemberRole::Unspecified as i32 {
            synctv_core::models::RoomRole::Member
        } else {
            proto_role_to_assignable_room_role(role)?
        };
        let remark_name = crate::impls::normalize_member_remark_name(remark_name);
        let display_tag = crate::impls::normalize_member_display_tag(display_tag);

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
            .add_member_with_outbox(AddMemberWithOutboxRequest {
                room_id: rid,
                actor_id: uid,
                target_user_id: target_uid,
                role,
                remark_name,
                display_tag,
                notify,
                outbox_event_factory: Some(prepared_membership_fanout.outbox_factory()),
            })
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        let username = required_member_username(self, &target_uid).await?;
        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: member.room_id,
            user_id: member.user_id,
            username,
            remark_name: member.remark_name.clone(),
            display_tag: member.display_tag.clone(),
            role: member.role,
            status: member.status,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            joined_at: member.joined_at,
            is_online: target_presence.is_online,
            is_active: member.status.is_active(),
        };
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let permissions = self
            .room_service
            .permission_service()
            .effective_member_with_user_permissions(&member_with_user, &room_settings);

        try_room_member_to_proto_with_permissions(
            &member_with_user,
            permissions,
            &self.public_id_codec,
        )
    }

    pub async fn approve_room_join_review(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::ApproveRoomJoinReviewRequest,
    ) -> Result<synctv_proto::client::ApproveRoomJoinReviewResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        self.require_member_review_permission(&rid, &uid).await?;
        let request_id = self
            .public_id_codec
            .decode_review_request_id(&req.request_id)
            .map_err(ApiError::InvalidInput)?;
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
            .approve_join_request_with_outbox(
                rid,
                uid,
                request_id,
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        let username = required_member_username(self, &target_uid).await?;
        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: member.room_id,
            user_id: member.user_id,
            username,
            remark_name: member.remark_name.clone(),
            display_tag: member.display_tag.clone(),
            role: member.role,
            status: member.status,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            joined_at: member.joined_at,
            is_online: target_presence.is_online,
            is_active: member.status.is_active(),
        };
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let permissions = self
            .room_service
            .permission_service()
            .effective_member_with_user_permissions(&member_with_user, &room_settings);

        Ok(synctv_proto::client::ApproveRoomJoinReviewResponse {
            review: Some(self.load_room_join_review(&rid, request_id).await?),
            member: Some(try_room_member_to_proto_with_permissions(
                &member_with_user,
                permissions,
                &self.public_id_codec,
            )?),
        })
    }

    pub async fn reject_room_join_review(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::RejectRoomJoinReviewRequest,
    ) -> Result<synctv_proto::client::RejectRoomJoinReviewResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        self.require_member_review_permission(&rid, &uid).await?;
        let request_id = self
            .public_id_codec
            .decode_review_request_id(&req.request_id)
            .map_err(ApiError::InvalidInput)?;
        let reason = (!req.reason.trim().is_empty()).then_some(req.reason.as_str());

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
            .reject_join_request_with_outbox(
                rid,
                uid,
                request_id,
                reason,
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        Ok(synctv_proto::client::RejectRoomJoinReviewResponse {
            review: Some(self.load_room_join_review(&rid, request_id).await?),
            success: true,
        })
    }

    pub async fn update_member_permissions(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::UpdateMemberPermissionsRequest,
    ) -> Result<synctv_proto::common::RoomMember, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let synctv_proto::client::UpdateMemberPermissionsRequest {
            user_id: target_user_id,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = req;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target_uid =
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec)?;

        // Fetch current target member state BEFORE any mutations.
        // This prevents partial mutation when role change + permission update
        // are requested together and validation fails after the role commit.
        let has_permission_changes = added_permissions > 0
            || removed_permissions > 0
            || admin_added_permissions > 0
            || admin_removed_permissions > 0;

        let role_is_changing = role != synctv_proto::common::RoomMemberRole::Unspecified as i32;

        // Proto scalar fields cannot distinguish "omitted" from "explicitly set to 0".
        // For permission-only updates, a zero-valued payload is the documented reset-to-default
        // operation, so we must still flow through to `set_member_permission(0, 0)`.
        let should_apply_permission_update = has_permission_changes || !role_is_changing;

        if should_apply_permission_update || role_is_changing {
            let target_member = self
                .room_service
                .member_service()
                .get_member(&rid, &target_uid)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Target member not found".to_string()))?;

            // Determine the effective role AFTER potential mutation.
            // When the request changes the role, we must use the NEW role
            // (not the current role) to decide which permission columns
            // to validate against and write to.
            let effective_is_admin = if role_is_changing {
                let new_role = proto_role_to_room_role(role)?;
                matches!(new_role, synctv_core::models::RoomRole::Admin)
            } else {
                matches!(target_member.role, synctv_core::models::RoomRole::Admin)
            };

            if has_permission_changes {
                if effective_is_admin && (added_permissions > 0 || removed_permissions > 0) {
                    return Err(ApiError::Authorization(
                        "Admin members must use admin_added_permissions/admin_removed_permissions"
                            .to_string(),
                    ));
                }
                if !effective_is_admin
                    && (admin_added_permissions > 0 || admin_removed_permissions > 0)
                {
                    return Err(ApiError::Authorization(
                        "Only admin members use admin_added_permissions/admin_removed_permissions"
                            .to_string(),
                    ));
                }
            }

            let new_role = if role_is_changing {
                Some(proto_role_to_room_role(role)?)
            } else {
                None
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
            self.room_service
                .update_member_with_outbox(UpdateMemberWithOutboxRequest {
                    room_id: rid,
                    actor_id: uid,
                    target_user_id: target_uid,
                    role: new_role,
                    permissions: MemberPermissionPatch {
                        apply_permission_update: should_apply_permission_update,
                        added_permissions,
                        removed_permissions,
                        admin_added_permissions,
                        admin_removed_permissions,
                    },
                    outbox_event_factory: Some(prepared_membership_fanout.outbox_factory()),
                })
                .await
                .map_err(ApiError::from)?;
            prepared_membership_fanout.publish_after_outbox_commit();
        }

        // Get updated member directly instead of fetching all members
        let member = self
            .room_service
            .get_member(&rid, &target_uid)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Member not found".to_string()))?;

        // Fetch username for the target user
        let username = required_member_username(self, &target_uid).await?;

        let target_presence = self
            .presence_service
            .user_room_stats_fresh(target_uid, rid)
            .await
            .map_err(ApiError::from)?;

        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: member.room_id,
            user_id: member.user_id,
            username,
            remark_name: member.remark_name.clone(),
            display_tag: member.display_tag.clone(),
            role: member.role,
            status: member.status,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            joined_at: member.joined_at,
            is_online: target_presence.is_online,
            is_active: true,
        };

        // Fetch room settings for proper three-layer permission calculation
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let permissions = self
            .room_service
            .permission_service()
            .effective_member_with_user_permissions(&member_with_user, &room_settings);

        try_room_member_to_proto_with_permissions(
            &member_with_user,
            permissions,
            &self.public_id_codec,
        )
    }

    pub async fn update_member_remark_name(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::UpdateMemberRemarkNameRequest,
    ) -> Result<synctv_proto::common::RoomMember, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let synctv_proto::client::UpdateMemberRemarkNameRequest {
            user_id: target_user_id,
            remark_name,
        } = req;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target_uid =
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec)?;
        let remark_name = crate::impls::normalize_member_remark_name(remark_name);

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
            .update_member_remark_name_with_outbox(UpdateMemberRemarkNameWithOutboxRequest {
                room_id: rid,
                actor_id: uid,
                target_user_id: target_uid,
                remark_name,
                outbox_event_factory: Some(prepared_membership_fanout.outbox_factory()),
            })
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        let username = required_member_username(self, &target_uid).await?;
        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: member.room_id,
            user_id: member.user_id,
            username,
            remark_name: member.remark_name.clone(),
            display_tag: member.display_tag.clone(),
            role: member.role,
            status: member.status,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            joined_at: member.joined_at,
            is_online: target_presence.is_online,
            is_active: true,
        };
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let permissions = self
            .room_service
            .permission_service()
            .effective_member_with_user_permissions(&member_with_user, &room_settings);

        try_room_member_to_proto_with_permissions(
            &member_with_user,
            permissions,
            &self.public_id_codec,
        )
    }

    pub async fn update_member_display_tag(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::UpdateMemberDisplayTagRequest,
    ) -> Result<synctv_proto::common::RoomMember, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let synctv_proto::client::UpdateMemberDisplayTagRequest {
            user_id: target_user_id,
            display_tag,
        } = req;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target_uid =
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec)?;
        let display_tag = crate::impls::normalize_member_display_tag(display_tag);

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
            .update_member_display_tag_with_outbox(UpdateMemberDisplayTagWithOutboxRequest {
                room_id: rid,
                actor_id: uid,
                target_user_id: target_uid,
                display_tag,
                outbox_event_factory: Some(prepared_membership_fanout.outbox_factory()),
            })
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        let username = required_member_username(self, &target_uid).await?;
        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: member.room_id,
            user_id: member.user_id,
            username,
            remark_name: member.remark_name.clone(),
            display_tag: member.display_tag.clone(),
            role: member.role,
            status: member.status,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            joined_at: member.joined_at,
            is_online: target_presence.is_online,
            is_active: true,
        };
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let permissions = self
            .room_service
            .permission_service()
            .effective_member_with_user_permissions(&member_with_user, &room_settings);

        try_room_member_to_proto_with_permissions(
            &member_with_user,
            permissions,
            &self.public_id_codec,
        )
    }

    pub async fn kick_member(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: synctv_proto::client::KickMemberRequest,
    ) -> Result<synctv_proto::client::KickMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let synctv_proto::client::KickMemberRequest {
            user_id: target_user_id,
            kick_cooldown_seconds,
        } = req;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target_uid =
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec)?;

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
        let actor_username = required_member_username(self, &uid).await?;
        let prepared_cleanup_fanout = prepare_delete_entries_outbox_fanout(
            self.media_fanout.clone(),
            self.playlist_fanout.clone(),
            self.playback_fanout.clone(),
            self.realtime_fanout.clone(),
            rid,
            uid,
            actor_username,
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
            .kick_member_with_outbox(
                rid,
                uid,
                target_uid,
                kick_cooldown_seconds,
                synctv_core::service::KickMemberOutboxOptions {
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

        Ok(synctv_proto::client::KickMemberResponse { success: true })
    }
}

#[async_trait::async_trait]
impl crate::impls::room_members_snapshot::RoomMembersSnapshotService for ClientApiImpl {
    async fn get_room_members_snapshot(
        &self,
        actor: &crate::impls::client::RoomActor,
        req: &synctv_proto::client::GetRoomMembersRequest,
    ) -> Result<synctv_proto::client::GetRoomMembersResponse, crate::impls::ApiError> {
        self.get_room_members_for_actor(actor, req.clone()).await
    }
}

#[cfg(test)]
mod tests {
    use super::{proto_room_member_list_sort_by, proto_sort_direction};

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn api_ok<T>(result: Result<T, crate::impls::ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    #[test]
    fn room_member_query_enum_mappers_reject_unknown_values_and_preserve_defaults() -> TestResult {
        assert_eq!(
            api_ok(proto_room_member_list_sort_by(
                synctv_proto::client::RoomMemberListSortBy::Unspecified as i32
            ))?,
            synctv_core::models::RoomMemberListSortBy::JoinedAt
        );
        assert_eq!(
            api_ok(proto_sort_direction(
                synctv_proto::client::SortDirection::Unspecified as i32
            ))?,
            synctv_core::models::SortDirection::Asc
        );

        assert!(matches!(
            proto_room_member_list_sort_by(99),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("room member list sort")
        ));
        assert!(matches!(
            proto_sort_direction(99),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("sort direction")
        ));
        Ok(())
    }
}
