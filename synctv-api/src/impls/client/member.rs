//! Member operations: `get_room_members`, `update_member_permissions`, kick, ban, unban

use crate::impls::ApiError;
use hex::encode as hex_encode;
use sha2::{Digest, Sha256};
use synctv_core::models::{PermissionBits, ReviewRequestId, ReviewStatus, UserId};
use synctv_core::service::{RoomJoinReviewListQuery, RoomJoinReviewRecord};

use super::convert::{
    members_to_proto, proto_role_filter_to_room_role, proto_role_to_room_role, room_member_to_proto,
};
use super::{ClientApiImpl, GuestRoomAccess, RoomActor};

pub(crate) fn compute_room_members_response_version(
    response: &crate::proto::client::GetRoomMembersResponse,
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
        hasher.update(member.role.to_le_bytes());
        hasher.update(member.permissions.to_le_bytes());
        hasher.update(member.status.to_le_bytes());
        hasher.update(member.added_permissions.to_le_bytes());
        hasher.update(member.removed_permissions.to_le_bytes());
        hasher.update(member.admin_added_permissions.to_le_bytes());
        hasher.update(member.admin_removed_permissions.to_le_bytes());
        hasher.update(member.joined_at.to_le_bytes());
        hasher.update([u8::from(member.is_online)]);
    }
    hex_encode(hasher.finalize())
}

fn review_status_i32_to_core(value: i32) -> ReviewStatus {
    ReviewStatus::try_from(value).unwrap_or_default()
}

fn page_i32_to_usize(value: i32) -> usize {
    usize::try_from(value.max(1)).unwrap_or(1)
}

fn page_size_i32_to_usize(value: i32) -> usize {
    usize::try_from(if value <= 0 { 50 } else { value.min(100) }).unwrap_or(50)
}

