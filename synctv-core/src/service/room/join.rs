use sqlx::{Postgres, Transaction};

use crate::{
    models::{
        AddMemberOptions, ReviewRequestId, ReviewStatus, Room, RoomId, RoomMember,
        RoomMemberWithUser, RoomRole, RoomSettings, RoomStatus, UserId,
    },
    repository::ReviewRepository,
    service::room::{RealtimeOutboxPermissionChangedEventFactory, RoomService},
    Error, Result,
};

#[derive(Clone, Debug)]
pub(super) enum RoomPasswordJoinProof {
    None,
    Plaintext(String),
    OpaqueVerified { expected_version: i32 },
}

const ROOM_JOIN_PENDING_LOCK_NS: i32 = 20_260_419;
const ROOM_JOIN_REQUEST_PENDING: ReviewStatus = ReviewStatus::Pending;

impl RoomService {
    /// Join a room.
    pub async fn join_room(
        &self,
        room_id: RoomId,
        user_id: UserId,
        password: Option<String>,
    ) -> Result<(Room, RoomMember, Vec<RoomMemberWithUser>)> {
        self.join_room_with_outbox(room_id, user_id, password, None)
            .await
    }

    pub async fn join_room_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        password: Option<String>,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<(Room, RoomMember, Vec<RoomMemberWithUser>)> {
        let proof = password.map_or(
            RoomPasswordJoinProof::None,
            RoomPasswordJoinProof::Plaintext,
        );
        self.join_room_with_password_proof(room_id, user_id, proof, outbox_event_factory)
            .await
    }

    pub(super) async fn join_room_with_password_proof(
        &self,
        room_id: RoomId,
        user_id: UserId,
        password_proof: RoomPasswordJoinProof,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<(Room, RoomMember, Vec<RoomMemberWithUser>)> {
        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            has_password = matches!(password_proof, RoomPasswordJoinProof::Plaintext(_) | RoomPasswordJoinProof::OpaqueVerified { .. }),
            "User attempting to join room"
        );

        let ctx = self
            .room_repo
            .get_join_context(&room_id, &user_id)
            .await?
            .ok_or_else(|| {
                tracing::warn!(room_id = %room_id, user_id = %user_id, "Room not found");
                Error::NotFound("Room not found".to_string())
            })?;

        self.ensure_room_creator_is_active_for_access(&ctx.room, &user_id)
            .await?;

        if ctx.room.is_banned {
            tracing::warn!(room_id = %room_id, user_id = %user_id, "Attempted to join banned room");
            return Err(Error::Authorization("Room is banned".to_string()));
        }

        if ctx.room.status != RoomStatus::Active {
            tracing::warn!(room_id = %room_id, user_id = %user_id, status = ?ctx.room.status, "Attempted to join inactive room");
            return Err(Error::InvalidInput("Room is closed".to_string()));
        }

        if ctx.is_in_kick_cooldown {
            tracing::warn!(room_id = %room_id, user_id = %user_id, "Kicked user attempted to join room during cooldown");
            return Err(Error::Authorization(
                crate::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE.to_string(),
            ));
        }
        if self
            .member_repo
            .is_in_kick_cooldown(&room_id, &user_id)
            .await?
        {
            tracing::warn!(room_id = %room_id, user_id = %user_id, "Kicked user attempted to join room during cooldown");
            return Err(Error::Authorization(
                crate::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE.to_string(),
            ));
        }

        self.verify_room_password_join_proof(&ctx, &room_id, &user_id, &password_proof)?;

        if let Some(ref lock) = self.distributed_lock {
            let lock_key = format!("join_room:{room_id}:{user_id}");
            return crate::service::distributed_lock::with_coordination_lock(
                lock.as_ref(),
                &lock_key,
                10,
                || {
                    let password_proof = password_proof.clone();
                    let outbox_event_factory = outbox_event_factory.clone();
                    async move {
                        let fresh_ctx = self
                            .room_repo
                            .get_join_context(&room_id, &user_id)
                            .await?
                            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

                        self.ensure_room_creator_is_active_for_access(&fresh_ctx.room, &user_id)
                            .await?;

                        if fresh_ctx.room.is_banned {
                            return Err(Error::Authorization("Room is banned".to_string()));
                        }

                        if fresh_ctx.room.status != RoomStatus::Active {
                            return Err(Error::InvalidInput("Room is closed".to_string()));
                        }
                        if fresh_ctx.is_in_kick_cooldown {
                            return Err(Error::Authorization(
                                crate::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE
                                    .to_string(),
                            ));
                        }
                        if self
                            .member_repo
                            .is_in_kick_cooldown(&room_id, &user_id)
                            .await?
                        {
                            return Err(Error::Authorization(
                                crate::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE
                                    .to_string(),
                            ));
                        }

                        self.verify_room_password_join_proof(
                            &fresh_ctx,
                            &room_id,
                            &user_id,
                            &password_proof,
                        )?;

                        self.do_join_room(
                            fresh_ctx.room,
                            fresh_ctx.settings,
                            room_id,
                            user_id,
                            outbox_event_factory,
                        )
                        .await
                    }
                },
            )
            .await;
        }

        self.do_join_room(
            ctx.room,
            ctx.settings,
            room_id,
            user_id,
            outbox_event_factory,
        )
        .await
    }

