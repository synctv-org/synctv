use crate::{
    models::{
        AuditAction, AuditTargetType, RoomAdminPermissionBits, RoomGuestPermissionBits, RoomId,
        RoomMember, RoomMemberPermissionBits, RoomRole, UserId,
    },
    service::{member::AdminMemberUpdate, optimistic_retry, permission::PermissionWriteFence},
    Error, Result,
};

use super::MemberService;
use std::future::Future;

impl MemberService {
    pub(super) const MAX_RETRIES: u32 = 3;
    pub(super) const PERMISSION_UPDATE_MAX_RETRIES: u32 = 8;
    pub(super) const BACKOFF_BASE_MS: u64 = 5;

    const fn uses_admin_overrides(role: RoomRole) -> bool {
        matches!(role, RoomRole::Admin)
    }

    fn validate_permission_bits_for_role(role: RoomRole, permissions: u64) -> Result<()> {
        match role {
            RoomRole::Creator | RoomRole::Admin => {
                Self::validate_admin_permission_bits(permissions)
            }
            RoomRole::Member => Self::validate_member_permission_bits(
                RoomMemberPermissionBits::from_permissions(permissions),
            )
            .and_then(|()| {
                if RoomMemberPermissionBits::to_permissions(
                    RoomMemberPermissionBits::from_permissions(permissions),
                ) == permissions
                {
                    Ok(())
                } else {
                    Err(Error::InvalidInput(
                        "Permission set includes bits outside the member permission bitspace"
                            .to_string(),
                    ))
                }
            }),
            RoomRole::Guest => Self::validate_guest_permission_bits(
                RoomGuestPermissionBits::from_permissions(permissions),
            )
            .and_then(|()| {
                if RoomGuestPermissionBits::to_permissions(
                    RoomGuestPermissionBits::from_permissions(permissions),
                ) == permissions
                {
                    Ok(())
                } else {
                    Err(Error::InvalidInput(
                        "Permission set includes bits outside the guest permission bitspace"
                            .to_string(),
                    ))
                }
            }),
        }
    }

    fn validate_override_bits_for_role(role: RoomRole, permissions: u64) -> Result<()> {
        match role {
            RoomRole::Creator | RoomRole::Admin => {
                Self::validate_admin_permission_bits(permissions)
            }
            RoomRole::Member => Self::validate_member_permission_bits(permissions),
            RoomRole::Guest => Self::validate_guest_permission_bits(permissions),
        }
    }

    fn validate_member_permission_bits(permissions: u64) -> Result<()> {
        if RoomMemberPermissionBits::includes_only_defined(permissions) {
            return Ok(());
        }

        Err(Error::InvalidInput(
            "Permission set includes bits outside the member permission bitspace".to_string(),
        ))
    }

    fn validate_guest_permission_bits(permissions: u64) -> Result<()> {
        if RoomGuestPermissionBits::includes_only_defined(permissions) {
            return Ok(());
        }

        Err(Error::InvalidInput(
            "Permission set includes bits outside the guest permission bitspace".to_string(),
        ))
    }

    fn validate_admin_permission_bits(permissions: u64) -> Result<()> {
        if RoomAdminPermissionBits::includes_only_defined(permissions) {
            return Ok(());
        }

        Err(Error::InvalidInput(
            "Permission set includes bits outside the admin permission bitspace".to_string(),
        ))
    }

    fn override_bits_from_permissions(role: RoomRole, permissions: u64) -> u64 {
        role.override_bits_from_permissions(permissions)
    }

    fn override_pair_from_permissions(
        role: RoomRole,
        added_permissions: u64,
        removed_permissions: u64,
    ) -> (u64, u64) {
        (
            role.override_bits_from_permissions(added_permissions),
            role.override_bits_from_permissions(removed_permissions),
        )
    }

