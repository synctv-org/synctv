//! Type-safe room settings with a static registry.

use crate::models::permission::{
    RoomAdminPermissionBits, RoomGuestPermissionBits, RoomMemberPermissionBits, RoomPermissionSet,
};
use crate::{Error, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// Trait for room setting operations (type-erased)
///
/// This trait provides a unified interface for working with room settings dynamically.
/// The static `RoomSettingsRegistry` lets callers validate, parse, and apply settings
/// by key without knowing the concrete type.
pub trait RoomSettingProvider: Send + Sync {
    /// Get the setting key
    fn key(&self) -> &'static str;

    /// Get the setting type name
    fn type_name(&self) -> &'static str;

    /// Validate a raw string value (for dynamic API validation)
    fn is_valid_raw(&self, value: &str) -> Result<()>;

    /// Parse raw string to the setting's value type
    fn parse_raw(&self, value: &str) -> Result<Box<dyn std::any::Any + Send + Sync>>;

    /// Get default value as string
    fn default_as_string(&self) -> Result<String>;

    /// Apply a raw string value to the corresponding field of `RoomSettings`.
    ///
    /// This is the key method that enables fully generic `set_by_key` without
    /// a match block — the registry dispatches through `dyn RoomSettingProvider`.
    fn apply_to(&self, settings: &mut RoomSettings, value: &str) -> Result<()>;
}

/// Global registry for all room setting types.
///
/// The set of room settings is fixed at compile time, so a lazy immutable map is
/// enough; no startup constructors or runtime mutation are required.
static REGISTRY: std::sync::LazyLock<HashMap<&'static str, Arc<dyn RoomSettingProvider>>> =
    std::sync::LazyLock::new(|| {
        [
            provider_entry(ChatEnabled::default()),
            provider_entry(AllowGuestJoin::default()),
            provider_entry(RequireApproval::default()),
            provider_entry(AllowAutoJoin::default()),
            provider_entry(MaxMembers::default()),
            provider_entry(AdminAddedPermissions::default()),
            provider_entry(AdminRemovedPermissions::default()),
            provider_entry(MemberAddedPermissions::default()),
            provider_entry(MemberRemovedPermissions::default()),
            provider_entry(GuestAddedPermissions::default()),
            provider_entry(GuestRemovedPermissions::default()),
            provider_entry(AutoPlay::default()),
        ]
        .into_iter()
        .collect()
    });

fn provider_entry<T>(provider: T) -> (&'static str, Arc<dyn RoomSettingProvider>)
where
    T: RoomSetting + RoomSettingProvider,
{
    (T::KEY, Arc::new(provider))
}

/// Global registry for all room setting types
pub struct RoomSettingsRegistry;

impl RoomSettingsRegistry {
    /// Get provider for a setting by key
    pub fn get_provider(key: &str) -> Option<Arc<dyn RoomSettingProvider>> {
        REGISTRY.get(key).cloned()
    }

    /// Get all registered setting keys
    pub fn all_keys() -> Vec<String> {
        REGISTRY.keys().map(ToString::to_string).collect()
    }

    /// Check if a setting exists
    pub fn has_key(key: &str) -> bool {
        REGISTRY.contains_key(key)
    }

    /// Validate a setting value by key (dynamic validation)
    pub fn validate_setting(key: &str, value: &str) -> Result<()> {
        let provider = Self::get_provider(key)
            .ok_or_else(|| Error::NotFound(format!("Setting '{key}' not found")))?;
        provider.is_valid_raw(value)
    }

    /// Apply a setting value to `RoomSettings` by key (fully generic, no match block).
    ///
    /// Looks up the provider by key, then delegates to `provider.apply_to()`.
    pub fn apply_setting(settings: &mut RoomSettings, key: &str, value: &str) -> Result<()> {
        let provider = Self::get_provider(key)
            .ok_or_else(|| Error::NotFound(format!("Unknown room setting: {key}")))?;
        provider.apply_to(settings, value)
    }
}

/// Core trait for room settings
///
/// Each setting type implements this trait.
pub trait RoomSetting: Sized + Send + Sync + 'static {
    /// Storage key in database
    const KEY: &'static str;

    /// The underlying value type
    type Value: Clone + Send + Sync + 'static;

    /// Get the underlying value
    fn value(&self) -> &Self::Value;

    /// Get mutable reference to the value
    fn value_mut(&mut self) -> &mut Self::Value;

    /// Validate the setting value (override for custom validation)
    fn validate(&self) -> Result<()> {
        Ok(())
    }

    /// Parse from string (for dynamic API validation)
    fn parse_from_str(value: &str) -> Result<Self::Value>;

    /// Format to string (for serialization)
    fn format_value(value: &Self::Value) -> Result<String>;

    /// Type name (for debugging/registry)
    const TYPE_NAME: &'static str;

    /// Get default value
    fn default_value() -> Self::Value;
}

/// Generates a room setting type and its dynamic provider implementation.
#[macro_export]
macro_rules! room_setting {
    ($name:ident, $ty:ty, $key:expr, $default:expr) => {
        $crate::room_setting!(@impl $name, $ty, $key, $default, |_v: &$ty| -> $crate::Result<()> { Ok(()) });
    };
    ($name:ident, $ty:ty, $key:expr, $default:expr, $validator:expr) => {
        $crate::room_setting!(@impl $name, $ty, $key, $default, $validator);
    };
    (@impl $name:ident, $ty:ty, $key:expr, $default:expr, $validator:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub $ty);

        impl $name {
            /// Validate the parsed value (custom validator from macro invocation).
            fn validate_value(v: &$ty) -> $crate::Result<()> {
                let validator = $validator;
                validator(v)
            }
        }

        impl $crate::models::room_settings::RoomSetting for $name {
            const KEY: &'static str = $key;
            const TYPE_NAME: &'static str = stringify!($name);
            type Value = $ty;

            fn value(&self) -> &Self::Value {
                &self.0
            }

            fn value_mut(&mut self) -> &mut Self::Value {
                &mut self.0
            }

            fn validate(&self) -> $crate::Result<()> {
                $name::validate_value(&self.0)
            }

            fn parse_from_str(value: &str) -> $crate::Result<$ty> {
                value.parse::<$ty>().map_err(|_| {
                    $crate::Error::InvalidInput(format!("Invalid value for {}: {}", $key, value))
                })
            }

            fn format_value(value: &$ty) -> $crate::Result<String> {
                Ok(value.to_string())
            }

            fn default_value() -> $ty {
                $default
            }
        }

        // Implement RoomSettingProvider for dynamic operations (including apply_to)
        impl $crate::models::room_settings::RoomSettingProvider for $name {
            fn key(&self) -> &'static str {
                <$name as $crate::models::room_settings::RoomSetting>::KEY
            }

            fn type_name(&self) -> &'static str {
                <$name as $crate::models::room_settings::RoomSetting>::TYPE_NAME
            }

            fn is_valid_raw(&self, value: &str) -> $crate::Result<()> {
                let parsed = Self::parse_from_str(value)?;
                $name::validate_value(&parsed)?;
                Ok(())
            }

            fn parse_raw(&self, value: &str) -> $crate::Result<Box<dyn std::any::Any + Send + Sync>> {
                let parsed = Self::parse_from_str(value)?;
                $name::validate_value(&parsed)?;
                Ok(Box::new(parsed))
            }

            fn default_as_string(&self) -> $crate::Result<String> {
                Self::format_value(
                    &<$name as $crate::models::room_settings::RoomSetting>::default_value(),
                )
            }

            fn apply_to(
                &self,
                settings: &mut $crate::models::room_settings::RoomSettings,
                value: &str,
            ) -> $crate::Result<()> {
                let parsed = Self::parse_from_str(value)?;
                $name::validate_value(&parsed)?;
                paste::paste! {
                    settings.[<$name:snake>] = $name(parsed);
                }
                Ok(())
            }
        }

        impl std::default::Default for $name {
            fn default() -> Self {
                Self($default)
            }
        }
    };
}

