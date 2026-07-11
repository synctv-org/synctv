//! Type-safe room settings.

use crate::models::permission::{
    RoomAdminPermissionBits, RoomGuestPermissionBits, RoomMemberPermissionBits, RoomPermissionSet,
};
use crate::{models::SettingsValidationContext, Error, Result};

/// Core trait for room settings
///
/// Each setting type implements this trait.
pub trait RoomSetting: Sized + Send + Sync + 'static {
    /// The underlying value type
    type Value: Clone + Send + Sync + 'static;

    /// Get the underlying value
    fn value(&self) -> &Self::Value;

    /// Get mutable reference to the value
    fn value_mut(&mut self) -> &mut Self::Value;

    /// Validate the setting value (override for custom validation)
    fn validate(&self, _ctx: &SettingsValidationContext<'_>) -> Result<()> {
        Ok(())
    }

    /// Validate this setting with access to the complete room settings object.
    ///
    /// This is where cross-field rules owned by one concrete setting live.
    fn validate_in_settings(
        &self,
        _settings: &RoomSettings,
        ctx: &SettingsValidationContext<'_>,
    ) -> Result<()> {
        self.validate(ctx)
    }
}

/// Generates a typed room setting wrapper and validation implementation.
#[macro_export]
macro_rules! room_setting {
    ($name:ident, $ty:ty, $key:expr, $default:expr) => {
        $crate::room_setting!(@impl $name, $ty, $key, $default, |_v: &$ty, _ctx: &$crate::models::SettingsValidationContext<'_>| -> $crate::Result<()> { Ok(()) }, |_this: &$name, _settings: &$crate::models::room_settings::RoomSettings, ctx: &$crate::models::SettingsValidationContext<'_>| -> $crate::Result<()> { _this.validate(ctx) });
    };
    ($name:ident, $ty:ty, $key:expr, $default:expr, $validator:expr) => {
        $crate::room_setting!(@impl $name, $ty, $key, $default, $validator, |_this: &$name, _settings: &$crate::models::room_settings::RoomSettings, ctx: &$crate::models::SettingsValidationContext<'_>| -> $crate::Result<()> { _this.validate(ctx) });
    };
    ($name:ident, $ty:ty, $key:expr, $default:expr, $validator:expr, $settings_validator:expr) => {
        $crate::room_setting!(@impl $name, $ty, $key, $default, $validator, $settings_validator);
    };
    (@impl $name:ident, $ty:ty, $key:expr, $default:expr, $validator:expr, $settings_validator:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub $ty);

        impl $name {
            pub const KEY: &'static str = $key;

            #[must_use]
            pub const fn new(value: $ty) -> Self {
                Self(value)
            }

            /// Validate the parsed value (custom validator from macro invocation).
            fn validate_value(
                v: &$ty,
                ctx: &$crate::models::SettingsValidationContext<'_>,
            ) -> $crate::Result<()> {
                let validator = $validator;
                validator(v, ctx)
            }
        }

        impl $crate::models::room_settings::RoomSetting for $name {
            type Value = $ty;

            fn value(&self) -> &Self::Value {
                &self.0
            }

            fn value_mut(&mut self) -> &mut Self::Value {
                &mut self.0
            }

            fn validate(
                &self,
                ctx: &$crate::models::SettingsValidationContext<'_>,
            ) -> $crate::Result<()> {
                $name::validate_value(&self.0, ctx)
            }

            fn validate_in_settings(
                &self,
                settings: &$crate::models::room_settings::RoomSettings,
                ctx: &$crate::models::SettingsValidationContext<'_>,
            ) -> $crate::Result<()> {
                let validator = $settings_validator;
                validator(self, settings, ctx)
            }

        }

        impl std::default::Default for $name {
            fn default() -> Self {
                Self($default)
            }
        }
    };
}

room_setting!(ChatEnabled, bool, "chatEnabled", true);
room_setting!(AllowGuestJoin, bool, "allowGuestJoin", false);
room_setting!(RequireApproval, bool, "requireApproval", false);
room_setting!(AllowAutoJoin, bool, "allowAutoJoin", true);

/// Maximum allowed value for `max_members` setting (used in validator below)
const MAX_MEMBERS_LIMIT: u64 = 10_000;

room_setting!(
    MaxMembers,
    u64,
    "maxMembers",
    100,
    |v: &u64, _ctx: &SettingsValidationContext<'_>| {
        if *v > MAX_MEMBERS_LIMIT {
            Err(crate::Error::InvalidInput(format!(
                "max_members cannot exceed {MAX_MEMBERS_LIMIT}"
            )))
        } else {
            Ok(())
        }
    }
);

impl MaxMembers {
    /// Maximum allowed value for `max_members` setting
    pub const MAX: u64 = MAX_MEMBERS_LIMIT;
}