    pub(super) async fn finalize_committed_permission_write_best_effort(
        &self,
        fence: &PermissionWriteFence,
        room_id: &RoomId,
        user_id: &UserId,
        version: i64,
        operation: &'static str,
    ) {
        if let Err(error) = self
            .permission_service
            .commit_permission_write(fence, version)
            .await
        {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                user_id = %user_id,
                version,
                operation,
                "Failed to finalize permission fence after committed member write"
            );
        }
    }

    pub(super) async fn abort_permission_write(&self, fence: &PermissionWriteFence) {
        self.permission_service.abort_permission_write(fence).await;
    }

    pub(super) async fn apply_permission_write<F, Fut>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        db_version: i64,
        update: F,
    ) -> Result<RoomMember>
    where
        F: FnOnce(i64) -> Fut,
        Fut: Future<Output = Result<RoomMember>>,
    {
        let fence = self
            .permission_service
            .begin_permission_write(room_id, user_id, db_version)
            .await?;
        match update(fence.version()).await {
            Ok(updated) => {
                self.finalize_committed_permission_write_best_effort(
                    &fence,
                    room_id,
                    user_id,
                    updated.version,
                    "apply_permission_write",
                )
                .await;
                Ok(updated)
            }
            Err(error) => {
                self.abort_permission_write(&fence).await;
                Err(error)
            }
        }
    }

    pub async fn set_member_permissions(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        added_permissions: u64,
        removed_permissions: u64,
    ) -> Result<RoomMember> {
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &granter_id,
                crate::models::RoomPermission::SET_MEMBER_PERMISSIONS,
            )
            .await?;
        let granter_username = self.lookup_username(&granter_id).await?;

        let updated_member = optimistic_retry::retry_with_optimistic_lock(
            Self::PERMISSION_UPDATE_MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Permission update failed after maximum retry attempts",
            || async {
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;
                let (stored_added_permissions, stored_removed_permissions) =
                    Self::override_pair_from_permissions(
                        member.role,
                        added_permissions,
                        removed_permissions,
                    );
                Self::validate_permission_bits_for_role(member.role, added_permissions)?;
                Self::validate_permission_bits_for_role(member.role, removed_permissions)?;
                self.apply_permission_write(
                    &room_id,
                    &target_user_id,
                    member.version,
                    |reserved_version| async move {
                        if Self::uses_admin_overrides(member.role) {
                            if reserved_version > 0 {
                                self.member_repo
                                    .update_admin_permissions_with_exact_version(
                                        &room_id,
                                        &target_user_id,
                                        stored_added_permissions,
                                        stored_removed_permissions,
                                        member.version,
                                        reserved_version,
                                    )
                                    .await
                            } else {
                                self.member_repo
                                    .update_admin_permissions(
                                        &room_id,
                                        &target_user_id,
                                        stored_added_permissions,
                                        stored_removed_permissions,
                                        member.version,
                                    )
                                    .await
                            }
                        } else if reserved_version > 0 {
                            self.member_repo
                                .update_permissions_with_exact_version(
                                    &room_id,
                                    &target_user_id,
                                    stored_added_permissions,
                                    stored_removed_permissions,
                                    member.version,
                                    reserved_version,
                                )
                                .await
                        } else {
                            self.member_repo
                                .update_permissions(
                                    &room_id,
                                    &target_user_id,
                                    stored_added_permissions,
                                    stored_removed_permissions,
                                    member.version,
                                )
                                .await
                        }
                    },
                )
                .await
            },
        )
        .await?;

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;

        self.audit_log(
            &granter_id,
            &granter_username,
            AuditAction::MemberPermissionUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "added_permissions": added_permissions,
                "removed_permissions": removed_permissions,
            }),
        )
        .await;

        self.notify_permission_changed(&room_id, &granter_id, &granter_username, &updated_member)
            .await;

        Ok(updated_member)
    }

    pub async fn admin_update_member(&self, update: AdminMemberUpdate) -> Result<RoomMember> {
        let AdminMemberUpdate {
            room_id,
            actor_id,
            actor_username,
            target_user_id,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = update;
        Self::validate_admin_permission_bits(admin_added_permissions)?;
        Self::validate_admin_permission_bits(admin_removed_permissions)?;

        let has_permission_changes = added_permissions > 0
            || removed_permissions > 0
            || admin_added_permissions > 0
            || admin_removed_permissions > 0;

        let target_member = self
            .member_repo
            .get(&room_id, &target_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;

        let effective_role = role.unwrap_or(target_member.role);
        let effective_is_admin = matches!(effective_role, RoomRole::Admin);
        Self::validate_override_bits_for_role(effective_role, added_permissions)?;
        Self::validate_override_bits_for_role(effective_role, removed_permissions)?;

        if has_permission_changes {
            if effective_is_admin && (added_permissions > 0 || removed_permissions > 0) {
                return Err(Error::Authorization(
                    "Admin members must use admin_added_permissions/admin_removed_permissions"
                        .to_string(),
                ));
            }
            if !effective_is_admin && (admin_added_permissions > 0 || admin_removed_permissions > 0)
            {
                return Err(Error::Authorization(
                    "Only admin members use admin_added_permissions/admin_removed_permissions"
                        .to_string(),
                ));
            }
        }

        if let Some(new_role) = role {
            if new_role == RoomRole::Creator {
                return Err(Error::InvalidInput(
                    "Creator role is bound to room ownership and cannot be assigned via set_member_role"
                        .to_string(),
                ));
            }

            let room = self
                .room_repo
                .get_by_id(&room_id)
                .await?
                .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

            if target_user_id == room.created_by {
                return Err(Error::InvalidInput(
                    "Cannot change the role of the room creator via set_member_role".to_string(),
                ));
            }

            let updated_member = optimistic_retry::retry_with_optimistic_lock(
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
                    self.apply_permission_write(
                        &room_id,
                        &target_user_id,
                        member.version,
                        |reserved_version| async move {
                            if reserved_version > 0 {
                                self.member_repo
                                    .update_role_with_exact_version(
                                        &room_id,
                                        &target_user_id,
                                        new_role,
                                        member.version,
                                        reserved_version,
                                    )
                                    .await
                            } else {
                                self.member_repo
                                    .update_role(
                                        &room_id,
                                        &target_user_id,
                                        new_role,
                                        member.version,
                                    )
                                    .await
                            }
                        },
                    )
                    .await
                },
            )
            .await?;

            self.permission_service
                .invalidate_committed_member_write_cache(&room_id, &target_user_id)
                .await;

            if let Some(ref invalidation) = self.cache_invalidation {
                if let Err(error) = invalidation.invalidate_room_settings(&room_id).await {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        "Failed to broadcast room settings cache invalidation after admin role change"
                    );
                }
            }

            self.audit_log(
                &actor_id,
                &actor_username,
                AuditAction::MemberRoleUpdated,
                AuditTargetType::Member,
                Some(target_user_id.to_string()),
                serde_json::json!({
                    "room_id": room_id,
                    "role": new_role.to_string(),
                    "mode": "admin_override",
                }),
            )
            .await;

            if !has_permission_changes {
                self.notify_permission_changed(
                    &room_id,
                    &actor_id,
                    &actor_username,
                    &updated_member,
                )
                .await;
                return Ok(updated_member);
            }
        }

        let updated_member = optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Permission update failed after maximum retry attempts",
            || async {
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;
                self.apply_permission_write(
                    &room_id,
                    &target_user_id,
                    member.version,
                    |reserved_version| async move {
                        if Self::uses_admin_overrides(member.role) {
                            if reserved_version > 0 {
                                self.member_repo
                                    .update_admin_permissions_with_exact_version(
                                        &room_id,
                                        &target_user_id,
                                        admin_added_permissions,
                                        admin_removed_permissions,
                                        member.version,
                                        reserved_version,
                                    )
                                    .await
                            } else {
                                self.member_repo
                                    .update_admin_permissions(
                                        &room_id,
                                        &target_user_id,
                                        admin_added_permissions,
                                        admin_removed_permissions,
                                        member.version,
                                    )
                                    .await
                            }
                        } else if reserved_version > 0 {
                            self.member_repo
                                .update_permissions_with_exact_version(
                                    &room_id,
                                    &target_user_id,
                                    added_permissions,
                                    removed_permissions,
                                    member.version,
                                    reserved_version,
                                )
                                .await
                        } else {
                            self.member_repo
                                .update_permissions(
                                    &room_id,
                                    &target_user_id,
                                    added_permissions,
                                    removed_permissions,
                                    member.version,
                                )
                                .await
                        }
                    },
                )
                .await
            },
        )
        .await?;

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;

        self.audit_log(
            &actor_id,
            &actor_username,
            AuditAction::MemberPermissionUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "added_permissions": if effective_is_admin { admin_added_permissions } else { added_permissions },
                "removed_permissions": if effective_is_admin { admin_removed_permissions } else { removed_permissions },
                "mode": "admin_override",
            }),
        )
        .await;

        self.notify_permission_changed(&room_id, &actor_id, &actor_username, &updated_member)
            .await;

        Ok(updated_member)
    }

    pub async fn grant_permission(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &granter_id,
                crate::models::RoomPermission::SET_MEMBER_PERMISSIONS,
            )
            .await?;
        let granter_username = self.lookup_username(&granter_id).await?;

        let updated_member = optimistic_retry::retry_with_optimistic_lock(
            Self::PERMISSION_UPDATE_MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Permission grant failed after maximum retry attempts",
            || async {
                let target_member = self.member_repo.get(&room_id, &target_user_id).await?;
                let target_member = target_member.ok_or_else(|| {
                    Error::NotFound("User is not a member of this room".to_string())
                })?;
                let stored_permission =
                    Self::override_bits_from_permissions(target_member.role, permission);
                Self::validate_permission_bits_for_role(target_member.role, permission)?;
                self.apply_permission_write(
                    &room_id,
                    &target_user_id,
                    target_member.version,
                    |reserved_version| async move {
                        if Self::uses_admin_overrides(target_member.role) {
                            if reserved_version > 0 {
                                self.member_repo
                                    .grant_admin_permission_atomic_for_role_with_exact_version(
                                        &room_id,
                                        &target_user_id,
                                        stored_permission,
                                        target_member.role,
                                        target_member.version,
                                        reserved_version,
                                    )
                                    .await
                            } else {
                                self.member_repo
                                    .grant_admin_permission_atomic_for_role(
                                        &room_id,
                                        &target_user_id,
                                        stored_permission,
                                        target_member.role,
                                    )
                                    .await
                            }
                        } else if reserved_version > 0 {
                            self.member_repo
                                .grant_permission_atomic_for_role_with_exact_version(
                                    &room_id,
                                    &target_user_id,
                                    stored_permission,
                                    target_member.role,
                                    target_member.version,
                                    reserved_version,
                                )
                                .await
                        } else {
                            self.member_repo
                                .grant_permission_atomic_for_role(
                                    &room_id,
                                    &target_user_id,
                                    stored_permission,
                                    target_member.role,
                                )
                                .await
                        }
                    },
                )
                .await
            },
        )
        .await?;

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;

        self.audit_log(
            &granter_id,
            &granter_username,
            AuditAction::PermissionGranted,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "permission": permission,
            }),
        )
        .await;

        Ok(updated_member)
    }

    pub async fn revoke_permission(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &granter_id,
                crate::models::RoomPermission::SET_MEMBER_PERMISSIONS,
            )
            .await?;
        let granter_username = self.lookup_username(&granter_id).await?;

        let updated_member = optimistic_retry::retry_with_optimistic_lock(
            Self::PERMISSION_UPDATE_MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Permission revoke failed after maximum retry attempts",
            || async {
                let target_member = self.member_repo.get(&room_id, &target_user_id).await?;
                let target_member = target_member.ok_or_else(|| {
                    Error::NotFound("User is not a member of this room".to_string())
                })?;
                let stored_permission =
                    Self::override_bits_from_permissions(target_member.role, permission);
                Self::validate_permission_bits_for_role(target_member.role, permission)?;
                self.apply_permission_write(
                    &room_id,
                    &target_user_id,
                    target_member.version,
                    |reserved_version| async move {
                        if Self::uses_admin_overrides(target_member.role) {
                            if reserved_version > 0 {
                                self.member_repo
                                    .revoke_admin_permission_atomic_for_role_with_exact_version(
                                        &room_id,
                                        &target_user_id,
                                        stored_permission,
                                        target_member.role,
                                        target_member.version,
                                        reserved_version,
                                    )
                                    .await
                            } else {
                                self.member_repo
                                    .revoke_admin_permission_atomic_for_role(
                                        &room_id,
                                        &target_user_id,
                                        stored_permission,
                                        target_member.role,
                                    )
                                    .await
                            }
                        } else if reserved_version > 0 {
                            self.member_repo
                                .revoke_permission_atomic_for_role_with_exact_version(
                                    &room_id,
                                    &target_user_id,
                                    stored_permission,
                                    target_member.role,
                                    target_member.version,
                                    reserved_version,
                                )
                                .await
                        } else {
                            self.member_repo
                                .revoke_permission_atomic_for_role(
                                    &room_id,
                                    &target_user_id,
                                    stored_permission,
                                    target_member.role,
                                )
                                .await
                        }
                    },
                )
                .await
            },
        )
        .await?;

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;

        self.audit_log(
            &granter_id,
            &granter_username,
            AuditAction::PermissionRevoked,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "permission": permission,
            }),
        )
        .await;

        Ok(updated_member)
    }

    pub async fn reset_member_permissions(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
    ) -> Result<RoomMember> {
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &granter_id,
                crate::models::RoomPermission::SET_MEMBER_PERMISSIONS,
            )
            .await?;

        let updated_member = optimistic_retry::retry_with_optimistic_lock(
            Self::PERMISSION_UPDATE_MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Permission reset failed after maximum retry attempts",
            || async {
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;
                self.apply_permission_write(
                    &room_id,
                    &target_user_id,
                    member.version,
                    |reserved_version| async move {
                        if reserved_version > 0 {
                            self.member_repo
                                .reset_permissions_with_exact_version(
                                    &room_id,
                                    &target_user_id,
                                    member.version,
                                    reserved_version,
                                )
                                .await
                        } else {
                            self.member_repo
                                .reset_permissions(&room_id, &target_user_id, member.version)
                                .await
                        }
                    },
                )
                .await
            },
        )
        .await?;

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;

        Ok(updated_member)
    }
}