room_setting!(ChatEnabled, bool, "chat_enabled", true);
room_setting!(AllowGuestJoin, bool, "allow_guest_join", false);
room_setting!(RequireApproval, bool, "require_approval", false);
room_setting!(AllowAutoJoin, bool, "allow_auto_join", true);

/// Maximum allowed value for `max_members` setting (used in validator below)
const MAX_MEMBERS_LIMIT: u64 = 10_000;

room_setting!(MaxMembers, u64, "max_members", 100, |v: &u64| {
    if *v > MAX_MEMBERS_LIMIT {
        Err(crate::Error::InvalidInput(format!(
            "max_members cannot exceed {MAX_MEMBERS_LIMIT}"
        )))
    } else {
        Ok(())
    }
});

impl MaxMembers {
    /// Maximum allowed value for `max_members` setting
    pub const MAX: u64 = MAX_MEMBERS_LIMIT;
}

room_setting!(AdminAddedPermissions, u64, "admin_added_permissions", 0);
room_setting!(AdminRemovedPermissions, u64, "admin_removed_permissions", 0);
room_setting!(MemberAddedPermissions, u64, "member_added_permissions", 0);
room_setting!(
    MemberRemovedPermissions,
    u64,
    "member_removed_permissions",
    0
);
room_setting!(GuestAddedPermissions, u64, "guest_added_permissions", 0);
room_setting!(GuestRemovedPermissions, u64, "guest_removed_permissions", 0);

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
    const KEY: &'static str = "auto_play";
    const TYPE_NAME: &'static str = "AutoPlay";
    type Value = AutoPlaySettings;

    fn value(&self) -> &AutoPlaySettings {
        &self.value
    }

    fn value_mut(&mut self) -> &mut AutoPlaySettings {
        &mut self.value
    }

    fn parse_from_str(value: &str) -> Result<AutoPlaySettings> {
        serde_json::from_str(value)
            .map_err(|_| crate::Error::InvalidInput(format!("Invalid JSON for auto_play: {value}")))
    }

    fn format_value(value: &AutoPlaySettings) -> Result<String> {
        serde_json::to_string(value).map_err(crate::Error::from)
    }

    fn default_value() -> AutoPlaySettings {
        AutoPlaySettings::default()
    }
}

