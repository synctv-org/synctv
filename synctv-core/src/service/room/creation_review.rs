use crate::{
    models::{
        AuditAction, AuditDetails, AuditTargetType, Room, RoomId, RoomMember, RoomRole, UserId,
    },
    repository::ReviewRepository,
    Error, Result,
};

use super::{creation_policy::RoomCreationPolicy, RoomService};

impl RoomService {
    /// Approve a pending room_creation creation request and create the room.
    ///
    /// This is an admin-only operation for rooms created when `approval_required=true`.
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
        tracing::info!(request_id = %request_id, ?admin_id, "Approving room_creation creation request");

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
                    "Pending room_creation creation request {request_id} not found"
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
        let (category_id, label_ids) = self
            .resolve_enabled_room_taxonomy(request.category_id, &request.label_ids)
            .await?;

        let mut room = Room::new_with_description(
            request.name.clone(),
            request.description.clone(),
            request.requested_by,
        );
        room.is_public = request.is_public;
        let mut updated = self
            .room_repo
            .create_with_taxonomy_executor(&room, category_id, &mut *tx)
            .await?;
        crate::repository::RoomTaxonomyRepository::assign_room_labels(
            updated.id,
            &label_ids,
            Some(request.requested_by),
            &mut tx,
        )
        .await?;

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
                "Pending room_creation creation request {request_id} not found"
            )));
        }

        tx.commit().await?;

        crate::metrics::application::ROOMS_ACTIVE.inc();

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
            AuditDetails {
                request_id: Some(request_id.to_string()),
                previous_review_status: Some("pending".to_string()),
                new_review_status: Some("approved".to_string()),
                ..Default::default()
            },
        )
        .await;

        tracing::info!(request_id = %request_id, room_id = %updated.id, ?admin_id, "Room approved and activated");

        self.hydrate_room_taxonomy(&mut updated).await?;

        Ok(updated)
    }

    /// Reject a pending room_creation creation request.
    ///
    /// This is an admin-only operation for rooms created when `approval_required=true`.
    /// Rejected requests are preserved for review/audit; no room row is created.
    ///
    /// # Errors
    /// - `Error::NotFound` if room_creation doesn't exist
    /// - `Error::NotFound` if the pending request does not exist
    /// - Permission error if caller is not a global admin
    pub async fn reject_room(
        &self,
        room_id: RoomId,
        admin_id: Option<&UserId>,
        reason: Option<String>,
    ) -> Result<Room> {
        tracing::info!(room_id = %room_id, ?admin_id, "Rejecting pending room_creation");

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
                Error::NotFound(format!(
                    "Pending room_creation creation request {room_id} not found"
                ))
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
                "Pending room_creation creation request {room_id} not found"
            )));
        }
        tx.commit().await?;

        let mut updated =
            Room::new_with_description(request.name, request.description, request.requested_by);
        updated.id = request.id;
        updated.is_public = request.is_public;

        // Audit log
        self.audit_log(
            admin_id.unwrap_or(&updated.created_by),
            &audit_actor_username,
            AuditAction::RoomRejected,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            AuditDetails {
                previous_review_status: Some("pending".to_string()),
                new_review_status: Some("rejected".to_string()),
                reason,
                ..Default::default()
            },
        )
        .await;

        tracing::info!(room_id = %room_id, ?admin_id, "Room rejected");

        Ok(updated)
    }
}