    fn verify_room_password_join_proof(
        &self,
        ctx: &crate::repository::room::JoinRoomContext,
        room_id: &RoomId,
        user_id: &UserId,
        proof: &RoomPasswordJoinProof,
    ) -> Result<()> {
        if !ctx.password_enabled {
            return Ok(());
        }
        match proof {
            RoomPasswordJoinProof::Plaintext(password) => {
                let credential = ctx.password_credential.as_ref().ok_or_else(|| {
                    tracing::warn!(room_id = %room_id, "Room requires password but none is set");
                    Error::Authorization("Invalid password".to_string())
                })?;
                if !self
                    .opaque_password_service
                    .verify_password(credential, password)?
                {
                    tracing::warn!(room_id = %room_id, user_id = %user_id, "Invalid password provided");
                    return Err(Error::Authorization("Invalid password".to_string()));
                }
                Ok(())
            }
            RoomPasswordJoinProof::OpaqueVerified { expected_version } => {
                let current_version = ctx.password_version.ok_or_else(|| {
                    tracing::warn!(room_id = %room_id, "Room requires password but credential version is missing");
                    Error::Authorization("Invalid password".to_string())
                })?;
                if current_version != *expected_version {
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
                Ok(())
            }
            RoomPasswordJoinProof::None => {
                tracing::warn!(room_id = %room_id, user_id = %user_id, "Password required but not provided");
                Err(Error::Authorization("Password required".to_string()))
            }
        }
    }

    async fn do_join_room(
        &self,
        room: Room,
        settings: RoomSettings,
        room_id: RoomId,
        user_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<(Room, RoomMember, Vec<RoomMemberWithUser>)> {
        if let Some(existing_member) = self.member_repo.get(&room_id, &user_id).await? {
            tracing::debug!(
                room_id = %room_id,
                user_id = %user_id,
                "User is already an active member of the room"
            );
            let members = self.member_service.list_members(&room_id).await?;
            self.touch_room_activity(room_id).await;
            return Ok((room, existing_member, members));
        }

        if !settings.allow_auto_join.0 {
            return Err(Error::Authorization(
                "This room does not allow self-service joins. Ask a room manager to add you."
                    .to_string(),
            ));
        }

        if settings.require_approval.0 {
            let pending_member = self
                .create_or_get_pending_join_request(&room_id, &user_id, RoomRole::Member)
                .await?;
            tracing::info!(
                room_id = %room_id,
                user_id = %user_id,
                "Join request created and is awaiting approval"
            );
            return Ok((room, pending_member, Vec::new()));
        }

        let options = AddMemberOptions::new().with_max_members(settings.max_members.0);
        let member = RoomMember::new(room_id, user_id, RoomRole::Member);
        let username = self.actor_username_required(&user_id).await?;
        let mut tx = self.pool.begin().await?;
        let created_member = match self
            .member_repo
            .add_with_options_tx(&member, &options, &mut tx)
            .await
        {
            Ok(member) => member,
            Err(Error::AlreadyExists(_)) => {
                tracing::debug!(
                    room_id = %room_id,
                    user_id = %user_id,
                    "User is already a member of the room (idempotent join)"
                );
                tx.rollback().await?;
                let existing_member =
                    self.member_repo
                        .get(&room_id, &user_id)
                        .await?
                        .ok_or_else(|| {
                            Error::Internal("Member disappeared after AlreadyExists".to_string())
                        })?;
                let members = self.member_service.list_members(&room_id).await?;
                self.touch_room_activity(room_id).await;
                return Ok((room, existing_member, members));
            }
            Err(e) => return Err(e),
        };
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                user_id,
                user_id,
                Some(&created_member),
                Self::role_member_event_scope(),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;

        self.permission_service
            .seed_added_member_cache(&room_id, &user_id, created_member.version)
            .await;

        let members = self.member_service.list_members(&room_id).await?;

        let subscriber_count = self
            .notification_service
            .notify_user_joined(&room_id, &user_id, &username);
        tracing::debug!(
            room_id = %room_id,
            user_id = %user_id,
            subscriber_count,
            "User joined notification dispatched"
        );

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            username = %username,
            member_count = members.len(),
            "User joined room successfully"
        );

        self.touch_room_activity(room_id).await;

        Ok((room, created_member, members))
    }

    async fn create_or_get_pending_join_request(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        role: RoomRole,
    ) -> Result<RoomMember> {
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1, hashtext($2))",
            ROOM_JOIN_PENDING_LOCK_NS,
            format!("{room_id}:{user_id}"),
        )
        .execute(&mut *tx)
        .await?;

