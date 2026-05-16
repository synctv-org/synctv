use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Audit actions stored in `audit_logs.action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    UserCreated,
    UserDeleted,
    UserBanned,
    UserUnbanned,
    UserPasswordUpdated,
    UserUsernameUpdated,
    UserPreferencesUpdated,
    UserRoleUpdated,
    RoomCreated,
    RoomDeleted,
    RoomBanned,
    RoomUnbanned,
    RoomPasswordUpdated,
    RoomOwnershipTransferred,
    PermissionGranted,
    PermissionRevoked,
    ProviderInstanceCreated,
    ProviderInstanceUpdated,
    ProviderInstanceDeleted,
    ProviderInstanceReconnected,
    SettingsUpdated,
    MemberKicked,
    MemberBanned,
    MemberUnbanned,
    MemberRoleUpdated,
    MemberPermissionUpdated,
    MemberStatusUpdated,
    RoomSettingsUpdated,
    UserApproved,
    RoomApproved,
    RoomRejected,
    StreamKicked,
    RateLimitResetFailed,
    UserLogin,
    UserLogout,
    TokenIssued,
    TokenRefreshed,
    TokenFamilyRevoked,
    // Settings access audit (read operations)
    SettingsViewed,
    SettingsGroupViewed,
}

impl AuditAction {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::UserCreated => "user_created",
            Self::UserDeleted => "user_deleted",
            Self::UserBanned => "user_banned",
            Self::UserUnbanned => "user_unbanned",
            Self::UserPasswordUpdated => "user_password_updated",
            Self::UserUsernameUpdated => "user_username_updated",
            Self::UserPreferencesUpdated => "user_preferences_updated",
            Self::UserRoleUpdated => "user_role_updated",
            Self::RoomCreated => "room_created",
            Self::RoomDeleted => "room_deleted",
            Self::RoomBanned => "room_banned",
            Self::RoomUnbanned => "room_unbanned",
            Self::RoomPasswordUpdated => "room_password_updated",
            Self::RoomOwnershipTransferred => "room_ownership_transferred",
            Self::PermissionGranted => "permission_granted",
            Self::PermissionRevoked => "permission_revoked",
            Self::ProviderInstanceCreated => "provider_instance_created",
            Self::ProviderInstanceUpdated => "provider_instance_updated",
            Self::ProviderInstanceDeleted => "provider_instance_deleted",
            Self::ProviderInstanceReconnected => "provider_instance_reconnected",
            Self::SettingsUpdated => "settings_updated",
            Self::MemberKicked => "member_kicked",
            Self::MemberBanned => "member_banned",
            Self::MemberUnbanned => "member_unbanned",
            Self::MemberRoleUpdated => "member_role_updated",
            Self::MemberPermissionUpdated => "member_permission_updated",
            Self::MemberStatusUpdated => "member_status_updated",
            Self::RoomSettingsUpdated => "room_settings_updated",
            Self::UserApproved => "user_approved",
            Self::RoomApproved => "room_approved",
            Self::RoomRejected => "room_rejected",
            Self::StreamKicked => "stream_kicked",
            Self::RateLimitResetFailed => "rate_limit_reset_failed",
            // Token security events
            Self::UserLogin => "user_login",
            Self::UserLogout => "user_logout",
            Self::TokenIssued => "token_issued",
            Self::TokenRefreshed => "token_refreshed",
            Self::TokenFamilyRevoked => "token_family_revoked",
            // Settings access audit (read operations)
            Self::SettingsViewed => "settings_viewed",
            Self::SettingsGroupViewed => "settings_group_viewed",
        }
    }

    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::UserCreated => 1,
            Self::UserDeleted => 2,
            Self::UserBanned => 3,
            Self::UserUnbanned => 4,
            Self::UserPasswordUpdated => 5,
            Self::UserUsernameUpdated => 6,
            Self::UserPreferencesUpdated => 7,
            Self::UserRoleUpdated => 8,
            Self::RoomCreated => 9,
            Self::RoomDeleted => 10,
            Self::RoomBanned => 11,
            Self::RoomUnbanned => 12,
            Self::RoomPasswordUpdated => 13,
            Self::RoomOwnershipTransferred => 14,
            Self::PermissionGranted => 15,
            Self::PermissionRevoked => 16,
            Self::ProviderInstanceCreated => 17,
            Self::ProviderInstanceUpdated => 18,
            Self::ProviderInstanceDeleted => 19,
            Self::ProviderInstanceReconnected => 20,
            Self::SettingsUpdated => 21,
            Self::MemberKicked => 22,
            Self::MemberBanned => 23,
            Self::MemberUnbanned => 24,
            Self::MemberRoleUpdated => 25,
            Self::MemberPermissionUpdated => 26,
            Self::MemberStatusUpdated => 27,
            Self::RoomSettingsUpdated => 28,
            Self::UserApproved => 29,
            Self::RoomApproved => 30,
            Self::RoomRejected => 31,
            Self::StreamKicked => 32,
            Self::RateLimitResetFailed => 33,
            Self::UserLogin => 34,
            Self::UserLogout => 35,
            Self::TokenIssued => 36,
            Self::TokenRefreshed => 37,
            Self::TokenFamilyRevoked => 38,
            Self::SettingsViewed => 39,
            Self::SettingsGroupViewed => 40,
        }
    }
}

impl From<AuditAction> for i16 {
    fn from(value: AuditAction) -> Self {
        value.as_i16()
    }
}

impl TryFrom<i16> for AuditAction {
    type Error = String;

    fn try_from(value: i16) -> std::result::Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::UserCreated),
            2 => Ok(Self::UserDeleted),
            3 => Ok(Self::UserBanned),
            4 => Ok(Self::UserUnbanned),
            5 => Ok(Self::UserPasswordUpdated),
            6 => Ok(Self::UserUsernameUpdated),
            7 => Ok(Self::UserPreferencesUpdated),
            8 => Ok(Self::UserRoleUpdated),
            9 => Ok(Self::RoomCreated),
            10 => Ok(Self::RoomDeleted),
            11 => Ok(Self::RoomBanned),
            12 => Ok(Self::RoomUnbanned),
            13 => Ok(Self::RoomPasswordUpdated),
            14 => Ok(Self::RoomOwnershipTransferred),
            15 => Ok(Self::PermissionGranted),
            16 => Ok(Self::PermissionRevoked),
            17 => Ok(Self::ProviderInstanceCreated),
            18 => Ok(Self::ProviderInstanceUpdated),
            19 => Ok(Self::ProviderInstanceDeleted),
            20 => Ok(Self::ProviderInstanceReconnected),
            21 => Ok(Self::SettingsUpdated),
            22 => Ok(Self::MemberKicked),
            23 => Ok(Self::MemberBanned),
            24 => Ok(Self::MemberUnbanned),
            25 => Ok(Self::MemberRoleUpdated),
            26 => Ok(Self::MemberPermissionUpdated),
            27 => Ok(Self::MemberStatusUpdated),
            28 => Ok(Self::RoomSettingsUpdated),
            29 => Ok(Self::UserApproved),
            30 => Ok(Self::RoomApproved),
            31 => Ok(Self::RoomRejected),
            32 => Ok(Self::StreamKicked),
            33 => Ok(Self::RateLimitResetFailed),
            34 => Ok(Self::UserLogin),
            35 => Ok(Self::UserLogout),
            36 => Ok(Self::TokenIssued),
            37 => Ok(Self::TokenRefreshed),
            38 => Ok(Self::TokenFamilyRevoked),
            39 => Ok(Self::SettingsViewed),
            40 => Ok(Self::SettingsGroupViewed),
            other => Err(format!("Unknown audit action code: {other}")),
        }
    }
}

impl FromStr for AuditAction {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "user_created" => Ok(Self::UserCreated),
            "user_deleted" => Ok(Self::UserDeleted),
            "user_banned" => Ok(Self::UserBanned),
            "user_unbanned" => Ok(Self::UserUnbanned),
            "user_password_updated" => Ok(Self::UserPasswordUpdated),
            "user_username_updated" => Ok(Self::UserUsernameUpdated),
            "user_preferences_updated" => Ok(Self::UserPreferencesUpdated),
            "user_role_updated" => Ok(Self::UserRoleUpdated),
            "room_created" => Ok(Self::RoomCreated),
            "room_deleted" => Ok(Self::RoomDeleted),
            "room_banned" => Ok(Self::RoomBanned),
            "room_unbanned" => Ok(Self::RoomUnbanned),
            "room_password_updated" => Ok(Self::RoomPasswordUpdated),
            "room_ownership_transferred" => Ok(Self::RoomOwnershipTransferred),
            "permission_granted" => Ok(Self::PermissionGranted),
            "permission_revoked" => Ok(Self::PermissionRevoked),
            "provider_instance_created" => Ok(Self::ProviderInstanceCreated),
            "provider_instance_updated" => Ok(Self::ProviderInstanceUpdated),
            "provider_instance_deleted" => Ok(Self::ProviderInstanceDeleted),
            "provider_instance_reconnected" => Ok(Self::ProviderInstanceReconnected),
            "settings_updated" => Ok(Self::SettingsUpdated),
            "member_kicked" => Ok(Self::MemberKicked),
            "member_banned" => Ok(Self::MemberBanned),
            "member_unbanned" => Ok(Self::MemberUnbanned),
            "member_role_updated" => Ok(Self::MemberRoleUpdated),
            "member_permission_updated" => Ok(Self::MemberPermissionUpdated),
            "member_status_updated" => Ok(Self::MemberStatusUpdated),
            "room_settings_updated" => Ok(Self::RoomSettingsUpdated),
            "user_approved" => Ok(Self::UserApproved),
            "room_approved" => Ok(Self::RoomApproved),
            "room_rejected" => Ok(Self::RoomRejected),
            "stream_kicked" => Ok(Self::StreamKicked),
            "rate_limit_reset_failed" => Ok(Self::RateLimitResetFailed),
            "user_login" => Ok(Self::UserLogin),
            "user_logout" => Ok(Self::UserLogout),
            "token_issued" => Ok(Self::TokenIssued),
            "token_refreshed" => Ok(Self::TokenRefreshed),
            "token_family_revoked" => Ok(Self::TokenFamilyRevoked),
            "settings_viewed" => Ok(Self::SettingsViewed),
            "settings_group_viewed" => Ok(Self::SettingsGroupViewed),
            other => Err(format!("Unknown audit action: {other}")),
        }
    }
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Target types stored in `audit_logs.target_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditTargetType {
    User,
    Room,
    Member,
    ProviderInstance,
    Settings,
    System,
    Stream,
    Token,
}

