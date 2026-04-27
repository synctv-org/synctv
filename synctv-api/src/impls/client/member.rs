//! Member operations: `get_room_members`, `update_member_permissions`, kick, ban, unban

use crate::impls::ApiError;
use hex::encode as hex_encode;
use sha2::{Digest, Sha256};
use sqlx::Row;
use synctv_core::models::{PermissionBits, ReviewRequestId, ReviewStatus, RoomId, UserId};

use super::convert::{members_to_proto, proto_role_to_room_role, room_member_to_proto};
use super::ClientApiImpl;

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
    if value == synctv_proto::common::ReviewStatus::Unspecified as i32 {
        ReviewStatus::Pending
    } else {
        i16::try_from(value)
            .ok()
            .and_then(|value| ReviewStatus::try_from(value).ok())
            .unwrap_or(ReviewStatus::Pending)
    }
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
    row: &sqlx::postgres::PgRow,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<crate::proto::client::RoomJoinReview, ApiError> {
    let id: ReviewRequestId = row.try_get("id")?;
    let requested_at: chrono::DateTime<chrono::Utc> = row.try_get("requested_at")?;
    let reviewed_at: Option<chrono::DateTime<chrono::Utc>> = row.try_get("reviewed_at")?;
    let room_id: RoomId = row.try_get("room_id")?;
    let user_id: UserId = row.try_get("user_id")?;
    let reviewed_by: Option<UserId> = row.try_get("reviewed_by")?;
    let rejection_reason: Option<String> = row.try_get("rejection_reason")?;
    let status: ReviewStatus = row.try_get("status")?;
    Ok(crate::proto::client::RoomJoinReview {
        id: public_id_codec
            .encode_review_request_id(id)
            .map_err(ApiError::InvalidInput)?,
        room_id: public_id_codec
            .encode_room_id(room_id)
            .map_err(ApiError::InvalidInput)?,
        user_id: public_id_codec
            .encode_user_id(user_id)
            .map_err(ApiError::InvalidInput)?,
        username: row.try_get("username")?,
        requested_role: row.try_get("requested_role")?,
        status: i32::from(i16::from(status)),
        requested_at: requested_at.timestamp(),
        reviewed_at: reviewed_at.map_or(0, |timestamp| timestamp.timestamp()),
        reviewed_by: reviewed_by
            .map(|id| {
                public_id_codec
                    .encode_user_id(id)
                    .map_err(ApiError::InvalidInput)
            })
            .transpose()?
            .unwrap_or_default(),
        rejection_reason: rejection_reason.unwrap_or_default(),
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
        let row = sqlx::query(
            r"
            SELECT rjr.id, rjr.room_id, rjr.user_id, COALESCE(u.username, '') AS username,
                   COALESCE(rjr.requested_role, 0)::int4 AS requested_role, rjr.status,
                   rjr.requested_at, rjr.reviewed_at, rjr.reviewed_by, rjr.rejection_reason
            FROM room_join_requests rjr
            LEFT JOIN users u ON u.id = rjr.user_id
            WHERE rjr.id = $1 AND rjr.room_id = $2
            ",
        )
        .bind(request_id)
        .bind(room_id)
        .fetch_optional(self.user_service.pool())
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

        let total = sqlx::query_scalar::<_, i64>(
            r"
            SELECT COUNT(*)
            FROM room_join_requests
            WHERE room_id = $1
              AND status = $2
              AND ($3::bigint IS NULL OR user_id = $3)
            ",
        )
        .bind(rid)
        .bind(status)
        .bind(target_user_id)
        .fetch_one(self.user_service.pool())
        .await?;

        let rows = sqlx::query(
            r"
            SELECT rjr.id, rjr.room_id, rjr.user_id, COALESCE(u.username, '') AS username,
                   COALESCE(rjr.requested_role, 0)::int4 AS requested_role, rjr.status,
                   rjr.requested_at, rjr.reviewed_at, rjr.reviewed_by, rjr.rejection_reason
            FROM room_join_requests rjr
            LEFT JOIN users u ON u.id = rjr.user_id
            WHERE rjr.room_id = $1
              AND rjr.status = $2
              AND ($3::bigint IS NULL OR rjr.user_id = $3)
            ORDER BY rjr.requested_at DESC, rjr.id DESC
            LIMIT $4 OFFSET $5
            ",
        )
        .bind(rid)
        .bind(status)
        .bind(target_user_id)
        .bind(usize_to_i64_saturating(page_size))
        .bind(usize_to_i64_saturating(offset))
        .fetch_all(self.user_service.pool())
        .await?;
        let reviews = rows
            .iter()
            .map(|row| room_join_review_row_to_proto(row, &self.public_id_codec))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(crate::proto::client::ListRoomJoinReviewsResponse {
            reviews,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    /// Get room members with pagination (E8 fix).
    ///
    /// Uses `get_room_members_paginated` to avoid loading ALL members into memory,
    /// matching the pattern used by the admin endpoint.
    pub async fn get_room_members(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let permissions = self
            .room_service
            .permission_service()
            .get_user_permissions_no_cache(&rid, &uid)
            .await
            .map_err(ApiError::from)?;

        if !permissions.has(PermissionBits::VIEW_MEMBER_LIST) {
            return Err(ApiError::Authorization(
                "Forbidden: Permission denied".to_string(),
            ));
        }

        let role = match req
            .role
            .and_then(|value| synctv_proto::common::RoomMemberRole::try_from(value).ok())
        {
            Some(synctv_proto::common::RoomMemberRole::Guest) => {
                Some(synctv_core::models::RoomRole::Guest)
            }
            Some(synctv_proto::common::RoomMemberRole::Member) => {
                Some(synctv_core::models::RoomRole::Member)
            }
            Some(synctv_proto::common::RoomMemberRole::Admin) => {
                Some(synctv_core::models::RoomRole::Admin)
            }
            Some(synctv_proto::common::RoomMemberRole::Creator) => {
                Some(synctv_core::models::RoomRole::Creator)
            }
            _ => None,
        };
        let requested_status = match req
            .status
            .and_then(|value| synctv_proto::common::MemberStatus::try_from(value).ok())
        {
            Some(synctv_proto::common::MemberStatus::Active) => {
                Some(synctv_core::models::MemberStatus::Active)
            }
            Some(synctv_proto::common::MemberStatus::Left) => {
                Some(synctv_core::models::MemberStatus::Left)
            }
            _ => None,
        };
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
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec);
        let role = if role == synctv_proto::common::RoomMemberRole::Unspecified as i32 {
            synctv_core::models::RoomRole::Member
        } else {
            proto_role_to_room_role(role)?
        };

        let changed_by = uid;
        let member = self
            .room_service
            .add_member(rid, uid, target_uid, role, notify)
            .await
            .map_err(ApiError::from)?;
        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &changed_by, None)
            .await?;

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
        let changed_by = uid;
        let member = self
            .room_service
            .approve_join_request(rid, uid, request_id)
            .await
            .map_err(ApiError::from)?;
        let target_uid = member.user_id;
        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &changed_by, None)
            .await?;

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

        let changed_by = uid;
        let target_uid = self
            .room_service
            .reject_join_request(rid, uid, request_id, reason)
            .await
            .map_err(ApiError::from)?;

        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &changed_by, None)
            .await?;

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
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec);
        let permission_fanout = self
            .membership_event_fanout
            .reserve_permission_changed()
            .await?;

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

            // All validation passed — apply mutations now.

            // Handle role update if provided (non-zero = specified).
            if role_is_changing {
                let new_role = proto_role_to_room_role(role)?;
                self.room_service
                    .member_service()
                    .set_member_role(rid, uid, target_uid, new_role)
                    .await
                    .map_err(ApiError::from)?;
            }

            // Handle permission updates
            if should_apply_permission_update {
                // The service layer (set_member_permissions) already enforces
                // GRANT_PERMISSION as the single source of truth via
                // check_permission_no_cache.
                let added = if effective_is_admin {
                    admin_added_permissions
                } else {
                    added_permissions
                };

                let removed = if effective_is_admin {
                    admin_removed_permissions
                } else {
                    removed_permissions
                };

                self.room_service
                    .set_member_permission(rid, uid, target_uid, added, removed)
                    .await
                    .map_err(ApiError::from)?;
            }
        }

        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &uid, permission_fanout)
            .await?;

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
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec);
        let cluster_event = self.member_fanout.reserve_kick_user_from_room().await?;
        let permission_fanout = self
            .membership_event_fanout
            .reserve_permission_changed()
            .await?;

        self.room_service
            .kick_member(rid, uid, target_uid)
            .await
            .map_err(ApiError::from)?;

        self.realtime_lifecycle
            .disconnect_user_from_room(&rid, &target_uid)
            .await;

        self.member_fanout
            .publish_kick_user_from_room(cluster_event, &rid, &target_uid, "kicked");

        // Notify other replicas to invalidate permission cache
        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &uid, permission_fanout)
            .await?;

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
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec);
        let reason = if reason.is_empty() {
            None
        } else {
            Some(reason)
        };
        let cluster_event = self.member_fanout.reserve_kick_user_from_room().await?;
        let permission_fanout = self
            .membership_event_fanout
            .reserve_permission_changed()
            .await?;

        self.room_service
            .member_service()
            .ban_member(rid, uid, target_uid, reason)
            .await
            .map_err(ApiError::from)?;

        self.realtime_lifecycle
            .disconnect_user_from_room(&rid, &target_uid)
            .await;

        self.member_fanout
            .publish_kick_user_from_room(cluster_event, &rid, &target_uid, "banned");

        // Notify other replicas to invalidate permission cache
        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &uid, permission_fanout)
            .await?;

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
            crate::impls::proto_validated_user_id(target_user_id, &self.public_id_codec);
        let permission_fanout = self
            .membership_event_fanout
            .reserve_permission_changed()
            .await?;

        self.room_service
            .member_service()
            .unban_member(rid, uid, target_uid)
            .await
            .map_err(ApiError::from)?;

        // Notify other replicas to invalidate permission cache
        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &uid, permission_fanout)
            .await?;

        Ok(crate::proto::client::UnbanMemberResponse { success: true })
    }
}

#[async_trait::async_trait]
impl crate::impls::room_members_snapshot::RoomMembersSnapshotService for ClientApiImpl {
    async fn get_room_members_snapshot(
        &self,
        user_id: &UserId,
        room_id: &synctv_core::models::RoomId,
        req: &crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, crate::impls::ApiError> {
        let public_room_id = self
            .public_id_codec
            .encode_room_id(*room_id)
            .map_err(crate::impls::ApiError::InvalidInput)?;
        self.get_room_members(user_id, &public_room_id, req.clone())
            .await
    }
}
