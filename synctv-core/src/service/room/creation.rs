use sqlx::{Postgres, Transaction};

use crate::{
    models::{
        AuditAction, AuditTargetType, OpaquePasswordRecord, PageParams, ReviewStatus, Room,
        RoomCategoryId, RoomId, RoomLabelId, RoomMember, RoomRole, RoomSettings, UserId,
        UserListQuery, UserRole, UserStatus,
    },
    repository::RoomRepository,
    service::{
        room::{
            RealtimeOutboxPermissionChangedEventFactory, RealtimeOutboxRoomEventFactory,
            RoomService,
        },
        RoomPasswordPolicy,
    },
    Error, Result,
};

pub(super) struct PendingRoomCreationRequest {
    pub(super) id: RoomId,
    pub(super) requested_by: UserId,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) category_id: Option<RoomCategoryId>,
    pub(super) label_ids: Vec<RoomLabelId>,
    pub(super) settings: RoomSettings,
    pub(super) opaque_password_record: Option<OpaquePasswordRecord>,
}

struct PendingRoomCreationRequestRow {
    id: RoomId,
    requested_by: UserId,
    name: String,
    description: String,
    category_id: Option<RoomCategoryId>,
    settings_payload: Option<serde_json::Value>,
    opaque_password_record: Option<Vec<u8>>,
    opaque_password_credential_identifier: Option<Vec<u8>>,
    opaque_password_ciphersuite: Option<String>,
    opaque_password_server_setup_version: Option<i32>,
}