fn usize_to_i64_saturating(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn room_join_review_row_to_proto(
    row: &RoomJoinReviewRecord,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<crate::proto::client::RoomJoinReview, ApiError> {
    Ok(crate::proto::client::RoomJoinReview {
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
            .transpose()?
            .unwrap_or_default(),
        rejection_reason: row.rejection_reason.clone().unwrap_or_default(),
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

        if permissions.has(PermissionBits::APPROVE_MEMBER) {
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
    ) -> Result<crate::proto::client::RoomJoinReview, ApiError> {
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
        req: crate::proto::client::ListRoomJoinReviewsRequest,
    ) -> Result<crate::proto::client::ListRoomJoinReviewsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        self.require_member_review_permission(&rid, &uid).await?;

        let page = page_i32_to_usize(req.page);
        let page_size = page_size_i32_to_usize(req.page_size);
        let offset = page.saturating_sub(1).saturating_mul(page_size);
        let status = review_status_i32_to_core(req.status);
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
                limit: usize_to_i64_saturating(page_size),
                offset: usize_to_i64_saturating(offset),
            })
            .await?;
        let reviews = page
            .rows
            .iter()
            .map(|row| room_join_review_row_to_proto(row, &self.public_id_codec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(crate::proto::client::ListRoomJoinReviewsResponse {
            reviews,
            total: i32::try_from(page.total).unwrap_or(i32::MAX),
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
        req: crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_room_members_for_actor(&actor, req).await
    }

    pub async fn get_room_members_as_guest(
        &self,
        access: &GuestRoomAccess,
        req: crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, ApiError> {
        self.get_room_members_for_actor(&RoomActor::Guest(access.clone()), req)
            .await
    }

    pub async fn get_room_members_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_room_permission(actor, PermissionBits::VIEW_MEMBER_LIST)
            .await?;
        let rid = actor.room_id();

        let permissions = match actor {
            RoomActor::User { room_id, user_id } => self
                .room_service
                .permission_service()
                .get_user_permissions_no_cache(room_id, user_id)
                .await
                .map_err(ApiError::from)?,
            RoomActor::Guest(access) => access.permissions,
        };

        let role = req.role.and_then(proto_role_filter_to_room_role);
        let requested_status = req
            .status
            .and_then(|value| synctv_core::models::MemberStatus::try_from(value).ok());
        let can_view_non_active_members = permissions.has_any(
            PermissionBits::APPROVE_MEMBER
                | PermissionBits::KICK_MEMBER
                | PermissionBits::BAN_MEMBER
                | PermissionBits::ADD_MEMBER
                | PermissionBits::SET_MEMBER_PERMISSIONS,
        );
        let status = if can_view_non_active_members {
            requested_status
        } else {
            match requested_status {
                Some(synctv_core::models::MemberStatus::Active) | None => {
                    Some(synctv_core::models::MemberStatus::Active)
                }
                Some(_) => {
                    return Err(ApiError::Authorization(
                        "Forbidden: Viewing pending or historical room members requires room moderation permissions".to_string(),
                    ));
                }
            }
        };
        let query = synctv_core::models::RoomMemberListQuery {
            pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
            search: (!req.search.is_empty()).then_some(req.search),
            role,
            status,
            is_banned: req.is_banned,
            is_online: None,
            sort_by: match crate::proto::client::RoomMemberListSortBy::try_from(req.sort_by) {
                Ok(crate::proto::client::RoomMemberListSortBy::Username) => {
                    synctv_core::models::RoomMemberListSortBy::Username
                }
                Ok(crate::proto::client::RoomMemberListSortBy::Role) => {
                    synctv_core::models::RoomMemberListSortBy::Role
                }
                Ok(crate::proto::client::RoomMemberListSortBy::Status) => {
                    synctv_core::models::RoomMemberListSortBy::Status
                }
                _ => synctv_core::models::RoomMemberListSortBy::JoinedAt,
            },
            sort_direction: match crate::proto::client::SortDirection::try_from(req.sort_direction)
            {
                Ok(crate::proto::client::SortDirection::Desc) => {
                    synctv_core::models::SortDirection::Desc
                }
                _ => synctv_core::models::SortDirection::Asc,
            },
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
            .unwrap_or_default();
        let proto_members = members_to_proto(
            &members,
            &room_settings,
            self.room_service.permission_service(),
            &self.public_id_codec,
        );

        let mut response = crate::proto::client::GetRoomMembersResponse {
            members: proto_members,
            total: i32::try_from(total)
                .map_err(|_| ApiError::Internal("Member count exceeds i32 range".to_string()))?,
            version: String::new(),
        };
        response.version = compute_room_members_response_version(&response);
        Ok(response)
    }

    pub async fn add_member(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::AddMemberRequest,
    ) -> Result<crate::proto::client::AddMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::AddMemberRequest {
            user_id: target_user_id,
            role,
            notify,
        } = req;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target_uid =
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec)?;
        let role = if role == synctv_proto::common::RoomMemberRole::Unspecified as i32 {
            synctv_core::models::RoomRole::Member
        } else {
            proto_role_to_room_role(role)?
        };

        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(target_uid, uid);
        let member = self
            .room_service
            .add_member_with_outbox(
                rid,
                uid,
                target_uid,
                role,
                notify,
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        let username = self
            .user_service
            .get_user(&target_uid)
            .await
            .map_or_else(|_| format!("user_{target_uid}"), |u| u.username);
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
            left_at: member.left_at,
            is_online,
            is_active: member.status.is_active(),
            is_banned: member.is_banned(),
            banned_at: member.banned_at,
            banned_reason: member.banned_reason,
        };
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .unwrap_or_default();
        let role_default = self
            .room_service
            .permission_service()
            .calculate_role_default_permissions(&member_with_user.role, &room_settings);

        Ok(crate::proto::client::AddMemberResponse {
            member: Some(room_member_to_proto(
                &member_with_user,
                role_default,
                &self.public_id_codec,
            )),
        })
    }

    pub async fn approve_room_join_review(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::ApproveRoomJoinReviewRequest,
    ) -> Result<crate::proto::client::ApproveRoomJoinReviewResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        self.require_member_review_permission(&rid, &uid).await?;
        let request_id = self
            .public_id_codec
            .decode_review_request_id(&req.request_id)
            .map_err(ApiError::InvalidInput)?;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(UserId::MAX, uid);
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
        let target_uid = member.user_id;
        prepared_membership_fanout.publish_after_outbox_commit();

        let username = self
            .user_service
            .get_user(&target_uid)
            .await
            .map_or_else(|_| format!("user_{target_uid}"), |u| u.username);
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
            left_at: member.left_at,
            is_online,
            is_active: member.status.is_active(),
            is_banned: member.is_banned(),
            banned_at: member.banned_at,
            banned_reason: member.banned_reason,
        };
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .unwrap_or_default();
        let role_default = self
            .room_service
            .permission_service()
            .calculate_role_default_permissions(&member_with_user.role, &room_settings);

        Ok(crate::proto::client::ApproveRoomJoinReviewResponse {
            review: Some(self.load_room_join_review(&rid, request_id).await?),
            member: Some(room_member_to_proto(
                &member_with_user,
                role_default,
                &self.public_id_codec,
            )),
        })
    }

    pub async fn reject_room_join_review(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::RejectRoomJoinReviewRequest,
    ) -> Result<crate::proto::client::RejectRoomJoinReviewResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        self.require_member_review_permission(&rid, &uid).await?;
        let request_id = self
            .public_id_codec
            .decode_review_request_id(&req.request_id)
            .map_err(ApiError::InvalidInput)?;
        let reason = (!req.reason.trim().is_empty()).then_some(req.reason.as_str());

        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(UserId::MAX, uid);
        let _target_uid = self
            .room_service
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

        Ok(crate::proto::client::RejectRoomJoinReviewResponse {
            review: Some(self.load_room_join_review(&rid, request_id).await?),
            success: true,
        })
    }

    pub async fn update_member_permissions(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::UpdateMemberPermissionsRequest,
    ) -> Result<crate::proto::client::UpdateMemberPermissionsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::UpdateMemberPermissionsRequest {
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
            let prepared_membership_fanout = self
                .membership_event_fanout
                .prepare_permission_changed_outbox_fanout(target_uid, uid);
            self.room_service
                .update_member_with_outbox(
                    rid,
                    uid,
                    target_uid,
                    new_role,
                    should_apply_permission_update,
                    added_permissions,
                    removed_permissions,
                    admin_added_permissions,
                    admin_removed_permissions,
                    Some(prepared_membership_fanout.outbox_factory()),
                )
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
        let username = self
            .user_service
            .get_user(&target_uid)
            .await
            .map_or_else(|_| format!("user_{target_uid}"), |u| u.username);

        // Query ConnectionManager for actual online status instead of hardcoding false
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
            left_at: member.left_at,
            is_online,
            is_active: true,
            is_banned: member.is_banned(),
            banned_at: member.banned_at,
            banned_reason: member.banned_reason,
        };

        // Fetch room settings for proper three-layer permission calculation
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .unwrap_or_default();
        let role_default = self
            .room_service
            .permission_service()
            .calculate_role_default_permissions(&member_with_user.role, &room_settings);

        Ok(crate::proto::client::UpdateMemberPermissionsResponse {
            member: Some(room_member_to_proto(
                &member_with_user,
                role_default,
                &self.public_id_codec,
            )),
        })
    }

    pub async fn kick_member(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::KickMemberRequest,
    ) -> Result<crate::proto::client::KickMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::KickMemberRequest {
            user_id: target_user_id,
        } = req;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target_uid =
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec)?;

        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(target_uid, uid);
        let lifecycle_event = synctv_realtime::sync::RealtimeEvent::KickUserFromRoom {
            event_id: synctv_common::snanoid!(16),
            room_id: rid,
            user_id: target_uid,
            reason: "kicked".to_string(),
            timestamp: chrono::Utc::now(),
        };
        let lifecycle_outbox_event = self.realtime_fanout.outbox_event(&lifecycle_event);
        self.room_service
            .kick_member_with_outbox(
                rid,
                uid,
                target_uid,
                Some(prepared_membership_fanout.outbox_factory()),
                lifecycle_outbox_event,
            )
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();
        self.realtime_fanout
            .publish_after_outbox_commit(lifecycle_event);

        self.realtime_lifecycle
            .disconnect_user_from_room(&rid, &target_uid)
            .await;

        Ok(crate::proto::client::KickMemberResponse { success: true })
    }

    pub async fn ban_member(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::BanMemberRequest,
    ) -> Result<crate::proto::client::BanMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::BanMemberRequest {
            user_id: target_user_id,
            reason,
        } = req;

        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target_uid =
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec)?;
        let reason = if reason.is_empty() {
            None
        } else {
            Some(reason)
        };

        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(target_uid, uid);
        let lifecycle_reason = reason.clone().unwrap_or_else(|| "banned".to_string());
        let lifecycle_event = synctv_realtime::sync::RealtimeEvent::KickUserFromRoom {
            event_id: synctv_common::snanoid!(16),
            room_id: rid,
            user_id: target_uid,
            reason: lifecycle_reason,
            timestamp: chrono::Utc::now(),
        };
        let lifecycle_outbox_event = self.realtime_fanout.outbox_event(&lifecycle_event);
        self.room_service
            .ban_member_with_outbox(
                rid,
                uid,
                target_uid,
                reason,
                Some(prepared_membership_fanout.outbox_factory()),
                lifecycle_outbox_event,
            )
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();
        self.realtime_fanout
            .publish_after_outbox_commit(lifecycle_event);

        self.realtime_lifecycle
            .disconnect_user_from_room(&rid, &target_uid)
            .await;

        Ok(crate::proto::client::BanMemberResponse { success: true })
    }

    pub async fn unban_member(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::UnbanMemberRequest,
    ) -> Result<crate::proto::client::UnbanMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::UnbanMemberRequest {
            user_id: target_user_id,
        } = req;
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let target_uid =
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec)?;

        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(target_uid, uid);
        self.room_service
            .unban_member_with_outbox(
                rid,
                uid,
                target_uid,
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        Ok(crate::proto::client::UnbanMemberResponse { success: true })
    }
}

#[async_trait::async_trait]
impl crate::impls::room_members_snapshot::RoomMembersSnapshotService for ClientApiImpl {
    async fn get_room_members_snapshot(
        &self,
        actor: &crate::impls::client::RoomActor,
        req: &crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, crate::impls::ApiError> {
        self.get_room_members_for_actor(actor, req.clone()).await
    }
}
