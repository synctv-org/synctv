//! Member management service
//!
//! Handles room member operations including joining, leaving, kicking,
//! and role management with Allow/Deny permission pattern.

use crate::{
    cache::CacheInvalidationRuntime,
    models::{
        AddMemberOptions, AuditAction, AuditDetails, AuditTargetType, MemberStatus,
        MyRoomListQuery, PageParams, Room, RoomId, RoomMember, RoomMemberWithUser, RoomRole,
        UserId,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::audit::{AuditEventParams, AuditService},
    service::notification::{NotificationService, PermissionChangedNotification},
    service::PermissionService,
    Error, Result,
};

use std::sync::Arc;

mod permissions;

/// Member management service
///
/// Responsible for all member-related operations within rooms.
#[derive(Clone)]
pub struct MemberService {
    member_repo: RoomMemberRepository,
    room_repo: RoomRepository,
    room_settings_repo: Option<RoomSettingsRepository>,
    permission_service: PermissionService,
    audit_service: Option<Arc<AuditService>>,
    cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    notification_service: NotificationService,
}

pub struct AdminMemberUpdate {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub actor_username: String,
    pub target_user_id: UserId,
    pub role: Option<RoomRole>,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
}

impl std::fmt::Debug for MemberService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemberService").finish()
    }
}

impl MemberService {
    /// Create a new member service
    #[must_use]
    pub fn new(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        permission_service: PermissionService,
        notification_service: NotificationService,
    ) -> Self {
        Self {
            member_repo,
            room_repo,
            room_settings_repo: None,
            permission_service,
            audit_service: None,
            cache_invalidation: None,
            notification_service,
        }
    }

    /// Create a member service with optional collaborators wired at construction time.
    #[must_use]
    pub fn new_with_runtime(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        room_settings_repo: Option<RoomSettingsRepository>,
        permission_service: PermissionService,
        audit_service: Option<Arc<AuditService>>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
        notification_service: NotificationService,
    ) -> Self {
        Self {
            member_repo,
            room_repo,
            room_settings_repo,
            permission_service,
            audit_service,
            cache_invalidation,
            notification_service,
        }
    }

