use crate::{
    models::{Room, RoomId, RoomStatus, UserId, UserRole},
    Error, Result,
};

use super::RoomService;

#[derive(Debug, Clone)]
pub struct AuthorizedAdminActor {
    user_id: UserId,
    username: String,
}

impl AuthorizedAdminActor {
    pub fn new(user_id: UserId, username: String, role: UserRole) -> Result<Self> {
        if !role.is_admin_or_above() {
            return Err(Error::Authorization(
                "Admin role required for this operation".to_string(),
            ));
        }

        Ok(Self { user_id, username })
    }

    pub fn new_management(user_id: UserId, username: String, role: UserRole) -> Result<Self> {
        Self::new(user_id, username, role)
    }

    #[must_use]
    pub fn user_id(&self) -> &UserId {
        &self.user_id
    }

    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }
}

impl RoomService {
    pub(in crate::service::room) async fn load_authorized_admin_actor(
        &self,
        admin_user_id: &UserId,
    ) -> Result<AuthorizedAdminActor> {
        let admin_user = self.user_service.get_user(admin_user_id).await?;
        AuthorizedAdminActor::new(*admin_user_id, admin_user.username, admin_user.role)
    }

    /// Update room status from the admin plane.
    ///
    /// Validates the status transition before applying it. Rooms support
    /// `Active` and `Closed`; review workflows use dedicated request tables.
    pub async fn update_room_status(
        &self,
        room_id: &RoomId,
        new_status: RoomStatus,
    ) -> Result<Room> {
        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if !room.status.can_transition_to(&new_status) {
            return Err(Error::InvalidInput(format!(
                "Invalid status transition from {} to {}",
                room.status.as_str(),
                new_status.as_str()
            )));
        }

        let room = self.room_repo.update_status(room_id, new_status).await?;
        self.notify_room_invalidation(room_id).await;
        Ok(room)
    }

    /// Update room directly from the admin plane.
    pub async fn admin_update_room(&self, room: &Room, admin_user_id: &UserId) -> Result<Room> {
        let actor = self.load_authorized_admin_actor(admin_user_id).await?;
        self.admin_update_room_as(room, &actor).await
    }

    pub async fn admin_update_room_as(
        &self,
        room: &Room,
        _actor: &AuthorizedAdminActor,
    ) -> Result<Room> {
        let old_version = room.version;

        crate::validation::RoomNameValidator::new()
            .validate(&room.name)
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        crate::validation::validate_room_description(&room.description)
            .map_err(|error| Error::InvalidInput(error.to_string()))?;

        let current = self
            .room_repo
            .get_by_id(&room.id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let mut tx = self.pool.begin().await?;
        if current.name != room.name {
            self.ensure_room_name_available_for_creator_tx(
                &mut tx,
                &current.created_by,
                &room.name,
            )
            .await?;
        }
        let updated = self
            .room_repo
            .update_with_executor(room, old_version, &mut *tx)
            .await?;
        tx.commit().await?;

        self.notify_room_invalidation(&room.id).await;
        Ok(updated)
    }
}
