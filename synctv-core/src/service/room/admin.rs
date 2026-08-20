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
}
