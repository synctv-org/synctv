//! Member operations: `get_room_members`, `update_member_permissions`, kick, ban, unban

use crate::impls::ApiError;
use hex::encode as hex_encode;
use sha2::{Digest, Sha256};
use synctv_core::models::{PermissionBits, UserId};

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

impl ClientApiImpl {
    /// Get room members with pagination (E8 fix).
    ///
    /// Uses `get_room_members_paginated` to avoid loading ALL members into memory,
    /// matching the pattern used by the admin endpoint.
    pub async fn get_room_members(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

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
            Some(synctv_proto::common::MemberStatus::Pending) => {
                Some(synctv_core::models::MemberStatus::Pending)
            }
            Some(synctv_proto::common::MemberStatus::Rejected) => {
                Some(synctv_core::models::MemberStatus::Rejected)
            }
            Some(synctv_proto::common::MemberStatus::Banned) => {
                Some(synctv_core::models::MemberStatus::Banned)
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
            pagination: synctv_core::models::PageParams::new(
                Some(u32::try_from(req.page).unwrap_or(1)),
                Some(u32::try_from(req.page_size).unwrap_or(50)),
            ),
            search: (!req.search.is_empty()).then_some(req.search),
            role,
            status,
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
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::AddMemberRequest,
    ) -> Result<crate::proto::client::AddMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::AddMemberRequest {
            user_id: target_user_id,
            role,
            notify,
        } = req;
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let target_uid = crate::impls::proto_validated_user_id(target_user_id);
        let role = if role == synctv_proto::common::RoomMemberRole::Unspecified as i32 {
            synctv_core::models::RoomRole::Member
        } else {
            proto_role_to_room_role(role)?
        };

        let changed_by = uid.clone();
        let member = self
            .room_service
            .add_member(rid.clone(), uid, target_uid.clone(), role, notify)
            .await
            .map_err(ApiError::from)?;
        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &changed_by, None)
            .await?;

        let username = self
            .user_service
            .get_user(&target_uid)
            .await
            .map_or_else(|_| format!("user_{}", target_uid.as_str()), |u| u.username);
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
            member: Some(room_member_to_proto(&member_with_user, role_default)),
        })
    }

    pub async fn approve_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::ApproveMemberRequest,
    ) -> Result<crate::proto::client::ApproveMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::ApproveMemberRequest {
            user_id: target_user_id,
        } = req;
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let target_uid = crate::impls::proto_validated_user_id(target_user_id);

        let changed_by = uid.clone();
        let member = self
            .room_service
            .approve_member(rid.clone(), uid, target_uid.clone())
            .await
            .map_err(ApiError::from)?;
        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &changed_by, None)
            .await?;

        let username = self
            .user_service
            .get_user(&target_uid)
            .await
            .map_or_else(|_| format!("user_{}", target_uid.as_str()), |u| u.username);
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

        Ok(crate::proto::client::ApproveMemberResponse {
            member: Some(room_member_to_proto(&member_with_user, role_default)),
        })
    }

    pub async fn reject_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::RejectMemberRequest,
    ) -> Result<crate::proto::client::RejectMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::RejectMemberRequest {
            user_id: target_user_id,
            reason,
        } = req;
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let target_uid = crate::impls::proto_validated_user_id(target_user_id);
        let reason = (!reason.trim().is_empty()).then_some(reason.as_str());

        let changed_by = uid.clone();
        self.room_service
            .reject_member(rid.clone(), uid, target_uid.clone(), reason)
            .await
            .map_err(ApiError::from)?;

        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &changed_by, None)
            .await?;

        Ok(crate::proto::client::RejectMemberResponse { success: true })
    }

    pub async fn update_member_permissions(
        &self,
        user_id: &str,
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
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let target_uid = crate::impls::proto_validated_user_id(target_user_id);
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
                    .set_member_role(rid.clone(), uid.clone(), target_uid.clone(), new_role)
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
                    .set_member_permission(
                        rid.clone(),
                        uid.clone(),
                        target_uid.clone(),
                        added,
                        removed,
                    )
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
            .map_or_else(|_| format!("user_{}", target_uid.as_str()), |u| u.username);

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
            is_online,
            is_active: true,
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
            member: Some(room_member_to_proto(&member_with_user, role_default)),
        })
    }

    pub async fn kick_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::KickMemberRequest,
    ) -> Result<crate::proto::client::KickMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::KickMemberRequest {
            user_id: target_user_id,
        } = req;
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let target_uid = crate::impls::proto_validated_user_id(target_user_id);
        let cluster_event = self.member_fanout.reserve_kick_user_from_room().await?;
        let permission_fanout = self
            .membership_event_fanout
            .reserve_permission_changed()
            .await?;

        self.room_service
            .kick_member(rid.clone(), uid.clone(), target_uid.clone())
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
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::BanMemberRequest,
    ) -> Result<crate::proto::client::BanMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::BanMemberRequest {
            user_id: target_user_id,
            reason,
        } = req;

        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let target_uid = crate::impls::proto_validated_user_id(target_user_id);
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
            .ban_member(rid.clone(), uid.clone(), target_uid.clone(), reason)
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
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UnbanMemberRequest,
    ) -> Result<crate::proto::client::UnbanMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::client::UnbanMemberRequest {
            user_id: target_user_id,
        } = req;
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let target_uid = crate::impls::proto_validated_user_id(target_user_id);
        let permission_fanout = self
            .membership_event_fanout
            .reserve_permission_changed()
            .await?;

        self.room_service
            .member_service()
            .unban_member(rid.clone(), uid.clone(), target_uid.clone())
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
        self.get_room_members(user_id.as_str(), room_id.as_str(), req.clone())
            .await
    }
}
