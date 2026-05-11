use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::str::FromStr;

use super::{ProviderType, UserId};

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
pub struct UserProviderDefaults {
    instance_names: BTreeMap<String, String>,
}

impl UserProviderDefaults {
    fn canonical_provider_name(provider: &str) -> Result<String, String> {
        let provider = provider.trim();
        if provider.is_empty() {
            return Err("provider default provider cannot be empty".to_string());
        }

        ProviderType::from_str(provider)
            .map(|provider| provider.to_string())
            .map_err(|_| format!("unsupported provider default provider {provider}"))
    }

    pub fn try_from_iter<I, K, V>(defaults: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut instance_names = BTreeMap::new();
        for (provider, instance_name) in defaults {
            let provider = Self::canonical_provider_name(&provider.into())?;
            let instance_name = instance_name.into().trim().to_string();
            if instance_name.is_empty() {
                return Err(format!(
                    "default instance name for provider {provider} cannot be empty"
                ));
            }
            if instance_names
                .insert(provider.clone(), instance_name)
                .is_some()
            {
                return Err(format!(
                    "duplicate provider default for provider {provider}"
                ));
            }
        }

        Ok(Self { instance_names })
    }

    #[must_use]
    pub fn get_instance_name(&self, provider: &str) -> Option<&str> {
        let provider = Self::canonical_provider_name(provider).ok()?;
        self.instance_names.get(&provider).map(String::as_str)
    }

    #[must_use]
    pub fn instance_names(&self) -> &BTreeMap<String, String> {
        &self.instance_names
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &str)> {
        self.instance_names
            .iter()
            .map(|(provider, instance_name)| (provider.as_str(), instance_name.as_str()))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.instance_names.is_empty()
    }
}

impl TryFrom<BTreeMap<String, String>> for UserProviderDefaults {
    type Error = String;

    fn try_from(instance_names: BTreeMap<String, String>) -> Result<Self, Self::Error> {
        Self::try_from_iter(instance_names)
    }
}

impl IntoIterator for UserProviderDefaults {
    type Item = (String, String);
    type IntoIter = std::collections::btree_map::IntoIter<String, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.instance_names.into_iter()
    }
}

impl<'a> IntoIterator for &'a UserProviderDefaults {
    type Item = (&'a str, &'a str);
    type IntoIter = std::iter::Map<
        std::collections::btree_map::Iter<'a, String, String>,
        fn((&'a String, &'a String)) -> (&'a str, &'a str),
    >;

    fn into_iter(self) -> Self::IntoIter {
        fn map_entry<'a>(
            (provider, instance_name): (&'a String, &'a String),
        ) -> (&'a str, &'a str) {
            (provider.as_str(), instance_name.as_str())
        }

        self.instance_names.iter().map(map_entry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UserPreferencesUpdate {
    pub two_factor_enabled: Option<bool>,
    pub notifications: Option<UserNotificationPreferences>,
    pub provider_defaults: Option<UserProviderDefaults>,
}

impl UserPreferencesUpdate {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.two_factor_enabled.is_none()
            && self.notifications.is_none()
            && self.provider_defaults.is_none()
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserPreferences {
    pub user_id: UserId,
    pub two_factor_enabled: bool,
    pub notifications: UserNotificationPreferences,
    pub provider_defaults: UserProviderDefaults,
    pub settings: Value,
}

impl UserPreferences {
    #[must_use]
    pub fn default_for_user(user_id: UserId) -> Self {
        Self {
            user_id,
            two_factor_enabled: false,
            notifications: UserNotificationPreferences::default(),
            provider_defaults: UserProviderDefaults::default(),
            settings: json!({}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{UserAuthFactors, UserProviderDefaults};

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

    #[test]
    fn provider_defaults_normalize_and_reject_ambiguous_entries() {
        let defaults = UserProviderDefaults::try_from_iter([
            (" alist ".to_string(), " primary ".to_string()),
            ("emby".to_string(), "home".to_string()),
        ])
        .unwrap();
        assert_eq!(defaults.get_instance_name("alist"), Some("primary"));
        assert_eq!(defaults.get_instance_name("emby"), Some("home"));
        assert_eq!(defaults.get_instance_name("ALIST"), Some("primary"));

        assert!(UserProviderDefaults::try_from_iter([("", "primary")]).is_err());
        assert!(UserProviderDefaults::try_from_iter([("alist", " ")]).is_err());
        assert!(UserProviderDefaults::try_from_iter([("alist", "one"), ("alist", "two")]).is_err());
        assert!(UserProviderDefaults::try_from_iter([("unknown", "one")]).is_err());
        assert!(UserProviderDefaults::try_from_iter([("Alist", "one"), ("alist", "two")]).is_err());
    }
}
