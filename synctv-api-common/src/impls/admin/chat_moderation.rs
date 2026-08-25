use synctv_core::models::{AuditDetails, RoomId, UserId, UserRole};
use synctv_core::repository::{
    ChatModerationJob, ChatModerationJobPhase, ChatModerationProgress, NewChatModerationJob,
};

use super::{AdminApiImpl, ApiError};

impl AdminApiImpl {
    async fn admin_chat_messages_to_proto(
        &self,
        messages: Vec<synctv_core::models::ChatMessageWithAttachments>,
    ) -> Result<Vec<synctv_proto::client::ChatMessageReceive>, ApiError> {
        let user_ids: Vec<synctv_core::models::UserId> = messages
            .iter()
            .filter_map(|message| message.message.user_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let usernames = self
            .user_service
            .get_usernames(&user_ids)
            .await
            .map_err(ApiError::from)?;
        messages
            .iter()
            .map(|message| {
                let username = message
                    .message
                    .user_id
                    .and_then(|user_id| usernames.get(&user_id).cloned());
                crate::impls::messaging::chat_message_receive_to_proto(
                    message,
                    &self.public_id_codec,
                    username,
                )
                .map_err(ApiError::Internal)
            })
            .collect()
    }

    pub async fn get_room_chat_history(
        &self,
        room_id_raw: &str,
        req: synctv_proto::client::GetChatHistoryRequest,
    ) -> Result<synctv_proto::client::GetChatHistoryResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let room_id = crate::impls::proto_validated_room_id(room_id_raw, &self.public_id_codec)?;
        if !self
            .room_service
            .room_exists(&room_id)
            .await
            .map_err(ApiError::from)?
        {
            return Err(ApiError::NotFound("Room not found".to_string()));
        }
        let (limit, cursor, selection) =
            crate::impls::client::build_get_chat_history_request(&req)?;
        let cursor = cursor
            .map(|(created_at, id)| synctv_core::models::ChatHistoryCursor { created_at, id });
        let chat_service = self.chat_service.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable("Chat service is unavailable".to_string())
        })?;
        let page = chat_service
            .get_history_page_with_attachments_for_viewer(
                &room_id, cursor, limit, true, None, &selection,
            )
            .await
            .map_err(ApiError::from)?;
        let messages = self.admin_chat_messages_to_proto(page.messages).await?;
        let next_cursor = page.next_cursor.map(|cursor| {
            format!(
                "{}|{}",
                synctv_common::time::format_datetime_rfc3339(cursor.created_at),
                cursor.id
            )
        });
        Ok(synctv_proto::client::GetChatHistoryResponse {
            messages,
            next_cursor: next_cursor.unwrap_or_default(),
            event_cursor: Some(synctv_proto::client::EventCursor {
                event_id: page.event_cursor.event_id,
                sequence: page.event_cursor.sequence,
            }),
        })
    }

    pub async fn get_room_chat_message_context(
        &self,
        room_id_raw: &str,
        req: synctv_proto::client::GetChatMessageContextRequest,
    ) -> Result<synctv_proto::client::GetChatMessageContextResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let room_id = crate::impls::proto_validated_room_id(room_id_raw, &self.public_id_codec)?;
        if !self
            .room_service
            .room_exists(&room_id)
            .await
            .map_err(ApiError::from)?
        {
            return Err(ApiError::NotFound("Room not found".to_string()));
        }
        let message_id = req.message_id.parse::<i64>().map_err(|_| {
            ApiError::InvalidInput("message_id must be a numeric chat message ID".to_string())
        })?;
        let chat_service = self.chat_service.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable("Chat service is unavailable".to_string())
        })?;
        let limit = |value: i32| if value <= 0 { 20 } else { value.min(50) };
        let context = chat_service
            .get_message_context_for_viewer(
                &room_id,
                message_id,
                limit(req.before_limit),
                limit(req.after_limit),
                req.include_deleted,
                None,
            )
            .await
            .map_err(ApiError::from)?;
        let before_len = context.before.len();
        let after_len = context.after.len();
        let mut messages = context.before;
        messages.push(context.anchor);
        messages.extend(context.after);
        let messages = self.admin_chat_messages_to_proto(messages).await?;
        let mut messages = messages.into_iter();
        let before = messages.by_ref().take(before_len).collect();
        let message = messages
            .next()
            .ok_or_else(|| ApiError::Internal("Chat context anchor is missing".to_string()))?;
        let after = messages.take(after_len).collect();
        Ok(synctv_proto::client::GetChatMessageContextResponse {
            before,
            message: Some(message),
            after,
        })
    }

    pub async fn moderate_room_chat_user(
        &self,
        req: synctv_proto::admin::ModerateRoomChatUserRequest,
        admin_user_id: &UserId,
        caller_role: UserRole,
    ) -> Result<synctv_proto::admin::ModerateRoomChatUserResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        if !req.delete_all_messages
            && !req.delete_all_reactions
            && !req.ban_user
            && req.message_id.trim().is_empty()
        {
            return Err(ApiError::InvalidInput(
                "At least one moderation action is required".to_string(),
            ));
        }
        let room_id: RoomId =
            crate::impls::proto_validated_room_id(req.room_id, &self.public_id_codec)?;
        let target_user_id: UserId =
            crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;
        if !self
            .room_service
            .room_exists(&room_id)
            .await
            .map_err(ApiError::from)?
        {
            return Err(ApiError::NotFound("Room not found".to_string()));
        }
        let reason = (!req.reason.trim().is_empty()).then(|| req.reason.trim().to_string());
        let message_id = if req.message_id.trim().is_empty() {
            None
        } else {
            let message_id = req.message_id.parse::<i64>().map_err(|_| {
                ApiError::InvalidInput("message_id must be a numeric chat message ID".to_string())
            })?;
            if message_id <= 0 {
                return Err(ApiError::InvalidInput(
                    "message_id must be positive".to_string(),
                ));
            }
            Some(message_id)
        };
        let chat_service = self.chat_service.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable("Chat service is unavailable".to_string())
        })?;
        let admin_actor = self.require_authorized_admin_actor(admin_user_id).await?;
        self.validate_user_action_target(&target_user_id, caller_role, "moderate chat for")
            .await?;
        if let Some(message_id) = message_id {
            chat_service
                .validate_moderation_message_anchor(&room_id, message_id, &target_user_id)
                .await
                .map_err(ApiError::from)?;
        }
        let operation_id = synctv_common::snanoid!(16);
        chat_service
            .moderation_job_repository()
            .insert(&NewChatModerationJob {
                id: operation_id.clone(),
                room_id,
                target_user_id,
                actor_user_id: *admin_user_id,
                actor_username: admin_actor.username().to_string(),
                actor_role: caller_role,
                message_id,
                ban_user: req.ban_user,
                delete_all_messages: req.delete_all_messages,
                delete_all_reactions: req.delete_all_reactions,
                reason: reason.or_else(|| Some("admin_bulk_deleted".to_string())),
                snapshot_at: self.clock.now(),
            })
            .await
            .map_err(ApiError::from)?;
        tracing::info!(
            job_id = %operation_id,
            room_id = %room_id,
            target_user_id = %target_user_id,
            "Chat moderation job queued"
        );
        Ok(synctv_proto::admin::ModerateRoomChatUserResponse {})
    }

    pub async fn process_chat_moderation_jobs(
        &self,
        worker_id: &str,
        limit: i64,
    ) -> Result<bool, ApiError> {
        let chat_service = self.chat_service.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable("Chat service is unavailable".to_string())
        })?;
        let jobs = chat_service
            .moderation_job_repository()
            .claim_batch(worker_id, limit)
            .await
            .map_err(ApiError::from)?;
        if jobs.is_empty() {
            return Ok(false);
        }
        for job in jobs {
            if let Err(error) = self
                .process_chat_moderation_job(chat_service, &job, worker_id)
                .await
            {
                let message = error.to_string();
                match chat_service
                    .moderation_job_repository()
                    .mark_failed(&job, worker_id, &message)
                    .await
                {
                    Ok(true) if job.attempts.saturating_add(1) >= 10 => {
                        tracing::error!(
                            error = %message,
                            job_id = %job.id,
                            attempts = job.attempts.saturating_add(1),
                            "Chat moderation job permanently failed"
                        );
                    }
                    Ok(true) => {
                        tracing::warn!(
                            error = %message,
                            job_id = %job.id,
                            attempts = job.attempts.saturating_add(1),
                            "Chat moderation job failed and will be retried"
                        );
                    }
                    Ok(false) => {
                        tracing::warn!(
                            job_id = %job.id,
                            "Chat moderation job failure was not persisted because its lease changed"
                        );
                    }
                    Err(mark_error) => {
                        tracing::error!(error = %mark_error, job_id = %job.id, "Failed to persist chat moderation job error");
                    }
                }
            }
        }
        Ok(true)
    }

    async fn process_chat_moderation_job(
        &self,
        chat_service: &synctv_core::service::ChatService,
        job: &ChatModerationJob,
        worker_id: &str,
    ) -> Result<(), ApiError> {
        let actor = synctv_core::service::AuthorizedAdminActor::for_persisted_job(
            job.actor_user_id,
            job.actor_username.clone(),
        );
        let mut next = job.clone();
        let dispatcher = self.chat_event_dispatcher.clone();
        let mut phase = job.phase;
        let mut deleted_messages = job.deleted_messages;
        let mut deleted_reactions = job.deleted_reactions;
        let mut explicit_message_done = job.explicit_message_done;
        let mut ban_done = job.ban_done;
        let mut snapshot_at = job.snapshot_at;
        let (message_cursor, reaction_cursor, hidden_reaction_cursor) = (
            job.message_cursor,
            job.reaction_cursor,
            job.hidden_reaction_cursor.clone(),
        );
        let mut message_cursor = message_cursor;
        let mut reaction_cursor = reaction_cursor;
        let mut hidden_reaction_cursor = hidden_reaction_cursor;
        let moderation_progress = ChatModerationProgress {
            job_id: &job.id,
            worker_id,
            lock_version: job.lock_version,
        };

        if let Some(message_id) = job.message_id.filter(|_| !explicit_message_done) {
            let outcome = chat_service
                .delete_moderation_message_event_outcome_as_admin_with_progress(
                    &job.room_id,
                    message_id,
                    &job.target_user_id,
                    &actor,
                    job.reason.as_deref().or(Some("admin_deleted")),
                    Some(moderation_progress),
                )
                .await
                .map_err(ApiError::from)?;
            if let Some(outcome) = outcome.filter(|outcome| outcome.inserted) {
                deleted_messages = deleted_messages.checked_add(1).ok_or_else(|| {
                    ApiError::Internal("Deleted message count exceeds i64::MAX".to_string())
                })?;
                deleted_reactions = deleted_reactions
                    .checked_add(i64::try_from(outcome.deleted_reactions).map_err(|_| {
                        ApiError::Internal("Deleted reaction count exceeds i64::MAX".to_string())
                    })?)
                    .ok_or_else(|| {
                        ApiError::Internal("Deleted reaction count exceeds i64::MAX".to_string())
                    })?;
                dispatcher.dispatch(&outcome.event);
                if let Some(pin_event) = &outcome.pin_event {
                    dispatcher.dispatch_pin(pin_event);
                }
            }
            explicit_message_done = true;
        }

        if job.ban_user && !ban_done {
            let newly_banned = self
                .ensure_persisted_user_banned_with_cleanup(
                    &job.target_user_id,
                    &job.actor_user_id,
                    job.actor_role,
                    job.reason
                        .clone()
                        .or_else(|| Some("chat_moderation".to_string())),
                )
                .await?;
            if newly_banned {
                if let Err(error) = self
                    .audit_service
                    .log(synctv_core::service::AuditEventParams {
                        actor_id: job.actor_user_id.to_string(),
                        actor_username: job.actor_username.clone(),
                        action: synctv_core::models::AuditAction::UserBanned,
                        target_type: synctv_core::models::AuditTargetType::User,
                        target_id: Some(job.target_user_id.to_string()),
                        details: AuditDetails {
                            target_user_id: Some(job.target_user_id.to_string()),
                            caller_role: Some(format!("{:?}", job.actor_role)),
                            ..Default::default()
                        },
                        ip_address: None,
                        user_agent: None,
                    })
                    .await
                {
                    tracing::error!(error = %error, job_id = %job.id, "Failed to write async chat moderation ban audit log");
                }
            }
            ban_done = true;
            snapshot_at = self.clock.now();
        }

        match phase {
            ChatModerationJobPhase::Messages if job.delete_all_messages => {
                let page = chat_service
                    .moderate_user_messages_page(
                        &job.room_id,
                        &job.target_user_id,
                        &actor,
                        job.reason.as_deref(),
                        snapshot_at,
                        message_cursor,
                        Some(moderation_progress),
                        move |event, pin_event| {
                            dispatcher.dispatch(event);
                            if let Some(pin_event) = pin_event {
                                dispatcher.dispatch_pin(pin_event);
                            }
                        },
                    )
                    .await
                    .map_err(ApiError::from)?;
                deleted_messages = deleted_messages
                    .checked_add(i64::try_from(page.outcome.deleted_messages).map_err(|_| {
                        ApiError::Internal("Deleted message count exceeds i64::MAX".to_string())
                    })?)
                    .ok_or_else(|| {
                        ApiError::Internal("Deleted message count exceeds i64::MAX".to_string())
                    })?;
                deleted_reactions = deleted_reactions
                    .checked_add(i64::try_from(page.outcome.deleted_reactions).map_err(|_| {
                        ApiError::Internal("Deleted reaction count exceeds i64::MAX".to_string())
                    })?)
                    .ok_or_else(|| {
                        ApiError::Internal("Deleted reaction count exceeds i64::MAX".to_string())
                    })?;
                message_cursor = page.next_cursor;
                phase = if page.done {
                    if job.delete_all_reactions {
                        ChatModerationJobPhase::Reactions
                    } else {
                        ChatModerationJobPhase::Done
                    }
                } else {
                    ChatModerationJobPhase::Messages
                };
            }
            ChatModerationJobPhase::Messages => {
                phase = if job.delete_all_reactions {
                    ChatModerationJobPhase::Reactions
                } else {
                    ChatModerationJobPhase::Done
                };
            }
            ChatModerationJobPhase::Reactions if job.delete_all_reactions => {
                let dispatcher = self.chat_event_dispatcher.clone();
                let page = chat_service
                    .remove_user_reactions_page(
                        &job.room_id,
                        &job.target_user_id,
                        actor.user_id(),
                        snapshot_at,
                        reaction_cursor,
                        hidden_reaction_cursor,
                        Some(moderation_progress),
                        move |event, pin_event| {
                            dispatcher.dispatch(event);
                            if let Some(pin_event) = pin_event {
                                dispatcher.dispatch_pin(pin_event);
                            }
                        },
                    )
                    .await
                    .map_err(ApiError::from)?;
                deleted_reactions = deleted_reactions
                    .checked_add(i64::try_from(page.deleted_reactions).map_err(|_| {
                        ApiError::Internal("Deleted reaction count exceeds i64::MAX".to_string())
                    })?)
                    .ok_or_else(|| {
                        ApiError::Internal("Deleted reaction count exceeds i64::MAX".to_string())
                    })?;
                reaction_cursor = page.next_cursor;
                hidden_reaction_cursor = page.hidden_next_cursor;
                phase = if page.hidden_done {
                    ChatModerationJobPhase::Done
                } else {
                    ChatModerationJobPhase::Reactions
                };
            }
            ChatModerationJobPhase::Reactions => phase = ChatModerationJobPhase::Done,
            ChatModerationJobPhase::Done => {}
        }

        next.phase = phase;
        next.message_cursor = message_cursor;
        next.reaction_cursor = reaction_cursor;
        next.hidden_reaction_cursor = hidden_reaction_cursor.clone();
        next.deleted_messages = deleted_messages;
        next.deleted_reactions = deleted_reactions;
        next.snapshot_at = snapshot_at;
        if phase == ChatModerationJobPhase::Done {
            let updated = chat_service
                .moderation_job_repository()
                .update_progress(
                    &next,
                    worker_id,
                    phase,
                    message_cursor,
                    reaction_cursor,
                    hidden_reaction_cursor,
                    deleted_messages,
                    deleted_reactions,
                    explicit_message_done,
                    ban_done,
                )
                .await
                .map_err(ApiError::from)?;
            if !updated {
                return Ok(());
            }
            next.lock_version += 1;
            let completed = chat_service
                .moderation_job_repository()
                .mark_completed(&next, worker_id, deleted_messages, deleted_reactions)
                .await
                .map_err(ApiError::from)?;
            if !completed {
                return Ok(());
            }
        } else {
            let updated = chat_service
                .moderation_job_repository()
                .update_progress(
                    &next,
                    worker_id,
                    phase,
                    message_cursor,
                    reaction_cursor,
                    hidden_reaction_cursor,
                    deleted_messages,
                    deleted_reactions,
                    explicit_message_done,
                    ban_done,
                )
                .await
                .map_err(ApiError::from)?;
            if !updated {
                return Ok(());
            }
        }
        Ok(())
    }
}
