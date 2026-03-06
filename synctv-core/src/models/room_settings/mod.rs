//! Type-safe room settings with automatic `lazy_static` registration
//!
//! # Architecture
//!
//! Each room setting is an **independent type** that implements `RoomSetting` trait.
//! The `room_setting!` macro generates the type with **`lazy_static`! auto-registration**.
//!
//! # Examples
//!
//! ```text
//! // Define a setting - auto-registers on first use!
//! room_setting!(ChatEnabled, bool, "chat_enabled", true);
//!
//! // Use by type (compile-time safe)
//! let setting = ChatEnabled(true);
//! // Registration happens automatically on first Default::default() call!
//!
//! // Or use via registry
//! RoomSettingsRegistry::has_key("chat_enabled");  // auto-registers
//! ```
//!
//! # Auto-Registration with `lazy_static`!
//!
//! Each type has a **`lazy_static`!** block in the macro that:
//! - Runs once on first access
//! - Registers the type in the global registry
//! - No manual registration needed!

use crate::models::permission::PermissionBits;
use crate::{Error, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

// Forward-declare RoomSettings so the trait can reference it.
// The actual struct definition is below, after the setting type definitions.

/// Trait for room setting operations (type-erased)
///
/// This trait provides a unified interface for working with room settings dynamically.
/// Each setting type auto-registers into `RoomSettingsRegistry`, so callers can
/// validate, parse, and apply settings by key without knowing the concrete type.
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
    fn default_as_string(&self) -> String;

    /// Apply a raw string value to the corresponding field of `RoomSettings`.
    ///
    /// This is the key method that enables fully generic `set_by_key` without
    /// a match block — the registry dispatches through `dyn RoomSettingProvider`.
    fn apply_to(&self, settings: &mut RoomSettings, value: &str) -> Result<()>;
}

/// Global registry for all room setting types.
///
/// Auto-populated by `#[ctor]` functions in each setting type at program startup
/// (before `main`). After initialization, this registry is effectively read-only.
///
/// Unlike `ProviderRegistry` (oauth2), this cannot easily be converted to DI
/// because `#[ctor]` runs before any application context exists. The `LazyLock`
/// + `RwLock` pattern is appropriate here since:
/// 1. Writes happen only during static initialization (before `main`)
/// 2. All runtime access is read-only
/// 3. The set of settings is fixed at compile time (not configurable)
static REGISTRY: std::sync::LazyLock<RwLock<HashMap<String, Arc<dyn RoomSettingProvider>>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

/// Global registry for all room setting types
pub struct RoomSettingsRegistry;

impl RoomSettingsRegistry {
    /// Register a setting type (called automatically by ctor)
    pub fn register(key: &'static str, provider: Arc<dyn RoomSettingProvider>) {
        let mut registry = REGISTRY.write();
        registry.insert(key.to_string(), provider);
    }

    /// Get provider for a setting by key
    pub fn get_provider(key: &str) -> Option<Arc<dyn RoomSettingProvider>> {
        let registry = REGISTRY.read();
        registry.get(key).cloned()
    }

    /// Get all registered setting keys
    pub fn all_keys() -> Vec<String> {
        let registry = REGISTRY.read();
        registry.keys().cloned().collect()
    }