impl PendingRoomCreationRequestRow {
    fn into_request(self) -> std::result::Result<PendingRoomCreationRequest, sqlx::Error> {
        let settings_payload = self
            .settings_payload
            .unwrap_or_else(|| serde_json::json!({}));
        let settings = serde_json::from_value::<RoomSettings>(settings_payload)
            .map_err(|error| sqlx::Error::Decode(error.into()))?;
        let opaque_password_record = match (
            self.opaque_password_record,
            self.opaque_password_credential_identifier,
            self.opaque_password_ciphersuite,
            self.opaque_password_server_setup_version,
        ) {
            (Some(record), Some(credential_identifier), Some(ciphersuite), Some(version)) => {
                Some(OpaquePasswordRecord {
                    record,
                    credential_identifier,
                    ciphersuite,
                    server_setup_version: version,
                })
            }
            (None, None, None, None) => None,
            _ => {
                return Err(sqlx::Error::Decode(
                    "Incomplete pending room OPAQUE password material".into(),
                ));
            }
        };

        Ok(PendingRoomCreationRequest {
            id: self.id,
            requested_by: self.requested_by,
            name: self.name,
            description: self.description,
            category_id: self.category_id,
            label_ids: Vec::new(),
            settings,
            opaque_password_record,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::settings_registry_unavailable_for_room_creation;
    use crate::Error;

    #[test]
    fn room_creation_policy_unavailable_error_is_service_unavailable() {
        let error = settings_registry_unavailable_for_room_creation();

        assert!(matches!(
            error,
            Error::ServiceUnavailable(message)
                if message.contains("Room creation policy")
        ));
    }
}

#[derive(Clone, Debug)]
pub struct CreateRoomWithTaxonomyRequest {
    pub name: String,
    pub description: String,
    pub created_by: UserId,
    pub password: Option<String>,
    pub settings: Option<RoomSettings>,
    pub category_id: Option<RoomCategoryId>,
    pub label_ids: Vec<RoomLabelId>,
}

pub(super) struct RoomCreationRequestDraft<'a> {
    requested_by: UserId,
    name: &'a str,
    description: &'a str,
    category_id: Option<RoomCategoryId>,
    label_ids: &'a [RoomLabelId],
    settings: &'a RoomSettings,
    password: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RoomCreationPolicy {
    pub(super) enforce_creation_toggle: bool,
}

fn initial_room_settings(settings: Option<RoomSettings>) -> RoomSettings {
    settings.unwrap_or_default()
}

fn settings_registry_unavailable_for_room_creation() -> Error {
    Error::ServiceUnavailable("Room creation policy is temporarily unavailable".to_string())
}

const ROOM_NAME_POLICY_LOCK_NS: i32 = 20_260_420;
const ROOM_OWNER_POLICY_LOCK_NS: i32 = 20_260_421;

impl RoomService {
    pub(super) async fn create_room_creation_request_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        draft: RoomCreationRequestDraft<'_>,
    ) -> Result<Room> {
        let RoomCreationRequestDraft {
            requested_by,
            name,
            description,
            category_id,
            label_ids,
            settings,
            password,
        } = draft;
        let settings_payload = serde_json::to_value(settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

        let request_id = sqlx::query_scalar!(
            r"
            INSERT INTO room_creation_requests (
                requested_by, name, description, category_id, settings_payload, status, requested_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, CURRENT_TIMESTAMP)
            RETURNING id
            ",
            requested_by.as_i64(),
            name,
            description,
            category_id.map(|id| id.as_i64()),
            settings_payload,
            i16::from(ReviewStatus::Pending)
        )
        .fetch_one(&mut **tx)
        .await?;

        let mut room =
            Room::new_with_description(name.to_string(), description.to_string(), requested_by);
        room.id = RoomId::try_from(request_id).map_err(Error::Internal)?;
        if let Some(category_id) = category_id {
            room.category = self
                .taxonomy_repo
                .categories_by_ids(&[category_id])
                .await?
                .remove(&category_id);
        }
        room.labels = self
            .taxonomy_repo
            .labels_by_ids(label_ids)
            .await?
            .into_values()
            .collect();
        crate::repository::RoomTaxonomyRepository::assign_room_creation_request_labels(
            room.id, label_ids, &mut *tx,
        )
        .await?;
        if let Some(password) = password {
            let opaque_record = self
                .opaque_password_service
                .register_password(&Self::room_opaque_credential_identifier(&room.id), password)?;
            sqlx::query!(
                r"
                UPDATE room_creation_requests
                SET opaque_password_record = $2,
                    opaque_password_credential_identifier = $3,
                    opaque_password_ciphersuite = $4,
                    opaque_password_server_setup_version = $5
                WHERE id = $1
                ",
                room.id.as_i64(),
                opaque_record.record.as_slice(),
                opaque_record.credential_identifier.as_slice(),
                opaque_record.ciphersuite.as_str(),
                opaque_record.server_setup_version
            )
            .execute(&mut **tx)
            .await?;
        }
        Ok(room)
    }

    pub(super) async fn load_pending_room_creation_request_for_update(
        request_id: &RoomId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Option<PendingRoomCreationRequest>> {
        let row = sqlx::query_as!(
            PendingRoomCreationRequestRow,
            r#"
            SELECT id AS "id: RoomId",
                   requested_by AS "requested_by: UserId",
                   name,
                   description,
                   category_id AS "category_id: RoomCategoryId",
                   settings_payload,
                   opaque_password_record,
                   opaque_password_credential_identifier,
                   opaque_password_ciphersuite,
                   opaque_password_server_setup_version
            FROM room_creation_requests
            WHERE id = $1 AND reviewed_at IS NULL AND status = $2
            FOR UPDATE
            "#,
            request_id.as_i64(),
            i16::from(ReviewStatus::Pending)
        )
        .fetch_optional(&mut **tx)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let mut request = row.into_request().map_err(Error::Database)?;
        request.label_ids = sqlx::query_scalar!(
            r#"
            SELECT label_id AS "label_id: RoomLabelId"
            FROM room_creation_request_labels
            WHERE request_id = $1
            ORDER BY label_id ASC
            "#,
            request_id as &RoomId
        )
        .fetch_all(&mut **tx)
        .await?;
        Ok(Some(request))
    }

    pub(super) async fn ensure_user_can_create_room_now_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: &UserId,
    ) -> Result<()> {
        let user = self
            .user_service
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut **tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        if !user.can_create_room(true) {
            return Err(Error::Authorization(format!(
                "User cannot create rooms while account status is {}",
                user.status
            )));
        }

        Ok(())
    }

    pub(super) fn enforce_current_room_creation_policy(
        &self,
        user_id: &UserId,
        password_enabled: bool,
        policy: RoomCreationPolicy,
    ) -> Result<()> {
        if let Some(ref registry) = self.settings_registry {
            if policy.enforce_creation_toggle {
                if registry.disable_create_room.get()? {
                    tracing::warn!(user_id = %user_id, "Room creation rejected: disable_create_room is true");
                    return Err(Error::Authorization(
                        "Room creation is currently disabled".to_string(),
                    ));
                }
                if !registry.allow_room_creation.get()? {
                    tracing::warn!(user_id = %user_id, "Room creation rejected: allow_room_creation is false");
                    return Err(Error::Authorization(
                        "Room creation is currently disabled".to_string(),
                    ));
                }
            }
            match registry.room_password_policy.get()? {
                RoomPasswordPolicy::Required if !password_enabled => {
                    tracing::warn!(user_id = %user_id, "Room creation rejected: password required by server policy");
                    return Err(Error::InvalidInput(
                        "Room password is required by server policy".to_string(),
                    ));
                }
                RoomPasswordPolicy::Forbidden if password_enabled => {
                    tracing::warn!(user_id = %user_id, "Room creation rejected: passwords not allowed by server policy");
                    return Err(Error::InvalidInput(
                        "Room passwords are not allowed by server policy".to_string(),
                    ));
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn lock_room_name_policy(
        tx: &mut Transaction<'_, Postgres>,
        creator_id: &UserId,
        name: &str,
    ) -> Result<()> {
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1, hashtext($2))",
            ROOM_NAME_POLICY_LOCK_NS,
            format!("{creator_id}:{name}"),
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn lock_room_owner_policy(
        tx: &mut Transaction<'_, Postgres>,
        owner_id: &UserId,
    ) -> Result<()> {
        let lock_key = format!("room-owner-policy:{ROOM_OWNER_POLICY_LOCK_NS}:{owner_id}");
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            lock_key,
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub(super) async fn ensure_room_name_available_for_creator_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        creator_id: &UserId,
        name: &str,
    ) -> Result<()> {
        self.ensure_room_name_available_for_creator_excluding_pending_tx(tx, creator_id, name, None)
            .await
    }

    pub(super) async fn ensure_room_name_available_for_creator_excluding_pending_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        creator_id: &UserId,
        name: &str,
        excluding_pending_request_id: Option<RoomId>,
    ) -> Result<()> {
        Self::lock_room_name_policy(tx, creator_id, name).await?;
        let exists = RoomRepository::active_name_exists_for_creator_with_executor(
            creator_id, name, &mut **tx,
        )
        .await?;
        let pending_exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM room_creation_requests
                WHERE requested_by = $1
                  AND name = $2
                  AND reviewed_at IS NULL
                  AND status = $3
                  AND ($4::BIGINT IS NULL OR id != $4)
            ) AS "exists!"
            "#,
            creator_id as &UserId,
            name,
            i16::from(ReviewStatus::Pending),
            excluding_pending_request_id.map(|id| id.as_i64()),
        )
        .fetch_one(&mut **tx)
        .await?;
        if exists || pending_exists {
            return Err(Error::AlreadyExists(
                "You already have a room with this name".to_string(),
            ));
        }
        Ok(())
    }

    pub(super) async fn enforce_room_ownership_limit_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        owner_id: &UserId,
        excluding_room_id: Option<&RoomId>,
    ) -> Result<()> {
        let max_rooms = self
            .settings_registry
            .as_ref()
            .map(|registry| registry.max_rooms_per_user.get())
            .transpose()?
            .unwrap_or(10);

        Self::lock_room_owner_policy(tx, owner_id).await?;

        let owned_room_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) as "count!"
            FROM rooms
            WHERE created_by = $1
              AND deleted_at IS NULL
              AND ($2::BIGINT IS NULL OR id != $2)
            "#,
            owner_id as &UserId,
            excluding_room_id.map(RoomId::as_i64),
        )
        .fetch_one(&mut **tx)
        .await?;

        if owned_room_count >= max_rooms {
            return Err(Error::InvalidInput(format!(
                "User has reached the maximum number of rooms ({max_rooms})"
            )));
        }

        Ok(())
    }

    /// All database operations run inside a single transaction so the room is
    /// either fully created or not visible at all.
    pub async fn create_room(
        &self,
        name: String,
        description: String,
        created_by: UserId,
        password: Option<String>,
        settings: Option<RoomSettings>,
    ) -> Result<(Room, RoomMember)> {
        self.create_room_with_outbox(name, description, created_by, password, settings, None)
            .await
    }

    pub async fn create_room_with_outbox(
        &self,
        name: String,
        description: String,
        created_by: UserId,
        password: Option<String>,
        settings: Option<RoomSettings>,
        outbox_event_factory: Option<RealtimeOutboxRoomEventFactory>,
    ) -> Result<(Room, RoomMember)> {
        self.create_room_with_taxonomy_outbox(
            CreateRoomWithTaxonomyRequest {
                name,
                description,
                created_by,
                password,
                settings,
                category_id: None,
                label_ids: Vec::new(),
            },
            outbox_event_factory,
        )
        .await
    }

    pub async fn create_room_with_taxonomy_outbox(
        &self,
        request: CreateRoomWithTaxonomyRequest,
        outbox_event_factory: Option<RealtimeOutboxRoomEventFactory>,
    ) -> Result<(Room, RoomMember)> {
        if let Some(ref lock) = self.distributed_lock {
            let created_by = request.created_by;
            let lock_key = format!("create_room:{created_by}");
            return crate::service::distributed_lock::with_coordination_lock(
                lock.as_ref(),
                &lock_key,
                Self::CREATE_ROOM_LOCK_TTL_SECS,
                || {
                    let request = request.clone();
                    let outbox_event_factory = outbox_event_factory.clone();
                    async move { self.do_create_room(request, outbox_event_factory).await }
                },
            )
            .await;
        }

        self.do_create_room(request, outbox_event_factory).await
    }

    async fn do_create_room(
        &self,
        request: CreateRoomWithTaxonomyRequest,
        outbox_event_factory: Option<RealtimeOutboxRoomEventFactory>,
    ) -> Result<(Room, RoomMember)> {
        self.do_create_room_with_policy(request, true, outbox_event_factory)
            .await
    }

    async fn do_create_room_with_policy(
        &self,
        command: CreateRoomWithTaxonomyRequest,
        enforce_creation_policy: bool,
        outbox_event_factory: Option<RealtimeOutboxRoomEventFactory>,
    ) -> Result<(Room, RoomMember)> {
        let CreateRoomWithTaxonomyRequest {
            name,
            description,
            created_by,
            password,
            settings,
            category_id,
            label_ids,
        } = command;
        let password_enabled = password.is_some();
        let room_settings = initial_room_settings(settings);
        room_settings.validate()?;
        let (category_id, label_ids) = self
            .resolve_enabled_room_taxonomy(category_id, &label_ids)
            .await?;

        tracing::info!(
            user_id = %created_by,
            room_name = %name,
            password_provided = password_enabled,
            password_enabled,
            "Creating new room"
        );

        self.enforce_current_room_creation_policy(
            &created_by,
            password_enabled,
            RoomCreationPolicy {
                enforce_creation_toggle: enforce_creation_policy,
            },
        )?;

        crate::validation::RoomNameValidator::new()
            .validate(&name)
            .map_err(|e| Error::InvalidInput(e.to_string()))?;

        if description.chars().count() > 500 {
            tracing::warn!(user_id = %created_by, desc_len = description.chars().count(), "Attempted to create room with description too long");
            return Err(Error::InvalidInput(
                "Room description too long (max 500 characters)".to_string(),
            ));
        }

        let need_review = if enforce_creation_policy {
            self.settings_registry
                .as_ref()
                .ok_or_else(settings_registry_unavailable_for_room_creation)?
                .create_room_need_review
                .get()?
        } else {
            false
        };

        if need_review {
            tracing::info!(
                user_id = %created_by,
                room_name = %name,
                "Room requires review, creating room creation request"
            );

            let mut tx = self.pool.begin().await?;
            self.ensure_user_can_create_room_now_tx(&mut tx, &created_by)
                .await?;
            self.enforce_room_ownership_limit_tx(&mut tx, &created_by, None)
                .await?;
            self.ensure_room_name_available_for_creator_tx(&mut tx, &created_by, &name)
                .await?;
            let pending_room = self
                .create_room_creation_request_tx(
                    &mut tx,
                    RoomCreationRequestDraft {
                        requested_by: created_by,
                        name: &name,
                        description: &description,
                        category_id,
                        label_ids: &label_ids,
                        settings: &room_settings,
                        password: password.as_deref(),
                    },
                )
                .await?;
            tx.commit().await?;
            let pending_member = RoomMember::new(pending_room.id, created_by, RoomRole::Creator);

            if let Some(ref notif_service) = self.user_notification_service {
                let mut all_admins = Vec::new();
                for role in [UserRole::Root, UserRole::Admin] {
                    let query = UserListQuery {
                        pagination: PageParams::new(Some(1), Some(100)),
                        search: None,
                        status: Some(UserStatus::Active),
                        role: Some(role),
                        is_banned: Some(false),
                        sort_by: crate::models::UserListSortBy::CreatedAt,
                        sort_direction: crate::models::SortDirection::Desc,
                    };
                    match self.user_service.list_users(&query).await {
                        Ok((users, _)) => all_admins.extend(users),
                        Err(error) => {
                            tracing::warn!(
                                role = %role,
                                error = %error,
                                "Failed to load admins for pending room notification"
                            );
                        }
                    }
                }

                for admin in all_admins {
                    if let Err(e) = notif_service
                        .create_system_announcement(
                            admin.id,
                            format!("Room Pending Review: {name}"),
                            format!(
                                "User {created_by} requested room \"{name}\" which requires admin review."
                            ),
                            Some(serde_json::json!({
                                "room_request_id": pending_room.id,
                                "room_name": &name,
                                "creator_id": created_by,
                            })),
                        )
                        .await
                    {
                        tracing::warn!(
                            admin_id = %admin.id,
                            error = %e,
                            "Failed to notify admin about pending room"
                        );
                    }
                }
            }

            return Ok((pending_room, pending_member));
        }

        let mut tx = self.pool.begin().await?;

        self.ensure_user_can_create_room_now_tx(&mut tx, &created_by)
            .await?;
        self.enforce_room_ownership_limit_tx(&mut tx, &created_by, None)
            .await?;
        self.ensure_room_name_available_for_creator_tx(&mut tx, &created_by, &name)
            .await?;

        let room = Room::new_with_description(name, description, created_by);
        let created_room = self
            .room_repo
            .create_with_taxonomy_executor(&room, category_id, &mut *tx)
            .await?;
        crate::repository::RoomTaxonomyRepository::assign_room_labels(
            created_room.id,
            &label_ids,
            Some(created_by),
            &mut tx,
        )
        .await?;
        if let Some(password) = password.as_deref() {
            let opaque_record = self.opaque_password_service.register_password(
                &Self::room_opaque_credential_identifier(&created_room.id),
                password,
            )?;
            self.room_password_repo
                .set_opaque_credential_with_executor(&created_room.id, &opaque_record, &mut *tx)
                .await?;
        }

        self.room_settings_repo
            .set_settings_with_executor(&created_room.id, &room_settings, &mut *tx)
            .await?;

        let member = RoomMember::new(created_room.id, created_by, RoomRole::Creator);
        let created_member = self.member_repo.add_with_executor(&member, &mut tx).await?;

        self.playback_repo
            .create_or_get_with_executor(&created_room.id, &mut tx)
            .await?;

        let outbox_event = outbox_event_factory
            .as_ref()
            .map(|factory| factory(&created_room))
            .transpose()?;
        self.insert_realtime_outbox_tx(&mut tx, outbox_event.as_ref())
            .await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %created_room.id,
            user_id = %created_by,
            "Room creation completed"
        );

        crate::metrics::http::ROOMS_ACTIVE.inc();

        self.permission_service
            .seed_added_member_cache(&created_room.id, &created_by, created_member.version)
            .await;

        let mut created_room = created_room;
        self.hydrate_room_taxonomy(&mut created_room).await?;

        Ok((created_room, created_member))
    }

