use sqlx::{Postgres, Transaction};

use crate::{
    models::{AddMemberOptions, RoomId, User, UserId},
    Error, Result,
};

use super::RoomService;

impl RoomService {
    pub(super) async fn active_member_add_options(
        &self,
        room_id: &RoomId,
    ) -> Result<AddMemberOptions> {
        let room_settings = self.room_settings_repo.get(room_id).await?;
        Ok(AddMemberOptions::default().with_max_members(room_settings.max_members.0))
    }

    pub(super) fn validate_user_can_join(user: &User) -> Result<()> {
        if user.is_banned {
            return Err(Error::Authorization(
                "Target user cannot be added while banned".to_string(),
            ));
        }
        if !user.status.can_join_room() {
            return Err(Error::Authorization(format!(
                "Target user cannot be added while account status is {}",
                user.status
            )));
        }
        Ok(())
    }

    pub(super) async fn ensure_target_user_can_join(&self, target_user_id: &UserId) -> Result<()> {
        let target_user = self.user_service.get_user(target_user_id).await?;
        Self::validate_user_can_join(&target_user)
    }

    pub(super) async fn ensure_target_user_can_join_now_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        target_user_id: &UserId,
    ) -> Result<()> {
        let target_user = self
            .user_service
            .repository
            .get_by_id_for_update_with_executor(target_user_id, &mut **tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {target_user_id} not found")))?;

        Self::validate_user_can_join(&target_user)
    }

    pub(super) async fn ensure_room_can_admit_member_now_tx(
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        target_user_id: &UserId,
    ) -> Result<()> {
        let room_state = sqlx::query!(
            r"
            SELECT closed_at,
                   EXISTS (
                       SELECT 1
                       FROM room_bans rb
                       WHERE rb.room_id = rooms.id
                         AND rb.revoked_at IS NULL
                         AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                   ) AS is_banned,
                   EXISTS (
		                       SELECT 1
		                       FROM room_member_kick_cooldowns rmkc
		                       WHERE rmkc.room_id = rooms.id
	                         AND rmkc.user_id = $2
	                         AND rmkc.ends_at > CURRENT_TIMESTAMP
	                   ) AS is_target_in_kick_cooldown
	            FROM rooms
            WHERE id = $1
              AND deleted_at IS NULL
            FOR UPDATE
            ",
            room_id as &RoomId,
            target_user_id as &UserId,
        )
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room_state.closed_at.is_some() {
            return Err(Error::InvalidInput("Room is closed".to_string()));
        }
        let is_banned = room_state
            .is_banned
            .ok_or_else(|| Error::Internal("Room ban EXISTS query returned NULL".to_string()))?;
        if is_banned {
            return Err(Error::Authorization("Room is banned".to_string()));
        }
        let is_target_in_kick_cooldown =
            room_state.is_target_in_kick_cooldown.ok_or_else(|| {
                Error::Internal("Room kick cooldown EXISTS query returned NULL".to_string())
            })?;
        if is_target_in_kick_cooldown {
            return Err(Error::kick_cooldown_denied());
        }

        Ok(())
    }
}