    /// Check if a setting exists
    pub fn has_key(key: &str) -> bool {
        let registry = REGISTRY.read();
        registry.contains_key(key)
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
    fn format_value(value: &Self::Value) -> String;

    /// Type name (for debugging/registry)
    const TYPE_NAME: &'static str;

    /// Get default value
    fn default_value() -> Self::Value;
}

/// Macro to generate room setting types with automatic ctor registration
///
/// # Examples
///
/// ```text
/// // Without validator
/// room_setting!(ChatEnabled, bool, "chat_enabled", true);
///
/// // With validator (called during is_valid_raw and apply_to)
/// room_setting!(MaxMembers, u64, "max_members", 0, |v: &u64| {
///     if *v > 10_000 {
///         Err(crate::Error::InvalidInput("max_members cannot exceed 10000".into()))
///     } else {
///         Ok(())
///     }
/// });
/// ```
///
/// **Auto-registration**: Each type has a `#[ctor]` function that registers default instance!
///
/// The macro auto-derives the field name on `RoomSettings` from the type name
/// (e.g., `ChatEnabled` → `chat_enabled`) via `paste::paste!`.
#[macro_export]
macro_rules! room_setting {
    // Without validator — delegates to @impl with a no-op validator
    ($name:ident, $ty:ty, $key:expr, $default:expr) => {
        $crate::room_setting!(@impl $name, $ty, $key, $default, |_v: &$ty| -> $crate::Result<()> { Ok(()) });
    };
    // With validator
    ($name:ident, $ty:ty, $key:expr, $default:expr, $validator:expr) => {
        $crate::room_setting!(@impl $name, $ty, $key, $default, $validator);
    };
    // Internal implementation
    (@impl $name:ident, $ty:ty, $key:expr, $default:expr, $validator:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub $ty);

        impl $name {
            /// Validate the parsed value (custom validator from macro invocation).
            fn validate_value(v: &$ty) -> $crate::Result<()> {
                #[allow(clippy::redundant_closure_call)]
                ($validator)(v)
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

            fn parse_from_str(value: &str) -> $crate::Result<$ty> {
                value.parse::<$ty>().map_err(|_| {
                    $crate::Error::InvalidInput(format!("Invalid value for {}: {}", $key, value))
                })
            }

            fn format_value(value: &$ty) -> String {
                value.to_string()
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

            fn default_as_string(&self) -> String {
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

        // Auto-registration at program startup using ctor
        paste::paste! {
            #[ctor::ctor]
            fn [<_register_ $name:snake>]() {
                let default_instance: $name = std::default::Default::default();
                $crate::models::room_settings::RoomSettingsRegistry::register(
                    $key,
                    std::sync::Arc::new(default_instance),
                );
            }
        }

        impl std::default::Default for $name {
            fn default() -> Self {
                Self($default)
            }
        }
    };
}

// ==================== Generate Setting Types ====================
// Each type has its own lazy_static! that auto-registers!

room_setting!(ChatEnabled, bool, "chat_enabled", true);
room_setting!(DanmakuEnabled, bool, "danmaku_enabled", true);
room_setting!(AllowGuestJoin, bool, "allow_guest_join", false);
room_setting!(RequirePassword, bool, "require_password", false);
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

// RTMP player setting - controls whether RTMP play is allowed for a room.
// Default is false since RTMP play has no authentication and is insecure.
// Viewers should use HTTP-FLV or HLS endpoints instead.
room_setting!(RtmpPlayer, bool, "rtmp_player", false);

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

    fn format_value(value: &AutoPlaySettings) -> String {
        serde_json::to_string(value).unwrap_or_else(|e| {
            tracing::warn!("Failed to serialize AutoPlaySettings: {e}");
            String::from("{}")
        })
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

    fn default_as_string(&self) -> String {
        Self::format_value(&AutoPlaySettings::default())
    }

    fn apply_to(&self, settings: &mut RoomSettings, value: &str) -> Result<()> {
        settings.auto_play = Self::new(Self::parse_from_str(value)?);
        Ok(())
    }
}

// Auto-registration at program startup using ctor for AutoPlay
#[ctor::ctor]
fn _auto_play_register() {
    let default_instance: AutoPlay = std::default::Default::default();
    RoomSettingsRegistry::register("auto_play", std::sync::Arc::new(default_instance));
}

use serde::{Deserialize, Serialize};

/// Room settings composed of individual type-safe settings
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RoomSettings {
    pub require_password: RequirePassword,
    pub allow_guest_join: AllowGuestJoin,
    pub max_members: MaxMembers,
    #[serde(default)]
    pub require_approval: RequireApproval,
    #[serde(default)]
    pub allow_auto_join: AllowAutoJoin,
    pub chat_enabled: ChatEnabled,
    pub danmaku_enabled: DanmakuEnabled,
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
    #[serde(default)]
    pub rtmp_player: RtmpPlayer,
}

impl RoomSettings {
    /// Get effective permissions for Admin role
    ///
    /// Formula: (`global_default` | added) & ~removed
    #[must_use]
    pub const fn admin_permissions(&self, global_default: PermissionBits) -> PermissionBits {
        let mut result = global_default.0;
        result |= self.admin_added_permissions.0;
        result &= !self.admin_removed_permissions.0;
        PermissionBits(result)
    }

    /// Get effective permissions for Member role
    #[must_use]
    pub const fn member_permissions(&self, global_default: PermissionBits) -> PermissionBits {
        let mut result = global_default.0;
        // Cap member added permissions to DEFAULT_ADMIN ceiling
        result |= self.member_added_permissions.0 & PermissionBits::DEFAULT_ADMIN;
        result &= !self.member_removed_permissions.0;
        PermissionBits(result)
    }

    /// Get effective permissions for Guest
    #[must_use]
    pub const fn guest_permissions(&self, global_default: PermissionBits) -> PermissionBits {
        let mut result = global_default.0;
        // Cap guest added permissions to DEFAULT_MEMBER ceiling
        result |= self.guest_added_permissions.0 & PermissionBits::DEFAULT_MEMBER;
        result &= !self.guest_removed_permissions.0;
        PermissionBits(result)
    }

    /// Set a field by key from a string value via the registry (fully generic).
    ///
    /// Dispatches through `dyn RoomSettingProvider::apply_to` — no match block needed.
    /// Business-rule validations (e.g., `max_members` ceiling, `require_password` preconditions)
    /// are the caller's responsibility.
    pub fn set_by_key(&mut self, key: &str, value: &str) -> Result<()> {
        RoomSettingsRegistry::apply_setting(self, key, value)
    }

    /// Validate that permission overrides don't escalate beyond role ceilings
    ///
    /// - Guest added permissions cannot exceed `DEFAULT_MEMBER`
    /// - Member added permissions cannot exceed `DEFAULT_ADMIN`
    pub fn validate_permissions(&self) -> Result<()> {
        let guest_overflow = self.guest_added_permissions.0 & !PermissionBits::DEFAULT_MEMBER;
        if guest_overflow != 0 {
            return Err(Error::InvalidInput(
                "Guest added permissions cannot exceed member-level permissions".to_string(),
            ));
        }

        let member_overflow = self.member_added_permissions.0 & !PermissionBits::DEFAULT_ADMIN;
        if member_overflow != 0 {
            return Err(Error::InvalidInput(
                "Member added permissions cannot exceed admin-level permissions".to_string(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bool_setting() {
        let setting = ChatEnabled(true);
        assert_eq!(ChatEnabled::KEY, "chat_enabled");
        assert!(*setting.value());
    }

    #[test]
    fn test_optional_setting() {
        let setting = MaxMembers(100);
        assert_eq!(MaxMembers::KEY, "max_members");
        assert_eq!(*setting.value(), 100);
        // Test that 0 means no limit
        let no_limit = MaxMembers(0);
        assert_eq!(*no_limit.value(), 0);
    }

    #[test]
    fn test_registry() {
        assert!(RoomSettingsRegistry::has_key("chat_enabled"));
        assert!(RoomSettingsRegistry::has_key("max_members"));
    }

    #[test]
    fn test_dynamic_validation() {
        // Test bool validation
        assert!(RoomSettingsRegistry::validate_setting("chat_enabled", "true").is_ok());
        assert!(RoomSettingsRegistry::validate_setting("chat_enabled", "false").is_ok());
        assert!(RoomSettingsRegistry::validate_setting("chat_enabled", "invalid").is_err());

        // Test i64 validation
        assert!(RoomSettingsRegistry::validate_setting("admin_added_permissions", "123").is_ok());
        assert!(
            RoomSettingsRegistry::validate_setting("admin_added_permissions", "invalid").is_err()
        );

        // Test u64 validation (max_members)
        assert!(RoomSettingsRegistry::validate_setting("max_members", "100").is_ok());
        assert!(RoomSettingsRegistry::validate_setting("max_members", "0").is_ok());
        assert!(RoomSettingsRegistry::validate_setting("max_members", "invalid").is_err());
    }

    #[test]
    fn test_get_provider() {
        let provider = RoomSettingsRegistry::get_provider("chat_enabled").unwrap();
        assert_eq!(provider.key(), "chat_enabled");
        assert_eq!(provider.type_name(), "ChatEnabled");
        assert_eq!(provider.default_as_string(), "true");
    }

    #[test]
    fn test_apply_to_via_registry() {
        let mut settings = RoomSettings::default();
        assert!(settings.chat_enabled.0);

        // Apply via registry (fully generic)
        RoomSettingsRegistry::apply_setting(&mut settings, "chat_enabled", "false").unwrap();
        assert!(!settings.chat_enabled.0);

        // Apply max_members
        RoomSettingsRegistry::apply_setting(&mut settings, "max_members", "42").unwrap();
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
        settings.set_by_key("danmaku_enabled", "false").unwrap();
        assert!(!settings.danmaku_enabled.0);
    }

    #[test]
    fn test_serialize_deserialize() {
        let settings = RoomSettings {
            chat_enabled: ChatEnabled(false),
            max_members: MaxMembers(100),
            ..Default::default()
        };

        let json = serde_json::to_string(&settings).unwrap();
        let deserialized: RoomSettings = serde_json::from_str(&json).unwrap();

        assert!(!deserialized.chat_enabled.0);
        assert_eq!(deserialized.max_members.0, 100);
    }

    #[test]
    fn test_admin_permissions_default() {
        let settings = RoomSettings::default();
        let global = PermissionBits(PermissionBits::DEFAULT_ADMIN);
        let result = settings.admin_permissions(global);
        // No overrides -> result should equal global default
        assert_eq!(result.0, PermissionBits::DEFAULT_ADMIN);
    }

    #[test]
    fn test_admin_permissions_with_added() {
        let settings = RoomSettings {
            admin_added_permissions: AdminAddedPermissions(PermissionBits::EXPORT_DATA),
            ..Default::default()
        };
        let global = PermissionBits(PermissionBits::DEFAULT_ADMIN);
        let result = settings.admin_permissions(global);
        assert!(result.has(PermissionBits::EXPORT_DATA));
        // Original permissions preserved
        assert!(result.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_member_permissions_with_removed() {
        let settings = RoomSettings {
            member_removed_permissions: MemberRemovedPermissions(PermissionBits::SEND_CHAT),
            ..Default::default()
        };
        let global = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        let result = settings.member_permissions(global);
        // SEND_CHAT should be removed
        assert!(!result.has(PermissionBits::SEND_CHAT));
        // Other permissions remain
        assert!(result.has(PermissionBits::ADD_MOVIE));
    }

    #[test]
    fn test_guest_permissions_with_added_and_removed() {
        let settings = RoomSettings {
            // Give guests chat ability
            guest_added_permissions: GuestAddedPermissions(PermissionBits::SEND_CHAT),
            // But remove their playlist view
            guest_removed_permissions: GuestRemovedPermissions(PermissionBits::VIEW_PLAYLIST),
            ..Default::default()
        };
        let global = PermissionBits(PermissionBits::DEFAULT_GUEST);
        let result = settings.guest_permissions(global);
        assert!(result.has(PermissionBits::SEND_CHAT));
        assert!(!result.has(PermissionBits::VIEW_PLAYLIST));
    }

    #[test]
    fn test_max_members_max_constant() {
        assert_eq!(MaxMembers::MAX, 10_000);
    }

    #[test]
    fn test_max_members_validator_rejects_over_limit() {
        let result = RoomSettingsRegistry::validate_setting("max_members", "10001");
        assert!(result.is_err());
    }

    #[test]
    fn test_max_members_validator_accepts_at_limit() {
        assert!(RoomSettingsRegistry::validate_setting("max_members", "10000").is_ok());
    }

    #[test]
    fn test_max_members_validator_accepts_zero() {
        assert!(RoomSettingsRegistry::validate_setting("max_members", "0").is_ok());
    }

    #[test]
    fn test_apply_to_max_members_rejects_over_limit() {
        let mut settings = RoomSettings::default();
        let result = settings.set_by_key("max_members", "99999");
        assert!(result.is_err());
        // Value should not have been applied (default is 100)
        assert_eq!(settings.max_members.0, 100);
    }
}