    async fn notify_permission_changed(
        &self,
        room_id: &RoomId,
        actor_id: &UserId,
        actor_username: &str,
        member: &RoomMember,
    ) {
        let resolved_actor_username = if actor_username.trim().is_empty() {
            match self.lookup_username(actor_id).await {
                Ok(username) => username,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        actor_id = %actor_id,
                        user_id = %member.user_id,
                        "Skipped permission changed notification because actor username lookup failed"
                    );
                    return;
                }
            }
        } else {
            actor_username.to_string()
        };
        let Some(repo) = self.room_settings_repo.as_ref() else {
            tracing::warn!(
                room_id = %room_id,
                actor_id = %actor_id,
                user_id = %member.user_id,
                "Skipped permission changed notification because room settings repository is unavailable"
            );
            return;
        };
        let room_settings = match repo.get(room_id).await {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    actor_id = %actor_id,
                    user_id = %member.user_id,
                    "Skipped permission changed notification because room settings lookup failed"
                );
                return;
            }
        };
        let effective_permissions = self
            .permission_service
            .effective_member_permissions(member, &room_settings)
            .0;

        let subscriber_count = self.notification_service.notify_permission_changed(
            room_id,
            PermissionChangedNotification {
                user_id: &member.user_id,
                role: i32::from(member.role),
                effective_permissions,
                added_permissions: member.added_permissions,
                removed_permissions: member.removed_permissions,
                admin_added_permissions: member.admin_added_permissions,
                admin_removed_permissions: member.admin_removed_permissions,
                updated_by_user_id: actor_id,
                updated_by_username: &resolved_actor_username,
            },
        );
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                user_id = %member.user_id,
                "Permission changed event had no local subscribers"
            );
        }
    }

    async fn lookup_username(&self, user_id: &UserId) -> Result<String> {
        UserRepository::new(self.member_repo.pool().clone())
            .get_by_id(user_id)
            .await?
            .map(|user| user.username)
            .ok_or_else(|| Error::NotFound("Actor user not found".to_string()))
    }

    /// Log an audit event if the audit service is configured.
    /// Failures are logged as warnings but never propagated.
    ///
    /// The `actor_username` is passed from the caller to avoid a separate DB
    /// lookup. Pass an empty string if the username is not available (e.g., in
    /// background tasks).
    async fn audit_log(
        &self,
        actor_id: &UserId,
        actor_username: &str,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: AuditDetails,
    ) {
        if let Some(ref audit) = self.audit_service {
            if let Err(e) = audit
                .log(AuditEventParams {
                    actor_id: actor_id.to_string(),
                    actor_username: actor_username.to_string(),
                    action,
                    target_type,
                    target_id,
                    details,
                    ip_address: None,
                    user_agent: None,
                })
                .await
            {
                tracing::warn!(error = %e, "Failed to write audit log from MemberService");
            }
        }
    }

    /// Add a user as a member to a room (with default options)
    ///
    /// This is a convenience method that uses default options.
    pub async fn add_member(
        &self,
        room_id: RoomId,
        user_id: UserId,
        role: RoomRole,
    ) -> Result<RoomMember> {
        self.add_member_with_options(room_id, user_id, role, AddMemberOptions::new())
            .await
    }

    /// Add a user as a member to a room with custom options
    ///
    /// This method uses a database transaction to perform all checks and the insert atomically.
    pub async fn add_member_with_options(
        &self,
        room_id: RoomId,
        user_id: UserId,
        role: RoomRole,
        mut options: AddMemberOptions,
    ) -> Result<RoomMember> {
        // Get room settings and apply to options if max_members check is enabled
        if options.check_max_members {
            let settings_repo = self.room_settings_repo.as_ref().ok_or_else(|| {
                Error::Internal(
                    "MemberService is missing room_settings_repo for member capacity checks"
                        .to_string(),
                )
            })?;
            let room_settings = settings_repo.get(&room_id).await?;

            options.max_members = room_settings.max_members.0;
        }

        let member = RoomMember::new(room_id, user_id, role);

        // Add member with options (transaction happens in repository)
        let created_member = self.member_repo.add_with_options(&member, &options).await?;

        // Invalidate permission cache (outside transaction)
        if options.invalidate_cache {
            self.permission_service
                .seed_added_member_cache(&room_id, &user_id, created_member.version)
                .await;
        }

        Ok(created_member)
    }

    /// Remove a user from all rooms.
    ///
    /// Used during user deletion to clean up all room memberships.
    /// Returns the number of memberships removed.
    pub async fn remove_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        self.member_repo.remove_all_for_user(user_id).await
    }

    /// Delete the active membership row for an internal room lifecycle operation.
    ///
    /// This is not a member-management API. Product-level exits are modeled as
    /// `leave_room` or `kick_member`; this helper only performs the shared
    /// physical row deletion and cache invalidation.
    pub async fn delete_active_membership(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        let mut tx = self.member_repo.pool().begin().await?;
        let Some(observed_version) = self
            .member_repo
            .active_member_version_for_update_with_executor(&room_id, &user_id, &mut tx)
            .await?
        else {
            return Err(Error::NotFound(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        };
        let fence = self
            .permission_service
            .begin_permission_write(&room_id, &user_id, observed_version)
            .await?;
        let removed_version = match self
            .member_repo
            .remove_with_version_executor(&room_id, &user_id, &mut tx)
            .await
        {
            Ok(version) => version,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let Some(removed_version) = removed_version else {
            self.abort_permission_write(&fence).await;
            return Err(Error::NotFound(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        };
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            &fence,
            &room_id,
            &user_id,
            removed_version,
            "delete_active_membership",
        )
        .await;

        self.permission_service
            .invalidate_removed_member_cache(&room_id, &user_id)
            .await;

        Ok(())
    }

    /// Get all members of a room with user info
    pub async fn list_members(&self, room_id: &RoomId) -> Result<Vec<RoomMemberWithUser>> {
        self.member_repo.list_by_room(room_id).await
    }

    /// Get members of a room with database-level pagination
    ///
    /// Returns the requested page and the total matching member count.
    ///
    /// # Performance
    ///
    /// This method should be used instead of `list_members` + in-memory pagination
    /// when dealing with rooms that may have many members. It only loads the
    /// requested page from the database.
    pub async fn list_members_paginated(
        &self,
        room_id: &RoomId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomMemberWithUser>, i64)> {
        pagination.validate()?;
        self.member_repo
            .list_by_room_paginated(room_id, pagination)
            .await
    }

    pub async fn list_members_query(
        &self,
        room_id: &RoomId,
        query: crate::models::RoomMemberListQuery,
    ) -> Result<(Vec<RoomMemberWithUser>, i64)> {
        query.pagination.validate()?;
        self.member_repo.list_by_room_query(room_id, &query).await
    }

    /// Get member count for a room
    pub async fn count_members(&self, room_id: &RoomId) -> Result<i32> {
        self.member_repo.count_by_room(room_id).await
    }

    /// Get member counts for multiple rooms in a single query.
    pub async fn count_members_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<std::collections::HashMap<RoomId, i32>> {
        self.member_repo.count_by_rooms_batch(room_ids).await
    }

    /// Check if a user is a member of a room
    pub async fn is_member(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        self.member_repo.is_member(room_id, user_id).await
    }

    /// Check if a user is banned from a room
    pub async fn is_in_kick_cooldown(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        self.member_repo.is_in_kick_cooldown(room_id, user_id).await
    }

    /// Get a specific member
    pub async fn get_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<RoomMember>> {
        self.member_repo.get(room_id, user_id).await
    }

    /// List all rooms a user is a member of
    pub async fn list_user_rooms(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomId>, i64)> {
        pagination.validate()?;
        self.member_repo.list_by_user(user_id, pagination).await
    }

    /// List all rooms a user is a member of with full details
    pub async fn list_user_rooms_with_details(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        pagination.validate()?;
        self.member_repo
            .list_by_user_with_details(user_id, pagination)
            .await
    }

    /// List all rooms a user is related to through membership with query semantics.
    pub async fn list_user_rooms_with_details_query(
        &self,
        user_id: &UserId,
        query: &MyRoomListQuery,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        query.pagination.validate()?;
        self.member_repo
            .list_by_user_with_query(user_id, query)
            .await
    }

    /// Set member role (member/admin/creator)
    pub async fn set_member_role(
        &self,
        room_id: RoomId,
        creator_id: UserId,
        target_user_id: UserId,
        role: RoomRole,
    ) -> Result<RoomMember> {
        if role == RoomRole::Creator {
            return Err(Error::InvalidInput(
                "Creator role is bound to room ownership and cannot be assigned via set_member_role"
                    .to_string(),
            ));
        }

        // Check if user is creator (only creator can change roles)
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.created_by != creator_id {
            return Err(Error::Authorization(
                "Only room creator can change member roles".to_string(),
            ));
        }

        if target_user_id == room.created_by {
            return Err(Error::InvalidInput(
                "Cannot change the role of the room creator via set_member_role".to_string(),
            ));
        }
        let creator_username = self.lookup_username(&creator_id).await?;

        // Verify target is a member and update role with optimistic lock retry
        let (updated_member, old_role) = super::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Role update failed after maximum retry attempts",
            || async {
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;

                let old_role = member.role;

                let updated = self
                    .apply_permission_write(
                        &room_id,
                        &target_user_id,
                        member.version,
                        |reserved_version| async move {
                            if reserved_version > 0 {
                                self.member_repo
                                    .update_role_with_exact_version(
                                        &room_id,
                                        &target_user_id,
                                        role,
                                        member.version,
                                        reserved_version,
                                    )
                                    .await
                            } else {
                                self.member_repo
                                    .update_role(&room_id, &target_user_id, role, member.version)
                                    .await
                            }
                        },
                    )
                    .await?;
                Ok((updated, old_role))
            },
        )
        .await?;

        // Invalidate permission cache (local)
        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;

        // Invalidate room settings cache to ensure fresh role default permissions
        // are used when recalculating the user's effective permissions.
        // This is necessary because the permission calculation depends on both
        // the member's role AND the room's role-specific permission settings.
        if let Some(ref invalidation) = self.cache_invalidation {
            if let Err(e) = invalidation.invalidate_room_settings(&room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    "Failed to broadcast room settings cache invalidation after role change"
                );
            }
        }

        // Audit log
        self.audit_log(
            &creator_id,
            &creator_username,
            AuditAction::MemberRoleUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            AuditDetails {
                room_id: Some(room_id.to_string()),
                old_role: Some(format!("{old_role:?}")),
                new_role: Some(format!("{role:?}")),
                ..Default::default()
            },
        )
        .await;

        Ok(updated_member)
    }

    /// List all members including inactive (left) (admin view)
    pub async fn list_members_all(
        &self,
        room_id: &RoomId,
        admin_id: UserId,
    ) -> Result<Vec<RoomMemberWithUser>> {
        // Check admin permission without cache - security-critical operation
        self.permission_service
            .check_permission_no_cache(
                &room_id.clone(),
                &admin_id,
                crate::models::RoomPermission::KICK_MEMBER,
            )
            .await?;

        // Get all current member rows.
        self.member_repo.list_by_room_all(room_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_role_to_i32_matches_wire_values() {
        assert_eq!(i32::from(RoomRole::Creator), 1);
        assert_eq!(i32::from(RoomRole::Admin), 2);
        assert_eq!(i32::from(RoomRole::Member), 3);
        assert_eq!(i32::from(RoomRole::Guest), 4);
    }
}
