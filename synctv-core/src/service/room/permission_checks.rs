use crate::{
    models::{RoomId, RoomMember, RoomPermission, RoomRole, RoomSettings, UserId},
    service::{PermissionService, RoomService},
    Error, Result,
};

impl RoomService {
    fn permission_tx_checker(&self) -> RoomPermissionTxChecker<'_> {
        RoomPermissionTxChecker {
            permission_service: &self.permission_service,
        }
    }

    pub(super) async fn has_room_permission_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        permission: RoomPermission,
    ) -> Result<bool> {
        self.permission_tx_checker()
            .has_room_permission_in_tx(tx, room_id, user_id, permission)
            .await
    }

    pub(super) async fn ensure_actor_has_room_permission_now_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        actor_id: &UserId,
        permission: RoomPermission,
    ) -> Result<()> {
        self.permission_tx_checker()
            .ensure_actor_has_room_permission_now_tx(tx, room_id, actor_id, permission)
            .await
    }
}

struct RoomPermissionTxChecker<'a> {
    permission_service: &'a PermissionService,
}

impl RoomPermissionTxChecker<'_> {
    async fn has_room_permission_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        permission: RoomPermission,
    ) -> Result<bool> {
        let row = sqlx::query!(
            r#"
            SELECT rm.role,
                   rm.added_permissions,
                   rm.removed_permissions,
                   rm.admin_added_permissions,
                   rm.admin_removed_permissions,
                   rs.settings AS "settings?: RoomSettings"
            FROM room_members rm
            LEFT JOIN room_settings rs
              ON rs.room_id = rm.room_id
            WHERE rm.room_id = $1
              AND rm.user_id = $2
              AND NOT EXISTS (
                  SELECT 1
                  FROM room_member_kick_cooldowns rmkc
                  WHERE rmkc.room_id = rm.room_id
                    AND rmkc.user_id = rm.user_id
                    AND rmkc.ends_at > CURRENT_TIMESTAMP
            )
            FOR UPDATE OF rm
            "#,
            room_id.as_i64(),
            user_id.as_i64()
        )
        .fetch_optional(&mut **tx)
        .await?;

        let Some(row) = row else {
            return Ok(false);
        };

        let role = RoomRole::try_from(i32::from(row.role))
            .map_err(|error| Error::Internal(format!("Invalid room member role: {error}")))?;
        if role == RoomRole::Creator {
            return Ok(true);
        }

        let settings = row.settings.unwrap_or_default();

        let mut member = RoomMember::new(*room_id, *user_id, role);
        member.added_permissions = permission_bits_from_signed(row.added_permissions)?;
        member.removed_permissions = permission_bits_from_signed(row.removed_permissions)?;
        member.admin_added_permissions = permission_bits_from_signed(row.admin_added_permissions)?;
        member.admin_removed_permissions =
            permission_bits_from_signed(row.admin_removed_permissions)?;

        let permissions = self
            .permission_service
            .effective_permission_calculator()
            .effective_for_member(&member, &settings);

        Ok(permissions.has(permission))
    }

    async fn ensure_actor_has_room_permission_now_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        actor_id: &UserId,
        permission: RoomPermission,
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
                   ) AS is_banned
            FROM rooms
            WHERE id = $1
              AND deleted_at IS NULL
            FOR UPDATE
            ",
            room_id as &RoomId,
        )
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let is_banned = room_state
            .is_banned
            .ok_or_else(|| Error::Internal("Room ban EXISTS query returned NULL".to_string()))?;
        if is_banned {
            return Err(Error::Authorization("Room is banned".to_string()));
        }
        if room_state.closed_at.is_some() {
            return Err(Error::Authorization("Room is not active".to_string()));
        }

        if !self
            .has_room_permission_in_tx(tx, room_id, actor_id, permission)
            .await?
        {
            return Err(Error::Authorization(
                synctv_common::messages::PERMISSION_DENIED.to_string(),
            ));
        }

        Ok(())
    }
}

pub(crate) async fn has_active_room_membership_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<bool> {
    let exists = sqlx::query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM room_members rm
            WHERE rm.room_id = $1
              AND rm.user_id = $2
              AND NOT EXISTS (
                  SELECT 1
                  FROM room_member_kick_cooldowns rmkc
                  WHERE rmkc.room_id = rm.room_id
                    AND rmkc.user_id = rm.user_id
                    AND rmkc.ends_at > CURRENT_TIMESTAMP
              )
            FOR UPDATE
        ) AS "exists!"
        "#,
        room_id.as_i64(),
        user_id.as_i64()
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(exists)
}

fn permission_bits_from_signed(bits: i64) -> Result<u64> {
    u64::try_from(bits).map_err(|error| {
        Error::Internal(format!(
            "Invalid negative permission bitmask loaded from database: {error}"
        ))
    })
}

#[cfg(test)]
fn effective_room_permissions_from_base(
    settings: &RoomSettings,
    member: &RoomMember,
    global_default: crate::models::RoomPermissionSet,
) -> crate::models::RoomPermissionSet {
    let calculator = crate::service::EffectivePermissionCalculator::new(
        crate::service::RuntimePermissionDefaults {
            admin: global_default,
            member: global_default,
            guest: global_default,
        },
    );
    calculator.effective_for_member(member, settings)
}

#[cfg(test)]
pub(crate) fn has_room_permission_from_base(
    settings: &RoomSettings,
    member: &RoomMember,
    global_default: crate::models::RoomPermissionSet,
    permission: RoomPermission,
) -> bool {
    if !member.has_permission(permission, crate::models::RoomPermissionSet::all()) {
        return false;
    }

    effective_room_permissions_from_base(settings, member, global_default).has(permission)
}