room_setting!(
    AdminAddedPermissions,
    u64,
    "adminAddedPermissions",
    0,
    |_v: &u64, _ctx: &SettingsValidationContext<'_>| Ok(()),
    |_this: &AdminAddedPermissions,
     settings: &RoomSettings,
     _ctx: &SettingsValidationContext<'_>| settings.validate_admin_permission_bits()
);
room_setting!(
    AdminRemovedPermissions,
    u64,
    "adminRemovedPermissions",
    0,
    |_v: &u64, _ctx: &SettingsValidationContext<'_>| Ok(()),
    |_this: &AdminRemovedPermissions,
     settings: &RoomSettings,
     _ctx: &SettingsValidationContext<'_>| settings.validate_admin_permission_bits()
);
room_setting!(
    MemberAddedPermissions,
    u64,
    "memberAddedPermissions",
    0,
    |_v: &u64, _ctx: &SettingsValidationContext<'_>| Ok(()),
    |_this: &MemberAddedPermissions,
     settings: &RoomSettings,
     _ctx: &SettingsValidationContext<'_>| settings.validate_member_permission_bits()
);
room_setting!(
    MemberRemovedPermissions,
    u64,
    "memberRemovedPermissions",
    0,
    |_v: &u64, _ctx: &SettingsValidationContext<'_>| Ok(()),
    |_this: &MemberRemovedPermissions,
     settings: &RoomSettings,
     _ctx: &SettingsValidationContext<'_>| settings.validate_member_permission_bits()
);
room_setting!(
    GuestAddedPermissions,
    u64,
    "guestAddedPermissions",
    0,
    |_v: &u64, _ctx: &SettingsValidationContext<'_>| Ok(()),
    |_this: &GuestAddedPermissions,
     settings: &RoomSettings,
     _ctx: &SettingsValidationContext<'_>| settings.validate_guest_permission_bits()
);
room_setting!(
    GuestRemovedPermissions,
    u64,
    "guestRemovedPermissions",
    0,
    |_v: &u64, _ctx: &SettingsValidationContext<'_>| Ok(()),
    |_this: &GuestRemovedPermissions,
     settings: &RoomSettings,
     _ctx: &SettingsValidationContext<'_>| settings.validate_guest_permission_bits()
);

use crate::models::room::AutoPlaySettings;

/// Auto play settings (complex type)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
#[derive(Default)]
pub struct AutoPlay {
    pub value: AutoPlaySettings,
}

impl AutoPlay {
    #[must_use]
    pub const fn new(value: AutoPlaySettings) -> Self {
        Self { value }
    }
}

impl RoomSetting for AutoPlay {
    type Value = AutoPlaySettings;

    fn value(&self) -> &AutoPlaySettings {
        &self.value
    }

    fn value_mut(&mut self) -> &mut AutoPlaySettings {
        &mut self.value
    }

    fn validate(&self, _ctx: &SettingsValidationContext<'_>) -> Result<()> {
        if self.value.delay > 86_400 {
            return Err(Error::InvalidInput(
                "autoPlay.delay cannot exceed 86400 seconds".to_string(),
            ));
        }
        Ok(())
    }
}

use serde::{Deserialize, Serialize};

/// Room settings composed of individual type-safe settings
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct RoomSettings {
    pub allow_guest_join: AllowGuestJoin,
    pub max_members: MaxMembers,
    #[serde(default)]
    pub require_approval: RequireApproval,
    #[serde(default)]
    pub allow_auto_join: AllowAutoJoin,
    pub chat_enabled: ChatEnabled,
    #[serde(default)]
    pub auto_play: AutoPlay,
    #[serde(default)]
    pub admin_added_permissions: AdminAddedPermissions,
    #[serde(default)]
    pub admin_removed_permissions: AdminRemovedPermissions,
    #[serde(default)]
    pub member_added_permissions: MemberAddedPermissions,
    #[serde(default)]
    pub member_removed_permissions: MemberRemovedPermissions,
    #[serde(default)]
    pub guest_added_permissions: GuestAddedPermissions,
    #[serde(default)]
    pub guest_removed_permissions: GuestRemovedPermissions,
}

#[derive(Debug, Default, Clone)]
pub struct RoomSettingsPatch {
    pub allow_guest_join: Option<AllowGuestJoin>,
    pub max_members: Option<MaxMembers>,
    pub require_approval: Option<RequireApproval>,
    pub allow_auto_join: Option<AllowAutoJoin>,
    pub chat_enabled: Option<ChatEnabled>,
    pub auto_play: Option<AutoPlay>,
    pub admin_added_permissions: Option<AdminAddedPermissions>,
    pub admin_removed_permissions: Option<AdminRemovedPermissions>,
    pub member_added_permissions: Option<MemberAddedPermissions>,
    pub member_removed_permissions: Option<MemberRemovedPermissions>,
    pub guest_added_permissions: Option<GuestAddedPermissions>,
    pub guest_removed_permissions: Option<GuestRemovedPermissions>,
}

