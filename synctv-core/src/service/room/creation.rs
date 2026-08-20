use crate::{
    models::{
        NotificationData, PageParams, Room, RoomCategoryId, RoomLabelId, RoomMember, RoomRole,
        RoomSettings, UserId, UserListQuery, UserStatus,
    },
    service::room::{RealtimeOutboxRoomEventFactory, RoomService},
    Error, Result,
};

use super::creation_policy::{
    runtime_settings_store_unavailable_for_room_creation, RoomCreationPolicy,
};
use super::creation_request::RoomCreationRequestDraft;

/// Accounts for password credential processing, database transaction latency,
/// and network delays under high load.
const CREATE_ROOM_LOCK_TTL_SECS: u64 = 30;

#[derive(Clone, Debug)]
pub struct CreateRoomWithTaxonomyRequest {
    pub name: String,
    pub description: String,
    pub created_by: UserId,
    pub password: Option<String>,
    pub settings: Option<RoomSettings>,
    pub category_id: Option<RoomCategoryId>,
    pub label_ids: Vec<RoomLabelId>,
    pub is_public: bool,
}

fn initial_room_settings(settings: Option<RoomSettings>) -> RoomSettings {
    settings.unwrap_or_default()
}

impl RoomService {
    /// All database operations run inside a single transaction so the room_creation is
    /// either fully created or not visible at all.
    pub async fn create_room(
        &self,
        name: String,
        description: String,
        created_by: UserId,
        password: Option<String>,
        settings: Option<RoomSettings>,
    ) -> Result<Room> {
        self.create_room_with_taxonomy_outbox(
            CreateRoomWithTaxonomyRequest {
                name,
                description,
                created_by,
                password,
                settings,
                category_id: None,
                label_ids: Vec::new(),
                is_public: true,
            },
            None,
        )
        .await
    }

    pub async fn create_room_with_taxonomy_outbox(
        &self,
        request: CreateRoomWithTaxonomyRequest,
        outbox_event_factory: Option<RealtimeOutboxRoomEventFactory>,
    ) -> Result<Room> {
        if let Some(ref lock) = self.distributed_lock {
            let created_by = request.created_by;
            let lock_key = format!("create_room:{created_by}");
            return crate::service::with_coordination_lock(
                lock.as_ref(),
                &lock_key,
                CREATE_ROOM_LOCK_TTL_SECS,
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
    ) -> Result<Room> {
        self.do_create_room_with_policy(request, true, outbox_event_factory)
            .await
    }

    async fn do_create_room_with_policy(
        &self,
        command: CreateRoomWithTaxonomyRequest,
        enforce_creation_policy: bool,
        outbox_event_factory: Option<RealtimeOutboxRoomEventFactory>,
    ) -> Result<Room> {
        let CreateRoomWithTaxonomyRequest {
            name,
            description,
            created_by,
            password,
            settings,
            category_id,
            label_ids,
            is_public,
        } = command;
        let password_enabled = password.is_some();
        let room_settings = initial_room_settings(settings);
        self.validate_room_settings(&room_settings)?;
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

        if let Err(error) = crate::validation::validate_room_description(&description) {
            tracing::warn!(
                user_id = %created_by,
                desc_len = description.chars().count(),
                "Attempted to create room with description too long"
            );
            return Err(Error::InvalidInput(error.to_string()));
        }
        if let Some(password) = password.as_deref() {
            crate::validation::validate_room_password_for_set(password)
                .map_err(|error| Error::InvalidInput(error.to_string()))?;
        }

        let need_review = if enforce_creation_policy {
            self.runtime_settings_store
                .as_ref()
                .ok_or_else(runtime_settings_store_unavailable_for_room_creation)?
                .room_creation
                .approval_required
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
                        is_public,
                    },
                )
                .await?;
            tx.commit().await?;
            if let Some(ref notif_service) = self.user_notification_service {
                let query = UserListQuery {
                    pagination: PageParams::new(Some(1), Some(100)),
                    search: None,
                    status: Some(UserStatus::Active),
                    role: None,
                    is_banned: None,
                    sort_by: crate::models::UserListSortBy::CreatedAt,
                    sort_direction: crate::models::SortDirection::Desc,
                    include_deleted: false,
                };
                let all_admins = match self.user_service.list_admins(&query).await {
                    Ok((users, _)) => users,
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "Failed to load admins for pending room creation notification"
                        );
                        Vec::new()
                    }
                };

                for admin in all_admins {
                    if let Err(e) = notif_service
                        .create_system_announcement(
                            admin.id,
                            format!("Room Pending Review: {name}"),
                            format!(
                                "User {created_by} requested room \"{name}\" which requires admin review."
                            ),
                            Some(NotificationData {
                                room_id: Some(pending_room.id.to_string()),
                                room_name: Some(name.clone()),
                                user_id: Some(created_by.to_string()),
                                ..Default::default()
                            }),
                        )
                        .await
                    {
                        tracing::warn!(
                            admin_id = %admin.id,
                            error = %e,
                            "Failed to notify admin about pending room creation"
                        );
                    }
                }
            }

            return Ok(pending_room);
        }

        let mut tx = self.pool.begin().await?;

        self.ensure_user_can_create_room_now_tx(&mut tx, &created_by)
            .await?;
        self.enforce_room_ownership_limit_tx(&mut tx, &created_by, None)
            .await?;
        self.ensure_room_name_available_for_creator_tx(&mut tx, &created_by, &name)
            .await?;

        let mut room = Room::new_with_description(name, description, created_by);
        room.is_public = is_public;
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
                &super::password::room_opaque_credential_identifier(&created_room.id),
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

        crate::metrics::application::ROOMS_ACTIVE.inc();

        self.permission_service
            .seed_added_member_cache(&created_room.id, &created_by, created_member.version)
            .await;

        let mut created_room = created_room;
        self.hydrate_room_taxonomy(&mut created_room).await?;

        Ok(created_room)
    }
}
