use crate::models::{
    RoomMember, RoomMemberWithUser, RoomPermission, RoomPermissionSet, RoomRole, RoomSettings,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimePermissionDefaults {
    pub admin: RoomPermissionSet,
    pub member: RoomPermissionSet,
    pub guest: RoomPermissionSet,
}

impl RuntimePermissionDefaults {
    #[must_use]
    pub const fn compiled() -> Self {
        Self {
            admin: RoomPermissionSet::default_admin(),
            member: RoomPermissionSet::default_member(),
            guest: RoomPermissionSet::default_guest(),
        }
    }

    #[must_use]
    pub const fn for_role(self, role: &RoomRole) -> RoomPermissionSet {
        match role {
            RoomRole::Creator => RoomPermissionSet::all(),
            RoomRole::Admin => self.admin,
            RoomRole::Member => self.member,
            RoomRole::Guest => self.guest,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EffectivePermissionCalculator {
    defaults: RuntimePermissionDefaults,
}

impl EffectivePermissionCalculator {
    #[must_use]
    pub const fn new(defaults: RuntimePermissionDefaults) -> Self {
        Self { defaults }
    }

    #[must_use]
    pub const fn compiled_defaults() -> Self {
        Self::new(RuntimePermissionDefaults::compiled())
    }

    #[must_use]
    pub const fn role_default(
        &self,
        role: &RoomRole,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        match role {
            RoomRole::Creator => RoomPermissionSet::all(),
            RoomRole::Admin => room_settings.admin_permissions(self.defaults.admin),
            RoomRole::Member => room_settings.member_permissions(self.defaults.member),
            RoomRole::Guest => room_settings.guest_permissions(self.defaults.guest),
        }
    }

    #[must_use]
    pub const fn effective_for_member(
        &self,
        member: &RoomMember,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        member.effective_permissions(self.role_default(&member.role, room_settings))
    }

    #[must_use]
    pub fn effective_for_member_with_user(
        &self,
        member: &RoomMemberWithUser,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        member.effective_permissions(self.role_default(&member.role, room_settings))
    }

    #[must_use]
    pub const fn has_permission(
        &self,
        member: &RoomMember,
        room_settings: &RoomSettings,
        permission: RoomPermission,
    ) -> bool {
        if !member.has_permission(permission, RoomPermissionSet::all()) {
            return false;
        }

        self.effective_for_member(member, room_settings)
            .has(permission)
    }
}
