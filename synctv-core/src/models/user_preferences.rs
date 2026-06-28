use serde::{Deserialize, Serialize};

use super::{RoomSettings, UserId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserNotificationPreferences {
    pub room_invitation_in_app: bool,
    pub room_event_in_app: bool,
    pub system_announcement_in_app: bool,
    pub room_invitation_email: bool,
    pub room_event_email: bool,
    pub system_announcement_email: bool,
}

impl Default for UserNotificationPreferences {
    fn default() -> Self {
        Self {
            room_invitation_in_app: true,
            room_event_in_app: true,
            system_announcement_in_app: true,
            room_invitation_email: false,
            room_event_email: false,
            system_announcement_email: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UserPreferencesUpdate {
    pub two_factor_enabled: Option<bool>,
    pub notifications: Option<UserNotificationPreferences>,
}

impl UserPreferencesUpdate {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.two_factor_enabled.is_none() && self.notifications.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAuthFactors {
    pub password: bool,
    pub webauthn: bool,
    pub email: bool,
}

impl UserAuthFactors {
    #[must_use]
    pub const fn eligible_count(&self) -> usize {
        self.password as usize + self.webauthn as usize + self.email as usize
    }

    #[must_use]
    pub const fn supports_two_factor(&self) -> bool {
        self.eligible_count() >= 2
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreferences {
    pub user_id: UserId,
    pub two_factor_enabled: bool,
    pub notifications: UserNotificationPreferences,
    pub settings: RoomSettings,
}

impl UserPreferences {
    #[must_use]
    pub fn default_for_user(user_id: UserId) -> Self {
        Self {
            user_id,
            two_factor_enabled: false,
            notifications: UserNotificationPreferences::default(),
            settings: RoomSettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UserAuthFactors;

    #[test]
    fn two_factor_requires_two_non_oauth_methods() {
        assert!(!UserAuthFactors {
            password: true,
            webauthn: false,
            email: false,
        }
        .supports_two_factor());
        assert!(UserAuthFactors {
            password: true,
            webauthn: true,
            email: false,
        }
        .supports_two_factor());
        assert!(UserAuthFactors {
            password: false,
            webauthn: true,
            email: true,
        }
        .supports_two_factor());
    }
}
