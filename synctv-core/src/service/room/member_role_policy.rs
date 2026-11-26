mod overrides;

use crate::{
    Error, Result,
    models::{
        Room, RoomAdminPermissionBits, RoomGuestPermissionBits, RoomMemberPermissionBits, RoomRole,
        UserId,
    },
};

pub(in crate::service::room) fn validate_role_can_be_assigned(role: RoomRole) -> Result<()> {
    if role == RoomRole::Creator {
        return Err(Error::InvalidInput(
            "Creator role is bound to room ownership and cannot be assigned via set_member_role"
                .to_string(),
        ));
    }

    Ok(())
}

/// Elevated roles are creator-conferred (the same invariant as
/// `validate_creator_member_role_update` and join-request review): a
/// non-creator ADD_MEMBERS holder may only admit Member or Guest members.
pub(in crate::service::room) fn validate_add_member_role(
    room: &Room,
    actor_id: &UserId,
    role: RoomRole,
) -> Result<()> {
    validate_role_can_be_assigned(role)?;
    if role == RoomRole::Admin && room.created_by != *actor_id {
        return Err(Error::Authorization(
            "Only room creator can grant the admin role".to_string(),
        ));
    }

    Ok(())
}

pub(in crate::service::room) fn validate_creator_member_role_update(
    room: &Room,
    actor_id: &UserId,
    target_user_id: &UserId,
) -> Result<()> {
    if room.created_by != *actor_id {
        return Err(Error::Authorization(
            "Only room creator can change member roles".to_string(),
        ));
    }

    validate_admin_member_role_update(room, target_user_id)
}

pub(in crate::service::room) fn validate_admin_member_role_update(
    room: &Room,
    target_user_id: &UserId,
) -> Result<()> {
    if *target_user_id == room.created_by {
        return Err(Error::InvalidInput(
            "Cannot change the role of the room creator via set_member_role".to_string(),
        ));
    }

    Ok(())
}

/// Permission overrides are creator-conferred like the roles they attach to:
/// nobody may write their own overrides (the self-edit kick already follows),
/// and overrides on elevated members (admin or creator) are writable only by
/// the room creator. Mirrors the strict-outranking rule of kick.
pub(in crate::service) fn validate_member_permission_update_actor(
    room: &Room,
    actor_id: &UserId,
    target_user_id: &UserId,
    target_is_elevated: bool,
) -> Result<()> {
    overrides::check_actor(
        room.created_by.get(),
        actor_id.get(),
        target_user_id.get(),
        target_is_elevated,
    )
    .map_err(|err| {
        Error::Authorization(
            match err {
                overrides::ActorError::SelfEdit => "Cannot change your own member permissions",
                overrides::ActorError::ElevatedTarget => {
                    "Only room creator can change permissions of elevated members"
                }
            }
            .to_string(),
        )
    })
}

pub(in crate::service::room) fn validate_admin_override_bits(
    admin_added_permissions: u64,
    admin_removed_permissions: u64,
) -> Result<()> {
    if !RoomAdminPermissionBits::includes_only_defined(admin_added_permissions)
        || !RoomAdminPermissionBits::includes_only_defined(admin_removed_permissions)
    {
        return Err(Error::InvalidInput(
            "Permission set includes bits outside the target role permission bitspace".to_string(),
        ));
    }

    Ok(())
}

pub(in crate::service::room) fn validate_override_bits_for_role(
    role: RoomRole,
    added_permissions: u64,
    removed_permissions: u64,
) -> Result<()> {
    let valid = match role {
        RoomRole::Creator | RoomRole::Admin => {
            RoomAdminPermissionBits::includes_only_defined(added_permissions)
                && RoomAdminPermissionBits::includes_only_defined(removed_permissions)
        }
        RoomRole::Member => {
            RoomMemberPermissionBits::includes_only_defined(added_permissions)
                && RoomMemberPermissionBits::includes_only_defined(removed_permissions)
        }
        RoomRole::Guest => {
            RoomGuestPermissionBits::includes_only_defined(added_permissions)
                && RoomGuestPermissionBits::includes_only_defined(removed_permissions)
        }
    };

    if valid {
        Ok(())
    } else {
        Err(Error::InvalidInput(
            "Permission set includes bits outside the target role permission bitspace".to_string(),
        ))
    }
}

pub(in crate::service::room) fn validate_member_permission_override_channel(
    effective_is_admin: bool,
    added_permissions: u64,
    removed_permissions: u64,
    admin_added_permissions: u64,
    admin_removed_permissions: u64,
) -> Result<()> {
    if overrides::valid_channel(
        effective_is_admin,
        added_permissions,
        removed_permissions,
        admin_added_permissions,
        admin_removed_permissions,
    ) {
        Ok(())
    } else {
        let message = if effective_is_admin {
            "Admin members must use admin_added_permissions/admin_removed_permissions"
        } else {
            "Only admin members use admin_added_permissions/admin_removed_permissions"
        };
        Err(Error::Authorization(message.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::validate_member_permission_update_actor;
    use crate::{Error, models::UserId, test_helpers::RoomFixture};

    #[test]
    fn member_permission_update_rejects_self_edit() {
        let creator = UserId::new();
        let admin = UserId::new();
        let room = RoomFixture::new().with_owner(creator).build();

        assert!(matches!(
            validate_member_permission_update_actor(&room, &admin, &admin, true),
            Err(Error::Authorization(_))
        ));
        assert!(matches!(
            validate_member_permission_update_actor(&room, &creator, &creator, true),
            Err(Error::Authorization(_))
        ));
    }

    #[test]
    fn member_permission_update_on_elevated_target_requires_creator() {
        let creator = UserId::new();
        let admin = UserId::new();
        let other_admin = UserId::new();
        let member = UserId::new();
        let room = RoomFixture::new().with_owner(creator).build();

        assert!(matches!(
            validate_member_permission_update_actor(&room, &admin, &other_admin, true),
            Err(Error::Authorization(_))
        ));
        assert!(validate_member_permission_update_actor(&room, &admin, &creator, true).is_err());
        assert!(validate_member_permission_update_actor(&room, &creator, &admin, true).is_ok());
        // Non-elevated targets remain manageable by any permission holder.
        assert!(validate_member_permission_update_actor(&room, &admin, &member, false).is_ok());
    }
}