impl RoomSettings {
    /// Validate all room setting fields. Each concrete setting owns its own
    /// value-level and whole-settings validation.
    pub fn validate(&self, ctx: &SettingsValidationContext<'_>) -> Result<()> {
        self.allow_guest_join.validate_in_settings(self, ctx)?;
        self.max_members.validate_in_settings(self, ctx)?;
        self.require_approval.validate_in_settings(self, ctx)?;
        self.allow_auto_join.validate_in_settings(self, ctx)?;
        self.chat_enabled.validate_in_settings(self, ctx)?;
        self.auto_play.validate_in_settings(self, ctx)?;
        self.admin_added_permissions
            .validate_in_settings(self, ctx)?;
        self.admin_removed_permissions
            .validate_in_settings(self, ctx)?;
        self.member_added_permissions
            .validate_in_settings(self, ctx)?;
        self.member_removed_permissions
            .validate_in_settings(self, ctx)?;
        self.guest_added_permissions
            .validate_in_settings(self, ctx)?;
        self.guest_removed_permissions
            .validate_in_settings(self, ctx)
    }

    pub fn apply_patch(
        &mut self,
        patch: RoomSettingsPatch,
        ctx: &SettingsValidationContext<'_>,
    ) -> Result<()> {
        let mut next = self.clone();
        next.merge_patch(patch);
        next.validate(ctx)?;
        *self = next;
        Ok(())
    }

    pub fn merge_patch(&mut self, patch: RoomSettingsPatch) {
        if let Some(value) = patch.allow_guest_join {
            self.allow_guest_join = value;
        }
        if let Some(value) = patch.max_members {
            self.max_members = value;
        }
        if let Some(value) = patch.require_approval {
            self.require_approval = value;
        }
        if let Some(value) = patch.allow_auto_join {
            self.allow_auto_join = value;
        }
        if let Some(value) = patch.chat_enabled {
            self.chat_enabled = value;
        }
        if let Some(value) = patch.auto_play {
            self.auto_play = value;
        }
        if let Some(value) = patch.admin_added_permissions {
            self.admin_added_permissions = value;
        }
        if let Some(value) = patch.admin_removed_permissions {
            self.admin_removed_permissions = value;
        }
        if let Some(value) = patch.member_added_permissions {
            self.member_added_permissions = value;
        }
        if let Some(value) = patch.member_removed_permissions {
            self.member_removed_permissions = value;
        }
        if let Some(value) = patch.guest_added_permissions {
            self.guest_added_permissions = value;
        }
        if let Some(value) = patch.guest_removed_permissions {
            self.guest_removed_permissions = value;
        }
    }

    /// Get effective permissions for Admin role
    ///
    /// Formula: (`global_default` | added) & ~removed
    #[must_use]
    pub const fn admin_permissions(&self, global_default: RoomPermissionSet) -> RoomPermissionSet {
        let mut result = global_default.0;
        result |= RoomAdminPermissionBits::to_permissions(self.admin_added_permissions.0);
        result &= !RoomAdminPermissionBits::to_permissions(self.admin_removed_permissions.0);
        RoomPermissionSet(result)
    }

    /// Get effective permissions for Member role
    #[must_use]
    pub const fn member_permissions(&self, global_default: RoomPermissionSet) -> RoomPermissionSet {
        let mut result = global_default.0;
        result |= RoomMemberPermissionBits::to_permissions(self.member_added_permissions.0);
        result &= !RoomMemberPermissionBits::to_permissions(self.member_removed_permissions.0);
        RoomPermissionSet(result)
    }

    /// Get effective permissions for Guest
    #[must_use]
    pub const fn guest_permissions(&self, global_default: RoomPermissionSet) -> RoomPermissionSet {
        let mut result = global_default.0 & RoomPermissionSet::guest_assignable().0;
        result |= RoomGuestPermissionBits::to_permissions(self.guest_added_permissions.0);
        result &= !RoomGuestPermissionBits::to_permissions(self.guest_removed_permissions.0);
        RoomPermissionSet(result)
    }

