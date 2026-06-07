use chrono::{Duration, Utc};

use crate::{
    models::{Room, RoomAdminPermissionBits, RoomId, RoomMember, RoomRole, UserId},
    repository::room_member::{
        KickCooldownInsert, MemberPermissionExactVersionUpdate,
        MemberRolePermissionExactVersionUpdate,
    },
    service::{member::AdminMemberUpdate, optimistic_retry, permission::PermissionWriteFence},
    Error, Result,
};

use super::{
    cleanup_member_resources_in_tx, ensure_actor_has_room_permission_now_tx,
    validate_kick_cooldown_seconds, KickMemberOutboxOptions, MemberPermissionPatch,
    RealtimeOutboxPermissionChangedEventFactory, RoomService, UpdateMemberWithOutboxRequest,
};

impl RoomService {
    /// Grant permission to user
    pub async fn grant_permission(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        permission: u64,
    ) -> Result<crate::models::RoomMember> {
        self.member_service
            .grant_permission(room_id, granter_id, target_user_id, permission)
            .await
    }

    /// Update member permissions (Allow/Deny pattern)
    ///
    /// This method sets both `added_permissions` and `removed_permissions`.
    /// To reset to role default, pass 0 for both.
    pub async fn set_member_permission(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        added_permissions: u64,
        removed_permissions: u64,
    ) -> Result<crate::models::RoomMember> {
        self.set_member_permission_with_outbox(
            room_id,
            granter_id,
            target_user_id,
            added_permissions,
            removed_permissions,
            None,
        )
        .await
    }

    pub async fn set_member_permission_with_outbox(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        added_permissions: u64,
        removed_permissions: u64,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<crate::models::RoomMember> {
        let updated_member = optimistic_retry::retry_with_optimistic_lock(
            3,
            5,
            "Permission update failed after maximum retry attempts",
            || async {
                let mut tx = self.pool.begin().await?;
                ensure_actor_has_room_permission_now_tx(
                    &mut tx,
                    &self.permission_service,
                    &room_id,
                    &granter_id,
                    crate::models::RoomPermission::SET_MEMBER_PERMISSIONS,
                )
                .await?;
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;
                Self::validate_override_bits_for_role(
                    member.role,
                    added_permissions,
                    removed_permissions,
                )?;
                let fence = self
                    .begin_permission_write(&room_id, &target_user_id, member.version)
                    .await?;
                let reserved_version = fence.version();
                let updated = if matches!(member.role, RoomRole::Admin) {
                    if reserved_version > 0 {
                        match self
                            .member_repo
                            .update_admin_permissions_with_exact_version_executor(
                                MemberPermissionExactVersionUpdate {
                                    room_id: &room_id,
                                    user_id: &target_user_id,
                                    added_permissions,
                                    removed_permissions,
                                    current_version: member.version,
                                    new_version: reserved_version,
                                },
                                &mut *tx,
                            )
                            .await
                        {
                            Ok(updated) => updated,
                            Err(error) => {
                                self.abort_permission_write(&fence).await;
                                return Err(error);
                            }
                        }
                    } else {
                        self.member_repo
                            .update_admin_permissions_with_executor(
                                &room_id,
                                &target_user_id,
                                added_permissions,
                                removed_permissions,
                                member.version,
                                &mut *tx,
                            )
                            .await?
                    }
                } else if reserved_version > 0 {
                    match self
                        .member_repo
                        .update_permissions_with_exact_version_executor(
                            MemberPermissionExactVersionUpdate {
                                room_id: &room_id,
                                user_id: &target_user_id,
                                added_permissions,
                                removed_permissions,
                                current_version: member.version,
                                new_version: reserved_version,
                            },
                            &mut *tx,
                        )
                        .await
                    {
                        Ok(updated) => updated,
                        Err(error) => {
                            self.abort_permission_write(&fence).await;
                            return Err(error);
                        }
                    }
                } else {
                    self.member_repo
                        .update_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            added_permissions,
                            removed_permissions,
                            member.version,
                            &mut *tx,
                        )
                        .await?
                };
                let snapshot = match self
                    .permission_changed_snapshot_tx(
                        &mut tx,
                        room_id,
                        target_user_id,
                        granter_id,
                        Some(&updated),
                    )
                    .await
                {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.abort_permission_write(&fence).await;
                        return Err(error);
                    }
                };
                if let Err(error) = self
                    .insert_permission_changed_outbox_tx(
                        &mut tx,
                        &snapshot,
                        outbox_event_factory.as_ref(),
                    )
                    .await
                {
                    self.abort_permission_write(&fence).await;
                    return Err(error);
                }
                if let Err(error) = tx.commit().await {
                    self.abort_permission_write(&fence).await;
                    return Err(error.into());
                }
                self.finalize_committed_permission_write_best_effort(
                    &fence,
                    &room_id,
                    &target_user_id,
                    updated.version,
                    "grant_member_permissions_with_outbox",
                )
                .await;
                Ok(updated)
            },
        )
        .await?;

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;