    /// Transfer room ownership to another active member.
    pub async fn transfer_room_ownership(
        &self,
        room_id: RoomId,
        current_owner_id: UserId,
        new_owner_id: UserId,
    ) -> Result<Room> {
        self.transfer_room_ownership_with_outbox(room_id, current_owner_id, new_owner_id, None)
            .await
    }

    pub async fn transfer_room_ownership_with_outbox(
        &self,
        room_id: RoomId,
        current_owner_id: UserId,
        new_owner_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<Room> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.created_by != current_owner_id {
            return Err(Error::Authorization(
                "Only the current room owner can transfer ownership".to_string(),
            ));
        }

        if current_owner_id == new_owner_id {
            return Err(Error::InvalidInput(
                "Room ownership is already assigned to this user".to_string(),
            ));
        }

        let new_owner = self.user_service.get_user(&new_owner_id).await?;
        if !new_owner.status.is_active() {
            return Err(Error::Authorization(
                "New room owner must be an active user".to_string(),
            ));
        }

        let new_owner_member = self
            .member_repo
            .get(&room_id, &new_owner_id)
            .await?
            .ok_or_else(|| {
                Error::InvalidInput(
                    "New room owner must already be an active member of this room".to_string(),
                )
            })?;

        if !new_owner_member.status.is_active() {
            return Err(Error::InvalidInput(
                "New room owner must already be an active member of this room".to_string(),
            ));
        }

        let current_owner_member = self
            .member_repo
            .get(&room_id, &current_owner_id)
            .await?
            .ok_or_else(|| {
                Error::Internal(
                    "Current room owner is missing the required creator membership".to_string(),
                )
            })?;

        let mut tx = self.pool.begin().await?;
        let current_owner_username =
            Self::membership_snapshot_username_tx(&mut tx, &current_owner_id).await?;
        self.enforce_room_ownership_limit_tx(&mut tx, &new_owner_id, Some(&room_id))
            .await?;
        self.ensure_room_name_available_for_creator_tx(&mut tx, &new_owner_id, &room.name)
            .await?;

        let current_owner_fence = self
            .begin_permission_write(&room_id, &current_owner_id, current_owner_member.version)
            .await?;
        let new_owner_fence = match self
            .begin_permission_write(&room_id, &new_owner_id, new_owner_member.version)
            .await
        {
            Ok(fence) => fence,
            Err(error) => {
                self.abort_permission_write(&current_owner_fence).await;
                return Err(error);
            }
        };

        let updated_room = self
            .room_repo
            .transfer_ownership_with_executor(&room_id, &new_owner_id, &mut *tx)
            .await;
        let updated_room = match updated_room {
            Ok(room) => room,
            Err(error) => {
                self.abort_permission_write(&current_owner_fence).await;
                self.abort_permission_write(&new_owner_fence).await;
                return Err(error);
            }
        };

        let updated_current_owner = if current_owner_fence.version() > 0 {
            match self
                .member_repo
                .update_role_with_exact_version_executor(
                    &room_id,
                    &current_owner_id,
                    RoomRole::Admin,
                    current_owner_member.version,
                    current_owner_fence.version(),
                    &mut *tx,
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.abort_permission_write(&current_owner_fence).await;
                    self.abort_permission_write(&new_owner_fence).await;
                    return Err(error);
                }
            }
        } else {
            match self
                .member_repo
                .update_role_with_version_executor(
                    &room_id,
                    &current_owner_id,
                    RoomRole::Admin,
                    current_owner_member.version,
                    &mut *tx,
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.abort_permission_write(&current_owner_fence).await;
                    self.abort_permission_write(&new_owner_fence).await;
                    return Err(error);
                }
            }
        };
        let updated_new_owner = if new_owner_fence.version() > 0 {
            match self
                .member_repo
                .update_role_with_exact_version_executor(
                    &room_id,
                    &new_owner_id,
                    RoomRole::Creator,
                    new_owner_member.version,
                    new_owner_fence.version(),
                    &mut *tx,
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.abort_permission_write(&current_owner_fence).await;
                    self.abort_permission_write(&new_owner_fence).await;
                    return Err(error);
                }
            }
        } else {
            match self
                .member_repo
                .update_role_with_version_executor(
                    &room_id,
                    &new_owner_id,
                    RoomRole::Creator,
                    new_owner_member.version,
                    &mut *tx,
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    self.abort_permission_write(&current_owner_fence).await;
                    self.abort_permission_write(&new_owner_fence).await;
                    return Err(error);
                }
            }
        };
        let current_owner_snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                current_owner_id,
                current_owner_id,
                Some(&updated_current_owner),
                Self::role_member_event_scope(),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(&current_owner_fence).await;
                self.abort_permission_write(&new_owner_fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(
                &mut tx,
                &current_owner_snapshot,
                outbox_event_factory.as_ref(),
            )
            .await
        {
            self.abort_permission_write(&current_owner_fence).await;
            self.abort_permission_write(&new_owner_fence).await;
            return Err(error);
        }
        let new_owner_snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                new_owner_id,
                current_owner_id,
                Some(&updated_new_owner),
                Self::role_member_event_scope(),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(&current_owner_fence).await;
                self.abort_permission_write(&new_owner_fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(
                &mut tx,
                &new_owner_snapshot,
                outbox_event_factory.as_ref(),
            )
            .await
        {
            self.abort_permission_write(&current_owner_fence).await;
            self.abort_permission_write(&new_owner_fence).await;
            return Err(error);
        }

        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&current_owner_fence).await;
            self.abort_permission_write(&new_owner_fence).await;
            return Err(error.into());
        }

        self.finalize_committed_permission_write_best_effort(
            &current_owner_fence,
            &room_id,
            &current_owner_id,
            updated_current_owner.version,
            "transfer_room_ownership_with_outbox:current_owner",
        )
        .await;
        self.finalize_committed_permission_write_best_effort(
            &new_owner_fence,
            &room_id,
            &new_owner_id,
            updated_new_owner.version,
            "transfer_room_ownership_with_outbox:new_owner",
        )
        .await;
        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &current_owner_id)
            .await;
        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &new_owner_id)
            .await;

        self.invalidate_room_caches(&room_id).await;
        self.notify_room_settings_invalidation(&room_id).await;

        self.audit_log(
            &current_owner_id,
            &current_owner_username,
            AuditAction::RoomOwnershipTransferred,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            serde_json::json!({
                "operation": "transfer_ownership",
                "previous_owner_id": current_owner_id,
                "new_owner_id": new_owner_id,
                "previous_owner_role": format!("{:?}", current_owner_member.role),
                "new_owner_previous_role": format!("{:?}", new_owner_member.role),
            }),
        )
        .await;

        Ok(updated_room)
    }
}
