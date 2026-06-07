use crate::{
    models::{RoomMember, RoomMemberWithUser, RoomPermissionSet, RoomRole, RoomSettings},
    service::permission::{
        EffectivePermissionCalculator, PermissionService, RuntimePermissionDefaults,
    },
    Result,
};

impl PermissionService {
    fn get_global_default_permissions(&self, role: &RoomRole) -> RoomPermissionSet {
        if let Some(registry) = &self.settings_registry {
            match role {
                RoomRole::Admin => registry
                    .admin_default_permissions
                    .get()
                    .map_or(RoomPermissionSet::default_admin(), |permissions| {
                        permissions.bits()
                    }),
                RoomRole::Member => registry
                    .member_default_permissions
                    .get()
                    .map_or(RoomPermissionSet::default_member(), |permissions| {
                        permissions.bits()
                    }),
                RoomRole::Guest => registry
                    .guest_default_permissions
                    .get()
                    .map_or(RoomPermissionSet::default_guest(), |permissions| {
                        permissions.bits()
                    }),
                RoomRole::Creator => RoomPermissionSet::all(),
            }
        } else {
            match role {
                RoomRole::Admin => RoomPermissionSet::default_admin(),
                RoomRole::Member => RoomPermissionSet::default_member(),
                RoomRole::Guest => RoomPermissionSet::default_guest(),
                RoomRole::Creator => RoomPermissionSet::all(),
            }
        }
    }

    #[must_use]
    pub fn runtime_permission_defaults(&self) -> RuntimePermissionDefaults {
        RuntimePermissionDefaults {
            admin: self.get_global_default_permissions(&RoomRole::Admin),
            member: self.get_global_default_permissions(&RoomRole::Member),
            guest: self.get_global_default_permissions(&RoomRole::Guest),
        }
    }

    #[must_use]
    pub fn effective_permission_calculator(&self) -> EffectivePermissionCalculator {
        EffectivePermissionCalculator::new(self.runtime_permission_defaults())
    }

    #[must_use]
    pub fn effective_member_permissions(
        &self,
        member: &RoomMember,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        self.effective_permission_calculator()
            .effective_for_member(member, room_settings)
    }

    pub(super) fn runtime_permission_defaults_strong(&self) -> Result<RuntimePermissionDefaults> {
        let Some(registry) = &self.settings_registry else {
            return Ok(RuntimePermissionDefaults::compiled());
        };

        Ok(RuntimePermissionDefaults {
            admin: registry.admin_default_permissions.get()?.bits(),
            member: registry.member_default_permissions.get()?.bits(),
            guest: registry.guest_default_permissions.get()?.bits(),
        })
    }

    pub(super) fn effective_member_permissions_strong(
        &self,
        member: &RoomMember,
        room_settings: &RoomSettings,
    ) -> Result<RoomPermissionSet> {
        Ok(
            EffectivePermissionCalculator::new(self.runtime_permission_defaults_strong()?)
                .effective_for_member(member, room_settings),
        )
    }

    #[must_use]
    pub fn effective_member_with_user_permissions(
        &self,
        member: &RoomMemberWithUser,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        self.effective_permission_calculator()
            .effective_for_member_with_user(member, room_settings)
    }

    #[must_use]
    pub fn calculate_role_default_permissions(
        &self,
        role: &RoomRole,
        room_settings: &RoomSettings,
    ) -> RoomPermissionSet {
        self.effective_permission_calculator()
            .role_default(role, room_settings)
    }

    #[must_use]
    pub fn calculate_role_default_permissions_from_base(
        role: &RoomRole,
        room_settings: &RoomSettings,
        global_default: RoomPermissionSet,
    ) -> RoomPermissionSet {
        let defaults = RuntimePermissionDefaults {
            admin: global_default,
            member: global_default,
            guest: global_default,
        };
        EffectivePermissionCalculator::new(defaults).role_default(role, room_settings)
    }
}
