use crate::{
    models::{RoomId, UserId},
    repository::room_password::RoomPasswordCredentialState,
    Error, Result,
};

use super::RoomService;

impl RoomService {
    pub async fn admin_set_room_password(
        &self,
        room_id: &RoomId,
        new_password: Option<&str>,
        actor_user_id: Option<&UserId>,
    ) -> Result<RoomPasswordCredentialState> {
        let _room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.set_room_password_from_plaintext(room_id, actor_user_id, new_password)
            .await
    }
}
