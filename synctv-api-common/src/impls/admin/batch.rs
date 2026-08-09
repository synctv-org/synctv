use synctv_core::models::{AuditDetails, UserId, UserRole};

use super::{
    check_role_hierarchy, usize_to_i32_api, AdminApiImpl, ApiError, BatchResultsAccumulator,
    RequestContext,
};

const MAX_BATCH_ITEMS: usize = 100;
const MAX_BATCH_REASON_LEN: usize = 500;

fn validate_batch_items(items: &[String], label: &str) -> Result<(), ApiError> {
    if items.is_empty() {
        return Err(ApiError::InvalidInput(format!(
            "{label} must contain at least one item"
        )));
    }
    if items.len() > MAX_BATCH_ITEMS {
        return Err(ApiError::InvalidInput(format!(
            "{label} must contain at most {MAX_BATCH_ITEMS} items"
        )));
    }
    Ok(())
}

fn validate_batch_reason(reason: &str) -> Result<(), ApiError> {
    if reason.len() > MAX_BATCH_REASON_LEN {
        return Err(ApiError::InvalidInput(format!(
            "reason must be at most {MAX_BATCH_REASON_LEN} characters"
        )));
    }
    Ok(())
}

impl AdminApiImpl {
    pub async fn batch_ban_users(
        &self,
        req: synctv_proto::admin::BatchBanUsersRequest,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::BatchBanUsersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        validate_batch_items(&req.user_ids, "user_ids")?;
        validate_batch_reason(&req.reason)?;
        let parsed_user_ids = super::parse_batch_user_ids(&req.user_ids, &self.public_id_codec)?;
        let reason = req.reason.trim();
        let reason = (!reason.is_empty()).then(|| reason.to_string());

        let mut accumulator = BatchResultsAccumulator::new(req.user_ids.len());

        for (user_id_str, uid) in req.user_ids.iter().zip(parsed_user_ids.iter()) {
            match self
                .ban_user_with_cleanup(uid, admin_user_id, caller_role, reason.clone())
                .await
            {
                Ok(_) => accumulator.record_ok(user_id_str.clone()),
                Err(e) => accumulator.record_err(user_id_str.clone(), e),
            }
        }

        let (results, succeeded, failed) = accumulator.into_parts();
        let total = usize_to_i32_api(req.user_ids.len(), "batch user count")?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserBanned,
            synctv_core::models::AuditTargetType::User,
            None,
            AuditDetails {
                action: Some("batch_ban".to_string()),
                total: Some(total),
                succeeded: Some(succeeded),
                failed: Some(failed),
                reason: (!req.reason.trim().is_empty()).then_some(req.reason),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::BatchBanUsersResponse {
            results,
            succeeded,
            failed,
        })
    }

    pub async fn batch_delete_users(
        &self,
        req: synctv_proto::admin::BatchDeleteUsersRequest,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::BatchDeleteUsersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        validate_batch_items(&req.user_ids, "user_ids")?;
        let parsed_user_ids = super::parse_batch_user_ids(&req.user_ids, &self.public_id_codec)?;

        let mut allowed_ids = Vec::with_capacity(req.user_ids.len());
        let mut accumulator = BatchResultsAccumulator::new(req.user_ids.len());

        for (user_id_str, uid) in req.user_ids.iter().zip(parsed_user_ids) {
            match self.user_service.get_user(&uid).await {
                Ok(target_user) => {
                    if let Err(e) = check_role_hierarchy(caller_role, target_user.role, "delete") {
                        accumulator.record_err(user_id_str.clone(), e);
                        continue;
                    }
                    allowed_ids.push((user_id_str.clone(), uid));
                }
                Err(e) => accumulator.record_err(user_id_str.clone(), e),
            }
        }

        for (user_id, uid) in allowed_ids {
            let owned_room_ids = match self.list_owned_room_ids(&uid).await {
                Ok(room_ids) => room_ids,
                Err(error) => {
                    accumulator.record_err(user_id, ApiError::from(error));
                    continue;
                }
            };

            let (deleted_room_outbox_events, deleted_room_fanout) =
                self.prepare_deleted_room_outbox_fanout(&owned_room_ids, admin_user_id)?;

            match self
                .user_service
                .delete_user_with_summary_and_outbox_with_options(
                    &uid,
                    deleted_room_outbox_events,
                    synctv_core::service::UserDeletionOptions {
                        source: synctv_core::service::UserDeletionSource::Admin,
                        deleted_by: (*admin_user_id != super::LOCAL_MANAGEMENT_ACTOR_USER_ID)
                            .then_some(*admin_user_id),
                        reason: Some("Deleted by administrator batch operation".to_string()),
                    },
                )
                .await
            {
                Ok(summary) => {
                    accumulator.record_ok(user_id.clone());

                    self.realtime_lifecycle
                        .finalize_user_deletion(
                            self.room_service.as_ref(),
                            &summary,
                            admin_user_id,
                            "batch_deleted",
                            deleted_room_fanout,
                        )
                        .await;
                }
                Err(e) => accumulator.record_err(user_id, ApiError::from(e)),
            }
        }

        let (results, succeeded, failed) = accumulator.into_parts();
        let total = usize_to_i32_api(req.user_ids.len(), "batch user count")?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserDeleted,
            synctv_core::models::AuditTargetType::User,
            None,
            AuditDetails {
                action: Some("batch_delete".to_string()),
                total: Some(total),
                succeeded: Some(succeeded),
                failed: Some(failed),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::BatchDeleteUsersResponse {
            results,
            succeeded,
            failed,
        })
    }

