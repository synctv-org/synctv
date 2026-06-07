use synctv_core::models::{UserId, UserRole};

use super::{
    check_role_hierarchy, list_owned_room_ids, map_batch_result_error, parse_batch_user_ids,
    AdminApiImpl, ApiError, RequestContext,
};

impl AdminApiImpl {
    pub async fn batch_ban_users(
        &self,
        req: synctv_proto::admin::BatchBanUsersRequest,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::BatchBanUsersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let parsed_user_ids = parse_batch_user_ids(&req.user_ids, &self.public_id_codec)?;
        let reason = req.reason.trim();
        let reason = (!reason.is_empty()).then(|| reason.to_string());

        let mut proto_results = Vec::with_capacity(req.user_ids.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

        for (user_id_str, uid) in req.user_ids.iter().zip(parsed_user_ids.iter()) {
            match self.user_service.get_user(uid).await {
                Ok(target_user) => {
                    if let Err(e) = check_role_hierarchy(caller_role, target_user.role, "ban") {
                        proto_results.push(synctv_proto::admin::BatchResultItem {
                            id: user_id_str.clone(),
                            success: false,
                            error: map_batch_result_error(e),
                        });
                        failed += 1;
                        continue;
                    }
                    match self
                        .ban_user_with_cleanup(uid, admin_user_id, caller_role, reason.clone())
                        .await
                    {
                        Ok(_) => {
                            proto_results.push(synctv_proto::admin::BatchResultItem {
                                id: user_id_str.clone(),
                                success: true,
                                error: String::new(),
                            });
                            succeeded += 1;
                        }
                        Err(e) => {
                            proto_results.push(synctv_proto::admin::BatchResultItem {
                                id: user_id_str.clone(),
                                success: false,
                                error: map_batch_result_error(e),
                            });
                            failed += 1;
                        }
                    }
                }
                Err(e) => {
                    proto_results.push(synctv_proto::admin::BatchResultItem {
                        id: user_id_str.clone(),
                        success: false,
                        error: map_batch_result_error(e),
                    });
                    failed += 1;
                }
            }
        }

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserBanned,
            synctv_core::models::AuditTargetType::User,
            None,
            serde_json::json!({
                "action": "batch_ban",
                "total": req.user_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
                "reason": req.reason,
            }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::BatchBanUsersResponse {
            results: proto_results,
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
        let parsed_user_ids = parse_batch_user_ids(&req.user_ids, &self.public_id_codec)?;

        let mut allowed_ids = Vec::with_capacity(req.user_ids.len());
        let mut proto_results = Vec::with_capacity(req.user_ids.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

        for (user_id_str, uid) in req.user_ids.iter().zip(parsed_user_ids) {
            match self.user_service.get_user(&uid).await {
                Ok(target_user) => {
                    if let Err(e) = check_role_hierarchy(caller_role, target_user.role, "delete") {
                        proto_results.push(synctv_proto::admin::BatchResultItem {
                            id: user_id_str.clone(),
                            success: false,
                            error: map_batch_result_error(e),
                        });
                        failed += 1;
                        continue;
                    }
                    allowed_ids.push((user_id_str.clone(), uid));
                }
                Err(e) => {
                    proto_results.push(synctv_proto::admin::BatchResultItem {
                        id: user_id_str.clone(),
                        success: false,
                        error: map_batch_result_error(e),
                    });
                    failed += 1;
                }
            }
        }

        for (user_id, uid) in allowed_ids {
            let owned_room_ids = match list_owned_room_ids(&self.room_service, &uid).await {
                Ok(room_ids) => room_ids,
                Err(error) => {
                    proto_results.push(synctv_proto::admin::BatchResultItem {
                        id: user_id,
                        success: false,
                        error: map_batch_result_error(ApiError::from(error)),
                    });
                    failed += 1;
                    continue;
                }
            };

            let (deleted_room_outbox_events, deleted_room_fanout) =
                self.prepare_deleted_room_outbox_fanout(&owned_room_ids, admin_user_id)?;

            match self
                .user_service
                .delete_user_with_summary_and_outbox(&uid, deleted_room_outbox_events)
                .await
            {
                Ok(summary) => {
                    proto_results.push(synctv_proto::admin::BatchResultItem {
                        id: user_id.clone(),
                        success: true,
                        error: String::new(),
                    });
                    succeeded += 1;

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
                Err(e) => {
                    proto_results.push(synctv_proto::admin::BatchResultItem {
                        id: user_id,
                        success: false,
                        error: map_batch_result_error(ApiError::from(e)),
                    });
                    failed += 1;
                }
            }
        }

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserDeleted,
            synctv_core::models::AuditTargetType::User,
            None,
            serde_json::json!({
                "action": "batch_delete",
                "total": req.user_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
            }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::BatchDeleteUsersResponse {
            results: proto_results,
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
        let mut proto_results = Vec::with_capacity(req.room_ids.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

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
                    .disconnect_room(&rid, "room_batch_banned")
                    .await;

                Ok::<(), ApiError>(())
            }
            .await;

            match result {
                Ok(()) => {
                    proto_results.push(synctv_proto::admin::BatchResultItem {
                        id: room_id.clone(),
                        success: true,
                        error: String::new(),
                    });
                    succeeded += 1;
                }
                Err(e) => {
                    proto_results.push(synctv_proto::admin::BatchResultItem {
                        id: room_id.clone(),
                        success: false,
                        error: map_batch_result_error(e),
                    });
                    failed += 1;
                }
            }
        }

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::RoomBanned,
            synctv_core::models::AuditTargetType::Room,
            None,
            serde_json::json!({
                "action": "batch_ban",
                "total": req.room_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
                "reason": req.reason,
            }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::BatchBanRoomsResponse {
            results: proto_results,
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
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;
        let mut proto_results = Vec::with_capacity(req.room_ids.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

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
                    .disconnect_room(&rid, "room_batch_deleted")
                    .await;

                Ok::<(), ApiError>(())
            }
            .await;

            match result {
                Ok(()) => {
                    proto_results.push(synctv_proto::admin::BatchResultItem {
                        id: room_id.clone(),
                        success: true,
                        error: String::new(),
                    });
                    succeeded += 1;
                }
                Err(e) => {
                    proto_results.push(synctv_proto::admin::BatchResultItem {
                        id: room_id.clone(),
                        success: false,
                        error: map_batch_result_error(e),
                    });
                    failed += 1;
                }
            }
        }

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::RoomDeleted,
            synctv_core::models::AuditTargetType::Room,
            None,
            serde_json::json!({
                "action": "batch_delete",
                "total": req.room_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
            }),
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::BatchDeleteRoomsResponse {
            results: proto_results,
            succeeded,
            failed,
        })
    }
}
