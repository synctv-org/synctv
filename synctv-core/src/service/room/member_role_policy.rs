use crate::{
    models::{
        Room, RoomAdminPermissionBits, RoomGuestPermissionBits, RoomMemberPermissionBits, RoomRole,
        UserId,
    },
    Error, Result,
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
    if effective_is_admin && (added_permissions > 0 || removed_permissions > 0) {
        return Err(Error::Authorization(
            "Admin members must use admin_added_permissions/admin_removed_permissions".to_string(),
        ));
    }

    if !effective_is_admin && (admin_added_permissions > 0 || admin_removed_permissions > 0) {
        return Err(Error::Authorization(
            "Only admin members use admin_added_permissions/admin_removed_permissions".to_string(),
        ));
    }

    Ok(())
}