impl AuditTargetType {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Room => "room",
            Self::Member => "member",
            Self::ProviderInstance => "provider_instance",
            Self::Settings => "settings",
            Self::System => "system",
            Self::Stream => "stream",
            Self::Token => "token",
        }
    }

    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::User => 1,
            Self::Room => 2,
            Self::Member => 3,
            Self::ProviderInstance => 4,
            Self::Settings => 5,
            Self::System => 6,
            Self::Stream => 7,
            Self::Token => 8,
        }
    }
}

impl From<AuditTargetType> for i16 {
    fn from(value: AuditTargetType) -> Self {
        value.as_i16()
    }
}

impl TryFrom<i16> for AuditTargetType {
    type Error = String;

    fn try_from(value: i16) -> std::result::Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::User),
            2 => Ok(Self::Room),
            3 => Ok(Self::Member),
            4 => Ok(Self::ProviderInstance),
            5 => Ok(Self::Settings),
            6 => Ok(Self::System),
            7 => Ok(Self::Stream),
            8 => Ok(Self::Token),
            other => Err(format!("Unknown audit target type code: {other}")),
        }
    }
}

impl FromStr for AuditTargetType {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "room" => Ok(Self::Room),
            "member" => Ok(Self::Member),
            "provider_instance" => Ok(Self::ProviderInstance),
            "settings" => Ok(Self::Settings),
            "system" => Ok(Self::System),
            "stream" => Ok(Self::Stream),
            "token" => Ok(Self::Token),
            other => Err(format!("Unknown audit target type: {other}")),
        }
    }
}

impl fmt::Display for AuditTargetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