    pub async fn batch_ban_rooms(
        &self,
        req: synctv_proto::admin::BatchBanRoomsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::BatchBanRoomsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        validate_batch_items(&req.room_ids, "room_ids")?;
        validate_batch_reason(&req.reason)?;
        let mut accumulator = BatchResultsAccumulator::new(req.room_ids.len());

        for room_id in &req.room_ids {
            let rid =
                crate::impls::proto_validated_room_id(room_id.clone(), &self.public_id_codec)?;
            let result = async {
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
                self.room_service
                    .ban_room_with_outbox(
                        &rid,
                        admin_user_id,
                        Some(prepared_outbox_fanout.cloned_outbox_event()),
                    )
                    .await
                    .map_err(ApiError::from)?;

                prepared_outbox_fanout.publish_after_outbox_commit();
                self.realtime_lifecycle
                    .disconnect_room(&rid, synctv_realtime::sync::RoomDisconnectReason::Banned)
                    .await;

                Ok::<(), ApiError>(())
            }
            .await;

            match result {
                Ok(()) => accumulator.record_ok(room_id.clone()),
                Err(e) => accumulator.record_err(room_id.clone(), e),
            }
        }

        let (results, succeeded, failed) = accumulator.into_parts();
        let total = usize_to_i32_api(req.room_ids.len(), "batch room count")?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::RoomBanned,
            synctv_core::models::AuditTargetType::Room,
            None,
            AuditDetails {
                action: Some("batch_ban".to_string()),
                total: Some(total),
                succeeded: Some(succeeded),
                failed: Some(failed),
                reason: (!req.reason.trim().is_empty()).then_some(req.reason),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::BatchBanRoomsResponse {
            results,
            succeeded,
            failed,
        })
    }

    pub async fn batch_delete_rooms(
        &self,
        req: synctv_proto::admin::BatchDeleteRoomsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::BatchDeleteRoomsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        validate_batch_items(&req.room_ids, "room_ids")?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;
        let mut accumulator = BatchResultsAccumulator::new(req.room_ids.len());

        for room_id in &req.room_ids {
            let rid =
                crate::impls::proto_validated_room_id(room_id.clone(), &self.public_id_codec)?;
            let result = async {
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
                self.realtime_lifecycle
                    .disconnect_room(&rid, synctv_realtime::sync::RoomDisconnectReason::Deleted)
                    .await;

                Ok::<(), ApiError>(())
            }
            .await;

            match result {
                Ok(()) => accumulator.record_ok(room_id.clone()),
                Err(e) => accumulator.record_err(room_id.clone(), e),
            }
        }

        let (results, succeeded, failed) = accumulator.into_parts();
        let total = usize_to_i32_api(req.room_ids.len(), "batch room count")?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::RoomDeleted,
            synctv_core::models::AuditTargetType::Room,
            None,
            AuditDetails {
                action: Some("batch_delete".to_string()),
                total: Some(total),
                succeeded: Some(succeeded),
                failed: Some(failed),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::BatchDeleteRoomsResponse {
            results,
            succeeded,
            failed,
        })
    }
}