    fn validate_admin_permission_bits(&self) -> Result<()> {
        if !RoomAdminPermissionBits::includes_only_defined(self.admin_added_permissions.0)
            || !RoomAdminPermissionBits::includes_only_defined(self.admin_removed_permissions.0)
        {
            return Err(Error::InvalidInput(
                "Room admin permission defaults contain bits outside the admin permission bitspace"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn validate_member_permission_bits(&self) -> Result<()> {
        if !RoomMemberPermissionBits::includes_only_defined(self.member_added_permissions.0)
            || !RoomMemberPermissionBits::includes_only_defined(self.member_removed_permissions.0)
        {
            return Err(Error::InvalidInput(
                "Room member permission defaults contain bits outside the member permission bitspace"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn validate_guest_permission_bits(&self) -> Result<()> {
        if !RoomGuestPermissionBits::includes_only_defined(self.guest_added_permissions.0)
            || !RoomGuestPermissionBits::includes_only_defined(self.guest_removed_permissions.0)
        {
            return Err(Error::InvalidInput(
                "Room guest permission defaults contain bits outside the guest permission bitspace"
                    .to_string(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn with_context<R>(f: impl FnOnce(&SettingsValidationContext<'_>) -> R) -> R {
        SettingsValidationContext::with_strict_policy(f)
    }

    #[test]
    fn test_apply_typed_patch() {
        let mut settings = RoomSettings::default();
        assert!(settings.chat_enabled.0);

        ok(
            with_context(|ctx| {
                settings.apply_patch(
                    RoomSettingsPatch {
                        chat_enabled: Some(ChatEnabled(false)),
                        max_members: Some(MaxMembers(42)),
                        ..Default::default()
                    },
                    ctx,
                )
            }),
            "typed room settings patch should apply",
        );
        assert!(!settings.chat_enabled.0);
        assert_eq!(settings.max_members.0, 42);
    }

    #[test]
    fn test_apply_typed_patch_validates_final_settings() {
        let mut settings = RoomSettings::default();
        let result = with_context(|ctx| {
            settings.apply_patch(
                RoomSettingsPatch {
                    max_members: Some(MaxMembers(MaxMembers::MAX + 1)),
                    ..Default::default()
                },
                ctx,
            )
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_admin_permissions_with_added() {
        let settings = RoomSettings {
            admin_added_permissions: AdminAddedPermissions(RoomAdminPermissionBits::PLAY_CONTROL),
            ..Default::default()
        };
        let global = RoomPermissionSet::default_member();
        let result = settings.admin_permissions(global);
        assert!(result.has(crate::models::RoomPermission::PLAY_CONTROL));
        assert!(result.has(crate::models::RoomPermission::CHAT));
    }

    #[test]
    fn test_member_permissions_with_removed() {
        let settings = RoomSettings {
            member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::CHAT),
            ..Default::default()
        };
        let global = RoomPermissionSet::default_member();
        let result = settings.member_permissions(global);
        assert!(!result.has(crate::models::RoomPermission::CHAT));
        assert!(result.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_guest_permissions_with_added_and_removed() {
        let settings = RoomSettings {
            // Give guests additional guest-level abilities.
            guest_added_permissions: GuestAddedPermissions(
                RoomGuestPermissionBits::USE_WEBRTC | RoomGuestPermissionBits::VIEW_MEMBER_LIST,
            ),
            // But remove one of them.
            guest_removed_permissions: GuestRemovedPermissions(
                RoomGuestPermissionBits::VIEW_MEMBER_LIST,
            ),
            ..Default::default()
        };
        let global = RoomPermissionSet::default_guest();
        let result = settings.guest_permissions(global);
        assert!(result.has(crate::models::RoomPermission::USE_WEBRTC));
        assert!(!result.has(crate::models::RoomPermission::VIEW_MEMBER_LIST));
        assert!(!result.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn max_members_validation_accepts_zero_and_limit() {
        with_context(|ctx| {
            assert!(RoomSettings {
                max_members: MaxMembers(0),
                ..Default::default()
            }
            .validate(ctx)
            .is_ok());
            assert!(RoomSettings {
                max_members: MaxMembers(MaxMembers::MAX),
                ..Default::default()
            }
            .validate(ctx)
            .is_ok());
            assert!(RoomSettings {
                max_members: MaxMembers(MaxMembers::MAX + 1),
                ..Default::default()
            }
            .validate(ctx)
            .is_err());
        });
    }

    #[test]
    fn apply_to_max_members_rejects_over_limit() {
        let mut settings = RoomSettings::default();
        let result = with_context(|ctx| {
            settings.apply_patch(
                RoomSettingsPatch {
                    max_members: Some(MaxMembers(99999)),
                    ..Default::default()
                },
                ctx,
            )
        });
        assert!(result.is_err());
        assert_eq!(settings.max_members.0, 100);
    }

    #[test]
    fn room_settings_validation_rejects_over_limit_max_members() {
        let settings = RoomSettings {
            max_members: MaxMembers(MaxMembers::MAX + 1),
            ..RoomSettings::default()
        };

        let result = with_context(|ctx| settings.validate(ctx));

        assert!(result.is_err());
    }
}
