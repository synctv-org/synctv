use std::sync::Arc;

use crate::{
    models::{
        AuditAction, AuditTargetType, PageParams, ReviewStatus, Room, RoomId, RoomMember,
        RoomPermission, RoomPermissionSet, RoomRole, RoomStatus, UserId,
    },
    repository::ReviewRepository,
    Error, Result,
};

use super::{
    creation::RoomCreationPolicy, permission_fence_guard::PermissionFenceGuard,
    soft_delete_room_and_cleanup_in_tx, NewRealtimeOutboxEvent, RoomService,
};

impl RoomService {
    /// Check if guests are allowed to access a room
    ///
    /// Validates guest access based on:
    /// 1. Global `enable_guest` setting
    /// 2. Room `allow_guest_join` setting
    /// 3. Room password requirement (guests blocked if password required)
    ///
    /// # Arguments
    /// * `room_id` - Room ID to check
    /// * `settings_registry` - Optional global settings registry (if None, guests are denied -- fail-closed)
    ///
    /// # Returns
    /// * `Ok(())` if guests are allowed
    /// * `Err` with appropriate error message if guests are not allowed
    pub async fn check_guest_allowed(
        &self,
        room_id: &RoomId,
        settings_registry: Option<&crate::service::SettingsRegistry>,
    ) -> Result<()> {
        // Check global enable_guest setting (fail-closed: deny when registry unavailable)
        if let Some(registry) = settings_registry {
            let enable_guest = registry.enable_guest.get()?;
            if !enable_guest {
                tracing::debug!(room_id = %room_id, "Guest access denied: global guest mode disabled");
                return Err(Error::Authorization(
                    "Guest mode is disabled globally".to_string(),
                ));
            }
        } else {
            tracing::debug!(room_id = %room_id, "Guest access denied: settings registry unavailable (fail-closed)");
            return Err(Error::Authorization(
                "Guest mode is not available".to_string(),
            ));
        }

        // Get room settings
        let room_settings = self.room_settings_repo.get(room_id).await?;

        // Check room-level allow_guest_join setting
        if !room_settings.allow_guest_join.0 {
            tracing::debug!(room_id = %room_id, "Guest access denied: room guest mode disabled");
            return Err(Error::Authorization(
                "Guest access is not allowed in this room".to_string(),
            ));
        }

        // Check if room has password (guests cannot join password-protected rooms)
        let password_enabled = self
            .room_password_repo
            .get_state(room_id)
            .await?
            .is_some_and(|state| state.enabled);
        if password_enabled {
            tracing::debug!(room_id = %room_id, "Guest access denied: room has password");
            return Err(Error::Authorization(
                "Guests cannot join password-protected rooms. Please create an account and join as a member.".to_string(),
            ));
        }

        tracing::debug!(room_id = %room_id, "Guest access allowed");
        Ok(())
    }

    /// Return the effective room permissions for guests.
    ///
    /// This is the single entry point for combining the global guest default
    /// permission set with room-level guest added/removed permissions.
    pub async fn get_guest_permissions(&self, room_id: &RoomId) -> Result<RoomPermissionSet> {
        let settings = self.get_room_settings(room_id).await?;
        Ok(self
            .permission_service
            .effective_permission_calculator()
            .role_default(&RoomRole::Guest, &settings))
    }

