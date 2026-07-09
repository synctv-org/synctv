use sqlx::{Postgres, Transaction};

use crate::{
    models::{ReviewStatus, RoomId, UserId},
    repository::RoomRepository,
    service::{room::RoomService, RoomPasswordPolicy},
    Error, Result,
};

#[derive(Debug, Clone, Copy)]
pub(super) struct RoomCreationPolicy {
    pub(super) enforce_creation_toggle: bool,
}

pub(super) fn runtime_settings_store_unavailable_for_room_creation() -> Error {
    Error::ServiceUnavailable("Room creation policy is temporarily unavailable".to_string())
}

const ROOM_NAME_POLICY_LOCK_NS: i32 = 20_260_420;
const ROOM_OWNER_POLICY_LOCK_NS: i32 = 20_260_421;

impl RoomService {
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
        if let Some(ref registry) = self.runtime_settings_store {
            if policy.enforce_creation_toggle && !registry.room_creation.enabled.get()? {
                tracing::warn!(user_id = %user_id, "Room creation rejected: room_creation.enabled is false");
                return Err(Error::Authorization(
                    "Room creation is currently disabled".to_string(),
                ));
            }
            match registry.room_creation.password_policy.get()? {
                RoomPasswordPolicy::Required if !password_enabled => {
                    tracing::warn!(user_id = %user_id, "Room creation rejected: password required by room creation policy");
                    return Err(Error::InvalidInput(
                        "Room password is required by room creation policy".to_string(),
                    ));
                }
                RoomPasswordPolicy::Forbidden if password_enabled => {
                    tracing::warn!(user_id = %user_id, "Room creation rejected: passwords forbidden by room creation policy");
                    return Err(Error::InvalidInput(
                        "Room password is not allowed by room creation policy".to_string(),
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
            .runtime_settings_store
            .as_ref()
            .map(|registry| registry.room_creation.max_rooms_per_user.get())
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
}

#[cfg(test)]
mod tests {
    use super::runtime_settings_store_unavailable_for_room_creation;
    use crate::Error;

    #[test]
    fn room_creation_policy_unavailable_error_is_service_unavailable() {
        let error = runtime_settings_store_unavailable_for_room_creation();

        assert!(matches!(
            error,
            Error::ServiceUnavailable(message)
                if message.contains("Room creation policy")
        ));
    }
}
