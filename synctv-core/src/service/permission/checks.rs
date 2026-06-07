use crate::{
    models::{RoomId, RoomPermission, RoomPermissionSet, RoomRole, UserId},
    service::permission::PermissionService,
    Error, Result,
};

impl PermissionService {
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

    pub async fn is_creator(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let member = self.member_repo()?.get(room_id, user_id).await?;

        Ok(member.is_some_and(|m| m.role == RoomRole::Creator))
    }

    pub async fn is_admin_or_creator(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let member = self.member_repo()?.get(room_id, user_id).await?;

        Ok(member.is_some_and(|m| matches!(m.role, RoomRole::Admin | RoomRole::Creator)))
    }
}