    /// Soft-delete a room.
    ///
    /// Sets the `deleted_at` timestamp on the room row. The room and its related
    /// data (members, playlists, media, chat messages, settings, playback state)
    /// remain in the database until the periodic `CleanupService` permanently
    /// purges rows whose `deleted_at` exceeds the configured retention period
    /// (default: 90 days). Permanent purge uses the same explicit cleanup path
    /// as normal room deletion before removing the room row itself.
    ///
    /// **Soft-delete lifecycle (optimized):**
    /// 1. This method sets `rooms.deleted_at = NOW()` (room becomes invisible to queries)
    /// 2. IMMEDIATELY deletes non-critical related data to free storage:
    /// - playlists and nested media via explicit subtree cleanup
    /// - `room_members`
    /// - `room_settings`
    /// - `room_playback_state`
    /// - `chat_messages`
    /// 3. Preserves only the room row (for audit) and `audit_logs` entries
    /// 4. `CleanupService::purge_soft_deleted_rooms()` eventually purges the room row
    ///    after `room_soft_delete_retention_days` (default: 90 days)
    ///
    /// Authorization model:
    /// - room creator can delete their own room
    /// - room members with DELETE_ROOM can delete the room
    /// - global admin/root can delete any room
    pub async fn delete_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        self.delete_room_with_outbox(room_id, user_id, None).await
    }

    pub async fn delete_room_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        outbox_event: Option<NewRealtimeOutboxEvent>,
    ) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "Soft-deleting room");

        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found or already deleted".to_string()))?;

        let actor = self.user_service.get_user(&user_id).await?;
        let is_global_admin = actor.role.is_admin_or_above();
        let is_creator = room.created_by == user_id;

        // Check authorization: creator, global admin, or member with delete permission
        if !is_creator && !is_global_admin {
            if self.member_repo.get(&room_id, &user_id).await?.is_none() {
                return Err(Error::Authorization(
                    "You are not a member of this room".to_string(),
                ));
            }
            self.permission_service
                .check_permission_no_cache(&room_id, &user_id, RoomPermission::DELETE_ROOM)
                .await?;
        }

        let mut tx = self.pool.begin().await?;
        let guard =
            PermissionFenceGuard::reserve(Arc::new(self.clone()), &room_id, &mut tx).await?;

        let impact = match soft_delete_room_and_cleanup_in_tx(&mut tx, &room_id).await {
            Ok(impact) => impact,
            Err(error) => {
                guard.abort().await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_realtime_outbox_tx(&mut tx, outbox_event.as_ref())
            .await
        {
            guard.abort().await;
            return Err(error);
        }

        // Commit transaction - all or nothing
        if let Err(error) = tx.commit().await {
            guard.abort().await;
            return Err(error.into());
        }

        if let Err(error) = guard.commit(&impact.removed_members).await {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                "Failed to finalize one or more room deletion permission fences after DB commit"
            );
        }

        self.invalidate_room_caches(&room_id).await;
        self.invalidate_removed_room_member_permission_caches(&impact.removed_members)
            .await;

        let subscriber_count = self.notification_service.notify_room_deleted(&room_id);
        super::outbox::log_if_no_local_subscribers(subscriber_count, &room_id, "Room deleted");

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            is_creator,
            is_global_admin,
            playlists_deleted = impact.deleted_playlist_ids.len(),
            media_deleted = impact.deleted_media_ids.len(),
            members_deleted = impact.members_deleted,
            settings_deleted = impact.settings_deleted,
            chat_deleted = impact.chat_deleted,
            "Room soft-deleted with immediate cleanup of related data (room row preserved for audit, will be purged by CleanupService after retention period)"
        );

        // Track room metrics
        crate::metrics::http::ROOMS_ACTIVE.dec();

        // Audit log (preserved - not deleted with room data)
        self.audit_log(
            &user_id,
            &actor.username,
            AuditAction::RoomDeleted,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({
                "reason": "Room deleted by user",
                "playlists_deleted": impact.deleted_playlist_ids.len(),
                "media_deleted": impact.deleted_media_ids.len(),
                "members_deleted": impact.members_deleted,
                "settings_deleted": impact.settings_deleted,
                "chat_deleted": impact.chat_deleted,
            }),
        )
        .await;

        Ok(())
    }

    /// Approve a pending room creation request and create the room.
    ///
    /// This is an admin-only operation for rooms created when `create_room_need_review=true`.
    /// After approval, the room becomes visible and usable by its creator.
    ///
    /// # Errors
    /// - `Error::NotFound` if the pending request does not exist
    /// - `Error::Authorization` if caller is not a global admin
    pub async fn approve_pending_room(
        &self,
        request_id: RoomId,
        admin_id: Option<&UserId>,
    ) -> Result<Room> {
        tracing::info!(request_id = %request_id, ?admin_id, "Approving room creation request");

        let admin_username = if let Some(admin_id) = admin_id {
            let admin = self.user_service.get_user(admin_id).await?;
            if !admin.role.is_admin_or_above() {
                return Err(Error::Authorization(
                    "Only admins can approve rooms".to_string(),
                ));
            }
            Some(admin.username)
        } else {
            None
        };

        let mut tx = self.pool.begin().await?;
        let request = Self::load_pending_room_creation_request_for_update(&request_id, &mut tx)
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "Pending room creation request {request_id} not found"
                ))
            })?;
        let audit_actor_username = match admin_username {
            Some(username) => username,
            None => Self::membership_snapshot_username_tx(&mut tx, &request.requested_by).await?,
        };

        self.ensure_user_can_create_room_now_tx(&mut tx, &request.requested_by)
            .await?;
        self.enforce_current_room_creation_policy(
            &request.requested_by,
            request.opaque_password_record.is_some(),
            RoomCreationPolicy {
                enforce_creation_toggle: true,
            },
        )?;
        self.enforce_room_ownership_limit_tx(&mut tx, &request.requested_by, None)
            .await?;
        self.ensure_room_name_available_for_creator_excluding_pending_tx(
            &mut tx,
            &request.requested_by,
            &request.name,
            Some(request_id),
        )
        .await?;

        let room = Room::new_with_description(
            request.name.clone(),
            request.description.clone(),
            request.requested_by,
        );
        let updated = self.room_repo.create_with_executor(&room, &mut *tx).await?;

        self.room_settings_repo
            .set_settings_with_executor(&updated.id, &request.settings, &mut *tx)
            .await?;
        if let Some(ref opaque_password_record) = request.opaque_password_record {
            self.room_password_repo
                .set_opaque_credential_with_executor(&updated.id, opaque_password_record, &mut *tx)
                .await?;
        }

        let member = RoomMember::new(updated.id, request.requested_by, RoomRole::Creator);
        self.member_repo.add_with_executor(&member, &mut tx).await?;
        self.playback_repo
            .create_or_get_with_executor(&updated.id, &mut tx)
            .await?;

        let approved = ReviewRepository::approve_room_creation_with_executor(
            &mut *tx,
            request_id,
            admin_id.copied(),
        )
        .await?;
        if approved == 0 {
            return Err(Error::NotFound(format!(
                "Pending room creation request {request_id} not found"
            )));
        }

        tx.commit().await?;

        crate::metrics::http::ROOMS_ACTIVE.inc();

        self.notify_room_invalidation(&updated.id).await;
        self.permission_service
            .invalidate_room_cache(&updated.id)
            .await;

        // Audit log
        self.audit_log(
            admin_id.unwrap_or(&request.requested_by),
            &audit_actor_username,
            AuditAction::RoomApproved,
            AuditTargetType::Room,
            Some(updated.id.to_string()),
            serde_json::json!({
                "request_id": request_id.to_string(),
                "previous_review_status": "pending",
                "new_review_status": "approved",
            }),
        )
        .await;

        tracing::info!(request_id = %request_id, room_id = %updated.id, ?admin_id, "Room approved and activated");

        Ok(updated)
    }

    /// Reject a pending room creation request.
    ///
    /// This is an admin-only operation for rooms created when `create_room_need_review=true`.
    /// Rejected requests are preserved for review/audit; no room row is created.
    ///
    /// # Errors
    /// - `Error::NotFound` if room doesn't exist
    /// - `Error::NotFound` if the pending request does not exist
    /// - Permission error if caller is not a global admin
    pub async fn reject_room(
        &self,
        room_id: RoomId,
        admin_id: Option<&UserId>,
        reason: Option<String>,
    ) -> Result<Room> {
        tracing::info!(room_id = %room_id, ?admin_id, "Rejecting pending room");

        let admin_username = if let Some(admin_id) = admin_id {
            let admin = self.user_service.get_user(admin_id).await?;
            if !admin.role.is_admin_or_above() {
                return Err(Error::Authorization(
                    "Only admins can reject rooms".to_string(),
                ));
            }
            Some(admin.username)
        } else {
            None
        };

        let mut tx = self.pool.begin().await?;
        let request = Self::load_pending_room_creation_request_for_update(&room_id, &mut tx)
            .await?
            .ok_or_else(|| {
                Error::NotFound(format!("Pending room creation request {room_id} not found"))
            })?;
        let audit_actor_username = match admin_username {
            Some(username) => username,
            None => Self::membership_snapshot_username_tx(&mut tx, &request.requested_by).await?,
        };

        let rejected = ReviewRepository::reject_room_creation_with_executor(
            &mut *tx,
            room_id,
            admin_id.copied(),
            reason.as_deref(),
        )
        .await?;
        if rejected == 0 {
            return Err(Error::NotFound(format!(
                "Pending room creation request {room_id} not found"
            )));
        }
        tx.commit().await?;

        let mut updated =
            Room::new_with_description(request.name, request.description, request.requested_by);
        updated.id = request.id;

        // Audit log
        self.audit_log(
            admin_id.unwrap_or(&updated.created_by),
            &audit_actor_username,
            AuditAction::RoomRejected,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({
                "previous_review_status": "pending",
                "new_review_status": "rejected",
                "reason": reason,
            }),
        )
        .await;

        tracing::info!(room_id = %room_id, ?admin_id, "Room rejected");

        Ok(updated)
    }

    /// List pending room creation requests (admin only).
    ///
    /// Returns room-shaped DTOs synthesized from pending request records.
    pub async fn list_pending_rooms(
        &self,
        admin_id: UserId,
        pagination: PageParams,
    ) -> Result<(Vec<Room>, i64)> {
        pagination.validate()?;

        // Verify admin permission
        let admin = self.user_service.get_user(&admin_id).await?;

        if !admin.role.is_admin_or_above() {
            return Err(Error::Authorization(
                "Only admins can list pending rooms".to_string(),
            ));
        }

        let total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM room_creation_requests
            WHERE reviewed_at IS NULL AND status = $1
            "#,
            i16::from(ReviewStatus::Pending),
        )
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query!(
            r#"
            SELECT id AS "id: RoomId",
                   requested_by AS "requested_by: UserId",
                   name,
                   description,
                   requested_at
            FROM room_creation_requests
            WHERE reviewed_at IS NULL AND status = $1
            ORDER BY requested_at DESC, id DESC
            LIMIT $2 OFFSET $3
            "#,
            i16::from(ReviewStatus::Pending),
            pagination.limit_i64()?,
            pagination.offset_i64()?,
        )
        .fetch_all(&self.pool)
        .await?;

        let rooms = rows
            .into_iter()
            .map(|row| {
                let requested_at = row.requested_at;
                Room {
                    id: row.id,
                    name: row.name,
                    description: row.description,
                    cover_file_reference_id: None,
                    created_by: row.requested_by,
                    status: RoomStatus::Active,
                    is_banned: false,
                    closed_at: None,
                    created_at: requested_at,
                    updated_at: requested_at,
                    deleted_at: None,
                    version: 0,
                    last_activity_at: requested_at,
                }
            })
            .collect();

        Ok((rooms, total))
    }
}
