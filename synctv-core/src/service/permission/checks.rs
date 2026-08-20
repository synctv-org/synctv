use std::collections::HashSet;

use crate::{
    models::{RoomId, RoomPermission, RoomPermissionSet, RoomRole, UserId},
    Error, Result,
};

use super::PermissionService;

impl PermissionService {
    pub async fn ensure_resource_creator_is_available(
        &self,
        room_id: &RoomId,
        creator_id: Option<&UserId>,
        resource_kind: &'static str,
    ) -> Result<()> {
        let Some(creator_id) = creator_id else {
            return Ok(());
        };

        if self
            .available_resource_creator_pairs(&[(*room_id, *creator_id)])
            .await?
            .contains(&(*room_id, *creator_id))
        {
            return Ok(());
        }

        Err(Error::Authorization(format!(
            "{resource_kind} is unavailable because its creator cannot provide room resources"
        )))
    }

    pub async fn available_resource_creator_pairs(
        &self,
        pairs: &[(RoomId, UserId)],
    ) -> Result<HashSet<(RoomId, UserId)>> {
        self.available_resource_creator_pairs_with_executor(pairs, self.member_repo()?.pool())
            .await
    }

    pub(crate) async fn available_resource_creator_pairs_with_executor<'e, E>(
        &self,
        pairs: &[(RoomId, UserId)],
        executor: E,
    ) -> Result<HashSet<(RoomId, UserId)>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        if pairs.is_empty() {
            return Ok(HashSet::new());
        }

        let requested = pairs.iter().copied().collect::<HashSet<_>>();
        let room_ids = requested
            .iter()
            .map(|(room_id, _)| room_id.as_i64())
            .collect::<Vec<_>>();
        let user_ids = requested
            .iter()
            .map(|(_, user_id)| user_id.as_i64())
            .collect::<Vec<_>>();
        let rows = sqlx::query!(
            r#"
            SELECT rm.room_id AS "room_id!: RoomId", rm.user_id AS "user_id!: UserId"
            FROM room_members rm
            JOIN users u ON u.id = rm.user_id
            WHERE rm.room_id = ANY($1)
              AND rm.user_id = ANY($2)
              AND u.deleted_at IS NULL
              AND NOT EXISTS (
                  SELECT 1
                  FROM user_bans ub
                  WHERE ub.user_id = u.id
                    AND ub.revoked_at IS NULL
                    AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
              )
              AND NOT EXISTS (
                  SELECT 1
                  FROM room_member_kick_cooldowns cooldown
                  WHERE cooldown.room_id = rm.room_id
                    AND cooldown.user_id = rm.user_id
                    AND cooldown.ends_at > CURRENT_TIMESTAMP
              )
            "#,
            &room_ids,
            &user_ids,
        )
        .fetch_all(executor)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| (row.room_id, row.user_id))
            .filter(|pair| requested.contains(pair))
            .collect())
    }

    pub(crate) async fn lock_active_resource_creator_with_executor(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        executor: &mut sqlx::PgConnection,
    ) -> Result<()> {
        let locked_user_id = sqlx::query_scalar!(
            r#"
            SELECT id AS "id!: UserId"
            FROM users
            WHERE id = $1
            FOR KEY SHARE
            "#,
            user_id.as_i64(),
        )
        .fetch_optional(&mut *executor)
        .await?;
        if locked_user_id.is_none() {
            return Err(Error::Authorization(
                "Resource creator is no longer available".to_string(),
            ));
        }

        let locked_member_user_id = sqlx::query_scalar!(
            r#"
            SELECT rm.user_id AS "user_id!: UserId"
            FROM room_members rm
            WHERE rm.room_id = $1
              AND rm.user_id = $2
            FOR KEY SHARE
            "#,
            room_id.as_i64(),
            user_id.as_i64(),
        )
        .fetch_optional(&mut *executor)
        .await?;

        if locked_member_user_id.is_none() {
            return Err(Error::Authorization(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        }

        let active_creators = self
            .available_resource_creator_pairs_with_executor(&[(*room_id, *user_id)], &mut *executor)
            .await?;
        if active_creators.contains(&(*room_id, *user_id)) {
            return Ok(());
        }

        Err(Error::Authorization(
            "Resource creator is no longer active in this room".to_string(),
        ))
    }

    async fn ensure_room_accepts_member_actions(&self, room_id: &RoomId) -> Result<()> {
        let room = self
            .room_repo()?
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.is_banned {
            return Err(Error::Authorization("Room is banned".to_string()));
        }

        if !room.status.is_active() {
            return Err(Error::Authorization("Room is not active".to_string()));
        }

        Ok(())
    }

    pub async fn check_permission(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: RoomPermission,
    ) -> Result<()> {
        self.ensure_room_accepts_member_actions(room_id).await?;
        Self::ensure_permissions_contain(
            self.get_user_permissions_strong(room_id, user_id).await?,
            permission,
        )
    }

    pub async fn check_permission_no_cache(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: RoomPermission,
    ) -> Result<()> {
        self.ensure_room_accepts_member_actions(room_id).await?;
        Self::ensure_permissions_contain(
            self.get_user_permissions_no_cache(room_id, user_id).await?,
            permission,
        )
    }

    pub async fn check_permissions(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permissions: &[RoomPermission],
    ) -> Result<()> {
        let user_permissions = self.get_user_permissions_strong(room_id, user_id).await?;

        for &permission in permissions {
            Self::ensure_permissions_contain(user_permissions, permission)?;
        }

        Ok(())
    }

    fn ensure_permissions_contain(
        permissions: RoomPermissionSet,
        permission: RoomPermission,
    ) -> Result<()> {
        if permissions.has(permission) {
            return Ok(());
        }

        Err(Error::Authorization(
            synctv_common::messages::PERMISSION_DENIED.to_string(),
        ))
    }

    pub async fn check_role(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        expected_role: RoomRole,
    ) -> Result<()> {
        let member = self
            .member_repo()?
            .get(room_id, user_id)
            .await?
            .ok_or_else(|| {
                Error::Authorization(synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string())
            })?;

        if member.role != expected_role {
            return Err(Error::Authorization("Insufficient permissions".to_string()));
        }

        Ok(())
    }

    pub async fn is_admin_or_creator(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let member = self.member_repo()?.get(room_id, user_id).await?;

        Ok(member.is_some_and(|m| matches!(m.role, RoomRole::Admin | RoomRole::Creator)))
    }
}