        Ok(updated_member)
    }

    pub async fn set_member_role_with_outbox(
        &self,
        room_id: RoomId,
        creator_id: UserId,
        target_user_id: UserId,
        role: RoomRole,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<crate::models::RoomMember> {
        if role == RoomRole::Creator {
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

        let mut tx = self.pool.begin().await?;
        let member = self
            .member_repo
            .get(&room_id, &target_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;
        let fence = self
            .begin_permission_write(&room_id, &target_user_id, member.version)
            .await?;
        let updated_member = if fence.version() > 0 {
            match self
                .member_repo
                .update_role_with_exact_version_executor(
                    &room_id,
                    &target_user_id,
                    role,
                    member.version,
                    fence.version(),
                    &mut *tx,
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.abort_permission_write(&fence).await;
                    return Err(error);
                }
            }
        } else {
            self.member_repo
                .update_role_with_version_executor(
                    &room_id,
                    &target_user_id,
                    role,
                    member.version,
                    &mut *tx,
                )
                .await?
        };
        let snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                creator_id,
                Some(&updated_member),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            &fence,
            &room_id,
            &target_user_id,
            updated_member.version,
            "set_member_role_with_outbox",
        )
        .await;

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;
        self.notify_room_settings_invalidation(&room_id).await;

        Ok(updated_member)
    }

    /// Kick member from room
    pub async fn kick_member(
        &self,
        room_id: RoomId,
        kicker_id: UserId,
        target_user_id: UserId,
        cooldown_seconds: i64,
    ) -> Result<()> {
        self.kick_member_with_outbox(
            room_id,
            kicker_id,
            target_user_id,
            cooldown_seconds,
            KickMemberOutboxOptions::default(),
        )
        .await
    }

    pub async fn kick_member_with_outbox(
        &self,
        room_id: RoomId,
        kicker_id: UserId,
        target_user_id: UserId,
        cooldown_seconds: i64,
        outbox: KickMemberOutboxOptions,
    ) -> Result<()> {
        validate_kick_cooldown_seconds(cooldown_seconds)?;
        if kicker_id == target_user_id {
            return Err(Error::InvalidInput("Cannot kick yourself".to_string()));
        }

        let mut tx = self.pool.begin().await?;
        ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &self.permission_service,
            &room_id,
            &kicker_id,
            crate::models::RoomPermission::KICK_MEMBER,
        )
        .await?;
        let Some(observed_version) = self
            .member_repo
            .active_member_version_for_update_with_executor(&room_id, &target_user_id, &mut tx)
            .await?
        else {
            return Err(Error::Authorization(
                "User is not a member or cannot kick a member with equal or higher role"
                    .to_string(),
            ));
        };
        let fence = self
            .begin_permission_write(&room_id, &target_user_id, observed_version)
            .await?;
        let removed_version = match self
            .member_repo
            .kick_with_role_check_with_executor(&room_id, &kicker_id, &target_user_id, &mut tx)
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
            return Err(Error::Authorization(
                "User is not a member or cannot kick a member with equal or higher role"
                    .to_string(),
            ));
        };
        let now = Utc::now();
        if let Err(error) = self
            .member_repo
            .add_kick_cooldown_with_executor(
                KickCooldownInsert {
                    room_id: &room_id,
                    user_id: &target_user_id,
                    kicked_by: Some(&kicker_id),
                    starts_at: now,
                    ends_at: now + Duration::seconds(cooldown_seconds),
                    reason: Some("kicked"),
                },
                &mut *tx,
            )
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        let cleanup = match cleanup_member_resources_in_tx(&mut tx, &room_id, &target_user_id).await
        {
            Ok(cleanup) => cleanup,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let cleanup_outbox_events = outbox
            .cleanup
            .as_ref()
            .map(|factory| factory(&cleanup))
            .transpose()?
            .unwrap_or_default();
        let snapshot = match self
            .permission_changed_snapshot_tx(&mut tx, room_id, target_user_id, kicker_id, None)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(
                &mut tx,
                &snapshot,
                outbox.permission_changed.as_ref(),
            )
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_realtime_outbox_tx(&mut tx, outbox.lifecycle.as_ref())
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_realtime_outbox_events_tx(&mut tx, &cleanup_outbox_events)
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            &fence,
            &room_id,
            &target_user_id,
            removed_version,
            "kick_member_with_outbox",
        )
        .await;

        self.permission_service
            .invalidate_removed_member_cache(&room_id, &target_user_id)
            .await;
        self.finalize_member_resource_cleanup_after_commit(&room_id, &target_user_id, &cleanup)
            .await;
        let subscriber_count = self
            .notification_service
            .notify_member_kicked(&room_id, &target_user_id);
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                user_id = %target_user_id,
                "Member kick event had no local subscribers"
            );
        }
        Ok(())
    }

    pub async fn admin_update_member_with_outbox(
        &self,
        update: AdminMemberUpdate,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<crate::models::RoomMember> {
        let AdminMemberUpdate {
            room_id,
            actor_id,
            actor_username: _,
            target_user_id,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = update;
        if !RoomAdminPermissionBits::includes_only_defined(admin_added_permissions)
            || !RoomAdminPermissionBits::includes_only_defined(admin_removed_permissions)
        {
            return Err(Error::InvalidInput(
                "Permission set includes bits outside the target role permission bitspace"
                    .to_string(),
            ));
        }

        let current = self
            .member_repo
            .get(&room_id, &target_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;
        let effective_role = role.unwrap_or(current.role);
        let effective_is_admin = matches!(effective_role, RoomRole::Admin);
        Self::validate_override_bits_for_role(
            effective_role,
            added_permissions,
            removed_permissions,
        )?;

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
        }

        if effective_is_admin && (added_permissions > 0 || removed_permissions > 0) {
            return Err(Error::Authorization(
                "Admin members must use admin_added_permissions/admin_removed_permissions"
                    .to_string(),
            ));
        }
        if !effective_is_admin && (admin_added_permissions > 0 || admin_removed_permissions > 0) {
            return Err(Error::Authorization(
                "Only admin members use admin_added_permissions/admin_removed_permissions"
                    .to_string(),
            ));
        }

        let mut tx = self.pool.begin().await?;
        let mut updated = current;
        let mut fence: Option<PermissionWriteFence> = None;
        let has_permission_changes = added_permissions > 0
            || removed_permissions > 0
            || admin_added_permissions > 0
            || admin_removed_permissions > 0;
        let combine_role_and_permissions = role.is_some() && has_permission_changes;

        if let Some(new_role) = role.filter(|_| combine_role_and_permissions) {
            let write_fence = self
                .begin_permission_write(&room_id, &target_user_id, updated.version)
                .await?;
            updated = if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_role_and_permissions_with_exact_version_executor(
                        MemberRolePermissionExactVersionUpdate {
                            room_id: &room_id,
                            user_id: &target_user_id,
                            role: new_role,
                            added_permissions: if effective_is_admin {
                                admin_added_permissions
                            } else {
                                added_permissions
                            },
                            removed_permissions: if effective_is_admin {
                                admin_removed_permissions
                            } else {
                                removed_permissions
                            },
                            use_admin_permissions: effective_is_admin,
                            current_version: updated.version,
                            new_version: write_fence.version(),
                        },
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        self.abort_permission_write(&write_fence).await;
                        return Err(error);
                    }
                }
            } else {
                let updated_role = self
                    .member_repo
                    .update_role_with_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        &mut *tx,
                    )
                    .await?;
                if effective_is_admin {
                    self.member_repo
                        .update_admin_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            admin_added_permissions,
                            admin_removed_permissions,
                            updated_role.version,
                            &mut *tx,
                        )
                        .await?
                } else {
                    self.member_repo
                        .update_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            added_permissions,
                            removed_permissions,
                            updated_role.version,
                            &mut *tx,
                        )
                        .await?
                }
            };
            fence = Some(write_fence);
        } else if let Some(new_role) = role {
            let write_fence = self
                .begin_permission_write(&room_id, &target_user_id, updated.version)
                .await?;
            updated = if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_role_with_exact_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        write_fence.version(),
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        self.abort_permission_write(&write_fence).await;
                        return Err(error);
                    }
                }
            } else {
                self.member_repo
                    .update_role_with_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        &mut *tx,
                    )
                    .await?
            };
            fence = Some(write_fence);
        }

        if !combine_role_and_permissions && (has_permission_changes || role.is_none()) {
            if fence.is_none() {
                fence = Some(
                    self.begin_permission_write(&room_id, &target_user_id, updated.version)
                        .await?,
                );
            }
            let Some(write_fence) = fence.as_ref() else {
                return Err(Error::Internal(
                    "Permission update missing write fence".to_string(),
                ));
            };
            updated = if effective_is_admin {
                if write_fence.version() > 0 {
                    match self
                        .member_repo
                        .update_admin_permissions_with_exact_version_executor(
                            MemberPermissionExactVersionUpdate {
                                room_id: &room_id,
                                user_id: &target_user_id,
                                added_permissions: admin_added_permissions,
                                removed_permissions: admin_removed_permissions,
                                current_version: updated.version,
                                new_version: write_fence.version(),
                            },
                            &mut *tx,
                        )
                        .await
                    {
                        Ok(updated) => updated,
                        Err(error) => {
                            if let Some(fence) = &fence {
                                self.abort_permission_write(fence).await;
                            }
                            return Err(error);
                        }
                    }
                } else {
                    self.member_repo
                        .update_admin_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            admin_added_permissions,
                            admin_removed_permissions,
                            updated.version,
                            &mut *tx,
                        )
                        .await?
                }
            } else if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_permissions_with_exact_version_executor(
                        MemberPermissionExactVersionUpdate {
                            room_id: &room_id,
                            user_id: &target_user_id,
                            added_permissions,
                            removed_permissions,
                            current_version: updated.version,
                            new_version: write_fence.version(),
                        },
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        if let Some(fence) = &fence {
                            self.abort_permission_write(fence).await;
                        }
                        return Err(error);
                    }
                }
            } else {
                self.member_repo
                    .update_permissions_with_executor(
                        &room_id,
                        &target_user_id,
                        added_permissions,
                        removed_permissions,
                        updated.version,
                        &mut *tx,
                    )
                    .await?
            };
        }

        let snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&updated),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Some(fence) = &fence {
                    self.abort_permission_write(fence).await;
                }
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await
        {
            if let Some(fence) = &fence {
                self.abort_permission_write(fence).await;
            }
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            if let Some(fence) = &fence {
                self.abort_permission_write(fence).await;
            }
            return Err(error.into());
        }
        if let Some(fence) = &fence {
            self.finalize_committed_permission_write_best_effort(
                fence,
                &room_id,
                &target_user_id,
                updated.version,
                "admin_update_member_with_outbox",
            )
            .await;
        }

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;
        if role.is_some() {
            self.notify_room_settings_invalidation(&room_id).await;
        }
        Ok(updated)
    }

    pub async fn update_member_with_outbox(
        &self,
        request: UpdateMemberWithOutboxRequest,
    ) -> Result<crate::models::RoomMember> {
        let UpdateMemberWithOutboxRequest {
            room_id,
            actor_id,
            target_user_id,
            role,
            permissions,
            outbox_event_factory,
        } = request;
        let MemberPermissionPatch {
            apply_permission_update,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = permissions;

        if !RoomAdminPermissionBits::includes_only_defined(admin_added_permissions)
            || !RoomAdminPermissionBits::includes_only_defined(admin_removed_permissions)
        {
            return Err(Error::InvalidInput(
                "Permission set includes bits outside the target role permission bitspace"
                    .to_string(),
            ));
        }

        let current = self
            .member_repo
            .get(&room_id, &target_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;
        let effective_role = role.unwrap_or(current.role);
        let effective_is_admin = matches!(effective_role, RoomRole::Admin);
        if apply_permission_update {
            Self::validate_override_bits_for_role(
                effective_role,
                added_permissions,
                removed_permissions,
            )?;
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

            if room.created_by != actor_id {
                return Err(Error::Authorization(
                    "Only room creator can change member roles".to_string(),
                ));
            }

            if target_user_id == room.created_by {
                return Err(Error::InvalidInput(
                    "Cannot change the role of the room creator via set_member_role".to_string(),
                ));
            }
        }

        if apply_permission_update {
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

        let mut tx = self.pool.begin().await?;
        if apply_permission_update {
            ensure_actor_has_room_permission_now_tx(
                &mut tx,
                &self.permission_service,
                &room_id,
                &actor_id,
                crate::models::RoomPermission::SET_MEMBER_PERMISSIONS,
            )
            .await?;
        }
        let mut updated = current;
        let mut fence: Option<PermissionWriteFence> = None;
        let combine_role_and_permissions = role.is_some() && apply_permission_update;

        if let Some(new_role) = role.filter(|_| combine_role_and_permissions) {
            let write_fence = self
                .begin_permission_write(&room_id, &target_user_id, updated.version)
                .await?;
            updated = if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_role_and_permissions_with_exact_version_executor(
                        MemberRolePermissionExactVersionUpdate {
                            room_id: &room_id,
                            user_id: &target_user_id,
                            role: new_role,
                            added_permissions: if effective_is_admin {
                                admin_added_permissions
                            } else {
                                added_permissions
                            },
                            removed_permissions: if effective_is_admin {
                                admin_removed_permissions
                            } else {
                                removed_permissions
                            },
                            use_admin_permissions: effective_is_admin,
                            current_version: updated.version,
                            new_version: write_fence.version(),
                        },
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        self.abort_permission_write(&write_fence).await;
                        return Err(error);
                    }
                }
            } else {
                let updated_role = self
                    .member_repo
                    .update_role_with_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        &mut *tx,
                    )
                    .await?;
                if effective_is_admin {
                    self.member_repo
                        .update_admin_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            admin_added_permissions,
                            admin_removed_permissions,
                            updated_role.version,
                            &mut *tx,
                        )
                        .await?
                } else {
                    self.member_repo
                        .update_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            added_permissions,
                            removed_permissions,
                            updated_role.version,
                            &mut *tx,
                        )
                        .await?
                }
            };
            fence = Some(write_fence);
        } else if let Some(new_role) = role {
            let write_fence = self
                .begin_permission_write(&room_id, &target_user_id, updated.version)
                .await?;
            updated = if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_role_with_exact_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        write_fence.version(),
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        self.abort_permission_write(&write_fence).await;
                        return Err(error);
                    }
                }
            } else {
                self.member_repo
                    .update_role_with_version_executor(
                        &room_id,
                        &target_user_id,
                        new_role,
                        updated.version,
                        &mut *tx,
                    )
                    .await?
            };
            fence = Some(write_fence);
        }

        if apply_permission_update && !combine_role_and_permissions {
            if fence.is_none() {
                fence = Some(
                    self.begin_permission_write(&room_id, &target_user_id, updated.version)
                        .await?,
                );
            }
            let Some(write_fence) = fence.as_ref() else {
                return Err(Error::Internal(
                    "Permission update missing write fence".to_string(),
                ));
            };
            updated = if effective_is_admin {
                if write_fence.version() > 0 {
                    match self
                        .member_repo
                        .update_admin_permissions_with_exact_version_executor(
                            MemberPermissionExactVersionUpdate {
                                room_id: &room_id,
                                user_id: &target_user_id,
                                added_permissions: admin_added_permissions,
                                removed_permissions: admin_removed_permissions,
                                current_version: updated.version,
                                new_version: write_fence.version(),
                            },
                            &mut *tx,
                        )
                        .await
                    {
                        Ok(updated) => updated,
                        Err(error) => {
                            if let Some(fence) = &fence {
                                self.abort_permission_write(fence).await;
                            }
                            return Err(error);
                        }
                    }
                } else {
                    self.member_repo
                        .update_admin_permissions_with_executor(
                            &room_id,
                            &target_user_id,
                            admin_added_permissions,
                            admin_removed_permissions,
                            updated.version,
                            &mut *tx,
                        )
                        .await?
                }
            } else if write_fence.version() > 0 {
                match self
                    .member_repo
                    .update_permissions_with_exact_version_executor(
                        MemberPermissionExactVersionUpdate {
                            room_id: &room_id,
                            user_id: &target_user_id,
                            added_permissions,
                            removed_permissions,
                            current_version: updated.version,
                            new_version: write_fence.version(),
                        },
                        &mut *tx,
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        if let Some(fence) = &fence {
                            self.abort_permission_write(fence).await;
                        }
                        return Err(error);
                    }
                }
            } else {
                self.member_repo
                    .update_permissions_with_executor(
                        &room_id,
                        &target_user_id,
                        added_permissions,
                        removed_permissions,
                        updated.version,
                        &mut *tx,
                    )
                    .await?
            };
        }

        let snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&updated),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Some(fence) = &fence {
                    self.abort_permission_write(fence).await;
                }
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await
        {
            if let Some(fence) = &fence {
                self.abort_permission_write(fence).await;
            }
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            if let Some(fence) = &fence {
                self.abort_permission_write(fence).await;
            }
            return Err(error.into());
        }
        if let Some(fence) = &fence {
            self.finalize_committed_permission_write_best_effort(
                fence,
                &room_id,
                &target_user_id,
                updated.version,
                "admin_set_member_role_with_outbox",
            )
            .await;
        }

        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &target_user_id)
            .await;
        if role.is_some() {
            self.notify_room_settings_invalidation(&room_id).await;
        }
        Ok(updated)
    }

    pub async fn admin_kick_member_with_outbox(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        target_user_id: UserId,
        cooldown_seconds: i64,
        persisted_kicked_by: Option<UserId>,
        outbox: KickMemberOutboxOptions,
    ) -> Result<()> {
        validate_kick_cooldown_seconds(cooldown_seconds)?;
        if actor_id == target_user_id {
            return Err(Error::InvalidInput("Cannot kick yourself".to_string()));
        }

        let mut tx = self.pool.begin().await?;
        let Some(observed_version) = self
            .member_repo
            .active_member_version_for_update_with_executor(&room_id, &target_user_id, &mut tx)
            .await?
        else {
            return Err(Error::NotFound(
                "User is not an active member of this room".to_string(),
            ));
        };
        let fence = self
            .begin_permission_write(&room_id, &target_user_id, observed_version)
            .await?;
        let removed_version = match self
            .member_repo
            .remove_with_version_executor(&room_id, &target_user_id, &mut tx)
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
                "User is not an active member of this room".to_string(),
            ));
        };
        let now = Utc::now();
        if let Err(error) = self
            .member_repo
            .add_kick_cooldown_with_executor(
                KickCooldownInsert {
                    room_id: &room_id,
                    user_id: &target_user_id,
                    kicked_by: persisted_kicked_by.as_ref(),
                    starts_at: now,
                    ends_at: now + Duration::seconds(cooldown_seconds),
                    reason: Some("kicked"),
                },
                &mut *tx,
            )
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        let cleanup = match cleanup_member_resources_in_tx(&mut tx, &room_id, &target_user_id).await
        {
            Ok(cleanup) => cleanup,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let cleanup_outbox_events = outbox
            .cleanup
            .as_ref()
            .map(|factory| factory(&cleanup))
            .transpose()?
            .unwrap_or_default();
        let snapshot = match self
            .permission_changed_snapshot_tx(&mut tx, room_id, target_user_id, actor_id, None)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(
                &mut tx,
                &snapshot,
                outbox.permission_changed.as_ref(),
            )
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_realtime_outbox_tx(&mut tx, outbox.lifecycle.as_ref())
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_realtime_outbox_events_tx(&mut tx, &cleanup_outbox_events)
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            &fence,
            &room_id,
            &target_user_id,
            removed_version,
            "admin_kick_member_with_outbox",
        )
        .await;

        self.permission_service
            .invalidate_removed_member_cache(&room_id, &target_user_id)
            .await;
        self.finalize_member_resource_cleanup_after_commit(&room_id, &target_user_id, &cleanup)
            .await;
        let subscriber_count = self
            .notification_service
            .notify_member_kicked(&room_id, &target_user_id);
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                user_id = %target_user_id,
                "Admin member kick event had no local subscribers"
            );
        }
        Ok(())
    }

    /// Update the room's `last_activity_at` timestamp.
    ///
    /// Call this after chat messages, playback state changes, or member
    /// joins/leaves to prevent active rooms from being expired by the TTL
    /// cleanup.
    pub async fn touch_room_activity(&self, room_id: RoomId) {
        if let Err(e) = self.room_repo.touch_activity(&room_id).await {
            tracing::debug!(error = %e, room_id = %room_id, "Failed to touch room activity");
        }
    }

    /// Get room members with user info
    pub async fn get_room_members(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<crate::models::RoomMemberWithUser>> {
        self.member_service.list_members(room_id).await
    }

    /// Get room members with database-level pagination
    ///
    /// Uses `COUNT(*) OVER()` for atomic count + fetch.
    /// Returns (members, total_count) tuple.
    ///
    /// # Performance
    ///
    /// This method should be preferred over `get_room_members` for admin endpoints
    /// where rooms may have large numbers of members.
    pub async fn get_room_members_paginated(
        &self,
        room_id: &RoomId,
        pagination: crate::models::PageParams,
    ) -> Result<(Vec<crate::models::RoomMemberWithUser>, i64)> {
        self.member_service
            .list_members_paginated(room_id, pagination)
            .await
    }

    pub async fn get_room_members_query(
        &self,
        room_id: &RoomId,
        query: crate::models::RoomMemberListQuery,
    ) -> Result<(Vec<crate::models::RoomMemberWithUser>, i64)> {
        self.member_service.list_members_query(room_id, query).await
    }

    /// Get member count for a room
    pub async fn get_member_count(&self, room_id: &RoomId) -> Result<i32> {
        self.member_service.count_members(room_id).await
    }

    /// Get member counts for multiple rooms in a single query.
    pub async fn get_member_count_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<std::collections::HashMap<RoomId, i32>> {
        self.member_service.count_members_batch(room_ids).await
    }

    /// Get a specific room member record.
    ///
    /// Returns `None` if the user is not (or is no longer) a member of the room.
    pub async fn get_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<RoomMember>> {
        self.member_service.get_member(room_id, user_id).await
    }

    /// Check if user is a member of the room
    pub async fn check_membership(&self, room_id: &RoomId, user_id: &UserId) -> Result<()> {
        let room = self.get_room(room_id).await?;
        self.check_membership_with_room(&room, user_id).await
    }

    pub async fn check_membership_with_room(&self, room: &Room, user_id: &UserId) -> Result<()> {
        self.ensure_room_creator_is_active_for_access(room, user_id)
            .await?;

        if self.member_service.is_member(&room.id, user_id).await? {
            Ok(())
        } else {
            Err(Error::Authorization(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ))
        }
    }
}