        let existing_request_id = sqlx::query_scalar!(
            r#"
            SELECT id
            FROM room_join_requests
            WHERE room_id = $1
              AND user_id = $2
              AND reviewed_at IS NULL
            LIMIT 1
            "#,
            room_id.as_i64(),
            user_id.as_i64(),
        )
        .fetch_optional(&mut *tx)
        .await?;

        if existing_request_id.is_none() {
            let insert_result = sqlx::query!(
                r#"
                INSERT INTO room_join_requests (
                    room_id, user_id, requested_role, status, requested_at
                )
                VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP)
                "#,
                room_id.as_i64(),
                user_id.as_i64(),
                i16::from(role),
                i16::from(ROOM_JOIN_REQUEST_PENDING),
            )
            .execute(&mut *tx)
            .await;

            if let Err(error) = insert_result {
                if !matches!(
                    &error,
                    sqlx::Error::Database(db_error)
                        if db_error.constraint()
                            == Some("idx_room_join_requests_pending_unique")
                ) {
                    return Err(Error::Database(error));
                }
            }
        }

        tx.commit().await?;
        let pending = RoomMember::new(*room_id, *user_id, role);
        Ok(pending)
    }

    pub(super) async fn load_pending_join_request_by_id_for_update(
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        request_id: ReviewRequestId,
    ) -> Result<(UserId, RoomRole)> {
        let row = sqlx::query!(
            r#"
            SELECT user_id AS "user_id: UserId",
                   COALESCE(requested_role, $3) AS "requested_role!: RoomRole"
            FROM room_join_requests
            WHERE id = $1
              AND room_id = $2
              AND reviewed_at IS NULL
              AND status = $4
            FOR UPDATE
            "#,
            request_id.as_i64(),
            room_id.as_i64(),
            i16::from(RoomRole::Member),
            i16::from(ROOM_JOIN_REQUEST_PENDING),
        )
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| Error::NotFound("Pending join request not found".to_string()))?;

        Ok((row.user_id, row.requested_role))
    }

    pub(super) async fn resolve_pending_join_request_as_approved_tx(
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        reviewed_by: Option<&UserId>,
    ) -> Result<u64> {
        ReviewRepository::approve_room_join_by_member_with_executor(
            &mut **tx,
            *room_id,
            *user_id,
            reviewed_by.copied(),
        )
        .await
    }

    pub(super) async fn resolve_pending_join_request_by_id_as_approved_tx(
        tx: &mut Transaction<'_, Postgres>,
        request_id: ReviewRequestId,
        room_id: &RoomId,
        reviewed_by: Option<&UserId>,
    ) -> Result<u64> {
        ReviewRepository::approve_room_join_with_executor(
            &mut **tx,
            request_id,
            *room_id,
            reviewed_by.copied(),
        )
        .await
    }
}