// Implement RoomSettingProvider for AutoPlay (manual — not from macro)
impl RoomSettingProvider for AutoPlay {
    fn key(&self) -> &'static str {
        <Self as RoomSetting>::KEY
    }

    fn type_name(&self) -> &'static str {
        <Self as RoomSetting>::TYPE_NAME
    }

    fn is_valid_raw(&self, value: &str) -> Result<()> {
        Self::parse_from_str(value)?;
        Ok(())
    }

    fn parse_raw(&self, value: &str) -> Result<Box<dyn std::any::Any + Send + Sync>> {
        let parsed = Self::parse_from_str(value)?;
        Ok(Box::new(parsed))
    }

    fn default_as_string(&self) -> Result<String> {
        Self::format_value(&AutoPlaySettings::default())
    }

    fn apply_to(&self, settings: &mut RoomSettings, value: &str) -> Result<()> {
        settings.auto_play = Self::new(Self::parse_from_str(value)?);
        Ok(())
    }
}

use serde::{Deserialize, Serialize};

/// Room settings composed of individual type-safe settings
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
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

impl RoomSettings {
    /// Validate all room setting fields and cross-field permission ceilings.
    pub fn validate(&self) -> Result<()> {
        self.allow_guest_join.validate()?;
        self.max_members.validate()?;
        self.require_approval.validate()?;
        self.allow_auto_join.validate()?;
        self.chat_enabled.validate()?;
        self.auto_play.validate()?;
        self.admin_added_permissions.validate()?;
        self.admin_removed_permissions.validate()?;
        self.member_added_permissions.validate()?;
        self.member_removed_permissions.validate()?;
        self.guest_added_permissions.validate()?;
        self.guest_removed_permissions.validate()?;
        self.validate_permissions()
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

    /// Set a field by key from a string value via the registry (fully generic).
    ///
    /// Dispatches through `dyn RoomSettingProvider::apply_to` — no match block needed.
    /// Business-rule validations (e.g., `max_members` ceiling)
    /// are the caller's responsibility.
    pub fn set_by_key(&mut self, key: &str, value: &str) -> Result<()> {
        RoomSettingsRegistry::apply_setting(self, key, value)
    }

    /// Validate that permission overrides don't escalate beyond role ceilings
    ///
    /// - Guest added permissions cannot exceed `GUEST_ASSIGNABLE`
    /// - Member added permissions cannot exceed `DEFAULT_ADMIN`
    pub fn validate_permissions(&self) -> Result<()> {
        if !RoomAdminPermissionBits::includes_only_defined(self.admin_added_permissions.0)
            || !RoomAdminPermissionBits::includes_only_defined(self.admin_removed_permissions.0)
        {
            return Err(Error::InvalidInput(
                "Room admin permission defaults contain bits outside the admin permission bitspace"
                    .to_string(),
            ));
        }

        if !RoomMemberPermissionBits::includes_only_defined(self.member_added_permissions.0)
            || !RoomMemberPermissionBits::includes_only_defined(self.member_removed_permissions.0)
        {
            return Err(Error::InvalidInput(
                "Room member permission defaults contain bits outside the member permission bitspace"
                    .to_string(),
            ));
        }

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

    #[test]
    fn test_dynamic_validation() {
        assert!(RoomSettingsRegistry::validate_setting("chat_enabled", "true").is_ok());
        assert!(RoomSettingsRegistry::validate_setting("chat_enabled", "false").is_ok());
        assert!(RoomSettingsRegistry::validate_setting("chat_enabled", "invalid").is_err());

        assert!(RoomSettingsRegistry::validate_setting("admin_added_permissions", "123").is_ok());
        assert!(
            RoomSettingsRegistry::validate_setting("admin_added_permissions", "invalid").is_err()
        );

        assert!(RoomSettingsRegistry::validate_setting("max_members", "100").is_ok());
        assert!(RoomSettingsRegistry::validate_setting("max_members", "0").is_ok());
        assert!(RoomSettingsRegistry::validate_setting("max_members", "invalid").is_err());
    }

    #[test]
    fn test_apply_to_via_registry() {
        let mut settings = RoomSettings::default();
        assert!(settings.chat_enabled.0);

        ok(
            RoomSettingsRegistry::apply_setting(&mut settings, "chat_enabled", "false"),
            "chat_enabled setting should apply",
        );
        assert!(!settings.chat_enabled.0);

        ok(
            RoomSettingsRegistry::apply_setting(&mut settings, "max_members", "42"),
            "max_members setting should apply",
        );
        assert_eq!(settings.max_members.0, 42);
    }

    #[test]
    fn test_apply_to_unknown_key_returns_error() {
        let mut settings = RoomSettings::default();
        let result = RoomSettingsRegistry::apply_setting(&mut settings, "nonexistent", "true");
        assert!(result.is_err());
    }

    #[test]
    fn test_apply_to_invalid_value_returns_error() {
        let mut settings = RoomSettings::default();
        let result = RoomSettingsRegistry::apply_setting(&mut settings, "chat_enabled", "not_bool");
        assert!(result.is_err());
    }

    #[test]
    fn test_set_by_key_delegates_to_registry() {
        let mut settings = RoomSettings::default();
        ok(
            settings.set_by_key("chat_enabled", "false"),
            "set_by_key should apply chat_enabled",
        );
        assert!(!settings.chat_enabled.0);
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
        assert!(RoomSettingsRegistry::validate_setting("max_members", "0").is_ok());
        assert!(RoomSettingsRegistry::validate_setting(
            "max_members",
            &MaxMembers::MAX.to_string()
        )
        .is_ok());
        assert!(RoomSettingsRegistry::validate_setting(
            "max_members",
            &(MaxMembers::MAX + 1).to_string()
        )
        .is_err());
    }

    #[test]
    fn apply_to_max_members_rejects_over_limit() {
        let mut settings = RoomSettings::default();
        let result = settings.set_by_key("max_members", "99999");
        assert!(result.is_err());
        assert_eq!(settings.max_members.0, 100);
    }

    #[test]
    fn room_settings_validation_rejects_over_limit_max_members() {
        let settings = RoomSettings {
            max_members: MaxMembers(MaxMembers::MAX + 1),
            ..RoomSettings::default()
        };

        let result = settings.validate();

        assert!(result.is_err());
    }
}
